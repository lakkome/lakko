<!--
SPDX-License-Identifier: AGPL-3.0-or-later
SPDX-FileCopyrightText: 2026 Lakko Contributors
-->

# ADR-0009: Link-based call invitations

- **Status:** Accepted — **the multi-party joining model below was replaced by
  [ADR-0032](0032-multi-joiner-call-links.md)** (2026-07). The capability codec,
  the envelope formats, the revocation design and the URL parity are unchanged
  and still in force.
- **Date:** 2026-05-21

> **Update 2026-09-27 ([ADR-0028](0028-group-join-ticket.md) short-link
> amendment):** the `/c/` ledger id below is no longer `SHA-256(capability_bytes)`.
> All three invite ledgers now use a random 6-byte id the server mints at
> upload (8 base64url characters, `BINARY(6)`), so a `/c/` pointer link is
> `https://<host>/c/<8 chars>` and uploads are no longer idempotent by content.
- **Context:** Phase 3c3 (link calls — sub-sprint cluster of Phase 3 Calls)

> **AMENDED by [ADR-0032](0032-multi-joiner-call-links.md); recorded here
> 2026-09-02.** ADR-0032 cites this ADR four times as the thing it fixes. **This
> ADR cited it nowhere** — no status note, no forward link — so the superseded
> model below read as current.
>
> **What changed, and it is the load-bearing half:** this ADR has the **joiner
> found the MLS group** (`accept_call_invite` → the joiner is founder, owner
> added via the embedded KeyPackage), with the **first joiner becoming the
> committer**. That does not survive a shareable link: two joiners arriving at
> once each found a group, and the call splits in two — the **G-21 two-groups
> bug**. It also puts a **one-time KeyPackage in a link meant to be shared**.
>
> ADR-0032 inverted it: **the owner is the sole founder** (`found_call`), so
> there is no founding race by construction, the embedded owner KP goes unused
> and each joiner supplies its own fresh one; and the committer is chosen by the
> pure `elect_committer` rather than by arrival order (see the resolved Open
> question below). The new surface is `found_call` / `request_call_join` /
> `admit_joiner` / `join_call_from_welcome` / `elect_committer`.
>
> **`accept_call_invite` still exists and is still correct** — retained as the
> **1×1 fast path**, which is the topology this ADR was written for and where a
> founding race cannot occur. So read the multi-party sections below as the V1
> design and its post-mortem, not as the shipped one; the two topologies are
> documented side by side in
> [`link_invite.rs`](../../crates/lakko-calls/src/link_invite.rs).
>
> **A smaller scoping note:** this ADR says it expands ADR-0005 "scenario E",
> quoting it as *"invite a contact into a group call link"*. ADR-0005's scenario
> E is **"invite into a group"** — the group-invite case, which
> [ADR-0028](0028-group-join-ticket.md) actually built. ADR-0005 has no call-link
> scenario; this one is genuinely new rather than an expansion of an existing
> row.

## Context

ADR-0005 sketches six contact-invitation scenarios. Scenario E
("invite a contact into a group call link") was deferred to Phase 3
because the call-side cryptography (MLS group, SFrame media keys,
call-admission JWT) only exists from Phase 3 onward. By the time
Phase 3c3 starts the call media keying is solid (C-1..C-23 of
[`docs/security/calls.md`](../security/calls.md)) but the link-call
**onboarding** path has nothing — no codec for a portable invite, no
server-side ledger, no joiner-acceptor orchestration, no transport
for ferrying MLS Welcomes between strangers.

Forces:

1. **Strangers, not contacts.** A link-based call invitation lands on
   a joiner who is NOT already in the owner's contact list. There is
   no pre-existing MLS group to message through, no Ed25519
   fingerprint already in the joiner's KT-log pin set. The protocol
   must establish trust **and** route a Welcome at the same time.

