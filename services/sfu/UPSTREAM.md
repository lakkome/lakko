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
| Version pin | *pending* (set as exact version in `Cargo.toml` at `S-3-B-routing` kickoff, not a range) |
| Pin date (UTC) | *pending* |
| Pinned by | *pending* |
| Lakko sprint | `S-3-B-spike` decided; `S-3-B-routing` pins |

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
