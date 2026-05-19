<!-- SPDX-License-Identifier: AGPL-3.0-or-later -->
<!-- SPDX-FileCopyrightText: 2026 Lakko Contributors -->

# `lakko-additions/`

Lakko-only Go code that augments — but does not modify —
the pinned LiveKit upstream (see [`../UPSTREAM.md`](../UPSTREAM.md)).

The directory lives outside `upstream/` so the quarterly
upstream rebase leaves it untouched. Files here are owned
by Lakko, written from scratch, and reviewed against
Lakko's own crypto / security invariants (see
[`docs/security/calls.md`](../../../docs/security/calls.md)).

## Layout

```text
lakko-additions/
├── README.md              ← this file
├── go.mod                 ← (added in 3b1c — separate Go module)
├── go.sum
├── auth/                  ← (3b1c) JWT verifier middleware
│   ├── jwt_verifier.go
│   └── jwt_verifier_test.go
├── rpc/                   ← (3b1d) Lakko-fronted room-create RPC
│   ├── room_create.go
│   └── room_create_test.go
└── metrics/               ← (3b5) Lakko-specific Prometheus labels
    └── metrics.go
```

Nothing exists yet beyond this README; 3b1c is the first
sprint that lands actual Go code here.

## Why a separate Go module

The `go.mod` here is independent from the upstream's
`services/sfu/upstream/livekit/go.mod`. The boundary:

- **Upstream module** owns the SFU's main entry point, its
  own deps (pion/webrtc, protobuf types, etc.).
- **Additions module** depends on the upstream module as a
  replace-directive local dep. Lakko-side tests import
  upstream types but stay isolated from upstream's test
  surface.

This shape lets us:

1. Test Lakko-side logic in isolation (`go test ./...` inside
   `lakko-additions/`) without dragging upstream's heavy
   integration tests.
2. Pin Lakko-side deps (e.g. our chosen JWT library) without
   affecting upstream's dep graph.
3. Survive an upstream `go.mod` reshuffle on rebase without
   re-rolling our own deps.

The `replace` directive lives in `lakko-additions/go.mod`
and points to `../upstream/livekit/`. CI verifies the local
replace path resolves to the pinned vendor state.

## What lives here vs in a patch

| Pattern | Where |
|---|---|
| Modifying an upstream function | patch in `lakko-patches/` |
| Deleting an upstream code path | patch in `lakko-patches/` |
| Adding a NEW Lakko-specific package | here |
| Adding a NEW Go file that doesn't touch upstream | here |
| Adding a `// LAKKO:` marker comment to upstream | patch |

If the change can be expressed as a NEW file that doesn't
touch existing upstream files, prefer this directory — it
survives upstream rebases at zero cost. Patches incur a
real ops cost (every quarter, each patch is re-applied; a
patch that fails to apply is manual work).

## CI

`just sfu-verify-clean` (added in 3b1b) builds the upstream
SFU **then** builds + tests `lakko-additions/`:

```bash
# Upstream side — applies patches, builds, tests.
(cd services/sfu/upstream/livekit && go build ./... && go test ./...)

# Lakko additions side.
(cd services/sfu/lakko-additions && go build ./... && go test ./...)
```

Both must pass for the SFU CI gate to be green.

## Threat-model anchor

Per [`docs/security/threat-model.md`](../../../docs/security/threat-model.md)
the SFU is **assumed-compromised**. Code under this
directory operates inside that adversary model — never
trust a value coming in from the SFU host (e.g. validate
the JWT signature server-side before honouring its claims).

The JWT-verifier work in 3b1c is the load-bearing example:
the verifier uses **the auth service's published public
key** (fetched at SFU process startup, refreshed every 24 h)
as the trust anchor — NOT a key embedded in the SFU binary,
which a compromised SFU operator could swap.
