use crate::metadata::PlayerState;
use crate::source::Preset;
use serde::{Deserialize, Serialize};

/// A line of the `display` protocol.
///
/// **Adjacent** tagging, not internal: `PlayerState` contains a
/// `serde(flatten)` (`Track`), and the crossing of flatten × internally-tagged
/// is a known serde blind spot. Here the `data` of a state frame is
/// exactly the JSON that used to travel before the envelope.
///
/// **This enum is meant to grow**: every new variant is a message
/// that a display can ignore until it cares about it (see the default
/// body of `DisplayPlugin::sources_catalog` in the SDK). Nothing should therefore
/// assume the number of variants — neither an exhaustive `match` outside the SDK,
/// nor a count in a test.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "frame", content = "data", rename_all = "lowercase")]
// `PlayerState` weighs a good hundred bytes more than a sources_catalog, and
// clippy would like to put it in a `Box`. Refused: this envelope is
// built, serialized, then dropped within the same expression — the bytes
// live on the stack for the duration of a `to_string`. The box would trade that
// for one allocation per frame and per display, several times per second
// of playback, which is exactly the wrong direction on a Pi 2 B.
#[allow(clippy::large_enum_variant)]
pub enum DisplayFrame {
    State(PlayerState),
    Catalog(SourcesCatalog),
    Cover(Cover),
}

/// Cap on the bytes of a cover pushed over this protocol.
///
/// **Specific to this transport, and independent of any other cap.** The core
/// already applies a cap to a *download* (2 MiB, to rule out the bare
/// `front` from the Cover Art Archive), but that one only covers what comes
/// from the network: a `folder.jpg` from a share is treated as trusted and
/// streamed, with no size bound — the HTTP route never has to
/// materialize it. Pushing over a socket **forces** materialization, hence
/// the need for a bound here, which must not depend on that other one.
///
/// The value comes from measurement: serializing an image of `n` bytes into a
/// line of this protocol costs, at peak, about `3.6 × n` resident bytes (the
/// bytes, their base64, the rendered line).
///
/// **20 MiB, raised from 2 MiB.** The user keeps covers on their NAS
/// that exceeded 2 MiB — album scans, not plain thumbnails — and
/// explicitly accepted the memory cost after checking that the device
/// stays under 30% of its RAM at this cap. At the measured ratio, a 20 MiB cover
/// costs about 72 MiB of transient peak for the duration of a track
/// change, per subscribed display — under 7% of the device's 1024 MiB. This
/// value even covers a lossless PNG album scan, far heavier than a
/// comparable JPEG. Beyond that, it's no longer a cover but an accident: the
/// 150 MiB PNG on a share — the case the core's HTTP route names
/// explicitly (`cover_get`) as real — would cost, materialized over this
/// protocol, 540 MiB, half the machine.
///
/// Exceeding it is not an allocation error but a refusal: the producer never
/// materializes beyond it (it reads `COVER_MAX_BYTES + 1` bytes and stops,
/// or refuses on the size known in advance when it's available without
/// playback — see `cover::read_file_bounded` on the core side), no frame is
/// emitted, and the display simply has no image — the same silent-failure
/// policy as the retrieval itself.
pub const COVER_MAX_BYTES: usize = 20 * 1024 * 1024;

/// The cover of what's currently playing, pushed only to the displays that
/// requested it (see `Announcement::covers`).
///
/// A **self-contained** frame: one line carries an entire image, never a
/// chunk. This is what makes it compatible with the SDK's unreadable-line
/// policy — `warn` then `continue`, the connection survives: skipping a
/// self-contained line only loses one image, skipping a chunk would produce a
/// truncated image that no check would catch.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Cover {
    /// Exactly the `cover_href` that the state frame publishes for the same
    /// image (`/api/cover/{key}`).
    ///
    /// Without it, a display would have to guess which state the cover it
    /// just received corresponds to: frames do arrive in order over
    /// a single socket, but nothing in the image itself says *which one* it is,
    /// and a plugin that must answer "the cover for that track" (the
    /// MPD server) has no other correlation available to it.
    pub href: String,
    /// MIME type recognized from the header bytes, never from the extension
    /// nor a declared `Content-Type`.
    pub mime: String,
    /// The image bytes. In **base64** on the wire: the protocol is
    /// JSON per line, and a `Vec<u8>` that serde serializes raw becomes a
    /// decimal-number array — measured at 3.57 times the image's size,
    /// against 1.33 for base64, and 7.1 × n resident peak against 3.6.
    #[serde(with = "octets_base64")]
    pub bytes: Vec<u8>,
}

