//! Inspect a MoTeC M1 project's CAN topology and export `.m1dbc` databases as
//! standard Vector `.dbc` files.
//!
//! The crate is split by stage so each half is testable on its own:
//! - `config` — read the `[dbc]` section of `m1-tools.toml`: which files a
//!   repo exports, and where.
//! - `m1dbc` — decode and parse a `.m1dbc` into a neutral in-memory model.
//! - `writer` — render that model as the text of a standard Vector `.dbc`.
//! - `roundtrip` — read that text back with a third-party parser, so the
//!   output is checked by code this repo did not write.
//!
//! Every stage reports failure with [`ExportError`].
//!
//! [`export`] is the entry point the `export` subcommand calls: it runs all four
//! stages over every pair the repo declares, prints the report, and returns the
//! [`Outcome`] that becomes the process exit code. The export stage modules stay
//! private; CAN inspection is also exposed as a library API so `m1-mcp` and
//! other consumers share exactly the same bus-binding and overlap rules.

use std::fmt;
use std::path::{Path, PathBuf};

mod can;
mod config;
mod loader;
mod m1dbc;
mod roundtrip;
mod writer;

pub use can::{
    CanIdOverlapDto, CanInitDto, CanMessageDto, CanModuleDto, CanOutcome, CanOverlapMemberDto,
    inspect,
};

/// How an export run ended — the CLI's exit-code contract.
///
/// Ordered worst-last so a run over many files can fold with
/// [`Ord::max`]: one bad pair decides the exit code, but every pair is still
/// processed and reported first.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Outcome {
    /// Everything was written, or under `--check` everything was already in
    /// sync. Also a repo with no `[dbc]` section: nothing to export is not a
    /// failure.
    Success,
    /// The export does not agree with its source: a count or round-trip
    /// mismatch, or under `--check` a committed `.dbc` that is out of date.
    /// The tool worked; the files do not match.
    Mismatch,
    /// The tool could not do its job: config, I/O, or parse failure. Nothing
    /// about the affected pair can be trusted, including "in sync".
    Failed,
}

impl Outcome {
    /// The process exit code: `0` success, `1` mismatch, `2` failure.
    ///
    /// The 0/1 split is the M1 toolchain's usual "no findings / findings"
    /// convention; `2` is a deliberate extension so a CI gate or pre-commit hook
    /// can tell a stale export (regenerate and commit) from a broken setup (fix
    /// the config or the source) without parsing the report.
    pub fn code(self) -> u8 {
        match self {
            Outcome::Success => 0,
            Outcome::Mismatch => 1,
            Outcome::Failed => 2,
        }
    }
}

/// Export every `.dbc` the repo governing `start_dir` declares, printing the
/// per-file report to stdout as it goes.
///
/// `start_dir` is normally the working directory: the `[dbc]` section is found
/// by walking up from it, so the command works from anywhere in the repo, and
/// every path is resolved against the directory holding that config rather than
/// against `start_dir` itself.
///
/// With `check`, nothing in the working tree is touched: each export is
/// generated into a temporary directory and compared against the committed
/// file. The comparison is on *decoded text*, never bytes — both sides are read
/// through `m1_workspace::read_text` and then have their line endings
/// normalised — so three differences are deliberately not drift: CRLF vs LF (the
/// committed exports are `eol=crlf` in git while the writer emits LF), a leading
/// UTF-8 BOM on the committed file, and a committed export stored as
/// Windows-1252 rather than UTF-8. Only the text itself differing counts. A
/// committed file that does not exist compares as empty — out of date, not a
/// crash.
///
/// Failure is never fatal to the run: a pair that cannot be read, parsed or
/// verified is reported to stderr and the next one is attempted, so one broken
/// file does not hide the state of the other eleven. The returned [`Outcome`] is
/// the worst one seen.
pub fn export(start_dir: &Path, check: bool) -> Outcome {
    let config = match config::load_dbc_config(start_dir) {
        Ok(Some(config)) => config,
        Ok(None) => {
            println!(
                "no [dbc] section in {}; nothing to export",
                config::CONFIG_FILE
            );
            return Outcome::Success;
        }
        Err(e) => {
            eprintln!("error: {e}");
            return Outcome::Failed;
        }
    };

    // One scratch directory for the whole run under --check: generated output
    // has to land on a real filesystem so what is verified and compared is a
    // file that was actually written, not just a string in memory.
    let scratch = if check {
        match scratch_dir() {
            Ok(dir) => Some(dir),
            Err(e) => {
                eprintln!("error: {e}");
                return Outcome::Failed;
            }
        }
    } else {
        None
    };

    let mut outcome = Outcome::Success;
    for (source_stem, out_stem) in &config.exports {
        let src = config
            .root
            .join(&config.src_dir)
            .join(format!("{source_stem}.m1dbc"));
        let out = config
            .root
            .join(&config.out_dir)
            .join(format!("{out_stem}.dbc"));
        let pair = Pair {
            src: &src,
            out: &out,
            source_stem,
            out_stem,
            root: &config.root,
            scratch: scratch.as_deref(),
        };
        outcome = outcome.max(match export_pair(&pair) {
            Ok(o) => o,
            Err(e) => {
                eprintln!("error: {e}");
                Outcome::Failed
            }
        });
    }

    if let Some(dir) = &scratch {
        // Best-effort: a leftover temp directory is untidy, not a failure, and
        // reporting it would bury the report it followed.
        let _ = std::fs::remove_dir_all(dir);
    }
    outcome
}

