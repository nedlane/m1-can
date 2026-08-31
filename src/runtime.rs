//! Read-only CAN layout for evaluators and other in-process runtimes.
//!
//! The export parser remains private. This module converts its exact M1
//! component identities into a public model and joins each DBC module to the
//! bus resolved from the caller's already-loaded [`Project`] and
//! [`ParsedScript`] snapshot. Callers also provide the exact `.m1dbc` bytes they
//! loaded, so this path never re-reads a source behind their back.

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use m1_typecheck::parsed::ParsedScript;
use m1_typecheck::project::Project;
use m1_typecheck::symbols::{CanDirection, Symbol};
use m1_typecheck::types::{ValueType, primitive_type};

use crate::can::{
    CanInitDto, CanSkippedScriptDto, binding_from_calls, loaded_init_calls_for_paths,
    registered_dbc_paths,
};
use crate::m1dbc::{M1DbcFile, M1Message, M1Signal, parse_m1dbc};

/// One caller-owned `.m1dbc` snapshot.
///
/// `path` is preserved verbatim in [`CanRuntimeModule::source_path`]. `bytes`
/// are decoded with the same tolerant Windows-1252-aware parser as DBC export.
#[derive(Debug, Clone, Copy)]
pub struct CanDbcSource<'a> {
    /// Snapshot identity, normally the same project-relative path used for
    /// m1-typecheck augmentation.
    pub path: &'a str,
    /// Exact source contents already read by the caller.
    pub bytes: &'a [u8],
}

/// Standard 11-bit or extended 29-bit CAN identifier format.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CanFrameFormat {
    Standard,
    Extended,
}

/// Signal byte order as declared by M1's `Endian` attribute.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CanEndian {
    Little,
    Big,
}

/// Complete read-only CAN layout for one loaded project snapshot.
#[derive(Debug, Clone, PartialEq)]
pub struct CanRuntimeModel {
    /// DBC modules sorted by exact source component path.
    pub modules: Vec<CanRuntimeModule>,
    /// Scripts whose Init calls were unsafe to inspect. Their omission makes
    /// affected bus bindings incomplete and remains visible to the caller.
    pub skipped_scripts: Vec<CanSkippedScriptDto>,
}

/// One exact DBC module and the bus selected by its Init calls.
#[derive(Debug, Clone, PartialEq)]
pub struct CanRuntimeModule {
    /// Exact `BuiltIn.CAN.DBC` component `Name` from the `.m1dbc`.
    pub path: String,
    /// Exact usable spellings, canonical source path first. A normal project
    /// includes both `BMU` and `DBC.BMU`.
    pub aliases: Vec<String>,
    /// Caller-provided source path, preserved verbatim.
    pub source_path: String,
    /// Whether at least one loaded script calls this module's Init method.
    pub initialised: bool,
    /// The agreed Init argument, absent when uninitialised or conflicting.
    pub bus: Option<String>,
    /// `literal`, `constant`, `parameter`, `channel`, `symbol`, `expression`,
    /// `none`, or `conflicting-init`, matching the inspection API.
    pub bus_kind: String,
    /// Resolved bus number, when the loaded project snapshot carries one.
    pub bus_value: Option<i64>,
    /// True when `bus_value` came from the loaded calibration.
    pub bus_calibrated: bool,
    /// Frames in `.m1dbc` document order.
    pub messages: Vec<CanRuntimeMessage>,
}

/// One exact CAN frame layout.
#[derive(Debug, Clone, PartialEq)]
pub struct CanRuntimeMessage {
    /// Exact `BuiltIn.CAN.Message` component `Name` from the `.m1dbc`.
    pub path: String,
    /// Exact bare and `DBC.`-qualified spellings, canonical source path first.
    pub aliases: Vec<String>,
    /// Numeric CAN identifier after parsing the source's hexadecimal spelling.
    pub frame_id: u32,
    /// Standard 11-bit or extended 29-bit identifier format.
    pub format: CanFrameFormat,
    /// Frame payload length in bytes. M1 stores DLC in decimal.
    pub dlc: u8,
    /// `None` when the source declares no RX/TX direction.
    pub direction: Option<CanDirection>,
    /// Signals in `.m1dbc` document order.
    pub signals: Vec<CanRuntimeSignal>,
}

