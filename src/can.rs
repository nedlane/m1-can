//! The project's CAN model: which `.m1dbc` module is bound to which CAN bus,
//! and which CAN identifiers actually collide.
//!
//! A `.m1dbc` on its own carries no bus. It is bound to one by a
//! `DBC.<Name>.Init(<bus>)` call in *some* script (both corpora keep every
//! `Init` in one `CAN Init` script), and M1 Build rejects a project that uses a
//! DBC it never initialised (Error 1375, mirrored by `m1-typecheck` T107). So a
//! CAN identifier is only meaningful *per bus*: two messages that share an id
//! collide **only if their modules were initialised on the same bus**. The real
//! EV corpus relies on this — `SBG DBC.Init(2)` and `DTI FSIC RL.Init(1)` both
//! declare ids 133/173, and that is correct, not a clash.
//!
//! This module reconstructs that picture for an agent: every DBC module with
//! its `Init` call sites and bus argument, every message with its id and
//! resolved bus, and every repeated id classified as `same-bus` (a real clash),
//! `different-bus` (provably fine) or `unknown` (the bus argument is not a
//! static value — typically a calibratable Parameter — so nothing can be
//! proven either way).

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use m1_core::{Kind, Node};
use m1_typecheck::parsed::ParsedScript;
use m1_typecheck::project::Project;
use m1_typecheck::symbols::{CanDirection, SymbolKind};
use m1_typecheck::typer::path_text;
use schemars::JsonSchema;
use serde::Serialize;

/// Guidance returned with every response, so the rule travels with the data.
const GUIDANCE: [&str; 7] = [
    "`.m1dbc` files store `CANId` (and the other integer attributes) in HEXADECIMAL without a \
     prefix — `CANId=\"133\"` is 0x133 = 307, not decimal 133. `can_id` here is the correctly \
     parsed number and `can_id_hex` matches the file's own spelling; never re-read the raw XML \
     as decimal.",
    "A `.m1dbc` has no CAN bus of its own: a script must bind it with `DBC.<Name>.Init(<bus>)` \
     (conventionally one `CAN Init` script). A DBC that is used but never initialised is M1 Build \
     Error 1375 (m1-typecheck T107).",
    "CAN identifiers are per bus. Two messages sharing an id do NOT conflict when their modules \
     were initialised on different buses — check `bus`/`bus_value` before ever reporting a clash.",
    "A bus argument that names a symbol is resolved to a number where the project knows one: a \
     constant's `.m1prj` `Value`, or a parameter's cell in `parameters.m1cfg`. That number is in \
     `bus_value`.",
    "`verdict: \"different-bus\"` means proven safe, `\"same-bus\"` a real clash, and \
     `\"unknown\"` that at least one bus has no known value (uninitialised, or a symbol the \
     project carries no value for) — say so, do not guess.",
    "`depends_on_calibration: true` means the verdict rests on a parameter's value in \
     `parameters.m1cfg`: it holds for this calibration, and a retune can change it. Verdicts from \
     literals and constants alone are retune-proof.",
    "A non-empty `skipped_scripts` list means some scripts could not safely contribute their \
     `DBC.<Name>.Init(...)` calls. Treat module bus bindings and overlap verdicts as incomplete \
     until those scripts are fixed.",
];
/// How a module's bus argument was classified.
fn bus_kind_str(k: SymbolKind) -> &'static str {
    match k {
        SymbolKind::Constant => "constant",
        SymbolKind::Parameter => "parameter",
        SymbolKind::Channel => "channel",
        _ => "symbol",
    }
}

/// One `DBC.<Name>.Init(<bus>)` call site.
#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct CanInitDto {
    /// Script file the `Init` call lives in.
    pub script: String,
    /// 1-based line of the call within that script.
    pub line: u32,
    /// The call as written (`DBC.Datalogger.Init(Datalogger Bus)`).
    pub call: String,
    /// The bus argument verbatim (`1`, `Active Bus`, …).
    pub bus: String,
    /// What the argument is: `literal`, `constant`, `parameter`, `channel`,
    /// `symbol` or `expression`.
    pub bus_kind: String,
    /// The bus number the argument resolves to: the literal itself, a constant's
    /// `.m1prj` value, or a parameter's `parameters.m1cfg` cell. `None` when the
    /// project carries no value for it.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub bus_value: Option<i64>,
    /// True when `bus_value` came from the current calibration (a parameter
    /// cell) rather than a literal or a project constant — a retune moves it.
    pub bus_calibrated: bool,
}

