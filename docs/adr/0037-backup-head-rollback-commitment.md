<!--
SPDX-License-Identifier: AGPL-3.0-or-later
SPDX-FileCopyrightText: 2026 Lakko Contributors
-->

# ADR-0037: Auth-signed backup-head commitment (prevent fresh-install rollback)

- **Status:** Accepted — implemented S1–S6 (2026-07) (maintainer accepted Option C + the resolved sub-decisions; the 6-sprint decomposition below landed end-to-end — primitive + fuzz, stateful auth mint + migration, FFI verify + hash helper, `0xB1` blob framing, 3-client mint/restore gate — with S6 the docs/security reconcile)
- **Date:** 2026-07-20
- **Context:** Phase 2 — encrypted backup / account recovery. Closes the G-11
  fresh-install rollback residual and corrects the stale
  "client-with-state case CLOSED" claim in the known-gaps / invariant docs.

## Context

Lakko backs up an account's identity + address book + a whole-storage MLS
snapshot to `services/storage` as one opaque, phrase-encrypted blob per account
(`services/storage/src/backup.rs`: `PUT/GET/DELETE /backup`, keyed on the
session-token-derived `account_id`, one file at
`<root>/backup/<account_id_hex>`, `PUT` overwrites, `GET` streams it back — no
version, no history, no database). The envelope itself is a standalone AEAD blob
(`crates/lakko-backup/src/envelope.rs` v1: `build_aad` binds only
`domain ‖ version_byte ‖ salt ‖ nonce` — no chain, no head-of-history marker).

**The rollback threat.** An active attacker who controls the storage tier (or any
cache in front of it) can hand a restoring client an *older* backup blob instead
of the latest one. The AEAD opens cleanly — same phrase, same Argon2id KEK — and
the payload is structurally valid, so the client silently downgrades its state to
a stale baseline (resurrects already-revoked devices, forgets recently-accepted
contacts, reinstates a superseded MLS snapshot). Nothing on any path a client
runs today authenticates "this blob is the current head."

**The gap is worse than its label.** `docs/security/known-gaps.md` G-11 and
`docs/security/invariant-index.md` record the "client-with-state case" as
**CLOSED** by `crates/lakko-backup/src/chain.rs` — a v2 envelope carrying a
plaintext `(version: u64, prev_hash)` chain header (`ChainHeader`, `verify_chain`,
`header_of`) bound into the AEAD AAD. That closure is only *crate-level* true. The
v2 chain is **dormant**: it is not exposed on either FFI surface (the UniFFI +
wasm backup modules — `crates/lakko-ffi-{uniffi,wasm}/src/backup.rs` — export only
the v1 `build / seal / open / parse` primitives), no client persists a
`last_known_version` anchor, and no client calls `verify_chain`. So in practice
**neither** the client-with-state case **nor** the fresh-install case is closed on
any deployed path. This ADR closes both, in practice, and the G-11 doc claim must
be corrected accordingly (Sprint 6).