/// One exact signal bit layout and physical scaling rule.
#[derive(Debug, Clone, PartialEq)]
pub struct CanRuntimeSignal {
    /// Exact `BuiltIn.CAN.Signal` component `Name` from the `.m1dbc`.
    pub path: String,
    /// Exact bare and `DBC.`-qualified spellings, canonical source path first.
    pub aliases: Vec<String>,
    /// Effective M1 storage type, including the format's absent-Type `u32`
    /// default.
    pub raw_type: String,
    /// Current m1-typecheck value family for `raw_type`.
    pub raw_kind: ValueType,
    /// Whether the raw integer representation is signed.
    pub signed: bool,
    /// Whether the raw representation is an IEEE float.
    pub float: bool,
    /// Exact byte order declared by the source, including its documented default.
    pub endian: CanEndian,
    /// M1 `StartBit`, parsed from hexadecimal.
    pub start_bit: u16,
    /// Signal width in bits, parsed from hexadecimal.
    pub width: u16,
    /// `physical = raw * scale + offset`.
    pub scale: f64,
    /// Additive term in the physical conversion.
    pub offset: f64,
}

struct ParsedSource {
    source_path: String,
    module_path: String,
    file: M1DbcFile,
}

/// Build a runtime CAN model from one caller-owned project, script, and DBC
/// snapshot.
///
/// This function performs no filesystem I/O. It rejects duplicate source or
/// component identities, sources that do not match the project's registered
/// DBC modules, orphaned signals, and unsupported layout attributes rather than
/// joining by vector position or guessing.
pub fn runtime_model_loaded(
    project: &Project,
    scripts: &[ParsedScript],
    sources: &[CanDbcSource<'_>],
) -> Result<CanRuntimeModel, String> {
    validate_project_can_identities(project)?;
    let mut source_paths = BTreeSet::new();
    let mut module_paths = BTreeSet::new();
    let mut parsed = Vec::with_capacity(sources.len());

    for source in sources {
        if source.path.is_empty() {
            return Err("CAN source path is empty".to_string());
        }
        if !source_paths.insert(source.path) {
            return Err(format!("duplicate CAN source path `{}`", source.path));
        }
        let stem = source_stem(source.path)?;
        let file = parse_m1dbc(source.bytes, stem)
            .map_err(|error| format!("CAN source `{}`: {error}", source.path))?;
        let module_path = match file.module_paths.as_slice() {
            [path] if !path.is_empty() => path.clone(),
            [] => {
                return Err(format!(
                    "CAN source `{}` has no BuiltIn.CAN.DBC component",
                    source.path
                ));
            }
            paths => {
                return Err(format!(
                    "CAN source `{}` has {} BuiltIn.CAN.DBC components; expected exactly one",
                    source.path,
                    paths.len()
                ));
            }
        };
        if !module_paths.insert(module_path.clone()) {
            return Err(format!(
                "duplicate CAN module path `{module_path}` across supplied sources"
            ));
        }
        validate_source_module_identity(project, source.path, &module_path)?;
        if let Some(path) = file.duplicate_message_paths.first() {
            return Err(format!(
                "CAN source `{}` repeats message path `{path}`",
                source.path
            ));
        }
        if let Some(path) = file.orphan_signal_paths.first() {
            return Err(format!(
                "CAN source `{}` has signal `{path}` without a surviving parent message",
                source.path
            ));
        }
        parsed.push(ParsedSource {
            source_path: source.path.to_string(),
            module_path,
            file,
        });
    }
    parsed.sort_by(|a, b| a.module_path.cmp(&b.module_path));
    validate_augmented_project(project, &parsed)?;

    let registered = registered_dbc_paths(project);
    let mut aliases_by_module: Vec<BTreeSet<String>> = vec![BTreeSet::new(); parsed.len()];
    let mut alias_owner: BTreeMap<String, usize> = BTreeMap::new();
    for (index, source) in parsed.iter().enumerate() {
        alias_owner.insert(source.module_path.clone(), index);
    }
    for alias in &registered {
        let candidates: Vec<usize> = parsed
            .iter()
            .enumerate()
            .filter_map(|(index, source)| {
                alias_matches(alias, &source.module_path).then_some(index)
            })
            .collect();
        match candidates.as_slice() {
            [] => {
                return Err(format!(
                    "project DBC module `{alias}` has no matching supplied CAN source"
                ));
            }
            [index] => {
                aliases_by_module[*index].insert(alias.clone());
                alias_owner.insert(alias.clone(), *index);
            }
            _ => {
                return Err(format!(
                    "project DBC module `{alias}` matches more than one supplied CAN source"
                ));
            }
        }
    }
    for (index, source) in parsed.iter().enumerate() {
        if aliases_by_module[index].is_empty() {
            return Err(format!(
                "CAN source `{}` declares module `{}` which is absent from the loaded project",
                source.source_path, source.module_path
            ));
        }
    }

    let init_aliases: Vec<String> = alias_owner.keys().cloned().collect();
    let (init_calls, skipped_scripts) =
        loaded_init_calls_for_paths(project, scripts, &init_aliases);
    let mut calls_by_module: Vec<Vec<CanInitDto>> = vec![Vec::new(); parsed.len()];
    for (alias, call) in init_calls {
        let Some(index) = alias_owner.get(&alias) else {
            return Err(format!(
                "Init call resolved to project DBC alias `{alias}` without a supplied source"
            ));
        };
        calls_by_module[*index].push(call);
    }

    let mut seen_messages = BTreeSet::new();
    let mut seen_signals = BTreeSet::new();
    let mut modules = Vec::with_capacity(parsed.len());
    for (index, source) in parsed.into_iter().enumerate() {
        let aliases = canonical_aliases(&source.module_path, &aliases_by_module[index]);
        validate_component_aliases(
            project,
            &source.source_path,
            "module",
            "BuiltIn.CAN.DBC",
            &aliases,
        )?;
        let calls = &calls_by_module[index];
        let (bus, bus_kind, bus_value, bus_calibrated) = binding_from_calls(calls);
        let mut messages = Vec::with_capacity(source.file.messages.len());
        for message in source.file.messages {
            validate_child_path(
                &source.source_path,
                "message",
                &message.source_path,
                &source.module_path,
            )?;
            if !seen_messages.insert(message.source_path.clone()) {
                return Err(format!(
                    "duplicate CAN message path `{}` across supplied sources",
                    message.source_path
                ));
            }
            messages.push(runtime_message(
                project,
                &source.source_path,
                &source.module_path,
                &aliases,
                message,
                &mut seen_signals,
            )?);
        }
        modules.push(CanRuntimeModule {
            path: source.module_path,
            aliases,
            source_path: source.source_path,
            initialised: !calls.is_empty(),
            bus,
            bus_kind,
            bus_value,
            bus_calibrated,
            messages,
        });
    }

    Ok(CanRuntimeModel {
        modules,
        skipped_scripts,
    })
}

/// Reject exact CAN identities that the loaded Project carries more than once.
/// In particular, augmenting the same DBC twice must not become a positional or
/// last-wins join when the authoritative source bytes are compared later.
fn validate_project_can_identities(project: &Project) -> Result<(), String> {
    let mut seen: BTreeMap<&str, Option<&str>> = BTreeMap::new();
    for symbol in project.symbols().iter() {
        let classname = symbol.classname.as_deref();
        if let Some(previous) = seen.get(symbol.path.as_str()).copied() {
            match (can_component_kind(previous), can_component_kind(classname)) {
                (Some(_), Some(kind)) if previous == classname => {
                    return Err(format!(
                        "loaded project repeats CAN {kind} path `{}`",
                        symbol.path
                    ));
                }
                (Some(previous_kind), Some(kind)) => {
                    return Err(format!(
                        "loaded project path `{}` is ambiguous between CAN {previous_kind} and CAN {kind} components",
                        symbol.path
                    ));
                }
                (Some(kind), None) | (None, Some(kind)) => {
                    return Err(format!(
                        "loaded project path `{}` is ambiguous between a CAN {kind} and another symbol",
                        symbol.path
                    ));
                }
                (None, None) => {}
            }
        } else {
            seen.insert(symbol.path.as_str(), classname);
        }
    }
    Ok(())
}

fn can_component_kind(classname: Option<&str>) -> Option<&'static str> {
    match classname {
        Some("BuiltIn.CAN.DBC") => Some("module"),
        Some("BuiltIn.CAN.Message") => Some("message"),
        Some("BuiltIn.CAN.Signal") => Some("signal"),
        _ => None,
    }
}