/// A DBC module (one `.m1dbc`) and the bus it was initialised on.
#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct CanModuleDto {
    /// Module name as scripts write it (`BMU`, `PDM15 DBC`).
    pub name: String,
    /// The `.m1dbc` the module came from, relative to the project directory.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub file: Option<String>,
    /// Messages declared by this module.
    pub message_count: usize,
    /// False when no script calls its `Init` — M1 Build Error 1375 territory,
    /// and its messages have no bus to compare against.
    pub initialised: bool,
    /// The bus argument, when every `Init` call agrees on one; `None` when the
    /// module is uninitialised or its calls disagree.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub bus: Option<String>,
    /// Classification of `bus` (`literal`, `parameter`, …); `none` when
    /// uninitialised, `conflicting-init` when the `Init` calls disagree.
    pub bus_kind: String,
    /// The bus number `bus` resolves to — see [`CanInitDto::bus_value`].
    #[serde(skip_serializing_if = "Option::is_none")]
    pub bus_value: Option<i64>,
    /// True when `bus_value` is a calibration value that a retune moves.
    pub bus_calibrated: bool,
    /// Every `Init` call site found for this module.
    pub init_calls: Vec<CanInitDto>,
}

/// One CAN message declared by a `.m1dbc`.
#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct CanMessageDto {
    /// Full symbol path (`AMK.Actual Values 1 Left`).
    pub path: String,
    /// Owning DBC module.
    pub module: String,
    /// The parsed CAN identifier. The `.m1dbc` stores `CANId` in hexadecimal
    /// without a prefix (`CANId="4B3"` is 0x4B3) — this is the resulting number.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub can_id: Option<u32>,
    /// `can_id` in hex, the form DBC/CAN tooling usually prints (and the form
    /// the `.m1dbc` itself uses).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub can_id_hex: Option<String>,
    /// True when the message declares `IdType="Extended"` — a 29-bit id rather
    /// than a standard 11-bit one.
    pub extended: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub dlc: Option<u32>,
    /// `RX` (the M1 receives) or `TX` (the M1 transmits); absent when the
    /// `.m1dbc` declares no direction.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub direction: Option<String>,
    /// The bus its module was initialised on, when known.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub bus: Option<String>,
    /// Classification of `bus` — see [`CanModuleDto::bus_kind`].
    pub bus_kind: String,
    /// The bus number `bus` resolves to — see [`CanInitDto::bus_value`].
    #[serde(skip_serializing_if = "Option::is_none")]
    pub bus_value: Option<i64>,
    /// True when `bus_value` is a calibration value that a retune moves.
    pub bus_calibrated: bool,
}

/// One member of a repeated-id group.
#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct CanOverlapMemberDto {
    pub path: String,
    pub module: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub bus: Option<String>,
    pub bus_kind: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub bus_value: Option<i64>,
    pub bus_calibrated: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub direction: Option<String>,
}

/// A CAN id declared by more than one message, with the verdict on whether that
/// is actually a clash.
#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct CanIdOverlapDto {
    pub can_id: u32,
    pub can_id_hex: String,
    /// `same-bus` (a real clash), `different-bus` (proven safe — the buses
    /// resolve to different numbers), or `unknown` (at least one bus has no
    /// known value, so nothing is proven).
    pub verdict: String,
    /// The shared bus, when the verdict is `same-bus`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub bus: Option<String>,
    /// True when the verdict rests on a **calibration** value from
    /// `parameters.m1cfg` rather than on literals and project constants alone —
    /// i.e. it holds for the loaded calibration, and a retune can change it.
    pub depends_on_calibration: bool,
    /// Why the verdict came out this way, in one sentence.
    pub note: String,
    pub messages: Vec<CanOverlapMemberDto>,
}

/// A script whose `DBC.<Name>.Init(...)` calls could not be inspected safely.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, JsonSchema)]
pub struct CanSkippedScriptDto {
    /// Script name from the loaded project snapshot.
    pub script: String,
    /// Why the script was excluded from CAN bus-binding analysis.
    pub reason: String,
}

