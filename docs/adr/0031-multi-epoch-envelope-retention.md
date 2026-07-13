<!--
SPDX-License-Identifier: AGPL-3.0-or-later
SPDX-FileCopyrightText: 2026 Lakko Contributors
-->

# ADR-0031: Multi-epoch channel seed-envelope retention + subscriber catch-up

- **Status:** Accepted (maintainer approved the design; implementation in progress)
- **Date:** 2026-07-13
- **Context:** Phase 4 (channels, G-1) — an offline subscriber cannot read posts
  made under a rotation it missed. The N-retention / re-subscribe follow-up
  ADR-0030 explicitly deferred to "Sprint C".

## Context

A channel seed rotation ([ADR-0010](0010-channels-two-tier-broadcast.md)) advances
the epoch and reseals a fresh HPKE seed envelope to every subscriber. Each
`ChannelPost` carries its `epoch_id` in the clear header (`post.rs` wire layout)
and is AEAD-sealed under **that epoch's** content-key seed. A subscriber decrypts
a post by selecting the seed matching the post's epoch.

Today both the server and the clients keep exactly **one** seed envelope per
subscriber:

- Server (`services/channels`): `channel_envelopes` is keyed `(channel,
  subscriber)` — latest-wins. `deliver_envelope` / `rotate` / `commit_rotation`
  all UPSERT the single row, and `fetch_envelope` returns it. The in-memory store
  comments this the "N=1 in this MVP — multi-epoch retention is a follow-up"
  model.
- Clients (web/iOS/Android): a single `subscribedSeed` per channel. The web
  subscriber read path (`Channels.tsx` `fetchPosts` -> `verifyAndDecryptPost`)
  decrypts every post with that one seed.

The consequence: a subscriber that is offline across a rotation (epoch N -> N+1)
comes back, gets only the **current** (N+1) seed, and **cannot decrypt** any post
made under epoch N — the seed for N is gone on both sides. Those posts render as
undecryptable. At the 24h rotation cadence ADR-0010 targets, a subscriber offline
for even one day loses a day of history.

## Decision

**Retain the most-recent N seed envelopes per subscriber (default N = 4), keyed by
epoch, on the server; the client keeps a per-channel `epoch -> seed` map, fetches
the retained set on catch-up, and decrypts each post with the seed matching its
`epoch_id`. A subscriber offline for longer than N epochs re-syncs to the current
seed and accepts the older posts as unreadable.** (ADR-0010 §retention, N = 4
covers ~96h at the 24h cadence.)

### Server

1. **`channel_envelopes` keyed by epoch.** Migration `0004` adds an `epoch`
   column and re-keys the primary key to `(channel_id, subscriber_id, epoch)`, so
   up to N epochs of envelopes coexist per subscriber. Existing single rows
   backfill to the channel's `current_epoch`.
2. **Store-at-epoch + prune-to-N.** Every write path files the envelope under a
   concrete epoch and then prunes that `(channel, subscriber)` to its highest N
   epochs:
   - `deliver_envelope(…, epoch)` (initial join / re-sync) files at the
     publisher-supplied epoch, defaulting to the channel's `current_epoch` when
     the caller omits it.
   - `rotate` files the batch at the new epoch (`current + 1`).
   - `commit_rotation` promotes the staged rows at `target_epoch`
     ([ADR-0030](0030-batched-rotation-fanout.md)) and files them there.
3. **Two read shapes.** `fetch_envelope` (singular, unchanged wire) returns the
   envelope at the **highest** retained epoch — the current seed a fresh
   subscriber needs. A new `fetch_envelopes` (plural) returns the whole retained
   `[(epoch, envelope)]` set, ascending — what a catching-up subscriber opens
   into its `epoch -> seed` map. New endpoint `GET
   /v1/channels/:id/subscribers/:pk/envelopes`.
4. **Retention window is config.** `LAKKO_CHANNELS_ENVELOPE_RETENTION_EPOCHS`
   (default 4, floor 1).

### Client (web / iOS / Android)

1. **`epoch -> seed` map** replaces the single `subscribedSeed` per channel.
2. **Catch-up.** On opening a channel, `fetchEnvelopes` -> open each with the
   existing `openSubscriberSeedEnvelope` -> populate the map. (No FFI change —
   `decodeChannelPost` already exposes `epoch_id`, and `verifyAndDecryptPost`
   already takes a seed.)
3. **Decrypt-by-epoch.** For each fetched post, decode -> read `epoch_id` -> pick
   the seed from the map -> `verifyAndDecryptPost`. A post whose epoch is not in
   the map (older than retention, or a gap) renders as an "older post — key no
   longer available" placeholder and is skipped, never decrypted with the wrong
   seed.