fn validate_source_module_identity(
    project: &Project,
    source_path: &str,
    module_path: &str,
) -> Result<(), String> {
    if let Some(symbol) = project
        .symbols()
        .iter()
        .find(|symbol| symbol.path == module_path)
        && symbol.classname.as_deref() != Some("BuiltIn.CAN.DBC")
    {
        return Err(format!(
            "CAN source `{source_path}` module path `{module_path}` collides with a non-DBC symbol in the loaded project"
        ));
    }
    Ok(())
}

fn validate_component_aliases(
    project: &Project,
    source_path: &str,
    kind: &str,
    expected_classname: &str,
    aliases: &[String],
) -> Result<(), String> {
    for alias in aliases {
        if let Some(symbol) = project.symbols().get(alias)
            && symbol.classname.as_deref() != Some(expected_classname)
        {
            return Err(format!(
                "CAN source `{source_path}` {kind} alias `{alias}` collides with a non-matching symbol in the loaded project"
            ));
        }
    }
    Ok(())
}

fn validate_message_symbol(
    source_path: &str,
    alias: &str,
    message: &M1Message,
    symbol: &Symbol,
) -> Result<(), String> {
    let direction = message_direction(source_path, message)?;
    let differs = symbol.can.as_ref().is_none_or(|can| {
        can.can_id != Some(message.frame_id)
            || can.extended != message.is_extended
            || can.transmit != direction
    });
    if differs {
        return Err(format!(
            "CAN source `{source_path}` message alias `{alias}` frame metadata disagrees with the loaded project snapshot"
        ));
    }
    // m1-typecheck v0.51 parses DLC through its hexadecimal attribute helper,
    // while m1-can correctly treats DLC as decimal, so DLC is not comparable.
    Ok(())
}

