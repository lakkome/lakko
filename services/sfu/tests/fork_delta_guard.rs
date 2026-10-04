// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! Fork-delta guard for the vendored `str0m`.
//!
//! Lakko vendors `str0m` 0.19.0 under `crates/vendor/str0m` and consumes it
//! via `[patch.crates-io]`, the same shape as the `webauthn-rs-core` fork
//! guarded by `services/auth/tests/fork_delta_guard.rs`.
//!
//! ## What the fork contract is
//!
//! One capability, added because it did not exist: **tell an
//! `ExtensionSerializer` which packet of the frame it is writing.**
//!
//! The SFU has to relay the `generic-frame-descriptor-00` RTP header
//! extension — without it a libwebrtc receiver cannot recognise a key frame
//! in an SFrame-sealed stream, because the flag it reads lives in the
//! bitstream bytes the encryptor replaced. That extension's value is
//! *per packet*: its `B` and `E` bits mark the first and last packet of a
//! frame, so the frame-level value cannot simply be copied onto every packet
//! of a re-packetized frame. Upstream's `write_to(&self, buf, ev)` is handed
//! only the frame-level `ExtensionValues` and has no way to know its
//! position, which makes a correct relay inexpressible.
//!
//! The delta across six files, all additive except three lines replaced in
//! place:
//!
//! - `src/rtp/ext.rs` — new `FramePosition` type and
//!   `ExtensionValues::frame_position` field; new
//!   `UserExtensionValues::fill_missing_from`.
//! - `src/packet/payload.rs` — the packetizer sets `frame_position`, in the
//!   loop that already strips other per-packet fields.
//! - `src/packet/buffer_rx.rs` — new `Depacketized::ext_vals_for_frame`,
//!   which fills user extension values missing from the last packet in from
//!   the first. Upstream takes the last packet's values only, because some
//!   extensions are last-packet-only; a frame descriptor is the opposite,
//!   so neither packet alone carries the full set.
//! - `src/media/mod.rs` — `MediaData` is built from `ext_vals_for_frame()`.
//! - `src/rtp/mod.rs`, `src/lib.rs` — re-export `FramePosition`.
//!
//! No existing behaviour changes: every serializer that ignores the new
//! field behaves exactly as before, and `fill_missing_from` only ever adds a
//! value for a type the last packet did not already carry.
//!
//! ## Second change: how long a missing packet is asked for (2026-10-03)
//!
//! [ADR-0067](../../../docs/adr/0067-loss-resilient-call-media.md). Upstream
//! asks a publisher for a missing packet at most five times, once per 33 ms
//! NACK tick, and tracks a hole only within 100 packets of the newest. On a
//! live call the uplink lost 20-40% of packets for about a second at a time,
//! so all five requests fell inside the burst and the packet never reached
//! the SFU; every subscriber then waited 2-3 s for a keyframe. The fork asks
//! up to ten times on a schedule that doubles from 33 ms to a 200 ms cap
//! (about 1.4 s in all) and tracks holes within 512 packets. This one IS a
//! behaviour change, confined to the receive-side NACK register:
//!
//! - `src/streams/register_nack.rs` — `MAX_MISORDER` 100 -> 512,
//!   `MAX_NACKS` 5 -> 10, `retry_interval`, a `last_nack` time per packet,
//!   `nack_reports` takes the time; three tests added, upstream tests pass a
//!   time.
//! - `src/streams/register.rs`, `src/streams/receive.rs`,
//!   `src/streams/mod.rs`, `src/_internal_test_exports/fuzz.rs` — pass the
//!   time through.
//!
//! ## Fifth change: the loss fraction in a receiver report (2026-10-04)
//!
//! Upstream computes the RFC 3550 interval loss and casts it to a byte
//! without the appendix A.3 clamp. When packets lost in one interval are
//! repaired in the next, the interval receives more than it expected, the
//! value is a small negative number, and the byte wraps to about 97% loss.
//! Seen on a live call: reports of 248/255 and 241/255 for a publisher whose
//! video arrived at a full 30 fps, each followed by the sending browser
//! cutting its bitrate estimate to a tenth or less and climbing back over
//! about 25 s. The fork reports zero for a negative interval and saturates
//! at 255:
//!
//! - `src/streams/register.rs` - `fraction_lost` clamps; two tests added.
//!
//! ## Third change: redundant audio, RED (2026-10-04)
//!
//! [ADR-0067](../../../docs/adr/0067-loss-resilient-call-media.md) step 3.
//! Chrome and libwebrtc can wrap Opus in RED (RFC 2198), so an isolated lost
//! audio packet is repaired from the next one. Upstream has no `red` codec:
//! it parses `a=rtpmap:63 red/48000/2` as unknown and drops it from every
//! answer, so nobody sends RED. The fork lets an audio `red` payload type be
//! negotiated and carried by payload type; it never looks inside one (the
//! container is built before SFrame seals the frame, so it is ciphertext).
//! It is opt-in (`CodecConfig::enable_red`), not part of the default codec
//! set, so an endpoint that does not ask for it behaves as upstream.
//!
//! - `src/format/codec.rs` — `Codec::Red`, an audio codec named `red`.
//! - `src/format/format_params.rs` — `RedParams` (the fmtp list, `111/111`:
//!   2 to 32 entries of one payload type; any other list does not parse) and
//!   `FormatParams::red`.
//! - `src/format/mod.rs` — re-export `RedParams`.
//! - `src/format/codec_config.rs` — `CodecConfig::enable_red`: payload type
//!   63, 48 kHz, 2 channels, placed directly before Opus because the order
//!   of the list is the order of the `m=audio` format list and libwebrtc
//!   sends RED only when `red` precedes `opus`; no RTX, no NACK. Nine tests
//!   for the fmtp, the ordering and the answer rules.
//! - `src/format/payload_params.rs` — two `red` entries match when both
//!   carry a list; until locked the local entry takes the remote's list, so
//!   an answer echoes the offer's fmtp.
//! - `src/sdp/data.rs` — `FormatParam::Red`; `MediaLine::rtp_params` keeps a
//!   remote `red` entry only when its fmtp names an Opus payload type of the
//!   same m-line (which also drops video `red/90000`).
//! - `src/sdp/parser.rs` — a bare fmtp value that is a `red` list is kept
//!   instead of discarded.
//! - `src/packet/mod.rs` — `Codec::Red` maps to the pass-through packetizer
//!   and depacketizer (two exhaustive matches needed an arm; RTP mode uses
//!   neither).
//!
//! Receiving and `write_rtp` needed no change: both look a payload type up
//! in the session's codec list, where the `red` entry now is.
//!
//! ## Fourth change: video FEC, RED + ULPFEC (2026-10-04)
//!
//! [ADR-0067](../../../docs/adr/0067-loss-resilient-call-media.md) step 4.
//! Chrome and libwebrtc offer video `red/90000` (with an RTX) and
//! `ulpfec/90000`, and once both are negotiated send every video packet
//! inside RED, FEC packets included. Upstream knows neither, so no answer
//! keeps them. The fork lets the pair be negotiated and carried by payload
//! type; it never looks inside a RED or FEC payload (the SFU does, in
//! `services/sfu/src/video_fec.rs`, and only at the cleartext header bytes).
//! Opt-in (`CodecConfig::enable_video_fec`), not part of the default codec
//! set, so an endpoint that does not ask for it behaves as upstream. No file
//! is touched that the third change had not touched already:
//!
//! - `src/format/codec.rs` — `Codec::RedVideo` (SDP name `red`, told from
//!   audio `Codec::Red` by the 90 kHz clock) and `Codec::Ulpfec`, both
//!   video.
//! - `src/sdp/parser.rs` — an `a=rtpmap` named `red` with clock rate 90000
//!   parses as `Codec::RedVideo`.
//! - `src/sdp/data.rs` — `MediaLine::rtp_params` keeps video `red` and
//!   `ulpfec` only as a pair: a media line with one and not the other is
//!   read as having neither, so neither is matched or answered. The red RTX
//!   mapping (`apt`) comes from the existing generic code.
//! - `src/format/codec_config.rs` — `CodecConfig::enable_video_fec`: `red`
//!   116 with RTX 117 and `ulpfec` 118 (libwebrtc's numbers), 90 kHz, no
//!   RTCP feedback lines, appended after the video codecs. Seven tests for
//!   the configuration and the answer rules.
//! - `src/format/payload_params.rs` — two `RedVideo` or two `Ulpfec` entries
//!   match on codec and clock rate alone.
//! - `src/packet/mod.rs` — both map to the pass-through packetizer and
//!   depacketizer (exhaustive matches; RTP mode uses neither).
//!
//! Receiving, `write_rtp`, NACK and RTX needed no change. A packet under the
//! `red` payload type is looked up in the session's codec list like any
//! other and delivered as a packet of the video stream; because the `red`
//! entry has an RTX payload type, a retransmitted RED packet is rewritten
//! back onto the main stream under the `red` payload type before the SFU
//! sees it, and a RED packet the SFU sends is kept for a subscriber's NACK.
//!
//! ## What this guard is for
//!
//! `crates/vendor/str0m` is ~40k lines of third-party code that nobody on
//! this project reviews. This test freezes the files the fork touches,
//! so an edit to any of them — an accidental one above all — fails CI and
//! forces a conscious look. It says nothing about the other files; those are
//! upstream-verbatim by virtue of never having been opened.
//!
//! When the failure is intentional (an upstream rebase, or a reviewed change
//! to the fork):
//!   1. `diff -ru` the vendored `src` against the pristine registry copy of
//!      the same version and confirm the delta is still only the capability
//!      described above;
//!   2. update the hash this test prints.
//!
//! The second change was confirmed on 2026-10-03 the same way: the delta is
//! now eleven files, the six above plus the five NACK files.
//!
//! The third change was confirmed on 2026-10-04 the same way (`diff -rq`
//! against the registry copy): nineteen files, the eleven above plus the
//! eight RED files.
//!
//! The fourth change (2026-10-04) touches six of those nineteen files and no
//! others, by `git diff --stat -- crates/vendor/str0m` over the working tree
//! that carried the third change uncommitted. It was NOT re-confirmed against
//! the registry copy of pristine 0.19.0; do that before relying on the count.
//!
//! First confirmed on 2026-09-18 against the registry-cached pristine
//! `str0m` 0.19.0: six files, +63 / -3 lines, and all three removals are
//! lines replaced in place (two re-export lists gaining `FramePosition`, and
//! the `MediaData` construction switching to `ext_vals_for_frame()`).

