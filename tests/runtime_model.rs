use m1_can::{CanDbcSource, CanDirection, CanEndian, CanFrameFormat, runtime_model_loaded};
use m1_typecheck::Project;
use m1_typecheck::parsed::{ParsedScript, parse_all};
use m1_typecheck::types::ValueType;

const PROJECT: &str = r#"<?xml version="1.0"?>
<Project>
 <Component Classname="BuiltIn.GroupCompound" Name="Root"/>
 <Component Classname="BuiltIn.GroupCompound" Name="Root.CAN"/>
 <Component Classname="BuiltIn.Constant" Name="Root.CAN.Active Bus">
  <Props Type="s32" Value="2"/>
 </Component>
 <Component Classname="BuiltIn.CAN.DBCRoot" Name="DBC"/>
 <Component Classname="BuiltIn.CAN.DBC" Name="DBC.Vehicle Network"/>
</Project>"#;

const DBC: &str = r#"<?xml version="1.0"?>
<DBC>
 <ComponentStream>
  <List>
   <Component Classname="BuiltIn.CAN.Signal"
              Name="Vehicle Network.Wheel Status.Wheel Speed" Endian="Big">
    <Props Type="s16" StartBit="10" Length="10" Multiplier="0.5" Offset="-2"/>
   </Component>
   <Component Classname="BuiltIn.CAN.Message" Name="Vehicle Network.Wheel Status">
    <Props CANId="4B3" IdType="Extended" DLC="10" Transmit="RX"/>
   </Component>
   <Component Classname="BuiltIn.CAN.Signal"
              Name="Vehicle Network.Wheel Status.Filtered Speed" Endian="Big">
    <Props Type="f32" StartBit="20" Length="20" Endian="Little"
           Multiplier="1.25" Offset="3.5"/>
   </Component>
   <Component Classname="BuiltIn.CAN.Signal"
              Name="Vehicle Network.Wheel Status.Ready Flag">
    <Props Type="bool" StartBit="3F" Length="20"/>
   </Component>
   <Component Classname="BuiltIn.CAN.DBC" Name="Vehicle Network"/>
  </List>
 </ComponentStream>
</DBC>"#;

fn snapshot(script: &str) -> (Project, Vec<ParsedScript>) {
    let project = Project::from_xml(PROJECT).expect("project parses");
    let scripts = parse_all(&[("CAN Init.m1scr".to_string(), script.to_string())]);
    (project, scripts)
}

#[test]
fn runtime_model_preserves_exact_paths_aliases_and_layout() {
    let (project, scripts) = snapshot("DBC.Vehicle Network.Init(Active Bus);\n");
    let source = CanDbcSource {
        path: "dbc/vendor files/Vehicle Network.m1dbc",
        bytes: DBC.as_bytes(),
    };
    let model = runtime_model_loaded(&project, &scripts, &[source]).expect("runtime model builds");

    assert!(model.skipped_scripts.is_empty());
    let module = &model.modules[0];
    assert_eq!(module.path, "Vehicle Network");
    assert_eq!(module.aliases, ["Vehicle Network", "DBC.Vehicle Network"]);
    assert_eq!(module.source_path, "dbc/vendor files/Vehicle Network.m1dbc");
    assert!(module.initialised);
    assert_eq!(module.bus.as_deref(), Some("Active Bus"));
    assert_eq!(module.bus_kind, "constant");
    assert_eq!(module.bus_value, Some(2));
    assert!(!module.bus_calibrated);

    let message = &module.messages[0];
    assert_eq!(message.path, "Vehicle Network.Wheel Status");
    assert_eq!(
        message.aliases,
        [
            "Vehicle Network.Wheel Status",
            "DBC.Vehicle Network.Wheel Status",
        ]
    );
    assert_eq!(message.frame_id, 0x4B3);
    assert_eq!(message.format, CanFrameFormat::Extended);
    assert_eq!(message.dlc, 10, "DLC is decimal, unlike the layout fields");
    assert_eq!(message.direction, Some(CanDirection::Rx));

    let signed = &message.signals[0];
    assert_eq!(signed.path, "Vehicle Network.Wheel Status.Wheel Speed");
    assert_eq!(
        signed.aliases,
        [
            "Vehicle Network.Wheel Status.Wheel Speed",
            "DBC.Vehicle Network.Wheel Status.Wheel Speed",
        ]
    );
    assert_eq!(signed.raw_type, "s16");
    assert_eq!(signed.raw_kind, ValueType::Integer);
    assert!(signed.signed);
    assert!(!signed.float);
    assert_eq!(signed.endian, CanEndian::Big);
    assert_eq!(signed.start_bit, 0x10);
    assert_eq!(signed.width, 0x10);
    assert_eq!(signed.scale, 0.5);
    assert_eq!(signed.offset, -2.0);

    let float = &message.signals[1];
    assert_eq!(float.raw_kind, ValueType::Float);
    assert!(!float.signed);
    assert!(float.float);
    assert_eq!(
        float.endian,
        CanEndian::Little,
        "Props Endian takes precedence over Component Endian"
    );
    assert_eq!(float.start_bit, 0x20);
    assert_eq!(float.width, 0x20);

    let boolean = &message.signals[2];
    assert_eq!(boolean.raw_kind, ValueType::Boolean);
    assert_eq!(boolean.start_bit, 0x3F);
    assert_eq!(boolean.width, 1, "bool forces one bit");
    assert_eq!(boolean.endian, CanEndian::Little);
}