fn validate_signal_symbol(
    source_path: &str,
    alias: &str,
    signal: &M1Signal,
    symbol: &Symbol,
) -> Result<(), String> {
    let raw_kind = signal_raw_kind(source_path, signal)?;
    if symbol.value_type.is_known() && symbol.value_type != raw_kind {
        return Err(format!(
            "CAN source `{source_path}` signal alias `{alias}` raw type disagrees with the loaded project snapshot"
        ));
    }
    if let Some(can) = &symbol.can {
        let differs = can
            .start_bit
            .is_some_and(|value| value != u32::from(signal.start_bit))
            || (raw_kind != ValueType::Boolean
                && can
                    .length
                    .is_some_and(|value| value != u32::from(signal.length)))
            || can.multiplier.is_some_and(|value| value != signal.scale)
            || can.offset.is_some_and(|value| value != signal.offset);
        if differs {
            return Err(format!(
                "CAN source `{source_path}` signal alias `{alias}` bit layout or scaling disagrees with the loaded project snapshot"
            ));
        }
    }
    Ok(())
}

fn source_stem(path: &str) -> Result<&str, String> {
    Path::new(path)
        .file_stem()
        .and_then(|stem| stem.to_str())
        .filter(|stem| !stem.is_empty())
        .ok_or_else(|| format!("CAN source path `{path}` has no usable file stem"))
}

