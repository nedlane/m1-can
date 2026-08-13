//! The `[dbc]` section of `m1-tools.toml` — which `.m1dbc` files a repo exports
//! and what to call the `.dbc` it writes.
//!
//! ```toml
//! [dbc]
//! src_dir = "UQR-EV/01.00/dbc"
//! out_dir = "dbc"
//!
//! # source .m1dbc stem -> output .dbc stem
//! [dbc.exports]
//! "Balls3EV25" = "Balls3EV25"
//! "SBG DBC" = "SBG"
//! ```
//!
//! The mapping is explicit rather than derived: the M1-Build module often
//! carries a `" DBC"` suffix so it does not collide with another project
//! object, and the exported file should not. It is not a general "strip a
//! trailing ` DBC`" rule — `PDM P14` keeps its space — so each repo states the
//! pairs it wants.
//!
//! # Why the file is parsed twice
//!
//! Discovery goes through [`M1ToolsConfig::discover_result`], the same walk-up
//! every other M1 tool uses, so "which `m1-tools.toml` governs this directory"
//! can never drift between `m1-can`, `m1-project`, `m1-fmt`, `m1-lint` and the
//! LSP. That call parses the file into the *shared* schema, which has no `[dbc]`
//! section and discards it. This module therefore re-reads the discovered path
//! and parses `[dbc]` out of it directly. Adding the section to `M1ToolsConfig`
//! would be a cross-repo change to `m1-workspace`; re-reading one small config
//! file once per invocation is not worth that, and it keeps the DBC schema
//! owned by the tool that understands it. `toml` is pulled in for this module
//! alone, and the crate's no-serde stance is intact: `[dbc]` is navigated as a
//! [`toml::Table`], never deserialised into a derived type.

use crate::ExportError;
use m1_workspace::config::{M1ToolsConfig, TOOLS_CONFIG_FILE};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use toml::{Table, Value};

/// The config file this module reads, re-exported so a caller can name it in a
/// message without hard-coding the string.
pub const CONFIG_FILE: &str = TOOLS_CONFIG_FILE;

/// A repo's DBC export plan, as declared in `[dbc]`.
#[derive(Debug, PartialEq, Eq)]
pub struct DbcConfig {
    /// `[dbc] src_dir` — where the `.m1dbc` sources live, exactly as written in
    /// the config and therefore relative to [`root`](Self::root). Guaranteed
    /// non-empty and relative.
    pub src_dir: PathBuf,
    /// `[dbc] out_dir` — where the `.dbc` exports go, exactly as written in the
    /// config and therefore relative to [`root`](Self::root). Guaranteed
    /// non-empty and relative.
    pub out_dir: PathBuf,
    /// `(source .m1dbc stem, output .dbc stem)` pairs, **sorted by source
    /// stem**.
    ///
    /// Every stem is a bare file name — never empty, absolute, or path-bearing —
    /// and no two pairs share an output stem, both enforced at load time.
    ///
    /// TOML tables carry no ordering guarantee, so document order is not
    /// something a caller could rely on. Sorting makes the export run — and its
    /// console report — identical on every machine. It has no effect on any
    /// file's contents, only on the order they are handled in.
    pub exports: Vec<(String, String)>,
    /// The directory holding the `m1-tools.toml` these values came from, i.e.
    /// the repo root the other two paths are relative to.
    pub root: PathBuf,
}

