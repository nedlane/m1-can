//! `m1-can export` end-to-end: config discovery, generation, and `--check`
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

/// Run `m1-can export [args]` with `dir` as the working directory.
fn export_dbc(dir: &Path, args: &[&str]) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_m1-can"))
        .arg("export")
        .args(args)
        .current_dir(dir)
        .output()
        .expect("run m1-can")
}

#[test]
fn failed_candidates_preserve_existing_exports_and_create_nothing() {
    let root = tempfile::tempdir().unwrap();
    std::fs::create_dir(root.path().join("src")).unwrap();
    std::fs::write(root.path().join("m1-tools.toml"), CONFIG).unwrap();
    let source = root.path().join("src/Sample DBC.m1dbc");
    let destination = root.path().join("dbc/Sample.dbc");
    for invalid in [
        "<broken>",
        r#"<ComponentStream><List><Component Classname="BuiltIn.CAN.Message" Name="Sample DBC.Frame" Sender="Engine ECU"><Props CANId="123"/></Component></List></ComponentStream>"#,
        r#"<ComponentStream><List><Component Classname="BuiltIn.CAN.Message" Name="Sample DBC.Frame"><Props CANId="123"/></Component><Component Classname="BuiltIn.CAN.Signal" Name="Sample DBC.Frame.Value"><Props Multiplier="NaN"/></Component></List></ComponentStream>"#,
    ] {
        std::fs::write(&source, FIXTURE).unwrap();
        assert!(export_dbc(root.path(), &[]).status.success());
        let previous = std::fs::read(&destination).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&destination, std::fs::Permissions::from_mode(0o600)).unwrap();
        }
        std::fs::write(&source, invalid).unwrap();
        for args in [&[][..], &["--check"][..]] {
            let result = export_dbc(root.path(), args);
            assert_eq!(result.status.code(), Some(2), "{result:?}");
            assert_eq!(std::fs::read(&destination).unwrap(), previous, "{invalid}");
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                assert_eq!(
                    std::fs::metadata(&destination)
                        .unwrap()
                        .permissions()
                        .mode()
                        & 0o777,
                    0o600
                );
            }
        }
        std::fs::remove_file(&destination).unwrap();
        let result = export_dbc(root.path(), &[]);
        assert_eq!(result.status.code(), Some(2), "{result:?}");
        assert!(!destination.exists(), "{invalid}");
    }
}

