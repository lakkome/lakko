<!-- SPDX-License-Identifier: AGPL-3.0-or-later -->
<!-- SPDX-FileCopyrightText: 2026 Lakko Contributors -->

# Pinned dependencies

This file records the WebRTC-stack crate Lakko's SFU
depends on, plus the relevant transitive versions whose
maturity directly affects our deployment. It replaces the
older "LiveKit upstream pin" file that lived here before
[ADR-0011](../../docs/adr/0011-rust-sfu.md) — there is no
LiveKit fork anymore, so there is no upstream tag to
shadow.

## Current state

**Stack decided: `str0m`.** Rationale lives in
[`SPIKE.md`](./SPIKE.md); the headline + reversal criteria
are appended to [ADR-0011](../../docs/adr/0011-rust-sfu.md)
§"Spike outcome".

The first concrete version pin lands in
**`S-3-B-routing`** — that is the sprint that imports
`str0m` into `services/sfu/Cargo.toml` and starts using
it. The skeleton sprint (`S-3-B-skeleton`) deliberately
does **not** pull the dependency, so the workspace stays
fast to build until media routing is the work-in-progress.

| Field | Value |
|---|---|
| WebRTC stack | **`str0m`** |
| Version pin | **`=0.19.0`** (exact, no range) — recorded in `services/sfu/Cargo.toml` |
| Features | `default-features = false, features = ["rust-crypto"]` — pure-Rust crypto provider, no `openssl` / `aws-lc-rs` / `apple-crypto` / `wincrypto` pulled |
| Pin date (UTC) | 2026-05-28 |
| Pinned by | `S-3-B-routing-media` step 1 |
| Lakko sprint | `S-3-B-spike` decided; `S-3-B-routing-media` step 1 pinned the version + shipped `services/sfu/src/media.rs` foundation; steps 2–5 add SDP signaling wiring, UDP socket loop, RTP fanout, and integration tests |

## The vendored fork

The SFU does not build against the registry copy of `str0m` 0.19.0 but
against `crates/vendor/str0m`, through `[patch.crates-io]` in the root
`Cargo.toml`. Every change to it is described in the module documentation of
[`tests/fork_delta_guard.rs`](./tests/fork_delta_guard.rs), which also pins
the SHA-256 of every vendored file the fork touches, and in the comment above
the patch line in the root `Cargo.toml`. Changes one to eight are listed
there only. The three added for
[ADR-0070](../../docs/adr/0070-call-media-through-a-loss-burst.md) steps 3
and 4 (2026-10-05):

| # | Change | Files | Why |
|---|---|---|---|
| 9 | A receive stream can be given its own NACK retry interval (`StreamRx::set_nack_retry_interval`); the session then builds NACKs at least that often. Default unchanged: every stream every 60 ms | `streams/register_nack.rs`, `streams/register.rs`, `streams/receive.rs`, `streams/mod.rs`, `session.rs` | The SFU asks a publisher again for a missing packet of its light video every 20 ms |
| 10 | `StreamTx::resend(seq_no, now)` queues a retransmission of a cached packet without a NACK | `streams/send.rs` | The SFU sends each packet of a light companion track to a subscriber twice |
| 11 | `StreamTx::nacked_packets`, `receiver_reports`, `last_fraction_lost`: read-only counters of what the receiver of a send stream asked for and reported | `streams/send.rs` | The SFU sees a burst of loss on a subscriber's downlink and raises its playout delay |

The playout-delay header extension needed no change: upstream parses, writes
and negotiates it once it is in a session's extension map.

The two added for ADR-0070 step 6 (2026-10-05; see `README.md`,
"Downlink control"):

| # | Change | Files | Why |
|---|---|---|---|
| 12 | `DirectApi::set_twcc_on_local_media_only(on)`: with it on, a media line the remote side offered is answered without the transport-wide sequence number extension and without `transport-cc` feedback, and a media line this side offers carries both. Off by default: every line as upstream writes it | `session.rs`, `change/direct.rs`, `change/sdp.rs` | The SFU needs transport-wide feedback for what it sends a subscriber, and must not give a publisher any for what it publishes (ADR-0068). Upstream negotiates both per session, on every line alike |
| 13 | `DirectApi::keep_twcc_feedback(keep)` and `DirectApi::take_twcc_feedback()`: the records of the receiver's transport-wide feedback, per packet sent the send time, the size and the reported arrival time (or none). Off by default; at most 8192 records held, the oldest dropped; str0m's own estimator is fed as before | `session.rs`, `change/direct.rs` | The SFU runs its own estimate of each subscriber's downlink. str0m's send-side estimator, measured through the SFU, took a second of loss on a 7.5 Mbit/s path to 178 kbit/s, cannot rise above 1.5 times what is being sent while tracks are paused, and lowered the estimate of an unlimited path on its first probe. (A first thirteenth change, `Bwe::current_estimate`, was taken out again with that estimator) |
| 14 | A retransmission (RTX packet) is sent without RTP padding; a first transmission is padded to the SRTP block size as upstream does it | `streams/send.rs` | A libwebrtc receiver restores the original packet behind a copy of the RTX packet's header, padding bit included, and without the padding, so the restored packet lost as many bytes from its tail as its last byte says. Live call, 2026-10-07: a Chrome viewer restored 848 of 12 957 second copies of light frames intact (the one in sixteen whose RTX packet needed no padding); 12 109 were the frame less its last byte. The padded packet was correct by RFC 4588 and RFC 3550 |