/// The CAN picture for one project.
#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct CanOutcome {
    /// Every DBC module in the project, with its bus binding.
    pub modules: Vec<CanModuleDto>,
    /// Modules used/declared but never `Init`-ed (M1 Build Error 1375 / T107).
    pub uninitialised_modules: Vec<String>,
    /// CAN ids declared by more than one message, each with a verdict.
    pub id_overlaps: Vec<CanIdOverlapDto>,
    /// The messages themselves (subject to `filter`/`limit`).
    pub messages: Vec<CanMessageDto>,
    /// Total messages in the project, before `filter`/`limit`.
    pub total_messages: usize,
    /// Scripts excluded from `Init`-call analysis because their syntax or
    /// nesting made their reference shapes unsafe to inspect.
    pub skipped_scripts: Vec<CanSkippedScriptDto>,
    /// How to read all of the above — the bus rule, restated.
    pub guidance: Vec<String>,
}

/// A bus argument reduced to something comparable.
#[derive(Debug, Clone, PartialEq)]
enum Bus {
    /// A known bus number: written literally, or resolved from the symbol the
    /// argument names — a constant's `.m1prj` value, or a parameter's cell in
    /// `parameters.m1cfg`. `calibrated` marks the latter: the number is the
    /// project's *current calibration*, and a retune moves it.
    Number { value: i64, calibrated: bool },
    /// A symbol (or expression) whose value is not known, kept verbatim.
    Symbolic(String),
}

/// The outcome of comparing two buses: whether they are the same, and whether
/// that answer leaned on a calibratable value.
struct BusVerdict {
    same: bool,
    calibrated: bool,
}

impl Bus {
    /// `Some(...)` when the two buses are provably the same or provably
    /// different; `None` when it cannot be decided.
    ///
    /// Two *different* symbolic spellings stay undecidable — distinct symbols
    /// with no known value may still carry the same bus number at run time. The
    /// same symbol on both sides is decided without any calibration caveat: even
    /// if it is a calibratable parameter, both modules move with it together.
    fn same_as(&self, other: &Bus) -> Option<BusVerdict> {
        match (self, other) {
            (
                Bus::Number {
                    value: a,
                    calibrated: ca,
                },
                Bus::Number {
                    value: b,
                    calibrated: cb,
                },
            ) => Some(BusVerdict {
                same: a == b,
                calibrated: *ca || *cb,
            }),
            (Bus::Symbolic(a), Bus::Symbolic(b)) if a == b => Some(BusVerdict {
                same: true,
                calibrated: false,
            }),
            _ => None,
        }
    }
}

