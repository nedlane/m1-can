//! Parse a MoTeC `.m1dbc` CAN database into a neutral in-memory model.
//!
//! A `.m1dbc` is MoTeC XML: a flat `<ComponentStream><List>` of `<Component>`
//! elements, each tagged with a `Classname`. Messages (`BuiltIn.CAN.Message`)
//! and their signals (`BuiltIn.CAN.Signal`) may appear in **any order**, and a
//! signal is tied to its message only by name (`"<message name>.<signal name>"`),
//! so parsing is two passes: collect, then attach.
//!
//! Three details of the format routinely surprise people, and each has a test:
//!
//! 1. **`CANId`, `StartBit` and `Length` are hexadecimal**; `DLC` alone is
//!    decimal. A `Length="10"` is sixteen bits, a `DLC="10"` is ten bytes.
//! 2. **`Sender`, `Receivers` and (sometimes) `Endian` sit on the `<Component>`**,
//!    not on its `<Props>`.
//! 3. **The files carry Windows-1252 bytes but declare no XML encoding** — a
//!    bare `0xB0` for the degree sign in units like `°/s`. Decoding them as
//!    UTF-8 is lossy at best, so reads go through `m1-workspace`.
//!
//! Components MoTeC stores but a `.dbc` cannot represent — the
//! `VECTOR INDEPENDENT SIG MSG` container and its orphan signals, messages with
//! no `CANId`, and the `BuiltIn.CAN.DBC` file-metadata component — are dropped
//! and each recorded in [`M1DbcFile::skipped`], so the export can report exactly
//! what it discarded rather than silently losing it.
//!
//! Anything genuinely malformed (unparsable numbers, a component with no
//! `<Props>`) is an [`ExportError`] — the crate refuses rather than guesses.

use crate::ExportError;
use std::collections::{HashMap, HashSet};

/// `Classname` of a CAN frame component.
const MSG_CLASS: &str = "BuiltIn.CAN.Message";
/// `Classname` of a CAN signal component.
const SIG_CLASS: &str = "BuiltIn.CAN.Signal";
/// MoTeC's container for signals not assigned to any real CAN message. It is
/// encoded with `CANId 0x40000000` (the DBC container id is `0xC0000000`);
/// neither fits the 29-bit frame-id limit, and a DBC reader discards
/// `VECTOR__INDEPENDENT_SIG_MSG` on load. So it is skipped, not emitted.
const INDEPENDENT_SIG_MSG_SUFFIX: &str = "VECTOR INDEPENDENT SIG MSG";
/// DBC's placeholder node name, used when a component names no sender/receiver.
const DEFAULT_NODE: &str = "Vector__XXX";

/// One parsed `.m1dbc`: what can be exported, what was dropped, and the counts
/// that let a caller verify the two add up.
#[derive(Debug, Clone, PartialEq)]
pub struct M1DbcFile {
    /// Convertible messages in document order, each with its signals attached.
    pub messages: Vec<M1Message>,
    /// One human-readable note per dropped component, in document order.
    pub skipped: Vec<String>,
    /// Counts taken by a second, independent walk of the source (see
    /// [`SourceCounts`]).
    pub totals: SourceCounts,
}

/// Message/signal counts for a source `.m1dbc`.
///
/// The `convertible_*` fields are produced by a **second walk** of the component
/// list that re-derives the skip rules from scratch, rather than by measuring
/// [`M1DbcFile::messages`]. That redundancy is the point: it cross-checks the
/// parser, so a caller can assert `written == convertible` and have the
/// assertion mean something.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SourceCounts {
    /// Every `BuiltIn.CAN.Message` component in the file.
    pub total_messages: usize,
    /// Every `BuiltIn.CAN.Signal` component in the file.
    pub total_signals: usize,
    /// Messages that survive the skip rules — i.e. that will be written.
    pub convertible_messages: usize,
    /// Signals attached to a surviving message — i.e. that will be written.
    pub convertible_signals: usize,
}

/// A CAN frame.
#[derive(Debug, Clone, PartialEq)]
pub struct M1Message {
    /// DBC identifier: the MoTeC name with the file prefix stripped, sanitized.
    pub name: String,
    /// `CANId`, parsed from hexadecimal.
    pub frame_id: u32,
    /// `IdType="Extended"` — a 29-bit rather than 11-bit identifier.
    pub is_extended: bool,
    /// `DLC`, parsed from **decimal**: the frame's length in bytes.
    pub dlc: u8,
    /// The transmitting node, or `Vector__XXX` when the file names none.
    pub sender: String,
    /// The frame's signals, in document order.
    pub signals: Vec<M1Signal>,
}