use sha2::{Digest, Sha256};

/// The vendored files the fork touches, relative to `crates/vendor/str0m`,
/// each with the SHA-256 of its LF-normalized contents at the reviewed fork
/// state.
const PINNED: &[(&str, &str)] = &[
    (
        "src/rtp/ext.rs",
        "a843d9b1c70b44236fd9da9804d3943f5be4fc08041ae0b1b1f52ed8580a550a",
    ),
    (
        "src/rtp/mod.rs",
        "6373fdd79ccf8c3d866241b40f50c2eba809ec0b312e250e5179ff150670f46f",
    ),
    (
        "src/packet/payload.rs",
        "e028e08b64b016d96330c4bccf9f6538bf1c09f0c5e60d427acca48f75c62d9e",
    ),
    (
        "src/packet/buffer_rx.rs",
        "52e2162a35c7b9e68f44ccf194ca41d13dbc08aa874e73a1a26d9fdc7dd8bfaa",
    ),
    (
        "src/media/mod.rs",
        "1d280e4a578ab2fae4a350290618a5d8cb9381b0e32e32ccab35afeec9ec36cc",
    ),
    (
        "src/lib.rs",
        "f365c38ac92b64a86ec78a154d195fefd7166320203026538dc4eb3c463d73e4",
    ),
    (
        "src/streams/register_nack.rs",
        "5799b3e099329c560e19688ad1b351e29d9d135dcedddb5f922bda14667960a3",
    ),
    (
        "src/streams/register.rs",
        "c4e149ef9032dd1f8cc5198f7f52a2ad0272ff2886487964ab09eb61bed70755",
    ),
    (
        "src/streams/receive.rs",
        "aaa5d8189f866c87c79f4d751c3c332ca2607056f8fcab43d2294d4a91186fa0",
    ),
    (
        "src/streams/mod.rs",
        "039dda876bba5fdf27b404520c70cc8c6592bb0d15df9b24a37537e9db867629",
    ),
    (
        "src/_internal_test_exports/fuzz.rs",
        "bae5c3b1d66c072128aae974e3b2f859b70cc1cd3beeb001c75cd143733cb577",
    ),
    (
        "src/format/codec.rs",
        "2fbebe989cc36a0c2a895757ad92972749c76790081ef88205409c06542b4adc",
    ),
    (
        "src/format/codec_config.rs",
        "d4c49c71cfab81fa35ca8acf748b7015c637306addfd0ead5ed57dfa2b0f1b6f",
    ),
    (
        "src/format/format_params.rs",
        "2e135c425ce0cf23409369a8e43bbb1a412a50164daf72a31ebf5e71e51a7354",
    ),
    (
        "src/format/payload_params.rs",
        "a9833b7a9065a6714c633930090a9f187604d5e95dba4e04abf215dc4cb5b012",
    ),
    (
        "src/format/mod.rs",
        "92b575ec6c27fb667e0c118823dcc89137ada8affd3582695b82c2b7a5d19308",
    ),
    (
        "src/packet/mod.rs",
        "bc6d39baf66d2baf19032c25ed5b03b7c5d50b6c91060c346aac518d6d8cc842",
    ),
    (
        "src/sdp/data.rs",
        "2e7a85008e7abbb73adb8595bbc579b14bed4adfb96dcdb08dcd43ecd6e7d318",
    ),
    (
        "src/sdp/parser.rs",
        "2c95e93b07589857b7b88e25502d9cda92984c4cd69a4fc49ffd152f0c77568e",
    ),
];

