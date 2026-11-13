//! Render a parsed `.m1dbc` as standard Vector `.dbc` text.
//!
//! [`write_dbc`] is the whole module: a pure `&M1DbcFile -> String` with no I/O
//! and no hidden state, so the same input always produces the same string. That
//! is what makes a generated `.dbc` comparable against a committed one — any
//! difference is a real change in the source, never writer noise.
//!
//! This defines the tool's own canonical layout rather than reproducing some
//! other library's output byte-for-byte. Three rules carry the weight:
//!
//! - **Ordering is the source's.** Messages come out in `.m1dbc` document order
//!   and each message's signals in the order that file lists them. Nothing is
//!   sorted, so a diff between two exports is a diff between two sources.
//! - **Nothing is invented.** The minimum/maximum field is always `[0|0]`
//!   because MoTeC stores neither, and scale/offset are printed with shortest
//!   round-trip float formatting so an `f32`-precision multiplier such as
//!   `0.019999999552965164` survives the trip intact.
//! - **Identifiers are emitted whole.** Names longer than the 32 characters some
//!   tools truncate to are written out in full rather than shortened into a
//!   separate long-symbol table.
//!
//! The result is UTF-8 with LF line endings. Encoding it and putting it on disk
//! belongs to the caller.

use crate::ExportError;
use crate::m1dbc::{M1DbcFile, M1Message, M1Signal};
use std::collections::{HashMap, HashSet};

/// DBC's placeholder for "no node named". It is not a real node, so it is never
/// declared on the `BU_` line.
const DEFAULT_NODE: &str = "Vector__XXX";

/// Refuse layouts and identities that cannot be exported without losing data.
pub fn validate_export(file: &M1DbcFile) -> Result<(), ExportError> {
    let mut names = HashMap::new();
    let mut ids = HashMap::new();
    for message in &file.messages {
        if let Some(first) = names.insert(&message.name, &message.source_path) {
            return Err(ExportError::Invalid(format!(
                "messages '{first}' and '{}' both export as '{}'",
                message.source_path, message.name
            )));
        }
        if let Some(first) = ids.insert(
            (message.frame_id, message.is_extended),
            &message.source_path,
        ) {
            return Err(ExportError::Invalid(format!(
                "messages '{first}' and '{}' share {} CANId 0x{:X} in one export",
                message.source_path, message.id_type, message.frame_id
            )));
        }
        validate_node(&message.sender, &message.source_path, "Sender")?;
        let mut signal_names = HashMap::new();
        for signal in &message.signals {
            if let Some(first) = signal_names.insert(&signal.name, &signal.source_path) {
                return Err(ExportError::Invalid(format!(
                    "signals '{first}' and '{}' both export as '{}'",
                    signal.source_path, signal.name
                )));
            }
            validate_node(&signal.receiver, &signal.source_path, "Receivers")?;
            let unit = signal.unit.as_deref().unwrap_or("");
            if unit.ends_with('\\') || unit.chars().any(char::is_control) {
                return Err(ExportError::Invalid(format!(
                    "signal '{}': Unit contains an unsupported trailing backslash or control character",
                    signal.source_path
                )));
            }
            let expected_float_width = match signal.raw_type.as_str() {
                "f32" => Some(32),
                "f64" => Some(64),
                _ => None,
            };
            if expected_float_width.is_some_and(|width| signal.length != width) {
                return Err(ExportError::Invalid(format!(
                    "signal '{}': Type {} requires {} bits, got {}",
                    signal.source_path,
                    signal.raw_type,
                    expected_float_width.unwrap(),
                    signal.length
                )));
            }
            let limit = u32::from(message.dlc) * 8;
            let mut bit = u32::from(signal.start_bit);
            if signal.length == 0 {
                return Err(ExportError::Invalid(format!(
                    "signal '{}': Length must be nonzero",
                    signal.source_path
                )));
            }
            for _ in 0..signal.length {
                if bit >= limit {
                    return Err(ExportError::Invalid(format!(
                        "signal '{}': bit layout exceeds message '{}' DLC {}",
                        signal.source_path, message.source_path, message.dlc
                    )));
                }
                bit = if signal.little_endian {
                    bit + 1
                } else if bit % 8 == 0 {
                    bit + 15
                } else {
                    bit - 1
                };
            }
        }
    }
    Ok(())
}

