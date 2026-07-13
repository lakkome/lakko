<!--
SPDX-License-Identifier: AGPL-3.0-or-later
SPDX-FileCopyrightText: 2026 Lakko Contributors
-->

# ADR-0030: Batched channel-rotation fanout — staged prepare + atomic commit

- **Status:** Accepted (maintainer chose Variant 1; implementation in progress)
- **Date:** 2026-07-04
- **Context:** Phase 4 (channels, G-1) — scale the rotation fanout to >50k subscribers

## Context

A channel seed rotation ([ADR-0010](0010-channels-two-tier-broadcast.md)) must
deliver a fresh HPKE-sealed seed envelope to **every** current subscriber for the
new epoch. Today `POST /v1/channels/:id/rotate` takes the whole set in one shot:
`RotateRequest { envelopes: Vec<RotateBatchEntry> }` — the publisher client seals
all N envelopes and posts them in a single JSON body, and the server commits them
in one `FOR UPDATE` transaction (audit-pack invariant **C-42**: two-pass validate
then bump-epoch-and-replace-all under a single write lock, so a subscriber never
observes a torn rotation — it sees either the old epoch's envelope or the new
one, never a mix).

That single-request shape is fine for a handful of subscribers but unfit at the
channel scale ADR-0010 targets (50k–1M): the request body is multi-MB to GB, the
transaction is one giant write, and there is no chunking, streaming, or
resumability. A dropped connection mid-fanout wastes the whole attempt.

The hard constraint: **C-42 must survive.** Whatever chunking we add, a subscriber
must never see a partially-applied rotation (some subscribers on epoch N+1, others
still on N with no N+1 envelope).

## Decision

**Two-phase fanout: a chunked, resumable STAGE phase followed by a single atomic
COMMIT** (Variant 1, chosen over a bump-then-lazy-catch-up alternative that would
break C-42).

1. **Stage (chunked, resumable, idempotent).** The publisher uploads the new
   epoch's envelopes in bounded chunks to a **staging area** keyed by
   `(channel_id, target_epoch, subscriber_id)`:
   `POST /v1/channels/:id/rotate/stage { target_epoch, envelopes: [...] }`. The
   epoch is **not** bumped and staged envelopes are **never served** to
   subscribers — they still see the current live epoch. Each chunk is bounded
   (a fixed max entries/bytes), so no single request is unbounded. Re-posting a
   chunk is an idempotent upsert, so a client that dropped mid-fanout simply
   re-sends the missing chunks (resumability).

2. **Commit (atomic, single).** Once every chunk is staged, the publisher calls
   `POST /v1/channels/:id/rotate/commit { target_epoch }`. In ONE transaction the
   server: verifies `target_epoch == current_epoch + 1` (monotonic, rejects a
   stale/replayed commit), atomically moves the staged envelopes into the live
   set, bumps `current_epoch`, and clears the staging rows for that channel. This
   commit is the **single atomic point** — subscribers see the old epoch until it
   runs, then the new epoch with every staged envelope already live. **C-42 is
   preserved**, now as "the commit is atomic" rather than "the one giant request
   is atomic".

The existing single-shot `POST /rotate` stays for small channels (and/or is
re-expressed internally as stage-all-then-commit), so nothing regresses.

**Load-bearing properties:**

- **Staged ≠ served.** Staged envelopes live in a separate table the read path
  (`GET .../envelope`) never consults; only commit promotes them. So the prepare
  phase is invisible to subscribers — no torn rotation is observable mid-stage.
- **Monotonic target epoch.** Commit refuses unless `target_epoch == current + 1`,
  so a replayed or out-of-order commit cannot roll the epoch back or skip.
- **Server stays opaque (invariant #2).** Staged + live envelopes are opaque
  ciphertext; the server never seals, opens, or parses a seed. The staging table
  is ciphertext-only, like the live envelope table.
- **Idempotent + resumable.** Stage is an upsert keyed on
  `(channel, target_epoch, subscriber)`; commit is a no-op-safe single transition.

## Alternatives

- **Bump-epoch-immediately + lazy catch-up.** Advance the epoch first, let
  subscribers whose envelope hasn't landed catch up via re-subscribe. Rejected:
  it **breaks C-42** (a torn-rotation window where some subscribers cannot decrypt
  the new epoch) and is really the N-retention / re-subscribe work (a separate
  sprint), not batched fanout.
- **Keep the single unbounded request.** Rejected — the whole point is that it
  does not scale past a few thousand subscribers.
- **Server-side per-receiver worker (push).** A background worker that seals + pushes
  per subscriber. Rejected for now: the server must stay opaque (it cannot seal —
  no seed), so the client must produce the envelopes; the worker would only move
  bytes, which the stage endpoint already does incrementally. A push/streaming
  delivery layer is a later optimisation orthogonal to this ADR.

## Consequences

- New service surface: `POST /rotate/stage` + `POST /rotate/commit`, a
  `channel_staged_envelopes` table (migration), and `ChannelStore` gains
  `stage_envelopes` + `commit_rotation` (both in-memory + MariaDB impls). C-42's
  test coverage extends to the staged path (staged rows are never served; commit
  is atomic; a wrong `target_epoch` is refused; an abandoned prepare leaves the
  live epoch untouched).
- Client change: the rotation-cron fanout loop chunks the sealed envelopes into
  `stage` calls, then one `commit` — on web + iOS + Android. The crypto
  (`sealSubscriberSeedEnvelope`) is unchanged; only the transport loop chunks.
- **Invariants:** #2 (server opaque) holds — staging is ciphertext-only. #4 (no
  non-E2EE fallback) untouched. C-42 (rotation atomicity) is preserved and
  restated; this ADR + the extended tests are offered to `lakko-security` since
  the change touches the atomicity invariant.
- Deferred: staging TTL / GC for abandoned prepares (a prune sweep, like the
  post/subscriber retention loops); the partial-coverage policy (a subscriber not
  in the staged set at commit time — v1 keeps their prior-epoch envelope, and
  they catch up via the ADR-0010 re-subscribe path, Sprint C); a streaming/push
  delivery layer.

## Open questions

1. **Chunk size.** Fixed max entries per `stage` call (e.g. 1–5k) vs. a byte cap.
   v1 picks a conservative entry cap; tune under load test.
2. **Staging GC.** When does an abandoned prepare (staged but never committed)
   get swept? v1 adds a TTL prune alongside the existing retention loop.
3. **Coverage policy at commit.** Require the staged set to cover all registered
   subscribers, or allow a partial commit (missing subscribers keep their old
   envelope + re-subscribe)? v1 allows partial (a publisher may intentionally
   drop a revoked subscriber by omitting them).

## References

- [ADR-0010](0010-channels-two-tier-broadcast.md) — the two-tier channel design +
  the rotation model this ADR scales.
- Audit-pack **C-42** (`docs/security/calls.md`) — the rotation-atomicity
  invariant this ADR preserves.
- G-1 (`docs/security/known-gaps.md`) — the >50k channels residual; this is its
  scale-out (Sprint D).
- `services/channels/src/{api,storage,sql_store}.rs` — the current single-request
  `/rotate` this ADR extends.
