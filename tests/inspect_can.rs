// ---- CAN model (m1_can) ---------------------------------------------------

/// A DBC module declaring one or more `(message, can_id)` frames, as a `.m1dbc`.
/// `CANId` is written the way MoTeC writes it — hexadecimal without a prefix —
/// so the `can_id` each test passes round-trips through the hex parse.
fn m1dbc(module: &str, messages: &[(&str, u32)]) -> String {
    let mut s = String::from("<?xml version=\"1.0\"?>\n<DBC>\n <ComponentStream>\n  <List>\n");
    s.push_str(&format!(
        "   <Component Classname=\"BuiltIn.CAN.DBC\" Name=\"{module}\"/>\n"
    ));
    for (msg, id) in messages {
        s.push_str(&format!(
            "   <Component Classname=\"BuiltIn.CAN.Message\" Name=\"{module}.{msg}\">\n\
             \x20   <Props CANId=\"{id:X}\" DLC=\"8\" Transmit=\"RX\" Endian=\"Little\"/>\n\
             \x20  </Component>\n"
        ));
    }
    s.push_str("  </List>\n </ComponentStream>\n</DBC>\n");
    s
}

/// A project mirroring the real corpora's CAN layout: several `.m1dbc` modules
/// bound to buses by one `CAN Init` script, with the bus symbols valued the way
/// the real projects value them (a `.m1prj` constant, a `parameters.m1cfg` cell).
///
/// - `Alpha` (bus 1) and `Beta` (bus 2) both declare id 133 — different buses,
///   not a clash (this is exactly what EV-M1 does with `SBG DBC`/`DTI FSIC RL`).
/// - `Alpha` and `Epsilon` are both on bus 1 and both declare id 155 — a real clash.
/// - `Gamma` is bound to the parameter `Spare Bus` (cfg: 2) and `Delta` is never
///   initialised; they share id 144, which can be neither proven nor dismissed.
/// - `Zeta` is bound to the constant `Active Bus` (`.m1prj`: 0) and `Eta` to a
///   literal 0; they share id 177 — a clash, and a retune cannot undo it.
/// - `Theta` (`Spare Bus`, cfg: 2) and `Iota` (literal 1) share id 188 — safe,
///   but only for this calibration.
fn can_fixture() -> (tempfile::TempDir, std::path::PathBuf) {
    let dir = tempfile::tempdir().unwrap();
    let project_dir = dir.path().join("UQR-X").join("01.00");
    let scripts_dir = project_dir.join("Scripts");
    let dbc_dir = project_dir.join("dbc");
    std::fs::create_dir_all(&scripts_dir).unwrap();
    std::fs::create_dir_all(&dbc_dir).unwrap();

    std::fs::write(
        project_dir.join("Project.m1prj"),
        r#"<?xml version="1.0"?>
<MoTeCM1BuildSession>
 <Project Name="X" TargetHardware="ecu150">
  <ComponentStream>
   <List>
    <Component Classname="BuiltIn.GroupCompound" Name="Root.CAN"/>
    <Component Classname="BuiltIn.Parameter" Name="Root.CAN.Spare Bus">
     <Props Type="s32" Security="Calibration"/>
    </Component>
    <Component Classname="BuiltIn.Constant" Name="Root.CAN.Active Bus">
     <Props Type="s32" Value="0"/>
    </Component>
    <Component Classname="BuiltIn.FuncUser" Filename="CAN.CAN Init.m1scr" Name="Root.CAN.CAN Init"/>
    <Component Classname="BuiltIn.CAN.DBCRoot" Name="DBC"/>
    <Component Classname="BuiltIn.CAN.DBC" Name="DBC.Alpha"/>
    <Component Classname="BuiltIn.CAN.DBC" Name="DBC.Beta"/>
    <Component Classname="BuiltIn.CAN.DBC" Name="DBC.Gamma"/>
    <Component Classname="BuiltIn.CAN.DBC" Name="DBC.Delta"/>
    <Component Classname="BuiltIn.CAN.DBC" Name="DBC.Epsilon"/>
    <Component Classname="BuiltIn.CAN.DBC" Name="DBC.Zeta"/>
    <Component Classname="BuiltIn.CAN.DBC" Name="DBC.Eta"/>
    <Component Classname="BuiltIn.CAN.DBC" Name="DBC.Theta"/>
    <Component Classname="BuiltIn.CAN.DBC" Name="DBC.Iota"/>
   </List>
  </ComponentStream>
 </Project>
</MoTeCM1BuildSession>
"#,
    )
    .unwrap();

    for (module, messages) in [
        ("Alpha", &[("Status", 133u32), ("Extra", 155)][..]),
        ("Beta", &[("Status", 133)][..]),
        ("Gamma", &[("Status", 144)][..]),
        ("Delta", &[("Status", 144)][..]),
        ("Epsilon", &[("Status", 155)][..]),
        ("Zeta", &[("Status", 177)][..]),
        ("Eta", &[("Status", 177)][..]),
        ("Theta", &[("Status", 188)][..]),
        ("Iota", &[("Status", 188)][..]),
    ] {
        std::fs::write(
            dbc_dir.join(format!("{module}.m1dbc")),
            m1dbc(module, messages),
        )
        .unwrap();
    }

    std::fs::write(
        scripts_dir.join("CAN.CAN Init.m1scr"),
        "DBC.Alpha.Init(1);\nDBC.Beta.Init(2);\nDBC.Gamma.Init(Spare Bus);\nDBC.Epsilon.Init(1);\n\
         DBC.Zeta.Init(Active Bus);\nDBC.Eta.Init(0);\nDBC.Theta.Init(Spare Bus);\nDBC.Iota.Init(1);\n",
    )
    .unwrap();

    // The calibration: a parameter's value lives only here (real exports drop
    // the implicit `Root.` prefix, as this one does).
    std::fs::write(
        dir.path().join("parameters.m1cfg"),
        r#"<?xml version="1.0"?>
<Configuration>
 <Group Name="">
  <Parameter Name="CAN.Spare Bus">
   <Cell Type="s32"><![CDATA[2]]></Cell>
  </Parameter>
 </Group>
</Configuration>
"#,
    )
    .unwrap();

    let project = project_dir.join("Project.m1prj");
    (dir, project)
}