/// A signal within a frame.
#[derive(Debug, Clone, PartialEq)]
pub struct M1Signal {
    /// DBC identifier: the segment after the last `.`, sanitized.
    pub name: String,
    /// `StartBit`, parsed from hexadecimal.
    pub start_bit: u16,
    /// Width in bits, parsed from hexadecimal — always 1 for a `bool`.
    pub length: u16,
    /// `Endian="Little"` (the default, and universal in practice).
    pub little_endian: bool,
    /// The `Type` begins with `s`.
    pub is_signed: bool,
    /// The `Type` begins with `f`.
    pub is_float: bool,
    /// `Multiplier`, default 1.
    pub scale: f64,
    /// `Offset`, default 0.
    pub offset: f64,
    /// The display unit from `<Props><Locale><Default Unit="…">`, if any.
    pub unit: Option<String>,
    /// The receiving node, or `Vector__XXX` when the file names none.
    pub receiver: String,
}

/// Parse the raw bytes of a `.m1dbc`.
///
/// `file_stem` is the source file's name without its extension (e.g.
/// `"Steering DBC"`). MoTeC fully qualifies every component name with it, so it
/// is stripped from message and signal names to give the bare DBC identifiers.
///
/// # Errors
///
/// [`ExportError::Xml`] if the bytes are not well-formed XML, or
/// [`ExportError::Invalid`] if a CAN component is unusable — a numeric attribute
/// that will not parse, or a message/signal with no `<Props>`.
pub fn parse_m1dbc(bytes: &[u8], file_stem: &str) -> Result<M1DbcFile, ExportError> {
    // DIVERGENCE from the Python reference (`raw.decode("cp1252")`, an
    // *unconditional* Windows-1252 decode): `m1_workspace::decode` is UTF-8
    // first with a Windows-1252 fallback. AGENTS.md mandates that every MoTeC
    // read go through m1-workspace's tolerant helpers, and on real `.m1dbc`
    // files the two agree — they contain a bare 0xB0 degree byte, which is
    // invalid UTF-8, so the fallback always fires. The two would part company
    // only on a `.m1dbc` that is wholly valid UTF-8 *and* uses a multi-byte
    // sequence, in which case decoding it as UTF-8 is the better answer anyway.
    let text = m1_workspace::decode(bytes.to_vec());
    let doc = roxmltree::Document::parse(&text).map_err(|e| ExportError::Xml(e.to_string()))?;

    // `ComponentStream` at any depth (real files nest it under a `<DBC>` root;
    // a bare `<ComponentStream>` root is also accepted), then exactly
    // `List/Component` beneath it.
    //
    // DIVERGENCE from the Python reference (`root.findall(".//ComponentStream/
    // List/Component")`): ElementTree's `.//` searches the root's *descendants*
    // and never the root element itself, so a file whose root element IS
    // `<ComponentStream>` yields nothing there. `descendants()` includes the
    // root, so such a file parses here. Real `.m1dbc` files always nest the
    // stream under `<DBC>`, where the two agree; the difference shows only on
    // hand-written input — including this module's own bare-root test fixtures,
    // which is what the wider rule buys.
    let components: Vec<roxmltree::Node<'_, '_>> = doc
        .descendants()
        .filter(|n| n.has_tag_name("ComponentStream"))
        .flat_map(|cs| cs.children().filter(|n| n.has_tag_name("List")))
        .flat_map(|list| list.children().filter(|n| n.has_tag_name("Component")))
        .collect();

    // Pass 1: collect. The `<List>` is unordered, so signals are bucketed by the
    // name of the message they claim and attached once every message is known.
    let mut messages: Vec<(String, M1Message)> = Vec::new();
    let mut index: HashMap<String, usize> = HashMap::new();
    let mut signals: HashMap<String, Vec<M1Signal>> = HashMap::new();
    let mut skipped: Vec<String> = Vec::new();

    for comp in &components {
        let classname = comp.attribute("Classname").unwrap_or_default();
        let name = comp.attribute("Name").unwrap_or_default();

        if classname == MSG_CLASS {
            if name.ends_with(INDEPENDENT_SIG_MSG_SUFFIX) {
                skipped.push(format!(
                    "message '{name}' (VECTOR independent-signal container; \
                     not representable by cantools, discarded on DBC load)"
                ));
                continue;
            }
            let props = props_of(*comp, "message", name)?;
            let Some(can_id) = props.attribute("CANId") else {
                skipped.push(format!("message '{name}' (no CANId)"));
                continue;
            };
            let message = M1Message {
                name: sanitize(message_local_name(name, file_stem), file_stem),
                frame_id: parse_hex(can_id, "message", name, "CANId")?,
                is_extended: props.attribute("IdType").unwrap_or("Standard") == "Extended",
                dlc: parse_dec(
                    props.attribute("DLC").unwrap_or("8"),
                    "message",
                    name,
                    "DLC",
                )?,
                sender: or_default_node(comp.attribute("Sender")),
                signals: Vec::new(),
            };
            // A repeated `Name` overwrites in place, keeping document position —
            // matching the Python's dict, where `messages[name] = …` replaces
            // the value but not the insertion order.
            match index.get(name) {
                Some(&i) => messages[i].1 = message,
                None => {
                    index.insert(name.to_string(), messages.len());
                    messages.push((name.to_string(), message));
                }
            }
        } else if classname == SIG_CLASS {
            let parent = parent_name(name);
            if parent.ends_with(INDEPENDENT_SIG_MSG_SUFFIX) {
                skipped.push(format!(
                    "signal '{name}' (in VECTOR independent-signal container)"
                ));
                continue;
            }
            signals
                .entry(parent.to_string())
                .or_default()
                .push(build_signal(*comp, file_stem)?);
        } else {
            // `BuiltIn.CAN.DBC` file metadata, and anything else.
            let what = if classname.is_empty() {
                "unknown"
            } else {
                classname
            };
            skipped.push(format!("{what} '{name}' (not a CAN frame)"));
        }
    }

    // Pass 2: attach. Signals naming a message that does not exist (or that was
    // skipped) are simply dropped — they have nowhere to go in a `.dbc`.
    let messages: Vec<M1Message> = messages
        .into_iter()
        .map(|(full_name, mut m)| {
            m.signals = signals.remove(&full_name).unwrap_or_default();
            m
        })
        .collect();

    let totals = count_source(&components);
    Ok(M1DbcFile {
        messages,
        skipped,
        totals,
    })
}

