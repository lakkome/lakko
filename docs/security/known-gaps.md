<!-- SPDX-License-Identifier: AGPL-3.0-or-later -->
<!-- SPDX-FileCopyrightText: 2026 Lakko Contributors -->

# Lakko — Known gaps before public beta

This file is the **honest inventory**. Reviewers should
not re-discover these as findings; they are known and
tracked. Each entry has a target sprint / ROADMAP link.

Items here are either:

- (a) explicitly deferred to a later phase (the protocol
  shape is settled but the implementation lands later); or
- (b) known soft spots in the current implementation that
  we trade off for shipping speed, with a documented
  follow-up.

## MLS state machine

### G-1: Channel two-tier flow (>50k subscribers) — **crypto + service + UI + nav + RotationCron + batched fanout + multi-epoch retention + `@channel` client discovery BUILT (Sprints A–D, 2026-07); residual is the deploy-gated >50k scale proof only**

Phase 4 (ADR-0010, two-tier PublisherGroup + sender-key broadcast). A 2026-07
code audit found the prior "nothing is shipped" note **badly stale** — the crypto
substrate, backend, both FFI surfaces, discovery, and publisher / subscriber /
co-publisher UI on all three clients are built. What is built vs the real residual:

**Built:**

- `crates/lakko-channels` (4a/4b): `seed` (MLS-exporter content-key seed), `post`
  (per-message AEAD off the seed + Ed25519 signature — `compose_post` /
  `verify_and_decrypt`), `subscribe` (HPKE-equivalent seed envelope
  `seal_seed`/`open_seed`, 80-byte wire, AAD-bound to `channel_id‖epoch`),
  `rotation_policy` (`should_rotate`), 4 fuzz targets.
- `services/channels` (4c): register / posts (publish + cursor fetch) /
  subscribers / envelope-deliver+pull / atomic `POST /rotate` fanout / epoch
  routes; in-memory + MariaDB `ChannelStore`; retention prune; opacity-preserving
  (crate is dev-dep only).
- FFI (uniffi + wasm): full channel + subscriber + `PublisherGroup` surface.
- Clients (web / iOS / Android): create+publish, co-publisher (ADR-0028 OQ3),
  subscribe-to-another's-channel, and a read-posts feed are all **real screens**
  with real MLS-backed crypto.
- `services/discovery`: `@channel` OPRF `register` / `lookup` (server side).

**Landed since the 2026-07 audit (Sprints A–D):**

1. **Navigation (Sprint A).** Channels is now reachable from app navigation — a
   nav entry on web (`AppShell`) + a Home call site presenting the channels screen
   on iOS (`HomeScreen`/`LakkoApp`) and Android (`HomeScreen`/`MainActivity`). No
   longer built-but-unreachable.
2. **RotationCron activation (Sprint A).** Wired to run on all three clients
   (heartbeat-driven scheduled rotation + a "Rotate now" affordance + persistence)
   — ADR-0010's aggressive-rotation backward-secrecy mitigation is live. The
   ROADMAP-PHASE3-CLOSEOUT G-27 "rotation wired via RotationCron on all three"
   wording is now accurate.
4. **Multi-epoch envelope retention (Sprint C, [ADR-0031](../adr/0031-multi-epoch-envelope-retention.md)).**
   N=1 → N=4: `channel_envelopes` keyed `(channel, subscriber, epoch)` + a
   prune-to-N on every write path; a client-side `epoch → seed` map + a
   `fetch_envelopes` catch-up + decrypt-by-epoch (a missing-epoch post renders an
   "older key window" placeholder and is **skipped**, never force-decrypted, #4) +
   a re-subscribe on offline-past-N. Posts made under a missed rotation are
   readable again within the retention window; only pre-window posts stay
   unreadable (an availability cost, not a confidentiality break).
5. **Batched rotation fanout (Sprint D, [ADR-0030](../adr/0030-batched-rotation-fanout.md)).**
   The single unbounded `POST /rotate` is joined by a chunked, resumable, idempotent
   staged prepare (`/rotate/stage`, ≤5000/chunk, `channel_staged_envelopes` never
   served) + an atomic `/rotate/commit` — C-42 preserved. Fit for the 50k–1M
   fanout the single request could not carry.
3. **`@channel` discovery (Sprint B).** DONE on web + iOS + Android — clients
   register a `@handle` at channel-create + resolve `@handle` → `channel_id` via
   the OPRF discovery (`registerChannelName` / `lookupChannel`, reusing the
   `@username` OPRF plumbing), so a user subscribes by `@handle` instead of a raw
   hex id (the hex-paste path is preserved). The server side
   (`/discovery/channel/{register,lookup}`) was already done; this is client
   wiring only, no FFI change.

**Remaining residual:**

6. >50k **operational scale** (dedicated HPKE-envelope bandwidth allowance, load
   test p95 < 5 s) — deploy-gated, not yet exercised.

**Recommended next sprint:** wire `@channel` discovery into the three clients
(item 3, Sprint B) — the last usability gap before the deploy-gated scale proof
(item 6).

### G-2: MLS-2 / quantum-hybrid suites pinned for later

The IETF MLS WG is still landing hybrid PQ cipher suite
identifiers. We pin classical suites today and migrate
once the draft stabilises.

### G-3: External-join proposal approval — **CLOSED end-to-end on web + iOS + Android ([ADR-0027](../adr/0027-external-join-approval.md) admin approval + [ADR-0028](../adr/0028-group-join-ticket.md) join-ticket invite-link); the only residual is a deployed-env live-admin-approval e2e**

Was: `Group::process_incoming` returned a data-less
`Incoming::ExternalJoinProposal` unit variant and the
clients ignored it — no joiner identity, no approve path.

**Phase 1 landed (Jun 2026, ADR-0027).** The `lakko-mls`
core now surfaces the joiner identity and gates an admin
approve:
- `Incoming::ExternalJoinProposal { joiner_signature_key }`
  — extracted from the external `Add` proposal's KeyPackage
  (refusing a non-`Add` / non-`NewMemberProposal`-sender
  proposal, defence in depth), retained in an in-memory
  per-group queue bounded by `MAX_PENDING_EXTERNAL_JOINS`
  (anti-spam) + de-duped by signature key.
- `Group::approve_external_join(identity, proposal_ref)` —
  **role-gated** (ADR-0022: owner/admin only on a group with
  a role table, fail-closed; open group stays open) — adds
  the joiner via the audited `add_member` path (Commit +
  Welcome); peers re-verify the resulting `Add`'s committer
  role on receipt, so a forged local approve can't take
  effect on them either.
- `Group::reject_external_join(proposal_ref)` (local discard,
  no epoch advance) + `pending_external_joins()` accessor.
- 5 property tests (owner-approves-adds-joiner /
  reject-removes-without-adding / dedup-by-signature-key /
  unknown-ref-errors / non-admin-approve-refused); full
  `lakko-mls` suite + clippy green.

**Phase 2 landed (FFI surface).** `ProcessedMessage`
carries `joiner_signature_key` (uniffi field /
`externalJoinSignatureKey` wasm getter), and the Group FFI
exposes `approveExternalJoin` (→ `AddMember` commit+welcome)
/ `rejectExternalJoin` / `pendingExternalJoins` on uniffi
(Swift/Kotlin) + wasm (web). `regen-bindings` + the web
`.d.ts` emit them.

**Phase 3 landed (Welcome rendezvous transport
primitive).** `crates/lakko-mls/src/welcome_rendezvous.rs`
`WelcomeRendezvous` — the carrier the admin posts to the
group journal (`version ‖ sig_key_len ‖ joiner_signature_key
‖ welcome`, bounded + fail-closed + panic-free, 8 unit tests
+ a `welcome_rendezvous_decode` fuzz target); FFI
`wrapWelcomeRendezvous` (admin) + `welcomeRendezvousFor`
(web non-throwing matcher) / `parse_welcome_rendezvous`
(uniffi). The Welcome is HPKE-sealed to the joiner's init
key, so the DS stores it opaque (invariant #2).

**Phase 4 client UI landed (members-UI approve/reject on
web + iOS + Android).** Each client's chat poll loop
classifies an `ExternalJoinProposal` into a dedicated
`externalJoinRequest` event carrying the joiner key (not a
timeline bubble — it drives a members-panel pending list via
`group.pendingExternalJoins()`, the MLS-core retained set
being the source of truth). An owner/admin-only
members-panel section (same role gate as add/remove, hidden
when empty) renders pending requests with Approve — which
drives the role-gated `approveExternalJoin`, broadcasts the
resulting commit to the group journal, wraps the Welcome in
a `WelcomeRendezvous` and posts it to the same journal, and
suppresses the sender's own commit echo — and Reject (a
local drop of the retained proposal, no commit/journal
post). Existing members skip the rendezvous entry (not an
MLS message → their receive loop's `processIncoming`
rejects + drops it). Verified: web `tsc`/`vitest` (213)/
`vite build`; iOS `xcodebuild` BUILD SUCCEEDED; Android
`:app:compileDebugKotlin` BUILD SUCCESSFUL (the 7 failing
Android unit tests are pre-existing Ktor-mock / base64
environmental failures, confirmed identical on a clean tree
— no chat test regressed).

**Joiner-side core + FFI landed.**
`Identity::create_external_join_proposal(group_id, epoch)`
(`crates/lakko-mls/src/key_package.rs`) lets a non-member
craft the external `Add` proposal (`JoinProposal::new`) for a
group's id+epoch, embedding a fresh KeyPackage whose private
keys persist in its own provider so the same identity later
opens the Welcome; FFI-exposed as `createExternalJoinProposal`
on UniFFI (Swift/Kotlin) + wasm. A full **in-process e2e
property test**
(`joiner_api_full_loop_proposes_approves_joins_via_rendezvous`)
proves the whole G-3 loop end-to-end: a non-member proposes
→ an admin's `process_incoming_verifying_roles` surfaces it →
`approve_external_join` → the Welcome rides a
`WelcomeRendezvous` → the joiner matches it by its own
signature key + `join_from_welcome` → it is a member at the
post-approve epoch (lakko-mls 95 lib tests green, clippy +
fmt clean).

**Join-ticket core + FFI landed
([ADR-0028](../adr/0028-group-join-ticket.md) Phase 1).** The
"convey the group params to a non-member" carrier was
reframed: ADR-0010 channel *subscribers* are not MLS members
(sender-key broadcast), so the join-ticket is a **general
group invite-link** for adding a member/publisher, not an
ADR-0010 artefact. A capability-based `/g/<slug>` link
(mirroring [ADR-0009](../adr/0009-link-based-call-invitations.md))
carries only the `group_id`; the joiner reads the **current
epoch from the journal's public MLS framing**
(`lakko_mls::epoch_of_message` — openmls
`ProtocolMessage::epoch()` works without a group secret), so
the ticket is epoch-independent (no stale-on-commit). The
security boundary stays the admin approval gate, **not** the
ticket — a leaked link lets a stranger *ask*, never *in*.
Landed: `crates/lakko-contacts/src/group_invite.rs`
(`GroupInviteCapability` + `compose`/`decode_and_verify`, 14
unit tests + the `group_invite_decode` fuzz target) +
`epoch_of_message` (1 test) + FFI
(`composeGroupInviteCapability` / `GroupInviteCapability` /
`epochOfMessage` on UniFFI + wasm). lakko-contacts 141 + 
lakko-mls 96 lib tests green, clippy + fmt clean.

**Phase 2 landed (owner create-link UI, web + iOS +
Android).** A members-panel owner/admin-only "Create invite
link" button (same role gate as add/remove) signs a
`GroupInviteCapability` over the group's id and shows the
copyable `https://lakko.me/g/<base64url>` link. Verified web
`tsc`/`vitest`/`vite build`, iOS `xcodebuild`, Android
`:app:compileDebugKotlin`.

**Phase 3 web landed (joiner flow — the live requester).**
`contacts/client.ts` `requestToJoinFromLink` (decode +
verify the capability fail-closed → read the current epoch
from the journal head's public framing via `epochOfMessage`,
no group secret → `createExternalJoinProposal` → post the
proposal to the journal) + `pollForJoinWelcome` (poll →
`welcomeRendezvousFor` → the audited `joinGroupFromWelcome`).
A public `/g/:slug` route + `JoinGroup.tsx` screen drives
decode → "Request to join" → poll-loop → navigate to the
joined chat (anonymous visitors prompted to log in, mirroring
`/i/`). This is the **live requester** that produces a real
pending request — G-3 is now exercisable end-to-end on web
(modulo a live admin approving). Verified `tsc`/`vitest`
(213)/`vite build`.

**Phase 3 native landed (iOS + Android joiner) — G-3
CLOSED end-to-end.** `ChatRepository.requestToJoinFromLink`
+ `pollForJoinWelcome` mirror the web orchestration (decode
+ verify the capability fail-closed → read the epoch from
the journal head's public framing → `createExternalJoinProposal`
→ post → poll → `parseWelcomeRendezvous` match → the audited
`joinGroupFromWelcome`); a `/g/` deep-link (the extended
`InviteIntake` + `MainActivity.handleGroupJoinIntent` /
iOS `onOpenURL`, plus the Android manifest
`pathPrefix="/g/"`) routes to a `JoinGroupLandingScreen` that
drives request → poll → join. Verified iOS `xcodebuild`
BUILD SUCCEEDED + Android `:app:compileDebugKotlin` BUILD
SUCCESSFUL.

**G-3 is now implemented end-to-end on web + iOS + Android**
— the admin approval (ADR-0027) + the join-ticket invite-link
(ADR-0028) form a complete joiner-initiated external-join
flow. ADR-0028's anti-abuse hardenings shipped: OQ1 (single-use
/ max-uses tickets via a server blob ledger + per-joiner
consume dedup — `services/discovery/src/group_invite.rs`,
migrations 0014/0015, wired on all 3 clients) and OQ2 (surface
"via &lt;issuer&gt; · member / not a current member" per pending
request from
`crates/lakko-contacts/src/group_join_attribution.rs` in the
members panel — advisory only, since the ADR-0027 role gate,
not roster membership, is the boundary). The only remaining
residual (deferred, not a gap in the protocol) is a live
admin-approval e2e on a deployed environment.

## Multi-device

### G-4: AccountIdentity rotation on password change has no KT-log supersession proof yet — **proof + chain verifier + FFI landed (sprints 1-3)**

The recover flow rotates the exportKey → rotates the
deterministic AccountIdentity. Today the old identity is
simply replaced; sibling peers will see a new pubkey on
the next handshake and TOFU it. The ADR-0007 §6 plan is
to publish a supersession proof to the KT log that
attests the old key → new key transition for ≤ 30 days,
so peers can verify the rotation without bouncing the
trust.