fn load_can_snapshot(
    project_path: &std::path::Path,
) -> (
    m1_typecheck::project::Project,
    Vec<m1_typecheck::parsed::ParsedScript>,
) {
    let root = project_path.parent().unwrap();
    let mut project = m1_typecheck::project::Project::load(project_path).unwrap();
    if let Some(config) = m1_workspace::find_config_file(root) {
        project = project.with_config(&config).unwrap();
    }
    for dbc in m1_workspace::find_dbc_files(root) {
        let relative = dbc.strip_prefix(root).unwrap().to_string_lossy();
        project.augment_dbc(&dbc, &relative).unwrap();
    }
    let sources: Vec<(String, String)> = m1_workspace::find_scripts(root)
        .into_iter()
        .map(|script| {
            (
                script.file_name().unwrap().to_string_lossy().into_owned(),
                m1_workspace::read_text(&script).unwrap(),
            )
        })
        .collect();
    let scripts = m1_typecheck::parsed::parse_all(&sources);
    (project, scripts)
}

#[test]
fn can_loaded_snapshot_does_not_reread_project_files() {
    let (_dir, project_path) = can_fixture();
    let (project, scripts) = load_can_snapshot(&project_path);

    std::fs::write(
        project_path
            .parent()
            .unwrap()
            .join("Scripts/CAN.CAN Init.m1scr"),
        "DBC.Alpha.Init(9);\n",
    )
    .unwrap();

    let out = m1_can::inspect_loaded(&project, &scripts, None, 200);
    let alpha = out
        .modules
        .iter()
        .find(|module| module.name == "Alpha")
        .unwrap();
    assert_eq!(alpha.bus.as_deref(), Some("1"));
    assert!(out.skipped_scripts.is_empty());
}

#[test]
fn can_names_a_syntax_error_script_skipped_from_init_analysis() {
    let (_dir, project_path) = can_fixture();
    std::fs::write(
        project_path
            .parent()
            .unwrap()
            .join("Scripts/CAN.CAN Init.m1scr"),
        "DBC.Alpha.Init(1);\nlocal x = ;\n",
    )
    .unwrap();
    let (project, scripts) = load_can_snapshot(&project_path);

    let out = m1_can::inspect_loaded(&project, &scripts, None, 200);
    let alpha = out
        .modules
        .iter()
        .find(|module| module.name == "Alpha")
        .unwrap();
    assert!(
        !alpha.initialised,
        "the unsafe script must not supply a bus binding"
    );
    assert_eq!(out.skipped_scripts.len(), 1);
    let skipped = &out.skipped_scripts[0];
    assert_eq!(skipped.script, "CAN.CAN Init.m1scr");
    assert!(
        skipped.reason.contains("syntax diagnostic"),
        "{}",
        skipped.reason
    );
    assert!(skipped.reason.contains("Init calls were not inspected"));
}

#[test]
fn can_names_an_excessively_deep_script_skipped_from_init_analysis() {
    let (_dir, project_path) = can_fixture();
    let depth = m1_core::MAX_RECURSION_DEPTH + 100;
    let source = format!(
        "DBC.Alpha.Init(1);\nlocal x = {}1{};\n",
        "(".repeat(depth),
        ")".repeat(depth),
    );
    std::fs::write(
        project_path
            .parent()
            .unwrap()
            .join("Scripts/CAN.CAN Init.m1scr"),
        source,
    )
    .unwrap();
    let (project, scripts) = load_can_snapshot(&project_path);

    let out = m1_can::inspect_loaded(&project, &scripts, None, 200);
    assert_eq!(out.skipped_scripts.len(), 1);
    assert!(
        out.skipped_scripts[0].reason.contains("nesting depth"),
        "{}",
        out.skipped_scripts[0].reason,
    );
}

