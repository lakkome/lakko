<!-- SPDX-License-Identifier: AGPL-3.0-or-later -->
<!-- SPDX-FileCopyrightText: 2026 Lakko Contributors -->

# Lakko — Invariant index

Consolidated cross-reference for every numbered security
invariant in this audit pack. Reviewer cheat-sheet: open
this page, find the invariant ID, jump to the file +
section that describes it.

## Quick stats

| Class | Count | Range | Source file |
|---|---|---|---|
| **I-** | 8   | I-1..I-8   | [`mls-state-machine.md`](./mls-state-machine.md) |
| **C-** | 62  | C-1..C-62  | [`calls.md`](./calls.md) |
| **T-** | 15  | T-1..T-15  | [`transport.md`](./transport.md) |
| **G-** | 37  | G-1..G-37 | [`known-gaps.md`](./known-gaps.md) — open / deferred |
| Total invariants  | 84  | (I + C + T) | live properties |
| Total open gaps   | 13  | (G)         | tracked + scoped |

**Note on the `G-` counts.** These two rows had drifted badly and were corrected
on 2026-08-14: they claimed `G-1..G-27` with 27 open, while
[`known-gaps.md`](./known-gaps.md) had carried `### G-` headings up to **G-36**
for some time — G-29..G-36 were minted by later arcs and never reflected here.
**37** is the count of `### G-N` headings (G-37 minted by the ADR-0044 arc);
**13** is the count of those whose heading is *not* marked `CLOSED` (G-1, G-2,
G-4, G-14, G-15, G-16, G-20, G-26, G-29, G-30, G-32, G-36, G-37). Read the open
count as a mechanical marker tally, not an adjudication: several entries are
**partially** closed (G-30's finding-1 and finding-2 are addressed by ADR-0039 /
ADR-0036 while a residual remains; G-36 carries a per-residual status, currently
residuals 1 / 3 / 4 / 5 / 7 / 8 / 10 carrying open material with 2, 6 and 9
closed and 5 / 7 / 10 partially so), so the honest reading is "13 entries still
carry open material". Unlike the I / C / T classes, the `G-` class is **not**
covered by `just audit-index-check` — the gate only reconciles `### I-` /
`### C-` / `### T-` headers against this page — which is precisely why this row
was free to drift.

**A second, still-uncorrected `G-` drift, recorded rather than silently left.**
The per-category `## Known gaps` tables further down this page are a **partial
legacy listing**: they carry rows only up to G-27 plus G-31, so **G-28, G-29,
G-30, G-32, G-33, G-34, G-35** have no row at all. The ADR-0044 reconcile added a
**Push routing** section for the two gaps in its own scope (G-36, G-37) rather
than backfilling eight unrelated arcs' entries, which would have meant summarising
work it did not review. Backfilling the rest is a standing follow-up.

## MLS state machine invariants (I-1..I-8)

Source: [`mls-state-machine.md`](./mls-state-machine.md)
— OpenMLS wrapper properties the state machine MUST
preserve. Each invariant is backed by a property test
in `crates/lakko-mls/`.

| ID  | Title | Topic |
|---|---|---|
| I-1 | A Welcome targeting a different `account_id` is rejected before any state advance | DeviceGroup join gate |
| I-2 | `remove_device_at(own_leaf)` is rejected | Self-removal guard |
| I-3 | `remove_device_at(unknown_leaf)` is rejected before OpenMLS sees it | Pre-flight check |
| I-4 | After a successful `add_device_from_wire` / `join_from_welcome` pair, both devices' exporter secrets agree | Cross-device key agreement |
| I-5 | Sealing a `HistoryBundle` larger than `MAX_BUNDLE_MESSAGES = 1000` is rejected | Bundle bound |
| I-6 | Opening a `SealedBundle` with a wrong key surfaces as an authentication failure, not a parse failure | Failure-mode discrimination |
| I-7 | Opening a `SealedBundle` with an unknown `algorithm` discriminator is rejected before AEAD-open is attempted | Forward-compat ratchet |
| I-8 | `create_devicegroup_message` round-trips through `parse_devicegroup_payload` losslessly for every typed body variant | Wire-format byte-stability |

## Call-side invariants (C-1..C-62)

Source: [`calls.md`](./calls.md). The `C-` class is the
catch-all for "everything the SFU / channels / storage /
auth server might see, and every client-side property that
keeps it opaque". Five sub-sections —
media-key pipeline (C-1..C-14), call-admission JWT
(C-15..C-23), link-based invite + composer + dispatcher
(C-24..C-36), SFrame + RFC 9605 + SFU signaling
(C-37..C-39), channel-broadcast backend opacity
(C-40..C-44), then per-arc additions C-45..C-62
(SFrame native shim, TURN, PublisherGroup persistence,
host-side bridge validation, rotation policy, the
SFU multi-party client SFrame coverage, the KT-log
client-authorised leaf + witness quorum, the backup-head
rollback commitment, the iOS NSE silent-push
read-only-throwaway decrypt boundary, the opaque
per-group push routing id, and its mint-epoch recency
ordering).

### C-1..C-14 — media-key pipeline (SFrame + overlap window)

| ID   | Title |
|---|---|
| C-1  | Two parties in the same MLS group at the same epoch derive byte-identical `CallKey` seeds |
| C-2  | Cross-party seal/open works end-to-end through the MLS exporter |
| C-3  | Per-participant keys differ across leaves |
| C-4  | Three-party full-mesh seal/open holds |
| C-5  | Frame counter is strictly monotonic per (sender, stream) per epoch — no cross-stream `(key, nonce)` reuse (the participant key is derived from the FULL header KID `= (sender_leaf << 8) \| stream_index`, so every one of a leaf's concurrent streams — audio, camera, screen-share — keys into a disjoint space for any stream count; [ADR-0035](../adr/0035-sframe-per-stream-kid-key.md), supersedes [ADR-0034](../adr/0034-sframe-per-media-stream-key.md)'s per-media-type discriminator) |
| C-6  | AEAD AAD-binding holds across the cross-party path |
| C-7  | Wrong frame counter on the receive path is rejected |
| C-8  | MLS epoch advance rotates the `CallKey` seed |
| C-9  | The 1-second overlap window absorbs in-flight frames and ONLY in-flight frames |
| C-10 | Only the most-recent previous epoch is retained in the overlap slot |
| C-11 | The 1×1 call signaling dispatcher does NOT cross-feed group-control bodies into the state machine |
| C-12 | Wrong-`call_id` signaling bodies are dropped silently |
| C-13 | `EndReason` parser folds unknown discriminator values to `HungUp` |
| C-14 | `call_id` length is strictly enforced at parse time |

### C-15..C-23 — call-admission JWT

| ID   | Title |
|---|---|
| C-15 | Call JWTs are signed by an Ed25519 key persisted across restarts |
| C-16 | A JWT with the wrong `iss` or `aud` is rejected |
| C-17 | A JWT past its `exp` is rejected |
| C-18 | A JWT signed by a different key is rejected, even if the `kid` matches |
| C-19 | A JWT with a tampered header / payload is rejected |
| C-20 | `kid` is a deterministic, stable function of the public key |
| C-21 | `POST /call/jwt` requires a valid session token |
| C-22 | `verify_with_jwks` resolves the correct key by `kid` across a multi-key JWKS doc |
| C-23 | `Jwk::to_verifying_key` rejects non-OKP / non-Ed25519 JWKs |