The one added for the key frame at the end of an unrepaired burst
(2026-10-09; see `README.md`, "A key frame at the end of an unrepaired
burst"):

| # | Change | Files | Why |
|---|---|---|---|
| 15 | `StreamRx::give_up_missing()`: the packets of a receive stream that are missing right now are not asked for again. They stay missing, one that still arrives is delivered, and a packet that goes missing later is asked for as usual | `streams/register_nack.rs`, `streams/register.rs`, `streams/receive.rs` | When a burst of loss on a publisher's stream has ended unrepaired the SFU asks the publisher for a key frame. Upstream would go on asking for the packets of the burst until their request count ran out (up to 1.5 s from the first request), and each answer is a retransmission the publisher sends ahead of the key frame on a path that has just recovered |

The one added after the SFU aborted on a live call (2026-10-16):

| # | Change | Files | Why |
|---|---|---|---|
| 16 | `SrtpContext::protect_rtp` under `AES_CM_128_HMAC_SHA1_80` no longer asserts that the RTP body is a multiple of 16 bytes | `rtp/srtp.rs` | Change 14 sends retransmissions unpadded; the leftover upstream assert then fired on the first retransmission to a peer that negotiated AES-CM, and under `panic = "abort"` the whole SFU process died (test.lakko.me, 2026-10-16). Counter mode needs no alignment; the AEAD-GCM profiles never asserted |

The one added after an iOS client on webrtc-sdk 125.6422.07 refused the SFU's
subscribe offer (2026-11-09):

| # | Change | Files | Why |
|---|---|---|---|
| 17 | When str0m picks a payload type itself it uses 96-127, then 35-63, and never 64-95 (upstream tried 80-95 and 77-78 before 35-71) | `format/payload_params.rs` | With rtcp-mux RFC 5761 section 4 rules out 64-95, and libwebrtc refuses a whole remote description that names one: "invalid payload type 80 for codec 'rtx'". The iOS client then received no sound or video in a group call |

## Why we pin exactly

`Cargo.toml` lists dependencies with **exact** versions
(e.g. `webrtc = "=0.X.Y"`, not `"0.X"`) for the WebRTC
stack and any directly-depended crypto crate. Rationale:

1. **Reproducible builds.** The Nix flake locks the Rust
   toolchain; the `Cargo.lock` locks every transitive
   dep; the exact `Cargo.toml` pin makes the **policy**
   intent explicit so a `cargo update --aggressive`
   doesn't silently bump us across a major boundary.
2. **Audit-friendly.** Auditors look at this file to know
   which crates' code they need to inspect — a range
   would force them to inspect everything in the
   compatibility window.
3. **Security follow.** When `cargo audit` flags the
   stack, we explicitly bump the pin, regenerate the
   lock, and append a changelog entry below.

Indirect dependencies follow the workspace-wide policy —
locked by `Cargo.lock`, reviewed at PR time, tracked by
`cargo audit` and `cargo deny`.

## Decision history

### `str0m` ([github.com/algesten/str0m](https://github.com/algesten/str0m)) — chosen

- Pure Rust, **sync** (caller drives event loop).
- Narrower surface (~6 k LOC) — focused on the SFU
  forwarding use case.
- Strong audit story: no hidden tasks, no implicit
  scheduling — the auditor reads one event loop, not
  N tokio-spawned futures.
- Production users: Eyevinn streaming, sentry.io,
  Embark internals.
- `#![forbid(unsafe_code)]`-friendly.

### `webrtc-rs` ([github.com/webrtc-rs/webrtc](https://github.com/webrtc-rs/webrtc)) — rejected

- Async / tokio-based.
- Broader feature coverage (DataChannel, SCTP, multiple
  codecs). Lakko uses none of the extras — chat-during-
  call rides MLS (S-3-A-chat); signaling rides our own
  WS protocol.
- Production users: Cloudflare Calls, parts of Discord.
- API mirror of `pion/webrtc` (the stack LiveKit uses) —
  patterns transfer. **But** ~30 k LOC audit surface and
  hidden tokio tasks inflate the audit budget.

Full comparison + reversal criteria: see
[`SPIKE.md`](./SPIKE.md).

## Changelog

Append-only log of every pin change. Most recent at top.

```text
### YYYY-MM-DD — initial pin at <stack> <version>

- Sprint: S-3-B-skeleton
- Rationale: decision recorded in ADR-0011 §"Spike outcome"
- cargo audit: clean / N findings (links to known-gaps.md if any)
- cargo deny: clean
```

(empty — first pin not yet landed)

## How to read this file as an auditor

If you're reviewing the SFU:

1. Open this file — read the current pin row.
2. Cross-reference against `services/sfu/Cargo.toml` —
   the exact pin should match what's listed here.
3. Cross-reference against `Cargo.lock` — the resolved
   version should equal the pin (no `~` / `^` slack).
4. `cargo audit` output for the workspace is reproducible
   from `Cargo.lock` alone; the SFU's vulnerability
   posture is the workspace's posture.

If steps 2–3 agree, the SFU's stack posture is what this
file claims it is.

## Bumping the pin

```bash
# 1. Update services/sfu/Cargo.toml to the new exact version.
# 2. Update Cargo.lock.
cargo update -p webrtc        # or -p str0m
# 3. Run audit/lint gates.
just audit
just lint
just test
# 4. Append a changelog entry above.
# 5. Single dedicated commit: "chore(sfu): bump webrtc-rs to 0.X.Y"
```

Major bumps (1.x → 2.x) get their own PR with the
breaking-change diff explained in the commit message.
Minor / patch bumps are routine.