#[cfg(unix)]
#[test]
fn can_names_a_non_utf8_script_skipped_before_parsing() {
    use std::ffi::OsString;
    use std::os::unix::ffi::OsStringExt;

    let (_dir, project_path) = can_fixture();
    let invalid_name = OsString::from_vec(b"invalid-\xff.m1scr".to_vec());
    std::fs::write(
        project_path
            .parent()
            .unwrap()
            .join("Scripts")
            .join(invalid_name),
        "DBC.Alpha.Init(9);\n",
    )
    .unwrap();

    let out = m1_can::inspect(&project_path, None, 200).expect("CAN inspection succeeds");

    assert_eq!(out.skipped_scripts.len(), 1);
    let skipped = &out.skipped_scripts[0];
    assert!(
        skipped.script.contains("invalid-"),
        "the lossy path should still identify the skipped input: {}",
        skipped.script,
    );
    assert!(
        skipped.reason.contains("not valid UTF-8"),
        "{}",
        skipped.reason,
    );
}

#[test]
fn can_binds_each_dbc_module_to_the_bus_its_init_call_names() {
    let (_dir, project) = can_fixture();
    let out = m1_can::inspect(&project, None, 200).expect("can inspect runs");

    let alpha = out.modules.iter().find(|m| m.name == "Alpha").unwrap();
    assert!(alpha.initialised);
    assert_eq!(alpha.bus.as_deref(), Some("1"));
    assert_eq!(alpha.bus_kind, "literal");
    assert_eq!(alpha.message_count, 2);
    let init = &alpha.init_calls[0];
    assert_eq!(init.script, "CAN.CAN Init.m1scr");
    assert_eq!(init.line, 1, "1-based line of the Init call");
    assert!(init.call.starts_with("DBC.Alpha.Init"), "{}", init.call);

    assert_eq!(alpha.bus_value, Some(1), "a literal resolves to itself");
    assert!(!alpha.bus_calibrated);

    // A parameter bus resolves through parameters.m1cfg — the only place a
    // parameter's value exists — and is marked as calibration-sourced.
    let gamma = out.modules.iter().find(|m| m.name == "Gamma").unwrap();
    assert_eq!(gamma.bus.as_deref(), Some("Spare Bus"));
    assert_eq!(gamma.bus_kind, "parameter");
    assert_eq!(gamma.bus_value, Some(2), "from the .m1cfg cell");
    assert!(gamma.bus_calibrated, "a retune can move it");

    // A constant bus resolves from the .m1prj and is NOT calibration-dependent.
    let zeta = out.modules.iter().find(|m| m.name == "Zeta").unwrap();
    assert_eq!(zeta.bus.as_deref(), Some("Active Bus"));
    assert_eq!(zeta.bus_kind, "constant");
    assert_eq!(zeta.bus_value, Some(0), "from the .m1prj Props Value");
    assert!(!zeta.bus_calibrated);
}

#[test]
fn can_matches_a_constant_bus_against_a_literal_one() {
    let (_dir, project) = can_fixture();
    let out = m1_can::inspect(&project, None, 200).expect("can inspect runs");

    // `Active Bus` is a constant with value 0, so `Init(Active Bus)` and
    // `Init(0)` are the same bus — provable without any calibration.
    let o = out
        .id_overlaps
        .iter()
        .find(|o| o.can_id == 177)
        .expect("id 177 is declared twice");
    assert_eq!(o.verdict, "same-bus", "{}", o.note);
    assert!(
        !o.depends_on_calibration,
        "a constant is fixed by the project, so no retune caveat: {}",
        o.note
    );
}

#[test]
fn can_flags_a_verdict_that_rests_on_calibration() {
    let (_dir, project) = can_fixture();
    let out = m1_can::inspect(&project, None, 200).expect("can inspect runs");

    // `Spare Bus` (cfg: 2) vs literal 1 — different buses today, but retuning
    // the parameter to 1 would make it a clash. The verdict says so.
    let o = out
        .id_overlaps
        .iter()
        .find(|o| o.can_id == 188)
        .expect("id 188 is declared twice");
    assert_eq!(o.verdict, "different-bus", "{}", o.note);
    assert!(
        o.depends_on_calibration,
        "the bus came from parameters.m1cfg: {}",
        o.note
    );
    assert!(
        o.note.contains("retune"),
        "the note must spell the caveat out: {}",
        o.note
    );
}

