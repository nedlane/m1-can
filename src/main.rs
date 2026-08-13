//! `m1-dbc` CLI: export a repo's MoTeC `.m1dbc` CAN databases as standard
//! Vector `.dbc` files, and verify the committed exports are up to date.
//!
//! The whole tool is one verb today, but it is a *subcommand* rather than a bare
//! flag so the binary can grow a second one (an inspect/diff verb, say) without
//! breaking the pre-commit hooks that already call `m1-dbc export --check`.
use clap::{Parser, Subcommand};
use std::process::ExitCode;

#[derive(Parser)]
#[command(
    name = "m1-dbc",
    about = "Export MoTeC M1 .m1dbc CAN databases as standard Vector .dbc files",
    version
)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Export the repo's MoTeC `.m1dbc` CAN databases as standard Vector `.dbc`
    /// files.
    ///
    /// Takes no path: it is driven by the `[dbc]` section of the
    /// `m1-tools.toml` that governs the working directory, found by walking up,
    /// so it can be run from anywhere in the repo (and from a hook, which knows
    /// nothing about paths). That section names `src_dir` (where the `.m1dbc`
    /// sources live), `out_dir` (where the `.dbc` exports go), and one
    /// `[dbc.exports]` line per pair, `"source stem" = "output stem"` — see the
    /// README for the full block.
    ///
    /// The `.m1dbc` stays the source of truth; each `.dbc` is a generated export,
    /// committed so tools that cannot read MoTeC XML (Vector, SavvyCAN,
    /// python-can) see the same database. Output is deterministic — same source,
    /// same bytes — so a regenerated export is either identical or a real change.
    /// Every run verifies its own work: the messages and signals written must
    /// equal the convertible count in the source, and re-reading the written file
    /// with an independent DBC parser must reproduce those same counts.
    ///
    /// Exit codes: 0 = written, or with `--check` everything in sync (and also a
    /// repo with no `[dbc]` section — nothing to export is not a failure);
    /// 1 = a count or round-trip mismatch, or a committed export that is out of
    /// date; 2 = the config, a source, or a generated file could not be read or
    /// parsed. A failing file does not stop the run: every file is reported and
    /// the worst code is returned.
    Export {
        /// Verify the committed `.dbc` exports match their `.m1dbc` sources
        /// without writing anything: generate into a temporary directory and
        /// compare. Both sides are compared as decoded text, so line endings, a
        /// leading UTF-8 BOM and a committed export saved as Windows-1252 are
        /// not drift. Exit 1 if any export is out of date. This is what a
        /// pre-commit hook or CI job runs.
        #[arg(long)]
        check: bool,
    },
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    match run(&cli) {
        Ok(code) => code,
        Err(e) => {
            eprintln!("error: {e}");
            ExitCode::FAILURE
        }
    }
}

fn run(cli: &Cli) -> Result<ExitCode, Box<dyn std::error::Error>> {
    match &cli.command {
        Command::Export { check } => {
            // The working directory is the input: the `[dbc]` section is found by
            // walking up from it, so the verb works from anywhere in the repo.
            let cwd = std::env::current_dir()
                .map_err(|e| format!("cannot determine the working directory: {e}"))?;
            // 2 (a config/IO/parse failure) has to be returned, not raised: main's
            // catch-all maps every Err to 1, which is the "stale export" code.
            Ok(ExitCode::from(m1_dbc::export(&cwd, *check).code()))
        }
    }
}