/// Load the `[dbc]` section governing `start_dir`.
///
/// Walks up from `start_dir` for `m1-tools.toml` exactly as every other M1 tool
/// does, then reads `[dbc]` out of the file that was found.
///
/// Returns `Ok(None)` for both shapes of "this repo does not export DBCs": no
/// `m1-tools.toml` anywhere up the tree, or one that carries no `[dbc]` section.
/// Neither is a failure — the export must be shippable to repos that have no use
/// for it.
///
/// # Errors
///
/// [`ExportError::Config`] when a `[dbc]` section exists but cannot be honoured:
/// the file is unreadable or not valid TOML, `src_dir`/`out_dir`/`[dbc.exports]`
/// is missing, `[dbc.exports]` is empty, a value has the wrong TOML type,
/// `src_dir`/`out_dir` is empty or absolute, an export stem is anything other
/// than a bare file name, or two exports name the same output stem. A
/// half-understood export plan is refused rather than guessed at; the message
/// names the file and the offending key.
pub fn load_dbc_config(start_dir: &Path) -> Result<Option<DbcConfig>, ExportError> {
    // The walk-up climbs `parent()`s, which is a *lexical* operation: a relative
    // `start_dir` runs out of components at the working directory and the search
    // stops there. Handed `dbc/` from inside a repo, discovery would miss the
    // `m1-tools.toml` one level up and this function would report the clean no-op
    // — a false success in exactly the pre-push-hook case the export exists for.
    // `std::path::absolute` re-bases the path without touching the filesystem;
    // unlike `canonicalize` it does not require the path to exist, and it is what
    // makes `root` and every error message absolute as documented.
    let start = std::path::absolute(start_dir)
        .map_err(|e| ExportError::Config(format!("cannot resolve {}: {e}", start_dir.display())))?;
    let Some(found) =
        M1ToolsConfig::discover_result(&start).map_err(|e| ExportError::Config(e.to_string()))?
    else {
        return Ok(None);
    };
    let path = found.path;
    // `find_upward` builds the path as <dir>/m1-tools.toml, so a parent always
    // exists; the fallback keeps this total rather than panicking on a path no
    // discovery can actually produce.
    let root = path.parent().unwrap_or(Path::new(".")).to_path_buf();

    let text = m1_workspace::read_text(&path)
        .map_err(|e| ExportError::Config(format!("cannot read {}: {e}", path.display())))?;
    // Already parsed once by `discover_result`, so a syntax error is caught
    // above and this arm is belt-and-braces rather than the live path.
    let table: Table = text
        .parse()
        .map_err(|e| ExportError::Config(format!("cannot parse {}: {e}", path.display())))?;

    let Some(dbc) = table.get("dbc") else {
        return Ok(None);
    };
    let dbc = dbc
        .as_table()
        .ok_or_else(|| wrong_type(&path, "dbc", "a table", dbc))?;

    Ok(Some(DbcConfig {
        src_dir: required_dir(&path, dbc, "src_dir")?,
        out_dir: required_dir(&path, dbc, "out_dir")?,
        exports: exports(&path, dbc)?,
        root,
    }))
}

/// Read a required string key out of the `[dbc]` table.
fn required_str<'a>(path: &Path, dbc: &'a Table, key: &str) -> Result<&'a str, ExportError> {
    let value = dbc.get(key).ok_or_else(|| {
        ExportError::Config(format!("{}: [dbc] {key} is missing", path.display()))
    })?;
    value
        .as_str()
        .ok_or_else(|| wrong_type(path, &format!("dbc.{key}"), "a string", value))
}

/// Read a required directory key: a non-empty path relative to the repo root.
///
/// Both directories are joined onto [`DbcConfig::root`], so an absolute one
/// would silently discard that root ([`Path::join`] replaces its base) and point
/// the export at a directory the config's repo has nothing to do with. An empty
/// one means "the root itself", which is much more likely a truncated edit than
/// an intention — refuse rather than guess (AGENTS.md). Multi-component
/// relatives (`UQR-EV/01.00/dbc`) are the normal case and stay legal.
fn required_dir(path: &Path, dbc: &Table, key: &str) -> Result<PathBuf, ExportError> {
    let raw = required_str(path, dbc, key)?;
    if raw.is_empty() || Path::new(raw).is_absolute() {
        return Err(ExportError::Config(format!(
            "{}: [dbc] {key} {raw:?} must be a non-empty path relative to the \
             directory holding {CONFIG_FILE}",
            path.display()
        )));
    }
    Ok(PathBuf::from(raw))
}