/// One `.m1dbc` → `.dbc` pair, with everything needed to export and report it.
struct Pair<'a> {
    /// The source `.m1dbc`.
    src: &'a Path,
    /// The committed `.dbc`: where generation writes, and what `--check`
    /// compares against.
    out: &'a Path,
    /// The source's file stem, which MoTeC uses to qualify every component name.
    source_stem: &'a str,
    /// The output's file stem, which heads the report block.
    out_stem: &'a str,
    /// The repo root, so the report can name paths the way a developer typed
    /// them rather than as long absolute ones.
    root: &'a Path,
    /// Under `--check`, the directory to generate into instead of `out`.
    scratch: Option<&'a Path>,
}

/// Export one pair and print its report block.
///
/// # Errors
///
/// A message, already prefixed with the file it is about, for any failure that
/// makes the pair unverifiable: an unreadable source or output, a `.m1dbc` that
/// will not parse, or generated text an independent DBC parser rejects. The
/// caller prints it and carries on with the next pair.
fn export_pair(pair: &Pair<'_>) -> Result<Outcome, String> {
    let bytes = std::fs::read(pair.src).map_err(|e| format!("{}: {e}", pair.src.display()))?;
    let file = m1dbc::parse_m1dbc(&bytes, pair.source_stem)
        .map_err(|e| format!("{}: {e}", pair.src.display()))?;
    let text = writer::write_dbc(&file);

    let target = match pair.scratch {
        Some(dir) => dir.join(format!("{}.dbc", pair.out_stem)),
        None => pair.out.to_path_buf(),
    };
    write_export(&target, &text)?;

    // Read the file back rather than verifying the string that was meant to go
    // in it: the round-trip is a check on the artifact a consumer will open.
    let written_text =
        m1_workspace::read_text(&target).map_err(|e| format!("{}: {e}", target.display()))?;
    let written = (
        file.messages.len(),
        file.messages.iter().map(|m| m.signals.len()).sum::<usize>(),
    );
    let round_tripped = roundtrip::roundtrip_counts(&written_text)
        .map_err(|e| format!("{}: {e}", target.display()))?;

    let totals = file.totals;
    println!("=== {} ===", pair.out_stem);
    println!(
        "  {:<16}: {} messages, {} signals",
        "source .m1dbc", totals.total_messages, totals.total_signals
    );
    println!(
        "  {:<16}: {} messages, {} signals (source minus skipped)",
        "convertible", totals.convertible_messages, totals.convertible_signals
    );
    println!(
        "  {:<16}: {} messages, {} signals",
        "written .dbc", written.0, written.1
    );
    println!(
        "  {:<16}: {} messages, {} signals",
        "round-tripped", round_tripped.0, round_tripped.1
    );
    if file.skipped.is_empty() {
        println!("  {:<16}: none", "skipped");
    } else {
        println!("  {:<16}: {}", "skipped", file.skipped.len());
        for item in &file.skipped {
            println!("    - {item}");
        }
    }

    let mut outcome = Outcome::Success;
    // Two independent cross-checks, both reported before the run ends so a
    // developer sees every problem at once rather than one per re-run.
    if written != (totals.convertible_messages, totals.convertible_signals) {
        println!("  !! written .dbc count != convertible source count");
        outcome = Outcome::Mismatch;
    }
    if round_tripped != written {
        println!("  !! round-trip mismatch vs written .dbc");
        outcome = Outcome::Mismatch;
    }

    if pair.scratch.is_some() {
        // A committed export that does not exist yet compares as empty, so the
        // first run in a repo reports "out of sync" rather than failing.
        //
        // Both sides came through `read_text`, which strips a leading UTF-8 BOM
        // and falls back to Windows-1252, so the comparison below already
        // ignores those two differences as well as the line endings it
        // normalises explicitly. That is deliberate — a repo storing its
        // exports the way its editor saved them is not drift — and it is what
        // the `--help` and README text describe.
        let committed = if pair.out.exists() {
            m1_workspace::read_text(pair.out).map_err(|e| format!("{}: {e}", pair.out.display()))?
        } else {
            String::new()
        };
        if normalise_newlines(&written_text) == normalise_newlines(&committed) {
            println!("  {:<16}: yes", "in sync");
        } else {
            println!(
                "  !! {} is out of sync with {} — run: m1-can export",
                relative(pair.out, pair.root),
                relative(pair.src, pair.root)
            );
            outcome = outcome.max(Outcome::Mismatch);
        }
    }

    Ok(outcome)
}