fn validate_node(node: &str, path: &str, attr: &str) -> Result<(), ExportError> {
    let mut chars = node.chars();
    if !chars
        .next()
        .is_some_and(|c| c.is_ascii_alphabetic() || c == '_')
        || !chars.all(|c| c.is_ascii_alphanumeric() || c == '_')
    {
        return Err(ExportError::Invalid(format!(
            "component '{path}': {attr} {node:?} is not a single DBC node identifier"
        )));
    }
    Ok(())
}

/// Everything ahead of the `BU_` line: the version stub, the `NS_` table of
/// keywords a reader should expect, and the empty bit-timing section.
///
/// The list is fixed — no part of it comes from the `.m1dbc` — and its entries
/// are tab-indented, which is what every DBC tool writes and expects.
const HEADER: &str = "VERSION \"\"


NS_ :
\tNS_DESC_
\tCM_
\tBA_DEF_
\tBA_
\tVAL_
\tCAT_DEF_
\tCAT_
\tFILTER
\tBA_DEF_DEF_
\tEV_DATA_
\tENVVAR_DATA_
\tSGTYPE_
\tSGTYPE_VAL_
\tBA_DEF_SGTYPE_
\tBA_SGTYPE_
\tSIG_TYPE_REF_
\tVAL_TABLE_
\tSIG_GROUP_
\tSIG_VALTYPE_
\tSIGTYPE_VALTYPE_
\tBO_TX_BU_
\tBA_DEF_REL_
\tBA_REL_
\tBA_DEF_DEF_REL_
\tBU_SG_REL_
\tBU_EV_REL_
\tBU_BO_REL_
\tSG_MUL_VAL_

BS_:

";

/// Render `file` as the text of a Vector `.dbc`.
///
/// The output is the canonical header, a `BU_` node list, one blank-line-
/// separated block per message, and finally a `SIG_VALTYPE_` line for every
/// float signal. It ends with a single newline and contains no carriage
/// returns.
///
/// Deterministic by construction: the only inputs are the message list and its
/// order, and nothing is sorted, hashed into the output, or read from the
/// environment.
pub fn write_dbc(file: &M1DbcFile) -> String {
    let mut out = String::from(HEADER);

    out.push_str("BU_:");
    for node in nodes(file) {
        out.push(' ');
        out.push_str(node);
    }
    out.push('\n');

    for message in &file.messages {
        out.push('\n');
        out.push_str(&format!(
            "BO_ {} {}: {} {}\n",
            emitted_id(message),
            message.name,
            message.dlc,
            message.sender
        ));
        for signal in &message.signals {
            out.push_str(&signal_line(signal));
        }
    }

    // Extended value types are a file-level section, so they follow every
    // message block rather than sitting inside the block they describe.
    let mut opened = false;
    for message in &file.messages {
        for signal in &message.signals {
            let Some(value_type) = extended_value_type(signal) else {
                continue;
            };
            if !opened {
                out.push('\n');
                opened = true;
            }
            out.push_str(&format!(
                "SIG_VALTYPE_ {} {} : {};\n",
                emitted_id(message),
                signal.name,
                value_type
            ));
        }
    }

    out
}

/// One ` SG_ ` line.
///
/// `@1`/`@0` is little/big endian, `+`/`-` unsigned/signed, and the unit is
/// quoted even when empty. Scale and offset go through `{}` on `f64`, i.e.
/// shortest round-trip: `0.5` prints as `0.5` and `1` as `1`, with no `.0`
/// padding either way.
fn signal_line(signal: &M1Signal) -> String {
    format!(
        " SG_ {} : {}|{}@{}{} ({},{}) [0|0] \"{}\" {}\n",
        signal.name,
        signal.start_bit,
        signal.length,
        u8::from(signal.little_endian),
        if signal.is_signed { '-' } else { '+' },
        signal.scale,
        signal.offset,
        signal.unit.as_deref().unwrap_or("").replace('"', "\\\""),
        signal.receiver
    )
}

/// The identifier written on a message's `BO_` line, and on any `SIG_VALTYPE_`
/// that refers back to it.
///
/// DBC has no separate extended-frame flag: a 29-bit identifier is marked by
/// setting bit 31 of the number itself.
fn emitted_id(message: &M1Message) -> u32 {
    if message.is_extended {
        message.frame_id | 0x8000_0000
    } else {
        message.frame_id
    }
}