2. **The server still sees only ciphertext + pseudonyms.** Security
   invariants from
   [`CLAUDE.md`](../../CLAUDE.md) hold: the discovery service stores
   capability blobs content-addressed by SHA-256, but never sees
   plaintext intro messages, never sees the Welcome bytes, never
   sees who joined which call group. Stranger-inbox routing for the
   Welcome reuses ADR-0005's HPKE-sealed envelope path.

3. **Multi-platform parity.** Web (TypeScript), iOS (Swift), Android
   (Kotlin) all need the same surface — composer, acceptor, URL
   plumbing — and a drift between them surfaces as a UX bug or worse
   a cross-platform interop failure. The protocol must be specifiable
   tightly enough that all three platforms agree byte-for-byte.

4. **Multi-party.** Initially 1×1, but the design has to extend to
   3+ participants without re-keying the protocol — the second
   joiner should not require a "v2" of the codec or the envelope
   chain.

5. **DoS resistance.** PoW + Privacy Pass already gate uploads /
   consumes on the discovery side; the codec should not introduce a
   new attack surface (unbounded allocations, panics on malformed
   wire, signature-verification bypass) that the existing rate
   limit can't absorb.

6. **Defence in depth against link substitution.** A malicious
   server can serve substituted bytes; a captured URL can be
   shared with the wrong joiner. The joiner needs an
   out-of-band fingerprint they can verify, parallel to ADR-0005's
   Signal-style safety numbers.

## Decision

A **capability-based** protocol with content-addressed server-side
storage, owner-signed wire format, MLS-bound media keys, and a
two-envelope ferry path for joiner→owner communication. URL shape
`<origin>/c/<slug>`, mirroring contact-invite's `<origin>/i/<slug>`.

### 1. Wire format — `CallInviteCapability`

```text
schema:1B || ed25519_pubkey:32B || owner_account_id:16B
       || call_id:16B || expiry_ms:8B (BE) || max_uses:4B (BE)
       || media_flags:1B
       || kp_len:4B (BE) || one_time_key_package:kp_len
       || label_len:2B (BE) || label:label_len
       || ed25519_signature:64B
```