/// Cross-check fields retained by an already-augmented `Project`. Augmentation
/// is optional because v0.51 cannot consume caller-owned XML bytes directly;
/// when matching symbols are present, stale or differently-loaded layout must
/// not be silently joined to the authoritative supplied bytes.
fn validate_augmented_project(project: &Project, sources: &[ParsedSource]) -> Result<(), String> {
    for source in sources {
        let source_registrations: BTreeSet<&str> = project
            .symbols()
            .iter()
            .filter(|symbol| {
                symbol.classname.as_deref() == Some("BuiltIn.CAN.DBC")
                    && symbol.path == source.module_path
            })
            .filter_map(|symbol| symbol.filename.as_deref())
            .collect();
        if source_registrations.len() > 1 {
            return Err(format!(
                "CAN module `{}` has ambiguous augmented project source paths {}",
                source.module_path,
                display_set(&source_registrations),
            ));
        }
        if !source_registrations.is_empty()
            && !source_registrations.contains(source.source_path.as_str())
        {
            return Err(format!(
                "CAN source `{}` declares module `{}`, but the augmented project registered that exact module from {}",
                source.source_path,
                source.module_path,
                display_set(&source_registrations),
            ));
        }

        let augmented: Vec<_> = project
            .symbols()
            .iter()
            .filter(|symbol| symbol.filename.as_deref() == Some(source.source_path.as_str()))
            .collect();
        if augmented.is_empty() {
            continue;
        }

        let augmented_modules: BTreeSet<&str> = augmented
            .iter()
            .filter(|symbol| symbol.classname.as_deref() == Some("BuiltIn.CAN.DBC"))
            .map(|symbol| symbol.path.as_str())
            .collect();
        if augmented_modules.len() != 1 || !augmented_modules.contains(source.module_path.as_str())
        {
            return Err(format!(
                "CAN source `{}` module `{}` disagrees with augmented project module identities {}",
                source.source_path,
                source.module_path,
                display_set(&augmented_modules),
            ));
        }

        let expected_messages: BTreeSet<&str> = source
            .file
            .messages
            .iter()
            .map(|message| message.source_path.as_str())
            .collect();
        let augmented_messages: BTreeMap<&str, _> = augmented
            .iter()
            .filter(|symbol| symbol.classname.as_deref() == Some("BuiltIn.CAN.Message"))
            .filter(|symbol| {
                !symbol.path.ends_with("VECTOR INDEPENDENT SIG MSG")
                    && symbol.can.as_ref().and_then(|can| can.can_id).is_some()
            })
            .map(|symbol| (symbol.path.as_str(), *symbol))
            .collect();
        compare_paths(
            &source.source_path,
            "message",
            &expected_messages,
            &augmented_messages.keys().copied().collect(),
        )?;

        for message in &source.file.messages {
            let symbol = augmented_messages[message.source_path.as_str()];
            validate_message_symbol(&source.source_path, &message.source_path, message, symbol)?;
            // m1-typecheck v0.51 retains DLC in `CanMeta`, but parses it through
            // the hexadecimal attribute helper. m1-can correctly treats DLC as
            // decimal, so that one field is not comparable here.
        }

        let expected_signals: BTreeMap<&str, &M1Signal> = source
            .file
            .messages
            .iter()
            .flat_map(|message| message.signals.iter())
            .map(|signal| (signal.source_path.as_str(), signal))
            .collect();
        let augmented_signals: BTreeMap<&str, _> = augmented
            .iter()
            .filter(|symbol| symbol.classname.as_deref() == Some("BuiltIn.CAN.Signal"))
            .filter(|symbol| {
                parent_path(&symbol.path).is_some_and(|parent| expected_messages.contains(parent))
            })
            .map(|symbol| (symbol.path.as_str(), *symbol))
            .collect();
        compare_paths(
            &source.source_path,
            "signal",
            &expected_signals.keys().copied().collect(),
            &augmented_signals.keys().copied().collect(),
        )?;

        for (path, signal) in expected_signals {
            let symbol = augmented_signals[path];
            validate_signal_symbol(&source.source_path, path, signal, symbol)?;
        }
    }
    Ok(())
}

fn display_set(values: &BTreeSet<&str>) -> String {
    if values.is_empty() {
        "none".to_string()
    } else {
        values
            .iter()
            .map(|value| format!("`{value}`"))
            .collect::<Vec<_>>()
            .join(", ")
    }
}

fn compare_paths(
    source_path: &str,
    kind: &str,
    expected: &BTreeSet<&str>,
    augmented: &BTreeSet<&str>,
) -> Result<(), String> {
    if let Some(path) = expected.difference(augmented).next() {
        return Err(format!(
            "CAN source `{source_path}` {kind} `{path}` is absent from the augmented project snapshot"
        ));
    }
    if let Some(path) = augmented.difference(expected).next() {
        return Err(format!(
            "augmented project {kind} `{path}` is absent from supplied CAN source `{source_path}`"
        ));
    }
    Ok(())
}

fn parent_path(path: &str) -> Option<&str> {
    path.rsplit_once('.').map(|(parent, _)| parent)
}

fn alias_matches(alias: &str, module_path: &str) -> bool {
    alias == module_path || alias.strip_prefix("DBC.") == Some(module_path)
}

fn canonical_aliases(canonical: &str, registered: &BTreeSet<String>) -> Vec<String> {
    std::iter::once(canonical.to_string())
        .chain(
            registered
                .iter()
                .filter(|alias| alias.as_str() != canonical)
                .cloned(),
        )
        .collect()
}

fn child_aliases(
    source_path: &str,
    kind: &str,
    path: &str,
    module_path: &str,
    module_aliases: &[String],
) -> Result<Vec<String>, String> {
    let suffix = validate_child_path(source_path, kind, path, module_path)?;
    Ok(std::iter::once(path.to_string())
        .chain(
            module_aliases
                .iter()
                .filter(|alias| alias.as_str() != module_path)
                .map(|alias| format!("{alias}.{suffix}")),
        )
        .collect())
}

fn validate_child_path<'a>(
    source_path: &str,
    kind: &str,
    path: &'a str,
    parent: &str,
) -> Result<&'a str, String> {
    let prefix = format!("{parent}.");
    path.strip_prefix(&prefix)
        .filter(|suffix| !suffix.is_empty())
        .ok_or_else(|| {
            format!("CAN source `{source_path}` has {kind} `{path}` outside parent `{parent}`")
        })
}