#[test]
fn generation_writes_the_export_and_reports_the_counts() {
    let root = sample_repo("generate");

    let out = export_dbc(&root, &[]);
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        out.status.success(),
        "m1-can export failed: {}\n{stdout}",
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
fn injected_frame_in_unit_is_rejected_before_replacing_the_export() {
    let root = sample_repo("preflight-count-mismatch");
    let source = r#"<ComponentStream><List>
<Component Classname="BuiltIn.CAN.Message" Name="Sample DBC.Status">
  <Props CANId="100" DLC="8"/>
</Component>
<Component Classname="BuiltIn.CAN.Signal" Name="Sample DBC.Status.Value">
  <Props StartBit="0" Length="8">
    <Locale><Default Unit="&quot; Vector__XXX&#10;&#10;BO_ 2047 Extra: 8 Vector__XXX&#10; SG_ Extra : 0|8@1+ (1,0) [0|0] &quot;"/></Locale>
  </Props>
</Component>
</List></ComponentStream>"#;
    std::fs::write(root.join("src/Sample DBC.m1dbc"), source).unwrap();
    std::fs::create_dir(root.join("dbc")).unwrap();
    let target = root.join("dbc/Sample.dbc");
    let previous = b"previous usable export\n";
    std::fs::write(&target, previous).unwrap();

    let out = export_dbc(&root, &[]);
    assert_eq!(
        out.status.code(),
        Some(2),
        "invalid unit must fail before generating an export: {}\n{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(std::fs::read(&target).unwrap(), previous);
    assert!(
        String::from_utf8_lossy(&out.stderr)
            .contains("Unit contains an unsupported trailing backslash or control character"),
        "input validation must reject the injected extra frame"
    );
    std::fs::remove_dir_all(root).unwrap();
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
        stdout.contains("is out of sync with") && stdout.contains("m1-can export"),
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
        "m1-can export from a subdirectory failed: {}\n{stdout}",
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

fn source_with(message: &str, signal: &str, unit: &str) -> String {
    format!(
        r#"<ComponentStream><List>
<Component Classname="BuiltIn.CAN.Message" Name="Sample DBC.Frame"><Props {message}/></Component>
<Component Classname="BuiltIn.CAN.Signal" Name="Sample DBC.Frame.Value"><Props {signal}><Locale><Default Unit="{unit}"/></Locale></Props></Component>
</List></ComponentStream>"#
    )
}

#[test]
fn malformed_values_and_unrepresentable_layouts_never_replace_the_export() {
    let root = tempfile::tempdir().unwrap();
    std::fs::create_dir(root.path().join("src")).unwrap();
    std::fs::write(root.path().join("m1-tools.toml"), CONFIG).unwrap();
    let source = root.path().join("src/Sample DBC.m1dbc");
    let destination = root.path().join("dbc/Sample.dbc");
    std::fs::write(&source, FIXTURE).unwrap();
    assert!(export_dbc(root.path(), &[]).status.success());
    let previous = std::fs::read(&destination).unwrap();
    let message = r#"CANId="123" DLC="8""#;
    let signal = r#"Type="u32" StartBit="0" Length="10""#;
    let mut cases = vec![
        "<other/>".to_string(),
        "<ComponentStream/>".to_string(),
        "<ComponentStream><Lsist/></ComponentStream>".to_string(),
    ];
    for attrs in [
        r#"CANId="800""#,
        r#"CANId="80000123""#,
        r#"CANId="20000000" IdType="Extended""#,
        r#"CANId="123" IdType="Extened""#,
        r#"CANId="123" DLC="0""#,
    ] {
        cases.push(source_with(attrs, signal, ""));
    }
    for attrs in [
        r#"Type="f16" Length="10""#,
        r#"Type="f32" Length="10""#,
        r#"Type="f64" Length="20""#,
        r#"Type="int8""#,
        r#"Endian="Littel""#,
        r#"Length="0""#,
        r#"StartBit="3F" Length="2""#,
        r#"StartBit="38" Length="2" Endian="Big""#,
        r#"Multiplier="NaN""#,
        r#"Offset="inf""#,
    ] {
        cases.push(source_with(message, attrs, ""));
    }
    for unit in ["tail\\", "&#10;", "&#13;", "&#9;"] {
        cases.push(source_with(message, signal, unit));
    }
    for invalid in cases {
        std::fs::write(&source, &invalid).unwrap();
        for args in [&[][..], &["--check"][..]] {
            let result = export_dbc(root.path(), args);
            assert_eq!(
                result.status.code(),
                Some(2),
                "accepted {invalid}: {result:?}"
            );
            assert!(String::from_utf8_lossy(&result.stderr).contains("Sample DBC.m1dbc"));
            assert_eq!(std::fs::read(&destination).unwrap(), previous, "{invalid}");
            assert!(!String::from_utf8_lossy(&result.stdout).contains("in sync         : yes"));
        }
    }
}

#[test]
fn valid_layout_boundaries_and_escaped_units_remain_exportable() {
    let root = tempfile::tempdir().unwrap();
    std::fs::create_dir(root.path().join("src")).unwrap();
    std::fs::write(root.path().join("m1-tools.toml"), CONFIG).unwrap();
    for (message, signal, unit) in [
        (
            r#"CANId="7FF" DLC="8""#,
            r#"StartBit="3F" Length="1""#,
            "in&quot;",
        ),
        (
            r#"CANId="1FFFFFFF" IdType="Extended" DLC="8""#,
            r#"StartBit="3F" Length="8" Endian="Big""#,
            "a\\b",
        ),
        (
            r#"CANId="123" DLC="10""#,
            r#"StartBit="7" Length="10" Endian="Big" Type="s32""#,
            "a\\&quot;b",
        ),
        (
            r#"CANId="123""#,
            r#"Type="f32" Length="20" Multiplier="0" Offset="-1""#,
            "",
        ),
        (r#"CANId="123""#, r#"Type="f64" Length="40""#, ""),
        (
            r#"CANId="123""#,
            r#"Type="bool" StartBit="3F" Length="FFFF""#,
            "",
        ),
        (
            r#"CANId="123""#,
            r#"Type="u32" Length="C" Multiplier="-2""#,
            "",
        ),
    ] {
        std::fs::write(
            root.path().join("src/Sample DBC.m1dbc"),
            source_with(message, signal, unit),
        )
        .unwrap();
        for args in [&[][..], &["--check"][..]] {
            let result = export_dbc(root.path(), args);
            assert!(
                result.status.success(),
                "{message} {signal} {unit}: {result:?}"
            );
        }
    }
    for empty in [
        "<ComponentStream><List/></ComponentStream>",
        "<DBC><ComponentStream><List/></ComponentStream></DBC>",
    ] {
        std::fs::write(root.path().join("src/Sample DBC.m1dbc"), empty).unwrap();
        assert!(export_dbc(root.path(), &[]).status.success());
    }
}

#[test]
fn rejected_pair_does_not_prevent_other_exports() {
    let root = tempfile::tempdir().unwrap();
    std::fs::create_dir(root.path().join("src")).unwrap();
    std::fs::create_dir(root.path().join("dbc")).unwrap();
    std::fs::write(
        root.path().join("m1-tools.toml"),
        "[dbc]\nsrc_dir=\"src\"\nout_dir=\"dbc\"\n[dbc.exports]\nA=\"A\"\nB=\"B\"\nC=\"C\"\n",
    )
    .unwrap();
    std::fs::write(root.path().join("src/A.m1dbc"), FIXTURE).unwrap();
    std::fs::write(root.path().join("src/B.m1dbc"), "<other/>").unwrap();
    std::fs::write(root.path().join("src/C.m1dbc"), FIXTURE).unwrap();
    std::fs::write(root.path().join("dbc/B.dbc"), "previous").unwrap();
    let result = export_dbc(root.path(), &[]);
    assert_eq!(result.status.code(), Some(2));
    assert!(root.path().join("dbc/A.dbc").is_file());
    assert!(root.path().join("dbc/C.dbc").is_file());
    assert_eq!(
        std::fs::read_to_string(root.path().join("dbc/B.dbc")).unwrap(),
        "previous"
    );
}

#[test]
fn ambiguous_export_names_and_frame_identities_are_rejected() {
    let root = tempfile::tempdir().unwrap();
    std::fs::create_dir(root.path().join("src")).unwrap();
    std::fs::write(root.path().join("m1-tools.toml"), CONFIG).unwrap();
    for (first, second) in [("A B", "A_B"), ("A-B", "A B"), ("A__B", "A_B"), ("!", "?")] {
        for signals in [false, true] {
            let components = if signals {
                format!(
                    r#"<Component Classname="BuiltIn.CAN.Message" Name="Sample DBC.Frame"><Props CANId="1"/></Component>
<Component Classname="BuiltIn.CAN.Signal" Name="Sample DBC.Frame.{first}"><Props Length="8"/></Component>
<Component Classname="BuiltIn.CAN.Signal" Name="Sample DBC.Frame.{second}"><Props StartBit="8" Length="8"/></Component>"#
                )
            } else {
                format!(
                    r#"<Component Classname="BuiltIn.CAN.Message" Name="Sample DBC.{first}"><Props CANId="1"/></Component>
<Component Classname="BuiltIn.CAN.Message" Name="Sample DBC.{second}"><Props CANId="2"/></Component>"#
                )
            };
            std::fs::write(
                root.path().join("src/Sample DBC.m1dbc"),
                format!("<ComponentStream><List>{components}</List></ComponentStream>"),
            )
            .unwrap();
            let result = export_dbc(root.path(), &[]);
            assert_eq!(result.status.code(), Some(2), "{result:?}");
            let error = String::from_utf8_lossy(&result.stderr);
            assert!(
                error.contains(first) && error.contains(second) && error.contains("both export as"),
                "{error}"
            );
            assert!(!root.path().join("dbc/Sample.dbc").exists());
        }
    }
    for extended in [false, true] {
        let format = if extended { "Extended" } else { "Standard" };
        let source = format!(
            r#"<ComponentStream><List>
<Component Classname="BuiltIn.CAN.Message" Name="Sample DBC.One"><Props CANId="123"/></Component>
<Component Classname="BuiltIn.CAN.Message" Name="Sample DBC.Two"><Props CANId="123" IdType="{format}"/></Component>
</List></ComponentStream>"#
        );
        std::fs::write(root.path().join("src/Sample DBC.m1dbc"), source).unwrap();
        let result = export_dbc(root.path(), &[]);
        assert_eq!(
            result.status.code(),
            Some(if extended { 0 } else { 2 }),
            "{result:?}"
        );
    }
}
