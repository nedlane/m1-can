//! Read generated `.dbc` text back through a third-party parser.
//!
//! [`writer`](crate::writer) defines this tool's canonical `.dbc` layout, so on
//! its own it can only prove that the writer agrees with itself. This module
//! supplies the missing half: it hands the text to the [`can_dbc`] crate — code
//! nobody here wrote — and reports what that parser found. If a construct we
//! emit is not real DBC, the count comes back wrong or the parse fails
//! outright, and the export is caught before it reaches a consumer.
//!
//! It is the same check the Python reference gets for free by writing through
//! `cantools` and reading back with it, except the reader here is genuinely
//! independent of the writer.
//!
//! The counts are of *frames*, so the independent-signal container is excluded
//! — see [`roundtrip_counts`].

use crate::ExportError;
use can_dbc::Dbc;

/// DBC's holding pen for signals that belong to no frame.
///
/// Tools park orphan signals in a pseudo-message under this name (conventionally
/// id `0xC0000000`) purely so the file stays well-formed. It is not a frame
/// anyone transmits, and `cantools` discards it on load, so counting it would
/// overstate every database that has one.
///
/// The **name** is the discriminator, not the id: `cantools` keys on the name,
/// and `can-dbc` masks an extended identifier down to its lower 29 bits, so a
/// container parsed by this module reports `0x80000000` rather than the
/// `0xC0000000` that was written.
const INDEPENDENT_SIG_MSG: &str = "VECTOR__INDEPENDENT_SIG_MSG";