#[test]
fn runtime_model_uses_only_the_supplied_snapshot() {
    let dir = tempfile::tempdir().unwrap();
    let project_path = dir.path().join("Project.m1prj");
    let script_path = dir.path().join("CAN Init.m1scr");
    let dbc_path = dir.path().join("Vehicle Network.m1dbc");
    std::fs::write(&project_path, PROJECT).unwrap();
    std::fs::write(&script_path, "DBC.Vehicle Network.Init(1);\n").unwrap();
    std::fs::write(&dbc_path, DBC).unwrap();

    let project = Project::load(&project_path).unwrap();
    let script_source = m1_workspace::read_text(&script_path).unwrap();
    let scripts = parse_all(&[("CAN Init.m1scr".to_string(), script_source)]);
    let dbc_bytes = std::fs::read(&dbc_path).unwrap();

    std::fs::write(&script_path, "DBC.Vehicle Network.Init(9);\n").unwrap();
    std::fs::write(&dbc_path, DBC.replace("CANId=\"4B3\"", "CANId=\"111\"")).unwrap();

    let model = runtime_model_loaded(
        &project,
        &scripts,
        &[CanDbcSource {
            path: "Vehicle Network.m1dbc",
            bytes: &dbc_bytes,
        }],
    )
    .expect("the borrowed snapshot remains usable");
    assert_eq!(model.modules[0].bus_value, Some(1));
    assert_eq!(model.modules[0].messages[0].frame_id, 0x4B3);
}

#[test]
fn runtime_model_binds_the_exact_source_alias_without_project_augmentation() {
    let (project, scripts) = snapshot("Vehicle Network.Init(3);\n");
    let model = runtime_model_loaded(
        &project,
        &scripts,
        &[CanDbcSource {
            path: "Vehicle Network.m1dbc",
            bytes: DBC.as_bytes(),
        }],
    )
    .expect("the source-root alias is part of the runtime snapshot");

    let module = &model.modules[0];
    assert!(module.initialised);
    assert_eq!(module.bus.as_deref(), Some("3"));
    assert_eq!(module.bus_value, Some(3));
}

#[test]
fn runtime_model_rejects_a_source_alias_shadowed_by_a_project_symbol() {
    let shadowed_project = PROJECT.replace(
        "</Project>",
        " <Component Classname=\"BuiltIn.GroupCompound\" Name=\"Vehicle Network\"/>\n</Project>",
    );
    let project = Project::from_xml(&shadowed_project).expect("shadowed project parses");
    let scripts = parse_all(&[(
        "CAN Init.m1scr".to_string(),
        "Vehicle Network.Init(3);\n".to_string(),
    )]);
    let error = runtime_model_loaded(
        &project,
        &scripts,
        &[CanDbcSource {
            path: "Vehicle Network.m1dbc",
            bytes: DBC.as_bytes(),
        }],
    )
    .expect_err("a source alias must not replace the Project's exact symbol identity");
    assert!(
        error.contains("module path `Vehicle Network` collides with a non-DBC symbol"),
        "{error}"
    );
}