/// Refuse an export stem that is anything but a bare file name.
///
/// A stem is joined onto a directory to make one file path, so it has to name a
/// single file. [`Path::join`] *replaces* its base when handed an absolute
/// right-hand side, and a `..` component climbs out of it: either would send a
/// `--check` run's generated file into the working tree instead of the scratch
/// directory — and then compare that file against itself, reporting a green
/// "in sync" for an export that was never generated. Empty stems are refused for
/// the same reason the rest of this section is: refuse rather than guess.
///
/// `what` names the offending key the way a reader would look for it in the
/// file.
fn check_stem(path: &Path, what: &str, stem: &str) -> Result<(), ExportError> {
    use std::path::Component;
    let mut components = Path::new(stem).components();
    let is_bare_name = matches!(
        (components.next(), components.next()),
        (Some(Component::Normal(_)), None)
    );
    // `\` is not a separator on unix, so `components()` reads `..\x` there as a
    // single (very odd) file name. One config is shared across every machine
    // that clones the repo, so it is refused everywhere.
    if is_bare_name && !stem.contains('\\') {
        return Ok(());
    }
    Err(ExportError::Config(format!(
        "{}: [dbc.exports] {what} {stem:?} must be a bare file stem — one name, \
         not empty, not absolute, and with no `/`, `\\` or `..`",
        path.display()
    )))
}

/// Read `[dbc.exports]` as sorted `(source stem, output stem)` pairs.
fn exports(path: &Path, dbc: &Table) -> Result<Vec<(String, String)>, ExportError> {
    let value = dbc.get("exports").ok_or_else(|| {
        ExportError::Config(format!(
            "{}: [dbc.exports] is missing; list at least one source stem = output stem pair",
            path.display()
        ))
    })?;
    let table = value
        .as_table()
        .ok_or_else(|| wrong_type(path, "dbc.exports", "a table", value))?;
    if table.is_empty() {
        return Err(ExportError::Config(format!(
            "{}: [dbc.exports] is empty; list at least one source stem = output stem pair",
            path.display()
        )));
    }

    let mut pairs = table
        .iter()
        .map(|(source, out)| {
            let out = out.as_str().ok_or_else(|| {
                wrong_type(path, &format!("dbc.exports.{source}"), "a string", out)
            })?;
            check_stem(path, "source stem", source)?;
            check_stem(path, &format!("output stem for {source:?},"), out)?;
            Ok((source.clone(), out.to_string()))
        })
        .collect::<Result<Vec<_>, ExportError>>()?;
    // The `toml` crate's `Table` is a `BTreeMap` unless some crate in the graph
    // turns on its `preserve_order` feature, in which case iteration would
    // follow document order instead. Sorting here pins the result either way.
    pairs.sort();

    // Two sources sharing an output stem write the same file twice: the second
    // export silently replaces the first, and under `--check` both are compared
    // against that one file and both can report "in sync". Sorted first, so the
    // pair named in the message is the same on every machine.
    let mut claimed: HashMap<&str, &str> = HashMap::new();
    for (source, out) in &pairs {
        if let Some(first) = claimed.insert(out.as_str(), source.as_str()) {
            return Err(ExportError::Config(format!(
                "{}: [dbc.exports] {first:?} and {source:?} both export to {out:?}; \
                 output stems must be unique or one export overwrites the other",
                path.display()
            )));
        }
    }

    Ok(pairs)
}