### C-24..C-36 — link-based invite + composer + dispatcher

| ID   | Title |
|---|---|
| C-24 | A `CallInviteCapability` with a tampered payload is rejected |
| C-25 | Splicing a different owner pubkey into a signed capability is rejected |
| C-26 | Schema-version + bounds enforced at parse time |
| C-27 | Cross-party `CallKey` seeds agree across the bootstrap path |
| C-28 | Owner fingerprint enables OOB anti-phishing verification |
| C-29 | Server-side call-invite ledger handler invariants |
| C-30 | `/c/<slug>` URL plumbing — discrimination contract |
| C-31 | Composer + acceptor pipelines — single-call surface contract |
| C-32 | Call-Welcome envelope reuses ContactRequest transport with domain separation |
| C-33 | Call KeyPackage-forward envelope for subsequent joiners |
| C-34 | Owner-side incoming-envelope dispatcher pins dispatch order |
| C-35 | End-to-end link-call wire chain holds across both 1×1 and multi-party paths |
| C-36 | Owner-handover preserves call-key chain across mid-call committer leave |

### C-37..C-39 — SFrame + RFC 9605 + SFU signaling

| ID   | Title |
|---|---|
| C-37 | SFrame keying math + AEAD pipeline pinned |
| C-38 | RFC 9605 §4.3 wire-header round-trip pinned |
| C-39 | SFU enforces zero-payload-inspection + cross-call isolation across BOTH the signaling AND the media (RTP) plane. Media-plane fanout (`fanout_media_data`) + downstream-track opening (`propagate_new_tracks_in`) are call-scoped on the server-authoritative JWT `call_id` (fail-closed; renegotiation transitively scoped). The media-plane enforcement was a security fix — before it the fanout had no `call_id` filter, leaking SFrame-encrypted media across calls (confidentiality held via SFrame, isolation/metadata invariant did not); `lakko-security` APPROVE |

### C-40..C-44 — channel-broadcast backend

| ID   | Title |
|---|---|
| C-40 | services/channels publisher path treats every byte as opaque |
| C-41 | services/channels subscribe handshake opacity holds |
| C-42 | Rotation is atomic — either every envelope lands or none does |
| C-43 | FFI subscriber-side surface preserves the in-Rust opacity contract |
| C-44 | Production channel seed comes from the MLS exporter; the demo path is grep-clean |

### C-45..C-50 — SFrame native-shim stack (G3-sframe-native arc, closes G-28)

| ID   | Title |
|---|---|
| C-45 | lakko-sframe-native C ABI is the only path into the AEAD seal from non-Rust callers |
| C-46 | Bad seed length is refused at every layer, no silent downgrade |
| C-47 | Boot-time selector probe enforces invariant #4 (no fallback to non-E2EE) |
| C-48 | SFrame attach happens between addLocalAudioTrack and SDP build |
| C-49 | Per-sender ParticipantKey cache is bounded by leaf count + thread-safe under multi-sender fan-in |
| C-50 | Refcount machinery balances every AddRef with exactly one Release, no leak no double-free |

### C-51 — TURN credential issuance (G3-{1,2}-turn)

| ID   | Title |
|---|---|
| C-51 | TURN credential follows the coturn `static-auth-secret` REST scheme; bound to a per-call expiry |

### C-52 — PublisherGroup persistence + rotation (G4-2-mls-persistence)

| ID   | Title |
|---|---|
| C-52 | PublisherGroup MLS state is persisted by deterministic channel-id, exporter output stable across reloads, rotation advances the epoch |

### C-53 — SFrame native-shim host-side validation (N-{4,5}-validate)

| ID   | Title |
|---|---|
| C-53 | Host-side bridge round-trip exercises every documented tampering mode of the SFrame native shim before either platform binding is loaded |

### C-54 — Channel rotation policy (G4-2-rotation-timer)

| ID   | Title |
|---|---|
| C-54 | Scheduled rotation cadence is governed by a pure policy, identical across all three platforms, with clock-skew-tolerant arithmetic |

### C-55 — SFU multi-party client SFrame coverage + cross-platform AAD (SFU-3 arc, ADR-0013)

| ID   | Title |
|---|---|
| C-55 | SFU multi-party media is SFrame-sealed end-to-end on every client (web / iOS / Android); the AEAD AAD is the cross-platform wire header, not host RTP metadata |

### C-56 — Client-authorised transparency-log leaf + auth-attested genesis (G-30 #2, ADR-0019 + ADR-0036)

| ID   | Title |
|---|---|
| C-56 | Every new KT-log `(account_key → pubkey)` binding is account-authorised, not discovery-asserted: a rotation is chain-verified against a single committed, no-rollback log version on all three clients (`verify_supersession_chain`, gate 2), and a genesis / reset binding carries a self-signature by the leaf's own key (`verify_leaf_genesis`, domain-separated). The transparency `append` handler verifies the authorisation under the append lock and REJECTS an invalid one (403; absent refused when `LAKKO_TRANSPARENCY_REQUIRE_LEAF_AUTH` is on); discovery stores + forwards `leaf_auth` OPAQUE (never parsed, invariant #2). Clients mint the genesis self-sig on a new-key publish + verify a served genesis self-sig on first observation. Closes G-30 finding 2; rotation equivocation is now detectable (finding 1, rotation case). **Genesis squatting PREVENTED (ADR-0036).** The self-sig only DETECTS a fabricated genesis (a leaked append secret self-signs a victim's never-used `account_key` to the attacker's own key); ADR-0036 adds an additive **auth-attested genesis** — a 153-byte Ed25519 `GenesisAttestation` (`crates/lakko-attest/src/genesis.rs`, domain `lakko/genesis-attest/v1`) minted by auth at `POST /auth/genesis/attest` (bearer-authed → only the OPAQUE-authenticated account holder can mint one), binding `(account_id, username_hash, pubkey, expiry)`. `username_hash = BLAKE3("lakko/transparency/account/v1" ‖ account_id)` bridges it to the leaf (shared `lakko_transparency::account_username_hash` + golden drift pin; transparency never learns `account_id`). Two verify entry points: `verify_genesis_binding` (client, first observation, all-four-field bind, no freshness — permanent artifact, fail-closed `mismatch`) + `verify_genesis_fresh` (transparency append gate, `username_hash`+`pubkey` bind + freshness). The gate is fail-closed behind default-OFF `LAKKO_TRANSPARENCY_REQUIRE_GENESIS_ATTEST` + pinned `LAKKO_TRANSPARENCY_GENESIS_ATTEST_KEY`; discovery threads `genesis_attest` opaquely (migration `0016`, `COALESCE`-preserved, invariant #2); all 3 clients mint on genesis publish + verify beside the self-sig. Strengthens invariant #6 (the first binding is now account-authenticated). Residual: the deploy rollout (flip the flag once clients mint) + a live cross-service e2e; operator+auth collusion out of scope. Fuzz: `genesis_attestation_open`. `lakko-security` APPROVE-WITH-NOTES (ADR-0019); ADR-0036 to be flagged before flipping the gate |