/// The bytes of a cover, in base64 on the wire.
///
/// The cap is applied **on read** and **before decoding**: this is what
/// keeps an oversized line from allocating the bytes it announces.
/// Not on write, and that's deliberate — a serialization refusal
/// would surface to the caller as a send failure indistinguishable from a
/// dead socket, which would break the core's relay loop and deprive the display
/// of *everything* for the rest of the process. So the cap is kept where it can
/// be handled: at the point of materializing the bytes, which never exceeds it.
mod octets_base64 {
    use base64::Engine as _;
    use serde::{Deserialize as _, Deserializer, Serializer};

    pub fn serialize<S: Serializer>(o: &[u8], s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(&base64::engine::general_purpose::STANDARD.encode(o))
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<Vec<u8>, D::Error> {
        use serde::de::Error as _;
        // `Cow` and not `String`: `serde_json::from_str` borrows the text of
        // the line when it has no escapes to undo, which avoids a
        // copy of 1.33 × n before decoding even starts.
        let text = std::borrow::Cow::<str>::deserialize(d)?;
        // The cap, checked on the **text length**: four base64
        // characters are worth three bytes, so the decoded size is known
        // before allocating anything at all. Padding `=` characters are
        // stripped, without which the bound would kick in up to two bytes too
        // early — enough to reject an image of *exactly* `COVER_MAX_BYTES`,
        // which the producer is entitled to emit.
        let padding = text.bytes().rev().take_while(|b| *b == b'=').count();
        if (text.len() / 4 * 3).saturating_sub(padding) > super::COVER_MAX_BYTES {
            return Err(D::Error::custom(format!(
                "cover refused: over {} bytes",
                super::COVER_MAX_BYTES
            )));
        }
        base64::engine::general_purpose::STANDARD
            .decode(text.as_bytes())
            .map_err(D::Error::custom)
    }
}

/// What is structural and rarely changing: the declared sources, in
/// `SourceCycle`'s switch order, and each one's named presets
/// when it knows how to enumerate them.
///
/// Deliberately **outside** `PlayerState`: that one is a snapshot,
/// deduplicates by equality and is rebuilt on every publish; a catalog
/// there would send fifty station names on every playback frame.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct SourcesCatalog {
    pub sources: Vec<SourceCatalog>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SourceCatalog {
    pub name: String,
    /// Empty = this source doesn't know how to enumerate. The consumer falls
    /// back to `preset_count`, which remains the source of truth for the count.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub presets: Vec<Preset>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn state_frame_envelope_carries_the_json_that_used_to_travel_before() {
        // Adjacent tagging guarantees that `data` is exactly the old
        // payload: this is what makes the migration verifiable.
        let state = PlayerState { source: "radio".into(), volume: 40, ..Default::default() };
        let bare = serde_json::to_value(&state).unwrap();
        let envelope = serde_json::to_value(DisplayFrame::State(state.clone())).unwrap();
        assert_eq!(envelope["frame"], "state");
        assert_eq!(envelope["data"], bare);
    }