**Sprint 1 landed — the supersession proof primitive**
(`crates/lakko-transparency/src/supersession.rs`). A compact
fixed-layout Ed25519-signed blob
`version‖old_pubkey(32)‖new_pubkey(32)‖issued_at(8 BE)‖expires_at(8 BE)‖sig(64)`
(145 B) signed by the **old** identity key over the **new** key.
`SupersessionProof::sign` (rotating side) + the strict
`verify_supersession(bytes, trusted_old_pubkey, now)` (peer side:
signature against the embedded old key → the embedded key equals
the caller's trusted pin → a `<= 30-day` validity window
([`MAX_VALIDITY_SECS`], so a leaked old key cannot indefinitely
re-authorise, invariant #6) → freshness) returning the authorised
new key. `#![forbid(unsafe_code)]`, bounded parser, 7
unit/property tests (roundtrip, per-field tamper, stranger-forge,
old-key-mismatch, expiry, 30-day-window, malformed envelope).
**Sprint 2 landed — the peer-side chain verifier.**
`verify_supersession_chain(pinned, evidence, now)` ties the proof
to the KT log: it checks the operator's `SignedRoot` signature →
no-rollback vs the peer's last-seen root → the rotation
`LogEntry`'s Merkle inclusion under that root → the entry is for
this account (`username_hash` matches the pin) → a strictly newer
version → the supersession proof (old key signed, fresh, ≤30 d)
authorises **exactly** the entry's new key. So a rotation is both
cryptographically authorised AND non-equivocable, and the peer
re-pins to the returned `RotatedIdentity` without a blind TOFU.
This addresses the sprint-1 review's malleability note
structurally: the Merkle leaf commits to the
`(username_hash, version, pubkey)` tuple, **not** the supersession
signature bytes, so Ed25519 point-malleability cannot fork a log
entry. Six new tests (valid chain / wrong account / non-newer
version / proof-for-a-different-key / expired proof / forged
root). **Sprint 3 landed — the FFI surface for the proof primitive.**
`sign_supersession_proof(old_ed25519_seed, new_ed25519_seed, issued_at,
expires_at)` + `verify_supersession(proof, trusted_old_pubkey, now)` are
exposed on both UniFFI (`lakko-ffi-uniffi`, Swift / Kotlin) and
wasm-bindgen (`lakko-ffi-wasm`, web — `signSupersessionProof` /
`verifySupersession`, `i64` timestamps as `bigint`, byte slices as
`Uint8Array`, errors thrown). The sign helper takes each identity's
`ed25519_seed()` and derives both public keys internally, so the proof's
embedded `old_pubkey` always matches the signing key; verify returns the
authorised new key to re-pin. The chain verifier
(`verify_supersession_chain`) stays Rust-internal for now — it consumes
KT-log types (`SignedRoot` / `LogEntry` / `InclusionProof`) that have no
FFI wire format yet; that surface lands with the service. Four UniFFI
round-trip / reject tests; web `.d.ts` regenerated. **The transport +
client wiring landed (ADR-0016, Carrier B — the discovery identity
bundle).** Sprints 0-5 are wired end-to-end on web + iOS + Android
(build + unit-test verified): the discovery `identity_bundles` row carries
an opaque optional `supersession_proof` (migration 0010, server never
parses it — invariant #2); the recover flow emits a proof **atomically**
(capture the OLD exportKey before overwrite → sign old→new over a 14-day
window → upload `{new keys + proof}`, or upload nothing if it can't mint —
never a self-downgrade); and a per-contact pin store keyed on `account_id`
+ a fail-closed `verifyBundleAccountKey` gate at every `identityFetch` site
re-pins on a valid proof and **refuses** (typed `SupersessionRefused`, OOB)
a changed key without one — no blind TOFU. A malicious server can only
**strip** the proof (→ client fails closed), an availability impact never
an integrity break. **Remaining:** a `lakko-security` pass on the verify
path (explicit-request gate) + a live 2-device cross-platform e2e; and —
separately, for full equivocation resistance — the auth / transparency
*service* serving the KT-log entries that `verify_supersession_chain`
consumes (the chain verifier stays Rust-internal until then).

ROADMAP: Phase 2c (transparency log).

### G-5: Commit broadcast on `add_device_from_wire` to other pre-paired devices — **CLOSED**

**Closed (2026-06-03).** All three `runScanFlow` /
`PairingOrchestrator` implementations now broadcast the
`MemberAddition.commit` to the DeviceGroup DS journal
(`DsClient.publishGroup(group_id, commit)`) right after the
Welcome PUT, mirroring the established `removeDeviceAt`
broadcast. Other already-paired devices advance to the new
epoch when their DeviceGroup poll loop runs `processIncoming`
on the row (web `DeviceGroupSyncContext`, native lifecycle
poll). MLS requires in-order epoch processing, so the
broadcast is on the critical path (not best-effort) — a
missed commit would permanently strand a third device. For
the founder + first-pair case there is no other member, so
the row is a harmless no-op. Touchpoints:
`apps/web/src/devices/orchestrator.ts`,
`apps/ios/Lakko/Devices/PairingOrchestrator.swift`,
`apps/android/.../devices/PairingOrchestrator.kt`.

### G-6: History bundle `payload_bytes` placeholder — **cross-platform interop CLOSED; raw-ApplicationPayload-bytes aspiration still open**

`MessageLogStore` carries the projected `(text, attachments,
direction, createdAt)` shape — not the original wire
`ApplicationPayload` bytes. The history bundle round-trips
that shape as opaque JSON inside `payload_bytes`. Both
sides agree on the placeholder; the wire schema stays
proto-clean (`payload_bytes` is just bytes to the schema).

A future sprint can teach the message log to retain the
original wire bytes (smaller + lets a future "verify-replay"
step re-process every old message through the chat envelope
parser) and swap the placeholder for the real thing.

**Audited 2026-06-04 — the placeholder is currently
same-platform-only (web↔native is non-functional).** Reading
the three codecs end-to-end surfaced that the placeholder is
NOT cross-platform-interoperable today, which is broader than
the "JSON field-ordering" framing G-9 carried:

* **`HistoryMessage.message_id` field encoding diverges.** Web
  ([`history-bundle.ts`](../../apps/web/src/devices/history-bundle.ts))
  sends `textEncoder.encode(m.messageId)` — the UTF-8 bytes of
  the **decimal string**. iOS
  ([`HistoryBundleCodec.swift`](../../apps/ios/Lakko/Devices/HistoryBundleCodec.swift))
  and Android
  ([`HistoryBundleCodec.kt`](../../apps/android/app/src/main/kotlin/me/lakko/app/devices/HistoryBundleCodec.kt))
  send the **8-byte big-endian u64**. Different bytes for the
  same logical id.
* **`payload_bytes` placeholder shape diverges.** Web encodes
  `PersistedMessage.messageId` as a JSON **string** (the type
  is `string`); iOS (`Int64`) and Android (`Long`) encode it
  as a JSON **number**. A native `JSONDecoder` /
  `kotlinx.serialization` decode of web's string-typed field
  throws → the message is silently dropped (`continue`); web's
  `JSON.parse` of a native number lands a non-string where the
  `PersistedMessage` contract says string, mis-driving the
  downstream `messageId` string ops.

Net: **iOS↔Android history catch-up interoperates** (matching
field encodings + order-tolerant decode); **web↔native does
not** — a cross-platform pairing falls back to "joiner starts
with no history" (no data loss, but the catch-up is a no-op).

The proper fix closes G-6 and the interop gap together: move
the placeholder encode/decode into the **shared Rust core**
(one canonical codec exposed via UniFFI + wasm-bindgen, so all
three platforms encode/decode identically and it is unit-
testable in Rust), OR ship the raw `mls_app::v1::ApplicationPayload`
proto bytes directly.

**Step 1 landed (2026-06-04) — shared-core codec.**
[`crates/lakko-mls/src/history_placeholder.rs`](../../crates/lakko-mls/src/history_placeholder.rs)
is the canonical codec: a versioned, bounded, deterministic
binary layout carrying `(message_id: u64, direction, created_at,
text, attachments[{blob_id, key, nonce, size, mime?, name?}])`.
`message_id` / `size` ride as typed `u64` (each platform renders
its own way — web decimal string, native integer); `created_at`
rides as the platform's display string verbatim. Exposed
identically through both FFI crates — `encode_history_placeholder`
/ `decode_history_placeholder` + records in
`lakko-ffi-uniffi` (Swift / Kotlin) and `encodeHistoryPlaceholder`
/ `decodeHistoryPlaceholder` + classes in `lakko-ffi-wasm` (web)
— so every platform produces and consumes the **same bytes**.
13 Rust unit tests cover round-trip, all directions,
determinism, and bounded-parse rejection (wrong version,
truncation, over-cap length, non-UTF-8, bad direction tag,
trailing bytes).

**Step 2 landed (2026-06-04) — per-platform wiring.** All three
clients now call the shared FFI codec for `payload_bytes`, and
the sibling `HistoryMessage.message_id` field converged on the
native 8-byte big-endian `u64` (web previously sent UTF-8 of a
decimal string):

* **web** ([`apps/web/src/devices/history-bundle.ts`](../../apps/web/src/devices/history-bundle.ts))
  — `encodeMessagePlaceholder` / `decodeMessagePlaceholder` call
  `encodeHistoryPlaceholder` / `decodeHistoryPlaceholder`
  (wasm), base64-decoding the attachment refs to raw bytes; the
  `message_id` field is now `messageIdToBeBytes` (BE-u64).
  `tsc --noEmit` clean.
* **iOS** ([`apps/ios/Lakko/Devices/HistoryBundleCodec.swift`](../../apps/ios/Lakko/Devices/HistoryBundleCodec.swift))
  — `encodePlaceholder` / `decodePlaceholder` over the UniFFI
  `HistoryPlaceholder` record; `message_id` already BE-u64.
  Verified: `LakkoCore.xcframework` rebuilt (`just build-ios`)
  with the new FFI symbols, then `xcodebuild build -scheme Lakko
  -destination 'generic/platform=iOS Simulator'` exited 0 with
  zero errors — the app compiles + links against the rebuilt
  framework.
* **Android** ([`apps/android/.../devices/HistoryBundleCodec.kt`](../../apps/android/app/src/main/kotlin/me/lakko/app/devices/HistoryBundleCodec.kt))
  — `encodePlaceholder` / `decodePlaceholder` over the UniFFI
  `HistoryPlaceholder` data class; `message_id` already BE-u64.
  `:app:compileDebugKotlin` BUILD SUCCESSFUL.

All three feed the same Rust `encode`/`decode`, so the
`payload_bytes` bytes agree and the `message_id` field agrees —
a web↔native history catch-up now interoperates by construction.
A live cross-platform pairing roundtrip is not headlessly
runnable here; the codec-layer determinism + 13 Rust unit tests
+ web tsc + Android `compileDebugKotlin` + iOS `xcodebuild` (with
the rebuilt xcframework) are the evidence. **The cross-platform
interop bug (the G-9 concern and the web↔native break) is
closed.**

**Still open (downgraded to a nice-to-have):** the placeholder
is a canonical *projection* `(message_id, direction, text,
created_at, attachments)`, not the raw
`mls_app::v1::ApplicationPayload` wire bytes. Carrying the raw
bytes instead would be marginally smaller and would unlock a
future "verify-replay" step (re-parse every catch-up message
through the chat-envelope parser). That is a separate,
non-blocking enhancement — the interop and byte-stability
problems are fully resolved without it.

### G-7: DeviceGroup snapshot persistence may briefly diverge across in-flight ratchet operations — **CLOSED**

**Closed (2026-06-03).** The scan flow was reordered to
**persist-before-publish** on all three clients:
`add_device_from_wire` (merges the pending commit, advancing
the local view one epoch) → **persist snapshot** → publish
Welcome → broadcast commit (G-5) → best-effort history
bundle. Writing the advanced snapshot to disk before any
publish closes the window where a crash between the publishes
and the persist left the on-disk envelope an epoch behind
(which would make the next launch attempt to add the same
device twice). A crash after persist but before the publishes
leaves the on-disk state correctly advanced; the joiner
simply never receives the Welcome and retries the QR with a
fresh KeyPackage. Touchpoints: the three
`runScanFlow` / `PairingOrchestrator` files (see G-5).

## Web client

### G-8: Web message-log change listener — **CLOSED**

The unread badge (in `Contacts.tsx`) was sourced from a
one-shot `loadMessageLog(...)` on mount, so a message
written from another tab / the Chat route did not refresh
it until the next mount. A `BroadcastChannel`-backed pub/sub
now closes the gap (`apps/web/src/chat/message-log-events.ts`):
`saveMessageLog` calls `publishMessageLogChanged(accountIdB64)`
after every write, and the badge subscribes via
`subscribeMessageLogChanged` and reloads (filtered to the
active account). `BroadcastChannel` carries the signal across
tabs; because it never echoes to the posting instance, the
publisher also notifies in-process subscribers directly, so the
same-tab / different-route case works too. Degrades to a no-op
where `BroadcastChannel` is absent (the in-process path still
fires). This brings web to parity with the iOS / Android
`@Published` / `StateFlow` in-memory stores. Verified: web
`tsc --noEmit` + `vite build` green.

### G-9: Web `payload_bytes` JSON placeholder strings are NOT byte-stable — **CLOSED**

**Closed (2026-06-04).** `encodeMessagePlaceholder`
([`apps/web/src/devices/history-bundle.ts`](../../apps/web/src/devices/history-bundle.ts))
now serializes via a recursive sorted-key `canonicalJson`
helper instead of `JSON.stringify(m)` (whose key order is
insertion order, not byte-stable across JS engines — a
Chrome-scanner → Safari-joiner pairing on the same account is
a real case). Every `PersistedMessage` field is a string, so
recursive key sorting is the whole of RFC 8785 / JCS that
applies to this shape (no numeric canonicalization needed);
the doc-comment flags that a future numeric field would
require ECMAScript number canonicalization. Decode is
unaffected — `JSON.parse` is order-tolerant — so this is a
pure determinism improvement with no wire-compat change for
the web↔web path. `tsc --noEmit` clean. This matches the
canonical-encoding convention iOS already uses
(`JSONEncoder.outputFormatting = [.sortedKeys]`).

Note: this closes byte-stability of the **web-emitted** bytes;
it does NOT address cross-platform web↔native interop, which
is a deeper, separate problem tracked under G-6 below.

## Backup

### G-10: Backup envelope rotation folded into the recover round-trip — **CLOSED**

After `/recover` rotates the OPAQUE envelope on the
server, the backup baseline upload re-seals the backup
envelope under the new exportKey-derived material. That
upload used to be **fire-and-forget** (web: an un-awaited
`void (async () => …)()` IIFE; iOS: a detached `Task { … }`),
so `recover()` resolved before it landed — leaving a window
in which the server still held the pre-recover envelope
("old phrase still recovers").

The baseline write is now **awaited inside the recover
round-trip** on every platform, so `recover()` does not
resolve until the re-seal has been posted (collapsing the
window to the upload latency): web `AuthContext.recover`
awaits the restore-then-bootstrap IIFE (with the bootstrap
wrapped best-effort so a backup hiccup still cannot fail the
recover); iOS `AuthRepository.recover` calls
`bootstrapBackupBestEffort` directly instead of in a detached
`Task` (it is non-throwing, so awaiting cannot fail recover).
Android already awaited it (a direct `suspend` call in the
`recover` coroutine), so it needed no change. Verified: web
`tsc --noEmit` + iOS `xcodebuild` green.

### G-11: No tamper-evident chain across backup versions — **CLOSED IN PRACTICE (2026-07, [ADR-0037](../adr/0037-backup-head-rollback-commitment.md))**

**Closed (2026-07, [ADR-0037](../adr/0037-backup-head-rollback-commitment.md)).**
Both the returning-client and the fresh-install rollback
cases are closed in practice by one auth-signed
**backup-head** commitment minted on every upload and
verified on every restore.

**Correction to the prior claim.** This entry used to
credit the "client-with-state case" as CLOSED via the v2
chain ([`crates/lakko-backup/src/chain.rs`](../../crates/lakko-backup/src/chain.rs)).
That closure was only *crate-level*: the v2 chain is
**dormant** — it is exposed on neither FFI surface, no
client persists a `last_known_version` anchor, and no
client calls `verify_chain`, so it closed nothing on any
deployed path. ADR-0037 is what closes the case in
practice, via the head — not the chain. The v2 chain
stays **dormant / out of scope**; its distinct
sequence-audit role (walking a *served sequence* of
multiple versions, checking each `prev_hash` link) is a
separate forensics concern this gap does not need.

**How each case is closed.** A `BackupHead` is a compact,
domain-separated, Ed25519-signed blob committing
`(account_id, backup_version, envelope_hash, signed_at)`,
where `backup_version` is an **auth-assigned, per-account,
strictly-monotone** counter (never client-supplied, so a
regression is structurally impossible):

- **Returning client (ever persisted a floor) — HARD-closed.**
  The client stores `last_known_version` in
  device-key-encrypted local storage and refuses any head
  below it — a hard monotonic floor, no window. Auth
  guarantees the version never regresses, so the storage
  tier cannot serve an old head with a spuriously-high
  version to defeat the floor.
- **True zero-anchor fresh install — BOUNDED by freshness.**
  With no floor to compare against, the client falls back
  to the head's signed `signed_at` and a client-side
  staleness window (default 30 days): a rollback to a head
  older than the window is caught; a replay *within* the
  window is not.

**Residual, stated plainly.** A rollback that replays a
genuine *older* head whose `signed_at` falls **inside** the
window is **not** caught — but **only for a true
zero-anchor fresh install** (a genuine lost-everything
recovery). Any client that ever persisted a version floor
is hard-protected regardless of the window. Fully closing
the fresh-install window would require the client to read a
*trusted* current-version marker on restore (e.g. auth
serving its counter over the authenticated channel), a
second trusted round-trip ADR-0037's single-fetch design
deliberately declines. The fail-open on an **absent** head
(a legacy / pre-rollout blob) is a rollback-detection
availability choice only; the envelope stays phrase-AEAD
sealed regardless of the head, so invariant #4 (no silent
downgrade to non-E2EE) is untouched.

**Post-ADR-0037 `lakko-security` pass (APPROVE-WITH-NOTES).**
The review found no CRITICAL / HIGH and no invariant-#4 break
on the live path; the F1–F5b hardenings that landed from it
tighten the gate so the code now matches the residual wording
above rather than being looser than it. **F4** (all three
clients): the restore gate's ABSENT-head case no longer
silently fail-opens for a *returning* client. New policy —
absent head + no floor → fail-open (a true zero-anchor fresh
install, unchanged); absent head + a persisted floor +
`BLAKE3(legacy envelope) == floor.envelope_hash` → proceed
(the head was stripped benignly in transit, byte-identical to
the last known one); absent head + a floor + a **different**
envelope → user-acknowledged stale-override (a head-stripping
downgrade against head-protected history). So the earlier code,
in which the absent path bypassed the floor, is corrected — the
within-window fail-open residual is now genuinely confined to a
**true zero-anchor fresh install**, exactly as this entry's
"any client that ever persisted a floor is hard-protected"
claim already stated. **F5a** (all three clients): the floor
read distinguishes absent vs present vs **corrupt** — a
present-but-undecryptable / tampered floor routes to
stale-override, never a silent demote to the weaker
fresh-window path. **F2** (all three clients): the restore gate
is now mandatory (the ungated no-verification restore path was
removed). **F1** (all three clients): a loud warning when the
head is verified against an unpinned (JWKS-served) attest key.
**F3** (Android only): the `parseBackupBlob` catch widened to
`Throwable` — a clean hard-fail, not an uncaught escape. **F5b**
(auth): a per-account, **fail-open** rate-limit on
`POST /auth/backup/head` (`backup_head_mint_limits`, migration
`0016`), shipped **DEFAULT-OFF** (`LAKKO_BACKUP_HEAD_MINT_RATE_LIMIT`
default `0`; armed with a positive value only after live-DB
verification of the window-reset upsert).

Touchpoints: `crates/lakko-attest/src/backup_head.rs`
(`BackupHead` primitive, 129-byte layout, domain
`lakko/backup-head/v1`);
[`services/auth`](../../services/auth/src/backup_head.rs)
`POST /auth/backup/head` (stateful mint) + the
`backup_head_versions` monotone-counter table (migration
`0015`); `crates/lakko-backup/src/head_frame.rs` (the
`0xB1` container-tag framing embedding the head in the one
opaque storage blob — `services/storage` untouched); the
three-client restore gate (web
[`head-gate.ts`](../../apps/web/src/backup/head-gate.ts),
iOS `BackupHeadGate.swift`, Android `HeadGate.kt`) applying
the absent → fail-open / invalid → hard-fail / stale →
user-acknowledged-override policy. Fuzz:
`lakko-attest/backup_head_open` +
`lakko-backup/backup_blob_parse`. See C-58 in the
invariant index. Residual for a full close: the deferred
auth-served current-version marker (above) + a live
recovery e2e (mint → framed PUT → restore gate) on a
deployed environment.

## Auth

### G-12: Session token validation is single-process in-memory — **CLOSED (was already addressed; text was stale)**

**Closed (2026-06-03) — verified already implemented.** The
described per-process in-memory bearer-token map no longer
exists (and a code audit confirms there is none anywhere in
`services/auth` / `services/discovery` — the only `HashMap`
is discovery's IP rate-limit buckets). Session tokens are the
**stateless** HMAC-SHA256 shape in
[`crates/lakko-auth-token`](../../crates/lakko-auth-token/src/lib.rs):
`v1.<account_id_b64>.<expires_at_ms>.<tag>`, where the tag is
HMAC-SHA256 over the prefix under a 32+-byte secret loaded
from the shared `LAKKO_SESSION_TOKEN_SECRET` env var. Both
`services/auth` (issue + `webauthn.rs` / `opaque.rs` verify)
and `services/discovery` (`auth.rs` verify) load the SAME
secret, so:

- **Any instance validates without coordination** — exactly
  G-12's ask. No DB row, no shared cache, no sticky sessions.
- **Restarts don't invalidate tokens** — the secret is
  config, not process state.

Verification is constant-time (`subtle::ConstantTimeEq`),
version-pinned (`v1`), and expiry-checked; 11 unit tests
cover round-trip, wrong-secret, tamper, expiry, malformed
shapes. The one explicit non-goal documented in the crate is
a **revocation list** (a leaked token is valid until its TTL
expires) — that lands "when the threat model expects targeted
token-theft (Phase 2+)" and is a distinct concern from the
"single-process" claim this gap raised.

### G-13: WebAuthn passkey per-credential authentic-action attestation — **CLOSED**

**Closed.** WebAuthn action-binding landed via
[ADR-0014](../adr/0014-webauthn-action-binding.md) (challenge =
`BLAKE3_keyed(action_hash ‖ nonce)`, `POST /auth/action/{start,finish}`,
single-use server-side grants gating delete-passkey, client `assertAction`
on web + iOS + Android) and the device-removal half via
[ADR-0015](../adr/0015-device-removal-authorization.md) (peer-verified
`lakko-attest` Ed25519 attestation carried alongside the DeviceGroup Remove
commit + verified in the `lakko-mls` apply path, auto-enabled on first
passkey). The original framing below predates that work.

Historical context: the passkey assertion validated the user has the
device; it didn't bind to the specific OPAQUE message the request carries. A
reorder attack inside the same TLS session is implausible but not impossible
— Phase 2c can mix the OPAQUE message hash into the WebAuthn assertion
challenge.

**Server-side implementation landed (2026-06-04) —
[ADR-0014](../adr/0014-webauthn-action-binding.md).** A spike
confirmed the cryptographic binding is not a small change:
`webauthn-rs 0.5.5` (and the low-level `webauthn-rs-core`)
randomise the challenge internally with no injection hook, and
`AuthenticationState.challenge` is `pub(crate)` — so
RP-controlled `challenge = H(action_hash ‖ nonce)` requires
either owning verification or a fork. ADR-0014 chose a
**minimal vendored fork** of `webauthn-rs-core`
(`crates/vendor/webauthn-rs-core`, MPL-2.0, consumed via
`[patch.crates-io]`) that adds one challenge-injection method
(`generate_challenge_authenticate_with_challenge`, takes the
challenge as raw `Vec<u8>`) while reusing upstream's
`authenticate_credential` verification **verbatim**. Shipped in
`services/auth`:

- the action-challenge derivation primitive
  `BLAKE3_keyed(derive_key("lakko/webauthn-action/v1"),
  action_hash ‖ server_nonce_32)` (`webauthn_action.rs`);
- `start_action_auth` / `finish_action_auth` over the fork;
- the bearer-authenticated `POST /auth/action/{start,finish}`
  endpoint pair, backed by migration `0008_webauthn_action.sql`
  (`webauthn_states.action_hash`, kind = 3) with single-use
  state deletion, account binding, and a defence-in-depth
  `action_hash` re-check on `/finish`;
- property tests driving a real software authenticator
  (`webauthn-authenticator-rs` `SoftPasskey`, dev-only) that
  prove a correctly-bound assertion verifies and an assertion
  produced for action A is **rejected** on action B's session.

The existing username/discoverable login flows stay on the
safe high-level `Webauthn`. The fork-delta guard (ADR
test-plan item 5) landed as
`services/auth/tests/fork_delta_guard.rs`: it pins the SHA-256
of the vendored `core.rs` plus structural assertions, so any
edit to the verification path fails CI.

**Client-side wiring landed** (web / iOS / Android) for two
sensitive actions, each gated on a fresh passkey gesture
cryptographically bound to the action via
`action_hash = SHA-256("lakko/action/<op>/v1" ‖ identifier)`:
*delete-passkey* (identifier = credential id) is a strict gate;
*remove-device* (identifier = the removed DeviceGroup leaf's
MLS signature public key) is conditional — required only when
the account has a passkey (a 404 / `NoCredentials` from the
ceremony start means OPAQUE-only → proceed), and fail-closed on
a user-cancelled gesture. `assertAction` reuses each platform's
existing passkey-assertion machinery (web `navigator.
credentials.get`, iOS `ASAuthorizationController`, Android
`CredentialManager`).

**Server-side enforcement landed for delete-passkey.** The gate
is no longer client-side-only there: `/auth/action/finish` writes
a short-lived single-use grant `(account_id, action_hash)`
(table `webauthn_action_grants`, migration 0009, TTL 120 s,
pruned on the existing timer), and `DELETE
/webauthn/credentials/:id` recomputes `action_hash =
SHA-256("lakko/action/delete-credential/v1" ‖ credential_id)`,
atomically consumes a matching grant (`DELETE … rows_affected ==
1`, so it is race-free + strictly single-use), and refuses with
403 `action-authorization-required` if none is present. A stolen
session token alone cannot mint a grant (that needs a passkey
gesture at `/finish`), so the gate now resists token replay. The
no-op case (deleting a credential that does not exist) stays an
idempotent 204 with no grant required. A unit test pins the
server's `delete_credential_action_hash` to the clients'
convention so the two cannot drift, and a DB-gated HTTP
integration test (`action_grant_e2e_tests`, gated on
`LAKKO_AUTH_TEST_DATABASE_URL`, skips when no DB is provided)
drives a real `SoftPasskey` authenticator through the production
router end-to-end: it proves a ceremony writes a grant, the
delete consumes it (204), a delete without a grant is refused
(403), and a non-existent credential is an idempotent 204.

**Remove-device — analysed in [ADR-0015](../adr/0015-device-removal-authorization.md);
peer-verified attestation in progress.** A device removal is an
MLS `Remove` commit signed by the removing device's per-device
MLS signature key, whose seed is stored encrypted under a
non-extractable device key (invariant #7). So a remote attacker
holding only a stolen session token **cannot** forge a removal —
peers reject an invalidly-signed commit — which already closes the
token-replay threat that motivated the delete-passkey grant. The
DS is zero-trust opaque and cannot interpret an `action_hash`, and
the auth service is never in the removal path, so there is no
server request to gate. The **residual** the current client-side
gate does not cover is **on-device malware that bypasses the app
UI and drives the MLS FFI directly**.

ADR-0015 was accepted to close that residual via **peer-verified
attestation**: `services/auth` signs a compact Ed25519 attestation
over `(account_id, action_hash, expiry)` on `/auth/action/finish`,
the removing device carries it alongside its `Remove` commit, and
the receiving devices verify it in the `lakko-mls` apply path
before applying. **Sprints 1–2 landed.** Sprint 1: the
`lakko-attest` crate (`ActionAttestation` format + `sign` +
strict `verify_bound`, 7 unit/property tests). Sprint 2: a
dedicated Ed25519 attestation key in `services/auth` (migration
0010 `action_attest_signing_key`, generated on first boot,
separate from the call-JWT key), `/auth/action/finish` now signs
+ returns the attestation (`attestation_b64`, 1 h TTL), and the
verifying key is published at `GET
/.well-known/device-attest-jwks.json` — the DB-gated HTTP e2e test
verifies the issued attestation under the published key + its
(account, action) binding. The **sprint-4 OpenMLS feasibility
spike** also ran and **confirmed** the OPAQUE-only policy flag is
viable: `device_group.rs::spike_custom_group_context_extension_propagates_to_joiners`
proves OpenMLS 0.8.1 carries a custom `Extension::Unknown` in the
GroupContext, reads it back, and propagates it to a joining
device — so the application-level fallback is unneeded (the one
requirement: every device's leaf capabilities must advertise the
extension type). **Sprint 4 landed** — the policy flag is wired
into `lakko-mls`: every device `KeyPackage` + `DeviceGroup`
founder now advertises the removal-policy extension capability,
DeviceGroups carry `required_capabilities` for it from creation
(so it can be added later), `DeviceGroup::set_require_attested_removals`
flips it on via a `GroupContextExtensions` commit, and
`require_attested_removals` reads it from authenticated group
state; a property test proves the flip propagates to a joined
device through `process_incoming`. (Format note: this changes
DeviceGroup creation + KeyPackages — pre-existing groups/KPs
would need re-keying to gain the capability; immaterial pre-launch.)
**Sprint 3 landed** — the apply-path verification:
`Group::process_incoming_gated` exposes a Remove commit's removed
`(leaf, signature pubkey)` pairs to a gate *before* merging, and
`DeviceGroup::process_incoming_verifying_removals` uses it to
**refuse** (not merge) a removal when `require_attested_removals`
is on and a removed leaf lacks a valid `lakko-attest` attestation
bound to this account + `SHA-256("lakko/action/remove-device/v1"
‖ removed_leaf_pubkey)` + fresh. Six property tests cover valid /
missing / wrong-leaf / wrong-key attestations, policy-off
pass-through, and non-remove-commit pass-through; a refused
removal leaves the device present (commit not merged). So
on-device malware that evicts a device without a real passkey
gesture can no longer make honest peers drop it. **Sprint 5
(FFI surface) landed** — `set_require_attested_removals`,
`require_attested_removals`, `process_incoming_verifying_removals`,
and the free `remove_device_action_hash` are exposed on both the
UniFFI (Swift / Kotlin) and wasm-bindgen (web) DeviceGroup
surfaces (regenerated bindings carry all four; FFI-crate tests
green). The **web transport primitives** are in place — a
DeviceGroup journal envelope codec (`apps/web/src/devices/journal-envelope.ts`,
`version ‖ attestation_len ‖ attestation ‖ mls_message`) that lets a
Remove commit carry its attestation through the opaque journal, a
device-attest JWKS client (`apps/web/src/auth/device-attest-key.ts`)
that fetches the 32-byte verifying key, and the `attestation_b64`
field surfaced on the `ActionFinishResponse` type. The **web
client wiring landed** — `Devices.tsx` captures the
`attestation_b64` from the remove-device ceremony, `remove.ts`
wraps it with the Remove commit, every DeviceGroup `publishGroup`
site (add / remove / application message) emits the envelope, and
the `DeviceGroupSyncContext` receive loop unwraps + applies via
`processIncomingVerifyingRemovals` against the fetched device-attest
key (a zero-key fail-closed fallback when the JWKS endpoint is
unreachable: harmless while the policy is off, refuses removals
once it is on). `tsc --noEmit` + `vite build` clean; the wasm
bindings were rebuilt to expose the methods. The **iOS + Android
client wiring also landed** — the same shape over the UniFFI
surface: a Swift / Kotlin `DeviceGroupJournalEnvelope` codec + a
`DeviceAttestKey` JWKS fetch, `DevicesScreen` captures the
attestation, `DeviceRemoval.removeDeviceAt` wraps it, every
DeviceGroup `publishGroup` site (pairing add / remove / sync app
message) emits the envelope, and the `DeviceGroupSyncRepository`
receive loop unwraps + applies via `processIncomingVerifyingRemovals`
against the fetched device-attest key (same zero-key fail-closed
fallback). `xcodebuild` (iOS, after rebuilding the
`LakkoCore.xcframework`) and `:app:compileDebugKotlin` (Android)
both green. The **policy is now turned ON automatically** — all
three clients call `setRequireAttestedRemovals` (a
GroupContextExtensions commit, broadcast over the journal,
idempotent / no-op when already on) right after a passkey is first
registered (`PasskeysSection` on web, `PasskeysViewModel` on iOS +
Android; the shared `enableAttestedRemovalsIfNeeded` helper).
G-13 is thus **implemented end-to-end** — server attestation +
peer-verified apply path (6 `lakko-mls` property tests) + the
3-platform transport + auto-enable on passkey-enable. **Key
pinning landed** — all three `DeviceAttestKey` clients accept a
build-time pinned verifying key (`VITE_LAKKO_DEVICE_ATTEST_KEY` on
web, `Info.plist` `LakkoDeviceAttestKey` on iOS,
`BuildConfig.LAKKO_DEVICE_ATTEST_KEY` on Android); when set they
**never consult the JWKS endpoint**, so a compromised
JWKS-serving tier cannot substitute a key it controls (the
key-isolation deployment posture). Unset → JWKS fetch, unchanged.
The threat boundary: this defends a compromised serving tier, not
a fully-compromised auth service that also holds the signing key
(which could mint valid attestations regardless) — the latter is
the inherent trust G-13 places in the auth service's
passkey-ceremony verification. **Residual (optional, not
blocking):** distributing the pinned key via the *signed*
bootstrap config (vouched by an independent trust anchor) instead
of a build constant — needs the client-side bootstrap-config
consumption (a Phase-5 transport concern) that does not exist
yet, so it is out of G-13's scope.

## Calls

### G-17: Frame-counter exhaustion path is test-exercised — **CLOSED**

`ParticipantKey::seal` honours the RFC 9605 §4.4 32-bit
counter bound and surfaces `Error::CounterExhausted` when
exhausted; the caller is expected to trigger a fresh MLS
commit to re-key. A dedicated test that grinds 4 billion
seal ops is infeasible in CI; instead the test trio in
`crates/lakko-calls/src/sframe.rs` uses a `#[cfg(test)]`-
only `set_counter_for_test` accessor to land on the edge
in O(1):

- `counter_exhaustion_returns_typed_error` — counter at
  `u32::MAX - 2`: two seals succeed landing on
  `frame_counter = u32::MAX - 1` then `u32::MAX`; the
  third refuses with `Error::CounterExhausted` (no panic,
  no silent 33-bit wrap, no nonce re-use).
- `counter_max_frame_still_opens_on_receiver` — the last
  legal `frame_counter = u32::MAX` authenticates correctly
  on a receiver re-deriving the same `ParticipantKey`
  (no off-by-one in nonce composition at the boundary).
- `counter_exhaustion_is_idempotent` — 16 successive seal
  attempts past exhaustion all return the typed error and
  the counter stays at `u32::MAX` (never wraps to 0,
  which would re-use the very first nonce in this key's
  lifetime).

The test-only accessor is compiled out of the release
crate via `cfg(test)` so no production caller can reset
the counter and re-use an AEAD nonce.

### G-18: Overlap-window / mid-call rekey — **CLOSED (2026-07, [ADR-0029](../adr/0029-mid-call-key-rotation.md)); only the deployed-SFU live-rekey e2e deferred (like G-26)**

`GroupCallSession::drop_previous_call_key` is invoked by
the platform's own timer (iOS `Task.sleep` / Android
coroutine delay / web `setTimeout`). The Rust core stays
wall-clock-free per design (testability, no time-source
dependency). Each platform's timer correctness is reviewed
alongside the WebRTC pod integration in 3a9 / 3a10
followups; a "platform forgot to drop" would extend the
overlap window indefinitely and weaken seed-boundary
forward secrecy until the next rotation.

Mitigation: the platform's overlap timer is the same
trivial-to-audit `setTimeout(drop, 1000)` shape on every
client; failure mode is local-to-that-client (an attacker
cannot cause it remotely).

**All three platforms: helpers shipped.**

- Web — [`apps/web/src/calls/overlap-timer.ts`](../../apps/web/src/calls/overlap-timer.ts):
  `OverlapTimer` class wrapping `setTimeout`.
- iOS — [`apps/ios/Lakko/Calls/OverlapTimer.swift`](../../apps/ios/Lakko/Calls/OverlapTimer.swift):
  Final class with `Task.sleep` schedule, NSLock-guarded
  state, `@Sendable` callback.
- Android — [`apps/android/app/src/main/kotlin/me/lakko/app/calls/OverlapTimer.kt`](../../apps/android/app/src/main/kotlin/me/lakko/app/calls/OverlapTimer.kt):
  `kotlinx.coroutines.delay` schedule, `Mutex`-guarded
  state, scope-bound lifetime.

All three share the same API surface (`arm(callback)`,
`cancel()`, `state`/`isArmed`, three-state machine
`disarmed`/`armed`/`fired`), the same single-shot
"newer arm replaces prior" semantics, and the same
TTL bounds (`[50 ms, 10 s]`, default 1 s per ADR-0008).
Wiring them into a live call flow lands with the SFU
integration in 3b.

**Expiry decision moved into the core (Jun 2026).** The
per-platform timer no longer owns the 1-second policy:
`GroupCallSession::refresh_call_key_at(group, identity,
now_ms, overlap_ms)` stamps the retained previous key with
`now_ms + clamp(overlap_ms)` (bounds `OVERLAP_MIN_MS` ..
`OVERLAP_MAX_MS`, default `OVERLAP_DEFAULT_MS` = 1 s), and
`prune_expired_previous(now_ms)` drops it once the caller's
clock passes that instant (`has_live_previous(now_ms)`
reports the window). The core stays time-source-free — the
platform supplies only `now` — while the TTL bounds + the
1-second decision are owned + property-tested in one place
(5 tests in `group_call.rs`: retain-until-expiry, clamp,
no-clock-never-expires, second-rotation-resets-window,
saturating-add). The legacy no-clock `refresh_call_key` +
manual `drop_previous_call_key` remain for callers driving
their own timer. **The duplicated-logic part of G-18 is now
closed; the residual is just wiring the platforms onto
`refresh_call_key_at` + a clock-tick `prune_expired_previous`
when the live SFU call flow lands (3b).**

**CLOSED (2026-07, [ADR-0029](../adr/0029-mid-call-key-rotation.md)).** Mid-call
rekey is now wired end-to-end. A code reconciliation for this gap found the
overlap mechanism was **dormant** — no client instantiated `GroupCallSession`;
the clients seal/open at the raw-seed level, off-thread, without the MLS group.
So the fix was not "wire an existing timer" but to expose the overlap keyer at
the level the clients run at. The overlap logic was extracted into a seed-based
`lakko_calls::CallKeyRing` (current + previous key; `try_open_framed_with_overlap`
tries the current key then, during the window, the previous; `rekey_from_seed_at`
/ `prune_expired` / `has_live_previous`), which `GroupCallSession` **delegates**
to (its 5 property tests stay green, no behaviour change) and which is FFI-exposed
(uniffi + wasm) **and** surfaced in the lean `lakko-sframe-native` C ABI
(`lakko_sframe_call_key_ring_*`, Mutex-guarded) so the native SFrame media shim
can drive it. All three clients now rotate on a mid-call MLS epoch advance: the
chat receive loop re-derives `group.export_secret("lakko/call/v1", call_id, 32)`
at the new epoch and calls the live SFU call's `rekey(newSeed)`, which fans out to
every attached transform — the web `RTCRtpScriptTransform` worker (a `CallKeyRing`
per transform + a `rekey` postMessage + a self-prune timer), and the iOS / Android
native `LakkoFrameEncryptor::Rekey` (a seal-key swap — the send side keeps no
overlap) + `LakkoFrameDecryptor::Rekey` / `Prune` (the ring's overlap open, via
the C++ bridge). Fail-closed throughout: a re-derivation failure keeps the current
key, never plaintext (invariant #4). Verified headless on every stack — `cargo
test` (core `CallKeyRing` 6 property tests + the C-ABI overlap round-trip), the
uniffi (3) + wasm (3) overlap round-trips, the C++ bridge `test_mid_call_rekey_overlap`
(`just test-sframe-bridge`), web `vitest` (`call-key-ring.test.ts`), iOS
`xcodebuild`, Android `:app:compileDebugKotlin`. The only residual is a live
two-peer mid-call-rekey proof over a **deployed** SFU (a runtime gate, deferred
like G-26).

### G-19: SFU-side call-admission JWT validation — **CLOSED** (Rust SFU; LiveKit-fork plan superseded)

**Closed.** The validation side landed in the **Rust SFU** (ADR-0011, which
retired the LiveKit fork referenced below). `lakko-svc-sfu` verifies the
Ed25519 call-admission JWT against the auth service's JWKS on the
`/v1/connect` WebSocket admission path before a peer joins a call; the
dockerised `ci/sfu-e2e` gate exercises exactly this (a fixture JWT →
`Welcome` frame). The "pending 3b1c / Go-side LiveKit fork" framing below is
superseded — there is no Go verifier; the load-bearing path is the Rust
SFU's own `lakko_call_jwt::verify_with_jwks`.

Historical context (pre-ADR-0011):

**Issuance side: shipped in 3b4** — crypto in
[`crates/lakko-call-jwt/`](../../crates/lakko-call-jwt/src/lib.rs),
handlers + key persistence in
[`services/auth/src/call_jwt.rs`](../../services/auth/src/call_jwt.rs).
Mints Ed25519-signed JWTs with the C-15..C-23 invariants
in [`calls.md`](./calls.md). Public key is published at
`GET /.well-known/jwks.json`. Client-side parse + verify
is exposed through the FFI crates so a Lakko client can
self-verify a JWT before presenting it.

**Validation side: still pending 3b1c (Go-side, in the
LiveKit fork)**. The Rust-side `lakko_call_jwt::verify_with_jwks`
is functional and tested + serves as the reference
implementation the Go verifier interops against, but
isn't on the load-bearing SFU path until the LiveKit fork
consumes the JWKS doc.

Operational consequence: today's `POST /call/jwt`
endpoint issues admittedly-bearer tokens but there's no
SFU host to present them to. Clients can already cache
the JWT — the call won't actually open until the SFU is
deployed in 3b2.

### G-20: Call recording is out of scope (deferred per ROADMAP 3c8)

If recording ever lands, the design is "recording client
joins as a regular participant with its own MLS leaf" — no
break to call-side invariants C-1..C-10 in
[`calls.md`](./calls.md). The recorded stream is
end-to-end encrypted just like any other consumer's view.

### G-21: Link-based call invites — **multi-joiner CLOSED end-to-end on web + iOS + Android ([ADR-0032](../adr/0032-multi-joiner-call-links.md): owner-founds + deterministic committer election + guest-account join); residuals are the account-creation-spam throttle (deferred backend arc) + a live deployed-SFU N-joiner/failover e2e (G-26-class)**

**Multi-joiner (ADR-0032) landed 2026-07** — the owner is the sole founder (no
founding race → the original two-groups bug closed by design); joiners mint
their own KeyPackage and are admitted by the `elect_committer`-designated
committer (owner-if-present-else-lowest-leaf, so failover + reclaim are
automatic + split-brain-free); the KeyPackage/Welcome ride the SFU `signal-relay`
in-band (live) or the stranger-inbox welcome-ferry (cold start); an anonymous
`/c/<slug>` visitor joins as a **guest** (a username-less OPAQUE account minted
via the audited PoW-gated `signup` path); an empty owner-founds call is dead
(~18 s timeout). Fail-closed throughout (no media before a valid Welcome + join;
a post-join owner-membership check refuses a redirect into an impostor group).
`lakko-security` **APPROVE-WITH-NOTES**. The single-joiner V1 note below is the
historical baseline this superseded.



`CallInviteCapability` embeds one one-time `KeyPackage`
(see C-26 in [`calls.md`](./calls.md)). The first joiner
to call `accept_call_invite` founds the call MLS group
with the owner already added; a second joiner using the
same capability would call `Group::create` on their own
side and end up in a **separate** MLS group — both would
derive the same `CallKey` seed (same `call_id` exporter
context) but the two groups wouldn't share an epoch, so
the call wouldn't connect.

**Server-side blob storage shipped in 3c3-server** —
`services/discovery /call-invite` + `/call-invite/{blob_id}`
+ `/call-invite/{blob_id}/consume` endpoints (mirror of
the contact-invite ones from 1.18d). Capability bytes
now live server-side keyed by SHA-256(bytes); the URL
becomes `https://lakko.me/c/<blob_id>` instead of
inline-encoded.

**Owner-signed revocation shipped in 3c3-revoke** —
`DELETE /discovery/call-invite/{blob_id}` mirrors
`invite::revoke`. Cross-kind isolation via
`CALL_INVITE_REVOKE_DOMAIN_V1` (distinct from the
contact-invite revoke domain) prevents signature replay
across the two ecosystems.

**Client-side helper trio shipped in 3c3-client** —
`uploadCallInvite(capabilityBytes)`,
`fetchCallInvite(blobId)`,
`consumeCallInvite(blobId, capabilityBytes,
joinerAccountId)`, and `revokeCallInvite(blobId,
signingSeed)` on all three clients
(`apps/web/src/discovery/client.ts`,
`apps/ios/Lakko/Discovery/DiscoveryClient.swift`,
`apps/android/.../discovery/DiscoveryClient.kt`). Wire
shapes reuse the contact-invite envelopes — the server
emits byte-identical JSON for both ledgers, only the URL
path + revoke signature domain separator differ.

**URL plumbing shipped in 3c3-url-helpers** — the
`<origin>/c/<slug>` URL form + parser on three platforms.
The byte-count discrimination contract is C-30 in
[`calls.md`](./calls.md): exactly 32 bytes ⇒ pointer
(SHA-256 `blob_id`); ≥ 400 bytes ⇒ inline (censored-region
fallback that skips the server round-trip); anything else
is rejected as `slugWrongLength`. 25 JUnit cases on Android
pin the discrimination + the constants-vs-web-side drift
sentinels (`CALL_INVITE_URL_PREFIX == "/c/"`,
`CALL_INVITE_BLOB_ID_LEN == 32`,
`CALL_INVITE_INLINE_MIN_LEN == 400`).

**Composer + acceptor orchestration shipped in
3c3-{composer,acceptor}-helper** — one-call `composeAndUpload(...)`
(5 steps: mint `call_id` → mint one-time KP →
`composeCallInviteCapability` via FFI → `uploadCallInvite`
via HTTP → build URL) and `acceptFromSlug(...)` (7 steps
collapsed into an 8-variant discriminated `Result`) on web
/ iOS / Android. The acceptor's defence-in-depth
re-verify (`decodeAndVerify` runs locally AFTER the server
already verified at upload time, so a malicious server
cannot substitute bytes) and `404 → notFound` folding
(denies status-code probing of the `blob_id` space) are
pinned by the C-31 invariant. The acceptor's
`Result.accepted` exposes the 32-byte `callKeySeed` so UI
seals the joiner's first SFrame frame before the owner has
joined — no second FFI round-trip needed.

**Welcome-ferry wire path shipped in
3c3-welcome-ferry** — the joiner-side `acceptCallInvite`
flow's `welcomeForOwner` bytes now reuse the existing
`ContactRequest` envelope transport (HPKE-sealed to the
owner's identity key, routed via the
`StrangerInbox::derive(owner_account_id)` pseudonym).
Joiner-side: wraps the Welcome bytes with the 22-byte
domain separator
`CALL_WELCOME_ENVELOPE_DOMAIN_V1 = b"lakko/call-welcome/v1\x00"`
via [`lakko_contacts::call_welcome::wrap_call_welcome`](../../crates/lakko-contacts/src/call_welcome.rs)
before HPKE-sealing through the standard
`ContactRequest.compose(...)` path. Owner-side: polling
loop calls
[`try_unwrap_call_welcome(plaintext)`](../../crates/lakko-contacts/src/call_welcome.rs)
on every decrypted envelope and dispatches on a
three-way discriminator — `Some(welcome)` → drive
`Group::join_from_welcome(welcome)` and enter the call
group; `None` → render as a plain intro message
(ADR-0005 scenario A); `Err(TruncatedPayload)` → log +
drop a malformed envelope. The separator is asserted
distinct from all sibling domain prefixes (contact-invite
revoke, call-invite revoke, joiner-tag salt) by the
`separator_distinct_from_contact_invite_domains` test.
The reuse means **zero new server surface** — the
existing PoW + Privacy Pass anti-abuse gate on
`ContactRequest` ingress carries over for free, and the
server still sees only `(pseudonym → opaque ciphertext)`.
FFI exposure shipped on both wasm-bindgen
(`wrapCallWelcome` / `tryUnwrapCallWelcome` /
`callWelcomeMinPayloadLen`) and UniFFI surfaces. See
**C-32** in [`calls.md`](./calls.md) for the full
invariant set + 9 property tests in
`lakko_contacts::call_welcome::tests`.

**Platform-side wiring still pending** — the wrap /
unwrap helpers are in place but the platform polling
loops on web / iOS / Android still need to (a) call
`tryUnwrapCallWelcome` on every decrypted
`ContactRequest` they pull from the stranger inbox, (b)
dispatch a Welcome to `Group::join_from_welcome` instead
of the intro-message renderer, (c) transition the call
UI from "waiting for owner" to "in-call" once the
Welcome lands. This is straightforward glue work
deferred to the platform-UI sprint; the cryptographic
contract it depends on is fixed.

**Subsequent-joiner KP-forward wire path shipped in
3c3-kp-forward** — sibling envelope of call-welcome,
under a distinct domain separator
`CALL_KP_FORWARD_ENVELOPE_DOMAIN_V1 = b"lakko/call-keypackage/v1\x00"`.
Subsequent joiners wrap their fresh `KeyPackage` via
[`wrap_call_kp_forward`](../../crates/lakko-contacts/src/call_kp_forward.rs)
and ferry it through the same `ContactRequest` envelope
transport to the call group's current committer. The
committer's polling loop dispatches in order:
`try_unwrap_call_welcome` → `try_unwrap_call_kp_forward` →
intro-message fallthrough. The no-cross-dispatch property
([`welcome_and_kp_forward_round_trips_do_not_cross_dispatch`](../../crates/lakko-contacts/src/call_kp_forward.rs))
guarantees no payload can match two branches at once. See
**C-33** in [`calls.md`](./calls.md) for the full
invariant set + 10 + 5 tests.

**Owner-side add-member orchestration still pending** —
the committer reads the unwrapped KP and runs MLS
`Group::add_member_from_wire(kp_bytes)` + commits, then
ferries the resulting Welcome back to the subsequent
joiner via [`wrap_call_welcome`](../../crates/lakko-contacts/src/call_welcome.rs)
(same C-32 envelope). The MLS operations + the platform-
side polling-loop wiring is straightforward glue
deferred to the platform-UI sprint; the cryptographic
contract is now fully fixed for both halves of the
multi-party handshake.

**Owner-handover protocol shipped in 3c3-handover** —
the committer-leaves scenario now has a pinned policy:
the capability's `owner_account_id` designates the
**owner as the primary committer-of-record**. When the
first joiner disconnects, the owner drives the Remove
commit for the stale leaf (OpenMLS does not permit
self-removal via `remove_members`, and a disconnected
client can't drive `leave_group_via_self_remove`
anyway). Subsequent KP-forwards then route to the owner
as the new committer. Across the leave + add commits
the MLS exporter produces a fresh CallKey seed each
epoch (forward secrecy assertion). The integration test
[`link_call_owner_handover_after_committer_leaves`](../../crates/lakko-calls/src/link_invite.rs)
exercises the full 4-phase sequence (A↔B↔C baseline →
Alice removes Bob → Dave joins via Alice → all three
at same post-handover epoch + byte-identical CallKey
seeds + pre-leave seed differs from post-handover
seed). See **C-36** in [`calls.md`](./calls.md) for the
full invariant set.

The owner-handover test surfaced + fixed a load-bearing
MLS bug: `Group::join_from_welcome` was using
`MlsGroupJoinConfig::default()` which set
`use_ratchet_tree_extension = false` on joined groups,
so a member who joined via Welcome and later committed
add (the exact owner-handover scenario) produced a
Welcome that didn't carry the ratchet tree → the new
joiner failed with `MissingRatchetTree`. The one-line
fix in `lakko-mls::Group::join_from_welcome` flips the
flag on, matching the founder's `MlsGroupCreateConfig`,
so a chain of welcome-then-add-then-welcome propagation
now survives any number of hops.

Operational impact: V1 1:1 AND multi-party (3+) link
calls have a **complete protocol-level wire path** in
all directions including the committer-leaves
handover. The full cryptographic state machine is
backed by named integration tests (C-35 happy path,
C-36 handover path). What remains for V1 is purely
platform-UI glue: composer view, `/c/<slug>` landing
page, polling-loop integration with
`dispatch_incoming_envelope`. The cryptographic
contracts the UI work depends on are all fixed and
fuzz-covered.

### G-22: Per-joiner dedup for call invites — **fully CLOSED ([ADR-0032](../adr/0032-multi-joiner-call-links.md)): committer-side per-signature-key admission dedup on all 3 clients via the `keyPackageSignatureKey` FFI helper**

**Landed with the ADR-0032 multi-joiner flow (2026-07)** — the acting committer
dedups admissions on `(relay from-peer-id)` + exact-KeyPackage-bytes, so one
SFU-admitted peer is added at most once even if it re-mints fresh KeyPackages
(a retry re-serves the stored Welcome); the server `max_uses` on the
`/c/<blob_id>` blob is the orthogonal ceiling. **Per-signature-key dedup landed
(2026-07):** `lakko_mls::key_package::signature_key` extracts the stable leaf
(Ed25519) key from a wire KeyPackage (via `KeyPackageIn::unverified_credential`)
+ FFI `keyPackageSignatureKey` (uniffi + wasm); all three clients now dedup
**primarily** on that stable identity key — so a peer re-minting FRESH
KeyPackages under one identity is added at most once (the two are distinct KP
bytes but share the leaf key) — with the per-`from`-peer + exact-KP-bytes guards
kept as fallbacks. The historical note below is the pre-ADR-0032 baseline.



**Atomic `max_uses` enforcement shipped in 3c3-server**
via `services/discovery/src/call_invite.rs::consume` —
the handler bumps `used_count` only when the row has
remaining budget (same CASE-expression sentinel mapping
the contact-invite consume handler uses: `0 → 1`,
`u32::MAX → unlimited`, otherwise N). Exhausted budgets
surface as `410 Gone`; expired rows fold into `404 Not
Found` to deny status-code probing.

**Per-joiner dedup shipped in 3c3-multi-use** —
[`0008_call_invite_consumers.sql`](../../services/discovery/migrations/0008_call_invite_consumers.sql)
adds the `call_invite_consumers` side table mirroring
`invite_consumers` (PK on `(blob_id, joiner_tag)`,
`FOREIGN KEY ... CASCADE` on `call_invites.blob_id`).
The consume handler runs `INSERT IGNORE` into the table
inside the same transaction as the counter bump; a
repeat from the same joiner conflicts on the PK,
surfaces as `already_consumed = true` in the response,
and does NOT bump the counter.

Joiner-tag computation reuses the existing
[`lakko_contacts::invite::compute_joiner_tag`](../../crates/lakko-contacts/src/invite.rs)
function (HMAC-SHA-256 over a per-blob salt). The salt
is `SHA-256(JOINER_TAG_SALT_DOMAIN_V1 || capability_bytes)`,
so contact-invite and call-invite tags for the same
joiner don't collide even though the helper is shared —
[`lakko_contacts::call_invite::tests::joiner_tag_works_on_call_invite_bytes`](../../crates/lakko-contacts/src/call_invite.rs)
asserts this cross-kind salt isolation.

**KP-republish orchestration for multi-joiner calls
shipped in 3c3-kp-forward + 3c3-incoming-dispatcher** —
the second-and-subsequent-joiner path now has a fully
specified wire chain: joiner wraps a fresh KeyPackage
via `wrap_call_kp_forward`, ferries it through the
shared ContactRequest envelope transport to the current
committer; the committer's polling loop runs
`dispatch_incoming_envelope` which discriminates among
welcome / KP-forward / intro-message kinds; on a KP-
forward hit the committer runs
`Group::add_member_from_wire(kp)` + commits + wraps the
resulting Welcome via `wrap_call_welcome` and ferries
it back to the new joiner. The owner-handover sprint
(C-36) extended this with the policy for "what happens
when the original committer leaves" — the owner
designated in the capability becomes the new
committer-of-record. The full multi-party + handover
flow is backed by two named integration tests:
`link_call_multi_party_e2e_via_kp_forward_envelope`
(3-party rendezvous) and
`link_call_owner_handover_after_committer_leaves`
(committer-leaves + owner takeover). The dedup table
(this G-22's primary scope) composes with the KP-
forward chain cleanly: an already-consumed
`(blob_id, joiner_tag)` short-circuits to the
"already joined" UI branch without re-bootstrapping
the MLS group.

### G-31: Cross-stream SFrame AEAD nonce reuse (multiple streams under one leaf) — **CLOSED (2026-07, [ADR-0035](../adr/0035-sframe-per-stream-kid-key.md); [ADR-0034](../adr/0034-sframe-per-media-stream-key.md) was an incomplete first attempt, superseded)**

A `lakko-security` review of the mid-call camera-on arc
([ADR-0032](../adr/0032-multi-joiner-call-links.md) piece B)
surfaced a pre-existing, system-wide **HIGH**. The SFrame
header KID is `= sender_leaf`, and a participant sends both
audio and video under that one leaf. `CallKey::participant_key`
derived a single `(key, nonce_salt)` from `(seed, sender_leaf)`
alone, so the audio and video `ParticipantKey` instances — each
running an **independent** counter from 0 — sealed audio frame
`N` and video frame `N` under an **identical** `(key, nonce)`
pair on different plaintexts:

- ChaCha20 keystream reuse: `C_audio(n) XOR C_video(n) =
  P_audio(n) XOR P_video(n)` (leaks the XOR of the two streams).
- Poly1305 one-time-key reuse across two messages (tag forgery).

For an **SFU group video call** the zero-trust SFU relays and
holds both ciphertext streams — exactly the adversary invariant
#2 targets — so it is a real confidentiality + integrity break
(**HIGH**). A **1×1 native** call sits behind P2P DTLS-SRTP (no
server in the media path), so the reuse is defence-in-depth only
(low); **web 1×1** has no SFrame and is unaffected. Violates RFC
9605 §9 `(key, CTR)` uniqueness.

**First attempt (ADR-0034, incomplete — superseded).** ADR-0034
folded a 1-byte Audio/Video `MediaType` tag into `participant_key`.
That closed the audio-vs-video pair but **re-collided two video
streams** from one leaf: a participant publishing camera + screen-share
sends **two** `Video` streams, both mapping to the tag `0x02`, so they
re-derived the identical `(key, nonce_salt)` and their independent
counters reopened the exact HIGH reuse. A local media-type/track-kind
cannot distinguish two same-kind streams, and a transceiver MID differs
across the SFU — neither is a discriminator both endpoints share.

**Closure (ADR-0035).** The SFrame header KID becomes a
per-`(sender_leaf, stream)` identifier `KID = (sender_leaf << 8) |
stream_index` (`stream_index` a per-sender `u8` ordinal — audio 0,
camera 1, screen-share 2, …), and `CallKey::participant_key(kid)` folds
the **FULL KID** into **both** HKDF `info` strings. Distinct KID ⇒
disjoint `(key, nonce_salt)` space, so **every** concurrent stream of a
leaf keys into its own space — the reuse is closed for **any** number
of streams per leaf, fully covering the multi-video (camera +
screen-share) case ADR-0034's per-media-type fix did **not**. The KID
is the only per-stream signal both endpoints share through the SFU,
which forwards the RFC 9605 header verbatim (invariant #2); the receiver
reads the KID off each frame's header and derives `participant_key(KID)`
with **no** local media context. The `SframeHeader` codec, the
`header_decode` fuzz target, and the ADR-0013 AAD are unchanged (RFC
9605 §4.3 already encodes an arbitrary `u64` KID in an extended form).
The fix landed and is **cross-stack-verified** on all six SFrame stacks
(core, uniffi FFI, wasm FFI, native C-ABI, C++ bridge, web worker — plus
the iOS Obj-C++ shim and the Android JNI module, whose `SFrameMediaType`
mapping was deleted), each producing the same canonical golden wire
vector
`910300fe2c2af65799594a4c693ff4c9e0d49b0565756ddd7299b0945fc8adb1c9c12fced57c6937f372e6b94bdae40a`
(seed `0x42`×32, leaf 3, `stream_index 0` ⇒ KID `0x0300`). The
`MediaType` discriminator + the native `BadMediaType` status are
removed; the core fails **closed** on `stream_index > 255`
(`StreamIndexOverflow`). Property test:
`sframe::tests::three_streams_of_one_leaf_get_distinct_keys_and_nonces`
(audio + camera + screen-share). This re-words **C-5**
(`docs/security/calls.md`) to per-`(leaf, stream)`. The only remaining
care item is the sender's stable-distinct-`stream_index`-per-stream
discipline (an engineering invariant, not a crypto gap). Residual: the
same G-26 live deployed-SFU RTP interop proof that governs every SFrame
stack.

## Out-of-protocol

### G-14: Hostile-OS / hostile-browser threats

No app-level integrity attestation. We rely on the
platform's standard sandbox + the user's awareness. Phase 5
adds App Attest / Play Integrity / WebAuthn-attestation to
the signup / login round-trip.

### G-15: Side-channel + physical attacks

Out of scope for this audit pass. Mitigated by relying on
the platform's constant-time crypto primitives (`subtle`
crate, OpenMLS's vetted implementations). No hand-rolled
crypto in the codebase — the `lakko-crypto` crate is
plumbing only.

### G-16: Quantum adversaries

MLS suites pin classical curves today. PQ migration
follows the WG.

### G-26: RFC 9605 §4.3 SFrame wire header + framed pipeline + cross-platform AAD; §4.3-conformant, only live deployed-SFU RTP interop pending

**Update (2026-07) — the §4.3 rewrite landed; the header is now RFC 9605 §4.3
wire-compatible.** An earlier revision of `SframeHeader::encode` used a
Lakko-custom first-byte layout (`R | LEN | X | KID` with an always-trailing CTR)
that a 2026-06-30 finding (`lakko-security` APPROVE-WITH-NOTES, verified against
the RFC + its Appendix C.1 vectors) showed was NOT RFC 9605 §4.3. The maintainer
took the §4.3 rewrite: `encode`/`decode` now pack the config byte as the §4.3
`X | KKK | Y | CCC` with inline `KID<=7` / `CTR<=7`, **proven byte-for-byte
against RFC 9605 Appendix C.1's published vectors**
(`header.rs::rfc_9605_appendix_c1_header_vectors`), so `(kid=0, ctr=0)` is now
the single byte `00` exactly as the RFC specifies. Because the header IS the
AEAD AAD (ADR-0013) and it is now spec-conformant, a Lakko frame is
interoperable with any other RFC 9605 implementation (mlspp, Cisco libsframe, a
browser-native SFrame transform) that shares the key + nonce derivation —
external-stack interop is no longer broken by design. The golden vector was
re-pinned + re-verified firsthand across all six stacks (Rust core, the
`lakko-sframe-native` C ABI, the C++ bridge, web wasm, plus the iOS/Android
golden pins run in CI). The change carried no cryptographic risk (encode +
decode flip together; the nonce derives from the counter, not the header
layout). Everything below about cross-RUNTIME byte interop remains true — the
same golden vector, now in the §4.3 encoding. The one residual is a live
cross-platform / external-stack RTP round-trip over a DEPLOYED SFU (a
verification gate, not a design gap).

**Status (2026-06-02):** the framed pipeline + cross-platform AAD landed; the
Lakko-internal cross-runtime interop is proven (golden vector); the live
deployed-SFU RTP smoke is the only deploy-gated residual. Progress since
the 2026-05-25 entry:

1. **Wire serializer.**
   [`crates/lakko-calls/src/header.rs`](../../crates/lakko-calls/src/header.rs)
   ships `SframeHeader::encode` / `SframeHeader::decode`
   (RFC 9605 §4.3: config byte `X | KKK | Y | CCC` + inline
   `KID<=7` / `CTR<=7`; round trip + pinned wire layouts +
   truncation handling + bit-packing first-byte invariants +
   the RFC 9605 Appendix C.1 published-vector conformance
   test). See the Update above.
2. **Framed seal/open pipeline — DONE.**
   [`lakko_calls::sframe`](../../crates/lakko-calls/src/sframe.rs)
   now exposes `ParticipantKey::seal_framed` (peeks the
   counter, builds the header, seals with the header as
   AEAD AAD, returns `header || ciphertext`) and
   `open_framed` (decodes the header, binds the EXACT
   on-wire header bytes as AAD, checks `kid == sender_leaf`,
   opens). 7 property tests added on top of the prior
   suite. The SFU reads the KID off the wire to route by
   participant without touching the ciphertext.
3. **Cross-platform AAD contract — [ADR-0013](../adr/0013-sframe-cross-platform-aad.md).**
   The AEAD AAD is the SFrame wire header (KID + CTR), NOT
   any host RTP `additional_data`. This is the property
   that lets a frame sealed on one platform open on
   another: the host metadata is not reconstructible
   across stacks, so it cannot be in the authenticated set.
4. **All three client surfaces consume the framed path.**
   The flat C ABI (`lakko-sframe-native`:
   `lakko_sframe_seal_framed` / `_open_framed`), UniFFI
   (`ParticipantKey.seal_framed` / `open_framed`), and
   wasm-bindgen (`sealFramed` / `openFramed`) all expose
   it. The native libwebrtc C++ bridge
   (`apps/shared/sframe-bridge`) calls `seal_framed` /
   `open_framed` directly (host `additional_data`
   intentionally unbound); its 8-assertion host test
   (`just test-sframe-bridge`) proves both that a
   mismatched host AAD does NOT fail decryption and that a
   tampered header DOES. The browser side is the
   `RTCRtpScriptTransform` worker
   (`apps/web/src/calls/sframe-transform.ts` +
   `sframe-transform-worker.ts`) calling `sealFramed` /
   `openFramed` inside a module worker.

**What is still open — live cross-platform RTP verification.**
The byte-level interop is asserted by construction (the
header is platform-independent; native + web call the same
`lakko-calls` core through the same framed API) and by the
native host test. What has NOT been observed firsthand is a
real media frame sealed on one platform (e.g. a Chrome
`RTCRtpScriptTransform` sender) decrypting on another (e.g.
an iOS / Android libwebrtc receiver) over a live SFU
forwarding path. That requires two real endpoints plus the
SFU in the middle and cannot be done in a headless build
environment.

**Narrowed (2026-06-12).** The "does an SFrame frame survive
the live SFU at all?" question is now answered firsthand in a
headless test:
[`sframe_sealed_frame_round_trips_through_the_live_sfu`](../../services/sfu/tests/synthetic_client_e2e.rs)
seals a frame with the real `lakko_calls::sframe`
`seal_framed`, pushes it through the production
`spawn_media_task` over a real `str0m` ICE + DTLS handshake +
the cross-fanout renegotiation, and the subscriber
`open_framed`s the forwarded bytes back to the exact
plaintext — so the RFC 9605 §4.3 header (the AAD) and the
ciphertext provably survive the SFU forward intact, and the
SFU stays SFrame-opaque (`lakko-calls` is a **dev-only** dep
of `services/sfu`; the production binary never links it,
preserving C-39).

**Effectively closed (2026-06-12) — cross-runtime byte interop
proven by golden vector.** The cross-*runtime* leg no longer
rests on "by construction": a single canonical SFrame wire —
sealing `(seed=[0x42;32], leaf=3, plaintext="lakko sframe
golden vector v1")` on a fresh `ParticipantKey` (counter 0) —
is now pinned and asserted on ALL FOUR stacks, each of which
both SEALS to the identical bytes and OPENS them back to the
plaintext: the Rust core
([`sframe.rs::golden_vector_*`](../../crates/lakko-calls/src/sframe.rs)),
the wasm/web `RTCRtpScriptTransform` stack
([`sframe-golden.test.ts`](../../apps/web/src/calls/sframe-golden.test.ts),
vitest), the iOS UniFFI stack
([`SFrameGoldenTests.swift`](../../apps/ios/LakkoTests/SFrameGoldenTests.swift)),
and the Android UniFFI stack
([`SframeGoldenTest.kt`](../../apps/android/app/src/androidTest/kotlin/me/lakko/app/calls/SframeGoldenTest.kt)).
The Rust, web, and iOS golden tests were verified firsthand
(iOS live on a simulator); the Android instrumented test
compiles and runs in CI (`g3-4-mobile.yml` →
`connectedDebugAndroidTest`); the Rust + web tests gate every
CI run (`cargo test --workspace` + vitest). Because every stack
produces and consumes the byte-identical wire, a frame sealed
on any stack opens on any other — the cross-runtime interop is
now a *proven equality*, not an assumption. Composed with the
live-SFU verbatim-forwarding test above, the
sealed-on-web → relayed-by-SFU → opened-on-native path is
established by correctness: the SFU forwards the wire
unchanged, and that exact wire is what every native stack
opens.

**Composition now observed in one headless test (2026-06-15).**
The "two already-proven facts" no longer need to be composed
mentally: [`golden_vector_round_trips_through_the_live_sfu`](../../services/sfu/tests/synthetic_client_e2e.rs)
seals the EXACT canonical golden vector (`seed=[0x42;32]`,
`leaf=3`, `"lakko sframe golden vector v1"`), asserts the sealed
wire equals the byte-pinned `GOLDEN_WIRE_HEX` that the Rust /
web / iOS / Android golden tests all pin, pushes THOSE bytes
through the production `spawn_media_task` over a real `str0m`
ICE + DTLS + cross-fanout renegotiation, and the subscriber
`open_framed`s the forwarded bytes back to the golden plaintext.
So the bytes a real web / iOS / Android client emits ARE the
bytes observed to cross the live SFU intact — the composition is
a single firsthand-verified fact, not a mental join. Verified
firsthand (`cargo test -p lakko-svc-sfu --test
synthetic_client_e2e` → 5 passed) and gates every CI run.

**Native C ABI golden pin added (2026-06-17).** The flat
`lakko-sframe-native` `extern "C"` surface — the path the iOS
Obj-C++ shim + Android JNI module actually call before handing
bytes to libwebrtc's `FrameEncryptorInterface` /
`FrameDecryptorInterface` — is now itself pinned to the canonical
golden in
[`crates/lakko-sframe-native/tests/golden_cross_stack.rs`](../../crates/lakko-sframe-native/tests/golden_cross_stack.rs):
`lakko_sframe_seal_framed` over the golden seed / leaf / plaintext
produces the EXACT `GOLDEN_WIRE_HEX`, and `lakko_sframe_open_framed`
recovers the golden plaintext from that wire. Previously the C ABI
was only opacity-tested with arbitrary per-test seeds
(`sfu_opacity_e2e.rs`); the cross-stack golden was pinned on the
core + web/wasm + iOS/Android UniFFI surfaces but NOT on the C ABI
the native media shims wrap. This closes that last in-codebase
coverage gap — core, web (wasm), UniFFI, the native C ABI, and the
live `str0m` SFU round-trip now ALL agree on the one canonical wire.
Verified firsthand (`cargo test -p lakko-sframe-native --test
golden_cross_stack` → 3 passed).

The C++ bridge ONE LAYER up is now golden-pinned too: a
`test_cross_stack_golden_vector` assertion in
[`apps/shared/sframe-bridge/test_bridge.cc`](../../apps/shared/sframe-bridge/test_bridge.cc)
drives the actual `LakkoFrameEncryptor::Encrypt` /
`LakkoFrameDecryptor::Decrypt` (the concrete `webrtc::FrameEncryptorInterface`
/ `FrameDecryptorInterface` impls the iOS Obj-C++ shim + Android JNI module
wrap before handing bytes to libwebrtc) over the golden vector — `Encrypt`
produces the EXACT `GOLDEN_WIRE_HEX` (even with a non-empty host
`additional_data`, re-proving ADR-0013 host-AAD-independence) and `Decrypt`
recovers the golden plaintext. Verified firsthand (`just test-sframe-bridge`
→ 9 assertions pass). So every layer of the native media stack — the C ABI,
the C++ bridge, and the webrtc-interface impls — is proven byte-identical to
web / iOS-Android-UniFFI / the core, all the way down to the libwebrtc handoff.

The only residual is a single monolithic real-Chrome ⊕
real-native ⊕ deployed-SFU smoke run — redundant for
correctness (it would only re-observe the same composition with
real codec pipelines), and a deploy/CI-execution concern, not a
development one. The web multi-party session that drives the
worker (`apps/web/src/calls/sfu-call-session.ts` — composes
the `lakko-sfu-v1` signaling client + the call-admission JWT
+ one `RTCPeerConnection` to the SFU + `attachSenderSFrame` /
`attachReceiverSFrame` on every track + SFU-initiated
renegotiation) is now wired into the chat UI: a group chat
(>2 members) routes the call button through
`createSfuCallUi`, which drives the shared call-UI store
roster, plays remote audio, and renders per-tile remote
video, while 1×1 chats stay on the P2P path. The MSID
stream-id → source-peer correlation the video tiles need is
pinned by construction — the SFU sets the downstream MSID
stream-id to the source peer id (`add_media(..,
Some(source_peer.to_string()), ..)` in
`services/sfu/src/media.rs`), and that string is the same
`PeerId` the signaling roster carries, so
`ev.streams[0].id` maps straight onto the roster tile. The
**native** iOS + Android clients are now wired to the same
extent (SFU multi-party arc, complete): `SfuSignalingClient`
(WS + typed `lakko_sfu_proto` frames) + `SfuCallSession`
(one `RTCPeerConnection`/`PeerConnection` to the SFU,
`armSFrameEncryptors` on every local sender before the offer,
`armSFrameDecryptor` on every remote receiver, SFU-initiated
renegotiation) + the `SfuCallUi` adapter driving the shared
`CallUiState` + the chat-header trigger/presentation. Both
publish SFrame-sealed video when the call is started with the
video-call button (the local camera is captured, the video
track added before `armSFrameEncryptors` + the offer, so the
video sender is sealed alongside audio). All three platforms
therefore reach the SFU through the identical framed
`seal_framed` / `open_framed` core. What remains is only the
live cross-platform RTP verification above (a real frame
round-tripping web↔native through a deployed SFU) — a
verification step, not a missing mechanism.

**Why this isn't a security gap today.** The keying math +
the AAD set + the wire layout are all RFC 9605 conformant
and shared across platforms by construction. The residual
item is a verification step (does a live cross-stack frame
round-trip?), not a missing mechanism. The fail-closed
posture holds throughout: a frame that fails to
parse / authenticate is dropped, never forwarded as
plaintext (invariant #4), on every surface.

## Dependency advisories

The `cargo audit` baseline (RustSec advisory DB) surfaces
two open vulnerabilities + three unmaintained-crate
warnings in the workspace's transitive dependency tree.
None are in code we control; remediation depends on
upstream releases. Each entry below lists severity,
reachability analysis, and the upstream tracker the fix
is gated on.

### G-23: `libcrux-chacha20poly1305 0.0.7` — RUSTSEC-2026-0124 (HIGH 8.2) — **CLOSED (fixed)**

> Potential Panic on Overlong Ciphertext Buffer.
> Upgrade to >= 0.0.8.

Vulnerable as a transitive dep:
`libcrux-chacha20poly1305 0.0.7 → libcrux-aead 0.0.7 →
hpke-rs-libcrux 0.6.1 → hpke-rs 0.6.1 →
openmls_rust_crypto 0.5.1 → lakko-mls` (HPKE seal/open inside
OpenMLS).

**Fix — a Cargo.toml-only vendored fork of `hpke-rs-libcrux`.**
The published `hpke-rs-libcrux 0.6.1` hard-pins
`libcrux-aead ^0.0.7` (which transitively pins the vulnerable
`libcrux-chacha20poly1305 0.0.7`), and a `[patch.crates-io]`
cannot relax a semver requirement. So we vendor `hpke-rs-libcrux`
under `crates/vendor/hpke-rs-libcrux` and patch it in via the root
`[patch.crates-io]`. The delta is **two version bumps in its
`Cargo.toml` only** — `libcrux-aead 0.0.7 → 0.0.8` (which pulls
the fixed `libcrux-chacha20poly1305 0.0.8`) and its own
`libcrux-traits 0.0.6 → 0.0.7` so the AEAD trait it imports
matches what `libcrux-aead 0.0.8` implements. **The `src/` is
byte-identical to upstream 0.6.1** (verified) — zero crypto-code
changes.

The earlier worry about an "irreconcilable `libcrux-traits`
0.0.6-vs-0.0.7 conflict" was wrong: `hpke-rs-libcrux` uses
`libcrux-traits` only for the AEAD trait, so `libcrux-ecdh` /
`libcrux-hkdf` staying on `libcrux-traits 0.0.6` while the AEAD
path uses `0.0.7` is fine — Cargo carries both versions and no
trait type crosses the boundary. The fix **compiles + the full
workspace checks clean + all `lakko-mls` tests pass**, and
`cargo audit` now reports the libcrux advisory gone (only the
accepted-risk `rsa` G-24 remains).

Regression-guarded by the existing `cargo audit` / `cargo deny`
CI gate: if the bump were reverted, `libcrux-chacha20poly1305
0.0.7` would return and the advisory check would fail.

(The vulnerable path was, in any case, not demonstrably reachable
— every byte reaching `libcrux-chacha20poly1305::decrypt` first
passes OpenMLS's length-bounded `MlsMessageIn` parser + the
HPKE-RS length checks, and the attacker-facing ingest is gated by
a rate limit + PoW + Privacy Pass — so this was a low-risk DoS at
worst even before the fix.)

### G-24: `rsa 0.9.10` — RUSTSEC-2023-0071 (MEDIUM 5.9) — Marvin Attack — **CLOSED (Jun 2026)**

**Closed by dropping rsa, not accepting it.** Bumped `sqlx` 0.8 → 0.9
(released, `rust-version` 1.94 ≤ our pinned 1.95) across all 7 services and
omitted the now-feature-gated `rsa` feature (`sqlx-mysql/rsa`), so the crate is
no longer in the dependency tree at all — `cargo tree -i rsa` is empty and
`cargo deny check` passes with **no ignore** for RUSTSEC-2023-0071 (the
`deny.toml` entry was removed). The only 0.9 breaking change for our usage was
the SQL-injection guard (`SqlSafeStr`): the 4 audited dynamic-SQL sites in
`services/ds/src/journal.rs` (table/column from a fixed `Queue` enum, never user
input) now wrap the string in `sqlx::AssertSqlSafe`. Workspace `cargo check` +
`cargo deny` + the `ds` tests are green. The original analysis below is retained
for history.

---

> Potential key recovery through timing sidechannels.
> No fixed upgrade is available.

Transitive dependency: `rsa 0.9.10 → sqlx-mysql 0.8.6 →
sqlx → all `services/*`.

**Why this is structural, not negligence.** The `rsa`
crate is the only pure-Rust RSA implementation that
compiles on every target sqlx supports; the Marvin-attack
mitigation requires constant-time arithmetic that the
crate maintainers explicitly call out as "we will not
land this in the current codebase." The upstream
position is "use a different RSA library if you need
side-channel resistance" — `sqlx-mysql` would have to
swap implementations, which it has not.

**Reachability.** `rsa` is reached **only** via MySQL
native-auth handshake when the server's
`mysql_native_password` authentication method requires
RSA-OAEP padding to encrypt the client password (server-
side `caching_sha2_password` cache miss path). Lakko
configures MariaDB with `caching_sha2_password` ONLY,
and the warm-cache hit path uses SHA-2 without RSA. The
RSA path is reached only on first-time connect from a
client whose password hasn't been cached server-side.
**The application never sees user passwords directly**:
authentication runs OPAQUE-PAKE (`services/auth`), which
the database connection layer is downstream of — the
MySQL connection password is the **service's** own DB
password (configured in `/etc/lakko/lakko.env`), not a
user's. A successful timing attack on this RSA exchange
would leak the service's MySQL connection password to an
attacker who already has network access to MySQL itself
— at which point the attacker has direct DB access and
the RSA leak is the wrong attack to spend a Marvin
side-channel on.

**Mitigation**: production MariaDB binds only to the
internal loopback / VPC interface; only the Lakko
services reach it. An attacker who can mount a timing
side-channel on this internal hop has already broken
the perimeter.

**Status**: accepted risk pending upstream `sqlx-mysql`
swapping to a constant-time RSA crate (no tracker —
upstream position is "no fix coming"). Re-audit if MySQL
auth surface changes.

**Corrected + re-verified (Jun 2026).** An earlier note here
claimed this was "not fixable upstream" — that was wrong (it
parroted a stale `deny.toml` comment). The real status:
- `cargo update --dry-run` shows 0 **semver-compatible** updates
  — `rsa` 0.9.10 + `sqlx` 0.8.6 are the latest 0.x releases —
  but that only covers in-range bumps.
- On `sqlx-mysql` **0.8.x** the `rsa` crate cannot be removed:
  `optional = true` in the manifest, yet `connection/auth.rs`
  does `use rsa::{…}` UNCONDITIONALLY (no `#[cfg(feature="rsa")]`),
  so dropping the feature is a compile error.
- **`sqlx` 0.9.0 is released** (the stale comment said sqlx was
  still "waiting on rsa 0.10.0") and **feature-gates rsa**
  (`sqlx-mysql/rsa = [dep:rand, dep:rsa]`). The advisory itself
  has `patched = []` — no fixed rsa exists — so the fix is to
  **drop** rsa, not upgrade it: bump 0.8 → 0.9 and omit the `rsa`
  feature, and the crate leaves the tree entirely (our MariaDB is
  localhost-only over a Unix socket / TLS, so the
  caching_sha2_password RSA path is never exercised).
- **Status: a real, concrete fix exists (sqlx 0.9 + no rsa
  feature) — deferred only because the 0.8 → 0.9 bump is a
  breaking, multi-service migration.** Documented accept in
  `deny.toml` stands in the meantime; residual is near-zero per
  the reachability analysis above.

### G-25: Unmaintained-crate + advisory warnings — **all CLOSED (2026-07): the three unmaintained warnings + two 2026 transitive advisories (anyhow, crossbeam-epoch); `cargo deny check advisories` is clean**

The `cargo audit` baseline flagged three unmaintained-crate
advisories. None are vulnerabilities — the maintainers wound
down support, but the code works. Two were transitive through
`uniffi 0.28.x` and **closed** by the `uniffi 0.28 → 0.31` bump
(the same "major upstream bump drops the dep" remediation that
closed G-24's `rsa` via sqlx 0.9). The third (`instant`) reaches
us only on the wasm target via `openmls`'s `js` feature and is a
non-CVE unmaintained advisory. It has a **self-serviceable fix
that does not wait on openmls** — a `[patch.crates-io]` shim
re-exporting the maintained `web-time` in place of
`fluvio-wasm-timer` (detailed in the `instant` bullet below) —
but the maintainer deferred it (2026-07) as supply-chain surgery
disproportionate to a cosmetic-audit win; the accept posture
stands until a clean `cargo audit` is required.

- **`bincode 1.3.3`** (RUSTSEC-2025-0141) — **CLOSED
  (Jun 2026).** Was under `uniffi_macros 0.28.3 →
  uniffi 0.28.3` (build-time macro-output serialisation).
  `uniffi_macros 0.31.2` no longer depends on `bincode`
  (verified: `uniffi_macros@0.28.3` declared `bincode ^1.3
  kind=normal`; `@0.31.2` declares neither). After bumping
  `crates/lakko-ffi-uniffi` to `uniffi = "0.31"`,
  `cargo update -p uniffi` reported `Removing bincode v1.3.3`
  and `cargo tree -i bincode` is now empty.

- **`paste 1.0.15`** (RUSTSEC-2024-0436) — **CLOSED
  (Jun 2026).** Was under `uniffi_core 0.28.3` +
  `uniffi_bindgen 0.28.3` (compile-time macro helper).
  Both dropped `paste` at 0.31.2 (replaced by the maintained
  `pastey` fork). `cargo update -p uniffi` reported
  `Removing paste v1.0.15` and `cargo tree -i paste` is now
  empty.

- **`instant 0.1.13`** (RUSTSEC-2024-0384) — **CLOSED
  (2026-07) via a `web-time` shim.** Was under
  `parking_lot_core 0.8.6 → parking_lot 0.11.2 →
  fluvio-wasm-timer 0.2.5 → openmls 0.8.1`. A wasm-only
  timer abstraction (an **unmaintained** advisory, not a
  CVE). The self-serviceable fix documented below was
  **applied**: a vendored `crates/vendor/fluvio-wasm-timer`
  shim (`pub use web_time::{SystemTime, UNIX_EPOCH};`, AGPL,
  excluded from the workspace) wired in via `[patch.crates-io]`
  REPLACES the real crate, so `instant` leaves the graph
  entirely — `cargo tree -i instant --target all` reports "did
  not match any packages" and `cargo deny check advisories` no
  longer lists it. openmls builds + tests green against the shim
  on host + wasm (the wasm build is the one that actually uses
  the shim's `SystemTime` / `UNIX_EPOCH`).

  **What functionality pulls it (verified 2026-07 against
  the openmls 0.8.1 source):** `fluvio-wasm-timer` is an
  optional openmls dependency gated behind openmls' `js`
  feature, which Lakko enables in
  [`crates/lakko-ffi-wasm/Cargo.toml`](../../crates/lakko-ffi-wasm/Cargo.toml)
  (`openmls = { features = ["js"] }`). The `js` feature also
  carries `getrandom/wasm_js` — mandatory (no RNG, hence no
  MLS, on wasm without it) — so the feature itself cannot be
  dropped. openmls `use`s the timer in exactly one place,
  `src/key_packages/lifetime.rs`, under
  `#[cfg(target_arch = "wasm32")]`, to read `SystemTime::now()`
  for **KeyPackage lifetime (expiry) validation** — checking
  a KeyPackage's `not_before` / `not_after` window when a
  member is added or a group is joined on the web client. On
  wasm `std::time::SystemTime::now()` panics (no OS clock), so
  openmls substitutes `fluvio-wasm-timer`'s `SystemTime`
  (backed by JS `Date.now()`). The functionality is genuine
  and security-relevant (an expired KeyPackage must be
  rejected), so the code path cannot simply be removed.

  **A self-serviceable fix exists (deferred, not blocked on
  openmls):** openmls consumes only `fluvio_wasm_timer::{SystemTime,
  UNIX_EPOCH}`, and the maintained replacement `web-time`
  exposes the same std-compatible surface. A `[patch.crates-io]`
  entry pointing `fluvio-wasm-timer` at a tiny vendored shim
  crate whose body is `pub use web_time::{SystemTime, UNIX_EPOCH};`
  would satisfy openmls with **zero openmls source change** and
  drop `instant` from the graph entirely (`cargo tree -i instant`
  empty, incl. under wasm) — the same vendored-patch idiom Lakko
  already uses for `webauthn-rs-core` and `hpke-rs-libcrux`, and
  a net supply-chain improvement (an unmaintained crate replaced
  by a maintained one). **Applied 2026-07** (the maintainer called
  for the clean-audit posture): the shim landed exactly as described
  and `instant` is gone. **Two 2026 transitive advisories surfaced in
  the same audit run and were fixed by patch bumps** (unrelated to the
  shim — the RustSec DB had published them since the prior audit):
  `anyhow 1.0.102 → 1.0.103` (RUSTSEC-2026-0190, an `Error::downcast_mut`
  unsoundness) and `crossbeam-epoch 0.9.18 → 0.9.20` (RUSTSEC-2026-0204,
  an invalid-pointer-deref in a `fmt::Pointer` impl, via `rayon`), both
  `cargo update -p` lock bumps with no source change. `cargo deny check
  advisories` now reports **`advisories ok`**.

  The alternative upstream fix — openmls' planned 0.9 (which
  swaps `fluvio-wasm-timer` for `web-time` itself) — is
  unreleased and its timeline / scope are not guaranteed; when it
  does land, the bump is an MLS-core change requiring an ADR +
  property tests regardless. The shim above does **not** wait on
  it.

**The uniffi 0.28 → 0.31 bump cost zero source changes** —
`lakko-ffi-uniffi` uses proc-macro mode (`setup_scaffolding!`)
with a crate-local bindgen (`uniffi_bindgen_main()`), so the
runtime and generator move in lock-step. Verified end-to-end
firsthand (2026-06-23): `cargo build -p lakko-ffi-uniffi` +
`cargo test -p lakko-ffi-uniffi` (158 passed) + `cargo clippy`
clean; `just regen-bindings` (Swift + Kotlin) succeeds; the iOS
client (`xcodebuild` BUILD SUCCEEDED after `just build-ios`) and
the Android client (`:app:compileDebugKotlin` BUILD SUCCESSFUL)
both compile against the uniffi-0.31-generated bindings with no
hand-written-code changes; `just audit` reports `advisories ok`.

**Status**: bincode + paste closed; instant accepted upstream
debt (wasm-only, openmls-pinned). CI warns (not fails) on
unmaintained-crate advisories so they remain visible without
blocking releases.

**Cumulative posture** (updated 2026-06-23): zero
vulnerabilities in Lakko-authored code. The transitive-dep
advisories have since been remediated as the upstream chain
caught up — `rsa` (RUSTSEC-2023-0071, G-24) closed by sqlx
0.9, `libcrux-chacha20poly1305` (RUSTSEC-2026-0124, G-23)
closed by the vendored hpke-rs-libcrux fork, and `bincode` +
`paste` (G-25) closed by the uniffi 0.31 bump. The only
remaining unmaintained-crate warning is `instant`
(RUSTSEC-2024-0384, G-25) — wasm-only, pending an openmls
release that drops `fluvio-wasm-timer`. The `cargo audit`
baseline below is captured as of `2026-05-28` (re-run delta
vs `2026-05-27`: **zero change** — same 2 vulnerabilities,
same 3 unmaintained warnings, no new findings, no advisories
resolved upstream at that time. Post-G5-6-wireup
confirmation: the
`hpke 0.13` + `rand_core 0.9` deps added by ADR-0012
brought in their pure-Rust RustCrypto dependencies
[`x25519-dalek`, `aes-gcm`, `chacha20poly1305`,
`hkdf`, `hmac`, `sha2`] without introducing any new
advisory) and is re-run on every CI build; new
findings either get a G-entry of their own or are
remediated as the upstream chain catches up.

`cargo deny check` is the second tier of the supply-
chain gate (licence allowlist, advisory deduplication,
unmaintained-crate policy on top of `cargo audit`). The
binary is not currently installed on this dev host; the
`just audit` recipe invokes it during CI runs. A follow-
up sprint stands up `cargo deny` locally on the
maintainer's box so the audit pack can mirror its CI
output here.

### G-27: Channel PublisherGroup — **CLOSED** — all three platforms wired (G4-2-mls-publisher + G-27 iOS / Android slice) + persistence + rotation landed (G4-2-mls-persistence)

[`apps/web/src/channels/composer.ts::freshPublisherKeys`](../../apps/web/src/channels/composer.ts)
now derives the channel content-key seed from a real
MLS PublisherGroup exporter: `Group.create(identity)`
→ `exportSecret(identity, channelExporterLabel(),
channelIdBytes, 32)` →
`channelSeedFromMlsExporterOutput`. The identity is
rebuilt from the OPAQUE export key
(`wasm.Identity.fromSeed(secrets.exportKey)`) inside
the `NewChannel` route handler in
[`apps/web/src/routes/Channels.tsx`](../../apps/web/src/routes/Channels.tsx).
The wasm-pack bindings have been regenerated so
`channelExporterLabel` + `channelSeedFromMlsExporterOutput`
are exposed in the TypeScript `.d.ts`. TS typecheck +
`npm run build` are green.

**Closed — iOS + Android wired.** Both per-platform
composers now drive the same `Group::create` +
`Group::export_secret` +
`channelSeedFromMlsExporterOutput` triplet the web
composer uses:

- [`apps/ios/Lakko/Channels/ChannelsComposer.swift::freshPublisherKeys`](../../apps/ios/Lakko/Channels/ChannelsComposer.swift)
  now takes `(identity: Identity, channelIdHex:
  String)`, builds a transient `Group` via
  `Group.create(identity:)`, calls
  `group.exportSecret(identity:, label:
  channelExporterLabel(), context: channelId,
  length: 32)`, and crosses the FFI boundary via
  `LakkoCore.channelSeedFromMlsExporterOutput`. The
  caller plumbs `secrets.exportKey` from the
  `AuthRepository` session through
  `ChannelsRootScreen(exportKey:)` →
  `ChannelsListScreen` → `NewChannelScreen` → its
  `create()` action which constructs the throwaway
  `Identity.fromSeed(seed: exportKey)` for the
  duration of the call.
- [`apps/android/app/src/main/kotlin/me/lakko/app/channels/ChannelsComposer.kt::freshPublisherKeys`](../../apps/android/app/src/main/kotlin/me/lakko/app/channels/ChannelsComposer.kt)
  ships the same shape with the same lifecycle
  semantics — `Group` and `Identity` are both
  `AutoCloseable` via UniFFI's Disposable
  conformance, so the call sites use `try { ...
  } finally { group.close(); identity.close() }`
  to drop the Rust handles eagerly rather than
  waiting on the JVM GC. The Compose call site at
  `apps/android/.../ui/channels/ChannelsScreen.kt::ChannelsRootScreen(exportKey: ByteArray, ...)`
  threads the export key through the same shape
  as iOS.

`xcodebuild -sdk iphonesimulator -configuration Debug build`
and `./gradlew :app:assembleDebug` both green after
the wire-up. The audit-pack invariant C-44 ("the
channel seed crosses the FFI boundary via
`channelSeedFromMlsExporterOutput`, never via a raw
`Data` cast") now holds on every client platform
that ships a publisher composer.

**G4-2-mls-persistence — CLOSED (May 2026).**

The persistence + rotation gap closed across six
sub-sprints:

- **Step 1 (Rust core)** —
  `crates/lakko-mls/src/publisher_group.rs` ships
  `PublisherGroup` keyed by `channel_id`, with
  `load` / `create` / `open_or_create` routed
  through OpenMLS's `MlsGroup::new_with_group_id`
  and `MlsGroup::load`. Deterministic group id
  derivation: `prefix("lakko/publisher-group/v1/") ||
  channel_id`. 8 unit tests pin the load-or-create
  idempotence invariant.
- **Step 2 (FFI)** — UniFFI + wasm-bindgen
  `PublisherGroup` wrappers in `lakko-ffi-uniffi` +
  `lakko-ffi-wasm`. `publisher_group_load` /
  `publisher_group_open_or_create` free functions
  (Option<Self> work-around for UniFFI 0.28). 7
  UniFFI + 4 wasm host-side tests.
- **Step 3 (composer refactor)** — iOS + Android +
  web composers all swapped throwaway `Group.create`
  for `PublisherGroup.openOrCreate`. xcodebuild +
  `:app:assembleDebug` + `npm run build` all green.
- **Step 4 (rotation)** —
  `PublisherGroup.rotate(identity)` advances the
  MLS group epoch via `MlsGroup::self_update` +
  `merge_pending_commit`. Returns the wire-encoded
  commit for fanout to co-publishers. Both the
  Rust-core and the FFI surfaces are covered by
  `rotate_advances_epoch_and_changes_seed` /
  `rotated_seed_persists_across_reload` /
  `publisher_group_rotate_changes_seed`.
- **Step 5 (audit pack)** — see C-52 in
  `docs/security/calls.md` for the persistence +
  rotation invariants.
- **Step 6 (cross-publisher membership)** —
  `rotate_emits_commit_bytes_processable_by_co_publisher`
  proves: Alice mints PublisherGroup, adds Bob via
  Add proposal, Bob joins from welcome, Alice
  rotates, Bob processes the commit, both ends at
  the same epoch with the same exporter output.
  ADR-0010 §"PublisherGroup" cross-publisher
  seed-sync invariant satisfied.

Remaining deferred follow-ups (out of G4-2 scope):

- **G4-2-rotation-timer wave 1 DONE** (May 2026) —
  scheduled rotation policy + web orchestration.
  `crates/lakko-channels/src/rotation_policy.rs` ships
  `RotationPolicy` (24 h or 1 000 posts, both
  thresholds independently disable-able) + `RotationState`
  + pure `should_rotate(now_ms, state)` decision.
  Saturating arithmetic on the time delta tolerates
  host-clock jumps backward. 13 unit tests + UniFFI +
  wasm-bindgen FFI surface + audit-pack invariant
  **C-54** in [`calls.md`](./calls.md).
  `apps/web/src/channels/rotation-cron.ts` ships
  `runRotationOnce` (single-pass: policy check → MLS
  `PublisherGroup.rotate` → fetch subscribers → seal new
  envelopes per subscriber → atomic batch `POST
  /v1/channels/:id/rotate`) + `startRotationCron`
  (periodic poller, default 5-minute heartbeat). Empty
  batches are allowed (still bump the epoch — covers
  the "no subscribers yet" case). Server-side `POST
  /v1/channels/:id/rotate` is C-42 (atomic
  rotation); client now drives it via the new
  `rotateChannel` helper at
  `apps/web/src/channels/api.ts`.
- **G4-2-rotation-timer-ios DONE** (May 2026) —
  `apps/ios/Lakko/Channels/RotationCron.swift` ships
  the Swift orchestrator as an `actor` with `runOnce(policy,
  state, nowMs?)` + `start(policy, pollInterval,
  loadState, onError)` returning a `Task` whose
  `cancel()` stops the loop. Same shape as the web side:
  policy check via UniFFI `channelShouldRotate` →
  `PublisherGroup.rotate` → fetch subscribers → seal new
  envelopes per subscriber → atomic batch via
  `ChannelsClient.rotateChannel(...)`. New
  `RotateBatchEntry` wire type lives in
  `ChannelsClient.swift` next to the existing batch
  shapes. `xcodebuild -scheme Lakko -destination
  'platform=iOS Simulator,name=iPhone 17'` →
  BUILD SUCCEEDED.
- **G4-2-rotation-timer-android DONE** (May 2026) —
  `apps/android/app/src/main/kotlin/me/lakko/app/channels/RotationCron.kt`
  ships the Kotlin orchestrator with the same shape
  (`runOnce` suspend fun + `start(scope, policy,
  loadState, pollInterval, onError)` returning a `Job`).
  `RotateBatchEntry` @Serializable wire type +
  `ChannelsClient.rotateChannel` Ktor POST. Uses the
  UniFFI bindings (`channelShouldRotate`,
  `channelRotationStateAfterRotation`,
  `sealSubscriberSeedEnvelope`) verbatim — no Kotlin-side
  re-implementation of the policy. `./gradlew
  :app:assembleDebug` → BUILD SUCCESSFUL.

The G4-2-rotation-timer arc is now complete across all
three platforms (web + iOS + Android). All decisions
about _when_ to rotate flow through one Rust file
(`crates/lakko-channels/src/rotation_policy.rs`); each
platform's orchestrator is ~200 LOC of pure plumbing
above the same FFI surface.

The cryptographic property is preserved: the seed is
genuinely derived from the MLS exporter under the
publisher's identity, not from `OsRng`. What is
missing is the multi-device byte-identicality
property (per ADR-0010 Tier 1, two devices of the
same publisher on the same channel SHOULD produce
the byte-identical seed via shared identity +
identical `Group::create`/exporter inputs;
`Group::create` uses the identity's MLS provider
which currently treats `group_id` as random, so
that property only holds after group-id persistence

### G-28: SFrame on-device wiring — CLOSED (May 2026) via the G3-sframe-native arc (N-1..N-6)

**Status (May 2026)**: **CLOSED.** Six sub-sprints (N-1
through N-6) shipped the full native-shim stack on both
mobile platforms. The Rust SFrame algorithm is now
plumbed end-to-end through libwebrtc's
`FrameEncryptorInterface` / `FrameDecryptorInterface`
on iOS and Android, with security invariant #4 ("no
fallback to non-E2EE") enforced at five layers
(documented under C-45..C-50 in
[`docs/security/calls.md`](./calls.md)).

What landed:

- **N-1** `crates/lakko-sframe-native/` — flat C ABI
  around `lakko_calls::sframe` (11 `extern "C"`
  functions + 7 unit tests + cbindgen header emitted
  to a stable mirror at `target/lakko-include/`).
- **N-2** `apps/shared/webrtc-cpp-headers/M125/` — 13
  sha256-pinned libwebrtc M125 (branch-heads/6422)
  C++ headers + 2 Lakko-authored abseil shims +
  `fetch.sh` verify-mode + `probe.cc` compile-only
  validation harness.
- **N-3** `apps/shared/sframe-bridge/` — C++ subclass
  `LakkoFrameEncryptor` / `LakkoFrameDecryptor` of
  `webrtc::FrameEncryptorInterface` /
  `FrameDecryptorInterface` (the upstream M125
  shape). 4-test standalone round-trip suite proving
  AAD-binding, multi-sender cache, bad-seed rejection.
- **N-4** `apps/ios/Lakko/Calls/SFrameAttach.{h,mm}`
  + `Lakko-Bridging-Header.h` — Obj-C++ shim with
  boot-time `class_respondsToSelector` probe for
  `setFrameEncryptor:` / `setFrameDecryptor:`. Refuses
  to start the call when the probe goes negative.
  Swift-side `SFrameAvailability.shared` fail-closed
  gate. `cross-build-sframe-native-ios` justfile
  recipe lipo-fuses simulator slices.
- **N-5** `apps/android/lakko-sframe/` — Gradle AAR
  with Kotlin `LakkoFrameEncryptor` / `Decryptor`
  implementing `org.webrtc.FrameEncryptor` /
  `FrameDecryptor`. JNI bridge `libsframe_jni.so`
  built via `tools/build-sframe-jni.sh` against the
  cross-compiled Rust staticlib for each NDK ABI.
  Kotlin `SFrameAvailability` boot probe through
  `Class.forName + getMethod` against
  `org.webrtc.RtpSender.setFrameEncryptor`.
- **N-6** Production CallSession integration on both
  platforms — `setCallKey(seed, localLeaf)` injected
  after welcome-ferry / KP-forward dispatcher
  resolves; `attachSFrame` called between
  `addLocalAudioTrack` and SDP build so the
  encryptor is armed before libwebrtc's codec
  thread pulls bytes. Extended Rust E2E test
  `crates/lakko-sframe-native/tests/sfu_opacity_e2e.rs`
  (6 tests, all green) proving SFU opacity,
  AAD-tampering detection, wrong-key isolation,
  multi-party fan-in, cross-call isolation, counter
  monotonicity.

Validation chain (all green):

- `cargo test --workspace --all-targets` — 224+ tests
  pass including the new sfu_opacity_e2e suite.
- `xcodebuild -destination 'generic/platform=iOS
  Simulator' build` → BUILD SUCCEEDED. The full Swift
  / Obj-C++ / C++ / Rust path compiles and links
  cleanly with the lipo'd `liblakko_sframe_native.a`.
- `./gradlew :app:assembleDebug` → BUILD SUCCESSFUL.
  The full Kotlin / JNI / C++ / Rust path likewise.

**Deferred follow-ups (out of G-28 scope, tracked
separately)**:

- **N-{4,5}-validate** — host-side close-out DONE
  (May 2026). `apps/shared/sframe-bridge/test_bridge.cc`
  ships eight standalone C++17 assertions exercising
  every documented tampering mode of the SFrame
  native shim before either platform binding loads:
  single-sender round-trip, AAD-tamper, multi-sender
  per-leaf cache, bad-seed rejection, ciphertext-body
  tamper, counter monotonicity with out-of-order
  delivery, per-call key isolation (two distinct
  CallKey seeds cannot cross-decrypt), payload-size
  extremes (1B/80B/1500B/64KiB). Runner: `just
  test-sframe-bridge` locally and
  `.github/workflows/sframe-bridge.yml`
  (`workflow_dispatch` only, matrix `ubuntu-latest`
  + `macos-latest`) in CI. Pinned as audit-pack
  invariant **C-53** in
  [`docs/security/calls.md`](./calls.md).
- **N-{4,5}-validate-live** — dual-simulator /
  dual-emulator live-RTP gate (two iOS simulators
  + two Android emulators in CI running synthetic
  calls against the dockerised SFU from
  `ci/sfu-e2e/`, asserting tampered AAD →
  `kFailedToDecrypt` end-to-end). Lives in
  **G3-4-mobile** — the simulator / emulator matrix
  half of the parent **G3-4** sprint. Needs
  `macos-14` + KVM-enabled Linux runners we do not
  yet have budget for.

Build artefacts are already produced; the host-side
gate covers every code path below the platform shim,
and the dual-instance live-RTP gate is the only
remaining piece (waiting on CI-runner allocation).

---

**Historical context (preserved for audit trail)**:

The original G-28 investigation surfaced a structural
ceiling on both upstream WebRTC SDKs Lakko depends on:

- **Android `io.github.webrtc-sdk:android` 125.6422.07**
  exposes `org.webrtc.FrameEncryptor` and
  `org.webrtc.FrameDecryptor` only as Java interfaces
  with a single abstract method `long
  getNativeFrameEncryptor()` / `long
  getNativeFrameDecryptor()`. The actual `encrypt` /
  `decrypt` calls happen entirely inside the C++
  `webrtc::FrameEncryptorInterface` /
  `FrameDecryptorInterface` instance whose pointer
  that `long` returns. A pure-Kotlin override of an
  `encrypt(mediaType, ssrc, additionalData, frame)`
  method **does not exist on this SDK** — that shape
  matches the older `google-webrtc` artifact Lakko
  abandoned per G3-2 / ADR-0011's note that Google
  stopped publishing that artifact in 2021.
- **iOS stasel/WebRTC 125.0.0** — **G3-1-sframe-spike
  closed (May 2026)**: walked the
  `WebRTC.xcframework/ios-arm64/WebRTC.framework/Headers/`
  directory (92 public headers across iOS arm64, iOS
  simulator x86_64+arm64, macOS, mac-catalyst — every
  slice identical in this respect). The only public
  mention of `FrameEncryptor` / `FrameDecryptor` is the
  docstring on `RTCCryptoOptions.sframeRequireFrameEncryption`
  — a require-flag, **not** an attach-API. Neither
  `RTCRtpSender.h` nor `RTCRtpReceiver.h` exposes
  `frameEncryptor` / `frameDecryptor` /
  `frameTransformer` properties. The C++
  `webrtc::FrameEncryptorInterface` exists in upstream
  libwebrtc but stasel's Obj-C wrappers don't bridge
  it out. Same shape as Android.

**Path forward (Android, possibly iOS)**: a Rust crate
`lakko-sframe-jni` that compiles via `cxx-rs` to one
`.so` per ABI (arm64-v8a, x86_64). The `.so` exposes
a C++ subclass of `webrtc::FrameEncryptorInterface`
whose `Encrypt` / `Decrypt` thunks call back into
LakkoCore's `ParticipantKey` via the existing UniFFI
surface (a `long` pointer round-trips through the
Java `getNativeFrameEncryptor()` contract without
ever crossing back to the JVM mid-frame). This is
the canonical Insertable-Streams shape upstream
expects; it is also how Signal, Jitsi, and Matrix
do it. Estimated at ~2 sprints (cxx-rs setup +
libwebrtc header dep + Android NDK toolchain pin +
separate Gradle module for `.so` packaging).
Tracked as `G3-2-sframe-jni`.

**iOS path forward (post-spike)**: matching native-
shim shape under task **G3-1-sframe-native**. An
Objective-C++ (`.mm`) class subclasses
`webrtc::FrameEncryptorInterface` + `FrameDecryptorInterface`
from upstream libwebrtc's C++ headers (bundled
ourselves since stasel only ships Obj-C headers +
the linked binary; libwebrtc's BSD license permits
header redistribution). The shim is installed onto
`RTCRtpSender` / `RTCRtpReceiver` via the
non-public selector `setFrameEncryptor:` that the
upstream Obj-C wrapper carries but does not declare
in its public headers (the selector is hit by name
through `objc_msgSend` — same technique Signal-iOS
and Jitsi-Meet iOS use for the identical gap).
Inside `Encrypt` / `Decrypt` the shim calls into
LakkoCore's `ParticipantKey` via the existing
UniFFI surface. Estimated ~2 sprints in parallel
with G3-2-sframe-jni; the Rust side may share a
single crate (`lakko-sframe-native`) exposing a C
ABI both platforms wrap.

The native shim approach has a security caveat: the
non-public `setFrameEncryptor:` selector is not part
of stasel/WebRTC's stable API. An upstream rename or
removal would silently no-op the installation —
behaviour the audit pack must catch. Mitigation: a
boot-time `class_respondsToSelector` probe that
panics loud if the selector is missing, surfaced as
a "SFrame attach refused" log + the call MUST refuse
to start (no fallback to plain SRTP under any
circumstance, per security invariant #4 "no fallback
to non-E2EE").

**Security posture during the gap**: documented in
[`docs/security/calls.md`](./calls.md) under the
G3-1 / G3-2 partial-completion rows in
[`docs/ROADMAP-PHASE3-CLOSEOUT.md`](../ROADMAP-PHASE3-CLOSEOUT.md):
the iOS + Android call paths today ride **plain
SRTP** (per-hop encryption only). They are NOT
E2EE-complete. The wire still announces SFrame
support so the SFU still forwards opaque payload,
but the absence of an actual SFrame transformer at
the codec boundary means the SFU could in principle
decode the SRTP and see plaintext Opus/AV1. ROADMAP
explicitly states this. The fix is the JNI shim
above + the iOS spike + (subsequently) MLS-exporter
seed delivery from each platform's CallSession into
the freshly-created `ParticipantKey`.
+ replication via DeviceGroup are wired).

### G-29: in-code completion markers swept + reconciled; residual genuinely-open items

**Reconciliation pass (2026-06-09).** The codebase's in-code "TODO / not yet
wired / for now / lands in X / deferred" annotations had drifted badly out of
date — a four-area sweep (Rust crates + services, web, iOS, Android) found
that the large majority described work that had **already landed**, and
corrected **~45 stale comments** to match the code (e.g. iOS/Android/web call
surfaces claiming "audio-only / SFrame deferred / video lands later" when
video + mandatory SFrame are fully wired; "passkey sign-in not wired" when it
is; invite short-pointer / max-uses / KP-forward "deferred" when the discovery
endpoints + crate paths exist; the Android recover snapshot "only
re-published" when it is applied to `IdentitySnapshotStore`). The in-code
annotations now reflect reality; this entry records what genuinely **remains**
after the sweep.

**Already tracked elsewhere (cross-references, not new):** identity/invite
lookups are TOFU pending the deployed transparency service (G-4 / ADR-0016);
external-stack SFrame RTP interop unverified (G-26); single-link many-stranger
call rendezvous absent — multi-party works via owner KP-forward (G-21); the DS has no
auth/PoW (pseudonym-unguessability is the Phase-1 gate).

**Genuinely open, security-relevant:**

- **Channel subscriber secret moved to hardware-backed storage (G-29,
  Jun 2026).** The subscriber-side X25519 secret half (it unwraps the
  per-subscriber HPKE seed envelopes) now persists encrypted at rest on both
  native platforms (invariant #7). **iOS:** moved off `UserDefaults` plaintext
  into the Keychain (`ChannelSubscriberKeyStore`, `AfterFirstUnlockThisDeviceOnly`)
  with a one-time migration that scrubs the legacy plaintext copy. **Android:**
  the subscriber-key persistence (previously a `SharedPreferences` path that
  was never actually wired — `ChannelsUiState` was constructed with no prefs,
  so the keys were in-memory only and lost on relaunch) is now wired to an
  `EncryptedSharedPreferences` (Keystore AES-256-GCM, `ChannelKeyStore`), so the
  secret both survives a relaunch and is encrypted at rest. No Android migration
  is needed (no prior on-disk copy existed). Two prior claims on this surface were
  stale and are corrected: the publisher `signingKeySeed` is **in-memory only**
  (not persisted, so there is no at-rest exposure — persisting it to the
  Keychain for cross-relaunch publisher continuity is a separate functional
  follow-up), and the publisher content-key seed **is** derived from the
  PublisherGroup MLS exporter (`publisherGroupOpenOrCreate` + `channelSeed`,
  G4-2-mls-persistence), not a demo value. `xcodebuild` green.

**Genuinely open, UX / feature limitations (not correctness or security
breaks):**

- **Cross-device recover via discovery — wired (stale-doc reconcile,
  Jun 2026).** This entry described `recover()` as requiring a cached
  `account_id`, but the discovery lookup is in fact wired on all three
  platforms (verified firsthand): web `AuthContext.recover` (the optional
  `username` arg → `discovery.lookup` on a cache miss), iOS
  `AuthRepository.recover(mnemonicPhrase:newPassword:username:)`
  (`DiscoveryClient.lookup`), and Android `AuthRepository.recover`
  (`discoveryClientFactory().lookup`). All three resolve the `account_id` the
  same way `login()` does — a local `(accountId, username)` cache hit wins,
  else a `discovery.lookup(username)` round-trip — so a FRESH device can
  recover-from-words given the username + phrase; the `username` arg is
  required only when no account is cached. A username miss collapses into the
  same error as a bad phrase (no username-space probing). The live recovery
  `recovery_kt_e2e.rs` exercises the recovery pipeline, and the resolution
  branch is now factored into a pure `resolveRecoverAccount` helper with a
  6-case unit test (cache-hit / matching-username / fresh-device lookup /
  username-mismatch / unregistered-username / no-account) on **all three
  platforms** — web `apps/web/src/auth/recover-resolution.{ts,test.ts}`, iOS
  `apps/ios/Lakko/Auth/RecoverResolution.swift` + `LakkoTests/RecoverResolutionTests.swift`,
  Android `.../auth/RecoverResolution.kt` + `test/.../RecoverResolutionTest.kt`
  — locking the cross-device behaviour so it can't silently regress (each
  `recover()` is a behaviour-identical refactor onto the shared helper; 6/6
  green on every platform).
- **Web 1×1 video calls landed (Jun 2026).** The web P2P 1×1 path now
  captures the local camera (a "Video" call button dials `video:true`),
  renders a mirrored self-preview + the remote video tile, and toggles the
  camera in-call (`CallSession.setCameraEnabled` + the `onLocalStream`
  callback; `Call.tsx` `localStream` prop + `tileStream`). tsc + vitest (207)
  + vite build green; live two-peer video needs a browser with cameras (a
  runtime gate, like G-26). **In-call device picker — wired to real enumeration
  (Jun 2026):** `Call.tsx`'s settings panel now pulls
  `navigator.mediaDevices.enumerateDevices()` into `callActions.setAvailableDevices`
  on panel-open + on `devicechange` (the picker already rendered the list +
  blank-label fallback + empty-state hint; nothing fed it before).
  **Applying the selection — wired (Jun 2026):** the 1×1 `CallSession` honours
  the picked mic/camera at capture time (`CallMedia.audioDeviceId`/`videoDeviceId`
  → `getUserMedia({ deviceId: { exact } })`) and
  `switchAudioInput`/`switchVideoInput` re-capture + `replaceTrack` on the live
  sender for a mid-call change (no renegotiation, preserves the mute state);
  `Chat.tsx` threads the selection into the outgoing dial + a debounced effect
  applies a mid-call picker change. **The SFU multi-party group call honours +
  switches too (Jun 2026):** `SfuCallSession` honours the picked mic at publish
  (`SfuCallMedia.audioDeviceId`) + `switchAudioInput`/`switchVideoInput`
  `replaceTrack` on the live sender — the sender keeps its SFrame transform, so
  media stays end-to-end encrypted through the SFU (invariant #4, no re-attach);
  `trackConstraint` is shared from the 1×1 `CallSession`, the `SfuCallUiHandle`
  exposes the two methods, and the same Chat.tsx switch effect drives the SFU
  handle (group calls are audio-only for now, so the camera switch is a
  graceful no-op). Residual: the answerer captures the OS default at accept
  (the switch effect then applies any pre-selected device once the call is
  live). tsc + vitest (213) + vite build green.
- **iOS contact inbox — background refresh wired (Jun 2026).** A
  `BGAppRefreshTask` (`ContactBackgroundRefresh`, registered on the scene via
  `.backgroundTask(.appRefresh(_:))` in `LakkoApp`, scheduled on the
  foreground→background transition + re-chained after each run) now wakes the
  app opportunistically to run one quick `refreshInbox()` fetch, so a request
  that arrives while backgrounded is picked up without a manual reopen.
  `Info.plist` declares the `fetch` `UIBackgroundMode` +
  `BGTaskSchedulerPermittedIdentifiers`. `xcodebuild` green. This is
  opportunistic (iOS decides if/when to run it); an APNs `content-available`
  silent-push wake for *immediate* surfacing remains a follow-up — it needs the
  push gateway to emit a silent push on inbox arrival.
- **Android QR pairing scanner — resumable after a non-Lakko QR (CLOSED,
  Jun 2026).** The `QrScannerComposable` latch changed from a one-shot
  `AtomicBoolean hasDetected` to an `AtomicReference<String?> lastHandledRaw`:
  it now fires `onDetected` once per *distinct* decoded value, so the same
  non-Lakko QR held in frame doesn't busy-loop but a *different* code (the user
  panning onto the real pairing QR) fires immediately — no screen re-mount.
  `DevicesScreen` no longer dead-ends to `ScanUi.Error` on a non-pairing QR; it
  stays in `Scanning` and shows a transient inline hint. `:app:compileDebugKotlin`
  green.
- **iOS + Android accepted-contacts list is in-memory only** (rebuilt by
  re-polling the stranger inbox from cursor 0 on launch). The backup
  restore-merge of contacts is therefore an optimization, not a correctness
  gap; the MLS whole-storage snapshot **is** restored on recover on both.

**Genuinely open, maintenance / observability:**

- **CLOSED (Jun 2026).** The PoW challenge-issue/verify/consume/prune logic
  (previously duplicated near-verbatim across
  `services/{auth,discovery,push-gw}/src/pow.rs` — a drift risk where a
  security fix to one copy could miss the others) now lives once in the new
  `lakko-pow-server` crate (distinct from `lakko-pow`, the wasm-portable client
  *solver*). The three diverged only in the doc comments + the effort env-var
  name; the crate parameterises the latter (`configured_effort_bits(var_name)`)
  and each service's `src/pow.rs` is reduced to a thin re-export + the axum
  handler that needs its `AppState`. `equix` is dropped as a direct dep of
  discovery + push-gw (now transitive via the crate; auth keeps it for its
  `#[cfg(test)]` solver helpers). Verified: `cargo test -p lakko-pow-server`
  (5 passed), clippy `-D warnings` clean on the crate + the 3 services, their
  unit suites green, `just audit` clean.
- **CLOSED (Jun 2026).** `lakko_sfu_signaling_frames_total` is now labelled by
  `direction` (inbound = received from a peer / outbound = broadcast to peers)
  + `kind` (the typed frame kind, or `invalid` for an undecodable inbound
  frame). `lakko-sfu-proto` gained a `kind_str()` accessor on `ServerFrame` +
  `ClientFrame` (the single source of truth for the kebab tag, with unit tests
  asserting it never drifts from the serde `kind` tag); `signaling.rs` labels
  the inbound counter by the parsed `ClientFrame` kind, `router.rs` the
  outbound broadcast counter by the `ServerFrame` kind. The unlabelled
  zero-init was dropped (like the labelled `MEDIA_FRAMES_DROPPED_TOTAL`, it
  first appears once traffic flows). `cargo test -p lakko-sfu-proto`
  (21 passed) + `-p lakko-svc-sfu` green; clippy `-D warnings` clean.

### G-30: KT-log (S1-S3) deployed-verifier scope + append-binding trust + equivocation residual — **#2 closed (ADR-0019) + genesis squatting PREVENTED (ADR-0036, flag-gated), #1 closed (ADR-0020)**

**Status (2026-06-12):** the key-transparency log is built end-to-end —
`services/transparency` (operator-signed append-only CONIKS log: S1), discovery
logging every identity-key change (S2) + a reconciliation sweep (S2.5), and all
three clients verifying a fetched contact's published identity against the log
(S3: `verify_log_inclusion` FFI + `TransparencyVerify` on web/iOS/Android).
`lakko-security` reviewed the whole arc: **APPROVE-WITH-NOTES**. Three residuals
are recorded here so a future reader does not over-trust the deployed check:

1. **The deployed verifier is NOT the chain verifier — no equivocation
   resistance.** S3 wires the single-client `lakko_transparency::verify_inclusion`
   path (latest entry + inclusion proof + a monotone last-seen `log_size`
   no-rollback check), NOT `verify_supersession_chain`. A malicious operator
   (it holds the signing key) can sign two different valid roots and show each
   to a different victim (split-view / equivocation); each client sees a
   monotone, self-consistent view and CANNOT detect it. Detecting split-view
   needs gossip (clients cross-checking roots) or witness co-signing — **not
   built**. What the deployed design DOES defend: a server that strips / omits /
   rolls back (→ `absent` / `unreachable` surfaced, `rollback` hard-blocked) and
   a serving-tier (CDN / proxy / DB) key swap (defeated by the build-time
   operator-key pin — the client never trusts `GET /operator-key` for
   verification). The operator pin is a real reduction (compromised *serving*
   tier ≠ compromised *signing* tier), not theatre, even before gossip. The
   signing seed lives in the service env, so a host compromise forges roots
   freely — the same threat boundary as every other server-held signing key
   (call-JWT ADR-0008, device-attest ADR-0015, OPAQUE setup). Cross-ref
   ADR-0016 §equivocation.

2. **Append-binding trust — the log is only as honest as discovery + the append
   secret.** The transparency service sees only `username_hash =
   BLAKE3("lakko/transparency/account/v1" || account_id)`, never the
   `account_id`, so it cannot verify the `account_id ↔ username_hash` binding.
   Anyone holding `LAKKO_TRANSPARENCY_APPEND_SECRET` (the discovery service, or
   a leaked server-to-server secret) can append a leaf for any account-key with
   any pubkey, carrying a valid inclusion proof under the operator-signed root.
   This is NOT a silent key substitution — a forged leaf still has to pass the
   client's fail-closed ADR-0016 supersession gate (runs FIRST, refuses a
   changed key with no old-key-signed proof), so the worst it achieves is
   log-poisoning / a confusing `mismatch` surfaced to the user. Closing it fully
   needs a client-supplied signature by the account-identity key over its own
   `(username_hash, pubkey, version)`, so the service / auditors verify the
   binding without trusting discovery — **future work**.

3. **Mismatch / absent posture is surface-not-block (adjudicated correct, for
   now).** A `mismatch` (the log's latest pubkey ≠ the served pubkey) or
   `absent` (no log entry) is recorded + surfaced but does NOT block the
   contact; only `rollback` hard-blocks. Correct because the fail-closed
   supersession/pin gate (ADR-0016) is the PRIMARY trust and runs first, and
   because discovery's append is best-effort fail-open (S2), so a legitimate
   rotation reaching discovery before the S2.5 reconciler runs produces a
   transient `mismatch`/`absent`; hard-blocking would break legit contacts
   during the consistency window. The condition to flip `mismatch` (not
   `absent`) to fail-closed: the append moves onto discovery's **transactional
   critical path** (synchronous — "logged" becomes an invariant of "published",
   collapsing the consistency window) plus a bounded staleness SLA.

A `verify_log_inclusion` fuzz target was added
(`crates/lakko-transparency/fuzz/fuzz_targets/verify_log_inclusion.rs`) closing
the "new attacker-touchable verify surface → fuzz target" rule.

**Update — G-30 #2 closed by ADR-0019 (client-authorised log leaf), 2026-06-12.**
Finding 2 is CLOSED and finding 1's rotation case is materially addressed:

- **Rotations are now chain-verified (finding 1, rotation case).**
  `verify_supersession_chain` is FFI-exposed (UniFFI + wasm) and wired into all
  three clients' gate 2 (`verifyRotationChain`): on a key change the rotation is
  bound to a SINGLE committed log version under a no-rollback signed root
  (operator-root sig → no-rollback → inclusion → username match → strictly-newer
  version → old-key supersession proof → leaf-pubkey == proof key). This makes
  leaked-old-key equivocation **detectable** (two distinct successors cannot both
  be log-confirmed at the same version → the second peer to fetch sees a
  version-floor / inclusion failure → `mismatch`). It is detection (depends on
  ≥1 honest fetch + the OOB safety-number backstop), NOT full split-view
  resistance — operator equivocation across DIFFERENT signed roots needs
  witness co-signing (finding 1, **now closed by ADR-0020** — see the update
  below).

- **Append is now client-authorised (finding 2, CLOSED).** Every new
  `(account_key → pubkey)` binding must carry a `leaf_auth`: a genesis / reset
  self-signature by the leaf's own key (domain `lakko/transparency/leaf-genesis/v1`),
  or a rotation supersession proof signed by the previous key. The transparency
  `append` handler verifies it under the append lock and REJECTS an invalid one
  (403); `LAKKO_TRANSPARENCY_REQUIRE_LEAF_AUTH` (default on) also refuses an
  ABSENT one. Discovery threads `leaf_auth` opaquely — stored + forwarded, never
  parsed (invariant #2). All three clients mint the genesis self-sig on a
  new-key publish and verify a served genesis self-sig against the served key on
  a first observation. A leaked append secret can no longer fabricate a keyless
  leaf. **Residual (at the time of ADR-0019): genesis is TOFU** — a self-sig does
  not PREVENT first-entry squatting (an attacker with the append secret can
  self-sign their OWN key for a victim's never-used account_key); it is detected
  via CONIKS self-monitoring + the ADR-0016 fail-closed peer gate. The
  auth-attested-genesis hardening (which PREVENTS squatting) was recorded as the
  elective alternative in ADR-0019 — **now implemented by ADR-0036, see the update
  below.**

- Finding 3 (mismatch / absent posture) is UNCHANGED — still surface-not-block
  (a chain contradiction maps to `mismatch`, not a block; only `rollback`
  hard-fails), consistent with the deferred `lakko-security` ruling.

Two new attacker-touchable verify surfaces got fuzz targets:
`verify_supersession_chain` (rotation chain) and `verify_leaf_genesis` (genesis
self-sig). `lakko-security` re-reviewed Sprints 1+2: **APPROVE-WITH-NOTES**
(invariants #2/#4/#6 hold or strengthened; cleared to merge). See ADR-0019.

**Update — G-30 #1 closed by ADR-0020 (witness co-signing), 2026-06-13.**
Finding 1 (operator split-view / equivocation across DIFFERENT signed roots) is
CLOSED for the single-witness model:

- **An independent witness co-signs every operator root** — but ONLY after
  verifying a Merkle **consistency proof** (RFC 6962 §2.1.2,
  `crates/lakko-transparency/src/merkle.rs`) that the new root append-only-extends
  the witness's last-co-signed root. Two divergent forks cannot both extend one
  root, so a malicious operator cannot obtain a co-signature on a fork: the honest
  witness (`services/witness`, `POST /witness/cosign`) refuses (409 `NotConsistent`)
  and advances its `(root, log_size)` high-water under a lock.
- **Clients require operator AND witness.** All three pin the witness key
  build-time and run `witnessGateRefuses` immediately after the no-rollback gate
  in `verifyIdentityInLog` + `verifyRotationChain`: a root with no cosig OR an
  invalid one (`verify_witness_cosig` over the served `(tree_root, log_size)`,
  domain `lakko/transparency/witness/v1`) is **refused** (`mismatch`, fail-closed,
  invariant #4). No pinned key → operator-only transition posture. The FFI is
  verify-only; signing stays server-side.
- **The operator serves co-signs-on-read, fail-closed.** `services/transparency`
  signs the head root fresh per `/root`, reuses the cached cosig keyed on
  `(tree_root, log_size)`, re-obtains it only on growth, and serves **None**
  (never a stale cosig) when the witness is unreachable — a stale cosig can never
  bind-mismatch a client.
- Two new fuzz targets: `verify_consistency` (the RFC 6962 verifier, a fork must
  fail) + `verify_witness_cosig` (the 105-byte cosig blob parse + bind + verify).
- **Residual:** operator+witness COLLUSION — the single-witness case. **The
  K-of-N witness quorum that raises this bar is now implemented
  ([ADR-0039](../adr/0039-kt-log-witness-quorum.md), 2026-07-21):** the root gate
  now needs the operator signature AND ≥ K valid co-signatures from DISTINCT keys
  in a build-time-pinned witness key set of size N
  (`crates/lakko-transparency/src/witness.rs` `verify_witness_quorum`,
  dedup-by-distinct-key so a replayed / duplicated cosig can never inflate the
  quorum; the transparency service fans out to N witnesses best-effort and serves
  `witness_cosigs_b64: Vec<String>`, the clients enforce the threshold
  fail-closed; N=1/K=1 is the single-witness gate). So a fork now requires
  corrupting K witnesses, not one. The residual is now operator + K COLLUDING
  witnesses, closed in practice ONLY under INDEPENDENT witness operation (the N
  witnesses in distinct trust domains; a deploy/ops property, not code-enforced —
  a single operator running all N witnesses gains nothing). The deferred
  client-gossip layer would remove the witness-collusion residual entirely.

`lakko-security` reviewed the full arc (Sprints 1-6): **APPROVE-WITH-NOTES**
(invariants #2/#4/#6 hold or strengthened; one trivial hygiene finding —
`#![forbid(unsafe_code)]` on the witness service — applied). See ADR-0020.

**Update — G-30 finding 2: genesis squatting PREVENTED (not just detected) by
ADR-0036 (auth-attested genesis), 2026-07-20.** The ADR-0019 genesis self-sig
above only DETECTS first-entry squatting; ADR-0036 adds an **auth-attested
genesis** proof that PREVENTS it, additive to (never replacing) the self-sig.

- **The attestation.** A 153-byte fixed-layout Ed25519 blob
  (`crates/lakko-attest/src/genesis.rs`, domain `lakko/genesis-attest/v1`,
  distinct from device-attest) binding `(account_id, username_hash, pubkey,
  expiry)`. Auth mints it at `POST /auth/genesis/attest` — bearer-authed only, so
  holding a valid session token (proof of OPAQUE authentication) is the entire
  basis; **only the account holder can mint a genesis attestation for their own
  account.** A squatter cannot OPAQUE-authenticate as the victim's `account_id`,
  so cannot obtain auth's signature over the victim's fields. Stated plainly: this
  is a binding of the OPAQUE-authenticated account to a client-ASSERTED key, not
  an independent proof the key is "right" (auth has no such ground truth under
  invariant #2) — which is exactly sufficient, because squatting is precisely the
  case the binding rules out.
- **The bridge derivation.** Transparency never learns `account_id`, so the
  attestation carries `username_hash = BLAKE3("lakko/transparency/account/v1" ‖
  account_id)`, promoted to the one shared `lakko_transparency::account_username_hash`
  + a golden drift test (a one-byte domain drift would silently fail-close the
  gate).
- **The append gate (defence-in-depth, fail-closed).** The genesis/reset arm of
  `services/transparency/src/store.rs` runs `verify_genesis_fresh` (bind
  `username_hash`+`pubkey` + freshness) behind default-OFF
  `LAKKO_TRANSPARENCY_REQUIRE_GENESIS_ATTEST` + pinned
  `LAKKO_TRANSPARENCY_GENESIS_ATTEST_KEY`; under the flag an absent, unpinned-key,
  or invalid/expired attestation refuses the append (invariant #4). Discovery
  threads `genesis_attest` opaquely (migration `0016`, `COALESCE`-preserved,
  invariant #2). All three clients mint on the genesis publish (soft-fail) and
  verify a served attestation on first observation beside the self-sig
  (`verify_genesis_binding`, no freshness — a permanent artifact — fail-closed
  `mismatch`), reusing the pinned DeviceAttestKey. New fuzz target:
  `genesis_attestation_open`.
- **Residual.** Now the DEPLOY ROLLOUT — flip the flag to on once all three
  clients mint (S6, done) — plus a live cross-service end-to-end (mint → publish →
  append-gate → first-observation verify) on a deployed environment. Operator+auth
  collusion stays out of scope (a compromised attest signing key already co-signs
  the root — the same trust tier). See ADR-0036.

### G-32: Update-notice suppress/replay residual (ADR-0038) — **ACCEPTED (LOW), 2026-07**

ADR-0038 adds a signed "update available" notice: the client fetches a **global**,
Ed25519-signed `ReleaseManifest` (89 bytes) as a raw static blob from
`/.well-known/lakko-release`, verifies it against a build-time-pinned **dedicated
publisher** key, and — if its build-time `CURRENT_RELEASE` is behind — shows a
dismissible banner (mobile) with a store deep-link. The signature is an
**anti-forgery** control: a malicious serving tier / MITM **cannot** forge a higher
`latest_release` or `min_supported_release` (a phishing nudge toward a hostile store
link, or manufactured urgency) without the offline release signing key.

**Residual (accepted, LOW).** The same adversary CAN **suppress** the notice (strip
the file → 404 → the client renders no banner) or **replay** a genuine-but-old
manifest (→ an older asserted `latest`). Both are LOW: they keep the user on a
version they **already run** — no confidentiality or integrity break, and the store
link the button opens is unaffected. The client's own fail action on an unverifiable
manifest is *also* "show no banner" (fail-toward-silence), so a suppressor gains
nothing a stripped file would not already give them. Freshness is **DISABLED in v1**
(`max_staleness = i64::MAX`), so a replayed old manifest is not rejected as stale — a
deliberate call given the fail-action is silence and the re-sign-cadence operational
coupling (a static manifest whose `signed_at` ages would otherwise make the check go
dormant everywhere unless the publisher re-signs on a cron).

This is an **availability / anti-phishing** posture, **not** an E2EE control: its
"fail-safe" means *fail toward no nag*, distinct from invariant #4's
fail-closed-refuse-the-connection — it touches no message-path or key-material
invariant. Invariant #1 holds trivially (the manifest is global — no `account_id`, no
per-user field); invariant #2 holds (a public Ed25519-signed static artifact nginx
serves opaquely, no key material). Because it adds no E2EE / MLS / call invariant, it
takes **no indexed C-number** — the invariant index stays at 8 I + 58 C + 15 T. A
future tightening (a persisted highest-`latest_release`-seen floor, or a re-enabled
generous freshness window) is a clean addition if the residual is ever judged worth
closing. See ADR-0038 § "Freshness model + residual".

### G-33: Channel publish-counter not persisted → AEAD nonce reuse across relaunch — **CLOSED (2026-07)**

`lakko-channels` seals each `ChannelPost` with ChaCha20-Poly1305 under a key + nonce
HKDF-derived from `(content_seed, channel_id, epoch_id, sender_pubkey, message_counter)`
(`crates/lakko-channels/src/post.rs`), and the crate documents that `message_counter`
MUST be strictly monotone within `(channel_id, epoch_id, sender_pubkey)` and "lives in
client storage **between sessions**" (`post.rs:142-145`). The channel content seed is
**stable across an app relaunch/reload** (it re-derives from the persisted MLS
PublisherGroup exporter, and `epoch_id` is unchanged), so the counter is the ONLY input
that must be unique per post within an epoch. All three clients held the publish counter
**in memory only** (web `channels-state.ts` signal; iOS/Android `ChannelsUiState`
`counterByChannel`), restarting it at 0 on every relaunch + on `freshPublisherKeys`. A
relaunched publisher therefore re-issued an **identical `(key, nonce)`** on new
plaintexts within the same epoch — catastrophic ChaCha20 keystream reuse
(`P_a XOR P_b = C_a XOR C_b`) + Poly1305 forgery, exactly against the zero-trust server +
subscribers who hold the ciphertext.

**Fix (all 3 clients).** Persist the per-channel counter **at rest** and make it a
**forever-monotone floor** (safe: `epoch_id` is in the HKDF, so a monotone-across-epochs
counter is monotone within each epoch, and needs no per-epoch reset): web
`publish-counter-store.ts` (`localStorage`, non-secret operational metadata — the counter
rides in the clear in the post header), iOS `ChannelPublishCounterStore` (Keychain,
`AfterFirstUnlockThisDeviceOnly`), Android an `EncryptedSharedPreferences` row alongside
the publisher signing seed. **Load-on-init** restores the floor; **write-through** in
`nextCounter` persists `value+1` before returning `value` (a crash after a post can only
SKIP a counter, never re-issue one); `freshPublisherKeys` is **load-else-0** (seed 0 only
for a channel never published to, else keep the persisted floor — mirrors the G-29
publisher-seed reuse-else-generate); **no reset on epoch rotation**; and — the load-bearing
correction — `removeChannel` **KEEPS** the counter (un-publishing does NOT destroy the MLS
content seed, so a re-add of the same `channel_id` re-derives the same seed + epoch; the
counter must stay a floor so a re-add can never rewind it to 0 and re-collide, robust even
though the signing seed IS dropped on removeChannel). Verified: web vitest (19-case
`publish-counter-store.test.ts`, incl. the reload-regression), iOS `xcodebuild` +
Android `:app:testDebugUnitTest` (`ChannelPublishCounterTest`, incl. removeChannel-keeps).
Touches C-40..C-42 (channel publisher path); invariant #7 (the counter now persists in
hardware-backed storage on native).

**Residuals (accepted, narrow).** (a) **`reset()` drops the persisted counters** — safe
ONLY when paired with a full-device wipe (`forgetDevice`, which also clears the OpenMLS
provider so the content seed is gone); a `reset()` followed by a same-epoch
backup-restore of the PublisherGroup could in principle re-collide. `reset()` currently
has no soft-logout caller, so this is not live; flagged so it is never wired to a soft
logout. (b) **Multi-device publisher** (one account publishing the same channel from two
devices) keeps independent counters and could collide — a pre-existing concern the crate's
per-client-storage contract explicitly scopes out; closing it needs a synced publisher
counter (a separate arc).

### G-34: System-wide AEAD (key, nonce)-reuse audit — **T1 CLOSED + S1/S2 HARDENED (2026-07); one narrow chat-snapshot-cadence residual noted**

Following the G-33 channel publish-counter finding (a real ChaCha20 `(key, nonce)` reuse
across relaunch), a **system-wide hunt for the same misuse class** swept every AEAD site:
calls / SFrame, backup / crypto / recovery, MLS / DeviceGroup / pairing, and
transport / push / attachments. The class is: a fixed AEAD key paired with a
non-unique nonce (a fixed / persisted / reset-to-zero counter, salt, or IV) on distinct
plaintexts, which yields keystream reuse (`P_a XOR P_b = C_a XOR C_b`) + Poly1305 forgery.
Outcomes below; the invariant index is unchanged (this is a free-form audit-record entry,
adds no indexed C/I/T invariant — still **8 I + 58 C + 15 T**).

**T1 — CLOSED (this pass).** Shadowsocks-2022 reused the AEAD counter between the
handshake **prelude** and the **body** stream: both began the per-direction AEAD nonce at
0, so the first body chunk reused `(subkey, nonce 0)` (and the second `nonce 1`) already
consumed by the sealed prelude — an on-path censor holding both ciphertexts recovers the
XOR of the two plaintexts and can forge under the reused `(key, nonce)`. Severity HIGH
**in the SS-2022 layer**, but this is a **transport / obfuscation** layer only: the inner
application traffic stays MLS-E2EE, so invariant #2 (server / on-path sees only
ciphertext + public artefacts) holds and there is **no message-plaintext exposure**; the
adversary is a censor, and the anti-censorship stack is built-but-not-deployed (Phase 5,
[ADR-0026](../adr/0026-anti-censorship-wiring.md)). **Fixed:** the body stream now
continues the nonce counter past the prelude (`SS_PRELUDE_NONCE_COUNT = 2`), matching the
SS-2022 single-continuous-counter wire model; both ends shift symmetrically (a closed
ecosystem, [ADR-0011](../adr/0011-rust-sfu.md) / no external SS peer). Touchpoints:
`crates/lakko-transport/src/{shadowsocks,aead_stream}.rs` + nonce-continuity tests
(`prelude_and_body_use_disjoint_nonces_same_subkey`,
`handshake_then_first_body_frame_does_not_reuse_prelude_nonce`, and the
`SS_PRELUDE_NONCE_COUNT` resume-layout pins).

**S1 / S2 — HARDENED (this pass, defense-in-depth, no active bug).** The SFrame
counter-0 / fresh-seed invariant (a new call seed must never repeat under a leaf's
counter, which restarts at 0 per fresh `ParticipantKey`) was **caller-enforced with no
core guardrail** and carried a misleading "harmless" doc on the rekey path. There was no
live reuse — the seed IS fresh per epoch (see V1 / V2 below) — but a future caller feeding
`rekey` the same seed twice, or re-deriving a `ParticipantKey` on an unchanged seed, would
silently reset a counter under an unchanged key. Now `CallKeyRing::rekey` +
`GroupCallSession::rotate_call_key` **no-op on an unchanged seed** (constant-time compare),
the counter-0 / fresh-seed invariant is documented on
`CallKey::{from_seed, participant_key*}`, and property tests capture it. Touchpoints:
`crates/lakko-calls/src/{sframe,call_key_ring,group_call}.rs`.

**V1 + V2 — VERIFIED FALSE-ALARM (recorded so they are not re-discovered).** V1: a
suspected SFrame call-media nonce reuse on a link-based / multi-joiner **crash-rejoin at a
fixed `call_id`** is **not a bug** — the SFrame seed is the MLS exporter output bound to a
**fresh-random group epoch secret** (`InitSecret::random` at `Group::create`), not to
`call_id`. Every host / join founds a group with independent key material, so the exporter
seed never repeats across a rejoin, and the call group is never persisted. V2: OpenMLS
provider persistence — the provider is an in-memory `MemoryStorage` snapshotted after every
mutation (the chat ratchet survives relaunch), but the **call group is never snapshotted**
(it is founded fresh each session), so the call path is outside it and cannot resurrect a
stale call-ratchet state. **Scoped, non-blocking residual noted:** the chat-ratchet "no
generation-nonce repeat" property depends on the snapshot being **flushed durably between a
send and the next send** — a write cadence discipline, not a write-through-DB guarantee. A
crash in the window between a send and its snapshot flush could in principle replay a
message-key generation. This is a separate future look (durable-flush-before-send, or a
pre-send generation reservation), not part of this pass.

**Rest of the crypto surface: CLEAN.** Backup derives a fresh CSPRNG salt → Argon2id KEK
**per envelope** (v1 `envelope::seal`) so key + nonce are fresh; attachments use a fresh
key + nonce per blob; contacts sealed-box uses a fresh ephemeral per message; REALITY's
authenticator is an HMAC (no nonce); ECH is HPKE; ADR-0035 gives every `(leaf, stream)` a
disjoint `(key, nonce_salt)` space; the rekey-overlap ring only retains a **different**-seed
previous key; and all randomness is `OsRng`. Two latent footguns were **doc-hardened**
(INFO, no behavior change): `lakko-backup` `chain.rs::seal_v2` (DORMANT — superseded by
`head_frame` per [ADR-0037](../adr/0037-backup-head-rollback-commitment.md) — takes a
caller-supplied salt + nonce with no fresh-random wrapper; every caller is `#[cfg(test)]`)
now warns the caller MUST pass fresh CSPRNG salt AND nonce and points at v1
`envelope::seal`; `lakko-sframe-native` `lakko_sframe_wrap_frame` (stamps a **bare-leaf**
KID, not the ADR-0035 composite `(leaf << 8) | stream`; touches no key / nonce — the KID is
cosmetic / routing for pre-sealed bytes — and has **no production caller**, test-only) now
notes the keyed media path must use `seal_framed` / `open_framed`.