### C-57 — Witness co-signing for KT-log split-view resistance (G-30 #1, ADR-0020 + ADR-0039 K-of-N quorum)

| ID   | Title |
|---|---|
| C-57 | An operator root is trusted by a client only when it carries BOTH the operator signature AND a valid co-signature from a build-time-pinned, independent witness. The witness (`services/witness`) co-signs a root ONLY after verifying a Merkle consistency proof (RFC 6962 §2.1.2, `crates/lakko-transparency/src/merkle.rs` `verify_consistency` — reconstructs both roots) that it append-only-extends the witness's last-co-signed root; two divergent forks cannot both extend one root, so the operator cannot fork alone (the honest witness refuses → 409). The cosig is a 105-byte blob (`version ‖ tree_root(32) ‖ log_size(8 BE) ‖ sig(64)`, domain `lakko/transparency/witness/v1`, distinct from the operator root sig); the FFI is verify-only (`verify_witness_cosig` — signing stays server-side). All three clients run `witnessGateRefuses` immediately after the no-rollback gate in `verifyIdentityInLog` + `verifyRotationChain`: missing/invalid cosig → `mismatch` (fail-closed, invariant #4); no pinned witness key → operator-only transition posture. `services/transparency` co-signs-on-read (fresh head-root sig, cached cosig keyed on `(tree_root, log_size)`, re-obtained only on growth; witness unreachable → serve None, never stale). Strengthens invariant #4 (root gate needs operator AND witness) + #6 (rotation anchored to a non-forkable root). The cosig + consistency proof are public artefacts over public roots (invariant #2); the witness persists only roots/sizes, never accounts (invariant #5). Closes G-30 finding 1 for the single-witness model. **K-of-N quorum now IMPLEMENTED ([ADR-0039](../adr/0039-kt-log-witness-quorum.md))** — the root gate now needs the operator signature AND ≥ K valid co-signatures from DISTINCT keys in a build-time-pinned witness key set of size N (`verify_witness_quorum`, dedup-by-distinct-key so a replayed / duplicated cosig can never inflate the quorum; the transparency service fans out to N witnesses best-effort and serves `witness_cosigs_b64: Vec<String>`, the clients enforce the threshold fail-closed); so the operator must corrupt K independent witnesses, not one. N=1/K=1 is byte-for-byte the single-witness gate. Residual: operator + K COLLUDING witnesses — closed in practice ONLY under INDEPENDENT witness operation (distinct trust domains; a deploy/ops property, not code — a single operator running all N witnesses gains nothing) + independence-as-deploy-property. Fuzz: `verify_consistency` + `verify_witness_cosig` + `verify_witness_quorum`. `lakko-security` APPROVE-WITH-NOTES |

### C-58 — Auth-signed backup-head rollback commitment (G-11, ADR-0037)