#[test]
fn can_reports_a_dbc_that_no_script_initialises() {
    let (_dir, project) = can_fixture();
    let out = m1_can::inspect(&project, None, 200).expect("can inspect runs");

    assert_eq!(out.uninitialised_modules, vec!["Delta".to_string()]);
    let delta = out.modules.iter().find(|m| m.name == "Delta").unwrap();
    assert!(!delta.initialised);
    assert!(delta.bus.is_none());
    assert_eq!(delta.bus_kind, "none");
}

#[test]
fn can_does_not_call_the_same_id_on_different_buses_a_clash() {
    let (_dir, project) = can_fixture();
    let out = m1_can::inspect(&project, None, 200).expect("can inspect runs");

    let o = out
        .id_overlaps
        .iter()
        .find(|o| o.can_id == 133)
        .expect("id 133 is declared by two modules");
    assert_eq!(o.verdict, "different-bus", "{}", o.note);
    assert_eq!(o.can_id_hex, "0x85");
    assert!(o.bus.is_none(), "no shared bus when the buses differ");
    let buses: Vec<_> = o.messages.iter().map(|m| m.bus.clone()).collect();
    assert_eq!(
        buses,
        vec![Some("1".to_string()), Some("2".to_string())],
        "each member carries the bus its module was Init'd on"
    );
}

#[test]
fn can_flags_the_same_id_on_the_same_bus() {
    let (_dir, project) = can_fixture();
    let out = m1_can::inspect(&project, None, 200).expect("can inspect runs");

    let o = out
        .id_overlaps
        .iter()
        .find(|o| o.can_id == 155)
        .expect("id 155 is declared twice");
    assert_eq!(o.verdict, "same-bus", "{}", o.note);
    assert_eq!(o.bus.as_deref(), Some("1"));
    let paths: Vec<_> = o.messages.iter().map(|m| m.path.as_str()).collect();
    assert_eq!(paths, vec!["Alpha.Extra", "Epsilon.Status"]);
}

#[test]
fn can_leaves_a_non_static_bus_undecided() {
    let (_dir, project) = can_fixture();
    let out = m1_can::inspect(&project, None, 200).expect("can inspect runs");

    let o = out
        .id_overlaps
        .iter()
        .find(|o| o.can_id == 144)
        .expect("id 144 is declared twice");
    assert_eq!(
        o.verdict, "unknown",
        "an uninitialised module has no bus at all, so nothing is proven: {}",
        o.note
    );
    assert!(o.bus.is_none());
    assert!(
        !o.depends_on_calibration,
        "an undecided verdict rests on nothing, calibration included"
    );
}

#[test]
fn can_lists_messages_with_id_direction_and_bus() {
    let (_dir, project) = can_fixture();
    let out = m1_can::inspect(&project, None, 200).expect("can inspect runs");

    assert_eq!(out.total_messages, 10);
    let m = out
        .messages
        .iter()
        .find(|m| m.path == "Beta.Status")
        .unwrap();
    assert_eq!(m.module, "Beta");
    assert_eq!(m.can_id, Some(133));
    assert_eq!(m.can_id_hex.as_deref(), Some("0x85"));
    assert_eq!(m.dlc, Some(8));
    assert_eq!(m.direction.as_deref(), Some("RX"));
    assert_eq!(m.bus.as_deref(), Some("2"));

    // The guidance travels with the data, so an agent reading only the tool
    // output still learns the bus rule.
    assert!(
        out.guidance.iter().any(|g| g.contains("Init")),
        "guidance must state the Init/bus rule"
    );
}

// The `.m1dbc` stores CAN ids in hex without a prefix, so a lettered id like
// the DTI corpus's `CANId="4B3"` must be recognised (the old decimal parse
// dropped it entirely), and `IdType="Extended"` must surface on the message.
#[test]
fn can_reads_hex_lettered_and_extended_ids() {
    let (_dir, project) = can_fixture();
    let dbc_dir = project.parent().unwrap().join("dbc");
    let mut xml = String::from(
        "<?xml version=\"1.0\"?>\n<DBC>\n <ComponentStream>\n  <List>\n\
         \x20  <Component Classname=\"BuiltIn.CAN.DBC\" Name=\"Kappa\"/>\n\
         \x20  <Component Classname=\"BuiltIn.CAN.Message\" Name=\"Kappa.Lettered\">\n\
         \x20   <Props CANId=\"4B3\" DLC=\"8\" Transmit=\"RX\"/>\n\
         \x20  </Component>\n\
         \x20  <Component Classname=\"BuiltIn.CAN.Message\" Name=\"Kappa.Wide\">\n\
         \x20   <Props IdType=\"Extended\" CANId=\"2968\" DLC=\"8\" Transmit=\"RX\"/>\n\
         \x20  </Component>\n",
    );
    xml.push_str("  </List>\n </ComponentStream>\n</DBC>\n");
    std::fs::write(dbc_dir.join("Kappa.m1dbc"), xml).unwrap();
    // Register the module in the .m1prj the way the real corpora do.
    let prj = std::fs::read_to_string(&project).unwrap().replace(
        "<Component Classname=\"BuiltIn.CAN.DBCRoot\" Name=\"DBC\"/>",
        "<Component Classname=\"BuiltIn.CAN.DBCRoot\" Name=\"DBC\"/>\n    \
         <Component Classname=\"BuiltIn.CAN.DBC\" Name=\"DBC.Kappa\"/>",
    );
    std::fs::write(&project, prj).unwrap();

    let out = m1_can::inspect(&project, Some("kappa"), 0).expect("can inspect runs");
    let lettered = out
        .messages
        .iter()
        .find(|m| m.path == "Kappa.Lettered")
        .expect("a hex-lettered CANId must not be dropped");
    assert_eq!(lettered.can_id, Some(0x4B3));
    assert_eq!(lettered.can_id_hex.as_deref(), Some("0x4B3"));
    assert!(!lettered.extended);

    let wide = out
        .messages
        .iter()
        .find(|m| m.path == "Kappa.Wide")
        .unwrap();
    assert_eq!(wide.can_id, Some(0x2968), "hex, not decimal 2968");
    assert!(wide.extended, "IdType=\"Extended\" surfaces on the message");

    // The guidance spells the hex rule out for agents reading raw XML.
    assert!(
        out.guidance
            .iter()
            .any(|g| g.to_lowercase().contains("hex")),
        "guidance must state that .m1dbc ids are hexadecimal"
    );
}