**The crux — why a signature alone proves nothing.** A rollback returns a *valid,
correctly-signed, internally-consistent* older pair: an older head-commitment
together with the older envelope it commits to. The signature verifies; the head
commits exactly the served envelope; every field is self-consistent. Verifying the
signature therefore proves the pair is *authentic*, not that it is *current*. The
usual rollback defence — a monotonic floor the client remembers and refuses to go
below (exactly what the KT-log no-rollback gate does with `log_size`, ADR-0020, and
what the v2 chain's `last_known_version` anchor intended) — **requires a local
anchor**. A fresh-install restore, by definition, has none: the user lost every
device and is rebuilding from only the recovery phrase. There is no persisted
version, no last-seen hash, nothing to compare against. The from-scratch restore is
precisely the state the anchor-based defences cannot cover.

The maintainer has chosen **Option C: an auth-service-signed backup-head
commitment with a server-enforced monotonic version.** This ADR designs C in full.
It does not re-open the placement question (auth-signed-head is decided); it
resolves the sub-decisions C leaves open — the freshness model, the fail-open vs
fail-closed policy per case, where the head lives, the mint ordering + torn-state
handling, and whether the dormant v2 chain is in scope.

## The load-bearing design decision

C defends the anchorless fresh-install restore with **two** mechanisms, and it is
worth stating precisely what each buys — because neither alone is sufficient, and
overselling either would be dishonest.

1. **Server-enforced monotonic version.** The auth service assigns each account's
   backup-head version from a per-account counter it only ever increments. The
   version is auth-assigned, never client-supplied, so a regression is
   *structurally impossible* — auth cannot be induced to sign a lower or duplicate
   version. This gives the version three properties: (a) no two heads ever share a
   version, so a version is an unambiguous marker; (b) an attacker cannot fabricate
   a *higher*-versioned head than the genuine latest (it lacks auth's signature,
   and auth signs only the counter's next value on a real authenticated request);
   (c) the version is a trustworthy monotone latest-marker **at mint time**.

   What monotonicity does **not** do by itself: stop the storage tier from
   *replaying* a genuine *older* head+envelope pair with its genuine lower version.
   For a client that persisted a floor, that replay is caught (mechanism applies
   below). For a **true zero-anchor fresh install**, monotonicity contributes
   nothing directly — the client has no floor to compare against. Its fresh-install
   value is *indirect*: it guarantees the freshness marker is unambiguous and
   forgery-resistant, so the only attack left against a fresh install is a
   within-window replay of a genuine old head, which the second mechanism bounds.

2. **Freshness.** The head carries an auth-signed `signed_at` (the mint
   timestamp). A restoring client trusts auth's timestamp as recent within a
   staleness window ≈ the auto-backup cadence plus slack. A rollback to a head
   older than the window is caught (its `signed_at` is too old); a replay of a head
   *within* the window is **not** caught — both a genuinely-recent latest head and
   a within-window replayed head look equally fresh. That residual is stated
   plainly in the Freshness section and sized there.

**The two cases the combined mechanism actually produces.** The same single head
artifact serves both:

- **Returning client (has ever persisted a floor).** The client stores
  `last_known_version` in device-key-encrypted local storage. On restore it refuses
  any head with `version < last_known_version` — a **hard monotonic floor, no
  window involved**. Because auth guarantees the version never regresses, the floor
  is trustworthy: the storage tier cannot serve an old head with a spuriously-high
  version to defeat it. This is the case the docs claim is "closed" but which is
  *not* wired today; this ADR wires it via the head, not the dormant v2 chain.
- **True fresh install (zero anchor).** No floor exists, so the client falls back
  to the **freshness window**. The within-window replay residual applies **only**
  here — to a genuine lost-everything recovery. Any client that ever persisted a
  floor is hard-protected regardless of the window.

**`account_id`, not `username_hash`.** Unlike ADR-0036's genesis attestation —
which had to carry `username_hash` because the transparency log never learns
`account_id` — the backup head never touches the KT log. It is minted by auth,
stored in the storage blob, and verified by the client, all of which speak
`account_id` natively (the client holds it from the OPAQUE login response at
restore time; storage keys on it; auth authenticates it). So the head binds
**`account_id`** directly, and the client checks `head.account_id == its
authenticated account_id` as defence-in-depth against a head served for the wrong
account.

**The head rides *inside* the backup blob (single atomic PUT).** The head and the
envelope must be swapped together — a head that commits envelope hash `h_old`
served next to envelope `h_new` is a hard mismatch (see § torn-state). Storage
keeps exactly one overwriting blob per account, and there is no cross-object atomic
swap over HTTP, so any design with the head as a *separate* object (a second file /
a DB column / an auth-served record) leaves a window where the two disagree and a
legitimate restore is bricked. Embedding the head as a container-tagged prefix of
the one backup blob makes the pair atomic *by construction* — one `PUT`, one `GET`,
one `rename(2)` — and requires **no** storage-service change (the head-prefix is
just more opaque bytes to a storage tier that already treats the blob as opaque).

The maintainer must accept this framing — "monotonic version + freshness, with the
window residual confined to zero-anchor fresh installs, head embedded for
atomicity" — before implementation.

## Decision

Add an **auth-signed backup head** committing the current backup envelope, minted
on every upload and verified on every restore. It reuses the ADR-0015 /
ADR-0036 `lakko-attest` fixed-layout-Ed25519 template throughout; the one material
deviation is that the mint is **stateful** (a per-account monotonic version
counter) where the genesis attestation was stateless.

### 1. `BackupHead` primitive (extend `lakko-attest`)

A fixed-layout, domain-separated, Ed25519-signed blob, mirroring
`GenesisAttestation` (`crates/lakko-attest/src/genesis.rs`):

```text
wire          = format_version(1) ‖ account_id(16) ‖ backup_version(8, big-endian u64)
                ‖ envelope_hash(32) ‖ signed_at_unix_millis(8, big-endian i64)
                ‖ Ed25519_sig(64)                                     = 129 bytes
signing input = DOMAIN ‖ format_version ‖ account_id ‖ backup_version
                ‖ envelope_hash ‖ signed_at
DOMAIN        = b"lakko/backup-head/v1"   (NEW — distinct from genesis-attest/v1
                                           and device-attest/v1)
```

- `format_version` is the blob-layout version (`= 1`), distinct from
  `backup_version` (the auth-monotonic counter). The two must not be conflated in
  the field names.
- `envelope_hash` = `BLAKE3(envelope_bytes)` over the envelope portion **only**
  (not the head prefix — no self-reference), reusing `lakko_backup::chain::envelope_hash`.
- `signed_at` is the **mint timestamp**, not an expiry, carried in **Unix
  milliseconds** (`signed_at_unix_millis`; the freshness `now` + staleness/skew
  windows are all milliseconds too — the clients work in milliseconds natively, and
  the field/parameter names carry `_millis` so the unit is unambiguous and the auth
  mint + clients cannot introduce a 1000× drift). This is a second deliberate
  deviation from the genesis template, whose `expires_at` is in seconds. The genesis
  attestation carried `expires_at` because the transparency gate enforces a
  server-fixed TTL; here the staleness window is a **client policy** (it tracks the
  client's own backup cadence), so the head carries the raw mint time and the client
  applies its own window.

Two verify entry points, mirroring the genesis freshness split:

- `verify_backup_head(bytes, verifying, expected_account_id, expected_envelope_hash,
  now_millis, max_staleness_millis) -> BackupHead` — the **full** client restore-path
  check: signature + `account_id` binding + `envelope_hash` binding + freshness,
  returning the parsed `BackupHead` so the caller reads `backup_version` for its own
  version-floor comparison against its persisted `last_known_version` (the floor
  needs client-persisted state, so it stays with the caller). All timestamps are Unix
  milliseconds.
- `verify_backup_head_binding(bytes, verifying, expected_account_id) -> BackupHead` —
  signature + `account_id` binding only, **no** hash / freshness check, for a caller
  that applies its own freshness and floor policy (e.g. against the served envelope it
  has not yet hashed).

`#![forbid(unsafe_code)]`, bounded fail-closed parser, unit/property tests
mirroring the `lakko-attest` suite, plus a `backup_head_open` fuzz target (§7).

### 2. Mint endpoint — `POST /auth/backup/head` (new, bearer-authed, STATEFUL)

Mirrors `services/auth/src/genesis_attest.rs` in shape — bearer-authenticated via
the `AuthenticatedAccount` extractor (holding a valid session token *is* proof the
caller OPAQUE-authenticated as `account_id`), reuses `state.attest_key.signing`
(§3) — but is **stateful** where genesis was stateless:

- Request: `{ envelope_hash_b64 }` — the URL-safe base64 `BLAKE3` (32 bytes) of the
  envelope the client is about to store. The client computes the hash locally
  (§7 FFI helper) **before** uploading.
- Handler: in **one transaction**, read-and-bump the per-account counter
  (`SELECT version ... FOR UPDATE` then `UPDATE`, or an
  `INSERT ... ON DUPLICATE KEY UPDATE version = version + 1` upsert) in a new
  `backup_head_versions` table; build
  `BackupHead::new(auth.0, new_version, envelope_hash, now)`;
  `sign(&state.attest_key.signing)`.
- Response: `{ backup_head_b64, backup_version }`.

The version is auth-assigned and never client-supplied, so **regression is
structurally impossible** — there is no client-provided version for auth to
validate or refuse; auth only ever emits its counter's next value. The transaction
guarantees no version is ever signed twice and versions strictly increase per
account.

Migration `services/auth/migrations/0015_backup_head_versions.sql`:
`account_id BINARY(16) PRIMARY KEY, version BIGINT UNSIGNED NOT NULL,
updated_at ...`. The table holds **only** a monotone counter — no key material, no
envelope bytes, no hash (auth signs the client-presented hash without persisting
it). This is the deviation from the ADR-0036 stateless mint and the reason a
migration is required here.

### 3. Signing key — reuse the existing attest key

Reuse the device/action-attestation signing key (`state.attest_key`, migration
`0010_action_attest_signing_key.sql`) and its published JWKS
(`GET /.well-known/device-attest-jwks.json`), exactly as ADR-0036 §3 does for
genesis. The `lakko/backup-head/v1` domain separator makes the three attestation
kinds (device / genesis / backup-head) cryptographically non-interchangeable under
one key, so a shared key is safe and avoids a second migration + JWKS + client pin.
The clients verify against the same key they already pin as `DeviceAttestKey`
(`device-attest-key.ts` / `.swift` / `.kt`). A dedicated backup-head key (signing-
role isolation) is a clean deferred follow-up (open question).

### 4. Transport — embed the head as a container-tagged prefix of the backup blob

The storage backup blob becomes:

```text
CONTAINER_TAG(1 = 0xB1)  ‖  BackupHead(129)  ‖  envelope(..)
```

`CONTAINER_TAG = 0xB1` is chosen to be **not** a valid envelope schema-version
byte (the v1/v2 envelope blobs begin with `0x01` / `0x02`), so the restore path
disambiguates unambiguously by peeking byte 0:

- `0xB1` → framed: parse the 129-byte head, then the envelope tail.
- `0x01` / `0x02` (or anything else) → **legacy** raw envelope, no head (the
  fail-open path, §6).

Storage is **untouched** — no migration, no schema, no second object. The blob
stays opaque to the storage tier (invariant #2); the head-prefix is more opaque
bytes. The single overwriting `PUT` + `GET` + atomic `tempfile/rename` the service
already implements now swaps head and envelope together. A separate `backup_head`
file / column / auth-served record was **rejected** for the torn-state hole it
opens (§ Alternatives).

### 5. Ordering + torn-state handling

Upload order: **build envelope → `BLAKE3(envelope)` → mint head (auth bumps the
version, signs) → single atomic `PUT` of `TAG ‖ head ‖ envelope`.** The head is
minted before the blob is written (the client computes the hash locally), and the
head + new envelope land in **one** `PUT`, so the two never disagree in storage.
Torn states:

- **Mint lands, `PUT` fails / client crashes.** Auth's counter advanced, but
  storage is unchanged — the previous `TAG ‖ head_k ‖ envelope_k` pair is still
  present and self-consistent, so restore succeeds with the last-consistent state.
  Auth's counter has a harmless gap (the next mint bumps past it); the restore path
  never consults auth's counter, so the gap is invisible. No brick.
- **The reverse (envelope written but head not) cannot occur** — there is a single
  `PUT`; embedding is what eliminates this class. This is the decisive advantage
  over a separate-object head, where overwriting the envelope before updating the
  head leaves `head.envelope_hash != BLAKE3(stored envelope)` → a hard mismatch →
  fail-closed brick until the next full cycle.
- **Concurrent multi-device backups** serialize on the counter's `FOR UPDATE`;
  each device embeds its own head with its own envelope, so whichever `PUT` lands
  last leaves a self-consistent pair. Versions strictly increased; no corruption
  (the version is a monotone marker, not a content log).

### 6. Client — mint on upload, verify on restore, fail-open/closed policy

- **Mint on upload** (three clients, at the `uploadNow` / `putBackup` seam — web
  `apps/web/src/backup/client.ts:140` + `apps/web/src/backup/bootstrap.ts:84`; iOS
  `apps/ios/Lakko/Backup/BackupClient.swift:52` + `AuthRepository.bootstrapBackupBestEffort`;
  Android the Kotlin twins): after building the envelope, compute its `BLAKE3`,
  `POST /auth/backup/head { envelope_hash }`, prepend `TAG ‖ head` to the envelope,
  and `PUT` the framed blob. On the returned `backup_version`, persist
  `last_known_version` (and `last_known_envelope_hash`) into the device-key-
  encrypted local store — this is the floor a *future* restore on this same device
  lineage would use. A mint failure is **soft** on the upload side (the current
  best-effort auto-backup posture): fall back to writing an unframed (legacy)
  envelope rather than failing the whole backup — a backup with no head is still a
  confidential backup, and the next successful cycle re-frames it. (This softness is
  the mirror image of the restore-side fail-open on an absent head, §below.)

- **Verify on restore** (three clients, at the `restore` seam — web
  `apps/web/src/backup/client.ts:209`, invoked from `AuthContext.tsx` ~2295; iOS
  `BackupClient.swift:121` from `AuthRepository.restoreFromServerBackupBestEffort`;
  Android `BackupClient.kt:124`): after `getBackup()` and before / around
  `openBackup`, peek byte 0 and apply the three-case policy:

  | Case | Condition | Policy |
  |---|---|---|
  | **Absent** | byte 0 ≠ `0xB1` (legacy blob, or storage stripped the head) | **FAIL-OPEN** — warn, restore proceeds. |
  | **Invalid** | framed, but the signature fails, `account_id` mismatches, or `envelope_hash != BLAKE3(envelope)` | **FAIL-CLOSED, hard** — refuse + alarm, no override. |
  | **Stale** | framed + valid, but `backup_version < last_known_version` (returning client) **or** `signed_at` older than the staleness window (fresh install) | **FAIL-CLOSED with explicit user-acknowledged override** + alarm. |

  Rationale for each:
  - **Absent → fail-open** is the deliberate availability exception, exactly like
    the ADR-0033 admission throttle's fail-open. Failing closed on absence would
    brick every pre-ADR-0037 legacy backup and every restore during the rollout
    window before all clients embed heads — a self-inflicted DoS strictly worse
    than the residual, since a head-stripping attacker gains only the *status-quo*
    rollback surface (the posture before this ADR), never more. Crucially, this
    fail-open is on the **rollback-detection** layer, **not** on E2EE: the envelope
    is AEAD-sealed under the recovery phrase regardless of the head, so invariant #4
    (no silent downgrade to non-E2EE) is untouched — confidentiality + content
    integrity never depend on the head.
  - **Invalid → hard fail-closed** is an unambiguous active tamper or corruption (a
    signature that does not verify, or a head that does not match the served
    envelope). There is no legitimate cause, so refuse with no escape hatch.
  - **Stale → fail-closed with override.** A stale head is *indistinguishable* from
    a rollback replay, so the default must be refuse (invariant #4 — never a silent
    downgrade to old state). But a genuinely lost-everything user who simply has not
    backed up in longer than the window holds a *genuine* stale latest head, and a
    pure hard-refuse would permanently brick their only recovery path. The
    resolution is an **explicit, warned, user-acknowledged override** ("This backup
    is older than expected — restore anyway?"): the downgrade requires deliberate
    human consent, never a silent accept. A maintainer who judges the long-dormant-
    user case out of scope may instead choose a pure hard-refuse (open question).

### 7. FFI verify surface + envelope-hash helper + fuzz

- Expose `verify_backup_head` (+ `verify_backup_head_binding`) on UniFFI (Swift/
  Kotlin) + wasm (web), mirroring the `verify_genesis_attestation` FFI exposure
  (`crates/lakko-ffi-uniffi/src/lib.rs`, `crates/lakko-ffi-wasm/src/supersession.rs`).
  **No sign on the FFI** — signing is auth-only, server-side; the client only
  verifies (same posture as genesis).
- Expose `backup_envelope_hash(envelope) -> [u8; 32]` (= `BLAKE3`, reusing
  `lakko_backup::chain::envelope_hash`) so all three clients compute the identical
  hash at both mint and restore without re-implementing BLAKE3.
- Add a `backup_head_open` fuzz target in `crates/lakko-attest/fuzz` mirroring
  `genesis_attestation_open` — the head bytes are attacker-controlled (a prefix of
  the storage blob a malicious storage tier crafts), reaching the bounded parser
  before any state is consulted; opened against a fixed verifying key so the fuzzer
  exercises the length / version / field-slicing / signature-parse / verify-fail
  path.

## Freshness model + residual

The staleness window is a **client-side** knob (`LAKKO_BACKUP_HEAD_STALENESS_SECS`
or the per-platform equivalent), not a server-fixed TTL, because it must track the
client's own auto-backup cadence.

- **Sizing.** The window must comfortably exceed the maximum gap between a
  recently-active user's last upload and their restore, so a legitimately-current
  head is never flagged stale for such a user. It should be set to roughly the
  auto-backup cadence times a small slack factor plus clock-skew allowance. The
  concrete default is a **rollout decision the maintainer tunes to the observed
  cadence** (not pinned in the current code); a reasonable *starting* value is on
  the order of tens of days, which bounds the fresh-install replay residual to at
  most that much state while rarely tripping the override for an active user. This
  is an engineering recommendation to tune, not a measured figure. It parallels the
  bounded 30-day validity window the supersession proof already uses (ADR-0016
  `MAX_VALIDITY_SECS`).
- **Trade-off.** Too tight → false "stale" refusals for legitimately-inactive
  users (availability, mitigated by the override). Too loose → a wider within-window
  rollback-replay residual (integrity). The window is the single dial between the
  two.
- **The residual, stated plainly.** A rollback that replays a genuine head whose
  `signed_at` falls **inside** the window is **not** caught for a **true
  zero-anchor fresh install** — it looks exactly as fresh as the real latest head.
  The residual is confined to that one case: any client that ever persisted a
  version floor is hard-protected by the floor (no window). Fully closing the
  fresh-install window would require the client to read a *trusted* current-version
  marker on restore — e.g. auth serving its counter over the authenticated channel
  (see Alternatives) — at the cost of a second trusted round-trip the chosen
  single-fetch design deliberately declines.

## Alternatives

- **Keep the dormant v2 chain and wire it instead.** Rejected as the mechanism.
  The v2 chain's `(version, prev_hash)` header binds each envelope to its
  *predecessor*, which detects a rollback only for a client that walks the chain
  from a *persisted anchor* — the client-with-state case, and even there it needs
  an anchor the fresh install lacks. It does nothing for the anchorless fresh
  install (the crux). The head-commitment subsumes the chain's rollback role for
  **both** cases via one artifact (floor for returning clients, freshness for fresh
  installs), so the v2 chain stays **out of scope**. Its distinct role — auditing a
  *served sequence* of multiple versions (forensics), where each `prev_hash` link
  is checked — is a separate concern this ADR does not need and does not touch. (The
  known-gaps "client-with-state CLOSED" claim, which credited the unwired chain, is
  corrected to reflect that this ADR is what closes it in practice.)

- **Head as a separate storage object (second file / `backup_head` column / auth-
  served record) vs. embedded in the blob.** Rejected in favor of **embedding**
  (§4). A separate object cannot be swapped atomically with the envelope over HTTP,
  so overwriting the envelope before updating the head leaves
  `head.envelope_hash != BLAKE3(stored envelope)` — a hard mismatch that bricks a
  legitimate restore until the next full cycle. Embedding makes the pair atomic by
  construction, needs no storage change, and keeps storage opaque.

- **Auth serves a trusted current-version marker on restore (tighter variant).**
  Still Option C (auth-signed head, monotonic version) — it differs only in *where
  the client reads the version floor on restore*. If, on restore, the client asked
  auth (the trusted mint authority, over the authenticated channel) for the current
  `backup_version` and refused any storage-served head below it, the fresh-install
  within-window residual would **close** (a fresh install would gain a trustworthy
  floor). The chosen design **declines** this for the single-fetch simplicity (the
  client already talks only to storage on restore; a second trusted round-trip to
  auth adds a dependency and a failure mode) and accepts the bounded window residual
  instead. Recorded as the clean tightening path (open question), not a rejection of
  C.

- **Server-fixed `expires_at` (like the genesis attestation) vs. client-applied
  window over `signed_at`.** Rejected in favor of `signed_at` + a client window
  (§1). The staleness tolerance is a property of the *client's* backup cadence, not
  a server constant; baking a TTL into the head would force one global window on all
  clients and make an inactive-user restore fail on a server timer rather than a
  user-tunable, user-overridable policy.

## Consequences

- **Positive.** Fresh-install rollback is bounded to a within-window replay of
  genuine state (and only for a true zero-anchor restore); returning-client
  rollback is **hard-closed** by the monotonic floor — the case the docs wrongly
  claimed closed is now actually closed, via one head artifact rather than the
  unwired v2 chain. Reuses the audited ADR-0015 / ADR-0036 `lakko-attest` template
  (fixed-layout Ed25519, shared signing key + JWKS, FFI verifier, pinned-key
  pattern) end-to-end. **No storage-service change** (the head embeds in the opaque
  blob); torn-state is eliminated by the single atomic `PUT`.

- **Negative / new risks.** A new **stateful** authenticated mint endpoint + a
  per-account counter table (a write on every backup) — the first auth state
  outside authentication proper; bounded (one row per account, one integer). The
  within-window fresh-install residual is real and permanent under this design
  (the tightening path is declined for simplicity). A new domain constant that must
  not drift or collide with `device-attest/v1` / `genesis-attest/v1` (mitigated by
  the distinct `lakko/backup-head/v1` separator + the shared-key domain-crossing
  test the `lakko-attest` suite already establishes). The head is only as strong as
  the pinned attest verifying key: a compromised attest *signing* key lets the
  operator forge heads (it already co-signs KT roots and mints device/genesis
  attestations — same trust tier), so this does not defend an operator that
  controls the auth signing key, only a malicious/compromised **storage** tier.
  Honest scope, stated.

- **Neutral.** Additive and non-breaking: a legacy (unframed) blob restores via the
  fail-open path, so no flag-day and no forced client-version floor for the rollout.
  The mint-on-upload is soft-fail, so a backup never fails because the head could
  not be minted.

### Invariants touched

- **#1 (no plaintext PII on the server) — holds.** The head carries `account_id`
  (a random UUIDv7, already ubiquitous), a `BLAKE3` of ciphertext, a monotone
  integer, and a timestamp. No phone / email / plaintext; the counter table stores
  only `(account_id, version)`.
- **#2 (server sees only ciphertext + public artefacts) — holds.** Storage handles
  the head as opaque bytes embedded in the opaque blob (never parses it); auth signs
  public fields and persists only a counter — no message / chain / root / media key
  material.
- **#4 (no silent downgrade / fail-closed) — holds, scoped.** Fail-closed on a
  present-but-mismatching head (hard) and on a stale head (user-acknowledged
  override, never silent). The deliberate fail-open on an **absent** head is on the
  rollback-detection layer only — the phrase-AEAD confidentiality + integrity of the
  content is unconditional and never downgraded — mirroring the ADR-0033 throttle
  fail-open as an availability choice, not an E2EE bypass.
- **Invariant-index touchpoints:** lands as a new **C-58** entry (auth-signed
  backup-head rollback commitment) in `docs/security/invariant-index.md`, and
  transitions **G-11** in `docs/security/known-gaps.md` from "fresh-install case
  open + client-with-state (crate-level) closed" to "both cases closed in practice
  (fresh-install bounded by freshness; residual: within-window zero-anchor replay)".

**Flag this ADR + the implementation through `lakko-security` before merge**
(maintainer-invoked) — it touches auth (a new stateful endpoint + migration +
monotonic counter), the storage-blob framing, the client restore gate, the rollback
threat model, and invariants #1 / #2 / #4.

## Open questions

- **Staleness window default + hard-refuse vs. override on stale.** The concrete
  window default is a rollout tuning decision; and whether a stale head is a
  pure hard-refuse or a user-acknowledged override is the maintainer's call (this
  ADR recommends the override to avoid bricking a long-dormant recovery).
- **Auth-served current-version marker (residual tightening).** Whether restore
  should read the trusted current version from auth to close the fresh-install
  within-window residual, at the cost of a second trusted round-trip (Alternatives).
- **Dedicated backup-head signing key.** Whether to split the signing role from
  device/genesis attestation (§3) for blast-radius isolation. Deferred as a clean
  follow-up.
- **Rate-limiting / PoW on the mint endpoint.** An authenticated account can call
  `POST /auth/backup/head` repeatedly (for its own account); harmless to others and
  bounded, but a limiter is cheap defence-in-depth. Deferred.

## Sprint decomposition

Each step names the ADR-0036 / ADR-0015 file it mirrors.

- **S1 — `BackupHead` primitive + fuzz.** New `crates/lakko-attest/src/backup_head.rs`
  (mirror `genesis.rs`): the 129-byte fixed layout, domain `lakko/backup-head/v1`,
  `sign` / `open` / `verify_backup_head` (full: signature + account + envelope-hash +
  freshness) / `verify_backup_head_binding` (binding only); re-export from `lib.rs`. A `domains_do_not_cross` test proving
  backup-head ↔ genesis ↔ device non-interchangeability under a shared signing key.
  Fuzz target `backup_head_open` in `crates/lakko-attest/fuzz` mirroring
  `genesis_attestation_open`.
- **S2 — auth mint endpoint + monotonic-version migration.** New
  `POST /auth/backup/head` in `services/auth/src/backup_head.rs` mirroring
  `genesis_attest.rs` (bearer-auth via `AuthenticatedAccount`, reuse
  `state.attest_key`), but **stateful**: migration
  `services/auth/migrations/0015_backup_head_versions.sql` (per-account monotone
  counter) + the transactional read-bump-sign. Route wiring in `main.rs` mirroring
  the `genesis_attest` registration.
- **S3 — FFI verify + hash helper.** Expose `verify_backup_head` /
  `verify_backup_head_binding` + `backup_envelope_hash` on UniFFI + wasm, mirroring
  the `verify_genesis_attestation` / `account_username_hash` exposure in
  `crates/lakko-ffi-uniffi/src/lib.rs` + `crates/lakko-ffi-wasm/src/supersession.rs`.
- **S4 — storage-blob framing (client-side, no storage change).** Define the
  `CONTAINER_TAG(0xB1) ‖ head(129) ‖ envelope` framing + the legacy-vs-framed peek
  discriminator as a shared helper (Rust core or per-client), reusing
  `lakko_backup::chain::envelope_hash`. `services/storage` is untouched — assert
  this explicitly (no migration).
- **S5 — 3-client mint-on-upload + verify-on-restore + floor persistence.** Mint at
  the `uploadNow` / `putBackup` seam (soft-fail); persist `last_known_version` into
  the device-key-encrypted local store; verify at the `restore` seam with the
  three-case fail-open/closed policy (§6) + the user-acknowledged stale override.
  Web (`backup/client.ts`, `backup/bootstrap.ts`, `AuthContext.tsx`), iOS
  (`BackupClient.swift`, `AuthRepository.swift`), Android (the Kotlin twins), each
  reusing the pinned `DeviceAttestKey`.
- **S6 — docs/security reconcile.** Correct G-11 in
  `docs/security/known-gaps.md` (drop the "client-with-state CLOSED" over-claim;
  record both cases closed in practice with the stated residual); add the C-58 entry
  in `docs/security/invariant-index.md`; flip this ADR's Status to Accepted and
  update the CLAUDE.md ADR-table row + module map (`lakko-attest` gains
  `BackupHead`; `services/auth` gains the mint endpoint + `backup_head_versions`
  table) + the fuzz list; add/extend the recovery e2e (mint → framed PUT → restore
  gate) as environment allows.

## References

- ADR-0036 — auth-attested KT-log genesis; the `lakko-attest` fixed-layout-Ed25519
  primitive, shared attest signing key + JWKS, FFI verifier, and pinned-key pattern
  mirrored throughout (the stateless mint is the point of deviation here).
- ADR-0015 — device-removal authorization; the original `lakko-attest` template.
- ADR-0016 — identity-supersession transport; precedent for a bounded validity
  window (`MAX_VALIDITY_SECS`) and for a fail-closed changed-key gate.
- ADR-0020 — witness co-signing / KT-log no-rollback root gate; the anchor-dependent
  monotonic-floor rollback defence a fresh install cannot use (the crux).
- ADR-0033 — call-admission anti-spam throttle; the fail-open-as-availability-choice
  precedent the absent-head policy mirrors.
- `crates/lakko-backup/src/chain.rs` — the dormant v2 chain this ADR supersedes for
  rollback (kept dormant; its sequence-audit role is a separate concern).
- `docs/security/known-gaps.md` — G-11 (the residual this closes + the doc claim it
  corrects).
- RFC 8032 — Ed25519 signatures.
