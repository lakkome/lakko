<!--
SPDX-License-Identifier: AGPL-3.0-or-later
SPDX-FileCopyrightText: 2026 Lakko Contributors
-->

# ADR-0042: Opaque per-group push routing id — close the push social-graph leak

- **Status:** Proposed
- **Date:** 2026-08-06
- **Context:** Phase 3 — the push subsystem (push-gw fan-out + the ADR-0041
  silent-push decrypt). Closes the invariant-#5 push social-graph leak: the push
  subsystem routes by a group identifier that **is** the DS journal key, which
  materialises "these devices are one group" both inside push-gw and — worse — in
  **cleartext at Apple / Google**. Mints gap **G-36**.

## Context

The push subsystem routes group wakes by a 32-byte group identifier that is
**identical to the Delivery Service (DS) journal key** — the client hex-encodes
the same `group_id` buffer that `services/ds`'s `/ds/group/{group_id_hex}` path
uses (`services/push-gw/migrations/0002_push_group_subscriptions.sql:44-49` states
this verbatim: the subscription's `group_id` "Matches the same hex form
`services/ds`'s `/ds/group/{group_id_hex}/messages` path uses"). That single
routing value appears in three places, and every one of them leaks the social
graph:

1. **The subscription row.** `push_group_subscriptions.group_id BINARY(32)`
   (`0002_...sql:50`), written by `group_subscribe`
   (`services/push-gw/src/registry.rs:216-252`, the group decoded at
   `registry.rs:222-234`), **materialises "device X ↔ group G"** inside push-gw —
   the migration's own `## Privacy trade-off` note (`0002_...sql:14-24`) admits
   this is "a real metadata leak vs the §4 'social graph not materialised on
   server' invariant".
2. **The notify query key.** `POST /push/notify-group`
   (`services/push-gw/src/notify.rs:205-233`) decodes the same 32-byte value and
   JOINs it against `push_tokens` (`notify.rs:218-223`) to fan out to every
   subscribed device.
3. **Worst — the delivered payload `g`, in CLEARTEXT, to the push providers.**
   push-gw echoes the notify key straight into the wake payload
   (`notify.rs:261-265`, `group_pseudonym: Some(group)`), and each provider
   serialises it as `g`:
   - **APNs** (`services/push-gw/src/providers/apns.rs:359-373`): `g` at line
     `:371` **and** the notification-grouping `thread-id` at line `:368`, both
     `hex::encode(group_pseudonym)`. Apple sees the same `g` fanned to N distinct
     device tokens — **"these N devices are one group", materialised at a third
     party**.
   - **FCM** (`services/push-gw/src/providers/fcm.rs:352-373`, `g` at `:369`):
     the same, at Google.
   - **WebPush** (`services/push-gw/src/providers/webpush.rs:404-423`, `g` at
     `:419`) is **not** exposed there — the whole `WakeBody` is RFC-8291
     `aes128gcm`-encrypted end-to-end to the subscription before it leaves push-gw
     (`webpush.rs:252-256`, `serde_json::to_vec(&WakeBody) → encrypt_payload`), so
     the browser push endpoint sees only ciphertext.

The migration-0002 comment sketched a fix — a "hash-based `group_id` derivation
(`HMAC(group_secret, "push")` on the client side)" (`0002_...sql:31-35`) — but it
scoped the fix to **only the subscription table** and it presumed a
`group_secret` that **does not exist today** (an MLS group has no stable per-group
shared secret; the exporter secret rotates every epoch, and the DS journal key IS
the leak). It also misses points 2 and 3 entirely.

### Why a subscription-table-only fix is incoherent

The subscription key, the notify key, and the payload `g` are **one shared
key-space**. The notify JOIN (`notify.rs:218-223`) requires the publisher's
notify key and the subscriber's subscription key to be the **same** value, and
push-gw echoes that value straight into the delivered payload
(`notify.rs:261-265` → `apns.rs:371`/`:368`). Opaquify the subscribe + notify
side and the payload `g` follows for free — one atomic change closes both the
**internal** (push-gw) and the **external** (APNs/FCM) leak. Conversely,
opaquifying only the subscription row leaves the notify key and the cleartext
payload `g` still equal to the DS key, closing nothing at the provider.

### push-gw needs no change

push-gw treats the value as an **opaque `BINARY(32)` lookup key** end to end —
`group_subscribe` decodes 32 bytes and `INSERT IGNORE`s them
(`registry.rs:222-234`); `notify_group` decodes 32 bytes and JOINs on them
(`notify.rs:211-224`); the payload just re-emits them. Nothing in push-gw parses
or interprets the value's **meaning**. So the fix is entirely client-side: change
what 32-byte value the clients put on the wire. **The value's meaning shifts from
"DS journal key" to "opaque push routing id"; push-gw's code and schema are
untouched** (invariant #2 — the server stays opaque).

### Interaction with the just-landed ADR-0041 decrypt

ADR-0041 (silent-push background decrypt) shipped on all three clients and uses
the wake payload's `g` field **directly** as the group key: each client's
`resolveAccount(groupHex:)` maps `g` to the owning account + `group_id`, then the
read-only DS fetch uses that `group_id`
(iOS `apps/ios/LakkoNotificationService/NsePushDecrypt.swift:241` + the fetch at
`:297`; web `apps/web/public/sw-push-decrypt.js:164` + fetch at `:227`; Android
`apps/android/app/src/main/kotlin/me/lakko/app/push/PushPreviewDecrypt.kt:223` +
fetch at `:262`). Today `g == hex(group_id)`, so `resolveAccount` is a trivial
identity lookup. Once `g` becomes an opaque routing id, `resolveAccount` must
**reverse-map** `push_gid → (account, real group_id)` — and the DS fetch keeps
using the real `group_id`. The reverse-map must live in the App-Group / origin-
shared index the background NSE / service-worker process can read (the same
`KnownGroupsStore`-class store `resolveAccount` already consults per ADR-0041).

## The load-bearing design decision

There is one decision the maintainer must weigh, and it turns on **stability**:

> **The push routing id must be a value that is (a) not the DS journal key and
> (b) shared by every group member, so publisher and subscriber compute the same
> id for the JOIN — and there is no such value in the system today.** An MLS group
> has no stable shared secret: the audited exporter
> (`crates/lakko-mls/src/group.rs:785`, `export_secret(identity, label, context,
> length)`) rotates on **every** epoch/Commit, and the only stable shared value —
> the `group_id` — is exactly the DS key we are trying to stop leaking.

So closing the leak requires **introducing** a stable, member-shared, non-DS-key
secret and distributing it in-band. That is the whole cost of the fix, and it is
why the decision is Option B (provision a stable distributed secret) rather than
Option A (reuse the rotating exporter):

- A **stable** routing id means the reverse-map (`push_gid → group_id`) is a fixed
  entry computed once per known group; a subscriber that goes offline across a
  membership Commit does not need to re-subscribe, and the iOS `thread-id`
  (`apns.rs:368`) stays constant so lock-screen grouping does not fracture.
- An **epoch-rotating** routing id (Option A) forces a re-subscribe on **every**
  membership commit, gives the NSE/SW a moving reverse-map window (it must track
  which epoch's id a given wake used), and destabilises `thread-id`. Its one
  upside is a forward-unlinkability bonus (a removed member's cached id goes stale
  next epoch) — real, but not worth the churn for V1.

**The maintainer must accept the framing** — "V1 introduces a new stable per-group
secret distributed in-band via an MLS application message, deriving a fixed opaque
push routing id that is not the DS key; it does not rotate per epoch, and it does
not rotate on member removal in V1" — before implementation.

## Decision

Give each group a **`push_routing_secret`** (32 random bytes), generated **once**
at group creation and distributed to every member + every new joiner over a new
**in-group MLS application-message envelope**. Derive a stable opaque
**`push_gid`** from it, and route all three push key-space sites under `push_gid`
instead of the DS journal key.

### The secret and its derivation

- **`push_routing_secret`** — 32 random bytes, minted **once** by the founder at
  `Group::create` and **never regenerated** (so there is exactly one canonical
  secret per group and no cross-member divergence). It is E2EE key material that
  never touches the server (invariant #2).
- **`push_gid = keyed-BLAKE3(push_routing_secret, "lakko/push/group-routing-id/v1")`**,
  truncated/rendered to **32 bytes** → hex. Domain-separated, one-way, and
  **stable** (no epoch input). 32 bytes → the existing `BINARY(32)` / hex
  key-space, so **push-gw's schema and code are untouched** (the value that flows
  through `registry.rs` / `notify.rs` / the providers is just a different 32-byte
  buffer). `push_gid ≠ hex(group_id)`, so a push-gw–vs–DS cross-correlation can no
  longer link a subscription to a group without the group secret.

### The distribution envelope

A new **domain-separated, fail-closed binary envelope**, mirroring the
`crates/lakko-contacts/src/media_state.rs` pattern (a fixed ASCII domain prefix
`"…\x00"` + a version byte + the payload, classified by its leading-byte prefix
in the receive-loop envelope dispatch, so it is **never** rendered as a chat
bubble). Wire shape:

```text
  PUSH_ROUTING_SECRET_ENVELOPE_DOMAIN_V1 || version_byte || secret(32)
  └──────── ASCII, "\x00"-terminated ────┘ └── 0x01 ──┘ └── 32 bytes ──┘
```

It rides as an **MLS application message on the group journal** (E2EE — the DS
sees ciphertext, invariant #2), with a **bounded, fail-closed codec** (exact-
length + domain-match + version gate, degrade-to-ignore on any mismatch) and a
**fuzz target** for the decoder (the receive-loop reaches it on attacker-touchable
journal bytes before any state is consulted, the same threat class as
`media_state_decode` / `welcome_rendezvous_decode`).

- **On creation:** the founder mints the secret and broadcasts the envelope to the
  group.
- **On a new join:** an existing member (the adder/committer) re-broadcasts the
  **current** secret so the joiner learns it post-Welcome. Joiners never mint a
  new secret — they only receive — so the "mint once at creation" invariant holds
  and there is no divergence.

Recommended placement: a new module beside `media_state.rs` in `lakko-contacts`
(sibling of the call-orchestration envelopes it dispatches), reusing the
`incoming_envelope` classification seam; exact crate placement is a minor call
(§ Open questions).

### Client routing + the reverse-map

- Generate + persist `push_routing_secret` at `Group::create`; receive + persist
  it on the distribution envelope; re-broadcast it to each new joiner.
- Compute `push_gid` and **subscribe / notify under it** (`POST
  /push/group/subscribe` + `POST /push/notify-group` carry `push_gid`; the
  payload `g` follows for free since push-gw echoes the notify key).
- Build + persist a **`push_gid → (account, group_id)` reverse index** in the
  App-Group / origin-shared store the background process reads. The client owns
  every group's secret, so it computes each known group's fixed `push_gid` once
  and files the mapping. The ADR-0041 `resolveAccount(groupHex:)` seam
  (iOS `NsePushDecrypt.swift:241`, web `sw-push-decrypt.js:164`, Android
  `PushPreviewDecrypt.kt:223`) changes from an identity lookup to a reverse-map
  lookup; the DS fetch **keeps using the real `group_id`**
  (`NsePushDecrypt.swift:297` / `sw-push-decrypt.js:227` /
  `PushPreviewDecrypt.kt:262`).

### Rotation policy (V1)

Distribute the secret at group creation + to each new joiner; **do NOT rotate on
member removal in V1.** A removed member already knows the public `group_id`, so
retaining the secret is no worse than the accepted baseline — the DS-side "device
fetches group G" correlation (`0002_...sql:20-24`, point 1), which is explicitly
out of scope. Rotation-on-removal is noted as a **future forward-unlinkability
enhancement** (§ Open questions).

## Alternatives

- **Option A — epoch-rotating exporter-derived id.** `push_gid =
  export_secret("lakko/push/group/v1", group_id, 32)` over the audited MLS
  exporter (`crates/lakko-mls/src/group.rs:785`). **Rejected for V1.** No new
  secret to distribute, and it carries a free forward-unlinkability bonus (a
  removed member's cached id goes stale next epoch). But the exporter **rotates
  every epoch/Commit**, so: every membership commit forces a group-wide
  re-subscribe; the NSE/SW reverse-map becomes a **moving window** (a wake must be
  matched against the epoch's id it was minted under, and an in-flight wake can
  reference an id one epoch stale); and the iOS `thread-id` (`apns.rs:368`)
  changes each epoch, fracturing lock-screen grouping. Kept as a noted cheaper-to-
  build alternative whose churn cost outweighs the unlinkability bonus for V1;
  revisit if rotation-on-removal is prioritised.

- **Subscription-table-only opaquification** (the migration-0002 sketch,
  `0002_...sql:31-35`). **Rejected — incoherent** (§ Context). The subscription
  key, the notify key, and the payload `g` are one shared key-space
  (`notify.rs:218-223` + `:261-265`); opaquifying only the subscription row leaves
  the notify key and the cleartext provider `g` still equal to the DS key, so the
  APNs/FCM external leak — the worst of the three — stands. It also assumed a
  `group_secret` that does not exist yet.

- **Do nothing.** **Rejected.** The leak stands: push-gw materialises the social
  graph and Apple/Google see it in cleartext, directly violating invariant #5.

- **Payload-encryption-only** (encrypt the APNs/FCM `g` like WebPush does,
  `webpush.rs:252-256`). **Rejected as insufficient.** APNs/FCM payloads cannot be
  E2EE to the provider the way RFC-8291 WebPush is (the OS push pipeline needs
  `thread-id` and routing fields in the clear for grouping / delivery), and it
  would not touch the **push-gw-internal** subscription-table leak at all. An
  opaque routing id is needed regardless; once you have it, encrypting the rest is
  moot for the `g`/`thread-id` correlation.

## Consequences

- **Positive.** Closes the invariant-#5 push social-graph leak on **both** fronts
  — the push-gw-internal subscription/notify materialisation **and** the external
  APNs/FCM cleartext `g`/`thread-id` correlation — with **zero push-gw code or
  schema change** (the server stays opaque, invariant #2). The routing id is
  stable, so the reverse-map is a fixed per-group entry, subscribers survive
  membership commits without re-subscribing, and iOS lock-screen grouping stays
  intact. The distribution envelope reuses the audited, fuzzed `media_state`
  fail-closed-codec pattern.

- **Negative / new risks.** A **wire-format change** (a new in-group envelope
  kind + a flip of the payload `g`'s semantics). A **new piece of E2EE key
  material per group** (`push_routing_secret`) that must be minted, persisted,
  distributed to joiners, and reverse-indexed on three platforms — new client
  state and a new failure mode (a member who never received the envelope subscribes
  under the wrong/absent id and misses wakes until it catches up). **Legacy groups**
  created before this change have no secret and must be provisioned or fall back to
  the leaky `g` until then (§ Open questions). The V1 no-rotation-on-removal choice
  means a removed member keeps a valid `push_gid` (bounded by the accepted DS-side
  baseline, but a real residual — see G-36).

- **Neutral.** Client-only; push-gw is verified-unchanged (its tests should be
  byte-identical). The ADR-0041 decrypt path gains a reverse-map step but its
  read-only-throwaway discipline is untouched. The DS-side "device fetches group G"
  correlation (`0002_...sql:20-24`) remains the accepted out-of-scope baseline.

### Invariants touched

- **#5 (the social graph is not materialised on the server) — the one being
  fixed.** Today the push subsystem materialises "device ↔ group" internally and
  at Apple/Google via a value equal to the DS key. Under this ADR the routing id
  is an opaque, one-way derivation of a member-only secret, so neither push-gw nor
  a push provider can link a subscription/wake to a group (or cross-correlate with
  the DS journal) without the group secret.
- **#2 (server sees only ciphertext + public artefacts) — holds / strengthened.**
  push-gw handles `push_gid` as the same opaque `BINARY(32)` it already handles;
  the distribution envelope is an E2EE MLS application message (the DS sees
  ciphertext). No key material reaches any server.
- **#3 (the MLS state machine is sacrosanct) — untouched.** The secret rides as an
  ordinary application message; no `lakko-mls` state-machine change. (If the
  distribution envelope is placed in `lakko-contacts`, no `crates/lakko-mls` edit
  at all.)
- **#4 (fail-closed) — holds.** The distribution-envelope codec is bounded and
  fail-closed (a malformed envelope is ignored, never mis-parsed); a client without
  the secret does not fall back to leaking the DS key as `g` — it simply does not
  subscribe under a valid id until it receives the secret (a delivery-availability
  cost, not a confidentiality downgrade).
- **Invariant-index touchpoint — a new C-entry on landing** (next after C-59, i.e.
  **C-60**): the opaque per-group push routing id + the in-band secret distribution
  + the fail-closed codec + the reverse-map. **Not added now** (Proposed); it lands
  with implementation. **Mints gap G-36** (the residual: the DS-side device↔group
  correlation baseline + the V1 no-rotation-on-removal window) — recorded here,
  **not** added to `docs/security/known-gaps.md` yet.

**`lakko-security` gate.** This is a privacy-invariant change (invariant #5) that
introduces new E2EE key material and a new wire envelope, so it **must go through
`lakko-security` before merge** (Sprint 6).

## Open questions

- **Legacy-group provisioning + concurrent-mint avoidance.** How groups created
  before this change acquire a secret without two members concurrently minting
  divergent secrets. Candidate: only the ADR-0022 **owner** mints for a legacy
  group (single authoritative minter), or legacy groups keep the leaky `g` until a
  natural re-creation. Needs a decision in Sprint 1/3.
- **Rotation-on-removal (forward unlinkability).** V1 deliberately does not rotate
  the secret when a member is removed. A future enhancement would re-mint +
  re-distribute on removal so a removed member's cached `push_gid` goes stale — the
  forward-unlinkability bonus Option A gets for free. Deferred; tracked under G-36.
- **Exact crate placement of the envelope + derivation.** Recommended beside
  `media_state.rs` in `lakko-contacts` (pattern + dispatch reuse); a dedicated tiny
  crate is the alternative. Minor.
- **Reverse-map staleness for in-flight wakes.** With a stable id this is a
  non-issue in steady state, but the legacy-transition window (a group mid-
  provisioning) needs the NSE/SW to fall back gracefully (no valid reverse-map
  entry → generic banner, per the ADR-0041 fail-closed posture).

## Sprint decomposition

1. **`lakko-*` core.** The `push_routing_secret` type, the in-group distribution
   envelope (bounded fail-closed codec + **fuzz target**, mirroring
   `media_state`), and the `push_gid` derivation (keyed-BLAKE3, domain
   `lakko/push/group-routing-id/v1`). Property tests for derivation stability +
   codec round-trip + fail-closed rejection.
2. **FFI (uniffi + wasm).** compose/parse the distribution envelope + derive
   `push_gid`; regen Swift/Kotlin bindings + rebuild wasm.
3. **Clients (web + iOS + Android).** Mint + persist the secret at `Group::create`;
   receive + persist it on the envelope; re-broadcast to joiners; subscribe /
   notify under `push_gid`; build + persist the `push_gid → (account, group_id)`
   reverse index in the App-Group / origin-shared store.
4. **ADR-0041 decrypt reverse-map.** Change `resolveAccount` on all three clients
   (iOS `NsePushDecrypt.swift:241`, web `sw-push-decrypt.js:164`, Android
   `PushPreviewDecrypt.kt:223`) to reverse-map `push_gid → group_id`; the DS fetch
   keeps the real `group_id`.
5. **push-gw — no change.** Verify the push-gw code + schema are untouched and its
   tests are byte-identical (the value is opaque to it end to end).
6. **`lakko-security`** review before merge (invariant #5 privacy change + new key
   material + new wire envelope).
7. **Docs reconcile.** On landing: mint **G-36** in `docs/security/known-gaps.md`,
   add the **C-60** invariant-index entry, and update the `CLAUDE.md` ADR table +
   module map (the new `lakko-contacts` push-routing module).

## References

- `services/push-gw/migrations/0002_push_group_subscriptions.sql` — the
  subscription table: `group_id BINARY(32)` (`:50`), the "same hex form as
  `/ds/group/{group_id_hex}`" note (`:44-49`), the `## Privacy trade-off`
  admission (`:14-24`), the accepted DS-side baseline (`:20-24`), and the
  subscription-only Phase-2 HMAC sketch this ADR supersedes/completes (`:31-35`).
- `services/push-gw/src/registry.rs:216-252` — `group_subscribe`; the group is
  decoded + `INSERT IGNORE`d as opaque bytes at `:222-234` (push-gw treats it as an
  opaque key).
- `services/push-gw/src/notify.rs:205-233` — `notify_group`; the JOIN requiring
  publisher/subscriber key equality (`:218-223`) and the echo of the notify key
  into the wake payload (`:261-265`).
- `services/push-gw/src/providers/apns.rs:359-373` — `ApsBody::from_payload`: the
  cleartext `g` (`:371`) and the `thread-id` (`:368`), both
  `hex::encode(group_pseudonym)`.
- `services/push-gw/src/providers/fcm.rs:352-373` — `WakeData::from_payload`, the
  cleartext `g` (`:369`).
- `services/push-gw/src/providers/webpush.rs:404-423` — `WakeBody` with `g`
  (`:419`) that is **not** exposed because the body is RFC-8291 `aes128gcm`-
  encrypted end-to-end before send (`:252-256`).
- `crates/lakko-mls/src/group.rs:785` — `export_secret(identity, label, context,
  length)`, the audited MLS exporter Option A would reuse (rotates per epoch).
- `crates/lakko-contacts/src/media_state.rs` — the domain-separated, fail-closed,
  fuzzed relay-envelope pattern the distribution envelope mirrors.
- `apps/ios/LakkoNotificationService/NsePushDecrypt.swift:241` (`resolveAccount`) +
  `:297` (DS fetch by `groupIdBytes`) — the ADR-0041 iOS decrypt seam the
  reverse-map changes.
- `apps/web/public/sw-push-decrypt.js:164` (`resolveAccount`) + `:227` (DS fetch)
  — the web decrypt seam.
- `apps/android/app/src/main/kotlin/me/lakko/app/push/PushPreviewDecrypt.kt:223`
  (`resolveAccount`) + `:262` (DS fetch) — the Android decrypt seam.
- **ADR-0041** — iOS silent-push NSE decrypt; this ADR adds the reverse-map to its
  three decrypt paths.
- **ADR-0022** — group-admin roles; the owner is the candidate single-minter for
  legacy-group provisioning (§ Open questions).
- **Invariant #5** — "the social graph is not materialised on the server" — the
  invariant this ADR closes for the push subsystem.