/// Count the source components, re-deriving the skip rules independently of
/// [`parse_m1dbc`]'s build path.
///
/// DIVERGENCE from the Python reference: its `count_source` applies only the
/// container rule, so a message with no `CANId` counts as convertible even
/// though nothing is written for it — and its `--check` then reports a spurious
/// "written .dbc count != convertible source count". Here both skip rules are
/// applied (and duplicate message names are counted once) so the invariant
/// `written == convertible` holds for every input the parser accepts, which is
/// what makes the check worth running.
fn count_source(components: &[roxmltree::Node<'_, '_>]) -> SourceCounts {
    let mut counts = SourceCounts {
        total_messages: 0,
        total_signals: 0,
        convertible_messages: 0,
        convertible_signals: 0,
    };
    let mut convertible: HashSet<&str> = HashSet::new();

    for comp in components {
        let classname = comp.attribute("Classname").unwrap_or_default();
        let name = comp.attribute("Name").unwrap_or_default();
        if classname == MSG_CLASS {
            counts.total_messages += 1;
            let has_can_id = comp
                .children()
                .find(|n| n.has_tag_name("Props"))
                .is_some_and(|p| p.attribute("CANId").is_some());
            if !name.ends_with(INDEPENDENT_SIG_MSG_SUFFIX) && has_can_id && convertible.insert(name)
            {
                counts.convertible_messages += 1;
            }
        } else if classname == SIG_CLASS {
            counts.total_signals += 1;
        }
    }

    for comp in components {
        if comp.attribute("Classname").unwrap_or_default() == SIG_CLASS
            && convertible.contains(parent_name(comp.attribute("Name").unwrap_or_default()))
        {
            counts.convertible_signals += 1;
        }
    }
    counts
}

