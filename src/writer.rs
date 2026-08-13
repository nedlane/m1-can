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

use crate::m1dbc::{M1DbcFile, M1Message, M1Signal};
use std::collections::HashSet;

/// DBC's placeholder for "no node named". It is not a real node, so it is never
/// declared on the `BU_` line.
const DEFAULT_NODE: &str = "Vector__XXX";

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
        signal.unit.as_deref().unwrap_or(""),
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
/// DBC defines exactly two extended value types — 1 for IEEE single precision,
/// 2 for double — so only 32- and 64-bit floats can be declared. MoTeC can name
/// a width DBC cannot express (`Type="f16"`); such a signal is written as an
/// ordinary signal with no `SIG_VALTYPE_` rather than being mislabelled as a
/// single-precision float, which would silently change how a reader decodes it.
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
            name: name.to_string(),
            start_bit,
            length,
            little_endian: true,
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
            name: name.to_string(),
            frame_id,
            is_extended: false,
            dlc: 8,
            sender: "Vector__XXX".to_string(),
            signals,
        }
    }

    /// Wrap messages in a file. The counts are irrelevant to the writer — it
    /// renders [`M1DbcFile::messages`] and nothing else.
    fn file(messages: Vec<M1Message>) -> M1DbcFile {
        M1DbcFile {
            messages,
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
    fn a_float_of_an_unrepresentable_width_gets_no_sig_valtype_line() {
        // DBC knows only IEEE single and double; a MoTeC `Type="f16"` has no
        // extended value type, so it is written as a plain integer signal
        // rather than guessing one.
        let mut half = sig("Half", 0, 16);
        half.is_float = true;
        let out = write_dbc(&file(vec![msg("A", 1, vec![half])]));
        assert!(
            !out.contains("SIG_VALTYPE_ 1 "),
            "no extended value type for a 16-bit float: {out}"
        );
        assert!(
            out.contains(" SG_ Half : 0|16@1+ (1,0) [0|0] \"\" Vector__XXX\n"),
            "the signal itself is still written: {out}"
        );
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
