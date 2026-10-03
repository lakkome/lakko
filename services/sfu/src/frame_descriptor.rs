// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! Relay of the `generic-frame-descriptor-00` RTP header extension.
//!
//! ## Why this exists
//!
//! Lakko encrypts the whole encoded frame with SFrame before libwebrtc
//! packetizes it. A libwebrtc receiver reads the key-frame flag from the
//! first byte of the VP8 bitstream and the picture size from bytes 6..9 of
//! it (`modules/rtp_rtcp/source/video_rtp_depacketizer_vp8.cc:180` at
//! `branch-heads/6422`) — all of which the encryptor has replaced with
//! ciphertext. So a receiver cannot recognise a key frame, drops every
//! frame for missing references, and asks for a picture forever.
//!
//! The cure is to carry those facts *outside* the payload, in the
//! `generic-frame-descriptor-00` header extension, which both native
//! clients advertise (field trial `WebRTC-GenericDescriptorAdvertised`) and
//! the web client turns on per transceiver through the Header Extension
//! Control API (`apps/web/src/calls/frame-descriptor.ts`), since a page
//! cannot set a field trial.
//! On the peer-to-peer path that is enough. On the group path the media
//! goes through this SFU, so the SFU has to negotiate the extension with
//! each peer and put its value back on every packet it relays — otherwise
//! group video cannot work however correct the clients are.
//!
//! ## Opacity — what the SFU does and does not do
//!
//! **The SFU relays the descriptor byte for byte.** It runs `str0m` in RTP
//! mode ([ADR-0066](../../../docs/adr/0066-sfu-forwards-rtp-packets.md)): a
//! publisher's packet goes out to each subscriber as the same packet, so the
//! value each packet arrived with — a full first-packet value or a one-byte
//! continuation — is the value it leaves with ([`PacketDescriptor`]). The two
//! frame-boundary bits are therefore the publisher's own and stay correct,
//! because the SFU no longer re-cuts a frame into packets.
//!
//! It reads exactly two flags of a first-packet value, both already in the
//! clear: `B` (this packet begins a frame) and `D` (the frame lists
//! dependencies, so it is not a key frame) — see [`is_independent_frame`].
//! They decide when a simulcast layer switch may take effect and feed the
//! key-frame count in the diagnostic ledger. It never reads the frame id,
//! the resolution or the dependency list, and it never looks at the
//! SFrame-sealed payload (invariant #2, C-39).
//!
//! **This nonetheless widens what the relay transports.** Before this, the
//! frame type and picture size lived inside the encrypted payload and the
//! server could not see them. Carrying the descriptor puts them in the
//! clear on the wire, where a server that chose to look could read them.
//! That is the price of a decodable picture on the group path, and it is
//! deliberate: the alternative considered was leaving part of the *frame*
//! unencrypted, which would additionally have needed the plaintext
//! authenticated against a relay flipping the key-frame flag.
//!
//! ## Wire format — read from libwebrtc, not guessed
//!
//! `rtp_generic_frame_descriptor_extension.cc:33-57` at `branch-heads/6422`
//! gives byte 0 as `|B|E|F|L|D| T |`:
//!
//! - `B` (`0x80`) and `E` (`0x40`) are **per packet** — first / last packet
//!   of the frame.
//! - `F` (`0x20`) and `L` (`0x10`) are vestigial. `Write` sets both
//!   unconditionally and `Parse` never reads them; the source says so:
//!   *"In practice, they were always set to `true`."*
//! - `D` (`0x08`) and `T` (`0x07`), plus every later byte — spatial
//!   bitmask, frame id, width/height, dependency diffs — are **frame
//!   level**, and present **only** when `B = 1`.
//!
//! `rtp_generic_frame_descriptor.h:41` states the rule: *"Properties below
//! undefined if !FirstPacketInSubFrame()"*. When `B = 0` the value is
//! exactly one byte, and that is enforced on parse —
//! `if (!begins_subframe) { return data.size() == 1; }`.
//!
//! ## Two ways a value is written
//!
//! - **Relayed packet (the SFU).** No packetizer is involved, so
//!   `ExtensionValues::frame_position` is `None` and the serializer writes
//!   the [`PacketDescriptor`] the packet arrived with, unchanged.
//! - **Packetized frame (a `str0m` endpoint in frame mode).** The SFU itself
//!   never packetizes; this path exists for an endpoint that writes whole
//!   frames, which is what the synthetic clients in
//!   `tests/synthetic_client_e2e.rs` are. There the frame-level value must
//!   **not** be copied onto every packet: the receiver takes its frame
//!   boundaries from this extension in preference to the codec header
//!   (`rtp_video_stream_receiver2.cc:482-485`), so every packet would claim
//!   to be a whole frame and multi-packet frames would never assemble.
//!   [`descriptor_for_packet`] is the per-packet computation that avoids it.

