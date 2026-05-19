<!-- SPDX-License-Identifier: AGPL-3.0-or-later -->
<!-- SPDX-FileCopyrightText: 2026 Lakko Contributors -->

# `services/sfu` — Lakko's LiveKit fork

Selective fork of [`livekit/livekit`](https://github.com/livekit/livekit)
(SFU media-routing server) + [`livekit/protocol`](https://github.com/livekit/protocol)
(Go protobuf + Go server types), specialised for Lakko's
threat model + auth flow.

The strategy is captured in [ADR-0008](../../docs/adr/0008-livekit-fork-strategy.md);
the call-side cryptography invariants this fork must preserve
live in [`docs/security/calls.md`](../../docs/security/calls.md).

This README is the operational entry point. Read [ADR-0008](../../docs/adr/0008-livekit-fork-strategy.md)
first for the "why"; then [`PLAYBOOK.md`](./PLAYBOOK.md) for
the "how".

## Status

**3b1a — Fork tree set up.** The directory is scaffolded
but the upstream Go code is NOT yet vendored in. The maintainer
will perform the initial vendor pull as a separate operational
step (see [`PLAYBOOK.md`](./PLAYBOOK.md) §1) — that's a single
large commit landing ~150k lines of Go in `upstream/`. The
scaffolding here is everything you need to know AROUND that
commit: where the pin lives, how to apply patches, how to
rebase.

Subsequent sprints land the actual modifications:

| Sprint | Deliverable |
|---|---|
| 3b1a | ✅ This scaffolding |
| 3b1b | Strip LiveKit `participant_key` flow + plaintext-media path |
| 3b1c | Wire Lakko JWT validation against `services/auth`'s public key |
| 3b1d | Replace LiveKit room-create RPC with our Lakko-fronted RPC |
| 3b2  | Deploy on dedicated Hetzner CCX VPS |
| 3b3  | TURN-over-TLS on port 443 via coturn alongside |
| 3b4  | `services/auth/call_jwt` endpoint |
| 3b5  | Prometheus / Grafana monitoring |
| 3b6  | Cost analysis + scale projection |

## Layout

```text
services/sfu/
├── README.md              ← this file
├── UPSTREAM.md            ← pin metadata: upstream commit, date, drift notes
├── PLAYBOOK.md            ← operational commands (vendor / patch / rebase)
├── upstream/              ← VENDORED LiveKit code. NOT YET POPULATED.
│   ├── livekit/           ← github.com/livekit/livekit @ pinned commit
│   └── protocol/          ← github.com/livekit/protocol @ pinned commit
├── lakko-patches/         ← git format-patch files applied on top of upstream
│   ├── README.md          ← how to add / order / re-roll patches
│   └── *.patch            ← one patch per logical change (3b1b, 3b1c, ...)
└── lakko-additions/       ← Lakko-only Go code that augments (not modifies)
                              upstream. Lives outside `upstream/` so the
                              vendor refresh leaves it untouched.
    └── README.md
```

**Important**: never edit files under `upstream/` directly.
Every change is either:

1. A patch in `lakko-patches/` (preferred — survives upstream
   rebase by re-applying).
2. A new file in `lakko-additions/` (preferred for code
   that doesn't need to live next to upstream Go).

Direct edits to `upstream/` get clobbered on the next
quarterly rebase. The `LAKKO:` marker grep that CI runs
treats any file under `upstream/` with no `LAKKO:` comment
as a hard error — forces every modification to be a patch.

## Why this shape

[ADR-0008](../../docs/adr/0008-livekit-fork-strategy.md) §"Upstream-tracking policy"
captures the full reasoning. Summary:

- **A pin commit** (`UPSTREAM.md`) makes the upstream
  position auditable.
- **Patch files** (`lakko-patches/`) survive the quarterly
  upstream rebase — applying them re-creates our fork from
  upstream + patches.
- **Separate additions** (`lakko-additions/`) lets Lakko-only
  code escape the vendor-refresh blast radius.
- **No silent vendoring** — every diff has a `LAKKO:` marker
  + a patch file describing the change.

The Signal-Server fork practitioners ([signalapp/Signal-Server](https://github.com/signalapp/Signal-Server))
use the same pattern; it scales for the quarterly
maintenance cadence we expect.

## Quick reference

```bash
# Read the pin
cat services/sfu/UPSTREAM.md

# Apply Lakko's patches on top of vendored upstream
(cd services/sfu && for p in lakko-patches/*.patch; do
   git apply --directory=upstream/livekit "$p"
done)

# Refresh upstream (quarterly)
# See PLAYBOOK.md §3 for the full sequence.
```

The full rebase / patch / vendor playbook lives in
[`PLAYBOOK.md`](./PLAYBOOK.md).

## What's NOT here

Per ADR-0008 §"What we fork", these are deliberately absent:

- `livekit-client` (browser SDK) — clients use native
  `RTCPeerConnection` directly. The LiveKit signaling
  protocol is small enough to speak from our own code.
- `livekit-server-sdk-go` (server SDK) — we issue our own
  JWTs from `services/auth`; the SFU validates the signature
  with the auth service's published public key.
- `egress` / `ingress` — no transcoding, no recording (per
  ROADMAP 3c8). If recording ever lands, it's a regular
  participant joining the call, not an egress hook.
- `livekit-cli` / helm charts / k8s manifests — single-VPS
  systemd deployment (per `infra/README.md`).

## Threat-model note

The SFU is **assumed-compromised** per
[`docs/security/threat-model.md`](../../docs/security/threat-model.md).
The deletions in 3b1b (the `participant_key` flow) reduce
the SFU's surface to:

- Validate the JWT signature.
- Forward encrypted SFrame frames.
- Emit Prometheus metrics (counts + bandwidth, never
  plaintext).

It cannot see media keys, plaintext SDP / ICE bodies, or
chat-group membership. See [`docs/security/calls.md`](../../docs/security/calls.md)
§"What the SFU sees vs what the client owns" for the
full table.