/// Root of the vendored fork, relative to this crate's manifest dir.
const VENDOR_ROOT: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/../../crates/vendor/str0m");

/// Read a vendored file and normalize line endings to LF so the hash does not
/// depend on the checkout's autocrlf setting.
fn read_normalized(rel: &str) -> String {
    let path = format!("{VENDOR_ROOT}/{rel}");
    let raw = std::fs::read_to_string(&path)
        .unwrap_or_else(|e| panic!("cannot read vendored str0m file at {path}: {e}"));
    raw.replace("\r\n", "\n").replace('\r', "\n")
}

fn sha256_hex(s: &str) -> String {
    let digest = Sha256::digest(s.as_bytes());
    let mut out = String::with_capacity(64);
    for b in digest {
        use std::fmt::Write as _;
        let _ = write!(out, "{b:02x}");
    }
    out
}

#[test]
fn vendored_str0m_matches_pinned_fork_state() {
    let mut drifted = Vec::new();
    for (rel, pinned) in PINNED {
        let actual = sha256_hex(&read_normalized(rel));
        if actual != *pinned {
            drifted.push(format!(
                "    (\n        {rel:?},\n        {actual:?},\n    ),"
            ));
        }
    }
    assert!(
        drifted.is_empty(),
        "the vendored str0m fork changed.\n\n\
         Confirm the delta against pristine str0m 0.19.0 is still only the \
         per-packet `frame_position` capability, the NACK retry schedule, \
         the audio `red` payload type and the video `red` + `ulpfec` pair \
         (see this file's module doc), then re-pin:\n\n{}\n",
        drifted.join("\n")
    );
}