use std::sync::Arc;

use str0m::rtp::{Extension, ExtensionSerializer, ExtensionValues, FramePosition};

/// The extension this module relays.
pub const GENERIC_FRAME_DESCRIPTOR_URI: &str =
    "http://www.webrtc.org/experiments/rtp-hdrext/generic-frame-descriptor-00";

/// Extension id the SFU registers the URI under.
///
/// **This is libwebrtc's own id for this extension, and that is the whole
/// reason for the value.** `WebRtcVideoEngine::GetRtpHeaderExtensions`
/// (`media/engine/webrtc_video_engine.cc:812-852` at `branch-heads/6422`)
/// numbers the video extensions sequentially — 1 toffset, 2 abs-send-time,
/// 3 video-rotation, 4 transport-cc, 5 playout-delay, 6 video-content-type,
/// 7 video-timing, 8 color-space, 9 mid, 10 rid, 11 repaired-rid — and then
/// stops incrementing, so abs-capture-time, the generic frame descriptor,
/// the dependency descriptor and video-layers-allocation all share **12**.
///
/// It is tempting to think the value does not matter, because
/// `ExtensionMap::remap` relocates our entry onto whatever id the peer
/// offered (`Extension`'s `PartialEq` compares an `UnknownUri` by URI
/// alone, so the match succeeds whichever serializer each side holds).
/// That reasoning has a hole: **the map is per-peer, and remap only runs
/// for extensions the peer itself offered.** A peer that joined a call
/// audio-only never offered a video m-line, so nothing ever pins its id —
/// and when the SFU later renegotiates a video track onto that peer, it
/// offers this extension at whatever it was registered as. Matching
/// libwebrtc makes the remapped and the never-remapped case agree.
///
/// The earlier value here was 5, which is free in `ExtensionMap::standard()`
/// (1, 2, 3, 4, 10, 11, 13) but is playout-delay in libwebrtc's table — so
/// an audio-only joiner was offered this extension on the slot its own stack
/// reserves for something else.
pub const GENERIC_FRAME_DESCRIPTOR_ID: u8 = 12;

/// First packet of the frame.
const FLAG_BEGIN_OF_SUBFRAME: u8 = 0x80;
/// Last packet of the frame.
const FLAG_END_OF_SUBFRAME: u8 = 0x40;
/// Vestigial `F`. libwebrtc's `Write` always sets it; `Parse` ignores it.
const FLAG_FIRST_SUBFRAME_V00: u8 = 0x20;
/// Vestigial `L`. Same.
const FLAG_LAST_SUBFRAME_V00: u8 = 0x10;
/// `D`: the frame lists dependency diffs, so it is not a key frame.
const FLAG_DEPENDENCIES: u8 = 0x08;

/// Shortest first-packet value libwebrtc's parser accepts: flags, spatial
/// bitmask, and the two frame-id bytes (`if (data.size() < 4) return false`).
const MIN_FIRST_PACKET_LEN: usize = 4;

/// Longest value the one-byte RFC 8285 header form can carry.
const MAX_ONE_BYTE_FORM_LEN: usize = 16;

/// A publisher's frame-level descriptor, as observed on the first packet of
/// one frame.
///
/// Opaque bytes apart from the two flags [`is_independent_frame`] reads.
/// Stored in `ExtensionValues::user_values`, which is keyed by type, so this
/// newtype is the key. Present only on a packet that begins a frame; a frame
/// endpoint sets it on a frame it writes and reads it off a frame it
/// assembles.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FrameDescriptor(pub Vec<u8>);

/// The descriptor value exactly as one RTP packet carried it: a first-packet
/// value or a one-byte continuation.
///
/// This is what the SFU relays. It is stored for every packet that carries
/// the extension and written back verbatim on the packet the SFU forwards,
/// so the frame-boundary bits a subscriber sees are the publisher's.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PacketDescriptor(pub Vec<u8>);