/// Write `text` to `path` as UTF-8, creating the output directory if needed.
///
/// UTF-8 rather than MoTeC's Windows-1252: a `.dbc` is a Vector file read by
/// tools that have nothing to do with M1, and the Python reference this replaces
/// writes UTF-8 too. Line endings are the LF the writer emits — `--check`
/// normalises them away, so a repo is free to store the file as CRLF.
fn write_export(path: &Path, text: &str) -> Result<(), String> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    }
    m1_workspace::atomic_write(path, text.as_bytes())
        .map_err(|e| format!("{}: {e}", path.display()))
}

/// A fresh directory under the system temp dir for one `--check` run.
fn scratch_dir() -> Result<PathBuf, String> {
    let mut dir = std::env::temp_dir();
    dir.push(format!("m1-can-export-{}", std::process::id()));
    // Left over from a crashed run with the same pid: start from nothing so a
    // stale file can never be read back as this run's output.
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    Ok(dir)
}

/// Collapse CRLF and CR to LF so a committed export stored with Windows line
/// endings compares equal to the LF text the writer produces.
fn normalise_newlines(text: &str) -> String {
    text.replace("\r\n", "\n").replace('\r', "\n")
}

/// `path` as written relative to the repo root, for the report.
fn relative(path: &Path, root: &Path) -> String {
    path.strip_prefix(root)
        .unwrap_or(path)
        .display()
        .to_string()
}

/// Why an export could not be produced.
///
/// A plain enum carrying `String` context (never a boxed source), so it stays
/// `PartialEq` and tests can compare error values directly — the same shape
/// every error type in the M1 toolchain uses.
#[derive(Debug, PartialEq, Eq)]
pub enum ExportError {
    /// The `.m1dbc` did not parse as XML.
    Xml(String),
    /// A CAN component is unusable: a numeric attribute that will not parse, or
    /// a message/signal with no `<Props>`.
    Invalid(String),
    /// `.dbc` text did not parse as a CAN database. Carries the third-party
    /// parser's own diagnosis, from the round-trip verification step.
    Dbc(String),
    /// The `[dbc]` section of `m1-tools.toml` is unusable — unreadable file,
    /// broken TOML, a missing required key, a value of the wrong type, an
    /// export stem that is not a bare file name, or two exports naming the same
    /// output. The message names the file and the offending key. An *absent*
    /// `[dbc]` section is not this: it is a clean no-op.
    Config(String),
}

impl fmt::Display for ExportError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ExportError::Xml(e) => write!(f, "invalid .m1dbc XML: {e}"),
            ExportError::Invalid(m) => write!(f, "{m}"),
            ExportError::Dbc(e) => write!(f, "invalid .dbc text: {e}"),
            ExportError::Config(m) => write!(f, "{m}"),
        }
    }
}
impl std::error::Error for ExportError {}