#[test]
fn can_filter_and_limit_narrow_messages_but_not_the_verdicts() {
    let (_dir, project) = can_fixture();
    let out = m1_can::inspect(&project, Some("alpha"), 1).expect("can inspect runs");

    assert_eq!(out.messages.len(), 1, "limit caps the returned list");
    assert!(out.messages[0].path.starts_with("Alpha."));
    assert_eq!(out.total_messages, 10, "total is the unfiltered count");
    assert!(
        out.id_overlaps.iter().any(|o| o.can_id == 144),
        "overlaps are computed over every message, not the filtered subset"
    );
}

#[test]
fn can_rejects_an_over_budget_project() {
    let dir = tempfile::tempdir().unwrap();
    let scripts = dir.path().join("Scripts");
    std::fs::create_dir_all(&scripts).unwrap();
    for i in 0..=2000 {
        std::fs::write(scripts.join(format!("s{i}.m1scr")), "A is True\n").unwrap();
    }
    std::fs::write(dir.path().join("Project.m1prj"), "<MoTeCM1BuildSession/>").unwrap();
    let err = m1_can::inspect(&dir.path().join("Project.m1prj"), None, 200).unwrap_err();
    assert!(err.contains("exceeds"), "unexpected error: {err}");
}

#[test]
fn ambiguous_bus_symbols_remain_unknown_in_both_loaded_apis() {
    for (first, second) in [
        (
            r#"Classname="BuiltIn.Constant" Name="Root.One.Bus"><Props Type="s32" Value="1""#,
            r#"Classname="BuiltIn.Parameter" Name="Root.Two.Bus"><Props Type="s32""#,
        ),
        (
            r#"Classname="BuiltIn.Parameter" Name="Root.One.Bus"><Props Type="s32""#,
            r#"Classname="BuiltIn.Parameter" Name="Root.Two.Bus"><Props Type="s32""#,
        ),
        (
            r#"Classname="BuiltIn.Constant" Name="Root.One.Bus"><Props Type="s32" Value="1""#,
            r#"Classname="BuiltIn.Constant" Name="Root.Two.Bus"><Props Type="s32" Value="1""#,
        ),
        (
            r#"Classname="BuiltIn.Constant" Name="Root.One.Bus"><Props Type="s32" Value="1""#,
            r#"Classname="BuiltIn.Constant" Name="Root.Two.Bus"><Props Type="s32" Value="2""#,
        ),
    ] {
        let (_dir, project_path) = can_fixture();
        let xml = std::fs::read_to_string(&project_path).unwrap();
        std::fs::write(
            &project_path,
            xml.replace(
                "</List>",
                &format!(
                    "<Component {first}/></Component><Component {second}/></Component></List>"
                ),
            ),
        )
        .unwrap();
        let (project, _) = load_can_snapshot(&project_path);
        let scripts = m1_typecheck::parsed::parse_all(&[(
            "CAN.CAN Init.m1scr".into(),
            "DBC.Alpha.Init(Bus); DBC.Beta.Init(Bus);".into(),
        )]);
        let out = m1_can::inspect_loaded(&project, &scripts, None, 200);
        let overlap = out
            .id_overlaps
            .iter()
            .find(|item| item.can_id == 133)
            .unwrap();
        assert_eq!(overlap.verdict, "unknown");
        for module in out
            .modules
            .iter()
            .filter(|module| ["Alpha", "Beta"].contains(&module.name.as_str()))
        {
            assert_eq!(module.bus_value, None);
            assert_eq!(module.bus_kind, "expression");
            assert!(!module.bus_calibrated);
        }
        let source_bytes: Vec<_> = m1_workspace::find_dbc_files(project_path.parent().unwrap())
            .into_iter()
            .map(|path| {
                let relative = path
                    .strip_prefix(project_path.parent().unwrap())
                    .unwrap()
                    .to_string_lossy()
                    .into_owned();
                (relative, std::fs::read(path).unwrap())
            })
            .collect();
        let sources: Vec<_> = source_bytes
            .iter()
            .map(|(path, bytes)| m1_can::CanDbcSource { path, bytes })
            .collect();
        let runtime = m1_can::runtime_model_loaded(&project, &scripts, &sources).unwrap();
        assert!(
            runtime
                .modules
                .iter()
                .filter(|module| ["Alpha", "Beta"].contains(&module.path.as_str()))
                .all(|module| module.bus_value.is_none() && module.bus_kind == "expression")
        );
    }
}