/// Whether a stored first-packet descriptor value marks an
/// independent frame, i.e. one with no frame dependencies: a key
/// frame. libwebrtc writes `D` (`0x08`) when the frame lists
/// dependency diffs and leaves it clear for a key frame
/// (`rtp_generic_frame_descriptor_extension.cc`, the `kFlagDependencies`
/// branch of `Write`). Returns `None` for a value that is not a
/// first-packet value, where the flag is undefined.
#[must_use]
pub fn is_independent_frame(value: &[u8]) -> Option<bool> {
    let first = *value.first()?;
    if first & FLAG_BEGIN_OF_SUBFRAME == 0 || value.len() < MIN_FIRST_PACKET_LEN {
        return None;
    }
    Some(first & FLAG_DEPENDENCIES == 0)
}

/// Build the descriptor value for one packet of a frame being packetized.
///
/// Used only by an endpoint that writes whole frames (`str0m` frame mode);
/// the SFU relays packets and writes [`PacketDescriptor`] verbatim instead.
///
/// `frame_value` is the first-packet value for this frame — the only one
/// that carries the frame-level fields. `position` says where the packet
/// being written sits in the packetized frame.
///
/// - First packet: the frame's bytes with `B` set and `E` set iff this
///   packet is also the frame's last. Everything after byte 0, and byte 0's
///   own low nibble, is copied untouched.
/// - Any later packet: a single byte, `E` iff last, plus the two vestigial
///   flags libwebrtc always writes.
/// - `None` position: written verbatim. The packetizer did not produce this
///   value, so there is no packetization to compensate for.
///
/// Returns `None` when nothing should be written — an empty value, or a
/// stored value that is not itself a first-packet value and so cannot be
/// expanded into one. Writing a malformed descriptor would be worse than
/// writing none: libwebrtc's parser rejects a `B = 1` value shorter than
/// four bytes and drops the packet.
#[must_use]
pub fn descriptor_for_packet(
    frame_value: &[u8],
    position: Option<FramePosition>,
) -> Option<Vec<u8>> {
    if frame_value.is_empty() {
        return None;
    }
    let Some(position) = position else {
        return Some(frame_value.to_vec());
    };

    if !position.first {
        let mut byte = FLAG_FIRST_SUBFRAME_V00 | FLAG_LAST_SUBFRAME_V00;
        if position.last {
            byte |= FLAG_END_OF_SUBFRAME;
        }
        return Some(vec![byte]);
    }

    // A first packet needs the frame-level fields, which only a
    // first-packet value carries. A continuation byte cannot be expanded
    // into one.
    if frame_value[0] & FLAG_BEGIN_OF_SUBFRAME == 0 || frame_value.len() < MIN_FIRST_PACKET_LEN {
        return None;
    }

    let mut out = frame_value.to_vec();
    out[0] |= FLAG_BEGIN_OF_SUBFRAME | FLAG_FIRST_SUBFRAME_V00 | FLAG_LAST_SUBFRAME_V00;
    if position.last {
        out[0] |= FLAG_END_OF_SUBFRAME;
    } else {
        out[0] &= !FLAG_END_OF_SUBFRAME;
    }
    Some(out)
}

/// The value to put on one outgoing packet, or `None` to omit the extension.
///
/// A packet a packetizer produced (`frame_position` is set) gets the
/// per-packet form of the frame's value. Any other packet — every packet the
/// SFU relays — gets the value it arrived with, unchanged; a value set by
/// hand as a [`FrameDescriptor`] with no position is written verbatim too.
fn value_to_write(ev: &ExtensionValues) -> Option<Vec<u8>> {
    if let Some(position) = ev.frame_position {
        let stored = ev.user_values.get::<FrameDescriptor>()?;
        return descriptor_for_packet(&stored.0, Some(position));
    }
    if let Some(packet) = ev.user_values.get::<PacketDescriptor>() {
        return (!packet.0.is_empty()).then(|| packet.0.clone());
    }
    let stored = ev.user_values.get::<FrameDescriptor>()?;
    descriptor_for_packet(&stored.0, None)
}

/// Serializer that carries the descriptor across the relay.
///
/// Video only: libwebrtc registers this extension on video media lines, and
/// an audio packet has no frame descriptor to relay.
#[derive(Debug, Clone, Copy)]
pub struct GenericFrameDescriptorSerializer;