/// Build one [`M1Signal`] from a `BuiltIn.CAN.Signal` component.
fn build_signal(comp: roxmltree::Node<'_, '_>, file_stem: &str) -> Result<M1Signal, ExportError> {
    let full_name = comp.attribute("Name").unwrap_or_default();
    let props = props_of(comp, "signal", full_name)?;

    // An absent *or empty* `Type` means u32; a `bool` is one unsigned bit and
    // ignores `Length` entirely.
    let sig_type = props.attribute("Type").unwrap_or_default().trim();
    let sig_type = if sig_type.is_empty() { "u32" } else { sig_type };
    let (length, is_signed, is_float) = if sig_type == "bool" {
        (1, false, false)
    } else {
        (
            parse_hex(
                props.attribute("Length").unwrap_or("1"),
                "signal",
                full_name,
                "Length",
            )?,
            sig_type.starts_with('s'),
            sig_type.starts_with('f'),
        )
    };

    // `Endian` may sit on the `<Props>` or on the `<Component>`; in practice
    // every real signal takes the default.
    let endian = non_empty(props.attribute("Endian"))
        .or_else(|| non_empty(comp.attribute("Endian")))
        .unwrap_or("Little");

    Ok(M1Signal {
        name: sanitize(signal_local_name(full_name), file_stem),
        start_bit: parse_hex(
            props.attribute("StartBit").unwrap_or("0"),
            "signal",
            full_name,
            "StartBit",
        )?,
        length,
        little_endian: endian == "Little",
        is_signed,
        is_float,
        scale: parse_float(props.attribute("Multiplier"), 1.0, full_name, "Multiplier")?,
        offset: parse_float(props.attribute("Offset"), 0.0, full_name, "Offset")?,
        unit: props
            .children()
            .filter(|n| n.has_tag_name("Locale"))
            .flat_map(|l| l.children().filter(|n| n.has_tag_name("Default")))
            .find_map(|d| d.attribute("Unit"))
            .filter(|u| !u.is_empty())
            .map(str::to_string),
        receiver: or_default_node(comp.attribute("Receivers")),
    })
}

/// The `<Props>` child every CAN component must have.
fn props_of<'a>(
    comp: roxmltree::Node<'a, '_>,
    kind: &str,
    name: &str,
) -> Result<roxmltree::Node<'a, 'a>, ExportError> {
    comp.children()
        .find(|n| n.has_tag_name("Props"))
        .ok_or_else(|| ExportError::Invalid(format!("{kind} '{name}': missing <Props>")))
}

/// `CANId` / `StartBit` / `Length` are base 16.
fn parse_hex<T: TryFrom<u32>>(
    raw: &str,
    kind: &str,
    name: &str,
    attr: &str,
) -> Result<T, ExportError> {
    u32::from_str_radix(raw.trim(), 16)
        .ok()
        .and_then(|v| T::try_from(v).ok())
        .ok_or_else(|| {
            ExportError::Invalid(format!(
                "{kind} '{name}': {attr} {raw:?} is not a hexadecimal integer"
            ))
        })
}

/// `DLC` is the one numeric attribute stored in base 10.
fn parse_dec(raw: &str, kind: &str, name: &str, attr: &str) -> Result<u8, ExportError> {
    raw.trim().parse().map_err(|_| {
        ExportError::Invalid(format!(
            "{kind} '{name}': {attr} {raw:?} is not a decimal integer"
        ))
    })
}

/// `Multiplier` / `Offset`, stored in `%.17e` form. An absent attribute takes
/// `default`; a present one must parse (an empty string is an error, matching
/// the reference implementation).
fn parse_float(
    raw: Option<&str>,
    default: f64,
    name: &str,
    attr: &str,
) -> Result<f64, ExportError> {
    match raw {
        None => Ok(default),
        Some(v) => v.trim().parse().map_err(|_| {
            ExportError::Invalid(format!("signal '{name}': {attr} {v:?} is not a number"))
        }),
    }
}

/// Treat an empty attribute as absent.
fn non_empty(value: Option<&str>) -> Option<&str> {
    value.filter(|v| !v.is_empty())
}

/// A node name, falling back to DBC's placeholder.
fn or_default_node(value: Option<&str>) -> String {
    non_empty(value).unwrap_or(DEFAULT_NODE).to_string()
}

/// The message a signal belongs to: everything before the last `.`.
fn parent_name(full_name: &str) -> &str {
    full_name.rsplit_once('.').map_or(full_name, |(p, _)| p)
}

