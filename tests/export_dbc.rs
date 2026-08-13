//! `m1-dbc export` end-to-end: config discovery, generation, and `--check`
//! drift.
//!
//! Every test builds a throwaway repo under the system temp dir — an
//! `m1-tools.toml`, a `src/` holding the sample `.m1dbc`, and nothing else —
//! and runs the real binary in it with `current_dir`, because the working
//! directory *is* the input: the verb takes no path argument and finds its
//! config by walking up from wherever it was invoked.
use std::path::{Path, PathBuf};
use std::process::Command;

/// The Task 2 fixture, embedded as raw bytes: it is stored Windows-1252 (a bare
/// `0xB0` in `°/s`), so it must be copied byte-for-byte rather than read as text.
const FIXTURE: &[u8] = include_bytes!("fixtures/Sample DBC.m1dbc");

/// The config every test starts from: one pair, `src/` in and `dbc/` out.
const CONFIG: &str = r#"[dbc]
src_dir = "src"
out_dir = "dbc"

# source .m1dbc stem -> output .dbc stem
[dbc.exports]
"Sample DBC" = "Sample"
"#;

/// A fresh, empty directory for one test, named after it and this process.
fn tmp_repo(name: &str) -> PathBuf {
    let mut p = std::env::temp_dir();
    p.push(format!("m1dbc-export-test-{}-{name}", std::process::id()));
    // A previous failed run may have left it behind; start from nothing.
    let _ = std::fs::remove_dir_all(&p);
    std::fs::create_dir_all(&p).expect("create the temp repo");
    p
}

/// A temp repo carrying the fixture at `src/Sample DBC.m1dbc` and `CONFIG`.
fn sample_repo(name: &str) -> PathBuf {
    let root = tmp_repo(name);
    std::fs::create_dir_all(root.join("src")).expect("create src/");
    std::fs::write(root.join("src").join("Sample DBC.m1dbc"), FIXTURE).expect("write the fixture");
    std::fs::write(root.join("m1-tools.toml"), CONFIG).expect("write m1-tools.toml");
    root
}

/// Run `m1-dbc export [args]` with `dir` as the working directory.
fn export_dbc(dir: &Path, args: &[&str]) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_m1-dbc"))
        .arg("export")
        .args(args)
        .current_dir(dir)
        .output()
        .expect("run m1-dbc")
}

#[test]
fn generation_writes_the_export_and_reports_the_counts() {
    let root = sample_repo("generate");

    let out = export_dbc(&root, &[]);
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        out.status.success(),
        "m1-dbc export failed: {}\n{stdout}",
        String::from_utf8_lossy(&out.stderr)
    );

    let written = std::fs::read_to_string(root.join("dbc").join("Sample.dbc"))
        .expect("dbc/Sample.dbc must have been written");
    assert!(
        written.starts_with("VERSION \"\""),
        "not a DBC file: {written}"
    );
    assert!(
        !written.contains('\r'),
        "the export must be written with LF line endings"
    );

    // The report is a contract: the pre-commit hook and CI read it, and it is
    // line-for-line the Python reference's so a reviewer can diff the two.
    for line in [
        "=== Sample ===",
        "  source .m1dbc   : 3 messages, 3 signals",
        "  convertible     : 1 messages, 2 signals (source minus skipped)",
        "  written .dbc    : 1 messages, 2 signals",
        "  round-tripped   : 1 messages, 2 signals",
        "  skipped         : 4",
    ] {
        assert!(
            stdout.contains(line),
            "report is missing {line:?}, got:\n{stdout}"
        );
    }
    assert!(
        stdout.contains("    - message 'Sample DBC.No Id' (no CANId)"),
        "the skipped list must name each dropped component, got:\n{stdout}"
    );

    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn check_passes_against_a_freshly_generated_export() {
    let root = sample_repo("check_in_sync");
    assert!(export_dbc(&root, &[]).status.success(), "generation failed");

    let out = export_dbc(&root, &["--check"]);
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        out.status.success(),
        "--check must pass right after generation, got:\n{stdout}"
    );
    assert!(
        stdout.contains("  in sync         : yes"),
        "--check must report the in-sync line, got:\n{stdout}"
    );

    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn check_fails_when_the_committed_export_drifts() {
    let root = sample_repo("check_drift");
    assert!(export_dbc(&root, &[]).status.success(), "generation failed");

    // One byte: a hand-edit of the generated file is exactly the drift the hook
    // exists to catch.
    let exported = root.join("dbc").join("Sample.dbc");
    let text = std::fs::read_to_string(&exported).unwrap();
    std::fs::write(&exported, text.replace("Status", "Statuz")).unwrap();

    let out = export_dbc(&root, &["--check"]);
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert_eq!(
        out.status.code(),
        Some(1),
        "a drifted export must exit 1, got:\n{stdout}"
    );
    assert!(
        stdout.contains("is out of sync with") && stdout.contains("m1-dbc export"),
        "the drift line must name both files and the fix, got:\n{stdout}"
    );
    // --check must never touch the working tree, drift or not.
    assert!(
        std::fs::read_to_string(&exported)
            .unwrap()
            .contains("Statuz"),
        "--check rewrote the committed export"
    );

    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn check_ignores_crlf_line_endings() {
    let root = sample_repo("check_crlf");
    assert!(export_dbc(&root, &[]).status.success(), "generation failed");

    // The committed exports are `eol=crlf` in git while the writer emits LF;
    // that difference is not drift.
    let exported = root.join("dbc").join("Sample.dbc");
    let text = std::fs::read_to_string(&exported).unwrap();
    std::fs::write(&exported, text.replace('\n', "\r\n")).unwrap();

    let out = export_dbc(&root, &["--check"]);
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        out.status.success(),
        "CRLF must compare equal to LF, got:\n{stdout}"
    );
    assert!(
        std::fs::read_to_string(&exported).unwrap().contains("\r\n"),
        "--check rewrote the committed export"
    );

    let _ = std::fs::remove_dir_all(&root);
}