impl ExtensionSerializer for GenericFrameDescriptorSerializer {
    /// Store the value this packet carried.
    ///
    /// Every non-empty value is kept as a [`PacketDescriptor`]: that is what
    /// the SFU writes back on the packet it relays. A **first-packet** value
    /// is additionally kept as a [`FrameDescriptor`], the only form that
    /// carries the frame-level fields; a frame endpoint finds it on the
    /// assembled frame, and the SFU reads its key-frame flag.
    ///
    /// Returns `true` either way: a continuation byte is a well-formed part
    /// of the stream, not a parse failure, and reporting a failure would
    /// make str0m discard the whole extension for that packet.
    fn parse_value(&self, buf: &[u8], ev: &mut ExtensionValues) -> bool {
        if let Some(&first) = buf.first() {
            ev.user_values.set(PacketDescriptor(buf.to_vec()));
            if first & FLAG_BEGIN_OF_SUBFRAME != 0 && buf.len() >= MIN_FIRST_PACKET_LEN {
                ev.user_values.set(FrameDescriptor(buf.to_vec()));
            }
        }
        true
    }

    /// Write this packet's descriptor value.
    ///
    /// Returning `0` omits the extension from the packet; str0m maps it to
    /// `None` rather than asserting, so a value that does not fit is dropped
    /// and never truncated.
    fn write_to(&self, buf: &mut [u8], ev: &ExtensionValues) -> usize {
        let Some(value) = value_to_write(ev) else {
            return 0;
        };
        if value.len() > buf.len() {
            return 0;
        }
        buf[..value.len()].copy_from_slice(&value);
        value.len()
    }

    fn is_video(&self) -> bool {
        true
    }

    fn is_audio(&self) -> bool {
        false
    }

    /// A key frame's descriptor carries a 4-byte resolution, and a frame may
    /// carry up to eight dependency diffs of two bytes each, so the value can
    /// reach 20 bytes — past what the one-byte RFC 8285 form can express.
    /// Answering honestly is what keeps `ExtensionMap::write_to`'s
    /// `assert!(n <= 16)` from firing on a live call.
    fn requires_two_byte_form(&self, ev: &ExtensionValues) -> bool {
        value_to_write(ev).is_some_and(|v| v.len() > MAX_ONE_BYTE_FORM_LEN)
    }
}