/// The point of the whole fork, asserted against the library rather than
/// described in prose: a serializer is told where its packet sits in the
/// frame. If an upstream rebase silently dropped the field, the hash guard
/// above would fire — but so would this, with a message that says what was
/// lost rather than that bytes moved.
#[test]
fn the_fork_capability_is_present() {
    use str0m::rtp::{ExtensionValues, FramePosition};

    let mut ev = ExtensionValues::default();
    assert_eq!(
        ev.frame_position, None,
        "a value that did not come from the packetizer has no frame position, \
         and a serializer must be able to tell that from a real position"
    );
    ev.frame_position = Some(FramePosition {
        first: true,
        last: false,
    });
    assert_eq!(
        ev.frame_position,
        Some(FramePosition {
            first: true,
            last: false
        })
    );
}

/// The third fork change, asserted against the library: audio RED can be
/// switched on, and it then sits before Opus and names Opus's payload type.
#[test]
fn the_red_capability_is_present() {
    use str0m::format::{Codec, CodecConfig};

    let mut config = CodecConfig::new_with_defaults();
    assert!(
        config.find(|p| p.spec().codec == Codec::Red).is_none(),
        "red is opt-in, not part of the default codec set"
    );
    config.enable_red(true);
    let audio: Vec<_> = config
        .params()
        .iter()
        .filter(|p| p.spec().codec.is_audio())
        .collect();
    assert_eq!(audio.len(), 2);
    assert_eq!(audio[0].spec().codec, Codec::Red, "red precedes opus");
    assert_eq!(audio[1].spec().codec, Codec::Opus);
    assert_eq!(
        audio[0].spec().format.red.map(|red| red.pt),
        Some(audio[1].pt()),
        "the red fmtp names the Opus payload type"
    );
    assert_eq!(audio[0].resend(), None, "audio has no RTX");
    assert!(!audio[0].fb_nack(), "audio has no NACK");
}

/// The fourth fork change, asserted against the library: video FEC can be
/// switched on, and it then adds `red` with an RTX and `ulpfec` without one,
/// distinct from audio RED.
#[test]
fn the_video_fec_capability_is_present() {
    use str0m::format::{Codec, CodecConfig};

    let mut config = CodecConfig::new_with_defaults();
    for codec in [Codec::RedVideo, Codec::Ulpfec] {
        assert!(
            config.find(|p| p.spec().codec == codec).is_none(),
            "video fec is opt-in, not part of the default codec set"
        );
    }
    config.enable_red(true);
    config.enable_video_fec(true);
    let red = config
        .find(|p| p.spec().codec == Codec::RedVideo)
        .expect("video red");
    let ulpfec = config
        .find(|p| p.spec().codec == Codec::Ulpfec)
        .expect("ulpfec");
    assert!(red.spec().codec.is_video() && ulpfec.spec().codec.is_video());
    assert!(
        red.resend().is_some(),
        "a retransmitted RED packet needs an RTX payload type to come back under"
    );
    assert_eq!(ulpfec.resend(), None, "an FEC packet travels inside RED");
    assert!(!red.fb_nack() && !ulpfec.fb_nack(), "no feedback lines");
    let audio_red = config
        .find(|p| p.spec().codec == Codec::Red)
        .expect("audio red");
    assert_ne!(audio_red.pt(), red.pt());
    assert!(audio_red.spec().codec.is_audio());
}