#[test]
fn runtime_model_rejects_child_alias_collisions() {
    let bare_signal_project = PROJECT.replace(
        "</Project>",
        " <Component Classname=\"BuiltIn.Constant\" Name=\"Vehicle Network.Wheel Status.Wheel Speed\"><Props Type=\"s16\" Value=\"0\"/></Component>\n</Project>",
    );
    let project = Project::from_xml(&bare_signal_project).expect("collision project parses");
    let scripts = parse_all(&[(
        "CAN Init.m1scr".to_string(),
        "DBC.Vehicle Network.Init(3);\n".to_string(),
    )]);
    let source = CanDbcSource {
        path: "Vehicle Network.m1dbc",
        bytes: DBC.as_bytes(),
    };
    let error = runtime_model_loaded(&project, &scripts, &[source])
        .expect_err("the canonical signal alias must preserve Project identity");
    assert!(
        error.contains(
            "signal alias `Vehicle Network.Wheel Status.Wheel Speed` collides with a non-matching symbol"
        ),
        "{error}"
    );

    let qualified_message_project = PROJECT.replace(
        "</Project>",
        " <Component Classname=\"BuiltIn.GroupCompound\" Name=\"DBC.Vehicle Network.Wheel Status\"/>\n</Project>",
    );
    let project =
        Project::from_xml(&qualified_message_project).expect("qualified collision parses");
    let error = runtime_model_loaded(&project, &scripts, &[source])
        .expect_err("the generated qualified alias must preserve Project identity");
    assert!(
        error.contains(
            "message alias `DBC.Vehicle Network.Wheel Status` collides with a non-matching symbol"
        ),
        "{error}"
    );
}

#[test]
fn runtime_model_rejects_duplicate_and_mismatched_identities() {
    let (project, scripts) = snapshot("DBC.Vehicle Network.Init(2);\n");
    let source = CanDbcSource {
        path: "Vehicle Network.m1dbc",
        bytes: DBC.as_bytes(),
    };
    let duplicate = runtime_model_loaded(&project, &scripts, &[source, source])
        .expect_err("duplicate paths are ambiguous");
    assert!(
        duplicate.contains("duplicate CAN source path"),
        "{duplicate}"
    );

    let duplicate_project_xml = PROJECT.replace(
        "</Project>",
        " <Component Classname=\"BuiltIn.CAN.DBC\" Name=\"DBC.Vehicle Network\"/>\n</Project>",
    );
    let duplicate_project =
        Project::from_xml(&duplicate_project_xml).expect("duplicate project parses");
    let duplicate = runtime_model_loaded(&duplicate_project, &scripts, &[source])
        .expect_err("duplicate exact project aliases are ambiguous");
    assert!(
        duplicate.contains("loaded project repeats CAN module path `DBC.Vehicle Network`"),
        "{duplicate}"
    );

    let duplicate_message = DBC.replace(
        "   <Component Classname=\"BuiltIn.CAN.Message\" Name=\"Vehicle Network.Wheel Status\">",
        "   <Component Classname=\"BuiltIn.CAN.Message\" Name=\"Vehicle Network.Wheel Status\">\n    <Props DLC=\"8\"/>\n   </Component>\n   <Component Classname=\"BuiltIn.CAN.Message\" Name=\"Vehicle Network.Wheel Status\">",
    );
    let duplicate = runtime_model_loaded(
        &project,
        &scripts,
        &[CanDbcSource {
            path: "Vehicle Network.m1dbc",
            bytes: duplicate_message.as_bytes(),
        }],
    )
    .expect_err("a no-ID declaration still makes the exact message path ambiguous");
    assert!(
        duplicate.contains("repeats message path `Vehicle Network.Wheel Status`"),
        "{duplicate}"
    );

    let other_project = Project::from_xml(&PROJECT.replace("Vehicle Network", "Other Network"))
        .expect("project parses");
    let mismatch = runtime_model_loaded(&other_project, &scripts, &[source])
        .expect_err("a source not registered in the project must fail");
    assert!(
        mismatch.contains("project DBC module `DBC.Other Network` has no matching"),
        "{mismatch}"
    );
}

