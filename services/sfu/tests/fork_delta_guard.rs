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
//! ## What this guard is for
//!
//! `crates/vendor/str0m` is ~40k lines of third-party code that nobody on
//! this project reviews. This test freezes the six files the fork touches,
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
        "d20e7a2eecda7327aab3cf78cb125f6c5e786323ad9cdf53cf71909e64cd4aca",
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
         per-packet `frame_position` capability and the NACK retry schedule \
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