/// Parse `dbc_text` with [`can_dbc`] and return `(messages, signals)`.
///
/// Both counts exclude the `VECTOR__INDEPENDENT_SIG_MSG` container and the
/// signals parked inside it. Our writer never emits one, but this function is the
/// arbiter for *any* DBC text — including a file some other tool produced — so
/// it applies the rule rather than assuming the input.
///
/// # Errors
///
/// [`ExportError::Dbc`] if the text is not a parseable CAN database, carrying
/// the third-party parser's own diagnosis. A malformed input is never a panic
/// and never a zero count.
pub fn roundtrip_counts(dbc_text: &str) -> Result<(usize, usize), ExportError> {
    let dbc = Dbc::try_from(dbc_text).map_err(|e| ExportError::Dbc(e.to_string()))?;
    Ok(dbc
        .messages
        .iter()
        .filter(|message| message.name != INDEPENDENT_SIG_MSG)
        .fold((0, 0), |(messages, signals), message| {
            (messages + 1, signals + message.signals.len())
        }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::m1dbc::{M1DbcFile, M1Message, M1Signal, SourceCounts, parse_m1dbc};
    use crate::writer::write_dbc;

    /// The Task 2 fixture, loaded as raw bytes: it is stored Windows-1252.
    const SAMPLE: &[u8] = include_bytes!("../tests/fixtures/Sample DBC.m1dbc");

    /// A hand-written database carrying one real message and the independent-
    /// signal container, so a count that failed to drop the container would come
    /// back as `(2, 3)` rather than `(1, 2)`.
    const WITH_INDEPENDENT_SIG_MSG: &str = "VERSION \"\"

NS_ :

BS_:

BU_: ECU

BO_ 256 Real: 8 ECU
 SG_ A : 0|8@1+ (1,0) [0|0] \"\" Vector__XXX
 SG_ B : 8|8@1+ (1,0) [0|0] \"\" Vector__XXX

BO_ 3221225472 VECTOR__INDEPENDENT_SIG_MSG: 0 Vector__XXX
 SG_ Orphan : 0|8@1+ (1,0) [0|0] \"\" Vector__XXX
";

    #[test]
    fn the_fixture_render_reads_back_as_one_message_and_two_signals() {
        let file = parse_m1dbc(SAMPLE, "Sample DBC").expect("the sample fixture must parse");
        let dbc = write_dbc(&file);
        assert_eq!(
            roundtrip_counts(&dbc),
            Ok((1, 2)),
            "an independent parser must see what the writer emitted:\n{dbc}"
        );
    }

    /// An IEEE float signal — the writer's baseline otherwise: little-endian,
    /// unsigned, unit-less, no named receiver.
    fn float_signal(name: &str, start_bit: u16, length: u16) -> M1Signal {
        M1Signal {
            name: name.to_string(),
            start_bit,
            length,
            little_endian: true,
            is_signed: false,
            is_float: true,
            scale: 1.0,
            offset: 0.0,
            unit: None,
            receiver: "Vector__XXX".to_string(),
        }
    }

    /// The two constructs the fixture cannot reach — a 29-bit identifier and the
    /// `SIG_VALTYPE_` section — checked against the independent parser.
    ///
    /// Both are built in memory rather than read from a corpus, so this runs in
    /// CI, where the corpus tests skip.
    #[test]
    fn an_extended_frame_carrying_ieee_floats_reads_back_intact() {
        let file = M1DbcFile {
            messages: vec![M1Message {
                name: "Floats".to_string(),
                frame_id: 0x18FF_50E5,
                is_extended: true,
                dlc: 8,
                sender: "Vector__XXX".to_string(),
                signals: vec![
                    float_signal("Single", 0, 32),
                    float_signal("Double", 32, 64),
                ],
            }],
            skipped: Vec::new(),
            totals: SourceCounts {
                total_messages: 1,
                total_signals: 2,
                convertible_messages: 1,
                convertible_signals: 2,
            },
        };
        let dbc = write_dbc(&file);
        assert_eq!(
            roundtrip_counts(&dbc),
            Ok((1, 2)),
            "an extended frame and its float signals must survive the trip:\n{dbc}"
        );

        // The counts alone would still pass if the identifier came back wrong or
        // the `SIG_VALTYPE_` lines were dropped, so check both directly.
        let parsed = Dbc::try_from(dbc.as_str()).expect("the rendered database must parse");
        assert_eq!(
            parsed.messages[0].id.raw(),
            0x18FF_50E5 | 0x8000_0000,
            "bit 31 marks the frame extended and the lower 29 bits are the id:\n{dbc}"
        );
        assert_eq!(
            parsed.signal_extended_value_type_list.len(),
            2,
            "one SIG_VALTYPE_ line per float signal must be read back:\n{dbc}"
        );
    }

    #[test]
    fn the_independent_signal_container_is_left_out_of_both_counts() {
        // Without this guard the test would still pass if `can-dbc` dropped the
        // container itself, and would prove nothing about our filtering.
        let parsed =
            Dbc::try_from(WITH_INDEPENDENT_SIG_MSG).expect("the hand-written database must parse");
        assert_eq!(
            parsed.messages.len(),
            2,
            "the parser hands us the container; dropping it is this module's job"
        );
        assert_eq!(
            roundtrip_counts(WITH_INDEPENDENT_SIG_MSG),
            Ok((1, 2)),
            "VECTOR__INDEPENDENT_SIG_MSG is a container, not a frame"
        );
    }

    #[test]
    fn the_container_is_recognised_by_name_because_its_id_does_not_survive_parsing() {
        // `can-dbc` masks an extended identifier to its lower 29 bits, so the
        // `0xC0000000` written above comes back as `0x80000000`. An id-based
        // filter would silently never fire; the name is what we can rely on.
        let parsed =
            Dbc::try_from(WITH_INDEPENDENT_SIG_MSG).expect("the hand-written database must parse");
        let container = parsed
            .messages
            .iter()
            .find(|m| m.name == INDEPENDENT_SIG_MSG)
            .expect("the container must be present under its conventional name");
        assert_ne!(
            container.id.raw(),
            0xC000_0000,
            "if this ever holds, an id check would be a viable filter too"
        );
    }

    #[test]
    fn signals_are_totalled_across_every_message() {
        let dbc = "VERSION \"\"

BS_:

BU_:

BO_ 1 A: 8 Vector__XXX
 SG_ S1 : 0|8@1+ (1,0) [0|0] \"\" Vector__XXX

BO_ 2 B: 8 Vector__XXX
 SG_ S2 : 0|8@1+ (1,0) [0|0] \"\" Vector__XXX
 SG_ S3 : 8|8@1+ (1,0) [0|0] \"\" Vector__XXX
";
        assert_eq!(roundtrip_counts(dbc), Ok((2, 3)));
    }

    #[test]
    fn a_database_with_no_messages_counts_nothing() {
        assert_eq!(
            roundtrip_counts("VERSION \"\"\n\nBS_:\n\nBU_:\n"),
            Ok((0, 0))
        );
    }

    #[test]
    fn text_that_is_not_a_dbc_is_an_error_rather_than_a_panic() {
        let err = roundtrip_counts("this is not a CAN database\n")
            .expect_err("unparseable text must not be reported as an empty database");
        assert!(
            matches!(err, ExportError::Dbc(ref m) if !m.is_empty()),
            "the parser's own diagnosis must be carried through: {err:?}"
        );
    }
}
