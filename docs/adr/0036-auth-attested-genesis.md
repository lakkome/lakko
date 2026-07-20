<!--
SPDX-License-Identifier: AGPL-3.0-or-later
SPDX-FileCopyrightText: 2026 Lakko Contributors
-->

# ADR-0036: Auth-attested KT-log genesis (prevent first-entry squatting)

- **Status:** Implemented (2026-07) — all 7 sprints below landed and were verified
  against the code this session; see the Implementation section. Flag-gated
  rollout (`LAKKO_TRANSPARENCY_REQUIRE_GENESIS_ATTEST`) ships default-OFF, so
  flipping the gate on is the remaining deploy step.
- **Date:** 2026-07-20
- **Context:** Phase 2 — key transparency. Closes the G-30 finding-2 residual
  (first-entry squatting) that ADR-0019's genesis self-signature only DETECTS.

## Context

The KT log records each account's `(account_key -> identity_pubkey -> version)`
binding in an operator-signed, append-only Merkle tree (ADR-0016 / ADR-0019 /
ADR-0020). ADR-0019 Sprint 2 made every leaf **self-certifying**: a new binding
must carry a `leaf_auth`. For a rotation that is a supersession proof signed by
the previous key; for the **genesis** entry (an account's first appearance) or a
fresh-start reset (ADR-0017) it is a *self-signature* — the leaf's own key signs
`DOMAIN ‖ username_hash ‖ pubkey`
(`crates/lakko-transparency/src/leaf_auth.rs`, domain
`lakko/transparency/leaf-genesis/v1`).

The genesis self-signature is honest TOFU and its own module docs say so: it
"does NOT prevent first-entry squatting." An attacker who holds the transparency
append secret (`LAKKO_TRANSPARENCY_APPEND_SECRET`) — or a malicious discovery
service — can write a version-1 leaf binding a *victim's never-used*
`account_key` to the *attacker's own* key and self-sign it with that key: a
perfectly self-consistent self-signature. The transparency append gate accepts
it (`services/transparency/src/store.rs:301-323` verifies the self-sig against
the leaf's own embedded key), and the operator co-signs it because it has no way
to tell an account-authorised genesis from a fabricated one. The residual is
DETECTED — via CONIKS self-monitoring (the real owner's client audits its own
entry and alarms on a foreign version-1 key) plus the ADR-0016 fail-closed peer
gate — but not PREVENTED. It is recorded as an accepted residual in
`docs/security/known-gaps.md` (G-30 finding 2) and as the elective hardening in
ADR-0019 lines 114-120.

**Why elect it now.** Detection is a weaker guarantee than prevention: it needs
at least one honest fetch plus an out-of-band backstop, and a squatted genesis
that is never noticed silently poisons the authoritative record for a
never-onboarded account. Every primitive this hardening needs already exists and
is audited — the ADR-0015 device-attestation shape (a fixed-layout Ed25519 blob,
an auth signing key, a `/well-known` JWKS, an FFI verifier, a build-time pinned
key) is a proven template we can mirror almost verbatim, and the opaque
`leaf_auth` sidecar transport (transparency migration `0002_leaf_auth.sql`,
discovery `0013`, threaded discovery -> transparency) is already in place. The
marginal cost is low and the guarantee upgrade — squatting *prevented* at the
authoritative log layer, not merely detected — is meaningful for an account
directory.

There is a related surface this also closes. `POST /discovery/identity`
currently trusts the client's `account_id` claim (a PoW token, but no
proof-of-ownership — `services/discovery/src/identity_bundle.rs:25-27`, "Phase
1.5b will gate this"). This ADR does not gate that endpoint, but it makes the
squatting *consequence* moot at the authoritative layer: even if a squatter
uploads a bundle claiming a victim's `account_id`, the operator-side genesis-
attest gate refuses to append the fabricated leaf to the KT log (the squatter
cannot mint an attestation for an account it cannot authenticate as), so the log
— the record peers actually gate on — never carries the squatted binding.

## The load-bearing design decision

**Auth does not know the identity pubkey at signup.** The `accounts` table is
`(id, created_at, identity_version, flags)` only
(`services/auth/migrations/0001_init.sql:19-27`, comment: "this table holds
authentication state only"); signup is `INSERT INTO accounts (id)`. The Ed25519
identity pubkey is client-generated and lives only in discovery's
`identity_bundles`. So auth cannot attest a key it independently derived — it can
only attest a **client-supplied** pubkey.

State the semantics plainly and do not oversell them. The genesis attestation
proves:

> the party that OPAQUE-authenticated as `account_id` presented THIS pubkey as
> its genesis identity key, at the time the attestation was minted.

It is NOT an independently-verified statement that the pubkey is
cryptographically the "right" key for the account — auth has no such ground
truth, and never can under the zero-trust model (the identity key is
client-generated, invariant #2). It is a **binding of the OPAQUE-authenticated
account to a client-asserted key.**

That is exactly sufficient to PREVENT squatting, because squatting is precisely
the case the binding rules out. A squatter cannot OPAQUE-authenticate as the
victim's `account_id` (that requires the victim's OPAQUE password envelope),
therefore cannot obtain auth's signature over `(victim account_id, victim
username_hash, squatter pubkey)`, therefore cannot produce a genesis attestation
that the transparency gate will accept for the victim's leaf. The honest account
holder — the only party who *can* OPAQUE-auth as `account_id` — is the only party
who can mint a genesis attestation for their own account, binding whatever key
they generated. That is the whole point: prevention needs only that *no one but
the account holder can mint the proof*, which OPAQUE authentication already
guarantees. A malicious client attesting its *own* account's key is a non-attack
(it is that account's key to choose).

**`username_hash` is derived, not looked up.** Transparency never sees
`account_id` — it is even more zero-trust than auth (discovery sends only
`username_hash_b64` + `identity_pubkey_b64`, never `account_id`). So auth must
carry `username_hash` inside the attestation as the bridge that lets transparency
bind the attestation to the leaf without ever learning `account_id`. Auth
derives it deterministically:

```
username_hash = BLAKE3("lakko/transparency/account/v1" ‖ account_id)
```

This is the SAME derivation discovery uses on append
(`services/discovery/src/transparency_log.rs:55-71`, `ACCOUNT_KEY_DOMAIN`) and
the client uses on verify (`ak` in `apps/web/src/transparency/verify.ts`). The
transparency gate then checks `attestation.username_hash == leaf.username_hash`
and `attestation.pubkey == leaf.identity_pubkey` — it never needs `account_id`.
The signature over `(account_id ‖ username_hash)` plus BLAKE3 preimage/collision
resistance is what ties the two together: an attacker cannot find a *different*
`account_id` whose `username_hash` collides with the victim's.

> **Drift hazard — must be guarded.** The domain constant
> `"lakko/transparency/account/v1"` currently lives only in
> `services/discovery/src/transparency_log.rs:55`. If auth computes
> `username_hash` with even a one-byte-different domain, every genesis
> attestation fails silently at the transparency gate
> (`attestation.username_hash != leaf.username_hash`), and genesis-attest would
> be inert — a fail-closed, hard-to-notice outage of the hardening. The constant
> and its derivation MUST be promoted to a single shared location (recommended:
> `lakko-transparency`, which already owns the leaf/genesis primitives and is
> depended on by both the FFI crates and, newly, auth) and pinned by a drift
> test that asserts the exact derived bytes for a fixed `account_id`.

The maintainer must accept this framing — "attestation of a client-supplied key,
sufficient because only the account holder can mint it" — before implementation.

## Decision

Add an **auth-attested genesis** proof, additive to (never replacing) the
ADR-0019 self-signature. The self-sig stays as belt-and-suspenders (it proves the
leaf's own key authorised the binding, defending the case where the append secret
is *not* leaked); the genesis attestation adds the anti-squatting property (only
the OPAQUE-authenticated account can mint one).

### 1. `GenesisAttestation` primitive (extend `lakko-attest`)

A fixed-layout, domain-separated, Ed25519-signed blob, mirroring
`ActionAttestation` (`crates/lakko-attest/src/lib.rs`):

```text
wire          = version(1) ‖ account_id(16) ‖ username_hash(32)
                ‖ pubkey(32) ‖ expires_at_unix_secs(8, big-endian i64)
                ‖ Ed25519_sig(64)                                  = 153 bytes
signing input = DOMAIN ‖ version ‖ account_id ‖ username_hash ‖ pubkey
                ‖ expires_at
DOMAIN        = b"lakko/genesis-attest/v1"      (NEW — never reuse device-attest/v1)
```

The domain string MUST be new and distinct from `lakko/device-attest/v1` so a
device-removal attestation can never be replayed as a genesis attestation (and
vice versa) even when signed by the same key.

Two verify entry points (the freshness split is load-bearing — see below):

- `verify_genesis_binding(bytes, verifying, expected_account, expected_username_hash, expected_pubkey)`
  — signature + account + username_hash + pubkey binding, **no freshness check**.
  This is what the **client** uses on first observation (the attestation is a
  permanent historical artifact; enforcing expiry would make every genesis leaf
  unverifiable after the TTL lapses).
- `verify_genesis_fresh(bytes, verifying, expected_username_hash, expected_pubkey, now)`
  — the binding check on the fields transparency has (it lacks `account_id`) PLUS
  freshness. This is what the **transparency append gate** uses; the expiry
  bounds the mint -> append replay window, not the leaf's lifetime.

`153` bytes fits the existing `VARBINARY(200)` `leaf_auth`-family columns.

### 2. Mint endpoint — `POST /auth/genesis/attest` (new, bearer-authed)

A NEW endpoint, NOT an extension of `/auth/action/finish`. `/auth/action/finish`
is passkey-ceremony-bound (`services/auth/src/webauthn_action.rs:390`), but
genesis must work for **OPAQUE-only** accounts that have never registered a
passkey. The new endpoint is authenticated by the session-token bearer
middleware only (the `AuthenticatedAccount([u8;16])` extractor,
`services/auth/src/webauthn.rs:700`) — holding a valid session token *is* proof
the caller OPAQUE-authenticated as `account_id`, which is the entire security
basis (§ the load-bearing decision).

Shape (mirrors `action_finish`'s signing tail,
`webauthn_action.rs:446-468`):

- Request: `{ pubkey_b64 }` (the client's genesis identity pubkey, URL-safe
  base64, 32 bytes).
- Handler: derive `username_hash = BLAKE3(ACCOUNT_KEY_DOMAIN ‖ auth.account_id)`
  (shared constant, § drift hazard); build
  `GenesisAttestation::new(auth.account_id, username_hash, pubkey,
  now + GENESIS_ATTEST_TTL_SECS)`; `sign(&state.attest_key.signing)`.
- Response: `{ genesis_attest_b64 }`.

Stateless (no DB row) — unlike device-authorize, there is nothing single-use to
persist; a valid attestation for one's own account is inherently self-limited by
the transparency gate's version monotonicity (a second "genesis" for a
`username_hash` that already has an entry is a rotation, refused as an
unauthorised genesis). A short TTL (recommend on the order of the action-attest
TTL, minutes-to-an-hour) bounds mint -> append replay. Optional PoW / rate-limit
against an authenticated account spamming the endpoint is deferred (open
question).

### 3. Signing key — reuse the existing attest key (recommended), or a second kid

**Recommendation: reuse the existing action-attestation signing key** (migration
`0010_action_attest_signing_key.sql`, `load_or_init_attest_key`,
`webauthn_action.rs:270`) and its published JWKS
(`GET /.well-known/device-attest-jwks.json`). The `lakko/genesis-attest/v1`
domain separator already makes the two attestation kinds cryptographically
non-interchangeable, so a shared key is safe, and it avoids a second migration +
a second key-load + a second JWKS endpoint. The clients pin the same verifying
key they already pin for device-attest.

The alternative — a **dedicated genesis-attest key** (a second `kid`, its own
`GET /.well-known/genesis-attest-jwks.json`, a new migration mirroring `0010`) —
buys blast-radius isolation (compromise of one signing role does not implicate
the other) at the cost of a second key to load, publish, and pin on three
clients. Recommend deferring it: the domain separation already prevents cross-
protocol misuse, and both attestation roles are the same trust tier (the auth
service signing a statement about an OPAQUE-authenticated account). If a future
review wants role isolation, the dedicated-key path is a clean follow-up (a new
migration + a `genesis_attest_key` field on `AppState`).

### 4. Transparency append gate + new flag (defence-in-depth)

Verify the genesis attestation operator-side at append, behind a new
`LAKKO_TRANSPARENCY_REQUIRE_GENESIS_ATTEST` flag (mirroring the existing
`LAKKO_TRANSPARENCY_REQUIRE_LEAF_AUTH`, `store.rs:288`). This is
defence-in-depth, exactly like the leaf_auth gate: the **client** first-
observation verify (§6) stays the primary trust because the server is untrusted;
the operator gate stops a leaked-append-secret attacker from polluting the log
and keeps it audit-clean.

In the `_ => { verify_leaf_genesis(...) }` genesis arm of the existing gate
(`store.rs:318-323`), additionally require a valid `genesis_attest` when the flag
is on: pin the auth attest verifying key
(`LAKKO_TRANSPARENCY_GENESIS_ATTEST_KEY`, base64 — mirroring how the witness key
is configured), run
`verify_genesis_fresh(genesis_attest, key, leaf.username_hash,
leaf.identity_pubkey, now)`, and REJECT (`StoreError::Unauthorised`) on failure
or, under the flag, on absence. The rotation arm is untouched — genesis-attest
applies only to genesis/reset leaves (there is no predecessor account-auth
question for a rotation; the supersession proof already binds it).

Note the reset (`is_reset`, ADR-0017) case: a lost-everything fresh start still
holds a session token (it OPAQUE-authenticated), so it CAN mint a genesis
attestation for its new key — the mint requires only account authentication, not
the old identity key. So the gate applies uniformly to genesis and reset.

### 5. Transport — a NEW dedicated `genesis_attest` column (recommended)

Carry the attestation opaquely via the established sidecar pattern, in a **new
nullable column** `genesis_attest` on both `identity_bundles` (discovery) and
`log_entries` (transparency), alongside — not overloading — the existing
`leaf_auth` column.

**Recommendation: a new column, not widened `leaf_auth` semantics.** The self-sig
already occupies `leaf_auth` and stays (§ Decision), and the append gate
discriminates `leaf_auth` purely by length (64 = self-sig, 145 = supersession
proof). A genesis leaf must now carry BOTH artifacts (self-sig 64B + genesis-
attest 153B), so overloading one column would force a structured, length-prefixed
composite envelope and change the length discriminator — churn plus a backward-
compat hazard for existing rows. A dedicated additive nullable column mirrors
exactly how the supersession proof (discovery `0010`) and `leaf_auth` (discovery
`0013`, transparency `0002`) were each added — the least-surprise path. Discovery
stores + forwards it verbatim, never parsing (invariant #2), preserved across
non-rotation re-uploads via `COALESCE` like `leaf_auth`; the transparency
`AppendReq` (`transparency_log.rs:91-104`) gains an optional
`genesis_attest_b64` field.

### 6. Client — mint on first publish, verify on first observation

Mirror the ADR-0019 genesis-self-sig client wiring, one step wider:

- **Mint** (three clients): on the first-ever key publish (and on an ADR-0017
  reset), after acquiring the session token, call `POST /auth/genesis/attest`
  with the new pubkey and upload the returned `genesis_attest` alongside the
  bundle (next to the existing self-sig). A mint failure fails closed — do not
  publish a genesis leaf you cannot attest (matching the ADR-0017 atomic emit-or-
  abort posture).
- **Verify** (three clients): on a first observation, beside the existing 64-byte
  self-sig check (`apps/web/src/transparency/verify.ts:287-302` + iOS/Android
  mirrors), if a `genesis_attest` is served, run `verify_genesis_binding` against
  the **pinned** auth attest verifying key — the client already knows the
  target's `account_id` (from the `username -> account_id` discovery lookup), so
  it recomputes `username_hash` and checks all four bindings (account_id,
  username_hash, served pubkey, signature). A failure surfaces as `mismatch`
  (never blind-TOFU). The pinned key rides the same build-time pin mechanism as
  the device-attest key (`device-attest-key.ts` / `.swift` / `.kt`); reuse it if
  §3 reuses the key. **No freshness check client-side** (§1).

### 7. FFI verify surface

Expose `verify_genesis_binding` on UniFFI (Swift/Kotlin) + wasm (web), mirroring
how `verify_leaf_genesis` and the device-attest verify were exposed. Add a fuzz
target `genesis_attestation_open` mirroring `lakko-attest/attestation_open` (the
bytes are attacker-controlled — a served bundle field a malicious discovery / DS
can craft, reaching the bounded parser before any state is consulted).

## Alternatives

- **Keep genesis TOFU (status quo).** Rejected — this ADR exists precisely to
  upgrade the residual from detected to prevented. Recorded honestly: the
  status-quo guarantee (self-sig + CONIKS self-monitoring + the ADR-0016
  fail-closed peer gate) is *adequate* for the current threat model, which is why
  ADR-0019 shipped without it; this ADR is an *elective* hardening, not a fix for
  a live break. If the maintainer judges the marginal guarantee not worth the
  auth<->transparency coupling, "keep TOFU" remains a defensible choice.

- **A distinct `lakko-genesis-attest` crate vs. extending `lakko-attest`.**
  Recommend **extending `lakko-attest`.** The two attestations share the exact
  binary-blob discipline (fixed layout, domain-separated Ed25519, bounded
  fuzzable parser, caller-supplied verifying key, `#![forbid(unsafe_code)]`); a
  second crate would duplicate the audited parser scaffolding for a second struct
  and split the audit surface. Extending reuses the reviewed code and keeps both
  attestation formats side-by-side in one small, already-fuzzed crate. The one
  cost — the crate now defines two domains — is exactly what the domain separator
  is for. A distinct crate would only pay off if the two needed different
  dependencies or trust boundaries; they do not.

- **Reuse the attest signing key vs. a dedicated genesis-attest key.** Recommend
  **reuse** (§3): the domain separator already prevents cross-protocol misuse, and
  a shared key avoids a second migration/JWKS/pin. The dedicated-key alternative
  (blast-radius isolation) is a clean deferred follow-up if a later review wants
  signing-role separation.

- **Overload the `leaf_auth` column vs. a new `genesis_attest` column.** Recommend
  **a new column** (§5): the self-sig stays in `leaf_auth`, the length
  discriminator stays untouched, and it mirrors the additive-column precedent.

- **Gate `POST /discovery/identity` for proof-of-ownership instead.** This is the
  other half of the "Phase 1.5b" trust gap. Rejected as the primary mechanism:
  gating the discovery upload defends the bundle *directory*, but the
  authoritative record peers gate on is the **KT log**, and squatting is defeated
  more cleanly at the log append (one operator-side check, server-untrusted,
  independent of the discovery tier's trust). Gating the discovery upload is a
  complementary follow-up, not a substitute.

## Consequences

- **Positive.** First-entry squatting is **prevented**, not merely detected, at
  the authoritative log layer: no party but the OPAQUE-authenticated account can
  mint a genesis attestation, so the KT log never accepts a fabricated genesis
  for a never-onboarded account (given the flag is on). The self-sig stays, so
  the case "append secret NOT leaked" keeps its independent belt-and-suspenders
  proof. Reuses the audited ADR-0015 template end-to-end (primitive, signing key,
  JWKS, FFI verifier, pinned-key pattern, opaque sidecar transport) — small new
  surface. The squatting *consequence* of the ungated `POST /discovery/identity`
  endpoint is closed at the log even without gating that endpoint.

- **Negative / new risks.** Introduces an auth <-> transparency coupling (auth now
  signs a statement transparency verifies) — the exact coupling ADR-0019 deferred
  this to avoid; accepted here as the price of prevention. A shared domain
  constant that must not drift across auth / discovery / transparency / three
  clients (§ drift hazard) — a silent fail-closed outage if it does; mitigated by
  promoting the constant to one crate + a drift test. The mint endpoint is a new
  authenticated attack surface (rate-limit deferred). Genesis-attest is only as
  strong as the pinned auth verifying key: a compromised attest *signing* key lets
  the operator forge genesis attestations (it already co-signs the root — same
  trust tier), so this does not defend operator+auth collusion, only a leaked
  *append secret* / malicious *discovery* tier. Honest scope, stated.

- **Neutral.** Flag-gated rollout (`LAKKO_TRANSPARENCY_REQUIRE_GENESIS_ATTEST`,
  default choice a deploy decision): default-off is additive and non-breaking for
  existing genesis leaves (which carry no genesis-attest); default-on requires
  every client to mint before it can publish a genesis leaf — sequence the client
  mint (S6) ahead of flipping the flag on. The attestation is a public,
  key-signed artifact carrying no key material — invariant #2 stance identical to
  the existing sidecars.

### Invariants touched

- **#2 (server sees only ciphertext + public artefacts) — holds.** The genesis
  attestation is a public Ed25519-signed artifact carrying no message/chain/root/
  media key material; discovery + transparency store it opaque and never parse it
  for secrets (the transparency gate parses it only to verify the signature +
  bindings, over public fields). Transparency still never learns `account_id`
  (carried inside the attestation, treated as opaque bytes at the gate).
- **#4 (no silent downgrade / fail-closed) — holds / strengthened.** The client
  first-observation verify is fail-closed (`mismatch`, never blind-TOFU); the
  operator append gate rejects an invalid or (under the flag) absent genesis-
  attest; a mint failure fails the publish closed.
- **#6 (identity rotation only via a KT-log supersession proof) — strengthened at
  the genesis boundary.** The first binding is now account-authenticated, not just
  self-consistent, closing the one point in the identity lifecycle the
  supersession chain does not cover.
- Invariant-index touchpoints: this lands as a new **C-** entry (auth-attested
  genesis) in `docs/security/invariant-index.md` and transitions the G-30
  finding-2 residual in `docs/security/known-gaps.md` from "detected" to
  "prevented (flag-gated)".

**Flag this ADR + the implementation through `lakko-security` before merge**
(maintainer-invoked) — it touches auth, transparency, and the identity/KT-log
invariants (#2/#4/#6).

## Open questions

- **Rate-limiting the mint endpoint.** An authenticated account can call
  `POST /auth/genesis/attest` repeatedly (for its own account). Harmless to other
  accounts and bounded by the transparency version monotonicity, but a PoW or
  simple limiter is cheap defence-in-depth. Deferred.
- **Default flag posture at GA.** Ship `LAKKO_TRANSPARENCY_REQUIRE_GENESIS_ATTEST`
  default-off (additive, non-breaking) and flip to default-on once all three
  clients mint — or default-on from the first release with a clean-slate log.
  A deploy decision; note the client-mint-before-flag sequencing.
- **Dedicated genesis-attest key.** Whether to split the signing role from
  device-attest (§3 alternative). Deferred as a clean follow-up.
- **Gating `POST /discovery/identity`.** The complementary bundle-directory
  proof-of-ownership gate (the other half of the Phase-1.5b gap). Out of scope
  here; this ADR closes only the log-layer squatting consequence.

## Sprint decomposition

Each step names the ADR-0015 / ADR-0019 file it mirrors.

- **S1 — `GenesisAttestation` primitive + fuzz.** Extend
  `crates/lakko-attest/src/lib.rs` (mirror `ActionAttestation`): the 153-byte
  fixed layout, domain `lakko/genesis-attest/v1`, `sign` /
  `verify_genesis_binding` (no freshness) / `verify_genesis_fresh` (freshness).
  Add the fuzz target `genesis_attestation_open` mirroring
  `crates/lakko-attest/fuzz` `attestation_open`. Unit/property tests mirroring the
  `lakko-attest` suite.
- **S2 — auth mint endpoint + shared derivation.** New
  `POST /auth/genesis/attest` in `services/auth/src/` mirroring
  `webauthn_action.rs:390-469`'s signing tail + route wiring in `main.rs:424`;
  bearer-auth via the `AuthenticatedAccount` extractor
  (`webauthn.rs:700`); reuse `state.attest_key` (§3). Promote
  `ACCOUNT_KEY_DOMAIN` + the `username_hash` derivation from
  `services/discovery/src/transparency_log.rs:55-71` into `lakko-transparency`
  (shared) and add the **drift test** pinning the derived bytes. No new migration
  if §3 reuses the key.
- **S3 — FFI verify.** Expose `verify_genesis_binding` on UniFFI + wasm, mirroring
  the `verify_leaf_genesis` FFI exposure in the `lakko-ffi-uniffi` /
  `lakko-ffi-wasm` transparency surface.
- **S4 — transparency append gate + migration + flag.** New migration
  `services/transparency/migrations/0004_genesis_attest.sql` (nullable
  `genesis_attest` column) mirroring `0002_leaf_auth.sql`; extend the genesis arm
  of `services/transparency/src/store.rs:318-323`; new flag
  `LAKKO_TRANSPARENCY_REQUIRE_GENESIS_ATTEST` + pinned-key env
  `LAKKO_TRANSPARENCY_GENESIS_ATTEST_KEY`, mirroring
  `LAKKO_TRANSPARENCY_REQUIRE_LEAF_AUTH` and the witness-key config.
- **S5 — discovery threading + migration.** New migration
  `services/discovery/migrations/0016_identity_bundle_genesis_attest.sql`
  mirroring `0013_identity_bundle_leaf_auth.sql`; thread the opaque column
  through `identity_bundle.rs` (upload/fetch, `COALESCE`-preserved) and the
  `AppendReq` in `transparency_log.rs:91-104`.
- **S6 — 3-client mint + verify.** Mint on first publish + reset (mirror the
  ADR-0019 genesis-self-sig mint + the ADR-0017 atomic emit-or-abort); verify on
  first observation beside the self-sig (mirror
  `apps/web/src/transparency/verify.ts:287-302` + iOS/Android mirrors); pin the
  auth verifying key via the device-attest pin mechanism
  (`device-attest-key.ts` / `.swift` / `.kt`).
- **S7 — docs/security reconcile.** Transition G-30 finding 2 in
  `docs/security/known-gaps.md` to "prevented (flag-gated)"; add the new C-entry
  in `docs/security/invariant-index.md`; update `docs/security/calls.md` /
  transparency docs as needed; flip this ADR's Status to Accepted and update the
  ADR-0019 cross-reference + the CLAUDE.md ADR table row + module map
  (`lakko-attest` gains the genesis attestation, `services/auth` gains the mint
  endpoint) after landing.

## Implementation

**Status: Implemented (2026-07).** All seven sprints landed; each was verified
against the code this session. Deviations from the plan are noted inline.

- **S1 — `GenesisAttestation` primitive + fuzz.** Landed in a dedicated
  `crates/lakko-attest/src/genesis.rs` module (not inlined into `lib.rs` as the
  sprint sketch suggested — same crate, re-exported from `lib.rs`, cleaner
  separation from `ActionAttestation`). The 153-byte fixed layout, domain
  `lakko/genesis-attest/v1`, `sign` / `open` / `is_expired` / `verify_genesis_binding`
  (no freshness) / `verify_genesis_fresh` (freshness) are all present, with
  `GENESIS_ATTESTATION_LEN == 153` asserted. 8 unit tests including
  `domains_do_not_cross_between_action_and_genesis`, which proves action↔genesis
  non-interchangeability by BOTH distinct total length AND the domain-prefixed
  signing input under a shared signing key. Fuzz target `genesis_attestation_open`
  registered in `crates/lakko-attest/fuzz`.
- **S2 — auth mint endpoint + shared derivation.** `POST /auth/genesis/attest`
  in `services/auth/src/genesis_attest.rs` — bearer-authed via the
  `AuthenticatedAccount` extractor, stateless (no DB row), reuses `state.attest_key`
  (the device-attest signing key, §3), `GENESIS_ATTEST_TTL_SECS = 3600`. It
  derives `username_hash` from the AUTHENTICATED `account_id` (never the request
  body) via the promoted shared definition `lakko_transparency::account_username_hash`
  (`crates/lakko-transparency/src/account.rs`, `ACCOUNT_KEY_DOMAIN =
  "lakko/transparency/account/v1"`), pinned by the drift test
  `golden_pins_the_account_username_hash`
  (`a5c9754b6ee1c04e65d76b50f1e68a61951ccb250d482c6016fdd5fa78b89229`).
- **S3 — FFI verify.** `verify_genesis_attestation` + `account_username_hash` on
  UniFFI (`crates/lakko-ffi-uniffi/src/lib.rs`); `verifyGenesisAttestation` +
  `accountUsernameHash` on wasm (`crates/lakko-ffi-wasm/src/supersession.rs`).
- **S4 — transparency append gate + migration + flag.** Migration
  `services/transparency/migrations/0004_genesis_attest.sql` (nullable
  `genesis_attest VARBINARY(256)` sidecar, opaque to the Merkle tree — never in
  `canonical_bytes`). The genesis/reset arm of the append gate
  (`services/transparency/src/store.rs`) additionally runs `verify_genesis_fresh`
  (fail-closed) when `require_genesis_attest` is on; the ADR-0019 self-sig gate
  STAYS as the independent belt-and-suspenders proof. Flag
  `LAKKO_TRANSPARENCY_REQUIRE_GENESIS_ATTEST` (default OFF) + pinned key
  `LAKKO_TRANSPARENCY_GENESIS_ATTEST_KEY` (`operator_key.rs`), with loud startup
  diagnostics for every flag×key combination (`main.rs`).
- **S5 — discovery threading + migration.** Migration
  `services/discovery/migrations/0016_identity_bundle_genesis_attest.sql`
  (nullable `genesis_attest` sidecar); `identity_bundle.rs` stores + forwards it
  opaquely, COALESCE-preserved across non-rotation re-uploads (invariant #2); the
  `AppendReq` in `transparency_log.rs` carries `genesis_attest_b64`.
- **S6 — 3-client mint + verify.** Web (`auth/client.ts` `genesisAttest`,
  `transparency/verify.ts` first-observation gate, `discovery/wire.ts`
  `genesis_attest`), iOS (`AuthClient` / `AuthRepository` / `TransparencyVerify`
  + `DiscoveryWire`), and Android (the Kotlin twins) mint on the genesis publish
  (soft-fail) and verify a served attestation on first observation beside the
  self-sig (fail-closed `mismatch`), each reusing the pinned DeviceAttestKey.
- **S7 — docs reconcile.** This section; the ADR status flip; the CLAUDE.md ADR
  table row + module map + fuzz list; the G-30 finding-2 transition + the C-56
  extension in the security docs.

### Rollout residual

The gate ships default-OFF, which is additive and non-breaking for existing
genesis leaves (they carry no attestation). Flipping
`LAKKO_TRANSPARENCY_REQUIRE_GENESIS_ATTEST` on — sequenced AFTER the three clients
mint (S6, done) — plus a live cross-service end-to-end (mint → publish →
append-gate → first-observation verify) on a deployed environment are the
remaining steps. Operator+auth collusion stays out of scope (a compromised attest
signing key already co-signs the root — the same trust tier).

## References

- ADR-0019 — client-authorised transparency-log leaf (G-30 #2); this ADR
  implements the "auth-attested genesis" alternative recorded at ADR-0019 lines
  114-120.
- ADR-0015 — device-removal authorization; the `lakko-attest` template mirrored
  throughout.
- ADR-0016 — identity-supersession transport; the rotation-side authorisation
  genesis-attest complements.
- ADR-0017 — phrase-seeded identity; the fresh-start reset case that also mints a
  genesis attestation.
- ADR-0020 — witness co-signing; the split-view defence orthogonal to squatting.
- `docs/security/known-gaps.md` — G-30 finding 2 (the residual this closes).
- RFC 8032 — Ed25519 signatures.