/// A signal's own name: everything after the last `.`.
fn signal_local_name(full_name: &str) -> &str {
    full_name.rsplit_once('.').map_or(full_name, |(_, s)| s)
}

/// A message's own name: the file prefix stripped, interior dots kept.
fn message_local_name<'a>(full_name: &'a str, file_stem: &str) -> &'a str {
    full_name
        .strip_prefix(&format!("{file_stem}."))
        .unwrap_or(full_name)
}

/// Turn a MoTeC component name into a valid DBC identifier — i.e. one matching
/// `[A-Za-z_][A-Za-z0-9_]*`.
///
/// In order: strip a leading `"{file_stem}."` **or** `"DBC."` (whichever matches
/// first — never both); collapse every run of non-identifier characters, and of
/// underscores, to a single `_`; trim leading and trailing `_`; substitute
/// `Unnamed` if nothing is left; and prefix `_` if the result starts with a
/// digit.
fn sanitize(name: &str, file_stem: &str) -> String {
    let name = match name.strip_prefix(&format!("{file_stem}.")) {
        Some(rest) => rest,
        None => name.strip_prefix("DBC.").unwrap_or(name),
    };

    // `[^A-Za-z0-9_]+ -> _` followed by `_+ -> _` is one pass: map every
    // non-alphanumeric character (underscores included) to `_` and drop repeats.
    let mut out = String::with_capacity(name.len());
    for ch in name.chars() {
        if ch.is_ascii_alphanumeric() {
            out.push(ch);
        } else if !out.ends_with('_') {
            out.push('_');
        }
    }

    let out = out.trim_matches('_');
    if out.is_empty() {
        return "Unnamed".to_string();
    }
    if out.starts_with(|c: char| c.is_ascii_digit()) {
        return format!("_{out}");
    }
    out.to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The committed fixture, loaded as **raw bytes** on purpose: it is stored
    /// Windows-1252, so reading it as UTF-8 would be lossy. `.gitattributes`
    /// pins it `-text` so git never rewrites its newlines either.
    const SAMPLE: &[u8] = include_bytes!("../tests/fixtures/Sample DBC.m1dbc");

    fn sample() -> M1DbcFile {
        parse_m1dbc(SAMPLE, "Sample DBC").expect("the sample fixture must parse")
    }

    #[test]
    fn the_fixture_is_stored_as_windows_1252_with_no_encoding_declaration() {
        // The parser's whole reason for going through m1-workspace's tolerant
        // decode is this byte: a bare 0xB0 (degree sign) that is *invalid* UTF-8.
        assert!(
            SAMPLE.windows(2).any(|w| w == [0xB0, b'/']),
            "fixture must carry the degree sign as the single byte 0xB0"
        );
        assert!(
            !SAMPLE.windows(9).any(|w| w == b"encoding="),
            "no declared encoding: real .m1dbc files open with <?xml version=\"1.0\"?> \
             and name no encoding, so the bytes alone decide (this fixture goes \
             further and omits the declaration entirely — see \
             parses_a_document_carrying_the_real_files_xml_declaration)"
        );
    }

    /// Real `.m1dbc` files open with `<?xml version="1.0"?>`; every fixture in
    /// this module starts at an element, so nothing else feeds the parser a
    /// prolog-bearing document.
    #[test]
    fn parses_a_document_carrying_the_real_files_xml_declaration() {
        let xml = format!("<?xml version=\"1.0\"?>\n{NESTED}");
        let f = parse_m1dbc(xml.as_bytes(), "Mod").expect("a declaration must not stop the parse");
        assert_eq!(f.messages.len(), 1, "messages: {:?}", f.messages);
        assert_eq!(f.messages[0].name, "Frame");
        assert_eq!(f.messages[0].signals.len(), 3);
    }

    #[test]
    fn decodes_the_windows_1252_degree_byte_into_a_unit_string() {
        let f = sample();
        assert_eq!(
            f.messages[0].signals[0].unit.as_deref(),
            Some("°/s"),
            "0xB0 must decode to U+00B0, not U+FFFD"
        );
    }

    #[test]
    fn keeps_only_the_one_convertible_message_from_the_fixture() {
        let f = sample();
        assert_eq!(f.messages.len(), 1, "messages: {:?}", f.messages);
        let m = &f.messages[0];
        assert_eq!(
            m.name, "Status",
            "file prefix must be stripped and sanitized"
        );
        assert_eq!(m.frame_id, 0x3FD, "CANId is hexadecimal");
        assert!(
            !m.is_extended,
            "IdType=\"Standard\" is not an extended frame"
        );
        assert_eq!(m.dlc, 8, "DLC is decimal");
        assert_eq!(
            m.sender, "ECU",
            "Sender lives on the Component, not the Props"
        );
    }

    #[test]
    fn parses_both_signals_in_document_order() {
        let f = sample();
        let sigs = &f.messages[0].signals;
        assert_eq!(sigs.len(), 2, "signals: {sigs:?}");

        let yaw = &sigs[0];
        assert_eq!(
            yaw.name, "Yaw_Rate",
            "signal name is the segment after the last '.'"
        );
        assert_eq!(yaw.start_bit, 0);
        assert_eq!(yaw.length, 16, "Length=\"10\" is hexadecimal");
        assert!(yaw.little_endian, "Endian defaults to Little");
        assert!(yaw.is_signed, "Type=\"s16\" starts with 's'");
        assert!(!yaw.is_float);
        assert_eq!(yaw.scale, 0.5);
        assert_eq!(yaw.offset, -100.0);
        assert_eq!(yaw.unit.as_deref(), Some("°/s"));
        assert_eq!(yaw.receiver, "Vector__XXX", "absent Receivers defaults");

        let ready = &sigs[1];
        assert_eq!(ready.name, "Ready");
        assert_eq!(ready.start_bit, 10, "StartBit=\"A\" is hexadecimal");
        assert_eq!(ready.length, 1, "Type=\"bool\" forces length 1");
        assert!(!ready.is_signed);
        assert!(!ready.is_float);
        assert_eq!(ready.scale, 1.0, "absent Multiplier defaults to 1");
        assert_eq!(ready.offset, 0.0, "absent Offset defaults to 0");
        assert_eq!(ready.unit, None, "no <Locale><Default Unit>");
    }

    #[test]
    fn counts_the_source_and_the_convertible_subset() {
        let f = sample();
        assert_eq!(
            f.totals,
            SourceCounts {
                total_messages: 3,
                total_signals: 3,
                convertible_messages: 1,
                convertible_signals: 2,
            },
            "the container and the no-CANId message are both inconvertible"
        );
    }

    #[test]
    fn convertible_counts_equal_what_was_actually_kept() {
        // The invariant the `--check` report leans on: everything counted as
        // convertible is a message/signal the writer will emit.
        let f = sample();
        assert_eq!(f.totals.convertible_messages, f.messages.len());
        assert_eq!(
            f.totals.convertible_signals,
            f.messages.iter().map(|m| m.signals.len()).sum::<usize>()
        );
    }

    #[test]
    fn records_one_skip_note_per_dropped_component_in_document_order() {
        let f = sample();
        assert_eq!(
            f.skipped,
            vec![
                "BuiltIn.CAN.DBC 'Sample DBC' (not a CAN frame)".to_string(),
                "message 'Sample DBC.VECTOR INDEPENDENT SIG MSG' (VECTOR independent-signal \
                 container; not representable by cantools, discarded on DBC load)"
                    .to_string(),
                "signal 'Sample DBC.VECTOR INDEPENDENT SIG MSG.Orphan' (in VECTOR \
                 independent-signal container)"
                    .to_string(),
                "message 'Sample DBC.No Id' (no CANId)".to_string(),
            ],
            "skip notes must match the Python reference byte-for-byte"
        );
    }

    #[test]
    fn sanitize_matches_the_python_reference_table() {
        for (input, expected) in [
            ("Sample DBC.Status", "Status"),
            ("DBC.Weird--Name!", "Weird_Name"),
            ("", "Unnamed"),
            ("9Lives", "_9Lives"),
        ] {
            assert_eq!(
                sanitize(input, "Sample DBC"),
                expected,
                "sanitize({input:?}, \"Sample DBC\")"
            );
        }
    }

    #[test]
    fn sanitize_strips_only_the_first_matching_prefix() {
        // The Python `break`s after the first prefix match, so a name that
        // starts with BOTH prefixes keeps the second one.
        assert_eq!(sanitize("Mod.DBC.Thing", "Mod"), "DBC_Thing");
    }

    const NESTED: &str = r#"<DBC>
 <ComponentStream>
  <List>
   <Component Classname="BuiltIn.CAN.Message" Name="Mod.Frame">
    <Props CANId="1A" IdType="Extended" DLC="10"/>
   </Component>
   <Component Classname="BuiltIn.CAN.Signal" Name="Mod.Frame.A" Receivers="Logger">
    <Props StartBit="B" Length="F" Type="" Endian="Big"/>
   </Component>
   <Component Classname="BuiltIn.CAN.Signal" Name="Mod.Frame.B" Endian="Big">
    <Props Type="f32" Length="20"/>
   </Component>
   <Component Classname="BuiltIn.CAN.Signal" Name="Mod.Frame.C">
    <Props Type="bool" Length="20"/>
   </Component>
  </List>
 </ComponentStream>
</DBC>
"#;

    #[test]
    fn finds_components_under_a_nested_component_stream() {
        // Real .m1dbc files wrap the stream in a <DBC> root; the fixture is a
        // bare <ComponentStream>. Both must work.
        let f = parse_m1dbc(NESTED.as_bytes(), "Mod").expect("must parse");
        assert_eq!(f.messages.len(), 1);
        assert_eq!(f.messages[0].name, "Frame");
    }

    #[test]
    fn applies_every_attribute_default_and_variant() {
        let f = parse_m1dbc(NESTED.as_bytes(), "Mod").expect("must parse");
        let m = &f.messages[0];
        assert_eq!(m.frame_id, 0x1A);
        assert!(m.is_extended, "IdType=\"Extended\"");
        assert_eq!(m.dlc, 10, "DLC=\"10\" is DECIMAL ten, not hex sixteen");
        assert_eq!(m.sender, "Vector__XXX", "absent Sender defaults");

        let a = &m.signals[0];
        assert_eq!(a.start_bit, 11, "StartBit=\"B\"");
        assert_eq!(a.length, 15, "Length=\"F\"");
        assert!(!a.is_signed, "Type=\"\" falls back to u32");
        assert!(!a.is_float);
        assert!(!a.little_endian, "Props Endian=\"Big\"");
        assert_eq!(a.receiver, "Logger");

        let b = &m.signals[1];
        assert_eq!(b.start_bit, 0, "absent StartBit defaults to 0");
        assert_eq!(b.length, 0x20, "Length=\"20\" is hexadecimal");
        assert!(b.is_float, "Type=\"f32\" starts with 'f'");
        assert!(!b.is_signed);
        assert!(!b.little_endian, "Component Endian=\"Big\"");

        let c = &m.signals[2];
        assert_eq!(c.length, 1, "bool ignores the Length attribute entirely");
        assert!(c.little_endian, "absent Endian defaults to Little");
    }

    #[test]
    fn drops_a_signal_whose_parent_message_is_absent() {
        let xml = r#"<ComponentStream><List>
   <Component Classname="BuiltIn.CAN.Signal" Name="Mod.Ghost.Sig"><Props/></Component>
</List></ComponentStream>"#;
        let f = parse_m1dbc(xml.as_bytes(), "Mod").expect("must parse");
        assert!(f.messages.is_empty());
        assert_eq!(f.totals.total_signals, 1);
        assert_eq!(
            f.totals.convertible_signals, 0,
            "a signal with no surviving parent message is never written"
        );
    }

    #[test]
    fn attaches_a_signal_that_precedes_its_message() {
        let xml = r#"<ComponentStream><List>
   <Component Classname="BuiltIn.CAN.Signal" Name="Mod.Late.Sig"><Props Length="4"/></Component>
   <Component Classname="BuiltIn.CAN.Message" Name="Mod.Late"><Props CANId="7"/></Component>
</List></ComponentStream>"#;
        let f = parse_m1dbc(xml.as_bytes(), "Mod").expect("must parse");
        assert_eq!(f.messages.len(), 1);
        assert_eq!(f.messages[0].signals.len(), 1, "the <List> is unordered");
        assert_eq!(f.messages[0].dlc, 8, "absent DLC defaults to 8");
    }

    #[test]
    fn a_component_with_no_classname_is_noted_as_unknown() {
        let xml = r#"<ComponentStream><List><Component Name="Odd"/></List></ComponentStream>"#;
        let f = parse_m1dbc(xml.as_bytes(), "Mod").expect("must parse");
        assert_eq!(
            f.skipped,
            vec!["unknown 'Odd' (not a CAN frame)".to_string()]
        );
    }

    #[test]
    fn rejects_input_that_is_not_xml() {
        let err = parse_m1dbc(b"not xml at all", "Mod").expect_err("must refuse");
        assert!(
            matches!(err, ExportError::Xml(_)),
            "expected an Xml error, got {err:?}"
        );
    }

    #[test]
    fn rejects_a_can_id_that_is_not_hexadecimal() {
        let xml = r#"<ComponentStream><List>
   <Component Classname="BuiltIn.CAN.Message" Name="Mod.Bad"><Props CANId="3FQ"/></Component>
</List></ComponentStream>"#;
        assert_eq!(
            parse_m1dbc(xml.as_bytes(), "Mod"),
            Err(ExportError::Invalid(
                "message 'Mod.Bad': CANId \"3FQ\" is not a hexadecimal integer".to_string()
            )),
            "refuse rather than guess"
        );
    }

    #[test]
    fn rejects_a_dlc_that_is_not_decimal() {
        let xml = r#"<ComponentStream><List>
   <Component Classname="BuiltIn.CAN.Message" Name="Mod.Bad"><Props CANId="7" DLC="A"/></Component>
</List></ComponentStream>"#;
        assert_eq!(
            parse_m1dbc(xml.as_bytes(), "Mod"),
            Err(ExportError::Invalid(
                "message 'Mod.Bad': DLC \"A\" is not a decimal integer".to_string()
            )),
            "DLC is the one decimal field; \"A\" must not silently mean 10"
        );
    }

    #[test]
    fn rejects_a_multiplier_that_is_not_a_number() {
        let xml = r#"<ComponentStream><List>
   <Component Classname="BuiltIn.CAN.Message" Name="Mod.M"><Props CANId="7"/></Component>
   <Component Classname="BuiltIn.CAN.Signal" Name="Mod.M.S"><Props Multiplier=""/></Component>
</List></ComponentStream>"#;
        assert_eq!(
            parse_m1dbc(xml.as_bytes(), "Mod"),
            Err(ExportError::Invalid(
                "signal 'Mod.M.S': Multiplier \"\" is not a number".to_string()
            ))
        );
    }

    #[test]
    fn rejects_a_component_with_no_props() {
        let xml = r#"<ComponentStream><List>
   <Component Classname="BuiltIn.CAN.Message" Name="Mod.Bare"/>
</List></ComponentStream>"#;
        assert_eq!(
            parse_m1dbc(xml.as_bytes(), "Mod"),
            Err(ExportError::Invalid(
                "message 'Mod.Bare': missing <Props>".to_string()
            ))
        );
    }

    #[test]
    fn parses_full_precision_scale_without_rounding() {
        // The corpus stores Multiplier/Offset in %.17e form; these are f32
        // values widened to f64 and must survive verbatim.
        let xml = r#"<ComponentStream><List>
   <Component Classname="BuiltIn.CAN.Message" Name="Mod.M"><Props CANId="7"/></Component>
   <Component Classname="BuiltIn.CAN.Signal" Name="Mod.M.S">
    <Props Multiplier="1.74532925199432955e-03" Offset="0.00000000000000000e+00"/>
   </Component>
</List></ComponentStream>"#;
        let f = parse_m1dbc(xml.as_bytes(), "Mod").expect("must parse");
        assert_eq!(f.messages[0].signals[0].scale, 0.0017453292519943296);
        assert_eq!(f.messages[0].signals[0].offset, 0.0);
    }

    #[test]
    fn export_error_displays_a_lowercase_sentence() {
        assert_eq!(
            ExportError::Xml("bad".into()).to_string(),
            "invalid .m1dbc XML: bad"
        );
        assert_eq!(ExportError::Invalid("boom".into()).to_string(), "boom");
        // Every variant, so a new one cannot be added without deciding how it
        // reads. `Dbc` wraps a third-party parser's diagnosis, which is the one
        // place the crate's one-sentence rule bends: the prefix is still a
        // lowercase sentence opener, the detail is the parser's own.
        assert_eq!(
            ExportError::Dbc("bad".into()).to_string(),
            "invalid .dbc text: bad"
        );
        assert_eq!(
            ExportError::Config("m1-tools.toml: [dbc] src_dir is missing".into()).to_string(),
            "m1-tools.toml: [dbc] src_dir is missing"
        );
    }
}