/// `--check` on a repo that has never been exported: the committed file is
/// absent, which compares as empty — out of date, not a crash — and nothing is
/// written on the way to saying so.
#[test]
fn check_reports_a_missing_committed_export_as_out_of_date_without_writing_it() {
    let root = sample_repo("check_missing");

    let out = export_dbc(&root, &["--check"]);
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        !root.join("dbc").join("Sample.dbc").exists(),
        "--check wrote an export, got:\n{stdout}"
    );
    assert_eq!(
        out.status.code(),
        Some(1),
        "a missing committed export is out of date, got:\n{stdout}"
    );

    let _ = std::fs::remove_dir_all(&root);
}

/// `--check` is the only flag this verb has.
///
/// It inherited none of `m1-project`'s global edit flags: there is no
/// `--dry-run` (it meant "preview without writing", which is `--check`) and no
/// `--stdout` (there is no single document to print — the payload is one `.dbc`
/// per configured pair, and stdout already carries the report). A caller that
/// tries either gets clap's bad-invocation exit `2`, the same code an unusable
/// config gets, and nothing is written on the way out.
#[test]
fn an_unrecognised_flag_is_a_bad_invocation_that_writes_nothing() {
    let root = sample_repo("unknown_flag");

    for flag in ["--stdout", "--dry-run"] {
        let out = export_dbc(&root, &[flag]);
        let stderr = String::from_utf8_lossy(&out.stderr);
        assert_eq!(
            out.status.code(),
            Some(2),
            "{flag} is a bad invocation, not drift: {stderr}"
        );
        assert!(
            !root.join("dbc").join("Sample.dbc").exists(),
            "a refused invocation must write nothing ({flag})"
        );
    }

    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn runs_from_a_subdirectory_of_the_repo() {
    let root = sample_repo("subdir");
    let sub = root.join("sub").join("deeper");
    std::fs::create_dir_all(&sub).unwrap();

    // The hook runs wherever the developer happens to be; discovery walks up.
    let out = export_dbc(&sub, &[]);
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        out.status.success(),
        "m1-dbc export from a subdirectory failed: {}\n{stdout}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        root.join("dbc").join("Sample.dbc").exists(),
        "the export must land relative to the config, not the working directory"
    );

    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn a_repo_with_no_config_is_a_clean_no_op() {
    let root = tmp_repo("no_config");

    let out = export_dbc(&root, &[]);
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        out.status.success(),
        "a repo that exports no DBCs is not a failure, got:\n{stdout}"
    );
    assert!(
        stdout.contains("no [dbc] section in m1-tools.toml; nothing to export"),
        "the no-op must say why nothing happened, got:\n{stdout}"
    );

    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn an_unusable_dbc_section_exits_2() {
    let root = sample_repo("bad_config");
    std::fs::write(
        root.join("m1-tools.toml"),
        "[dbc]\nsrc_dir = \"src\"\nout_dir = \"dbc\"\n\n[dbc.exports]\n",
    )
    .unwrap();

    let out = export_dbc(&root, &[]);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert_eq!(
        out.status.code(),
        Some(2),
        "a broken [dbc] section is a configuration error, not drift: {stderr}"
    );
    assert!(
        stderr.contains("m1-tools.toml"),
        "the error must name the config file, got: {stderr}"
    );

    let _ = std::fs::remove_dir_all(&root);
}

/// `--check` promises to touch nothing outside its own temp directory.
///
/// An absolute output stem broke that promise: the scratch target is built with
/// `Path::join`, which discards its base when the right-hand side is absolute,
/// so the generated file landed at the stem's own path — and was then compared
/// against itself, reporting "in sync: yes" and exit 0 for an export that had
/// never been generated. The stem is refused at config load now, so the run
/// stops before anything is written.
#[test]
fn an_absolute_output_stem_is_refused_before_check_can_escape_its_scratch_dir() {
    let root = sample_repo("absolute_stem");
    let escape = root.join("escaped");
    std::fs::create_dir_all(&escape).unwrap();
    std::fs::write(
        root.join("m1-tools.toml"),
        format!(
            "[dbc]\nsrc_dir = \"src\"\nout_dir = \"dbc\"\n\n\
             [dbc.exports]\n\"Sample DBC\" = '{}'\n",
            escape.join("Sample").display()
        ),
    )
    .unwrap();

    let out = export_dbc(&root, &["--check"]);
    let stderr = String::from_utf8_lossy(&out.stderr);
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert_eq!(
        out.status.code(),
        Some(2),
        "an unusable export stem is a configuration error:\n{stdout}{stderr}"
    );
    assert!(
        !escape.join("Sample.dbc").exists(),
        "--check wrote a file outside its scratch directory:\n{stdout}"
    );
    assert!(
        !stdout.contains("in sync"),
        "a refused config must never report on sync state:\n{stdout}"
    );
    assert!(
        stderr.contains("m1-tools.toml") && stderr.contains("bare file stem"),
        "the error must name the file and say what a stem may be: {stderr}"
    );

    let _ = std::fs::remove_dir_all(&root);
}

/// Two sources writing one `.dbc`: generation lost the first export and
/// `--check` compared both against the survivor, reporting "in sync: yes"
/// twice. Refused at config load now.
#[test]
fn two_sources_sharing_an_output_stem_are_refused() {
    let root = sample_repo("duplicate_stem");
    std::fs::write(root.join("src").join("Copy DBC.m1dbc"), FIXTURE).unwrap();
    std::fs::write(
        root.join("m1-tools.toml"),
        "[dbc]\nsrc_dir = \"src\"\nout_dir = \"dbc\"\n\n\
         [dbc.exports]\n\"Sample DBC\" = \"Sample\"\n\"Copy DBC\" = \"Sample\"\n",
    )
    .unwrap();

    let out = export_dbc(&root, &[]);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert_eq!(
        out.status.code(),
        Some(2),
        "two exports collapsing onto one file is a configuration error: {stderr}"
    );
    assert!(
        !root.join("dbc").join("Sample.dbc").exists(),
        "a refused config must write nothing"
    );
    assert!(
        stderr.contains("Sample DBC") && stderr.contains("Copy DBC"),
        "the error must name both sources, got: {stderr}"
    );

    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn a_missing_source_exits_2() {
    let root = sample_repo("missing_source");
    std::fs::remove_file(root.join("src").join("Sample DBC.m1dbc")).unwrap();

    let out = export_dbc(&root, &[]);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert_eq!(
        out.status.code(),
        Some(2),
        "an unreadable source is a hard failure: {stderr}"
    );
    assert!(
        stderr.contains("Sample DBC.m1dbc"),
        "the error must name the file, got: {stderr}"
    );

    let _ = std::fs::remove_dir_all(&root);
}