/// One phrasing for every wrong-type error, naming the dotted key, what was
/// wanted, and what the file actually holds.
fn wrong_type(path: &Path, key: &str, wanted: &str, got: &Value) -> ExportError {
    ExportError::Config(format!(
        "{}: {key} must be {wanted}, got {}",
        path.display(),
        got.type_str()
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use m1_workspace::config::M1ToolsConfig;

    /// The config shape this module defines, as documented in the README.
    const DOCUMENTED_EXAMPLE: &str = r#"[dbc]
src_dir = "UQR-EV/01.00/dbc"
out_dir = "dbc"

# source .m1dbc stem -> output .dbc stem
[dbc.exports]
"SBG DBC" = "SBG"
"Balls3EV25" = "Balls3EV25"
"#;

    /// A fresh, empty directory under the system temp dir.
    ///
    /// Discovery walks *up*, so a test dir must not sit under a real
    /// `m1-tools.toml`. Nothing above the system temp dir carries one, and the
    /// tests that expect a config put it in the test dir itself rather than
    /// relying on that.
    fn tmp_dir(name: &str) -> PathBuf {
        let mut p = std::env::temp_dir();
        p.push(format!("m1dbc-config-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&p);
        std::fs::create_dir_all(&p).expect("temp dir must be creatable");
        p
    }

    fn write_config(dir: &Path, body: &str) {
        std::fs::write(dir.join(CONFIG_FILE), body).expect("config must be writable");
    }

    /// A **relative** path that names `target` from any working directory: one
    /// `..` per named component of the CWD lands at the filesystem root, and
    /// `target`'s own components follow.
    ///
    /// Only *reads* the CWD — `std::env::set_current_dir` is process-global and
    /// these tests run in parallel, so changing it is never an option.
    fn relative_to_cwd(target: &Path) -> PathBuf {
        use std::path::Component;
        let cwd = std::env::current_dir().expect("a working directory");
        let mut rel = PathBuf::new();
        for _ in cwd
            .components()
            .filter(|c| matches!(c, Component::Normal(_)))
        {
            rel.push("..");
        }
        for c in target
            .components()
            .filter(|c| matches!(c, Component::Normal(_)))
        {
            rel.push(c);
        }
        assert!(
            rel.is_relative(),
            "the helper must hand load_dbc_config a relative path: {}",
            rel.display()
        );
        rel
    }

    #[test]
    fn the_documented_example_parses_to_both_pairs_sorted() {
        let dir = tmp_dir("documented");
        write_config(&dir, DOCUMENTED_EXAMPLE);

        let cfg = load_dbc_config(&dir)
            .expect("the documented example must load")
            .expect("a [dbc] section is present");
        assert_eq!(cfg.src_dir, PathBuf::from("UQR-EV/01.00/dbc"));
        assert_eq!(cfg.out_dir, PathBuf::from("dbc"));
        assert_eq!(
            cfg.exports,
            vec![
                ("Balls3EV25".to_string(), "Balls3EV25".to_string()),
                ("SBG DBC".to_string(), "SBG".to_string()),
            ],
            "pairs must come back sorted by source stem, not in document order"
        );
        assert_eq!(cfg.root, dir, "root is the directory holding m1-tools.toml");

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn discovery_walks_up_so_a_subdirectory_is_governed_by_the_repo_config() {
        let dir = tmp_dir("walkup");
        write_config(&dir, DOCUMENTED_EXAMPLE);
        let nested = dir.join("UQR-EV/01.00/dbc");
        std::fs::create_dir_all(&nested).expect("nested dir must be creatable");

        let cfg = load_dbc_config(&nested)
            .expect("discovery must walk up to the repo root")
            .expect("a [dbc] section is present");
        assert_eq!(
            cfg.root, dir,
            "root must be the config's directory, not the start dir"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A relative `start_dir` must be re-based before the walk-up.
    ///
    /// The walk climbs `parent()`s lexically, so an un-absolutised relative path
    /// exhausts its components at the working directory and the search stops
    /// there — `m1-can export` run from `dbc/` inside a repo would miss the
    /// `m1-tools.toml` above it and print "nothing to export".
    ///
    /// The truncation itself cannot be reproduced without a working directory
    /// inside the fixture tree, and `set_current_dir` is process-global (see
    /// [`relative_to_cwd`]). What this pins instead is the same fix's other
    /// observable half: the paths that come back are absolute — which they are
    /// not without the `std::path::absolute` call, so this fails on the
    /// unfixed code.
    #[test]
    fn a_relative_start_dir_yields_an_absolute_root() {
        let dir = tmp_dir("relative");
        write_config(&dir, DOCUMENTED_EXAMPLE);
        let rel = relative_to_cwd(&dir);

        let cfg = load_dbc_config(&rel)
            .expect("a relative start dir must load")
            .expect("a [dbc] section is present");
        assert!(
            cfg.root.is_absolute(),
            "root must not inherit the caller's relative base: {}",
            cfg.root.display()
        );
        // `std::path::absolute` keeps `..` components (it must not resolve
        // symlinks), so compare the directories rather than the spellings.
        assert_eq!(
            std::fs::canonicalize(&cfg.root).expect("root must exist"),
            std::fs::canonicalize(&dir).expect("the fixture dir must exist"),
            "root must still be the directory holding m1-tools.toml"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The error half of the same contract: a relative `start_dir` must not
    /// leave the caller with a path that only means something from the working
    /// directory this process happened to have.
    #[test]
    fn a_relative_start_dir_still_names_an_absolute_file_in_errors() {
        let dir = tmp_dir("relativeerror");
        write_config(
            &dir,
            "[dbc]\nout_dir = \"dbc\"\n\n[dbc.exports]\n\"A\" = \"A\"\n",
        );
        let rel = relative_to_cwd(&dir);

        let err = load_dbc_config(&rel).expect_err("a missing src_dir must be an error");
        let ExportError::Config(ref message) = err else {
            panic!("expected a config error, got {err:?}");
        };
        let named = PathBuf::from(
            message
                .split_once(": [dbc]")
                .expect("the message must lead with the file")
                .0,
        );
        assert!(
            named.is_absolute(),
            "the error must name the file absolutely: {message}"
        );
        assert_eq!(
            std::fs::canonicalize(&named).expect("the named file must exist"),
            std::fs::canonicalize(dir.join(CONFIG_FILE)).expect("the fixture config must exist"),
            "the error must name the config that was actually read: {message}"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_config_without_a_dbc_section_is_a_clean_no_op() {
        let dir = tmp_dir("nosection");
        write_config(&dir, "[format]\nindent_style = \"tab\"\n");

        assert_eq!(
            load_dbc_config(&dir),
            Ok(None),
            "a repo that does not export DBCs must not be an error"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn no_config_anywhere_is_a_clean_no_op() {
        // The dir is empty and nothing above the system temp dir carries an
        // m1-tools.toml, so discovery walks to the filesystem root and finds
        // nothing. That is the ship-anywhere case, not a failure.
        let dir = tmp_dir("noconfig");

        assert_eq!(
            load_dbc_config(&dir),
            Ok(None),
            "a repo with no m1-tools.toml at all must not be an error"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Assert the error names both the offending file and the offending key.
    fn assert_config_error(body: &str, name: &str, key: &str) {
        let dir = tmp_dir(name);
        write_config(&dir, body);
        let err = load_dbc_config(&dir).expect_err("an invalid [dbc] section must be an error");
        let ExportError::Config(ref message) = err else {
            panic!("expected a config error, got {err:?}");
        };
        assert!(
            message.contains(CONFIG_FILE),
            "the error must name the offending file: {message}"
        );
        assert!(
            message.contains(key),
            "the error must name the offending key {key}: {message}"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_missing_src_dir_is_an_error() {
        assert_config_error(
            "[dbc]\nout_dir = \"dbc\"\n\n[dbc.exports]\n\"A\" = \"A\"\n",
            "nosrc",
            "src_dir",
        );
    }

    #[test]
    fn a_missing_out_dir_is_an_error() {
        assert_config_error(
            "[dbc]\nsrc_dir = \"src\"\n\n[dbc.exports]\n\"A\" = \"A\"\n",
            "noout",
            "out_dir",
        );
    }

    #[test]
    fn an_empty_exports_table_is_an_error() {
        assert_config_error(
            "[dbc]\nsrc_dir = \"src\"\nout_dir = \"dbc\"\n\n[dbc.exports]\n",
            "emptyexports",
            "exports",
        );
    }

    #[test]
    fn a_missing_exports_table_is_an_error() {
        assert_config_error(
            "[dbc]\nsrc_dir = \"src\"\nout_dir = \"dbc\"\n",
            "noexports",
            "exports",
        );
    }

    #[test]
    fn a_src_dir_of_the_wrong_type_is_an_error() {
        assert_config_error(
            "[dbc]\nsrc_dir = 7\nout_dir = \"dbc\"\n\n[dbc.exports]\n\"A\" = \"A\"\n",
            "srctype",
            "src_dir",
        );
    }

    #[test]
    fn an_export_value_of_the_wrong_type_is_an_error() {
        assert_config_error(
            "[dbc]\nsrc_dir = \"src\"\nout_dir = \"dbc\"\n\n[dbc.exports]\n\"A\" = 7\n",
            "exportvaluetype",
            "A",
        );
    }

    /// The stem that made `--check` write outside its scratch directory.
    ///
    /// `Path::join` discards its base when the right-hand side is absolute, so
    /// an absolute output stem collapsed the scratch target and the committed
    /// path onto the same file: the run wrote a real `.dbc` into the working
    /// tree while promising to write nothing, then compared that file against
    /// itself and reported "in sync: yes" for an export that had never been
    /// generated. On the unfixed code this config loads without complaint.
    #[test]
    fn an_absolute_output_stem_is_refused() {
        let escape = std::env::temp_dir().join("m1dbc-escape/Sample");
        assert!(escape.is_absolute(), "the test needs an absolute stem");
        // A TOML *literal* string: no escape processing, so a Windows path's
        // backslashes survive as themselves.
        assert_config_error(
            &format!(
                "[dbc]\nsrc_dir = \"src\"\nout_dir = \"dbc\"\n\n\
                 [dbc.exports]\n\"A\" = '{}'\n",
                escape.display()
            ),
            "absoluteoutstem",
            "A",
        );
    }

    #[test]
    fn an_output_stem_that_climbs_out_of_its_directory_is_refused() {
        assert_config_error(
            "[dbc]\nsrc_dir = \"src\"\nout_dir = \"dbc\"\n\n[dbc.exports]\n\"A\" = \"../A\"\n",
            "climbingoutstem",
            "A",
        );
    }

    #[test]
    fn an_output_stem_that_is_only_a_parent_reference_is_refused() {
        // No separator and not absolute, so a "contains a separator" test would
        // let this through; `dbc/..` is the directory itself.
        assert_config_error(
            "[dbc]\nsrc_dir = \"src\"\nout_dir = \"dbc\"\n\n[dbc.exports]\n\"A\" = \"..\"\n",
            "parentoutstem",
            "A",
        );
    }

    #[test]
    fn a_source_stem_naming_a_subdirectory_is_refused() {
        // The source side is joined the same way, so it gets the same rule: a
        // stem names one file in `src_dir`, not a path from it.
        assert_config_error(
            "[dbc]\nsrc_dir = \"src\"\nout_dir = \"dbc\"\n\n[dbc.exports]\n\"nested/A\" = \"A\"\n",
            "nestedsourcestem",
            "nested/A",
        );
    }

    #[test]
    fn an_empty_output_stem_is_refused() {
        assert_config_error(
            "[dbc]\nsrc_dir = \"src\"\nout_dir = \"dbc\"\n\n[dbc.exports]\n\"A\" = \"\"\n",
            "emptyoutstem",
            "A",
        );
    }

    /// Two sources writing one file: generation loses the first export and
    /// `--check` compares both against the survivor, so both can report
    /// "in sync". Undetected on the unfixed code.
    #[test]
    fn two_sources_sharing_an_output_stem_are_refused() {
        let dir = tmp_dir("duplicateoutstem");
        write_config(
            &dir,
            "[dbc]\nsrc_dir = \"src\"\nout_dir = \"dbc\"\n\n\
             [dbc.exports]\n\"Alpha DBC\" = \"Shared\"\n\"Beta DBC\" = \"Shared\"\n",
        );

        let err = load_dbc_config(&dir).expect_err("a collapsed pair of exports must be an error");
        let ExportError::Config(ref message) = err else {
            panic!("expected a config error, got {err:?}");
        };
        for named in ["Alpha DBC", "Beta DBC", "Shared", CONFIG_FILE] {
            assert!(
                message.contains(named),
                "the error must name both source stems and the file, missing {named}: {message}"
            );
        }

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn an_empty_src_dir_is_refused() {
        assert_config_error(
            "[dbc]\nsrc_dir = \"\"\nout_dir = \"dbc\"\n\n[dbc.exports]\n\"A\" = \"A\"\n",
            "emptysrcdir",
            "src_dir",
        );
    }

    #[test]
    fn an_empty_out_dir_is_refused() {
        assert_config_error(
            "[dbc]\nsrc_dir = \"src\"\nout_dir = \"\"\n\n[dbc.exports]\n\"A\" = \"A\"\n",
            "emptyoutdir",
            "out_dir",
        );
    }

    #[test]
    fn an_absolute_out_dir_is_refused() {
        // Not the `--check` escape (the scratch dir replaces `out_dir` there),
        // but the same `Path::join` trap: an absolute directory discards the
        // repo root the rest of the section is written against.
        let elsewhere = std::env::temp_dir().join("m1dbc-elsewhere");
        assert_config_error(
            &format!(
                "[dbc]\nsrc_dir = \"src\"\nout_dir = '{}'\n\n[dbc.exports]\n\"A\" = \"A\"\n",
                elsewhere.display()
            ),
            "absoluteoutdir",
            "out_dir",
        );
    }

    #[test]
    fn a_dbc_key_that_is_not_a_table_is_an_error() {
        assert_config_error("dbc = \"yes\"\n", "notatable", "dbc");
    }

    #[test]
    fn unparseable_toml_is_an_error_rather_than_a_no_op() {
        let dir = tmp_dir("badtoml");
        write_config(&dir, "[dbc\nsrc_dir =\n");
        let err = load_dbc_config(&dir).expect_err("a broken config must never read as absent");
        assert!(
            err.to_string().contains(CONFIG_FILE),
            "the error must name the offending file: {err}"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn the_dbc_section_never_breaks_the_shared_workspace_parser() {
        // Cross-tool safety: `m1-tools.toml` is read by every M1 tool through
        // `M1ToolsConfig`. Adding `[dbc]` to a repo's config must stay invisible
        // to m1-fmt/m1-lint/the LSP — if this ever fails, the whole
        // config-file-driven design is unshippable.
        let (config, unknown) = M1ToolsConfig::from_toml_str_with_unknown_keys(DOCUMENTED_EXAMPLE)
            .expect("the shared parser must accept a config carrying [dbc]");
        assert!(
            config.format.indent_style.is_none() && config.lint.exclude.is_none(),
            "[dbc] must not bleed into any shared section"
        );
        // Invisible to the shared *schema*, but not invisible: the section
        // surfaces as one unknown key, so any tool that warns about unknown
        // keys will warn about every DBC-exporting repo's config until
        // `M1ToolsConfig` learns a `[dbc]` section. That consequence is the
        // reason this test exists, so it is asserted rather than left to an
        // `all()` that an empty list would satisfy vacuously.
        assert_eq!(
            unknown,
            vec!["dbc".to_string()],
            "[dbc] must reach the shared parser as exactly one unknown key — \
             the whole table, named once, and nothing else"
        );
    }
}