4. **Re-sync (offline past N).** If the map has no entry for the **current**
   epoch, the subscriber re-subscribes (idempotent) so the publisher reseals it
   on the next fanout, and marks the beyond-retention posts unreadable. This is
   the ADR-0010 "re-subscribe handshake" — light, because rotation already
   reseals every registered subscriber, so the current seed is always reachable
   via `fetch_envelope`.

**Load-bearing properties:**

- **C-42 preserved (no torn rotation).** `channel_envelopes` only ever holds
  epochs `<= current_epoch`: `deliver_envelope` **refuses** an epoch greater than
  `current_epoch` (a publisher cannot pre-seed a future epoch outside the
  staged/commit path), and `rotate` / `commit_rotation` file at the new epoch in
  the same atomic transaction that bumps `current_epoch`. So a subscriber never
  observes a future-epoch envelope before the rotation commits — `fetch_envelope`
  returning the highest retained epoch is always `<= current`.
- **Server stays opaque (invariant #2).** The new `epoch` column is server
  bookkeeping alongside opaque envelope ciphertext; the server never seals,
  opens, or parses a seed. The retained set is opaque bytes in, opaque bytes out.
- **No non-E2EE fallback (invariant #4).** A post whose epoch seed is absent is
  **not** rendered from plaintext or decrypted with a wrong seed — it is skipped
  with an explicit placeholder. Losing old history is an availability cost, never
  a confidentiality break.
- **Monotone, bounded.** Retention is the highest N epochs per subscriber;
  pruning is a bounded delete on each write. No unbounded growth.

## Alternatives

- **Keep N = 1 (status quo).** Rejected — it is the gap: a one-day offline
  subscriber loses a day of history.
- **Retain all epochs forever.** Rejected — unbounded per-subscriber growth at
  channel scale (50k–1M subscribers), and the 30-day post-prune already caps how
  far back history is readable, so retaining seeds past that is dead weight.
- **Re-encrypt old posts to the new seed on rotation.** Rejected — O(posts x
  subscribers) re-encryption per rotation, and the server cannot do it (opaque,
  no seed). The retained-envelope approach is O(subscribers) per rotation, which
  the fanout already pays.
- **Client keeps only the current seed; server returns per-epoch seeds on
  demand.** This is essentially the chosen design; the map is the client-side
  cache of the retained set so a re-open doesn't refetch.

## Consequences

- Service surface: migration `0004`, `channel_envelopes.epoch`, a `fetch_envelopes`
  store method + `GET …/envelopes` endpoint, `deliver_envelope` gains an epoch,
  and every write path prunes to N. Both `InMemoryChannelStore` +
  `SqlChannelStore` change in lock-step; the opacity contract is unchanged.
- Client change: `epoch -> seed` map, `fetchEnvelopes`, decrypt-by-epoch, and the
  re-sync detection — on web + iOS + Android. No FFI change (the crypto surface
  `decodeChannelPost` / `openSubscriberSeedEnvelope` / `verifyAndDecryptPost`
  already exists).
- **Invariants:** #2 (server opaque) holds; #4 (no non-E2EE fallback) holds — a
  missing-seed post is skipped, not force-decrypted. **C-42** (rotation
  atomicity) is preserved and slightly strengthened (the future-epoch refusal is
  now explicit). This ADR + the retention tests are offered to `lakko-security`
  since the change touches the C-42 atomicity path.
- Deferred: an explicit publisher-driven "re-seed me now" endpoint for a
  re-syncing subscriber (v1 leans on the next scheduled rotation reseal +
  `fetch_envelope` for the current seed); a per-subscriber retention override; a
  live 2-subscriber offline-catch-up e2e (no deployed channels environment).

## References

- [ADR-0010](0010-channels-two-tier-broadcast.md) §retention — "the most-recent N
  seed envelopes per subscriber are kept (default N = 4)… a subscriber offline
  for a week catches up via a re-subscribe handshake" — the source of this design.
- [ADR-0030](0030-batched-rotation-fanout.md) — deferred this work to "Sprint C";
  `commit_rotation` (its staged-commit path) now files at `target_epoch` +
  prunes to N.
- Audit-pack **C-42** (`docs/security/calls.md`) — the rotation-atomicity
  invariant this ADR preserves.
- G-1 (`docs/security/known-gaps.md`) — the >50k channels residual; this closes
  the offline-catch-up sub-item.
- `services/channels/src/{storage,sql_store,api,config}.rs` +
  `migrations/0004_multi_epoch_envelopes.sql` — the implementation.