Signed by the owner's Ed25519 identity key over everything except
the trailing 64-byte signature. `media_flags` is the bit pair
`audio | video << 1`. `one_time_key_package` is an MLS KeyPackage
the owner minted via `identity.createKeyPackage()` — used **once**
to bootstrap the call group with the first joiner. Pinned at
`MAX_KEY_PACKAGE_LEN = 4096` bytes for OpenMLS sanity; the codec
bounds-checks the field but does NOT parse the KP (MLS validation
is `lakko-mls`'s job at accept time).

Source of truth: `crates/lakko-contacts/src/call_invite.rs::compose`
+ `decode_and_verify`. Pinned by C-24, C-25, C-26 in `calls.md`.

### 2. Server-side blob ledger

Capability bytes go to **content-addressed** storage on the
discovery service. New table `call_invites` keyed by
`blob_id = SHA-256(capability_bytes)`:

| column | type | notes |
|---|---|---|
| `blob_id` | `BINARY(32)` PK | content-addressed |
| `capability_bytes` | `VARBINARY(8192)` | wire bytes; server re-verifies signature at upload |
| `owner_account_id` | `BINARY(16)` | denormalised, for owner-list queries |
| `call_id` | `BINARY(16)` | denormalised, for per-call queries |
| `expiry_ms` | `BIGINT UNSIGNED` | folded into 404 once elapsed |
| `max_uses` | `INT UNSIGNED` | sentinel `0` = single-use, `u32::MAX` = unlimited |
| `used_count` | `INT UNSIGNED` | atomic CAS on consume |

Endpoints:

- `POST /discovery/call-invite` — upload (session-token-gated, PoW,
  server re-runs `decode_and_verify` + matches embedded
  `owner_account_id` to the session's token).
- `GET /discovery/call-invite/{blob_id}` — anonymous fetch; expired
  rows fold into 404 to deny status-code probing.
- `POST /discovery/call-invite/{blob_id}/consume` — anonymous, PoW;
  atomic CAS on `used_count` with `CASE max_uses WHEN 0 THEN 1 WHEN
  4294967295 THEN 4294967295 ELSE max_uses END`. 410 Gone on
  exhausted; 404 on missing/expired. Per-joiner dedup via
  side-table `call_invite_consumers (blob_id, joiner_tag)` with
  `FOREIGN KEY ... ON DELETE CASCADE`.
- `DELETE /discovery/call-invite/{blob_id}` — owner-signed
  revocation. Body carries `signed_at_ms + Ed25519 signature` over
  `CALL_INVITE_REVOKE_DOMAIN_V1 || blob_id || signed_at_ms_be`.
  Server recovers owner pubkey from stored capability bytes and
  verifies. ±10-minute replay window.

Source: `services/discovery/src/call_invite.rs` +
`migrations/0007_call_invites.sql` + `0008_call_invite_consumers.sql`.
Pinned by C-29..C-31.

### 3. URL plumbing

Two URL forms, discriminated on byte length:

- **Pointer** — `<origin>/c/<base64url(blob_id)>` — 32-byte
  payload. Production default. Joiner runs `fetchCallInvite(blob_id)`
  to recover the wire bytes.
- **Inline** — `<origin>/c/<base64url(capability_bytes)>` — ≥ 400
  bytes (the 3c3 wire-format floor with a 256-byte one-time KP).
  Censored-region fallback when `/discovery/call-invite` is
  unreachable. No server round-trip on open.

Helpers on three platforms — `apps/web/src/calls/invite.ts`,
`apps/ios/Lakko/Calls/CallInviteUrl.swift`,
`apps/android/.../calls/CallInviteUrl.kt`. Parity API:
`composePointerCallInviteUrl`, `composeInlineCallInviteUrl`,
`composeCallInviteUrl` (auto-pick by byte count),
`parseCallInviteSlug` returning a discriminated
`{ pointer, blob_id }` / `{ inline, capability_bytes }`.

The `/c/` prefix mirrors contact-invite's `/i/` so the joiner-side
dispatcher (web router / iOS universal link / Android deep-link
intent filter) branches on the path segment without parsing the
slug.

### 4. Composer + acceptor orchestration

**Composer** (`composeAndUpload(...)`) collapses the five owner-side
steps into one call:

1. Generate 16-byte random `call_id`.
2. `identity.createKeyPackage()` — one-time KP for the invite.
3. `composeCallInviteCapability(...)` — wire bytes.
4. `DiscoveryClient.uploadCallInvite(bytes)` — POST + content-address.
5. `composePointerCallInviteUrl(blob_id, origin)` — final URL.

**Acceptor** (`acceptCallInviteFromSlug(...)`) collapses the seven
joiner-side steps into one discriminated `Result`:

1. `parseCallInviteSlug(slug)` — pointer vs. inline.
2. (pointer) `DiscoveryClient.fetchCallInvite(blob_id)`.
3. `CallInviteCapability.decodeAndVerify(bytes)` — local re-verify.
4. `capability.isValidNow(now_ms)` — expiry filter.
5. (pointer) `DiscoveryClient.consumeCallInvite(blob_id,
   capability_bytes, joiner_account_id)` — counter + dedup.
6. `acceptCallInvite(capability, joiner_identity)` — bootstrap a
   fresh MLS group with the joiner as founder, owner added via the
   embedded KP.
7. Return `{ blobId, callId, ownerFingerprint, welcomeForOwner,
   callKeySeed }`.

The `callKeySeed` is the SFrame seed derived via
`mls_exporter("lakko/call/v1", call_id, 32)` — the joiner can seal
its first frame BEFORE the owner has joined.

Source: `apps/{web,ios,android}/.../calls/composer.{ts,swift,kt}`
+ `acceptor.{ts,swift,kt}`. Pinned by C-27.

### 5. Welcome ferry + multi-party KP forward

After step 6 the joiner has `welcomeForOwner` bytes the owner needs
to run `Group::join_from_welcome` and enter the call MLS group.
**Reuse the existing ContactRequest HPKE-sealed envelope transport**
(ADR-0005 scenario A) — joiner sends a ContactRequest to the
owner's stranger-inbox pseudonym with the Welcome as the inner
plaintext. Decision: no new server endpoint. Domain-separated
wrapping inside the plaintext distinguishes a Welcome from a plain
intro message:

```text
wrapped_plaintext = CALL_WELCOME_ENVELOPE_DOMAIN_V1 || welcome_bytes
                   ───── 22 bytes, ASCII + NUL terminator ─────
                   "lakko/call-welcome/v1\x00"
```

For multi-party (3+ participants), the **second** joiner does NOT
found a new MLS group — they ferry their KeyPackage to the
committer (initially the first joiner, who becomes owner-side after
the bootstrap commit) via a sibling envelope:

```text
wrapped_plaintext = CALL_KP_FORWARD_ENVELOPE_DOMAIN_V1 || kp_bytes
                   ───── 25 bytes, ASCII + NUL terminator ─────
                   "lakko/call-keypackage/v1\x00"
```

The committer runs `Group::add_member_from_wire(kp)` on the
existing call group, the resulting Welcome flows back via the
welcome envelope, the second joiner runs `join_from_welcome`. The
two envelopes compose: a third-party KP-forward routes through the
dispatcher to the committer, who emits a fresh welcome envelope
back, which routes through the dispatcher on the new joiner's side.

**Owner-side dispatcher** runs after `open_envelope` on every
incoming ContactRequest and discriminates on byte prefix:

```rust
pub fn dispatch_incoming_envelope(plaintext: &[u8])
    -> Result<IncomingEnvelope<'_>, DispatchError>   // ADR said CallWelcomeError
```

Three variants — `CallWelcome(&[u8])`, `CallKeyPackage(&[u8])`,
`IntroMessage(&[u8])` — pinned by C-34.

The dispatch order matters and is **load-bearing**: welcome first,
KP-forward second, intro-message fallthrough third. Pinned in one
place (Rust) so all three platforms inherit the same order via
FFI; a future addition of a fourth envelope kind lands as a single
variant + step here, not as parallel edits on three platforms.

### 6. Owner fingerprint for OOB verification

Every accepted capability carries an `ownerFingerprint` — a 71-char
Signal-style safety number derived from the owner's Ed25519 pubkey
via `safety_number(pk, pk)` (same primitive ADR-0005 uses for
contact-invite QR pinning, applied with the pubkey as both
"local" and "remote" so a single capability produces a single
number).

The joiner-side landing screen surfaces the number under an "Is
this really Alice?" prompt; the owner reads their own
fingerprint off their profile screen, the joiner reads it off the
landing screen, they compare out-of-band. Defence-in-depth against
a captured-URL substitution.

Pinned by C-28.

## Alternatives

### Alternative A — Inline-only (no server-side blob ledger)

Encode the full capability bytes directly into the URL slug. No
discovery service round-trip on accept; censored-region path works
by default.

**Rejected** because:

- URLs become 500+ characters long with a 256-byte KP — past most
  messaging apps' link-preview cut-off, awkward to share, and the
  joiner-side `decodeAndVerify` runs over user-supplied bytes
  without any rate limit.
- `max_uses` enforcement requires per-URL state somewhere; without
  a server-side ledger it falls back to client-only enforcement
  which a malicious joiner trivially bypasses.
- Revocation is impossible — the URL is the capability; once
  shared, can't be revoked. Even a 30-day TTL can't bound the
  blast radius of a leaked URL.

Inline survives as the **fallback** when `/discovery/call-invite`
is unreachable (censored region), gated behind a UX warning that
the owner accepts the trade-off.

### Alternative B — Use a brand-new server endpoint for Welcome ferry

Add `POST /discovery/call-welcome` and `GET /discovery/call-welcome`
endpoints dedicated to opaque Welcome blob storage keyed by owner
pseudonym + call_id. Skip the ContactRequest envelope reuse.

**Rejected** because:

- The four hard properties the ContactRequest envelope already
  guarantees (HPKE seal to identity key, Ed25519 signature on the
  envelope, PoW + Privacy Pass gate, stranger-inbox pseudonym
  routing) would have to be re-implemented. Doubles the server-side
  attack surface for no security gain.
- Welcome bytes are typically 200–800 bytes — the same size class
  as a typed intro message. The transport doesn't need to be
  different just because the **content** is different.
- Adds two new tables to the discovery service for a payload kind
  that's structurally indistinguishable (to the server) from a
  ContactRequest already in the system.

### Alternative C — Single envelope kind with a type-byte header

Replace the two domain separators (`call-welcome` + `call-keypackage`)
with one 25-byte separator + a 1-byte type code:

```text
plaintext = "lakko/call-envelope/v1\x00" || type:1B || payload
```

**Rejected** because:

- Distinct constants give crisper test failures — a type-byte
  swap is a silent bug class the domain separators rule out by
  construction (the parser dispatches on a byte-prefix match,
  not on a numeric tag a corrupt envelope could trivially flip).
- The two payload kinds have **different minimum-length floors**
  (32 bytes for Welcome, 256 bytes for KP-forward). A unified
  parser would have to track the floor per type-byte, which is
  exactly the kind of branching mistake the separate parsers
  avoid by design.

### Alternative D — Multi-party V1 = "owner-only mediation"

Instead of subsequent joiners ferrying KPs to the committer, route
EVERY join through the owner: owner stays the committer forever,
new joiners send a fresh ContactRequest with their KP, owner adds
them via `add_member_from_wire`, Welcomes back.

**Rejected** because:

- Forces the owner to be online and reachable for every join. A
  call where the owner has dropped (network glitch, app
  background) becomes unjoinable. The first-joiner-is-committer
  model from accept_call_invite already gives us availability
  resilience: the original owner doesn't need to be online once
  Bob has joined.
- Owner-only mediation works for 3+ but breaks the symmetry
  invariant: the call group state should be agnostic to which
  member happens to be the active committer. Today's first
  joiner can leave; the next Add proposal flows from the next
  available member; the group survives.

KP-forward via envelope keeps the protocol committer-agnostic.

## Consequences

### Positive

- **Single transport for all stranger→owner messaging.** Welcome
  ferry and KP-forward reuse the ContactRequest envelope path —
  one server-side surface to audit, one HPKE seal to maintain, one
  rate-limit gate.
- **Capability-based revocation.** Owner can delete the blob via
  signed DELETE; the link goes dead immediately. Bounded blast
  radius for a leaked URL.
- **Server cannot enumerate calls.** The pseudonym routing for
  the Welcome envelope means the server only sees `pseudonym →
  opaque envelope`. It cannot link calls to owner identities, and
  it cannot tell whether an envelope is a Welcome or an intro
  message.
- **Defence-in-depth against link substitution.** The
  `ownerFingerprint` gives a Signal-style OOB verification step
  the joiner can run before trusting the call.
- **Multi-party is the same code path.** A 3rd / 10th / 50th
  joiner uses the same KP-forward envelope; no protocol fork.
- **Cross-platform parity is mechanically enforceable.** Three
  platforms invoke the same Rust dispatcher and inherit the same
  ordering by construction; a drift attempt fails at the FFI
  surface, not at runtime.

### Negative

- **Welcome-ferry latency is bounded by stranger-inbox polling.**
  The joiner's accept fires immediately, but the owner only
  enters the MLS group after the next stranger-inbox poll. Worst
  case: poll interval (~30s) + processing time. Acceptable for V1
  group-call rendezvous; we'd revisit if a low-latency 1×1 call
  needed sub-second handover.
- **Subsequent-joiner KP-forward is committer-dependent.** Whoever
  is the current MLS group committer has to be reachable to
  process the KP-forward and emit the new Welcome. A call where
  EVERY member has dropped becomes unjoinable until one
  reappears.
- **Inline-URL is a discoverable censorship workaround.** Long
  URLs in a region's traffic could trigger a DPI signature.
  Acceptable trade-off: the user explicitly opts in to inline
  ONLY when the discovery service is unreachable, and the URL
  carries no plaintext anyway (HPKE-sealed envelope is still
  required).

### Neutral

- **Codec is binary, not JSON.** Easier to bounds-check, more
  compact, and the wire format is already specified
  byte-for-byte (`schema:1B || ...`). Slightly less
  human-debuggable than JSON; the cargo of binary tooling
  available makes this neutral.

## Open questions

- **~~Owner handover protocol~~ — ANSWERED by
  [ADR-0032](0032-multi-joiner-call-links.md)** (recorded 2026-09-02), and it
  was answered by removing the protocol rather than writing one. There is **no
  handover message and no election exchange**: `elect_committer`
  ([`link_invite.rs`](../../crates/lakko-calls/src/link_invite.rs)) is a pure
  function recomputed by every participant on every roster change — the owner
  while present, else the lowest-leaf present member — so failover **and**
  reclaim are automatic and split-brain-free by construction. Shipped on all
  three clients (`multi-joiner-election.{ts,swift,kt}`); G-21 is closed.
- **KP-rotation cadence.** The first joiner's KP-forward path is
  symmetric to the original `acceptCallInvite`, but if a 4th
  joiner shows up 30 minutes later and the committer's KP has
  been rotated by then, the chain still works — but the joiner
  has to re-fetch the capability's embedded KP from the discovery
  service. Today the capability is content-addressed and never
  changes; future-proofing might want a "KP rotation embedded in
  the capability" pointer.
- **DS-mailbox-backed Welcome ferry.** When the DS service grows
  a per-account inbox (Phase 4+), the Welcome envelope could move
  off the stranger-inbox path. Same wire format, different
  transport. The dispatch contract doesn't change.

## References

- ADR-0001 (MLS-only): the call group is a vanilla MLS group; this
  ADR just specifies the **bootstrap** path.
- ADR-0003 (LiveKit fork): the call media keys are derived from
  the MLS exporter that this ADR's bootstrap path establishes.
- ADR-0005 (Contact invitations + 6 scenarios): this ADR is the
  detailed expansion of scenario E.
- ADR-0007 (Multi-device identity): the owner's `account_id`
  embedded in the capability is the account-level deterministic
  one, not a per-device pseudonym.
- [`docs/security/calls.md`](../security/calls.md) — C-24..C-35
  pin every property test that backs this ADR.
- [`docs/security/known-gaps.md`](../security/known-gaps.md) — G-21
  / G-22 track the remaining UI + handover gaps.
- RFC 9420 — MLS protocol; the `Group::create` /
  `Group::add_member_from_wire` / `Group::join_from_welcome`
  surface this ADR drives is the standard one.
- RFC 9180 — HPKE; the ContactRequest envelope's seal/open is
  standard `HPKE-Base-X25519-SHA256-ChaCha20Poly1305`.
- Signal Foundation, "Safety Numbers": the `ownerFingerprint`
  follows the same 60-decimal-digit / ~~5-group-of-6~~ **12-group-of-5** layout
  (corrected 2026-09-02 —
  [`safety.rs`](../../crates/lakko-contacts/src/safety.rs) pins
  `SAFETY_NUMBER_GROUPS = 12`, `SAFETY_NUMBER_DIGITS_PER_GROUP = 5`; the
  71-character total quoted earlier in this ADR is right, so only the grouping
  was misdescribed).