/// Collapse the whitespace M1 allows inside a multi-word name so `Active  Bus`
/// and `Active Bus` compare equal.
fn normalise(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// The leaf (last `.`-segment) of a symbol path — `DBC.BMU` and bare `BMU` share
/// the leaf `BMU`, which is how the two registrations of one DBC are unified
/// (same rule as `m1_typecheck::dbc_init`).
fn leaf(path: &str) -> &str {
    path.rsplit('.').next().unwrap_or(path)
}

/// Walk a script for `<dbc>.Init(<bus>)` calls, pushing the exact registered
/// DBC path and call. Callers decide how aliases map onto one module identity.
fn collect_init_calls(
    n: Node,
    dbc_paths: &[String],
    script: &str,
    resolve_bus: &dyn Fn(&str) -> BusArg,
    out: &mut Vec<(String, CanInitDto)>,
) {
    if n.kind() == Kind::CallExpression
        && let Some(callee) = n
            .named_children()
            .into_iter()
            .find(|c| matches!(c.kind(), Kind::Identifier | Kind::MemberExpression))
    {
        let text = path_text(callee);
        if let Some(obj) = dbc_paths.iter().find(|obj| {
            text.len() == obj.len() + 5 && text.starts_with(*obj) && text.ends_with(".Init")
        }) {
            let bus = n
                .children()
                .into_iter()
                .find(|c| c.kind() == Kind::ArgumentList)
                .and_then(|args| args.named_children().into_iter().next())
                .map(|a| normalise(a.text()))
                .unwrap_or_default();
            let arg = resolve_bus(&bus);
            out.push((
                obj.to_string(),
                CanInitDto {
                    script: script.to_string(),
                    line: n.range().start.line + 1,
                    call: normalise(n.text()),
                    bus,
                    bus_kind: arg.kind,
                    bus_value: arg.value,
                    bus_calibrated: arg.calibrated,
                },
            ));
        }
    }
    for c in n.children() {
        collect_init_calls(c, dbc_paths, script, resolve_bus, out);
    }
}

/// A bus argument, classified and (where possible) resolved to a number.
#[derive(Debug, Clone, Default)]
struct BusArg {
    /// `literal`, `constant`, `parameter`, `channel`, `symbol` or `expression`.
    kind: String,
    /// The bus number, when it is knowable: written literally, or carried by the
    /// symbol the argument names.
    value: Option<i64>,
    /// True when `value` came from a calibratable symbol rather than a literal
    /// or a project constant.
    calibrated: bool,
}

/// Read a `.m1prj`/`.m1cfg` value as a bus number. Cells are exported in the
/// declared cell type, so an `f32` bus lands as `2.00000000000000000e+00`;
/// accept it only when it is exactly integral.
fn bus_number(text: &str) -> Option<i64> {
    if let Ok(n) = text.parse::<i64>() {
        return Some(n);
    }
    let f = text.parse::<f64>().ok()?;
    (f.fract() == 0.0 && f.abs() < i64::MAX as f64).then_some(f as i64)
}

/// Classify a bus argument: a literal number, else the symbol it names — whose
/// kind and statically-known value (a constant's `.m1prj` `Value`, a parameter's
/// `parameters.m1cfg` cell) both come from the loaded project model.
fn classify_bus(arg: &str, project: &Project) -> BusArg {
    if arg.is_empty() {
        return BusArg {
            kind: "expression".to_string(),
            ..BusArg::default()
        };
    }
    if let Ok(value) = arg.parse::<i64>() {
        return BusArg {
            kind: "literal".to_string(),
            value: Some(value),
            calibrated: false,
        };
    }
    // A script writes the bus symbol by its tail (`Active Bus`), while the
    // project stores the full path (`Root.CAN.Active Bus`); match either.
    let suffix = format!(".{arg}");
    let mut kinds: BTreeSet<&'static str> = BTreeSet::new();
    let mut values: BTreeSet<i64> = BTreeSet::new();
    let mut is_constant = true;
    for s in project.symbols().iter() {
        if s.path == arg || s.path.ends_with(&suffix) {
            kinds.insert(bus_kind_str(s.kind));
            if let Some(v) = s.static_value.as_deref().and_then(bus_number) {
                values.insert(v);
                is_constant &= s.kind == SymbolKind::Constant;
            }
        }
    }
    // More than one symbol answers to this name, or they disagree on a value:
    // resolve nothing rather than pick one.
    let kind = match kinds.len() {
        1 => kinds.into_iter().next().unwrap().to_string(),
        _ => "expression".to_string(),
    };
    let value = (values.len() == 1).then(|| *values.iter().next().unwrap());
    BusArg {
        kind,
        value,
        // A constant is fixed by the project; anything else with a value got it
        // from the current calibration.
        calibrated: value.is_some() && !is_constant,
    }
}

/// Exact DBC symbol paths registered in the loaded project. A normal loaded
/// project contains both the `.m1prj` spelling (`DBC.BMU`) and the source DBC
/// spelling (`BMU`); keeping both is necessary for script alias resolution.
pub(crate) fn registered_dbc_paths(project: &Project) -> Vec<String> {
    let mut paths: Vec<String> = project
        .symbols()
        .iter()
        .filter(|symbol| symbol.classname.as_deref() == Some("BuiltIn.CAN.DBC"))
        .map(|symbol| symbol.path.clone())
        .collect();
    paths.sort_by(|a, b| b.len().cmp(&a.len()).then_with(|| a.cmp(b)));
    paths.dedup();
    paths
}

/// Resolve every usable script's DBC Init calls against one caller-owned
/// project/script snapshot. The path paired with each call is the exact alias
/// that appeared before `.Init`, not a leaf-name approximation.
pub(crate) fn loaded_init_calls(
    project: &Project,
    scripts: &[ParsedScript],
) -> (Vec<(String, CanInitDto)>, Vec<CanSkippedScriptDto>) {
    let dbc_paths = registered_dbc_paths(project);
    loaded_init_calls_for_paths(project, scripts, &dbc_paths)
}

/// Resolve Init calls against an explicit set of exact DBC aliases. The runtime
/// model adds source-root paths to the project registrations so caller-owned
/// DBC bytes remain usable without first augmenting the [`Project`].
pub(crate) fn loaded_init_calls_for_paths(
    project: &Project,
    scripts: &[ParsedScript],
    dbc_paths: &[String],
) -> (Vec<(String, CanInitDto)>, Vec<CanSkippedScriptDto>) {
    // Longest first ensures `DBC.Dash.Init` is matched before `Dash.Init`.
    let mut dbc_paths = dbc_paths.to_vec();
    dbc_paths.sort_by(|a, b| b.len().cmp(&a.len()).then_with(|| a.cmp(b)));
    dbc_paths.dedup();
    let resolve_bus = |arg: &str| classify_bus(arg, project);
    let mut found = Vec::new();
    let mut skipped_scripts = Vec::new();
    for script in scripts {
        // An unparseable script's reference shapes are unreliable. Keep the
        // omission visible rather than presenting its missing Init calls as a
        // clean uninitialised-module result.
        let syntax = script.cst.syntax_diagnostics();
        if let Some(first) = syntax.first() {
            let noun = if syntax.len() == 1 {
                "syntax diagnostic"
            } else {
                "syntax diagnostics"
            };
            skipped_scripts.push(CanSkippedScriptDto {
                script: script.name.clone(),
                reason: format!(
                    "{} {noun}; first at line {}, column {}: {}; Init calls were not inspected",
                    syntax.len(),
                    first.range.start.line + 1,
                    first.range.start.column + 1,
                    first.message,
                ),
            });
            continue;
        }
        let root = script.cst.root();
        let depth = root.max_depth();
        if depth > m1_core::MAX_RECURSION_DEPTH {
            skipped_scripts.push(CanSkippedScriptDto {
                script: script.name.clone(),
                reason: format!(
                    "nesting depth {depth} exceeds the safe limit {}; Init calls were not inspected",
                    m1_core::MAX_RECURSION_DEPTH,
                ),
            });
            continue;
        }
        collect_init_calls(root, &dbc_paths, &script.name, &resolve_bus, &mut found);
    }
    skipped_scripts.sort_by(|a, b| a.script.cmp(&b.script));
    (found, skipped_scripts)
}

/// module -> (bus argument, kind, resolved number, calibration-sourced)
pub(crate) type BusBinding = (Option<String>, String, Option<i64>, bool);

/// Collapse one module's Init calls into its single usable bus binding. Calls
/// that disagree keep the module initialised but deliberately unbound.
pub(crate) fn binding_from_calls(calls: &[CanInitDto]) -> BusBinding {
    let distinct: BTreeSet<&str> = calls.iter().map(|call| call.bus.as_str()).collect();
    match distinct.len() {
        0 => (None, "none".to_string(), None, false),
        1 => {
            let call = &calls[0];
            (
                Some(call.bus.clone()),
                call.bus_kind.clone(),
                call.bus_value,
                call.bus_calibrated,
            )
        }
        // Disagreeing `Init` calls: no single bus can be assumed.
        _ => (None, "conflicting-init".to_string(), None, false),
    }
}

/// Turn a recorded bus argument back into a comparable [`Bus`].
fn bus_of(bus: &str, kind: &str, value: Option<i64>, calibrated: bool) -> Option<Bus> {
    if bus.is_empty() {
        return None;
    }
    match value {
        Some(value) => Some(Bus::Number { value, calibrated }),
        // No value, but still identity-comparable: the same symbol is
        // necessarily the same bus. An `expression` is not even that.
        None if kind != "expression" => Some(Bus::Symbolic(bus.to_string())),
        None => None,
    }
}

/// Build the CAN picture of the project at `project_path` (a `Project.m1prj`).
/// `filter` narrows the returned `messages` by case-insensitive substring of
/// their path (module bindings and overlap verdicts are always computed over
/// *every* message); `limit` caps that list (0 = no cap).
pub fn inspect(
    project_path: &Path,
    filter: Option<&str>,
    limit: usize,
) -> Result<CanOutcome, String> {
    crate::loader::check_project_script_budget(project_path)?;
    let project = crate::loader::load_project_full(project_path)?;
    let gathered = crate::loader::gather_project_scripts(project_path);
    let mut outcome = inspect_loaded(&project, &gathered.scripts, filter, limit);
    outcome
        .skipped_scripts
        .extend(
            gathered
                .skipped
                .into_iter()
                .map(|failure| CanSkippedScriptDto {
                    script: failure.path,
                    reason: format!("{}; Init calls were not inspected", failure.error),
                }),
        );
    outcome
        .skipped_scripts
        .sort_by(|a, b| a.script.cmp(&b.script));
    Ok(outcome)
}

/// Build the CAN picture from one already-loaded project and parsed script
/// snapshot. This is the entry point for consumers that attach their own load
/// report, since the returned verdicts and that report can then describe the
/// same loaded snapshot instead of two independent loads.
pub fn inspect_loaded(
    project: &Project,
    scripts: &[ParsedScript],
    filter: Option<&str>,
    limit: usize,
) -> CanOutcome {
    // Registered DBC objects. A DBC appears twice — `DBC.<Name>` from the
    // `.m1prj` and bare `<Name>` from the `.m1dbc` — so unify by leaf name while
    // keeping every spelling for the `Init`-call match.
    let mut files: BTreeMap<String, String> = BTreeMap::new();
    for s in project.symbols().iter() {
        if s.classname.as_deref() == Some("BuiltIn.CAN.DBC")
            && let Some(f) = &s.filename
        {
            files.entry(leaf(&s.path).to_string()).or_insert(f.clone());
        }
    }
    let dbc_paths = registered_dbc_paths(project);
    let leaves: BTreeSet<String> = dbc_paths.iter().map(|p| leaf(p).to_string()).collect();

    // `Init` call sites, keyed by module leaf.
    let mut init_by_module: BTreeMap<String, Vec<CanInitDto>> = BTreeMap::new();
    let (init_calls, skipped_scripts) = loaded_init_calls(project, scripts);
    for (module_path, call) in init_calls {
        init_by_module
            .entry(leaf(&module_path).to_string())
            .or_default()
            .push(call);
    }

    // Resolve each module's bus: one agreed argument, or none.
    let mut modules: Vec<CanModuleDto> = Vec::new();
    let mut bus_of: BTreeMap<String, BusBinding> = BTreeMap::new();
    for name in &leaves {
        let calls = init_by_module.get(name).cloned().unwrap_or_default();
        let binding = binding_from_calls(&calls);
        bus_of.insert(name.clone(), binding.clone());
        let (bus, bus_kind, bus_value, bus_calibrated) = binding;
        modules.push(CanModuleDto {
            name: name.clone(),
            file: files.get(name).cloned(),
            message_count: 0,
            initialised: !calls.is_empty(),
            bus,
            bus_kind,
            bus_value,
            bus_calibrated,
            init_calls: calls,
        });
    }

    // Messages, attributed to the module whose leaf prefixes their path.
    let mut messages: Vec<CanMessageDto> = Vec::new();
    for s in project.symbols().iter() {
        if s.classname.as_deref() != Some("BuiltIn.CAN.Message") {
            continue;
        }
        let module = leaves
            .iter()
            .filter(|l| s.path.len() > l.len() && s.path.starts_with(&format!("{l}.")))
            // Longest wins, so `PDM15 DBC` beats a hypothetical `PDM15`.
            .max_by_key(|l| l.len())
            .cloned()
            .unwrap_or_else(|| leaf(&s.path).to_string());
        let (bus, bus_kind, bus_value, bus_calibrated) =
            bus_of
                .get(&module)
                .cloned()
                .unwrap_or((None, "none".to_string(), None, false));
        let can = s.can.as_ref();
        messages.push(CanMessageDto {
            path: s.path.clone(),
            module,
            can_id: can.and_then(|c| c.can_id),
            can_id_hex: can.and_then(|c| c.can_id).map(|id| format!("0x{id:X}")),
            extended: can.is_some_and(|c| c.extended),
            dlc: can.and_then(|c| c.dlc),
            direction: can.and_then(|c| c.transmit).map(|d| {
                match d {
                    CanDirection::Rx => "RX",
                    CanDirection::Tx => "TX",
                }
                .to_string()
            }),
            bus,
            bus_kind,
            bus_value,
            bus_calibrated,
        });
    }
    messages.sort_by(|a, b| a.path.cmp(&b.path));
    for m in &mut modules {
        m.message_count = messages.iter().filter(|msg| msg.module == m.name).count();
    }

    let id_overlaps = overlaps(&messages);
    let total_messages = messages.len();
    if let Some(f) = filter {
        let needle = f.to_ascii_lowercase();
        messages.retain(|m| m.path.to_ascii_lowercase().contains(&needle));
    }
    if limit > 0 {
        messages.truncate(limit);
    }

    CanOutcome {
        modules,
        uninitialised_modules: leaves
            .iter()
            .filter(|l| !init_by_module.contains_key(*l))
            .cloned()
            .collect(),
        id_overlaps,
        messages,
        total_messages,
        skipped_scripts,
        guidance: GUIDANCE.iter().map(|g| g.to_string()).collect(),
    }
}

/// Group messages by CAN id and judge each repeated id against the buses its
/// messages sit on.
fn overlaps(messages: &[CanMessageDto]) -> Vec<CanIdOverlapDto> {
    let mut by_id: BTreeMap<u32, Vec<&CanMessageDto>> = BTreeMap::new();
    for m in messages {
        if let Some(id) = m.can_id {
            by_id.entry(id).or_default().push(m);
        }
    }
    let mut out = Vec::new();
    for (id, group) in by_id {
        if group.len() < 2 {
            continue;
        }
        // Pairwise: any provably-shared bus is a clash; otherwise any
        // undecidable pair keeps the whole group unknown. Track whether the
        // deciding comparisons leaned on a calibration value, so a verdict that
        // a retune could invalidate says so.
        let mut shared_bus: Option<String> = None;
        let mut undecidable = false;
        let mut calibrated = false;
        for (i, a) in group.iter().enumerate() {
            for b in &group[i + 1..] {
                let (av, bv) = (
                    a.bus
                        .as_deref()
                        .and_then(|s| bus_of(s, &a.bus_kind, a.bus_value, a.bus_calibrated)),
                    b.bus
                        .as_deref()
                        .and_then(|s| bus_of(s, &b.bus_kind, b.bus_value, b.bus_calibrated)),
                );
                match (av, bv) {
                    (Some(x), Some(y)) => match x.same_as(&y) {
                        Some(v) => {
                            calibrated |= v.calibrated;
                            if v.same {
                                shared_bus.get_or_insert_with(|| a.bus.clone().unwrap_or_default());
                            }
                        }
                        None => undecidable = true,
                    },
                    // An uninitialised module has no bus at all.
                    _ => undecidable = true,
                }
            }
        }
        // A caveat only matters for a verdict that was actually decided.
        let calibrated = calibrated && !(undecidable && shared_bus.is_none());
        let caveat = if calibrated {
            " — but only for the calibration in parameters.m1cfg: the bus comes from a parameter, \
             so a retune can change this"
        } else {
            ""
        };
        let (verdict, note) = if shared_bus.is_some() {
            (
                "same-bus",
                format!(
                    "two or more messages with this id are on the same bus — a real CAN id clash{caveat}"
                ),
            )
        } else if undecidable {
            (
                "unknown",
                "at least one module's bus has no known value (uninitialised, or Init'd with a \
                 symbol the project carries no value for), so this id cannot be proven safe or \
                 clashing — check the bus binding before reporting it"
                    .to_string(),
            )
        } else {
            (
                "different-bus",
                format!(
                    "every message with this id is on a different CAN bus — not a clash{caveat}"
                ),
            )
        };
        out.push(CanIdOverlapDto {
            can_id: id,
            can_id_hex: format!("0x{id:X}"),
            verdict: verdict.to_string(),
            bus: shared_bus,
            depends_on_calibration: calibrated,
            note,
            messages: group
                .iter()
                .map(|m| CanOverlapMemberDto {
                    path: m.path.clone(),
                    module: m.module.clone(),
                    bus: m.bus.clone(),
                    bus_kind: m.bus_kind.clone(),
                    bus_value: m.bus_value,
                    bus_calibrated: m.bus_calibrated,
                    direction: m.direction.clone(),
                })
                .collect(),
        });
    }
    out
}