    #[test]
    fn a_catalog_frame_round_trips() {
        let frame = DisplayFrame::Catalog(SourcesCatalog {
            sources: vec![SourceCatalog {
                name: "radio".into(),
                presets: vec![Preset { index: 1, name: "FIP".into() }],
            }],
        });
        let json = serde_json::to_string(&frame).unwrap();
        assert!(json.contains(r#""frame":"catalog""#), "{json}");
        assert_eq!(serde_json::from_str::<DisplayFrame>(&json).unwrap(), frame);
    }

    #[test]
    fn a_wire_state_line_reads_back_as_a_state_frame() {
        // The **read** direction, from bytes written by hand rather than
        // from a round trip: a round trip stays true if the tagging
        // changes on both sides at once, which is exactly the case where a
        // display on one version and a core on another no longer understand
        // each other.
        //
        // Deliberately separate from the catalog test: a loop over an array of
        // frames would need touching up on every variant added, and
        // `DisplayFrame` is meant to grow.
        let line = r#"{"frame":"state","data":{"source":"cd","volume":30,"muted":false,"standby":false,"preset":3}}"#;
        match serde_json::from_str::<DisplayFrame>(line).unwrap() {
            DisplayFrame::State(e) => {
                assert_eq!(e.source, "cd");
                assert_eq!(e.preset, Some(3));
                assert_eq!(e.volume, 30);
            }
            other => panic!("expected a state frame, got {other:?}"),
        }
    }

    #[test]
    fn a_source_without_named_presets_serializes_no_list() {
        let c = SourceCatalog { name: "cd".into(), presets: Vec::new() };
        assert!(!serde_json::to_string(&c).unwrap().contains("presets"));
    }

    #[test]
    fn a_source_without_a_list_reads_back_without_error() {
        // The counterpart of `skip_serializing_if`: what the serializer omits,
        // the deserializer must accept, otherwise a frame emitted by the core
        // would be unreadable by the display.
        let c: SourceCatalog = serde_json::from_str(r#"{"name":"cd"}"#).unwrap();
        assert_eq!(c.presets, Vec::new());
    }

    // What becomes of a frame of an unknown kind is checked where it's
    // observable, in the SDK: see
    // `a_cover_beyond_the_cap_is_an_unreadable_line_and_the_connection_survives`.
    // Testing it here would catch nothing — no configuration of `DisplayFrame`
    // can swallow an unknown kind, since `PlayerState`'s fields are mandatory,
    // and the assertion would pass even with the tagging removed (measured).

    // -- the cover frame ---------------------------------------------------

    /// Bytes that are not text: exactly what JSON-per-line
    /// cannot carry raw, and what the encoding must render bit for
    /// bit. `0x0A` is present on purpose — it's the protocol's line
    /// separator, and seeing it survive is the property that matters.
    fn hostile_bytes() -> Vec<u8> {
        let mut v = vec![0xFFu8, 0xD8, 0xFF, 0xE0];
        v.extend_from_slice(b"\n\r\0\"\\{}");
        v.extend((0u16..=255).map(|b| b as u8));
        v
    }

    #[test]
    fn a_cover_frame_renders_the_bytes_bit_for_bit_and_without_a_newline() {
        let bytes = hostile_bytes();
        let frame = DisplayFrame::Cover(Cover {
            href: "/api/cover/1a2b3c4d".into(),
            mime: "image/jpeg".into(),
            bytes: bytes.clone(),
        });
        let json = serde_json::to_string(&frame).unwrap();
        assert!(json.contains(r#""frame":"cover""#), "{json}");
        // The protocol is delimited by line breaks: a frame containing
        // one would cut the line in two, and both halves
        // would be unreadable.
        assert!(!json.contains('\n'), "a frame must not contain any line break");
        assert_eq!(serde_json::from_str::<DisplayFrame>(&json).unwrap(), frame);
    }

    #[test]
    fn a_covers_bytes_travel_as_base64_not_as_a_number_array() {
        // What serde would do with a raw `Vec<u8>` — `[255,216,...]` — has
        // been measured at 3.57 times the image's size and 7.1 × n resident
        // peak, against 1.33 and 3.6 for base64. This is the only reason for
        // the encoding, and so it's what this test guards.
        let frame = DisplayFrame::Cover(Cover {
            href: "/api/cover/x".into(),
            mime: "image/png".into(),
            bytes: vec![0xFF, 0xD8, 0xFF],
        });
        let json = serde_json::to_string(&frame).unwrap();
        assert!(json.contains(r#""bytes":"/9j/""#), "{json}");
        assert!(!json.contains("255"), "bytes must not travel as numbers: {json}");
    }

    #[test]
    fn a_cover_over_the_cap_is_rejected_on_read() {
        // Rejected **before** decoding: the base64 text's length already
        // tells the decoded size, so nothing is allocated for an oversized
        // line. A rejection, not an allocation panic.
        let too_big = "A".repeat((COVER_MAX_BYTES + 3) / 3 * 4 + 4);
        let line = format!(
            r#"{{"frame":"cover","data":{{"href":"/api/cover/x","mime":"image/jpeg","bytes":"{too_big}"}}}}"#
        );
        let e = serde_json::from_str::<DisplayFrame>(&line).unwrap_err();
        assert!(e.to_string().contains("over"), "unexpected message: {e}");
    }

    #[test]
    fn a_cover_just_under_the_cap_passes() {
        // The counterpart of the rejection: the bound must be *at* the cap,
        // not below it. Without this test, a cap accidentally halved
        // would go unnoticed.
        let bytes = vec![0x41u8; COVER_MAX_BYTES];
        let frame = DisplayFrame::Cover(Cover {
            href: "/api/cover/x".into(),
            mime: "image/jpeg".into(),
            bytes: bytes.clone(),
        });
        let json = serde_json::to_string(&frame).unwrap();
        match serde_json::from_str::<DisplayFrame>(&json).unwrap() {
            DisplayFrame::Cover(c) => assert_eq!(c.bytes.len(), bytes.len()),
            other => panic!("expected a cover frame: {other:?}"),
        }
    }

    #[test]
    fn invalid_base64_is_an_error_not_arbitrary_bytes() {
        let line = r#"{"frame":"cover","data":{"href":"/api/cover/x","mime":"image/jpeg","bytes":"!!!!"}}"#;
        assert!(
            serde_json::from_str::<DisplayFrame>(line).is_err(),
            "invalid encoding must be an error: the SDK treats it as an unreadable line"
        );
    }
}