fn runtime_message(
    project: &Project,
    source_path: &str,
    module_path: &str,
    module_aliases: &[String],
    message: M1Message,
    seen_signals: &mut BTreeSet<String>,
) -> Result<CanRuntimeMessage, String> {
    let format = message_format(source_path, &message)?;
    let direction = message_direction(source_path, &message)?;
    let aliases = child_aliases(
        source_path,
        "message",
        &message.source_path,
        module_path,
        module_aliases,
    )?;
    validate_component_aliases(
        project,
        source_path,
        "message",
        "BuiltIn.CAN.Message",
        &aliases,
    )?;
    for alias in &aliases {
        if let Some(symbol) = project.symbols().get(alias) {
            validate_message_symbol(source_path, alias, &message, symbol)?;
        }
    }
    let mut signals = Vec::with_capacity(message.signals.len());
    for signal in message.signals {
        validate_child_path(
            source_path,
            "signal",
            &signal.source_path,
            &message.source_path,
        )?;
        if !seen_signals.insert(signal.source_path.clone()) {
            return Err(format!(
                "duplicate CAN signal path `{}` across supplied sources",
                signal.source_path
            ));
        }
        signals.push(runtime_signal(
            project,
            source_path,
            module_path,
            module_aliases,
            signal,
        )?);
    }
    Ok(CanRuntimeMessage {
        path: message.source_path,
        aliases,
        frame_id: message.frame_id,
        format,
        dlc: message.dlc,
        direction,
        signals,
    })
}

fn runtime_signal(
    project: &Project,
    source_path: &str,
    module_path: &str,
    module_aliases: &[String],
    signal: M1Signal,
) -> Result<CanRuntimeSignal, String> {
    let raw_kind = signal_raw_kind(source_path, &signal)?;
    let endian = match signal.endian.as_str() {
        "Little" => CanEndian::Little,
        "Big" => CanEndian::Big,
        other => {
            return Err(format!(
                "CAN source `{source_path}` signal `{}` has unsupported Endian `{other}`",
                signal.source_path
            ));
        }
    };
    let aliases = child_aliases(
        source_path,
        "signal",
        &signal.source_path,
        module_path,
        module_aliases,
    )?;
    validate_component_aliases(
        project,
        source_path,
        "signal",
        "BuiltIn.CAN.Signal",
        &aliases,
    )?;
    for alias in &aliases {
        if let Some(symbol) = project.symbols().get(alias) {
            validate_signal_symbol(source_path, alias, &signal, symbol)?;
        }
    }
    Ok(CanRuntimeSignal {
        path: signal.source_path,
        aliases,
        raw_type: signal.raw_type,
        raw_kind,
        signed: signal.is_signed,
        float: signal.is_float,
        endian,
        start_bit: signal.start_bit,
        width: signal.length,
        scale: signal.scale,
        offset: signal.offset,
    })
}

fn message_format(source_path: &str, message: &M1Message) -> Result<CanFrameFormat, String> {
    let (format, maximum) = match message.id_type.as_str() {
        "Standard" => (CanFrameFormat::Standard, 0x7FF),
        "Extended" => (CanFrameFormat::Extended, 0x1FFF_FFFF),
        other => Err(format!(
            "CAN source `{source_path}` message `{}` has unsupported IdType `{other}`",
            message.source_path
        ))?,
    };
    if message.frame_id > maximum {
        return Err(format!(
            "CAN source `{source_path}` message `{}` has CANId 0x{:X}, outside the {} identifier range 0x0..=0x{maximum:X}",
            message.source_path, message.frame_id, message.id_type
        ));
    }
    Ok(format)
}

fn message_direction(
    source_path: &str,
    message: &M1Message,
) -> Result<Option<CanDirection>, String> {
    match message.direction.as_deref() {
        None => Ok(None),
        Some("RX") => Ok(Some(CanDirection::Rx)),
        Some("TX") => Ok(Some(CanDirection::Tx)),
        Some(other) => Err(format!(
            "CAN source `{source_path}` message `{}` has unsupported Transmit `{other}`",
            message.source_path
        )),
    }
}

fn signal_raw_kind(source_path: &str, signal: &M1Signal) -> Result<ValueType, String> {
    primitive_type(&signal.raw_type).ok_or_else(|| {
        format!(
            "CAN source `{source_path}` signal `{}` has unsupported Type `{}`",
            signal.source_path, signal.raw_type
        )
    })
}