| ID   | Title |
|---|---|
| C-58 | A storage-tier backup **rollback** (serving a restoring client an older, cleanly-opening envelope) is detected by an auth-signed **backup head** committed atomically inside the one opaque storage blob. `BackupHead` (`crates/lakko-attest/src/backup_head.rs`) is a 129-byte fixed-layout Ed25519 blob binding `(account_id, backup_version, envelope_hash, signed_at)`, domain `lakko/backup-head/v1` (distinct from `device-attest/v1` + `genesis-attest/v1`; a three-way domain-crossing test proves the kinds are non-interchangeable even under the shared attest signing key). A signature proves the head **authentic**, not **current** — two mechanisms turn authenticity into a rollback defence: (a) an **auth-assigned, per-account, strictly-monotone `backup_version`** minted at `POST /auth/backup/head` (bearer-authed + STATEFUL — the `backup_head_versions` counter table, migration `0015`, read-bumped in one transaction via the MariaDB `LAST_INSERT_ID` idiom; never client-supplied, so a regression is structurally impossible) that a **returning client** refuses to go below via a device-key-encrypted `last_known_version` floor (HARD, no window); and (b) a signed **`signed_at`** mint timestamp (Unix millis) a **zero-anchor fresh install** bounds with a client-side staleness window (default 30 days). The head rides as a `0xB1` container-tag prefix of the single storage blob (`crates/lakko-backup/src/head_frame.rs` `frame_backup_blob` / `parse_backup_blob`), so head + envelope swap together in one atomic `PUT` — no torn-state window, no `services/storage` change (invariant #2: the tag + head are more opaque bytes below the storage tier). The three-client restore gate (`verify_backup_head` full / `verify_backup_head_binding`, FFI-exposed with a discriminable `BackupHeadStale` vs `Backup` / `invalid:` error) applies a three-case policy: **absent** (byte 0 ≠ `0xB1` — legacy / stripped) → **fail-open** (rollback-detection availability only — the envelope stays phrase-AEAD sealed regardless, so invariant #4's no-silent-downgrade-to-non-E2EE never depends on the head); **invalid** (bad signature / account / envelope-hash, or a framed-but-truncated blob) → **hard fail-closed**; **stale** (below the floor, or over the window) → **fail-closed with an explicit user-acknowledged override**. Invariants: #1 (the head carries only `account_id`, a `BLAKE3` of ciphertext, a monotone integer, a timestamp; the counter table stores only `(account_id, version)` — no PII), #2 (storage + auth both opaque to key material — auth persists only a counter), #4 (fail-closed, scoped to the rollback-detection layer). Residual: a within-window replay of a genuine old head is uncaught for a **true zero-anchor fresh install only** (any client that ever persisted a floor is hard-protected); the tightening path (auth serving a trusted current-version marker on restore) is declined for single-fetch simplicity; the dormant v2 chain (`lakko-backup::chain`) stays out of scope. **`lakko-security` APPROVE-WITH-NOTES** (no CRITICAL/HIGH, no invariant-#4 break on the live path); the F1-F5b hardenings from it: **F4** — the **absent**-head case now floor-checks a returning client (`BLAKE3(envelope) == floor.envelope_hash` → proceed, else stale-override), so the fail-open is scoped to a **true zero-anchor fresh install** and the shipped code matches the residual above (a returning client is hard-protected against a head-stripping downgrade); **F5a** — a present-but-corrupt floor → stale-override (never a silent demote to the fresh path); **F2** — the restore gate is mandatory (the ungated no-verification path removed); **F1** — a loud unpinned-attest-key warning; **F3** (Android) — a `Throwable` `parseBackupBlob` catch; **F5b** (auth) — a default-off per-account **fail-open** mint rate-limit (`backup_head_mint_limits`, migration `0016`, `LAKKO_BACKUP_HEAD_MINT_RATE_LIMIT` default `0`). Closes G-11 in practice. Fuzz: `backup_head_open` (`lakko-attest`) + `backup_blob_parse` (`lakko-backup`) |

### C-59 — iOS NSE silent-push read-only-throwaway decrypt boundary (ADR-0041 Phase 3)

| ID   | Title |
|---|---|
| C-59 | The iOS `UNNotificationServiceExtension` (NSE) renders a **real** decrypted push preview via a **read-only throwaway** MLS decrypt ([ADR-0041](../adr/0041-ios-silent-push-nse-decrypt.md) Phase 3), mirroring the already-approved web (`148753b`) + Android (`ae9a087`) slices, without ever advancing the foreground app's ratchet. **Seed-sharing boundary.** On-device decrypt requires the AccountIdentity seed (`Identity.fromSeed`, ADR-0017) to be NSE-readable, so it moves into a **team-scoped shared Keychain access group** (`group.me.lakko.dev.shared`), staying `AfterFirstUnlockThisDeviceOnly` + no-iCloud-sync + hardware-class-key-protected; only the derived **64-byte phrase seed** is shared (never the recovery phrase), and the seed `Data` is zeroed after use (`resetBytes`). The read scope widens app -> {app, first-party co-signed NSE}, not app -> any app: invariant #7's non-extractable / hardware-backed letter is preserved, only the process scope widens (the one invariant under tension, reviewed). **Three-layer structural no-write guarantee** (what makes the throwaway airtight): (1) the slim `lakko-ffi-extension` FFI exposes **no persist method** — only `from_seed` / `restore_storage` / `load_group` / `process_incoming_verifying_roles` / `parse_body_envelope`, with no `snapshot_storage` / compose / send / call / channel / transport surface; (2) the `NsePushDecryptDeps` / `NseThrowawayGroup` protocols expose **no write path** — every method is a read or a pure compute; (3) `NseThrowawayGroup.close()` is a documented no-op (ARC / UniFFI `deinit` frees the handles; never `snapshotStorage` / `putSnapshot`); and the app's DS cursor is **never advanced** (the fetch start is read-only). The foreground app later restores from the SAME snapshot and reprocesses from its OWN append-only cursor (idempotent), so the G-34-class publish/persist ratchet-desync is avoided by construction. **Fail-closed (invariant #4):** a frame that will not authenticate is dropped (never a plaintext guess); any missing input / non-member group / DS or decrypt failure / control frame / budget expiry degrades to the generic "New message" banner. Two Phase-3 findings **RESOLVED**: **Finding 1** — an anti-censorship-**armed** user's NSE never makes a direct un-tunneled DS connection (the NSE deliberately cannot carry the transport stack, so when armed it short-circuits straight to the generic banner rather than leaking a direct connection); no open residual gap. **Finding 2** — the read-only fetch start is floored at the app's persisted per-group cursor. Invariants #2 (the NSE does only an opaque read-only DS fetch + local decrypt; the server learns nothing new — it already routed the wake to the group pseudonym), #3 (no `lakko-mls` change — the slim crate re-exports the audited `process_incoming_verifying_roles` / `parse_body_envelope` path, it does not fork it), #4 (fail-closed to the generic banner), #7 (seed read-scope widening, letter preserved). `lakko-security` APPROVE-WITH-NOTES on Phase 1 (App Group / seed-sharing migration; the LOW-1 atomic-copy-with-read-fallback fixed, `7b18cec`) + Phase 3 (NSE decrypt) + the web sibling. |

### C-60 — Opaque per-group push routing id, unlinkable to the DS `group_id`, forward-unlinkable across a member removal (ADR-0042 + ADR-0043)

| ID   | Title |
|---|---|
| C-60 | Push subscribe / notify / the delivered payload route under an **opaque, member-secret-derived `push_gid`** that is **unlinkable to the DS `group_id`** ([ADR-0042](../adr/0042-push-opaque-group-routing-id.md)). **The claim is narrow and exact — this is NOT "the social graph is no longer materialised on the server"**: push-gw still stores device-to-routing-id rows and APNs/FCM still receive a **stable per-group pseudonym in cleartext**; what is removed is the ability to join that pseudonym to the group's DS identity (residual clustering tracked as **G-36**). **(1) Algebraic independence.** `push_gid = keyed-BLAKE3(push_routing_secret, "lakko/push/group-routing-id/v1")` (`crates/lakko-contacts/src/push_routing.rs` `derive_push_gid`, golden-pinned) over **32 CSPRNG bytes** never derived from the `group_id`, so `push_gid` carries no algebraic relation to it and a push-gw-vs-DS cross-correlation is **impossible without the secret**, which no server holds; the 32-byte width drops into push-gw's existing `BINARY(32)` / hex key-space unchanged. **(2) In-group-only distribution.** The secret rides an **MLS application message** as a fixed-length domain-separated envelope, so the DS and push-gw see only ciphertext (invariant #2); the codec is bounded + fail-closed (exact-length + domain-match + version gate), byte-distinct from every sibling envelope kind, and fuzzed (`push_routing_secret_decode`). **AMENDED by [ADR-0044](../adr/0044-push-routing-secret-recency.md): the envelope is v2, 70 bytes** — `domain(29, unchanged) || version(1)=0x02 || mint_epoch(8, BE u64) || secret(32)`; the retired v1 was the 62-byte `domain || version || secret(32)`. The 29 domain bytes are the `incoming_envelope` dispatch prefix and are frozen forever, so the in-payload version byte is the knob that moves. **Hard flag day, no dual-accept** (a v1 envelope carries no `mint_epoch` and so has no position in the C-62 order), version gated **before** length, and both cross-version refusals fail closed to **defer**, never to the raw `group_id`. **`derive_push_gid` and its golden are UNCHANGED by v2** — the epoch never enters the derivation (`derive_push_gid_ignores_the_mint_epoch`), so `push_gid` remains a **stable** per-group value with **no epoch input**, which is what lets a subscriber survive ordinary membership commits **without re-subscribing** and keeps the iOS `thread-id` constant; rotation changes the id by changing the **secret**, never the derivation. **(3) No raw-`group_id` path (invariant #4).** A member without the secret **defers** — it does not subscribe at all rather than falling back to the leaky DS key; there is no downgrade path. Accepted cost: a legacy / no-role-table group has no owner, so nobody mints and every member defers, meaning such groups get **no push at all** (G-36). **(4) Authority-gated receive path.** A pure, host-testable decision function on each of the three clients takes the MLS **sender leaf** (the ADR-0023 author-only-gate pattern), resolves it via the live group's `members()`, and ranks it against the ADR-0022 role table **owner > admin > member**: an *identical* secret is accepted from anyone (rotates nothing, keeps the post-add redistribute idempotent); the **owner** may always rotate, its branch evaluated **before** anything is read about what is stored (so owner priority is independent of envelope arrival order); an **admin** may **rotate** — see the ADR-0043 amendment below (ADR-0042 as shipped allowed an admin only to **seed** a *provably*-absent secret); a plain member, an **unreadable role table**, and an **unresolvable sender** are all refused fail-closed, while a group with **no role table** is *accepted* (the legacy / open-group case). "No role table" and "read failed" are kept **distinct** rather than one nullable. Without this gate any single non-owner could unilaterally rotate the group's routing id (`lakko-security` MEDIUM-1, fixed `21554f2`, refined `62f23ad`). **AMENDED by [ADR-0043](../adr/0043-push-routing-secret-rotation-on-removal.md) (ratified option (b)): an admin may now OVERWRITE, not only seed** (web `reject-admin-overwrite` -> `accept-admin-rotate`; the natives keep a binary accept/reject). The former sub-clause "a store-read failure is never allowed to read as a fillable hole on the admin branch" **no longer applies**, and its Android mechanism (stripping the admin set on an unreadable secret store) was **deliberately deleted** — under (b) it would have refused a legitimate admin rotation on a transient store error. **Rotation NEVER MINTS**, so mint authority stays **owner-only** and the `push_gid` cannot diverge into two; the cost is a widened DoS residual (admin-seed -> admin-churn, **G-36** residual 3). ADR-0043's mandatory `lakko-security` pass for option (b) **was run: APPROVE-WITH-NOTES, no CRITICAL/HIGH, no invariant #2/#3/#4 break on the live path**, its one blocking MEDIUM (a coalescing rotation lock folding a second removal into an in-flight rotation on web + iOS) fixed by adopting Android's serializing lock. **(5) Server untouched.** `services/push-gw` is **byte-identical** across the arc (`git diff 51fe05d..HEAD -- services/push-gw` is empty) — it never interpreted the value's meaning, only its width; the ADR-0041 / C-59 background decrypt gained a `push_gid -> (account, group_id)` reverse-map through a plaintext **local** index while the DS fetch keeps the real `group_id`, leaving the read-only-throwaway discipline unchanged. **(6) Forward-unlinkable across a member removal ([ADR-0043](../adr/0043-push-routing-secret-rotation-on-removal.md)).** On a membership **shrink** — a set difference over member Ed25519 signature keys, not counts and not leaf indices (OpenMLS reuses a vacated leaf for the next Add) — the owner **or an admin** re-mints and redistributes the secret **strictly after** the Remove commit, so the envelope is sealed at the post-remove epoch and RFC 9420 forward secrecy denies it to the evicted leaf (**C-61**). The old secret is read **before** the in-place overwrite so the retiring gid can still be named, and every remaining member retires **its own** old gid (unsubscribe + reverse-index delete) — required, not hygiene, since otherwise each rotation leaves one dead row per member, i.e. a live cluster **plus an archive of every previous one**. The evicted device's row is **never deleted** (no other party can, and no client has "you were removed" handling); it goes **INERT** because nothing ever notifies the old gid again. Never on an add; never mints. Gives per-group `unsubscribeGroup` its first real caller on all three clients. Invariants #2 (server opaque), #4 (defer, never downgrade; receive gate fail-closed), #5 (**improved but not fully satisfied** — no DS-linkable social graph, and no longer forward-linkable by an evicted member, but an opaque co-membership cluster is still materialised and still re-linkable across a rotation by token-set intersection, whose set *difference* even names the evicted device, G-36). `lakko-security` **APPROVE-WITH-NOTES**, no CRITICAL/HIGH — for **both** ADR-0042 and ADR-0043 (the latter additionally: no invariant #2/#3/#4 break on the live path; its one blocking MEDIUM, a coalescing rotation lock on web + iOS, fixed by adopting Android's serializing lock). |

### C-61 — The push-routing-secret rotation is sealed at the post-Remove epoch (ADR-0043)

| ID   | Title |
|---|---|
| C-61 | The [ADR-0043](../adr/0043-push-routing-secret-rotation-on-removal.md) rotation that makes an evicted member's `push_gid` inert (C-60 sub-invariant 6) rests entirely on one **sequencing rule**, pinned separately for the same reason the call-key overlap rules C-8/C-9/C-10 are: **the rotation broadcast MUST be emitted strictly AFTER the Remove commit is published.** The new secret rides an **MLS application message**; emitted at epoch N (pre-removal) the member being evicted is still a member of that epoch and **can open it**, learning the new secret and defeating the whole rotation silently. Emitted at epoch N+1 it is unreadable to the removed leaf, because RFC 9420 §12 forward secrecy is exactly the property that denies the removed leaf the post-Remove epoch's keys. A **second** ordering rule protects the teardown: **the OLD secret must be read BEFORE the in-place store overwrite** — the row is keyed `(account, group)` and clobbered in place, and the unsubscribe needs the OLD `push_gid`, so persist-first permanently loses the ability to name the row being retired. **Enforcement is asymmetric and that is the point of this entry.** The read-before-overwrite rule is test-pinned (e.g. `apps/ios/LakkoTests/PushGroupRoutingTests.swift:1805`, "the teardown names the OLD id", and `:1680` both assert the unsubscribe targets the OLD gid). The broadcast-after-commit rule is **structural ONLY** — web awaits `removeMemberFromGroup` (`Chat.tsx:2267-2271`) before rotating (`:2293-2299`), iOS awaits the publish (`ChatRepository.swift:891-894`) before `:917`, Android rotates at `:842` after the `groupsLock` block closes at `:820`, and on the poll path the commit is applied before the membership diff . As ADR-0043 shipped, **NO test pinned it on any client** — exactly the property a refactor can silently break, since hoisting the rotation above the publish, or making the publish non-blocking, would pass every existing test while restoring the leak, invisibly at runtime (the evicted member simply keeps receiving wakes); ADR-0043 states this "must be asserted in a test" and that requirement went unmet, recorded as a gap under **G-36** (residual 6). **NOW TEST-PINNED ON ALL THREE CLIENTS** ([ADR-0044](../adr/0044-push-routing-secret-recency.md), `936d849` web / `014e84e` iOS + Android), **closing G-36 residual 6**: the sequencing moved behind one named seam per client (e.g. iOS `PushGroupRouting.publishRemoveThenRotate`, whose sole production call site is `ChatRepository.removeMemberFromGroup`), each client asserts on the **emitted envelope's `mint_epoch` stamp** rather than on call order, and each carries a **permanent inverted-order companion** that runs the swap — with the discrimination **proven by executing the swap** on both natives (iOS recorded three failures under a temporary inverted copy; Android recorded the stamp assertion failing 11 vs 10; both probes reverted). Six tests on web, five on iOS, six on Android; the stale-secret fixture is `0xFF`, the bytewise maximum, so the rotation loses deterministically under the swap — the first web cut's `0xEE` was latently flaky, a random rotation secret out-sorting it roughly one run in fourteen. **ADR-0044 also sharpened the stakes**, which is why the tests stopped being optional: under ADR-0043 alone a regression cost the rotation's forward secrecy; under ADR-0044 it **also inverts the C-62 recency comparison** — a rotation stamped at the pre-removal epoch **ties** with the stale secret and may **lose** the byte tiebreak — so the guarantee degrades from "the rotation always wins" to a **silent coin flip**, making C-61 a precondition of two properties rather than one. Invariants #2 (a client sequencing property — the envelope is opaque to the DS and push-gw either way), #4 (no fallback path added), #5 (this ordering is the whole of C-60 sub-invariant 6). |

### C-62 — Mint-epoch recency ordering for the push routing secret (ADR-0044)

| ID   | Title |
|---|---|
| C-62 | The push-routing secret is **totally ordered** and only a **strictly newer** envelope may displace the stored one ([ADR-0044](../adr/0044-push-routing-secret-recency.md)). C-60 sub-invariant 4 gave the receive path an **authority** gate — *who sent this* — and until this entry there was **no ordering gate at all**, so a **retry** of the current secret and a **replay** of a superseded one were byte-indistinguishable on every client. That omission is what made a **silent, permanent rotation undo** reachable with no malice and no crash (**G-36 residual 9** / `lakko-security` **F-A**, closed by this entry for the single process context). **Five sub-invariants** — four about the order itself, and a fifth about the path that consumes it, added after the arc's own review showed the first four are inert without it. **(1) The order is total, canonical, defined ONCE.** Envelopes are ordered on the pair `(mint_epoch, secret_bytes)` — 8-byte big-endian epoch first, then the 32 secret bytes lexicographically — **strictly greater wins, equal is NOT newer** (what keeps a benign retry from reading as an update and makes the `accept-identical` fast path safe); wrong-length secrets **fail closed to `false`**; never panics. It lives in `crates/lakko-contacts` as `push_routing_secret_is_newer` and **every client routes its decision through it over the FFI** (`pushRoutingSecretIsNewer`, uniffi + wasm) rather than re-implementing it — three clients each hand-rolling a comparison being precisely the drift class that produced F-A. The **secret bytes** are the tiebreak because they are the only discriminator both available to every receiver by construction and **stable across relay** (a sender-derived one changes identity on every re-broadcast; a minter identity in the envelope is self-asserted), and the tiebreak is the **common path**: under ADR-0043 option (b) one removal produces **N+1 same-epoch mints**. **(2) The mint epoch is DURABLE state, replayed verbatim.** Stamped once at mint time; only a mint reads `Group::epoch()`; all six re-broadcast sites replay the stored stamp, because re-stamping at send time would make a stale envelope arrive **newer** than the live rotation and reproduce F-A with the gate fully in place. It is therefore a **store** change on all three clients (persisted with the secret and the `distributed` flag in one atomic write, read as one pair so it cannot tear); legacy unstamped rows read at **epoch 0**, the floor. Android additionally makes the rule impossible to break **by accident** via a sealed `MintStamp` — bounded claim: Kotlin `internal` is module-scoped and `StoredSecret`'s constructor is public, so a wrong stamp stays *constructible*; what is removed is the **silent** failure mode, since a bare `ULong` can no longer be passed where a stamp is expected. **(3) The future clamp, evaluated BEFORE authority.** Without a ceiling an authorised writer stamps `u64::MAX` and **freezes** the group's routing id permanently, escalating **G-36 residual 3** from admin-can-churn (self-repairing at the owner's next rotation) to admin-can-freeze (which is not) — a recoverable denial traded for an unrecoverable one. Clamping an incoming stamp against the receiver's own group epoch never rejects an honest envelope (`stamp <= seal epoch <= receiver's epoch at dispatch`), and running it ahead of authority means **not even the owner** can freeze the group. **The core owns the ORDER, the client owns the CLAMP**: `mint_epoch` is opaque to `lakko-contacts` (never a time, never bounded, never in `derive_push_gid`), so sourcing the stamp from `Group::epoch()` is the **client-side binding** that makes the clamp checkable at all — an MLS epoch is shared state every receiver independently observes, where a free-running counter could be bounded by nothing. **(4) `accept-identical` must NEVER move the stored stamp.** That rule is accepted from anyone and sits ahead of both the clamp and the order, so it may write nothing: re-persisting the incoming epoch there is an **epoch-inflation channel reaching the exact freeze (3) prevents, around it** — a **plain member** relays the group's current bytes stamped `u64::MAX` and every later rotation, the owner's included, loses forever. A **live defect in the first implementation**, found by the web verdict tests, fixed on all three with named regression tests. Generalised: **a rule that short-circuits a gate must not write any state that gate protects.** **(5) The gate must run against CURRENT state — its read, decision and persist are serialized per `(account, group)`.** The sub-invariant the arc's own review had to teach it, and the reason the other four are not self-sufficient: the gate is a **read-modify-write** with suspension points between the read and the write, and if two envelopes decide against the **same snapshot** a correct total order changes nothing — the survivor is whichever task wrote last. **Stated generally: an ordering rule is a property of the state the deciding read observed, so a total order is only meaningful if the gate consuming it runs against current state.** Enforcement is **one serializing (never coalescing) lock per `(account, group)`**, held across the whole read-decide-write and **shared** across four entry points — receive, rotate, ensure, **redistribute** (the same read-then-write; unserialized it puts a PRE-rotation secret back on the wire mid-rotation). Both design points are load-bearing: **shared**, proven by an Android probe (with receive on its own lock the F-A and N-rotation cases pass while **F-E** still fails), and **serializing**, since a coalescing lock folds a second distinct trigger into an in-flight one (the ADR-0043 F1 finding). **Every network call taken under the lock is individually bounded** (web 30 s, iOS 15 s, Android 15 s; Android additionally bounds `freshMintStamp()` for the **G-37** `groupsLock` reason), because a wedged row is not merely unavailable — it is the row that never applies a peer's rotation, so the device keeps notifying an id an evicted member still derives. The bound is **fail-closed for the operation** (an expired call is treated as failed; neither the authority gate nor the order is relaxed, and both still run in full inside the lock against a fresh read) and **fail-open for the row** (the lock is released so the queue proceeds, since the work queued behind it is precisely the peer rotation that retires an evicted member's id) — **not** the ADR-0033 fail-open, which relaxes an anti-abuse *gate*. **STATUS — built and armed on all three clients; the closures HOLD, qualified to the single PROCESS CONTEXT.** `936d849` / `014e84e` landed sub-invariants 1-4, and the `lakko-security` review of that landing returned **REQUEST CHANGES**, **reproducing the defect by execution** on web (verdicts `['accept-admin-rotate', 'accept-identical']`, final stored secret `0xEE` at epoch 10 — the stale one): the primitive was correct but the receive path that consumed it was an unserialized read-modify-write dispatched fire-and-forget per envelope, so the gate was **bypassed, not defeated** — sub-invariant 4 the vehicle, since nothing stopped `accept-identical` being *reached* on a stale snapshot, and the losing gate files the stale stored epoch it read. **`9a0f539` closed it** with the sub-invariant-5 lock (web + Android reproduced the defect before the fix, signature `expected 'accept-identical' to be 'reject-stale'`; on **iOS the batch race was REFUTED, not fixed** — no suspension point between the `getRecord` read and the persist and no `async` in `PushRoutingStore`, so `@MainActor` already excluded it, though iOS's rotate-vs-receive **F-E** interleave was real), and the review of that fix returned **APPROVE WITH NOTES**, no CRITICAL/HIGH, both actionable findings since closed (a web lock that was per-JS-realm rather than cross-tab — now a `navigator.locks` Web Lock nested inside the deliberately-retained in-process queue; and the lock held across unbounded network I/O — now bounded per call). Consequently **G-36 residual 9 CLOSES**, residual 7's **concurrent-rotator core CLOSES**, and residual 5's **rotate-vs-receive (F-E) path CLOSES** because the lock is shared; **residual 6** had already closed outright (C-61 test-pinned everywhere). **The qualification is sub-invariant 5's scope, the single process context**, and is tracked as the **re-scoped G-36 residual 10**: on web `navigator.locks` is secure-context only, so an insecure context / old browser / embedded webview degrades to same-tab-only serialization (the in-process queue is kept beneath it precisely so that state degrades rather than silently running unserialized); on the natives the ADR-0041 / C-59 cross-process boundary was **not examined** by this arc. Out of scope regardless: the order does not repair a device already holding a stale value, and has no bearing on residual 7's F-D stale-role window (an authority refusal). One transition caveat: two different pre-v2 secrets both read `mint_epoch = 0`, so a pair among them falls to the byte tiebreak rather than recency — bounded to the flag-day window and self-healing at the next rotation, which always stamps above 0. Invariants: **#2** (the `mint_epoch` rides inside the same E2EE MLS application-message plaintext; the DS sees ciphertext, push-gw sees only the derived 32-byte id and is **byte-identical unchanged**), **#3** (no `crates/lakko-mls` edit — the clients read the existing `Group::epoch()` accessor), **#4** (the fail-closed surface **widens** — reject-future, reject-stale and both cross-version refusals all drop or defer, never downgrade; the codec stays bounded and exact-length. The one fail-**open** here is scoped to the sub-invariant-5 lock's *hold time* and relaxes **no gate**), **#5** (unchanged posture). |

## Transport-layer invariants (T-1..T-11)

Source: [`transport.md`](./transport.md). Anti-censorship
+ rendezvous + Shadowsocks-2022 surfaces (Phase 5).

| ID   | Title |
|---|---|
| T-1  | Pluggable-kind wire identifiers are frozen at v1 |
| T-2  | Scaffold transports surface `NotImplemented` loudly |
| T-3  | Selector race preserves submission order |
| T-4  | Rendezvous verify is a 4-stage gate |
| T-5  | Canonical bytes = JSON with `signature_hex` zeroed |
| T-6  | TransportSpec kebab-case kind is part of the protocol surface |
| T-7  | Trust anchor is binary-pinned |
| T-8  | DoH fetcher preserves the rendezvous trust chain end-to-end |
| T-9  | Chunked AEAD stream primitive pins ChaCha20-Poly1305 + monotonic nonce |
| T-10 | Shadowsocks-2022 outer header layer pins BLAKE3 context + timestamp window + replay window |
| T-11 | SS-2022 TCP wiring preserves per-direction key separation + frame-size cap |
| T-12 | ECH config list parser is version-pinned (0xfe0d) + length-bounded (16 KiB outer / 16 inner configs / 4 KiB per config); wire-up via RustCrypto/hpke adapter on rustls 0.23 (HTTP + WebTransport landed, REALITY architecturally not applicable) |
| T-13 | REALITY authenticator is X25519 ECDH + HKDF-SHA-256 (label `lakko/reality/v1`) + HMAC-SHA-256 over (short_id || timestamp); tag verified constant-time before any downstream byte is trusted |
| T-14 | WebTransport client uses cert-hash pinning (SHA-256 of full DER cert, W3C `serverCertificateHashes` mode) + u32-BE length-prefixed framing with MAX_FRAME_BYTES enforcement |
| T-15 | MASQUE config + CONNECT-UDP request (RFC 9298 §3) + HTTP Datagram payload codec (RFC 9297 §3) are spec-aligned and length-bounded; CONNECT-UDP Context ID 0 pinned, others refused |

## Known gaps (G-1..G-37 in `known-gaps.md`; this listing is partial — see the note under Quick stats)

Source: [`known-gaps.md`](./known-gaps.md). Items the
maintainer knows are not audit-ready yet; reviewers
should not re-discover them.

### MLS state machine

| ID  | Title |
|---|---|
| G-1 | Channel two-tier flow (>50k subscribers) — **CLOSED** by Phase 4 backend + FFI (C-40..C-44) |
| G-2 | MLS-2 / quantum-hybrid suites pinned for later |
| G-3 | External-join proposals not wired into a UI surface |

### Multi-device

| ID  | Title |
|---|---|
| G-4 | AccountIdentity rotation on password change has no KT-log supersession proof yet |
| G-5 | Commit broadcast on `add_device_from_wire` to other pre-paired devices — **CLOSED** (all three orchestrators publish the commit to the DeviceGroup DS journal) |
| G-6 | History bundle `payload_bytes` use a JSON placeholder, not raw MLS application-payload bytes |
| G-7 | DeviceGroup snapshot persistence may briefly diverge across in-flight ratchet operations — **CLOSED** (persist-before-publish reorder on all three clients) |

### Web client

| ID  | Title |
|---|---|
| G-8 | Web has no Service Worker-side message-log mutation listener |
| G-9 | Web `payload_bytes` JSON placeholder strings are NOT byte-stable |

### Backup

| ID   | Title |
|---|---|
| G-10 | Backup envelope rotation on password change happens on the next baseline write, not the recover round-trip itself |
| G-11 | No tamper-evident chain across backup versions — **CLOSED IN PRACTICE (ADR-0037)** by the auth-signed backup head (C-58): returning-client rollback HARD-closed by the monotone version floor, fresh-install BOUNDED by the freshness window; the v2 chain the prior claim credited was dormant. Residual: within-window zero-anchor replay |

### Auth

| ID   | Title |
|---|---|
| G-12 | Session token validation is single-process in-memory — **CLOSED** (stale text; tokens are the stateless HMAC-SHA256 `lakko-auth-token` shape under a shared `LAKKO_SESSION_TOKEN_SECRET` — any instance validates without coordination, survives restarts) |
| G-13 | WebAuthn passkey lacks per-credential authentic-action attestation |

### Out-of-protocol

| ID   | Title |
|---|---|
| G-14 | Hostile-OS / hostile-browser threats |
| G-15 | Side-channel + physical attacks |
| G-16 | Quantum adversaries |

### Calls

| ID   | Title |
|---|---|
| G-17 | Frame-counter exhaustion path is test-exercised — **CLOSED** by 3-test trio in `crates/lakko-calls/src/sframe.rs` |
| G-18 | Overlap-window 1-second timer lives in platform code, not in the Rust core |
| G-19 | SFU-side JWT validation lands with 3b1c (LiveKit fork) — **superseded by ADR-0011 (str0m SFU); C-39 covers the new path** |
| G-20 | Call recording is out of scope (deferred per ROADMAP 3c8) |
| G-21 | Link-based call invites are single-joiner in V1 — **CLOSED** by C-33/C-35 KP-forward + multi-party tests |
| G-22 | Per-joiner dedup for call invites lands with multi-use orchestration — **CLOSED** by C-33/C-35 |
| G-26 | SFrame wire frame header (**RFC 9605 §4.3-conformant** — the §4.3 rewrite landed 2026-07: config byte `X\|K\|Y\|C` + inline `KID<=7` / `CTR<=7`, proven byte-for-byte against RFC 9605 Appendix C.1's published vectors (`header.rs::rfc_9605_appendix_c1_header_vectors`); the golden vector was re-pinned across all six stacks. External interop is now possible; only a live cross-platform / external-stack RTP round-trip over a deployed SFU remains) — serializer + framed pipeline + cross-platform AAD **landed** (C-38 / C-55, ADR-0013); SFrame sealed end-to-end on all three SFU clients. **Interop effectively closed:** the cross-runtime byte interop is now a *proven equality* via a golden vector pinned + asserted (seal AND open) on all four stacks — Rust core / wasm-web / iOS-UniFFI / Android-UniFFI (`sframe.rs::golden_vector_*`, `sframe-golden.test.ts`, `SFrameGoldenTests.swift`, `SframeGoldenTest.kt`; Rust+web+iOS verified firsthand, Android in `g3-4-mobile.yml` CI), and now ALSO on the two NATIVE-stack layers the platform shims wrap: the flat C ABI (`crates/lakko-sframe-native/tests/golden_cross_stack.rs` — `extern "C"` `seal_framed`/`open_framed`) and the C++ bridge (`apps/shared/sframe-bridge/test_bridge.cc` `test_cross_stack_golden_vector` — the `LakkoFrameEncryptor`/`LakkoFrameDecryptor` webrtc-interface impls), both pinned to the same `GOLDEN_WIRE_HEX`, so every native-stack layer is byte-identical to web/core down to the libwebrtc handoff. The live-SFU verbatim forward is proven by `sframe_sealed_frame_round_trips_through_the_live_sfu`. Composed, the sealed-on-web → SFU-relay → opened-on-native path holds by correctness. Residual: a single monolithic real-Chrome ⊕ real-native ⊕ deployed-SFU smoke — redundant for correctness, a deploy/CI-execution concern |
| G-27 | Channel PublisherGroup — **CLOSED** — web + iOS + Android all derive the channel seed from a real, persisted `PublisherGroup` (G4-2-mls-publisher + G4-2-mls-persistence); the `newChannelSeedForTest` demo path is gone; the co-publisher-approve path reseals subscribers via the atomic `/rotate` fanout; the **scheduled** `RotationCron` module ships on all three but is not yet wired to run (a G-1 residual — corrected 2026-07 from an earlier "rotation wired via RotationCron on all three" overstatement); co-publisher reuse landed (ADR-0028 OQ3) |
| G-31 | Cross-stream SFrame AEAD nonce reuse — multiple streams from one leaf shared a `(key, nonce_salt)` and their independent counters collided (HIGH for SFU group video) — **CLOSED (2026-07, [ADR-0035](../adr/0035-sframe-per-stream-kid-key.md))** by per-`(leaf, stream)` key derivation via the header KID `= (sender_leaf << 8) \| stream_index`, which fully covers the multi-video (camera + screen-share) case that [ADR-0034](../adr/0034-sframe-per-media-stream-key.md)'s per-media-type first attempt did NOT (superseded); landed + cross-stack-verified on all six SFrame stacks against one canonical golden; strengthens C-5 |

### Push routing

| ID   | Title |
|---|---|
| G-36 | Push routing under an opaque `push_gid` ([ADR-0042](../adr/0042-push-opaque-group-routing-id.md) / [ADR-0043](../adr/0043-push-routing-secret-rotation-on-removal.md) / [ADR-0044](../adr/0044-push-routing-secret-recency.md)) — the **co-membership cluster** and the surviving delivery-side residuals. Per-residual status: **2 CLOSED** (rotation on removal + live per-group unsubscribe), **6 CLOSED** (C-61 test-pinned on all three clients), **9 CLOSED** for the single process context (the recency order **plus** the serializing gate — C-62 sub-invariants 1-5), **7's concurrent-rotator core CLOSED** on the same fix, **5's rotate-vs-receive (F-E) path CLOSED** because the lock is shared; **10 RE-SCOPED** from "no client serializes the receive gate" (HIGH, blocking) to the cross-context remainder — web degrades to same-tab-only serialization without `navigator.locks`, and the native ADR-0041 / C-59 cross-process boundary was not examined; **1 / 3 / 4 / 8** untouched, and **1 is the substantive privacy limit** (an opaque co-membership cluster is still materialised and still re-linkable by token-set intersection; closing it needs per-device-blinded ids) |
| G-37 | Android `ChatRepository.groupsLock` is held across **unbounded** DS network I/O (`publishGroup` at `:619` / `:719` / `:863` / `:1024` / `:1030`, `publishMailbox` at `:326` / `:725`, and the commit-outbox republish lambdas at `:2563-2565`), so a stalled DS request wedges the shared lock indefinitely. Surfaced by the ADR-0044 lock-bounding work: the push path reached it via `freshMintStamp() -> groupsLock` and is now insulated (`boundedOrNull` at both sites that await it under the routing lock), but **every other `groupsLock` caller still inherits it** and the chat path still has it. Availability, except on the one path where liveness was carrying a confidentiality guarantee. The equivalent question has **not been asked** of the iOS and web chat repositories |

### Dependency advisories

| ID   | Title |
|---|---|
| G-23 | `libcrux-chacha20poly1305 0.0.7` — RUSTSEC-2026-0124 (HIGH 8.2) — upstream-blocked |
| G-24 | `rsa 0.9.10` — RUSTSEC-2023-0071 (MEDIUM 5.9) — Marvin Attack — `sqlx-mysql` MariaDB-auth transitive (NOT TLS-stack — see known-gaps.md G-24 for reachability analysis) |
| G-25 | Three unmaintained-crate warnings |

## How to add a new invariant

Adding a new invariant means **all four** of the steps
below — drift between this index and the source files is
the audit-grep failure mode this page exists to prevent.

1. Decide which class — `I-` (MLS), `C-` (calls /
   channels — everything the SFU/channels server might
   see), `T-` (transport), `G-` (gap, not yet a real
   invariant).
2. Pick the next free integer. The class is namespaced —
   `C-45` and `T-12` are both legal next IDs at any time.
3. Add the invariant to its source file in this audit
   pack. Convention: `### {ID}: {Title}` followed by a
   short rationale + pinned tests / code references.
4. Add a row in **this** index — same ID, same title.
5. If the new invariant supersedes an old one, edit the
   old row to mention the supersession (don't delete —
   the row's removal would break grep + historical
   commit-message links).

## Counter sanity checks

If `wc -l docs/security/{calls,transport,mls-state-machine}.md`
goes up significantly without this index changing, an
invariant landed without being indexed. **CI gates on
this** via [`tools/audit-index-check`](../../tools/audit-index-check)
— a bash script that diffs the `### {class}-{n}:` headers
in the three source files against the per-class tables on
this page. Both directions are checked: a new invariant
in the source must have a matching index row, and a stale
index row whose source-file header is gone is also a fail.
Wired into `just ci` as the final docs gate. Run locally
via `just audit-index-check`.

## Cross-links

- [`README.md`](./README.md) — entry point for the audit
  pack.
- [`threat-model.md`](./threat-model.md) — adversaries +
  assumptions every invariant is defending against.
- [`crypto-overview.md`](./crypto-overview.md) — the
  primitive library each invariant rests on.
- [`../ROADMAP-PHASE3-CLOSEOUT.md`](../ROADMAP-PHASE3-CLOSEOUT.md)
  — sprint-level decomposition; every invariant in this
  page traces back to a sprint row there.
- [`../adr/`](../adr/) — Architecture Decision Records;
  the strategic rationale behind the protocols every
  invariant pins.