#[test]
fn standard_and_extended_ids_form_separate_overlap_groups() {
    let (_dir, project_path) = can_fixture();
    let dbc_dir = project_path.parent().unwrap().join("dbc");
    std::fs::write(
        dbc_dir.join("Alpha.m1dbc"),
        m1dbc("Alpha", &[("Standard", 0x123), ("Extended", 0x123)]).replacen(
            "CANId=\"123\" DLC",
            "CANId=\"123\" IdType=\"Extended\" DLC",
            1,
        ),
    )
    .unwrap();
    std::fs::write(
        dbc_dir.join("Beta.m1dbc"),
        m1dbc("Beta", &[("Standard", 0x123)]),
    )
    .unwrap();
    let (project, scripts) = load_can_snapshot(&project_path);
    let out = m1_can::inspect_loaded(&project, &scripts, Some("does not match"), 1);
    let overlaps: Vec<_> = out
        .id_overlaps
        .iter()
        .filter(|item| item.can_id == 0x123)
        .collect();
    assert_eq!(overlaps.len(), 1);
    assert!(!overlaps[0].extended);
    assert_eq!(overlaps[0].messages.len(), 2);
    let beta = m1dbc("Beta", &[("Standard", 0x123), ("Extended", 0x123)]).replacen(
        "CANId=\"123\" DLC",
        "CANId=\"123\" IdType=\"Extended\" DLC",
        1,
    );
    std::fs::write(dbc_dir.join("Beta.m1dbc"), beta).unwrap();
    let (project, scripts) = load_can_snapshot(&project_path);
    let out = m1_can::inspect_loaded(&project, &scripts, None, 200);
    let overlaps: Vec<_> = out
        .id_overlaps
        .iter()
        .filter(|item| item.can_id == 0x123)
        .collect();
    assert_eq!(overlaps.len(), 2);
    assert_ne!(overlaps[0].extended, overlaps[1].extended);
    assert!(overlaps.iter().all(|item| item.messages.len() == 2));
}

#[test]
fn malformed_dbc_cannot_yield_a_successful_partial_cli_report() {
    let (_dir, project_path) = can_fixture();
    let bad = project_path.parent().unwrap().join("dbc/Broken.m1dbc");
    std::fs::write(&bad, "<broken>").unwrap();
    let error = m1_can::inspect(&project_path, None, 200).unwrap_err();
    assert!(error.contains("Broken.m1dbc"), "{error}");
    let result = std::process::Command::new(env!("CARGO_BIN_EXE_m1-can"))
        .arg("inspect")
        .arg("--project")
        .arg(&project_path)
        .output()
        .unwrap();
    assert_eq!(result.status.code(), Some(2));
    assert!(String::from_utf8_lossy(&result.stderr).contains("Broken.m1dbc"));
    assert!(result.stdout.is_empty());
}