#[test]
fn runtime_model_refuses_unknown_endian_and_out_of_module_paths() {
    let (project, scripts) = snapshot("DBC.Vehicle Network.Init(2);\n");
    let bad_endian = DBC.replace("Endian=\"Big\"", "Endian=\"Middle\"");
    let error = runtime_model_loaded(
        &project,
        &scripts,
        &[CanDbcSource {
            path: "Vehicle Network.m1dbc",
            bytes: bad_endian.as_bytes(),
        }],
    )
    .expect_err("unknown byte order must not be guessed");
    assert!(error.contains("unsupported Endian `Middle`"), "{error}");

    let outside = DBC.replace("Vehicle Network.Wheel Status", "Other Network.Wheel Status");
    let error = runtime_model_loaded(
        &project,
        &scripts,
        &[CanDbcSource {
            path: "Vehicle Network.m1dbc",
            bytes: outside.as_bytes(),
        }],
    )
    .expect_err("message paths must sit below their exact source module");
    assert!(
        error.contains("outside parent `Vehicle Network`"),
        "{error}"
    );
}

#[test]
fn runtime_model_rejects_ids_outside_the_declared_frame_format() {
    let (project, scripts) = snapshot("DBC.Vehicle Network.Init(2);\n");
    for (id_type, can_id, maximum) in [
        ("Standard", "800", "0x7FF"),
        ("Extended", "20000000", "0x1FFFFFFF"),
    ] {
        let invalid = DBC.replace(
            "CANId=\"4B3\" IdType=\"Extended\"",
            &format!("CANId=\"{can_id}\" IdType=\"{id_type}\""),
        );
        let error = runtime_model_loaded(
            &project,
            &scripts,
            &[CanDbcSource {
                path: "Vehicle Network.m1dbc",
                bytes: invalid.as_bytes(),
            }],
        )
        .expect_err("a frame identifier must fit its declared format");
        assert!(
            error.contains(&format!(
                "CANId 0x{can_id}, outside the {id_type} identifier range 0x0..={maximum}"
            )),
            "{error}"
        );
    }
}

#[test]
fn runtime_model_rejects_comparable_stale_augmented_layout() {
    let dir = tempfile::tempdir().unwrap();
    let dbc_path = dir.path().join("Vehicle Network.m1dbc");
    std::fs::write(&dbc_path, DBC).unwrap();
    let mut project = Project::from_xml(PROJECT).expect("project parses");
    project
        .augment_dbc(&dbc_path, "dbc/Vehicle Network.m1dbc")
        .expect("project DBC augmentation succeeds");
    let scripts = parse_all(&[(
        "CAN Init.m1scr".to_string(),
        "DBC.Vehicle Network.Init(2);\n".to_string(),
    )]);

    runtime_model_loaded(
        &project,
        &scripts,
        &[CanDbcSource {
            path: "dbc/Vehicle Network.m1dbc",
            bytes: DBC.as_bytes(),
        }],
    )
    .expect("matching augmented metadata is accepted");

    let stale = DBC.replace("CANId=\"4B3\"", "CANId=\"111\"");
    let error = runtime_model_loaded(
        &project,
        &scripts,
        &[CanDbcSource {
            path: "dbc/Vehicle Network.m1dbc",
            bytes: stale.as_bytes(),
        }],
    )
    .expect_err("comparable stale layout must not be joined");
    assert!(
        error.contains("frame metadata disagrees with the loaded project snapshot"),
        "{error}"
    );
}

#[test]
fn runtime_model_rejects_duplicate_augmented_project_identities() {
    let dir = tempfile::tempdir().unwrap();
    let dbc_path = dir.path().join("Vehicle Network.m1dbc");
    std::fs::write(&dbc_path, DBC).unwrap();
    let mut project = Project::from_xml(PROJECT).expect("project parses");
    project
        .augment_dbc(&dbc_path, "dbc/Vehicle Network.m1dbc")
        .expect("first augmentation succeeds");
    project
        .augment_dbc(&dbc_path, "dbc/Vehicle Network.m1dbc")
        .expect("the dependency permits a second augmentation");
    let scripts = parse_all(&[(
        "CAN Init.m1scr".to_string(),
        "DBC.Vehicle Network.Init(2);\n".to_string(),
    )]);

    let error = runtime_model_loaded(
        &project,
        &scripts,
        &[CanDbcSource {
            path: "dbc/Vehicle Network.m1dbc",
            bytes: DBC.as_bytes(),
        }],
    )
    .expect_err("duplicate augmented identities must not be collapsed");
    assert!(
        error.contains(
            "loaded project repeats CAN signal path `Vehicle Network.Wheel Status.Wheel Speed`"
        ),
        "{error}"
    );
}