/// The `SIG_VALTYPE_` code for a signal, or `None` when it needs no such line.
///
/// DBC defines 1 for IEEE single precision and 2 for double. Export validation
/// rejects other float widths before the writer runs.
fn extended_value_type(signal: &M1Signal) -> Option<u8> {
    if !signal.is_float {
        return None;
    }
    match signal.length {
        32 => Some(1),
        64 => Some(2),
        _ => None,
    }
}

/// Every named node, deduplicated, in first-appearance order: for each message
/// in source order, its sender and then its signals' receivers.
fn nodes(file: &M1DbcFile) -> Vec<&str> {
    let mut seen = HashSet::new();
    let mut nodes = Vec::new();
    for message in &file.messages {
        let names = std::iter::once(message.sender.as_str())
            .chain(message.signals.iter().map(|s| s.receiver.as_str()));
        for name in names {
            if name != DEFAULT_NODE && seen.insert(name) {
                nodes.push(name);
            }
        }
    }
    nodes
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::m1dbc::{SourceCounts, parse_m1dbc};

    /// The Task 2 fixture, loaded as raw bytes: it is stored Windows-1252.
    const SAMPLE: &[u8] = include_bytes!("../tests/fixtures/Sample DBC.m1dbc");

    fn sample() -> M1DbcFile {
        parse_m1dbc(SAMPLE, "Sample DBC").expect("the sample fixture must parse")
    }

    /// An unsigned little-endian integer signal with no unit — the baseline the
    /// per-field tests vary one attribute at a time from.
    fn sig(name: &str, start_bit: u16, length: u16) -> M1Signal {
        M1Signal {
            source_path: format!("Test.Message.{name}"),
            name: name.to_string(),
            raw_type: "u32".to_string(),
            start_bit,
            length,
            little_endian: true,
            endian: "Little".to_string(),
            is_signed: false,
            is_float: false,
            scale: 1.0,
            offset: 0.0,
            unit: None,
            receiver: "Vector__XXX".to_string(),
        }
    }

    /// A standard-frame 8-byte message from an unnamed sender.
    fn msg(name: &str, frame_id: u32, signals: Vec<M1Signal>) -> M1Message {
        M1Message {
            source_path: format!("Test.{name}"),
            name: name.to_string(),
            frame_id,
            is_extended: false,
            id_type: "Standard".to_string(),
            dlc: 8,
            direction: None,
            sender: "Vector__XXX".to_string(),
            signals,
        }
    }

    /// Wrap messages in a file. The counts are irrelevant to the writer — it
    /// renders [`M1DbcFile::messages`] and nothing else.
    fn file(messages: Vec<M1Message>) -> M1DbcFile {
        M1DbcFile {
            module_paths: vec!["Test".to_string()],
            messages,
            orphan_signal_paths: Vec::new(),
            duplicate_message_paths: Vec::new(),
            skipped: Vec::new(),
            totals: SourceCounts {
                total_messages: 0,
                total_signals: 0,
                convertible_messages: 0,
                convertible_signals: 0,
            },
        }
    }

    #[test]
    fn writes_the_fixture_message_block_with_its_signals_in_source_order() {
        let out = write_dbc(&sample());
        // Source order, not start-bit order: `Yaw Rate` (bit 0) precedes
        // `Ready` (bit 0xA) because that is how the `.m1dbc` lists them.
        let block = "BO_ 1021 Status: 8 ECU\n\
                     \x20SG_ Yaw_Rate : 0|16@1- (0.5,-100) [0|0] \"°/s\" Vector__XXX\n\
                     \x20SG_ Ready : 10|1@1+ (1,0) [0|0] \"\" Vector__XXX\n";
        assert!(
            out.contains(block),
            "expected block:\n{block}\n--- got ---\n{out}"
        );
        assert_eq!(
            out.matches("BO_ ").count(),
            1,
            "the fixture has exactly one convertible message: {out}"
        );
    }

    #[test]
    fn two_calls_on_the_same_file_produce_identical_output() {
        let f = sample();
        assert_eq!(
            write_dbc(&f),
            write_dbc(&f),
            "the writer must be a pure function of its input"
        );
    }

    #[test]
    fn an_extended_frame_emits_its_id_with_bit_31_set() {
        let mut m = msg("Frame", 0x18FF50E5, vec![sig("Sig", 0, 8)]);
        m.is_extended = true;
        let out = write_dbc(&file(vec![m]));
        let id = 0x18FF50E5u32 | 0x8000_0000;
        assert!(
            out.contains(&format!("BO_ {id} Frame: 8 Vector__XXX\n")),
            "extended ids carry the DBC extended-frame flag: {out}"
        );
    }

    #[test]
    fn a_standard_frame_emits_its_can_id_as_plain_decimal() {
        let out = write_dbc(&file(vec![msg("Frame", 0x3FD, vec![])]));
        assert!(
            out.contains("BO_ 1021 Frame: 8 Vector__XXX\n"),
            "standard ids are written unmodified: {out}"
        );
    }

    /// R7: names longer than the 32 characters some DBC tools truncate to are
    /// written whole, with no `SystemSignalLongSymbol` attribute block to carry
    /// the full spelling. It holds by construction — nothing here measures a
    /// name — but it is a consumer-visible ruling (the BMU corpus has such
    /// names, and post-regeneration firmware sees them untruncated), so it is
    /// pinned rather than left to the corpus.
    #[test]
    fn identifiers_longer_than_thirty_two_characters_are_written_whole() {
        let message = "Battery_Management_Cell_Voltage_Summary";
        let signal = "Cell_Voltage_Minimum_Across_All_Modules";
        assert!(
            message.len() > 32 && signal.len() > 32,
            "the test names must exceed the 32-character truncation point"
        );

        let out = write_dbc(&file(vec![msg(message, 0x100, vec![sig(signal, 0, 16)])]));
        assert!(
            out.contains(&format!("BO_ 256 {message}: 8 Vector__XXX\n")),
            "the message name must appear in full: {out}"
        );
        assert!(
            out.contains(&format!(
                " SG_ {signal} : 0|16@1+ (1,0) [0|0] \"\" Vector__XXX\n"
            )),
            "the signal name must appear in full: {out}"
        );
        assert!(
            !out.contains("SystemSignalLongSymbol"),
            "long names are the format, not an attribute-table workaround: {out}"
        );
    }

    #[test]
    fn every_file_opens_with_the_canonical_header() {
        let out = write_dbc(&file(vec![]));
        assert!(
            out.starts_with("VERSION \"\"\n\n\nNS_ :\n\tNS_DESC_\n"),
            "header must be tab-indented and start with VERSION: {out:?}"
        );
        assert!(
            out.contains("\n\tSG_MUL_VAL_\n\nBS_:\n\nBU_:\n"),
            "the NS_ list ends at SG_MUL_VAL_, then BS_: and BU_: : {out:?}"
        );
        assert_eq!(
            out.matches("SIG_VALTYPE_").count(),
            1,
            "the only SIG_VALTYPE_ in a float-free file is the NS_ entry: {out}"
        );
    }

    #[test]
    fn bu_lists_each_named_node_once_in_first_appearance_order() {
        let mut a = msg("A", 1, vec![sig("S1", 0, 8), sig("S2", 8, 8)]);
        a.sender = "Dash".to_string();
        a.signals[0].receiver = "Logger".to_string();
        a.signals[1].receiver = "Dash".to_string();
        let mut b = msg("B", 2, vec![sig("S3", 0, 8)]);
        b.sender = "PDM".to_string();
        let out = write_dbc(&file(vec![a, b]));
        assert!(
            out.contains("\nBU_: Dash Logger PDM\n"),
            "senders and receivers, deduped, in first-appearance order: {out}"
        );
    }

    #[test]
    fn bu_is_bare_when_every_node_is_the_placeholder() {
        let out = write_dbc(&file(vec![msg("A", 1, vec![sig("S", 0, 8)])]));
        assert!(
            out.contains("\nBU_:\n"),
            "Vector__XXX is a placeholder, not a node: {out}"
        );
    }

    #[test]
    fn a_blank_line_separates_the_header_and_every_message_block() {
        let out = write_dbc(&file(vec![
            msg("A", 1, vec![sig("S1", 0, 8)]),
            msg("B", 2, vec![sig("S2", 0, 8)]),
        ]));
        assert!(
            out.ends_with(
                "BU_:\n\
                 \n\
                 BO_ 1 A: 8 Vector__XXX\n\
                 \x20SG_ S1 : 0|8@1+ (1,0) [0|0] \"\" Vector__XXX\n\
                 \n\
                 BO_ 2 B: 8 Vector__XXX\n\
                 \x20SG_ S2 : 0|8@1+ (1,0) [0|0] \"\" Vector__XXX\n"
            ),
            "one blank line before each block, one trailing newline: {out:?}"
        );
    }

    #[test]
    fn endianness_and_signedness_render_in_the_byte_order_field() {
        let mut s = sig("S", 4, 12);
        s.little_endian = false;
        s.is_signed = true;
        let out = write_dbc(&file(vec![msg("A", 1, vec![s])]));
        assert!(
            out.contains(" SG_ S : 4|12@0- (1,0) [0|0] \"\" Vector__XXX\n"),
            "@0 is big-endian, - is signed: {out}"
        );
    }

    #[test]
    fn scale_and_offset_use_shortest_round_trip_formatting() {
        let mut s = sig("S", 0, 16);
        // Straight from a corpus `Multiplier="1.99999995529651642e-02"` — an
        // f32-precision value widened to f64. It must not be rounded.
        s.scale = 0.019999999552965164;
        s.offset = -0.5;
        let out = write_dbc(&file(vec![msg("A", 1, vec![s])]));
        assert!(
            out.contains(" (0.019999999552965164,-0.5) "),
            "full f64 precision, no forced .0 suffix: {out}"
        );
    }

    #[test]
    fn float_signals_get_sig_valtype_lines_after_every_message_block() {
        let mut single = sig("Single", 0, 32);
        single.is_float = true;
        let mut double = sig("Double", 32, 64);
        double.is_float = true;
        let mut m = msg("Floats", 0x18FF50E5, vec![single, double]);
        m.is_extended = true;
        let out = write_dbc(&file(vec![m]));
        let id = 0x18FF50E5u32 | 0x8000_0000;
        assert!(
            out.ends_with(&format!(
                "\nSIG_VALTYPE_ {id} Single : 1;\nSIG_VALTYPE_ {id} Double : 2;\n"
            )),
            "32-bit floats are type 1, 64-bit type 2, keyed by the BO_ id: {out}"
        );
    }

    #[test]
    fn a_float_of_an_unrepresentable_width_is_rejected() {
        let mut single = sig("Single", 0, 16);
        single.raw_type = "f32".into();
        single.is_float = true;
        let error = validate_export(&file(vec![msg("A", 1, vec![single])])).unwrap_err();
        assert!(error.to_string().contains("f32 requires 32 bits"));
    }

    #[test]
    fn output_is_lf_only_and_ends_with_exactly_one_newline() {
        let out = write_dbc(&sample());
        assert!(!out.contains('\r'), "no CR bytes may be emitted: {out:?}");
        assert!(out.ends_with('\n'), "must end with a newline: {out:?}");
        assert!(
            !out.ends_with("\n\n"),
            "must not end with a blank line: {out:?}"
        );
    }

    /// The whole file, byte for byte.
    ///
    /// Every other test here pins one line or one field; this one pins the
    /// layout as a unit — all 28 `NS_` entries, the `BU_` line, and the blank
    /// lines between blocks — so no part of the output can drift unnoticed.
    #[test]
    fn the_sample_fixture_renders_to_exactly_this_file() {
        let expected = r#"VERSION ""


NS_ :
	NS_DESC_
	CM_
	BA_DEF_
	BA_
	VAL_
	CAT_DEF_
	CAT_
	FILTER
	BA_DEF_DEF_
	EV_DATA_
	ENVVAR_DATA_
	SGTYPE_
	SGTYPE_VAL_
	BA_DEF_SGTYPE_
	BA_SGTYPE_
	SIG_TYPE_REF_
	VAL_TABLE_
	SIG_GROUP_
	SIG_VALTYPE_
	SIGTYPE_VALTYPE_
	BO_TX_BU_
	BA_DEF_REL_
	BA_REL_
	BA_DEF_DEF_REL_
	BU_SG_REL_
	BU_EV_REL_
	BU_BO_REL_
	SG_MUL_VAL_

BS_:

BU_: ECU

BO_ 1021 Status: 8 ECU
 SG_ Yaw_Rate : 0|16@1- (0.5,-100) [0|0] "°/s" Vector__XXX
 SG_ Ready : 10|1@1+ (1,0) [0|0] "" Vector__XXX
"#;
        assert_eq!(write_dbc(&sample()), expected);
    }
}