/// The `Extension` to register on a peer's session extension map.
#[must_use]
pub fn generic_frame_descriptor_extension() -> Extension {
    Extension::UnknownUri(
        GENERIC_FRAME_DESCRIPTOR_URI.to_owned(),
        Arc::new(GenericFrameDescriptorSerializer),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn independent_frame_flag_reads_d_on_a_first_packet_value_only() {
        // Key frame: first packet, no dependency diffs.
        assert_eq!(
            is_independent_frame(&[FLAG_BEGIN_OF_SUBFRAME, 0x01, 0x00, 0x2a]),
            Some(true)
        );
        // Delta frame: first packet with `D` set and one diff byte.
        assert_eq!(
            is_independent_frame(&[
                FLAG_BEGIN_OF_SUBFRAME | FLAG_DEPENDENCIES,
                0x01,
                0x00,
                0x2a,
                0x04
            ]),
            Some(false)
        );
        // Not a first packet: the flag is undefined there.
        assert_eq!(
            is_independent_frame(&[FLAG_END_OF_SUBFRAME
                | FLAG_FIRST_SUBFRAME_V00
                | FLAG_LAST_SUBFRAME_V00]),
            None
        );
        // Claims to be a first packet but is too short to be one.
        assert_eq!(is_independent_frame(&[FLAG_BEGIN_OF_SUBFRAME, 0x01]), None);
        assert_eq!(is_independent_frame(&[]), None);
    }

    /// A publisher's first-packet value for a key frame: flags `B|F|L`,
    /// spatial bitmask, frame id, then a 4-byte resolution.
    fn key_frame_value() -> Vec<u8> {
        vec![
            FLAG_BEGIN_OF_SUBFRAME
                | FLAG_END_OF_SUBFRAME
                | FLAG_FIRST_SUBFRAME_V00
                | FLAG_LAST_SUBFRAME_V00,
            0x01, // spatial layers bitmask
            0x2a, // frame id low
            0x00, // frame id high
            0x05,
            0x00, // width  1280
            0x02,
            0xd0, // height 720
        ]
    }

    #[allow(
        clippy::unnecessary_wraps,
        reason = "the API under test takes an Option"
    )]
    fn at(first: bool, last: bool) -> Option<FramePosition> {
        Some(FramePosition { first, last })
    }

    #[test]
    fn first_packet_keeps_every_frame_level_field() {
        let value = key_frame_value();
        let out = descriptor_for_packet(&value, at(true, false)).expect("first packet writes");
        // Byte 0's low nibble (D + temporal layer) and every later byte are
        // the publisher's, untouched — the SFU rewrites boundaries only.
        assert_eq!(out[1..], value[1..]);
        assert_eq!(out[0] & 0x0f, value[0] & 0x0f);
        assert_ne!(out[0] & FLAG_BEGIN_OF_SUBFRAME, 0, "B must be set");
    }

    #[test]
    fn a_middle_packet_is_one_byte_with_neither_boundary() {
        let out = descriptor_for_packet(&key_frame_value(), at(false, false))
            .expect("middle packet writes");
        assert_eq!(
            out.len(),
            1,
            "libwebrtc parses a non-first value as exactly one byte"
        );
        assert_eq!(out[0] & FLAG_BEGIN_OF_SUBFRAME, 0);
        assert_eq!(out[0] & FLAG_END_OF_SUBFRAME, 0);
    }

    #[test]
    fn a_last_packet_is_one_byte_carrying_end_of_frame() {
        let out =
            descriptor_for_packet(&key_frame_value(), at(false, true)).expect("last packet writes");
        assert_eq!(out.len(), 1);
        assert_eq!(out[0] & FLAG_BEGIN_OF_SUBFRAME, 0);
        assert_ne!(out[0] & FLAG_END_OF_SUBFRAME, 0, "E must be set");
    }

    #[test]
    fn a_multi_packet_frame_carries_exactly_one_begin_and_one_end() {
        let value = key_frame_value();
        let packets = 5;
        let mut begins = 0;
        let mut ends = 0;
        for idx in 0..packets {
            let out = descriptor_for_packet(&value, at(idx == 0, idx == packets - 1))
                .expect("every packet writes");
            if out[0] & FLAG_BEGIN_OF_SUBFRAME != 0 {
                begins += 1;
            }
            if out[0] & FLAG_END_OF_SUBFRAME != 0 {
                ends += 1;
            }
        }
        // This is the property the whole change exists for: the receiver
        // takes frame boundaries from this extension, so a frame that
        // reported five beginnings would assemble as five broken frames.
        assert_eq!(begins, 1);
        assert_eq!(ends, 1);
    }

    #[test]
    fn a_single_packet_frame_is_both_first_and_last() {
        let out = descriptor_for_packet(&key_frame_value(), at(true, true)).expect("writes");
        assert_ne!(out[0] & FLAG_BEGIN_OF_SUBFRAME, 0);
        assert_ne!(out[0] & FLAG_END_OF_SUBFRAME, 0);
        assert!(out.len() >= MIN_FIRST_PACKET_LEN);
    }

    #[test]
    fn a_first_packet_that_is_not_last_clears_end_of_frame() {
        // The stored value has E set (the publisher's frame was one packet).
        // Re-packetized into several, the first packet must no longer claim
        // to end the frame.
        let value = key_frame_value();
        assert_ne!(value[0] & FLAG_END_OF_SUBFRAME, 0, "fixture has E set");
        let out = descriptor_for_packet(&value, at(true, false)).expect("writes");
        assert_eq!(out[0] & FLAG_END_OF_SUBFRAME, 0);
    }

    #[test]
    fn no_position_writes_the_stored_value_verbatim() {
        let value = key_frame_value();
        let out = descriptor_for_packet(&value, None).expect("writes");
        assert_eq!(out, value);
    }

    #[test]
    fn an_empty_value_writes_nothing() {
        assert!(descriptor_for_packet(&[], at(true, true)).is_none());
        assert!(descriptor_for_packet(&[], None).is_none());
    }

    #[test]
    fn a_continuation_byte_cannot_be_expanded_into_a_first_packet() {
        // One byte with B clear carries no frame-level field. Setting B on it
        // would produce a value libwebrtc's parser rejects outright
        // (`if (data.size() < 4) return false`), so we write nothing instead.
        let continuation = vec![FLAG_FIRST_SUBFRAME_V00 | FLAG_LAST_SUBFRAME_V00];
        assert!(descriptor_for_packet(&continuation, at(true, true)).is_none());
        // A later packet needs none of those fields, so it still works.
        assert!(descriptor_for_packet(&continuation, at(false, true)).is_some());
    }

    #[test]
    fn a_truncated_first_packet_value_is_refused() {
        let short = vec![FLAG_BEGIN_OF_SUBFRAME, 0x01, 0x00];
        assert!(descriptor_for_packet(&short, at(true, true)).is_none());
    }

    #[test]
    fn parse_keeps_a_first_packet_value_and_ignores_a_continuation() {
        let ser = GenericFrameDescriptorSerializer;
        let mut ev = ExtensionValues::default();

        assert!(ser.parse_value(&key_frame_value(), &mut ev));
        let kept = ev.user_values.get::<FrameDescriptor>().cloned();
        assert_eq!(kept, Some(FrameDescriptor(key_frame_value())));

        // A continuation byte must not displace it.
        assert!(ser.parse_value(&[FLAG_FIRST_SUBFRAME_V00], &mut ev));
        assert_eq!(ev.user_values.get::<FrameDescriptor>().cloned(), kept);
    }

    /// Parse `wire` as an incoming packet's value and write it back out the
    /// way the SFU does for a packet it relays: only the per-packet value is
    /// carried over, and there is no frame position.
    fn relay(wire: &[u8]) -> Vec<u8> {
        let ser = GenericFrameDescriptorSerializer;
        let mut incoming = ExtensionValues::default();
        assert!(ser.parse_value(wire, &mut incoming));
        let mut outgoing = ExtensionValues::default();
        if let Some(d) = incoming.user_values.get::<PacketDescriptor>() {
            outgoing.user_values.set(d.clone());
        }
        let mut buf = [0_u8; 64];
        let n = ser.write_to(&mut buf, &outgoing);
        buf[..n].to_vec()
    }

    #[test]
    fn parse_keeps_every_packets_own_value_for_the_relay() {
        let ser = GenericFrameDescriptorSerializer;
        let mut ev = ExtensionValues::default();
        let continuation = [FLAG_END_OF_SUBFRAME | FLAG_FIRST_SUBFRAME_V00];
        assert!(ser.parse_value(&continuation, &mut ev));
        assert_eq!(
            ev.user_values.get::<PacketDescriptor>(),
            Some(&PacketDescriptor(continuation.to_vec())),
            "a continuation byte is what that packet must be relayed with"
        );
        assert!(ev.user_values.get::<FrameDescriptor>().is_none());

        let mut ev = ExtensionValues::default();
        assert!(ser.parse_value(&key_frame_value(), &mut ev));
        assert_eq!(
            ev.user_values.get::<PacketDescriptor>(),
            Some(&PacketDescriptor(key_frame_value()))
        );
    }

    #[test]
    fn a_relayed_first_packet_keeps_the_publishers_boundary_bits() {
        // First packet of a multi-packet frame: B set, E clear.
        let mut first = key_frame_value();
        first[0] &= !FLAG_END_OF_SUBFRAME;
        assert_eq!(relay(&first), first, "B set and E clear must survive");
        // Single-packet frame: both set.
        assert_eq!(relay(&key_frame_value()), key_frame_value());
    }

    #[test]
    fn a_relayed_continuation_packet_is_the_publishers_one_byte() {
        let middle = [FLAG_FIRST_SUBFRAME_V00 | FLAG_LAST_SUBFRAME_V00];
        let last = [FLAG_END_OF_SUBFRAME | FLAG_FIRST_SUBFRAME_V00 | FLAG_LAST_SUBFRAME_V00];
        assert_eq!(relay(&middle), middle);
        assert_eq!(relay(&last), last);
    }

    #[test]
    fn a_relayed_multi_packet_frame_has_one_begin_and_one_end() {
        // What a publisher's packetizer emits for a three-packet frame.
        let mut first = key_frame_value();
        first[0] &= !FLAG_END_OF_SUBFRAME;
        let packets: [Vec<u8>; 3] = [
            first,
            vec![FLAG_FIRST_SUBFRAME_V00 | FLAG_LAST_SUBFRAME_V00],
            vec![FLAG_END_OF_SUBFRAME | FLAG_FIRST_SUBFRAME_V00 | FLAG_LAST_SUBFRAME_V00],
        ];
        let relayed: Vec<Vec<u8>> = packets.iter().map(|p| relay(p)).collect();
        assert_eq!(relayed, packets, "every packet leaves as it arrived");
        let begins = relayed
            .iter()
            .filter(|p| p[0] & FLAG_BEGIN_OF_SUBFRAME != 0)
            .count();
        let ends = relayed
            .iter()
            .filter(|p| p[0] & FLAG_END_OF_SUBFRAME != 0)
            .count();
        assert_eq!((begins, ends), (1, 1));
    }

    #[test]
    fn a_relayed_long_value_asks_for_the_two_byte_form() {
        let ser = GenericFrameDescriptorSerializer;
        let mut long = vec![FLAG_BEGIN_OF_SUBFRAME | FLAG_DEPENDENCIES, 0x01, 0x2a, 0x00];
        long.extend(std::iter::repeat_n(0x00, 16));
        let mut ev = ExtensionValues::default();
        ev.user_values.set(PacketDescriptor(long.clone()));
        assert!(ser.requires_two_byte_form(&ev));
        let mut buf = [0_u8; 64];
        let n = ser.write_to(&mut buf, &ev);
        assert_eq!(&buf[..n], &long[..]);

        let mut short = ExtensionValues::default();
        short.user_values.set(PacketDescriptor(key_frame_value()));
        assert!(!ser.requires_two_byte_form(&short));
    }

    #[test]
    fn an_empty_packet_value_is_neither_stored_nor_written() {
        let ser = GenericFrameDescriptorSerializer;
        let mut ev = ExtensionValues::default();
        assert!(ser.parse_value(&[], &mut ev));
        assert!(ev.user_values.get::<PacketDescriptor>().is_none());

        let mut ev = ExtensionValues::default();
        ev.user_values.set(PacketDescriptor(Vec::new()));
        let mut buf = [0_u8; 8];
        assert_eq!(ser.write_to(&mut buf, &ev), 0);
    }

    #[test]
    fn a_packetizer_position_takes_the_frame_value_not_the_packet_value() {
        // A frame endpoint that received a frame and writes it out again
        // holds both forms; the packetizer's position decides.
        let ser = GenericFrameDescriptorSerializer;
        let mut ev = ExtensionValues::default();
        ser.parse_value(&key_frame_value(), &mut ev);
        ev.frame_position = at(false, false);
        let mut buf = [0_u8; 32];
        assert_eq!(
            ser.write_to(&mut buf, &ev),
            1,
            "a middle packet is one byte"
        );
    }

    #[test]
    fn parse_never_reports_failure_for_a_well_formed_continuation() {
        // Reporting failure would make str0m discard the extension for that
        // packet, which is not what a legitimate continuation byte deserves.
        let ser = GenericFrameDescriptorSerializer;
        let mut ev = ExtensionValues::default();
        assert!(ser.parse_value(&[FLAG_FIRST_SUBFRAME_V00 | FLAG_LAST_SUBFRAME_V00], &mut ev));
        assert!(ev.user_values.get::<FrameDescriptor>().is_none());
    }

    #[test]
    fn write_round_trips_through_the_serializer() {
        let ser = GenericFrameDescriptorSerializer;
        let mut ev = ExtensionValues::default();
        ser.parse_value(&key_frame_value(), &mut ev);

        ev.frame_position = at(true, false);
        let mut buf = [0_u8; 32];
        let n = ser.write_to(&mut buf, &ev);
        assert_eq!(
            &buf[..n],
            &descriptor_for_packet(&key_frame_value(), at(true, false)).expect("value")[..]
        );

        ev.frame_position = at(false, true);
        let n = ser.write_to(&mut buf, &ev);
        assert_eq!(n, 1);
    }

    #[test]
    fn write_omits_the_extension_rather_than_truncating() {
        let ser = GenericFrameDescriptorSerializer;
        let mut ev = ExtensionValues::default();
        ser.parse_value(&key_frame_value(), &mut ev);
        ev.frame_position = at(true, true);
        // Two bytes cannot hold an 8-byte key-frame descriptor.
        let mut buf = [0_u8; 2];
        assert_eq!(ser.write_to(&mut buf, &ev), 0);
    }

    #[test]
    fn write_without_a_stored_descriptor_writes_nothing() {
        let ser = GenericFrameDescriptorSerializer;
        let mut buf = [0_u8; 32];
        assert_eq!(ser.write_to(&mut buf, &ExtensionValues::default()), 0);
    }

    #[test]
    fn two_byte_form_is_required_exactly_when_the_value_exceeds_sixteen_bytes() {
        let ser = GenericFrameDescriptorSerializer;
        let mut ev = ExtensionValues::default();

        // A key frame's 8-byte descriptor fits the one-byte form.
        ser.parse_value(&key_frame_value(), &mut ev);
        ev.frame_position = at(true, true);
        assert!(!ser.requires_two_byte_form(&ev));

        // The worst case libwebrtc can emit: 4 header bytes plus eight
        // extended dependency diffs of two bytes each.
        let mut long = vec![FLAG_BEGIN_OF_SUBFRAME | 0x08, 0x01, 0x2a, 0x00];
        long.extend(std::iter::repeat_n(0x00, 16));
        assert_eq!(long.len(), 20);
        let mut ev_long = ExtensionValues::default();
        ser.parse_value(&long, &mut ev_long);
        ev_long.frame_position = at(true, true);
        assert!(ser.requires_two_byte_form(&ev_long));

        // A continuation packet of that same frame is one byte, so it does
        // not drag the packet into the two-byte form.
        ev_long.frame_position = at(false, true);
        assert!(!ser.requires_two_byte_form(&ev_long));
    }

    #[test]
    fn the_registered_extension_matches_the_uri_a_peer_offers() {
        // `Extension`'s PartialEq compares an UnknownUri by URI alone, and
        // ignores the serializer. That is what lets str0m match the
        // placeholder it builds while parsing a peer's SDP against our
        // registered entry, remap our id onto the one the peer chose, and
        // echo the extension in the answer.
        let ours = generic_frame_descriptor_extension();
        let placeholder_shape = Extension::UnknownUri(
            GENERIC_FRAME_DESCRIPTOR_URI.to_owned(),
            Arc::new(GenericFrameDescriptorSerializer),
        );
        assert_eq!(ours, placeholder_shape);
        assert_eq!(ours.as_uri(), GENERIC_FRAME_DESCRIPTOR_URI);

        let other = Extension::UnknownUri(
            "urn:example:not-the-frame-descriptor".to_owned(),
            Arc::new(GenericFrameDescriptorSerializer),
        );
        assert_ne!(ours, other);
    }

    #[test]
    fn the_chosen_id_does_not_collide_with_the_standard_map() {
        use str0m::rtp::ExtensionMap;
        let standard = ExtensionMap::standard();
        assert!(
            standard.lookup(GENERIC_FRAME_DESCRIPTOR_ID).is_none(),
            "id {GENERIC_FRAME_DESCRIPTOR_ID} is taken by ExtensionMap::standard()"
        );
    }

    #[test]
    fn the_chosen_id_is_the_one_libwebrtc_uses_for_this_extension() {
        // Pinned deliberately, because the id is only ever renegotiated for a
        // peer that offered a video m-line itself. A peer that joined
        // audio-only is offered a video track later at whatever this says, so
        // a value that merely avoids a collision inside str0m's own map is
        // not good enough — it has to be the one the peer's stack expects.
        //
        // `media/engine/webrtc_video_engine.cc:812-852` at `branch-heads/6422`
        // numbers the video extensions sequentially and then stops
        // incrementing, putting the frame descriptors on 12.
        const LIBWEBRTC_VIDEO_EXTENSION_IDS: &[(u8, &str)] = &[
            (1, "toffset"),
            (2, "abs-send-time"),
            (3, "video-rotation"),
            (4, "transport-cc"),
            (5, "playout-delay"),
            (6, "video-content-type"),
            (7, "video-timing"),
            (8, "color-space"),
            (9, "mid"),
            (10, "rid"),
            (11, "repaired-rid"),
            (12, "generic-frame-descriptor / dependency-descriptor"),
        ];
        let expected = LIBWEBRTC_VIDEO_EXTENSION_IDS
            .iter()
            .find(|(_, name)| name.starts_with("generic-frame-descriptor"))
            .map(|(id, _)| *id)
            .expect("the table names the frame descriptor");
        assert_eq!(
            GENERIC_FRAME_DESCRIPTOR_ID, expected,
            "the SFU must offer this extension on the id libwebrtc assigns it"
        );
        assert_ne!(
            GENERIC_FRAME_DESCRIPTOR_ID, 5,
            "5 is playout-delay in libwebrtc's table — the value this fix moved away from"
        );
    }
}