/// Check the public inspection and evaluator model against the same snapshot.
fn assert_scoped_bus(
    project: &m1_typecheck::project::Project,
    scripts: &[m1_typecheck::parsed::ParsedScript],
    sources: &[m1_can::CanDbcSource<'_>],
    expected_kind: &str,
    expected_value: Option<i64>,
) {
    for script in scripts {
        assert!(
            script.cst.syntax_diagnostics().is_empty(),
            "{}",
            script.name
        );
    }
    let inspection = m1_can::inspect_loaded(project, scripts, None, 200);
    assert!(inspection.skipped_scripts.is_empty());
    let alpha = inspection
        .modules
        .iter()
        .find(|m| m.name == "Alpha")
        .unwrap();
    assert!(alpha.initialised);
    assert_eq!(alpha.bus_kind, expected_kind);
    assert_eq!(alpha.bus_value, expected_value);
    assert!(!alpha.bus_calibrated);
    let overlap = inspection
        .id_overlaps
        .iter()
        .find(|o| o.can_id == 133)
        .unwrap();
    assert_eq!(
        overlap.verdict,
        if expected_value.is_some() {
            "different-bus"
        } else {
            "unknown"
        }
    );
    let runtime = m1_can::runtime_model_loaded(project, scripts, sources).unwrap();
    assert!(runtime.skipped_scripts.is_empty());
    let alpha_runtime = runtime.modules.iter().find(|m| m.path == "Alpha").unwrap();
    assert_eq!(alpha_runtime.bus_kind, alpha.bus_kind);
    assert_eq!(alpha_runtime.bus_value, alpha.bus_value);
    assert_eq!(alpha_runtime.bus_calibrated, alpha.bus_calibrated);
    assert!(alpha_runtime.initialised);
}

#[test]
fn scoped_bus_arguments_never_guess_shadowed_project_constants() {
    let (_dir, project_path) = can_fixture();
    let xml = std::fs::read_to_string(&project_path).unwrap().replace(
        "</List>",
        r#"<Component Classname="BuiltIn.Constant" Name="Root.CAN.Bus"><Props Type="s32" Value="1"/></Component>
<Component Classname="BuiltIn.Constant" Name="Root.CAN.In.Input"><Props Type="s32" Value="1"/></Component>
<Component Classname="BuiltIn.Constant" Name="Root.CAN.Out"><Props Type="s32" Value="1"/></Component>
<Component Classname="BuiltIn.FuncUserParam" Name="Root.CAN.Parameter" Filename="CAN.Parameter.m1scr"><Signature ReturnType="s32"><Params><Param Name="Input" Type="s32" Attrs="0"/></Params></Signature></Component>
<Component Classname="BuiltIn.Constant" Name="Root.CAN.Calculate"><Props Type="s32" Value="1"/></Component>
<Component Classname="BuiltIn.Constant" Name="Root.CAN.Bus 1"><Props Type="s32" Value="1"/></Component>
<Component Classname="BuiltIn.Constant" Name="Root.CAN.Active 1 Bus"><Props Type="s32" Value="1"/></Component>
<Component Classname="BuiltIn.Constant" Name="Root.CAN.Bus 1 Part 1"><Props Type="s32" Value="1"/></Component>
</List>"#,
    );
    std::fs::write(&project_path, xml).unwrap();
    let (project, _) = load_can_snapshot(&project_path);
    let bytes: Vec<_> = m1_workspace::find_dbc_files(project_path.parent().unwrap())
        .into_iter()
        .map(|p| {
            (
                format!("dbc/{}", p.file_name().unwrap().to_str().unwrap()),
                std::fs::read(p).unwrap(),
            )
        })
        .collect();
    let sources: Vec<_> = bytes
        .iter()
        .map(|(path, bytes)| m1_can::CanDbcSource { path, bytes })
        .collect();
    for (name, body, kind, value) in [
        ("unshadowed", "DBC.Alpha.Init(Bus);", "constant", Some(1)),
        (
            "interpolated local concrete read",
            "expand (N = 1 to 1) { local Bus $(N) = 2; DBC.Alpha.Init(Bus 1); }",
            "expression",
            None,
        ),
        (
            "interpolated local template read",
            "expand (N = 1 to 1) { local Bus $(N) = 2; DBC.Alpha.Init(Bus $(N)); }",
            "expression",
            None,
        ),
        (
            "interpolated local forward read",
            "DBC.Alpha.Init(Bus 1); expand (N = 1 to 1) { local Bus $(N) = 2; }",
            "expression",
            None,
        ),
        (
            "interpolated local suffix",
            "expand (N = 1 to 1) { local Active $(N) Bus = 2; DBC.Alpha.Init(Active 1 Bus); }",
            "expression",
            None,
        ),
        (
            "interpolated local repeated variable",
            "expand (N = 1 to 1) { local Bus $(N) Part $(N) = 2; DBC.Alpha.Init(Bus 1 Part 1); }",
            "expression",
            None,
        ),
        (
            "interpolated unrelated local",
            "expand (N = 1 to 1) { local Other $(N) = 2; DBC.Alpha.Init(Bus 1); }",
            "constant",
            Some(1),
        ),
        (
            "interpolated suffix does not match",
            "expand (N = 1 to 1) { local Bus $(N) Suffix = 2; DBC.Alpha.Init(Bus 1); }",
            "constant",
            Some(1),
        ),
        (
            "interpolated local qualified control",
            "expand (N = 1 to 1) { local Bus $(N) = 2; DBC.Alpha.Init(Root.CAN.Bus 1); }",
            "constant",
            Some(1),
        ),
        (
            "interpolated local literal control",
            "expand (N = 1 to 1) { local Bus $(N) = 2; DBC.Alpha.Init(1); }",
            "literal",
            Some(1),
        ),
        (
            "local",
            "local Bus = 2; DBC.Alpha.Init(Bus);",
            "expression",
            None,
        ),
        (
            "typed",
            "local <Integer> Bus = 2; DBC.Alpha.Init(Bus);",
            "expression",
            None,
        ),
        (
            "static",
            "static local Bus = 2; DBC.Alpha.Init(Bus);",
            "expression",
            None,
        ),
        (
            "uninitialized",
            "local Bus; DBC.Alpha.Init(Bus);",
            "expression",
            None,
        ),
        (
            "mutated",
            "local Bus = 1; Bus = 2; DBC.Alpha.Init(Bus);",
            "expression",
            None,
        ),
        (
            "self_initializer",
            "local Bus = DBC.Alpha.Init(Bus);",
            "expression",
            None,
        ),
        (
            "forward",
            "DBC.Alpha.Init(Bus); local Bus = 2;",
            "expression",
            None,
        ),
        (
            "block",
            "{ local Bus = 2; } DBC.Alpha.Init(Bus);",
            "expression",
            None,
        ),
        (
            "conditional",
            "if (True) { local Bus = 2; } DBC.Alpha.Init(Bus);",
            "expression",
            None,
        ),
        (
            "sibling_branch",
            "if (False) { local Bus = 2; } else { DBC.Alpha.Init(Bus); }",
            "expression",
            None,
        ),
        (
            "when",
            "when (1) { is (1) { local Bus = 2; } } DBC.Alpha.Init(Bus);",
            "expression",
            None,
        ),
        (
            "expand",
            "expand (Bus = 1 to 2) { DBC.Alpha.Init(Bus); }",
            "expression",
            None,
        ),
        (
            "after_expand",
            "expand (Bus = 1 to 2) { ; } DBC.Alpha.Init(Bus);",
            "expression",
            None,
        ),
        (
            "qualified",
            "local Bus = 2; DBC.Alpha.Init(Root.CAN.Bus);",
            "constant",
            Some(1),
        ),
        (
            "relative_qualified",
            "local Bus = 2; DBC.Alpha.Init(CAN.Bus);",
            "constant",
            Some(1),
        ),
        (
            "local_root",
            "local Root = 2; DBC.Alpha.Init(Root.CAN.Bus);",
            "constant",
            Some(1),
        ),
        (
            "unrelated_local",
            "local Other = 2; DBC.Alpha.Init(Bus);",
            "constant",
            Some(1),
        ),
        (
            "multiword",
            "local Active Bus = 2; DBC.Alpha.Init(Active Bus);",
            "expression",
            None,
        ),
        ("input", "DBC.Alpha.Init(In.Input);", "expression", None),
        ("output", "DBC.Alpha.Init(Out);", "expression", None),
        ("library", "DBC.Alpha.Init(Calculate);", "expression", None),
        ("arithmetic", "DBC.Alpha.Init(Bus + 1);", "expression", None),
    ] {
        let scripts = m1_typecheck::parsed::parse_all(&[(
            "CAN.CAN Init.m1scr".into(),
            format!("{body} DBC.Beta.Init(2);"),
        )]);
        eprintln!("scope case: {name}");
        assert_scoped_bus(&project, &scripts, &sources, kind, value);
    }
    // Script-local bindings must not leak into a neighboring function. Equal
    // source text from the two functions must not hide a conflicting binding.
    for reversed in [false, true] {
        let mut sources_text = vec![
            (
                "CAN.CAN Init.m1scr".into(),
                "DBC.Alpha.Init(Bus); DBC.Beta.Init(2);".into(),
            ),
            (
                "Other.m1scr".into(),
                "local Bus = 2; DBC.Alpha.Init(Bus);".into(),
            ),
        ];
        if reversed {
            sources_text.reverse();
        }
        let scripts = m1_typecheck::parsed::parse_all(&sources_text);
        assert_scoped_bus(&project, &scripts, &sources, "conflicting-init", None);
    }
    let scripts = m1_typecheck::parsed::parse_all(&[
        (
            "CAN.CAN Init.m1scr".into(),
            "DBC.Alpha.Init(Bus); DBC.Beta.Init(2);".into(),
        ),
        ("Other.m1scr".into(), "local Bus = 2;".into()),
    ]);
    assert_scoped_bus(&project, &scripts, &sources, "constant", Some(1));
    let scripts = m1_typecheck::parsed::parse_all(&[(
        "CAN.Parameter.m1scr".into(),
        "DBC.Alpha.Init(In.Input); DBC.Beta.Init(2);".into(),
    )]);
    assert_scoped_bus(&project, &scripts, &sources, "expression", None);
    // A unique suffix outside the script's actual group is not a binding.
    let scripts = m1_typecheck::parsed::parse_all(&[(
        "Other.m1scr".into(),
        "DBC.Alpha.Init(Bus); DBC.Beta.Init(2);".into(),
    )]);
    assert_scoped_bus(&project, &scripts, &sources, "expression", None);
}
