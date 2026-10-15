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

## Summary

Every numbered gap, in number order. Entries that are closed with nothing left
named in their status live in [`known-gaps-closed.md`](./known-gaps-closed.md);
everything else is in this file. The state column is a hand reading of each
entry's status line, not a mechanical fact.

| Gap | What | State | What remains |
|---|---|---|---|
| [G-1](./known-gaps.md#g-1) | Channel two-tier flow (>50k subscribers) | Partly closed | the >50k scale proof on a deployed environment |
| [G-2](./known-gaps.md#g-2) | MLS-2 / quantum-hybrid suites | Deferred | post-quantum / MLS-2 suites |
| [G-3](./known-gaps.md#g-3) | External-join proposal approval | Closed, live check owed | a live admin-approval run on a deployed environment |
| [G-4](./known-gaps.md#g-4) | AccountIdentity rotation has no KT-log supersession proof | Partly closed | see the entry's Remaining note |
| [G-5](./known-gaps-closed.md#g-5) | Commit broadcast on `add_device_from_wire` to other pre-paired devices | Closed |  |
| [G-6](./known-gaps.md#g-6) | History bundle `payload_bytes` placeholder | Partly closed | raw ApplicationPayload bytes in the history bundle |
| [G-7](./known-gaps-closed.md#g-7) | DeviceGroup snapshot persistence may briefly diverge across in-flight ratchet operations | Closed |  |
| [G-8](./known-gaps-closed.md#g-8) | Web message-log change listener | Closed |  |
| [G-9](./known-gaps-closed.md#g-9) | Web `payload_bytes` JSON placeholder strings are NOT byte-stable | Closed |  |
| [G-10](./known-gaps-closed.md#g-10) | Backup envelope rotation folded into the recover round-trip | Closed |  |
| [G-11](./known-gaps-closed.md#g-11) | No tamper-evident chain across backup versions | Closed |  |
| [G-12](./known-gaps-closed.md#g-12) | Session token validation is single-process in-memory | Closed |  |
| [G-13](./known-gaps-closed.md#g-13) | WebAuthn passkey per-credential authentic-action attestation | Closed |  |
| [G-14](./known-gaps.md#g-14) | Hostile-OS / hostile-browser threats | Out of scope |  |
| [G-15](./known-gaps.md#g-15) | Side-channel + physical attacks | Out of scope |  |
| [G-16](./known-gaps.md#g-16) | Quantum adversaries | Out of scope |  |
| [G-17](./known-gaps-closed.md#g-17) | Frame-counter exhaustion path is test-exercised | Closed |  |
| [G-18](./known-gaps.md#g-18) | Overlap-window / mid-call rekey | Closed, live check owed | a live mid-call rekey over a deployed SFU |
| [G-19](./known-gaps-closed.md#g-19) | SFU-side call-admission JWT validation | Closed |  |
| [G-20](./known-gaps.md#g-20) | Call recording | Out of scope |  |
| [G-21](./known-gaps.md#g-21) | Link-based call invites | Closed, live check owed | residuals named in the status line |
| [G-22](./known-gaps-closed.md#g-22) | Per-joiner dedup for call invites | Closed |  |
| [G-23](./known-gaps-closed.md#g-23) | `libcrux-chacha20poly1305 0.0.7` — RUSTSEC-2026-0124 (HIGH 8.2) | Closed |  |
| [G-24](./known-gaps-closed.md#g-24) | `rsa 0.9.10` — RUSTSEC-2023-0071 (MEDIUM 5.9) — Marvin Attack | Closed |  |
| [G-25](./known-gaps-closed.md#g-25) | Unmaintained-crate + advisory warnings | Closed |  |
| [G-26](./known-gaps.md#g-26) | RFC 9605 §4.3 SFrame wire header + framed pipeline + cross-platform AAD | Closed, live check owed | live RTP interop over a deployed SFU |
| [G-27](./known-gaps-closed.md#g-27) | Channel PublisherGroup | Closed |  |
| [G-28](./known-gaps-closed.md#g-28) | SFrame on-device wiring | Closed |  |
| [G-29](./known-gaps.md#g-29) | In-code completion markers | Partly closed | the items the entry lists as remaining |
| [G-30](./known-gaps.md#g-30) | KT-log deployed-verifier scope, append-binding trust, equivocation residual | Partly closed | the equivocation residual |
| [G-31](./known-gaps-closed.md#g-31) | Cross-stream SFrame AEAD nonce reuse (multiple streams under one leaf) | Closed |  |
| [G-32](./known-gaps.md#g-32) | Update-notice suppress/replay residual (ADR-0038) | Accepted (low) |  |
| [G-33](./known-gaps-closed.md#g-33) | Channel publish-counter not persisted → AEAD nonce reuse across relaunch | Closed |  |
| [G-34](./known-gaps-closed.md#g-34) | System-wide AEAD (key, nonce)-reuse audit | Closed |  |
| [G-35](./known-gaps-closed.md#g-35) | MLS group-commit publish-before-persist | Closed |  |
| [G-36](./known-gaps.md#g-36) | Push routing under an opaque `push_gid` | Partly closed | the co-membership cluster and the delivery-side residuals |
| [G-37](./known-gaps.md#g-37) | Android chat repository held one repository-wide lock across the DS long poll | Closed on Android | residuals recorded in the entry |
| [G-38](./known-gaps-closed.md#g-38) | Two membership commits in flight to the DS at once | Closed |  |
| [G-39](./known-gaps.md#g-39) | The external cryptographic review of the MLS state machine has not happened | Open | release gate |
| [G-40](./known-gaps.md#g-40) | A second device never joins the account's existing conversations | Open | see the entry's status paragraph |
| [G-41](./known-gaps.md#g-41) | three `libcrux` advisories under the MLS crypto provider | Open | a `cargo deny` failure; analysed as not reachable |
| [G-42](./known-gaps.md#g-42) | A journal's name is its only key | Closing | residuals named in the status line |
| [G-43](./known-gaps.md#g-43) | `services/channels` authorised no publisher operation | Closed, live check owed | not walked live |
| [G-44](./known-gaps.md#g-44) | Rejoining a call in the same MLS epoch reused SFrame `(key, nonce)` pairs | Closed, live check owed | not seen on a live call |
| [G-45](./known-gaps.md#g-45) | After a key reset the old identity's leaves stay in conversation groups until peers evict them | Partly closed | an owned group, a table-less group, a group with no admin contact |
| [G-46](./known-gaps.md#g-46) | A stolen session can reset the account keys; two discovery writes open to a superseded token | Partly closed | session + password, a lifted grant, the discovery part |
| [G-47](./known-gaps.md#g-47) | A fresh identity's phrase rule (ADR-0077 decision 3) | Partly closed | the window between a server reset that succeeded and the owed marker; owed-marking rules still differ |
| [G-48](./known-gaps.md#g-48) | Account store gate coverage (ADR-0077 decision 4) | Partly closed | the stores and paths listed in the entry |
| [G-49](./known-gaps.md#g-49) | An overtaken sign-in can already have changed server state; binding residuals | Open | see the entry |
| [G-50](./known-gaps.md#g-50) | Channel PublisherGroup commits publish before they persist | Open | an outbox-style record; iOS and Android not checked |
| [G-51](./known-gaps.md#g-51) | Channel publish counters restart at 0 after Forget device | Not real as reported | the reported path holds on none of the three; two adjacent reuse paths were fixed |

## MLS state machine

<a id="g-1"></a>
### G-1: Channel two-tier flow (>50k subscribers)

**Status:** **crypto + service + UI + nav + RotationCron + batched fanout + multi-epoch retention + `@channel` client discovery BUILT (Sprints A–D, 2026-07); residual is the deploy-gated >50k scale proof only**

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

<a id="g-2"></a>
### G-2: MLS-2 / quantum-hybrid suites

**Status:** Deferred: pinned for later

The IETF MLS WG is still landing hybrid PQ cipher suite
identifiers. We pin classical suites today and migrate
once the draft stabilises.

<a id="g-3"></a>
### G-3: External-join proposal approval

**Status:** **CLOSED end-to-end on web + iOS + Android ([ADR-0027](../adr/0027-external-join-approval.md) admin approval + [ADR-0028](../adr/0028-group-join-ticket.md) join-ticket invite-link); the only residual is a deployed-env live-admin-approval e2e**

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

<a id="g-4"></a>
### G-4: AccountIdentity rotation has no KT-log supersession proof

**Status:** On password change. **proof + chain verifier + FFI landed (sprints 1-3)**

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

<a id="g-6"></a>
### G-6: History bundle `payload_bytes` placeholder

**Status:** **cross-platform interop CLOSED; raw-ApplicationPayload-bytes aspiration still open**

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

## Multi-device chat membership

<a id="g-40"></a>
### G-40: A second device never joins the account's existing conversations

**Status:** **OPEN, closing under ADR-0050 (web built and live-tested 2026-09-15; natives not started)**

**Status 2026-09-15 (evening).** [ADR-0050](../adr/0050-per-device-conversation-leaves.md)
S1 core, S2 FFI, S3 discovery and S4 web are built; verified live on `test.lakko.me`
with three web clients: a device added through the ADR-0049 confirmation prompt was
admitted to the account's existing 1x1 chat by the confirming device and received the
messages sent after it joined. S5 (iOS + Android) built 2026-09-16, build and unit tests only. Still open under this gap: a live native and cross-platform run, the ADR-0049 confirmation prompt on the natives, incremental
contact sync between devices (the DeviceGroup sync kinds), channel subscriptions
following the device (blocked on the channel list surviving a reload at all), and the
removal fan-out unverified live. Closed later the same day: the scheduled re-admission
arm, the state bundle (contacts, pins, known groups) and the history bundle on the
confirmation route. The text below is the entry as minted that morning.

**Symptom.** Sign in on a second device by any route (QR pairing, the ADR-0049
confirmation prompt, the three-word vault, the full phrase, a passkey): the chat list
is empty, and a chat the first device holds never appears. Observed live on
`test.lakko.me` on 2026-09-15 with two Chrome profiles on one account.

**Cause.** Conversation groups are still on the ADR-0007 **Phase 1** model: one leaf
per *user*, signed by the AccountIdentity key. RFC 9420 section 5.3 requires every
leaf's signature key to be unique within a group, so a second device cannot be added
under the same key (OpenMLS `DuplicateSignatureKey`); discovery publishes one
KeyPackage per *account*, so a contact opening a chat adds one device; and pairing
hands over the DeviceGroup, the phrase seed and a plaintext history bundle only — the
bundle fills the message log, but the chat list is built from MLS group state the new
device does not hold. ADR-0007 named the fix — a per-device `DeviceIdentity` leaf in
every conversation group, bound to the account through the DeviceGroup — as
**Phase 2b** and only ever built Phase 2a (the DeviceGroup itself).

**Why it went unrecorded.** Every ADR that touched membership reported its own scope
as done, and the roadmap's Phase 2a line reads "multi-device sync via DeviceGroup
substrate", which the substrate satisfied. The missing product capability lived only
as an ADR-0007 open question and an ADR-0022 follow-up line, never as a gap. This
entry exists so that "multi-device" is not read as done again until it is.

**What closes it.** Phase 2b end to end: per-device conversation leaves with a
peer-verified device-to-account binding; per-device KeyPackages in discovery; the
existing device adding the new one to every conversation on pairing / confirmation
(ADR-0045 proposals where a role table exists); and every site that today equates a
leaf or the account key with a member (ADR-0022 roles, ADR-0023 authorship, ADR-0042
routing-secret authority, the ADR-0021 peer tag) resolving device to account. It is a
`lakko-mls` change and needs its own ADR plus property tests (invariant #3).

## Calls

<a id="g-18"></a>
### G-18: Overlap-window / mid-call rekey

**Status:** **CLOSED (2026-07, [ADR-0029](../adr/0029-mid-call-key-rotation.md)); only the deployed-SFU live-rekey e2e deferred (like G-26)**

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

<a id="g-20"></a>
### G-20: Call recording

**Status:** Out of scope (deferred per ROADMAP 3c8)

If recording ever lands, the design is "recording client
joins as a regular participant with its own MLS leaf" — no
break to call-side invariants C-1..C-10 in
[`calls.md`](./calls.md). The recorded stream is
end-to-end encrypted just like any other consumer's view.

<a id="g-21"></a>
### G-21: Link-based call invites

**Status:** **multi-joiner CLOSED end-to-end on web + iOS + Android ([ADR-0032](../adr/0032-multi-joiner-call-links.md): owner-founds + deterministic committer election + guest-account join); residuals are the account-creation-spam throttle (deferred backend arc) + a live deployed-SFU N-joiner/failover e2e (G-26-class)**

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

## Out-of-protocol

<a id="g-14"></a>
### G-14: Hostile-OS / hostile-browser threats

**Status:** Out of scope

No app-level integrity attestation. We rely on the
platform's standard sandbox + the user's awareness. Phase 5
adds App Attest / Play Integrity / WebAuthn-attestation to
the signup / login round-trip.

<a id="g-15"></a>
### G-15: Side-channel + physical attacks

**Status:** Out of scope

Out of scope for this audit pass. Mitigated by relying on
the platform's constant-time crypto primitives (`subtle`
crate, OpenMLS's vetted implementations). No hand-rolled
crypto in the codebase — the `lakko-crypto` crate is
plumbing only.

<a id="g-16"></a>
### G-16: Quantum adversaries

**Status:** Out of scope

MLS suites pin classical curves today. PQ migration
follows the WG.

<a id="g-26"></a>
### G-26: RFC 9605 §4.3 SFrame wire header + framed pipeline + cross-platform AAD

**Status:** §4.3-conformant, only live deployed-SFU RTP interop pending

**Update (2026-10-11) — the media path no longer uses the frame this
entry describes.** [ADR-0071](../adr/0071-codec-header-in-the-clear.md)
(Proposed) moved call media to `clear_prefix || ciphertext_and_tag ||
sframe_header || trailer`: the web client on 2026-10-07, the native bridge
on 2026-10-11. Read the 2026-07 update below with three corrections. (1)
The header codec is still RFC 9605 §4.3, but a whole frame is no longer
the RFC's shape, so the statement that a Lakko frame interoperates with
any other RFC 9605 implementation does not hold for call media; ADR-0071
says so. (2) The golden vector below pins the header-first `seal_framed` /
`open_framed`, which remain in the core, both FFI crates and the C ABI and
which no client's media path calls. The media layout has a second golden
vector, pinned in the core (`media_frame.rs`), the wasm FFI, the C ABI
test (`tests/media_frame_c_abi.rs`), the web suite and
`test_bridge.cc`; the iOS and Android golden tests go through UniFFI, which
has no entry point for the new layout, and pin the old one only. (3) The
residual is unchanged in kind and larger in fact: no live frame has
crossed from a browser to a phone or back in the new layout, and for four
days (2026-10-07 to 10-11) the two sides could not have opened each
other's media. The web side has carried video between browsers on live
calls; nothing native has been run.

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

## Maintenance + reconciliation

<a id="g-29"></a>
### G-29: In-code completion markers

**Status:** Swept + reconciled; residual genuinely-open items

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
  gap. (This entry also said the MLS whole-storage snapshot is restored on
  recover on both; since 2026-10-02 the backup carries no MLS state and
  neither client loads one, see `backup.md`.)

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

## Key transparency

<a id="g-30"></a>
### G-30: KT-log deployed-verifier scope, append-binding trust, equivocation residual

**Status:** **#2 closed (ADR-0019) + genesis squatting PREVENTED (ADR-0036, flag-gated), #1 closed (ADR-0020)**

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

## Client release / update

<a id="g-32"></a>
### G-32: Update-notice suppress/replay residual (ADR-0038)

**Status:** **ACCEPTED (LOW), 2026-07**

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
takes **no indexed C-number**, so the invariant index is unchanged by it (it stood at
8 I + 58 C + 15 T when this was written; the live counts are in the gated Quick stats
table in [`invariant-index.md`](./invariant-index.md), which is the only place they
are maintained). A future tightening (a persisted highest-`latest_release`-seen floor, or a re-enabled
generous freshness window) is a clean addition if the residual is ever judged worth
closing. See ADR-0038 § "Freshness model + residual".

## Push routing

<a id="g-36"></a>
### G-36: Push routing under an opaque `push_gid`

**Status:** The co-membership cluster and the surviving delivery-side residuals ([ADR-0042](../adr/0042-push-opaque-group-routing-id.md) + [ADR-0043](../adr/0043-push-routing-secret-rotation-on-removal.md), 2026-08)

[ADR-0042](../adr/0042-push-opaque-group-routing-id.md) landed end-to-end (core +
FFI + web + iOS + Android; `lakko-security` **APPROVE-WITH-NOTES**, no
CRITICAL/HIGH) and is recorded as **C-60**. This entry is the honest inventory of
what it did **not** close. Like G-33 / G-34 / G-35 this is a **free-form audit
record** — it adds **no** indexed C / I / T invariant of its own (C-60 is ADR-0042's
positive property and **C-61** is ADR-0043's; the counts moved to
**8 I + 61 C + 15 T** because of those two, not because of this gap, and have moved
again since — the live figures are in the gated Quick stats table in
[`invariant-index.md`](./invariant-index.md)).

**Update (2026-08-11) — [ADR-0043](../adr/0043-push-routing-secret-rotation-on-removal.md)
landed on all three clients (`935250a` web, `a6ed544` iOS + Android), rotating the
secret on member removal.** It **closes residual 2** (both halves), **widens
residual 3**, leaves **residual 1** substantively intact, and **adds residuals 5
and 6**. C-60 is **refined** rather than renumbered (the routing id is the same
property, now also forward-unlinkable across a removal), and the sequencing rule
the rotation depends on is pinned as the new **C-61**. The `lakko-security` pass
ADR-0043 declares mandatory for option (b) **was run: APPROVE-WITH-NOTES, no
CRITICAL/HIGH, no invariant #2/#3/#4 break on the live path**, with one blocking
MEDIUM (**F1** — a coalescing rotation lock on web + iOS folded a second, distinct
removal into an in-flight rotation, handing the second evictee the new secret;
Android's serializing `Mutex` was already correct) now fixed on both platforms by
adopting Android's lock. Its remaining findings are carried as residuals 3, 5, 6
and 7 below.

**What ADR-0042 actually bought, stated precisely.** Push subscribe / notify / the
delivered payload now route under `push_gid = keyed-BLAKE3(push_routing_secret,
"lakko/push/group-routing-id/v1")` over 32 CSPRNG bytes that never leave the E2EE
group, so the routing id is **algebraically independent of the DS `group_id`** and a
push-gw↔DS cross-correlation is impossible without the secret. That removes the
**linkage to the DS group identity**. It does **not** remove the **clustering**.

#### Residual 1 — the co-membership cluster is still materialised (the main one)

`services/push-gw` still stores device↔routing-id rows
(`push_group_subscriptions`), and APNs / FCM still receive a **stable per-group
pseudonym in cleartext** (the wake payload `g` plus the APNs `thread-id`, both
`hex::encode` of the routing id — `providers/apns.rs:368`/`:371`,
`providers/fcm.rs:369`). A provider (or push-gw itself) can observe "these N device
tokens share one opaque group handle" and track that cluster **and its churn** —
it simply can no longer join that handle to the DS journal key. The set-membership
and its evolution over time remain visible; only the group's *name* is opaque.
WebPush is unaffected either way (the whole `WakeBody` is RFC-8291
`aes128gcm`-encrypted before it leaves push-gw, `providers/webpush.rs:252-256`).

**ADR-0043's rotation does NOT close this, and the precise reason matters.**
Rotation **renames** the cluster; it does not unlink it. A rotation is not a
mixnet — the device-token set is identical-or-near-identical either side of the
boundary, so the provider **re-links across it by token-set intersection**. Worse,
the set *difference* across the boundary **names the evicted device**, which is
arguably a signal the stable id did not hand over so legibly: before ADR-0043 a
removal was invisible to the provider (nothing changed); after it, a removal is a
visible re-keying event with an identifiable diff. That trade was made
deliberately — it buys the closure of residual 2, a live channel to an
already-adversarial evicted party, at the cost of legibility to a party that
already watches the cluster continuously — but it is a judgement, and it means the
cluster leak is **unchanged in substance**.

Closing this needs **per-device-blinded routing ids** — each subscriber
registering under an id only it and the publisher can compute, so no two devices
ever share a key-space value — plus payload minimisation. That is the first change
in this arc that would **not** leave `services/push-gw` untouched (notify would
need per-recipient ids rather than one JOIN key). A rotating id is not a
substitute.

#### ~~Residual 2 — no rotation on membership change, compounded by dead unsubscribe~~ CLOSED (2026-08-11, ADR-0043)

**The original finding.** V1 deliberately did not re-mint the secret when a member
was removed, so **a removed member kept a valid `push_gid`** and kept deriving the
group's current routing id — an ongoing post-eviction metadata channel ("this group
was active at time T"), not merely stale rows. This was compounded by a second,
independent finding: **per-group unsubscribe was never called anywhere.**
`unsubscribeGroup` existed on all three clients
(`apps/web/src/push/client.ts:204`, `apps/ios/Lakko/Push/PushClient.swift:165`,
`apps/android/.../push/PushClient.kt:197`) and the `POST /push/group/unsubscribe`
endpoint was live (`services/push-gw/src/main.rs:250`), but **no call site existed**
— dead code on every client. The two compounded: rotation alone would not help
while the stale subscription persisted, and unsubscribe alone would not help while
the ex-member could still derive the id.

**Both halves are now closed by [ADR-0043](../adr/0043-push-routing-secret-rotation-on-removal.md).**

- **The wake stream ends — but state the mechanism exactly.** On a membership
  shrink the owner **or an admin** (ratified option (b)) re-mints and
  redistributes the secret, sealed at the **post-remove epoch**, so RFC 9420
  forward secrecy denies the evicted leaf the envelope. The ex-member's push-gw
  row is **not deleted by anyone else** — nobody else *can* delete it
  (`group_unsubscribe` removes only the `(device, group)` row named in the request,
  and a device knows only its own pseudonym) — so from the group's side it goes
  **INERT**: nothing ever notifies the old gid again. **That inertness is the
  win** — the ex-member stops learning the group is active — and it is
  deliberately *not* cleanup.

  > **AMENDED 2026-08-29 (`8763f3e`): the clause "and no client has 'you were
  > removed' handling" no longer holds, and with it the row's permanence.** The
  > ADR-0045 self-eviction work built exactly that handling — `Group::is_active()`
  > surfaced through both FFI surfaces, and a `retire-push-subscription` teardown
  > step on all three clients — so **the evicted device deletes its own row**
  > (`ENDED_TEARDOWN_STEPS` in `apps/web/src/chat/membership-ended.ts` and its two
  > native twins, reusing the same `retireOldPushGid` the rotation teardown uses).
  > That closes the half of this residual ADR-0043 recorded as unclosable. **The
  > bound is real and is stated rather than glossed:** it needs the evicted device
  > to come online and run it, so it **complements** rotation and replaces nothing —
  > a device that never returns still leaves its row, inert.
- **Per-group unsubscribe stops being dead code.** Its **first real caller** on
  every client is the receive-side teardown, in which each remaining member
  retires **its own** old gid (unsubscribe + delete the reverse-index row): web
  `apps/web/src/push/group-routing.ts:336`, iOS
  `apps/ios/Lakko/Push/PushGroupRouting.swift:220`, Android
  `apps/android/.../chat/ChatRepository.kt:1670`. This half is **required, not
  hygiene**: without it every member would leave one live push-gw row per
  rotation, turning a single live cluster into a live cluster **plus a frozen
  archive of every previous one** — a strictly richer artifact for residual 1 than
  not rotating at all.

Rotation fires only on a membership **shrink**, detected as a set difference over
member Ed25519 signature keys (not counts, not leaf indices — OpenMLS reuses a
vacated leaf for the next Add). It **never mints**: a device holding no secret is a
no-op, so widening rotation to admins cannot create a second minter.

#### Residual 3 — admin denial of service, WIDENED from seed to churn (ADR-0043)

The `62f23ad` refinement widened the receive gate from owner-only to **owner +
admins** to fix a real delivery regression (ADR-0022 lets an admin add members, and
a member added by a non-owner admin would otherwise never receive the secret). At
that point an admin could **seed** a secret into a hole but never **overwrite** a
stored one, so the residual was: a malicious admin can seed a bogus secret to a
member who holds none, denying that member's wakes until the owner next rotates.

**ADR-0043's ratified option (b) widens this.** The gate's admin row flipped from a
refusal to an acceptance (`reject-admin-overwrite` -> `accept-admin-rotate` on web;
the natives keep a binary accept/reject and simply flip the row), so an admin may
now **overwrite whatever is stored**. The residual therefore generalises from *"a
malicious admin can seed a member holding none"* to **"a malicious admin can churn
the whole group's routing id at will"**, repeatedly denying **every** member's
wakes until the owner rotates back.

Still bounded, and for the same reasons as before: foreground polling keeps
delivering messages, so this is a wake-latency denial rather than message loss; and
an admin can already remove members outright, so the capability gain is small. But
the widening is real and is recorded rather than waived — it re-opens the shape of
the `lakko-security` **MEDIUM-1** fix (`21554f2`), which existed precisely to stop
a non-owner from unilaterally rotating the routing id.

**Android additionally deleted a prior hardening to make (b) work.** The
strip-admins-on-unreadable-store rule (from the MEDIUM-1 arc) narrowed the admin
set when the secret-store read *threw*, so an unreadable store could not be
mistaken for a fillable hole. Under (b) that premise is gone — an admin may
overwrite whatever is stored — so keeping the rule would have refused the very
admin rotation (b) exists to permit, on nothing more than a transient store error.
It is removed (`GroupPushRouting.kt:499-504`, historical note `:464-474`). Who may
write is still decided by the role table and is still fail-closed; an unreadable
**role table** (as opposed to an unreadable secret store) still refuses. Web and
iOS encoded the same premise inline via a `storedUnreadable` flag rather than by
rewriting the authority, and both kept the parameter in a demoted role (web picks
*which* accept to report; iOS keeps it as an unread diagnostic).

**What did NOT change, and is what keeps (b) bounded:** rotation **never mints**.
Option (b) widened *overwrite* authority only, so an admin can replace a secret the
group already holds but can never bring one into existence — minting stays
owner-only, so the group's `push_gid` can never diverge into two.

**One further consequence, and the server-side premises it rests on.** Under (b) a
malicious admin can keep **re-establishing** a routing id it shares with an evicted
party *after each owner rotation*, rather than only leaking the current one once.
This is **not a new capability class** — an admin could always exfiltrate the
current secret out-of-band — but it turns a one-shot leak into a **sustained** one,
because the admin can re-converge the group onto a secret of its choosing every
time the owner rotates away from it. Two server-side facts make that directly
exploitable and are worth stating as premises rather than leaving implicit:

- **`POST /push/group/subscribe` performs no membership check.** push-gw cannot
  perform one — it is opaque to group membership by design (invariant #2) — so a
  shared secret is *directly subscribable* by a non-member. Possession of the
  secret **is** the authorisation.
- **`POST /push/group/unsubscribe` carries no ownership proof.** It deletes
  whatever `(device, group)` pair is named in the request and is PoW-gated only.
  So the claim elsewhere in this entry that "no remaining member can tear down the
  ex-member's row" rests on **the device pseudonym never being published**, not on
  any server-side authorization. That is a confidentiality assumption about an
  identifier, not an access-control guarantee.

Accepted, not closed. This residual is the substance of the `lakko-security` pass
ADR-0043 declares mandatory for option (b), which **was run — APPROVE-WITH-NOTES,
no CRITICAL/HIGH** — and which recorded both server-side premises above as explicit
judgements rather than leaving them implicit.

#### Residual 4 — legacy / no-role-table groups get no push at all

The send side mints only when the local member is the ADR-0022 **owner**. A legacy
or open group has **no role table**, hence no owner, so **nobody mints** (the
in-code rationale is explicit: "nobody mints (avoids divergence)",
`apps/web/src/push/group-routing.ts`). Every member therefore **defers** — a member
without the secret does not subscribe at all rather than falling back to the leaky
raw `group_id`. The privacy posture is the preferred one (invariant #4 — no silent
downgrade), but the functional consequence is real and silent: **pre-ADR-0022 groups
receive no push wakes whatsoever**, with no user-visible signal. A provisioning path
for legacy groups (or an explicit UI state) is unbuilt.

**ADR-0043 does not change this, and adds a second clause: such groups now also
never rotate.** The rotation decision refuses a group whose local role resolves to
neither owner nor admin, which a no-role-table group always does
(`skip-not-authorized`). Note the asymmetry, because it is easy to invert: the
**receive gate** *accepts* a changed secret in a no-role-table group
(`accept-no-owner` on web; the same branch on both natives) — that is the legacy /
open-group behaviour ADR-0042 established. It is the **rotation** decision that
refuses. Receive-accept and rotate-refuse are different questions about the same
group.

#### Residual 5 — the old gid can be orphaned, and NOT only by a crash (new, ADR-0043) — **one of its three paths (F-E, rotate-vs-receive) is CLOSED**; the crash and store-read-failure paths remain accepted

ADR-0043 prescribed a durable pending-rotation record modelled on the ADR-0040
commit outbox. **It was deliberately not built**, because its stated justification
went stale between that ADR being written (`5665018`) and being implemented: the
three commits `5729aa0` -> `9cb8f06` -> `7e7d143` had meanwhile added a durable
`distributed` flag on the secret row plus a re-broadcast branch open to **any**
holder, reached from both chat-open and the launch sweep — so a failed rotation
broadcast is already retried without a new record.

**What the flag does not cover** is the other side effect. It recovers the
**broadcast**, never the **teardown**: nothing anywhere records the OLD gid
awaiting retirement, which lives only as an in-memory local between the persist and
the unsubscribe. Once the secrets row is overwritten (it is keyed
`(account, group)` and clobbered in place) the old bytes are unrecoverable, the old
`push_gid` can never be named again, and its push-gw subscription plus
reverse-index row become a **permanent orphan**.

**Three paths reached it, and neither of the two that survive involves a crash at
all** (the third is now closed):

1. **A crash** between the persist and the teardown.
2. **A transient secret-store read failure on the receive path.** The teardown is
   conditioned on the *previously stored* secret, snapshotted before the persist.
   If that read fails it yields `stored = null` on all three clients — which is
   indistinguishable from "this device held nothing" — so the new secret is
   persisted and the teardown is simply **skipped**. No crash, no error surfaced,
   same inert orphan. This is **more likely** than the crash window, since it needs
   only a momentary store hiccup rather than process death inside a narrow window.
3. **~~A rotation/receive interleaving~~ (`lakko-security` **F-E**) — CLOSED**
   (`9a0f539`). Rotation and receive were **not mutually excluded** — no client's
   receive gate took the rotation lock — so a peer's envelope could interleave with
   a local rotation and leave an **intermediate** gid subscribed with nothing
   pointing at it. It was bounded the same way as the other two (every candidate id
   in that interleaving is **post-removal**, so never an id an evicted member can
   derive), and it is now closed outright because the residual-10 fix put receive,
   rotate, ensure and redistribute on **one shared** per-`(account, group)` lock —
   they exclude each other rather than each merely excluding itself. Android
   **proved the sharing is what does it**, by probing with receive on its own
   separate lock: the F-A and N-rotation cases passed while **F-E still failed**.
   On iOS this was the interleave that was genuinely real (its rotation parks in
   its broadcast), and its test failed four assertions before the fix.

**Verdict: accepted, and the durable record should NOT be built** — an explicit
`lakko-security` judgement on this arc, not merely this entry's opinion. The orphan is
**inert** — nothing ever notifies the old gid again — so this is an accumulation
defect, not a live channel, the same class as residual 2's ex-member row. What the
row leaks is bounded to "this device was once in *some* group", with no linkage to
which group or to the current cluster. Building the durable record to reclaim it
would cost a persist on a hot path plus a launch sweep, which is not proportionate
to reclaiming a row that leaks nothing further. Recorded rather than fixed. (Were
it ever built, it would be **lighter** than ADR-0043 prescribed: the new secret is
already durable by then, so only the *old gid* needs recording — an "old gids
awaiting retirement" list.) All three paths are accepted on the same reasoning.

#### Residual 6 — the post-Remove-epoch ordering (C-61) was enforced structurally, with no test — **CLOSED** (2026-08-14, [ADR-0044](../adr/0044-push-routing-secret-recency.md))

The whole of residual 2's closure depends on the rotation broadcast being emitted
**strictly after** the Remove commit, so that the envelope is sealed at the
post-remove epoch and RFC 9420 forward secrecy denies it to the evicted leaf. That
rule is pinned as **C-61**. As ADR-0043 shipped it was enforced **structurally
only** — by call-site sequencing (an awaited publish, or a lock block that closes)
on each client — with **no test on any client**; only the sibling
read-old-before-overwrite rule was test-pinned. ADR-0043 itself states the
ordering "must be asserted in a test"; that requirement was unmet.
(`lakko-security` **F3**, LOW — the review reached the same finding
independently.)

**The dependency sharpened before it was closed, which is why the test stopped
being optional.** Under ADR-0043 alone, a C-61 regression cost the rotation's
**forward secrecy**: the envelope would be sealed at the pre-removal epoch and the
evicted member could open it. Under **ADR-0044** it costs that **and inverts the
recency comparison** — a rotation stamped at the pre-removal epoch `N` **ties**
with the stale `S_old` minted at `N`, so the tie falls to the 32 secret bytes and
the rotation may simply **lose to the stale secret**. The guarantee degrades from
"the rotation always wins" to **a coin flip**, silently. ADR-0044 therefore
converts C-61 from a property protecting one guarantee into a **precondition of
two**, and landed the regression tests in the same arc rather than deferring them.

**CLOSED by `014e84e`** (web's landed in `936d849`). What that means concretely:

- **One named seam per client**, so the two steps cannot be reordered
  independently — e.g. iOS
  `PushGroupRouting.publishRemoveThenRotate(publishRemove:rotate:onRotationStarted:)`,
  whose sole production call site is `ChatRepository.removeMemberFromGroup`.
- **The assertion is on the emitted envelope's `mint_epoch` stamp, not on call
  order** — a strictly stronger check, since it fails on any path that reaches the
  broadcast with a pre-removal epoch, however the reordering happened.
- **A permanent inverted-order companion on each client** that runs the swap and
  asserts the consequence: web `the INVERTED order would stamp the pre-removal
  epoch and lose the guarantee`; iOS
  `testTheInvertedOrderStampsThePreRemovalEpochAndTheRotationLoses`; Android
  `` `C-61 - the INVERTED order stamps PRE-removal and the rotation LOSES` ``
  (which additionally asserts the *gate* agrees, resolving the stale replay as
  `ACCEPT_OWNER` over the rotation — F-A reproduced under the swap).
- **On both natives the discrimination was proven by running the swap, not
  asserted**: iOS temporarily added an inverted copy and recorded three failures;
  Android replaced the seam with rotate-then-advance and recorded the stamp
  assertion failing 11 vs 10. Both probes were reverted.
- Counts: **six** C-61 tests on web, **five** on iOS, **six** on Android.

**The fixture is load-bearing, and its first cut was latently flaky.** A test that
exists to prove the rotation *loses* under the swap must make it lose with
certainty, and at an epoch tie the winner is the bytewise-greater secret. The web
fixture landed in `936d849` filled the stale secret with `0xEE`, so a freshly
minted **random** rotation secret sorts higher roughly once in fourteen runs and
the rotation would have **won** — passing a test written to watch it lose. All
three now use `0xFF`, the bytewise maximum, so the rotation loses
deterministically. Recorded because the failure mode generalises: a
discrimination test whose fixture does not force the outcome is a test that
sometimes proves nothing, silently.

#### Residual 7 — concurrent rotators can strand a device on a superseded secret (ADR-0043; `lakko-security` F2, MEDIUM) — **CONCURRENT-ROTATOR CORE CLOSED for the single-context case** ([ADR-0044](../adr/0044-push-routing-secret-recency.md) + the serializing lock, `9a0f539`); the F-D stale-role window and the no-repair limit remain open

A rotator applies **its own** mint at **local** time, not at its own position in
the DS journal. So a device that publishes its rotation at journal id `p` and then
polls an earlier message `q < p` it had not yet seen will **overwrite its own newer
secret with the older one** — and the row is then marked `distributed: true`, so
nothing ever re-broadcasts to repair it.

Under option (b) this is **routine rather than a race**: one removal produces
**N+1 rotations** in a group with an owner and N admins, because the committer
rotates from its own remove path while every other owner/admin observes the same
shrink through its poll diff and rotates too. The affected device keeps deferring
or routes under a gid nobody notifies, self-healing only at the next rotation.
Foreground polling delivers messages throughout, so this is a **wake-delivery
defect, not message loss**.

**What ADR-0044 built, and what it took to make it hold.** The root cause above is
*mutual incomparability*: the N+1 authorised mints had no order between them, so
which one a device ended on was decided by arrival order and devices
**partitioned**. ADR-0044's total order on `(mint_epoch, secret_bytes)` removes
exactly that — the N+1 mints from one removal share an epoch (no intervening
commit), so the 32-byte tiebreak decides deterministically, identically for every
participant, and independently of arrival order. **This is the load-bearing reason
the tiebreak is not a corner case:** epoch alone would leave the N+1 mints
incomparable.

**The order alone was not enough, and that is the arc's lesson.** The N+1
same-epoch rotations typically arrive in **one poll batch**, and as first shipped
the receive gate was an unserialized read-modify-write dispatched fire-and-forget
per envelope — so all N+1 gates compared against a **single pre-batch snapshot**
and the survivor was decided by **scheduling order**. Stated exactly: **the total
order's arrival-order independence holds for the pure function, but not
automatically for the path that calls it.** It becomes a property of the device's
resulting state only once the gate consuming it is serialized. That serialization
landed in `9a0f539` — one per-`(account, group)` lock spanning the whole
read-decide-write, shared across receive / rotate / ensure / redistribute — and is
now pinned as **C-62 sub-invariant 5**.

**CLOSED, with the qualification stated precisely.** The concurrent-rotator core
is closed **for the single-context case**: within one JS realm (web), one app
process (natives), and — on web — across tabs where `navigator.locks` is
available. The remaining un-serialized boundaries are the re-scoped **residual
10** and are the qualification on this closure, not a separate reservation.

**Three further things survive the lock and remain open.**

1. **The F-D stale-role window, untouched and orthogonal** (below). A demoted
   rotator's envelope is refused on **authority**, not on recency, so an ordering
   predicate has no bearing on it whatsoever.
2. **An ordering gate stops a stale value SPREADING; it does not repair an
   already-stale holder.** The order pushes nothing. A device that ends on a loser
   converges only when an authorised newer envelope actually reaches it — in the
   normal case the winner still sitting in the journal ahead of its cursor, but
   **not** if it has already polled past that position (or the 30-day journal
   prune has taken it). A self-healing reply-with-my-newer-one was considered and
   **deferred**: it needs a storm guard, since N members each answering one stale
   broadcast is an N-fold journal amplification.
3. **The N+1 rotations themselves still happen.** The order governs the *final
   state*; it does not reduce the number of mints or the transient subscribe /
   unsubscribe churn they cause. Their **F-E orphan-gid exposure is now closed**
   by the shared lock (residual 5, path 3), but residual 5's crash and
   store-read-failure paths still apply to each of the N+1.

**The web-only stale-role sub-case remains fixed** — that is true for the case it
names (the `ownRole()` signal holding a pre-poll value; web now reads the role off
the live group via `readOwnRoleNow`).

**A narrower stale-role window survives on ALL THREE, and is narrowed rather than
eliminated** (`lakko-security` **F-D**). Every client captures the role **before**
the rotation queues behind a predecessor on the serializing lock — web
`Chat.tsx:2297` (`await readOwnRoleNow()` feeding a call that then takes the
`rotating` chain), iOS `ChatRepository.swift:1590` (`ownRole(groupIdHex:)` read
before the `Task` at `:1591` chains), Android `:1960` and `:1997`
(`rotationRole(...)` read before `pushScope.launch`). A **demotion that lands during
the queue wait** therefore rotates on a **stale** role: peers run the receive gate
against the *current* role table, refuse the envelope, and the rotator **strands
itself** on a secret nobody else accepts. **Availability-only** — it is the
fail-closed gate behaving correctly — and it **self-heals at the next authorised
envelope**, since the rotator's row still accepts any owner/admin write. Not fixed;
closing it would mean re-reading the role *after* acquiring the lock.

Note this also falsifies the convergence argument ADR-0043 originally offered for
option (b) ("the DS journal is a total order, so last-accepted-wins converges"),
which is corrected in that ADR.

#### Residual 8 — the receive-side teardown can precede the new subscription (new, web only; `lakko-security` F4, LOW)

On web the receive-side teardown may run **before** the new subscription is
established, leaving the device briefly subscribed to **neither** the old gid nor
the new one. A wake arriving in that window is not delivered; the message itself is
unaffected and arrives on the next poll. Recorded as a follow-up, not fixed.

#### Residual 9 — a stale `pending` re-broadcast can silently UNDO a rotation, permanently (ADR-0043 re-review; `lakko-security` F-A) — **CLOSED for the single-context case** ([ADR-0044](../adr/0044-push-routing-secret-recency.md) recency order + the serializing lock, `9a0f539`)

The one residual on this list whose consequence is **confidentiality rather than
wake delivery**, and it needs **no malice and no crash** — only a failed broadcast
followed by someone else's rotation. The mechanics below are kept as the
historical record; the status of the attempted fix is at the end of this entry.

1. An owner/admin's post-add `redistributeSecretToGroup` **broadcast fails**. The
   failure path marks the row `pending` (`markGroupRoutingPending`), so that device
   now holds the **PRE-rotation** secret `S_old` in an un-settled row.
2. Meanwhile another admin removes a member and rotates the group to `S1`
   (ADR-0043, ratified option (b) — any owner *or* admin may rotate).
3. The first device runs `ensureGroupPushRouting` **BEFORE** it applies the `S1`
   envelope. Its `pending` branch — the any-holder re-broadcast added in `7e7d143`
   — re-sends **`S_old`**.
4. Every member resolves that envelope as `accept-owner` / `accept-admin-rotate`,
   persists `S_old`, **and retires `gid(S1)`**. The group migrates **BACK** onto the
   id the evicted member still derives and is still subscribed to.
5. No further membership shrink occurs, so **no further rotation fires**. The state
   is **permanent, not transient** — unlike residual 7, nothing self-heals it.

**Root cause, stated plainly: the receive gate has no recency or monotonicity
notion at all.** `routingSecretVerdict` (web) and its iOS/Android twins ask only
*who sent it* — owner / admin / member, against the ADR-0022 role table — and never
*is it newer than what I hold*. Every input to the verdict is an authority question.
The any-holder re-broadcast branch and rotation are individually sound; they
interact badly precisely because no ordering predicate exists to separate a
**retry** of the current secret from a **replay** of a superseded one. The two are
byte-indistinguishable to the gate.

**Reachability differs per client**, and the ordering that narrows it is incidental
rather than designed:

- **Web** narrows it: `Chat.tsx:2497` `await refresh()` before `provisionPushRouting()`
  at `:2504`, so a normal chat-open applies the pending `S1` envelope first. But
  `refresh()` catches its own errors (`Chat.tsx:1694-1698` — `setError` and return,
  never rethrow), so a transient poll failure resolves the await normally and lets
  provisioning run anyway. Narrowed, not closed.
- **iOS** (`ChatRepository.swift:1471`) and **Android** (`:229`, `:339`, `:375`,
  `:1888`) fire provisioning on **independent tasks** with no such ordering, so the
  window is **wider** there.

**Severity: recorded, not blocking.** The worst case is **exactly the pre-ADR-0043
status quo** — the evicted member keeps receiving wakes under an id it already
holds — plus some inert orphan rows of the residual-5 class. It fails **TO** the old
behaviour, never below it: no member is downgraded to the leaky raw `group_id`
(invariant #4 holds), the DS and push-gw stay opaque (invariant #2), and no message
content is exposed. What it does is silently **revoke the ADR-0043 win** for that
group, with no user-visible signal — which is why it is recorded here rather than
folded into residual 7.

Note also that **residual 7's framing understates this variant.** That entry closes
with "a wake-delivery defect, not message loss", which is accurate for its own
scenario (a device stranded on a superseded secret misses wakes). This one is not a
delivery defect at all: its consequence is the **confidentiality** property C-60
sub-invariant 6 and C-61 exist to protect.

**Fix direction — recorded as a follow-up, deliberately NOT built in ADR-0043:**

1. **The general fix** — carry a **monotone counter or the minting epoch inside the
   62-byte distribution envelope** and refuse a non-increasing value on receive.
   This gives the gate the recency predicate it structurally lacks and closes the
   whole class, not just this path. It is a **`crates/lakko-contacts` wire change**
   (the envelope is `domain(29) ‖ version(1) ‖ secret(32)`, a fixed-length payload
   with a version discriminator), so it needs an envelope version bump, the
   fail-closed codec extended, its fuzz target updated
   (`push_routing_secret_decode`), and all three clients moved together.
2. **The narrow fix** — clear or skip the `pending` re-broadcast once a
   **different** secret has been accepted. Purely client-side, no wire change, but
   it patches this path rather than the missing predicate.

**CLOSED, in two steps, and the second step is the interesting one.**
[ADR-0044](../adr/0044-push-routing-secret-recency.md) built fix direction 1, the
general one (`936d849` core + FFI + web, `014e84e` iOS + Android). The
`lakko-security` review of that arc nonetheless returned **REQUEST CHANGES** and
**reproduced this residual by execution** on web, driving two envelopes
concurrently the way `Chat.tsx` actually drives them and observing verdicts
`['accept-admin-rotate', 'accept-identical']` with a final stored secret of `0xEE`
**at epoch 10** — the stale one. The reason was **residual 10**, not a fault in the
primitive: the ordering gate was correct and armed, but the receive path was an
unserialized read-modify-write dispatched fire-and-forget per envelope, so the gate
was handed a **stale snapshot** and was **bypassed, not defeated**. The vehicle was
`accept-identical`, which deliberately short-circuits **both** the future clamp and
the total order — correct for a benign retry, and ADR-0044 correctly stopped that
path from moving the stored epoch, but nothing stopped it being *reached* on a
stale snapshot and rewriting a superseded secret.

**The second step closed it** (`9a0f539`): one per-`(account, group)`
**serializing** lock spanning the whole read-decide-write, **shared** across all
four entry points — receive, rotate, ensure, redistribute. With it, the argument
below is finally the argument the device actually runs: the gate re-reads inside
the lock, so a stale re-broadcast meets the **current** `(secret, mint_epoch)` and
is refused `reject-stale`. Web and Android were driven into the failure before the
fix and observed to produce it (signature
`expected 'accept-identical' to be 'reject-stale'`); on iOS the batch race was
**refuted rather than fixed** — the read-to-persist span has no suspension point
and `PushRoutingStore` has no `async`, so `@MainActor` already excluded it — but
the lock was added anyway because that safety was incidental and one added `await`
would have reopened it silently.

**The qualification, stated precisely: closed for the SINGLE-CONTEXT case.** The
lock serializes within one JS realm (web), one app process (natives), and — since
the follow-up finding was fixed — across tabs on web wherever `navigator.locks` is
available. The remaining boundaries are the re-scoped **residual 10**.

The `lakko-security` review of the lock returned **APPROVE WITH NOTES**, no
CRITICAL / HIGH; both of its actionable findings (a per-realm rather than cross-tab
web lock; the lock held across unbounded network I/O) are closed.

What the arc built, all of which now stands:

- **The envelope went v2, a hard flag day** —
  `domain(29, unchanged) ‖ version(1)=0x02 ‖ mint_epoch(8, BE u64) ‖ secret(32)`,
  **70 bytes**. The 29 domain bytes are the `incoming_envelope` dispatch prefix and
  are frozen forever, so the in-payload version byte moved instead. **No
  dual-accept**: a retired v1 envelope is refused with `UnknownVersion`, because a
  v1 envelope carries **no `mint_epoch` and therefore has no position in the
  order** — admitting one would re-open this exact residual for precisely the
  envelopes a stale device sends. A mixed-version fleet fails closed to **defer**
  in both directions, never to the leaky raw `group_id`, so invariant #4 held
  through the flag day.
- **The gate gained the missing predicate.** Envelopes are ordered on the pair
  `(mint_epoch, secret_bytes)` — epoch first, then the 32 secret bytes
  lexicographically, **strictly greater wins, equal is not newer**. The order is
  defined **once**, in `crates/lakko-contacts` (`push_routing_secret_is_newer`),
  and consumed by all three clients over the FFI (`pushRoutingSecretIsNewer`);
  none re-implements it, because three clients each hand-rolling a comparison is
  exactly the drift class that produced this finding.
- **Why this specific scenario now fails, as intended.** `S_old` was minted at
  some epoch `E_old` no later than the pre-removal epoch; the rotation to `S1` is
  broadcast **strictly after** the Remove commit (**C-61**, now test-pinned — see
  residual 6), so it is stamped `E_rot >= E_remove > E_old`. The re-broadcast of
  `S_old` is therefore **strictly less** in the order, and a member holding `S1`
  refuses it as `reject-stale`: no persist, no subscribe, no teardown of
  `gid(S1)`. The load-bearing clause is "a member holding `S1`" — the gate must
  read `S1`, not the pre-batch snapshot it replaced, which is why the serializing
  lock (**C-62 sub-invariant 5**) is part of the closure and not an
  implementation detail alongside it.
- **The stamp is persisted and re-emitted verbatim.** All six re-broadcast sites
  recompose from the *stored* secret, so a site that re-stamped the *current* epoch
  at send time would make the stale envelope arrive **newer** than the live
  rotation and reproduce this residual with the ordering gate fully in place. Only
  a mint reads `Group::epoch()`. Android makes this impossible to break BY ACCIDENT:
  `composeEnvelope` takes a sealed `MintStamp` whose `Fresh` / `Replayed`
  subtypes have `internal` constructors with one producer each in `src/main`.
  The claim is bounded and should not be read as stronger — Kotlin `internal` is
  **module**-scoped and the gate, the repository and the chat client share the
  `:app` module, and `StoredSecret`'s constructor is public, so a wrong stamp is
  still *constructible*. What it removes is the **silent** failure mode: a bare
  `ULong` cannot be passed where a stamp is expected, so the mistake now takes a
  differently-named, reviewer-visible call.
- **A future clamp guards the new capability the ordering created.** Without it an
  authorised writer could stamp `u64::MAX` and **freeze** the group's routing id
  permanently, escalating **residual 3** from admin-can-churn (self-repairing at
  the owner's next rotation) to admin-can-freeze (which does not). The clamp
  bounds an incoming stamp against the receiver's own group epoch and is evaluated
  **before** authority, so not even the owner can freeze the group; it never
  rejects an honest envelope, since `stamp <= seal epoch <= receiver's epoch at
  dispatch`.
- **A related defect was found and fixed during implementation.** `accept-identical`
  short-circuits **both** the authority gate and the clamp, so re-persisting the
  *incoming* epoch on that path let a **plain member** relay the group's current
  secret bytes stamped `u64::MAX`, inflate the stored stamp, and refuse every later
  rotation including the owner's — reaching the very freeze the clamp prevents,
  **around it**. All three clients now preserve the **stored** stamp on that path,
  each with a named regression test that also proves a later owner rotation still
  lands. The general rule: **a rule that short-circuits a gate must not write any
  state that gate protects.**
- **Fix direction 2 turned out to be already structural** on all three clients and
  is **pinned rather than rebuilt**: every store writes the secret, the
  `distributed` flag and the stamp in **one atomic write**, so accepting a
  different secret replaces the bytes and clears the pending marker together. It
  remains **complementary, not superseded** — it removes the trigger, ADR-0044
  removes the effect.
- **Accepted narrowing.** A total order and unconditional owner-priority cannot
  both hold: the owner still wins whenever it mints at a later epoch (the normal
  case), but **at an exact epoch tie the bytes decide and the owner can lose to an
  admin**. The property owner-priority existed to provide — arrival-order
  independence — is preserved by a strictly stronger mechanism, and both tied
  values are authorised post-removal mints equally unknown to the evicted member.
- **Not closed by this:** the order stops a stale value **spreading**; it does not
  repair a device already holding one (see residual 7).

#### Residual 10 — the receive gate's read-modify-write is serialized per PROCESS-CONTEXT; the cross-context boundaries are not all covered (ADR-0044 review; originally HIGH and blocking — **the in-process race is CLOSED, the entry is RE-SCOPED, not deleted**)

**Original finding (CLOSED).** Found by the `lakko-security` review of the landed
ADR-0044 arc — which returned **REQUEST CHANGES** and **reproduced the defect by
execution** on web, driving two envelopes concurrently the way `Chat.tsx` actually
drives them and observing verdicts `['accept-admin-rotate', 'accept-identical']`
with a final stored secret of `0xEE` **at epoch 10**, the stale one. It was
recorded at **HIGH** because its consequence is the same **confidentiality**
property residual 9 is about, not wake delivery.

**The shape it had.** The receive gate is a **read-modify-write**: it reads the
stored `(secret, mint_epoch)` pair, computes a verdict against it, then persists.
There were **suspension points between the read and the write**, and **no client
held a per-`(account, group)` lock across the sequence** — web dispatched it
`void`-prefixed and fire-and-forget from a batch loop, Android launched it per
envelope on `pushScope` (`inflight` guarded `ensure`, `rotatingLocks` guarded
`rotate`; neither covered receive), iOS spawned a detached `Task`. So in the exact
journal order F-A describes — a device holding `S_OLD@10` polling
`[rotation S1@11, stale S_OLD@10]` — gate **A** accepted the rotation and persisted
`(S1, 11)`, while gate **B**, reading the same obsolete snapshot, found its
incoming bytes byte-identical, resolved `accept-identical` (which **by design**
short-circuits both the clamp and the order), and rewrote the superseded secret one
turn behind A. The ADR-0044 § 5 hardening did not save it: B files the *stored*
epoch it read, the stale `10`. **The gate was bypassed, not defeated** — nothing
here ever said the order was wrong.

**How it was closed** (`9a0f539`): one per-`(account, group)` **serializing**
(never coalescing) lock spanning the whole read-decide-write, **shared** across
**four** entry points — receive, rotate, ensure, **redistribute**. Redistribute was
included beyond the minimum because it is the same read-then-write and
unserialized it puts a **PRE-rotation** secret back on the wire mid-rotation.
Sharing **one** lock rather than several also closed **residual 5 / F-E's
rotate-vs-receive interleaving**, since rotate and receive now exclude each other
rather than each merely excluding itself.

Verified by execution on **web and Android**, both of which reproduced the defect
before the fix with the signature
`expected 'accept-identical' to be 'reject-stale'`; on Android the concurrency is
genuinely **parallel** (`pushScope` is `Dispatchers.IO`), so a confined dispatcher
would have masked it, and Android additionally **proved the lock must be SHARED**
by probing with receive on its own lock — F-A and the N-rotation case passed while
**F-E still failed**. On **iOS the batch race was REFUTED, not fixed**: the span
from the `getRecord` read to the persist contains no suspension point (both
`await`s sit after the write) and `PushRoutingStore` has no `async` at all, so
`@MainActor` already made it mutually exclusive. The lock was added regardless,
because that safety was **incidental** and one added `await` would have reopened it
silently. iOS was not defect-free: its rotation *does* park in its broadcast, so
the rotate-vs-receive interleave (**F-E**) was real there.

**What REMAINS, and why this entry is re-scoped rather than deleted.** A lock
serializes within one **process context**. Two boundaries are therefore still in
scope, one handled and one unexamined:

1. **Web, cross-tab — handled, with a degradation mode.** `routingRowQueue` is
   module-level state, so it orders callers within one JS realm, while
   `lakko-push-routing` is an IndexedDB database shared by **every tab on the
   origin**. That was raised as a **MEDIUM** by the review of the lock and is
   fixed: a **Web Lock** (`navigator.locks`) of the same per-row key nests inside
   the in-process queue. **But `navigator.locks` is secure-context only**, so a
   browser without it — an insecure-context origin, an old browser, an embedded
   webview — **degrades to same-tab-only serialization, silently**. That is the
   live remainder on web. (The in-process queue is deliberately **kept** rather
   than replaced precisely because this state is real: with one mechanism its
   absence would be an invisible unserialized run. The queue also captures
   ordering synchronously, so a burst in one turn runs in DS journal order.)
2. **Natives, cross-process — NOT EXAMINED by this arc.** The iOS NSE and the
   Android background decrypt (ADR-0041 / C-59) run in a different process from
   the foreground app and touch the same on-device storage. Whether either ever
   reaches the routing row's **write** path — as opposed to the read-only
   `push_gid -> (account, group_id)` reverse-map lookup the background decrypt is
   documented to do — was **not established either way** here. Recorded as an open
   question, not as a cleared one.

Two smaller lock-scope questions **were** examined and deliberately left alone, on
web: `clearAllGroupRouting()` (`AuthContext.tsx`, on `forgetDevice` / logout)
writes the store **outside** the lock, judged safe because it is a **whole-store
teardown** rather than a per-row read-modify-write — it cannot lose an update or
decide against a stale snapshot, and the state it leaves is the
no-secret / no-record case every entry point already handles defensively; and the
two service workers touch the same database but only the `routes` store and only
in **`readonly`** transactions, so they are not a party to the sequence the lock
serializes.

#### Two loose ends found during the arc

- **~~Nothing reads the web `?wake=` deep-link parameter.~~ CLOSED.** The service
  worker built `/?wake=<hex>` on notification tap and its comment claimed the SPA
  routed "via its own deep-link reader (set in 1.18g)" — but **no reader existed**,
  so a web notification tap had **never** opened the target chat; it landed on the
  app shell. (The routing id itself was fine: ADR-0042 threads the reverse-resolved
  real `group_id` hex into `tapGroupHex`.) Now closed by
  `apps/web/src/push/{wake-target.ts,WakeNavigator.tsx}`: the cold-open `?wake=`
  param and a new `LAKKO_NOTIFICATION_TAP` message (the already-open case had no
  usable click-time signal at all — the SW only called `focus()`) feed one reader
  that navigates once the session **and** identity secrets are restored, holding
  the target across the login bounce. It only ever navigates to a group already in
  the in-memory registry — an attacker-supplied `?wake=` can never make it fetch,
  subscribe, or create anything — and a malformed / unknown / still-opaque
  `push_gid` value is dropped silently. The stale SW comment is corrected. The
  Android tap deep-link already worked (`7da84ee`).
- **~~`unsubscribeGroup` is dead code on all three clients.~~ CLOSED** (2026-08-11,
  ADR-0043) — the receive-side teardown is its first real caller on every client;
  see Residual 2.

**Status: OPEN (accepted residuals).** **Residual 1 is the substantive privacy
limit and survives ADR-0043 intact** — it is inherent to *any* group-shared routing
id, not merely a stable one: rotation renames the cluster, the provider re-links
across the boundary by token-set intersection, and the set difference names the
evicted device. Closing it needs per-device-blinded ids. **Residual 2 is CLOSED**
(rotation on removal + live per-group unsubscribe). **Residual 3 is WIDENED** from
admin-seed to admin-churn by the ratified option (b); the `lakko-security` pass
ADR-0043 declares mandatory for (b) **was run and returned APPROVE-WITH-NOTES, no
CRITICAL/HIGH**, its one blocking MEDIUM (F1, the coalescing rotation lock) fixed.
**Residual 4 is unchanged** and gains a second clause (such groups now also never
rotate). **Residuals 5-9 were added by ADR-0043. ADR-0044 closed residual 6 outright, and —
once the serializing lock its own review demanded had landed (`9a0f539`) — closed
residual 9 and the concurrent-rotator core of residual 7 for the
single-context case, plus the rotate-vs-receive half of residual 5. Residual 10,
minted by that review, is re-scoped rather than closed.**

**Residual 6 is CLOSED** ([ADR-0044](../adr/0044-push-routing-secret-recency.md),
`936d849` / `014e84e`) — C-61 now has a regression test on all three clients, each
asserting on the emitted envelope's stamp and each carrying a permanent
inverted-order companion, with the discrimination **proven by running the swap** on
both natives; ADR-0044 also **sharpened** what a C-61 regression costs, from the
rotation's forward secrecy alone to that **plus an inverted recency comparison**
that turns "the rotation always wins" into a silent coin flip.

**Residual 9 is CLOSED for the single-context case.** ADR-0044 built the general
fix its own entry prescribed — a v2 envelope carrying a `mint_epoch`, a canonical
`(mint_epoch, secret_bytes)` total order defined once in `crates/lakko-contacts`
and consumed over both FFI surfaces, a persist-and-replay-verbatim rule, and a
future clamp evaluated ahead of authority — and the `lakko-security` review of the
landed arc nonetheless **reproduced the defect by execution**, returning **REQUEST
CHANGES**: the primitive was correct, but the receive path that consumed it was an
**unserialized read-modify-write dispatched fire-and-forget per envelope**, so the
gate decided against a **stale snapshot** and was **bypassed, not defeated**
(**residual 10**). The serializing lock in `9a0f539` closed that, and the review of
the lock returned **APPROVE WITH NOTES**, no CRITICAL / HIGH, with both actionable
findings (the per-realm web lock; the lock held across unbounded network I/O) since
closed. **Residual 7's concurrent-rotator core closes on the same fix and with the
same qualification.** Its **F-D stale-role window** is untouched regardless (an
authority refusal, on which recency has no bearing), an ordering gate **does not
repair an already-stale holder**, and the N+1 rotations and their churn still
happen.

Still open, then: **residual 5** (the orphaned old gid — its **rotate-vs-receive
half is CLOSED** by the shared lock; what remains is the transient store-read
failure and the crash case, accepted and inert), **residual 7's remainder** (the
F-D stale-role window, the no-repair limit, and the N+1 churn itself), **residual
8** (the web teardown-before-subscribe window, F4), and the **re-scoped residual
10** (no longer HIGH or blocking: the in-process race is closed and web's cross-tab
boundary is handled where `navigator.locks` exists; what remains is the
insecure-context degradation to same-tab-only serialization, and the
native cross-process boundary this arc did **not** examine). The `lakko-security`
pass on option (b) **is done — APPROVE-WITH-NOTES, no CRITICAL/HIGH, no invariant
#2/#3/#4 break on the live path** — with its one blocking MEDIUM (F1, the
coalescing rotation lock) fixed on web + iOS by adopting Android's serializing
lock.
Invariants: #2 holds
(push-gw is byte-identical unchanged across both ADRs and handles the same opaque
`BINARY(32)`; the secret rides an E2EE MLS application message), #4 holds (defer
rather than downgrade — a member without the current secret does not subscribe at
all; the receive gate still rejects on an unreadable role table or an unresolvable
sender), #5 is **improved but not fully satisfied** — the server no longer
materialises a *DS-linkable* social graph, and the cluster is no longer
*forward*-linkable across a removal by the evicted party, but an opaque
co-membership cluster is still materialised and still re-linkable by intersection.

## Dependency advisories

The `cargo audit` baseline (RustSec advisory DB) surfaces
two open vulnerabilities + three unmaintained-crate
warnings in the workspace's transitive dependency tree.
None are in code we control; remediation depends on
upstream releases. Each entry below lists severity,
reachability analysis, and the upstream tracker the fix
is gated on.

<a id="g-41"></a>
### G-41: three `libcrux` advisories under the MLS crypto provider

**Status:** **OPEN as a `cargo deny` failure; NOT reachable in Lakko's build (analysed 2026-09-22)**

`just audit` reports three vulnerabilities, all in one dependency
chain and none in code Lakko controls:

| Advisory | Crate | Fixed in |
|---|---|---|
| RUSTSEC-2026-0212 | `libcrux-secrets 0.0.5` | `>= 0.0.6` |
| RUSTSEC-2026-0207 | `libcrux-sha3 0.0.8` | `>= 0.0.10` |
| RUSTSEC-2026-0208 | `libcrux-sha3 0.0.8` | `>= 0.0.10` |

The chain is `libcrux-secrets <- libcrux-traits 0.0.6 <- libcrux-sha3
0.0.8 <- hpke-rs 0.6.1 <- openmls_rust_crypto 0.5.1 <- lakko-mls`, so
this sits under the MLS HPKE backend every client ships, and
RUSTSEC-2026-0212 is a constant-time failure on **aarch64** - every
iPhone and most Android devices. That is why it was worth reading
rather than waving through.

**Reachability: no, and by three independent arguments.** Any one of
them is sufficient; all three were checked against the resolved graph
for `aarch64-apple-ios` and `aarch64-linux-android`.

1. **The broken primitive has no callers.** RUSTSEC-2026-0212 is in
   `Select::select` / `Swap::cswap`, whose aarch64 bodies do
   `cmp {cond:w}, 0` on an 8-bit selector widened to a 32-bit
   register - the unspecified high 24 bits can make a zero selector
   compare non-zero, so `csel` picks the wrong operand
   (`libcrux-secrets-0.0.5/src/int.rs`, `mod aarch64`). Those two
   traits are **defined in `libcrux-secrets` and named nowhere else**:
   no crate in the dependency set calls `.cswap(`, and every crate
   that imports from `libcrux_secrets` takes only the type-level
   wrappers (`Classify`, `ClassifyRef`, `Declassify`, `DeclassifyRef`,
   `DeclassifyRefMut`, `U8`). The only glob importers are
   `libcrux-ml-kem`, which is not in the graph.

2. **`libcrux-sha3` is only called from the post-quantum arms.**
   `libcrux_sha3::shake256` has exactly two call sites in `hpke-rs`
   (`src/kem.rs`, `derive_key_pair`), both inside the
   `XWingDraft06` / `MlKem768` / `MlKem1024` match arms. Lakko's four
   ciphersuites
   ([`crates/lakko-mls/src/ciphersuite.rs`](../../crates/lakko-mls/src/ciphersuite.rs))
   are DH-KEM over X25519 / P-256 / P-384 and take the preceding arm.
   No Lakko source names SHA3, SHAKE or a PQ KEM anywhere.

3. **The libcrux HPKE backend is not compiled at all.** `hpke-rs`
   gates both backends behind optional features (`libcrux` /
   `rustcrypto`), and `cargo tree -i` resolves `hpke-rs-rust-crypto`
   while `hpke-rs-libcrux`, `libcrux-aead`, `libcrux-kem`,
   `libcrux-hkdf`, `libcrux-ecdh` and `libcrux-curve25519` are all
   absent from the graph on every target checked. `libcrux-sha3` is a
   NON-optional dependency of `hpke-rs`, which is the only reason any
   libcrux crate is present.

So the vulnerable code is compiled on aarch64 and never executed.
**This is an argument from the current resolved graph, not a
guarantee**: adopting a PQ or hybrid ciphersuite would make argument 2
false, and a feature-unification change upstream could make argument 3
false, so this reasoning must be re-run whenever the MLS crypto
provider or the ciphersuite set moves.

**Why it nevertheless stays OPEN.** `cargo deny check advisories`
fails on it, so `just ci` is red. The clean-audit posture the G-25
entry argues for is a real property worth keeping: a red gate that
everyone knows to ignore stops being a gate. The fix is not
available, though - see below - so the choice is between a red
`just audit` and an `[advisories] ignore` entry pinned to these three
IDs with a pointer here.

**Why `cargo update` cannot fix it.** `libcrux-traits 0.0.8` already
pins the FIXED `libcrux-secrets = "=0.0.6"`, so the vendored-manifest
idiom that closed G-23 looks like it should reach it - and does not,
because of an exact-version knot one level down: `libcrux-secrets
0.0.6` requires `hax-lib =0.3.7` while `libcrux-sha3 0.0.8` requires
`hax-lib =0.3.6`, and both pins are exact. Bumping the traits alone
fails to resolve (verified).

**The route, unbuilt.** Both lines have to move together:
`libcrux-traits 0.0.7 -> 0.0.8` in the existing vendored
[`crates/vendor/hpke-rs-libcrux`](../../crates/vendor/hpke-rs-libcrux/Cargo.toml),
plus a SECOND vendored crate - `hpke-rs 0.6.1` with its
`libcrux-sha3 ^0.0.8` requirement relaxed to `0.0.10` - since
`hpke-rs`, not the libcrux backend, pins the sha3 line. That is a
fourth `[patch.crates-io]` entry on the MLS crypto graph, for code
argument 1 says is never called. The alternative is upstream:
`openmls_rust_crypto 0.6.0` and `hpke-rs 0.7.0` both exist, and moving
to them is an openmls bump rather than a manifest delta.

**Side finding, unresolved.** The G-23 vendored `hpke-rs-libcrux` fork
patches a crate that is **not in the resolved graph on any target
checked here** (argument 3), so that fix may be a no-op in the current
feature set. It may have been reachable when it was made, and this was
not investigated - recorded so the next audit pass does not assume the
patch is load-bearing.

**Two advisories from the same run WERE closed** by plain lock bumps,
with `just lint` and the 2779-test workspace suite green afterwards:
`h2 0.4.13 -> 0.4.19` (RUSTSEC-2026-0258, unbounded empty DATA frames)
and `rustls 0.23.40 -> 0.23.45` with `rustls-webpki 0.103.13 ->
0.103.15` (RUSTSEC-2026-0285, TLS 1.3 handshake messages accepted
across encryption-level boundaries - which matters here because the
whole anti-censorship stack of
[ADR-0026](../adr/0026-anti-censorship-wiring.md) is built on rustls).

## Client concurrency

Chat-client locking + ordering gaps. Both entries below concern the CLIENT side of
the MLS commit / poll paths, not a server or a dependency.

**Filing note.** Everything from G-27 to G-36 still sits under the preceding
`## Dependency advisories` heading, which describes none of them — a structural
drift from a run of arcs that appended entries without opening a section. Only
this section was opened; re-filing the rest is a standing cleanup, deliberately
not folded into this pass because it would move a lot of unrelated prose. The
reviewer-facing categorisation in
[`invariant-index.md`](./invariant-index.md) is correct for all of them.

<a id="g-37"></a>
### G-37: Android chat repository held one repository-wide lock across the DS long poll

**Status:** A deliberate 30-second long poll. **CLOSED on Android (2026-08); residuals recorded below**

**The original framing of this entry was wrong, and the correction is the point.**
It was filed (2026-08-14, out of the ADR-0044 lock-bounding work) as a *resilience*
hazard: "a hung DS request wedges the shared lock indefinitely". The code was worse
than that — it was the **steady state**, no hang required. `ChatRepository.groupsLock`
was one `Mutex` per **repository**, not per group (its own doc comment said so and
even named the fix), and `pollGroup` awaited a **deliberate 30-second DS long poll
while holding it**, driven by an unbounded `while (isActive)` loop on the open chat
screen. So while any chat screen was open, that single global lock was held
near-continuously and every send, edit, reaction and membership commit **on every
group** queued behind it. The chat-list screen then polled groups serially, making
the worst case `waitSecs * N`.

**The fix is in two parts, and neither is "add a timeout".**

**1. The poll is restructured, not bounded.** A timeout here would sever a long poll
that is behaving exactly as designed. `pollMessages` was split into a network half
(`ChatClient.fetchGroupJournal`) and an MLS half (`ChatClient.applyFetchedMessages`),
the latter deliberately **not `suspend`** — a structural guarantee that a network
wait cannot be reintroduced into the critical section, because a suspending call
would not compile there. The fetch runs outside the locks; the apply and the cursor
advance run inside, atomic with respect to each other. The cursor is read before the
fetch and re-read under the lock: if it moved, the batch is **dropped unapplied**
(`SplitPollOutcome.SKIPPED_CURSOR_MOVED`), because applying it would re-feed already
consumed rows to OpenMLS and — worse — let the stale batch's end cursor move the
record's cursor **backward**. A per-group in-flight latch stops two polls of one
group overlapping, released in a `finally` so a failed or cancelled fetch never
wedges polling. See `apps/android/app/src/main/kotlin/me/lakko/app/chat/SplitPoll.kt`
and its host tests.

**2. DS I/O is bounded at the TRANSPORT, not with `withTimeout` — and that choice is
load-bearing, not tidiness.** A `withTimeout` wrapped around a commit site would put
a `TimeoutCancellationException` **inside** `runCrashSafeCommit`, where the
persist-catch would swallow it and fire `resetGroupToDurableSnapshotOrThrow` —
silently converting a network timeout into a commit rollback, i.e. manufacturing the
device-behind outcome [ADR-0040](../adr/0040-durable-pending-commit-outbox.md) exists
to prevent. Bounding `DsClient` instead means no such exception is ever constructed:
Ktor's `HttpTimeout` throws `HttpRequestTimeoutException`, which extends
`java.io.IOException`, so it lands in the existing `DsNetworkError` path and reaches
callers as `ChatError`. Nothing configured a timeout before, so the effective budget
was inherited and unknown; it is now explicit (15 s request + socket), and a
long-poll fetch widens **both** request and socket to `waitSecs + 10` — the socket
deadline has to move too, since a long poll is an idle socket by definition.

**Then the lock itself was narrowed to two tiers — and THIS correction is the part
worth reading.** The obvious fix is the one the old doc comment suggested: swap the
single mutex for a `ConcurrentHashMap<String, Mutex>` keyed per group. **A straight
per-group swap would have broken ADR-0040.** One OpenMLS provider serves *every*
group, and the FFI offers no per-group persistence:

- `Identity.snapshotStorage()` serialises the **whole** provider under a single read
  lock, while one logical MLS operation performs several separate writes into it
  (`MemoryStorage` takes its lock per write). A snapshot taken by group A part-way
  through group B's multi-write operation captures **B torn**, and that torn blob is
  what a later restore or restart loads.
- `Identity.restoreStorage()` — the ADR-0040 F1 abort path,
  `resetGroupToDurableSnapshotOrThrow` — replaces **every** group's state at once.
  Running it while another group sits at an un-persisted N+1 would **roll that group
  back**, re-opening the exact device-behind window ADR-0040 closes.

Both were impossible before only because ONE mutex covered every group — an
accidental safety the narrowing would have removed. So the shape is **two tiers**: a
per-group `Mutex` (outer, spanning the whole operation including the publish, so
same-group ordering is byte-for-byte what it was) plus a **global `providerLock`**
(inner, held only over provider-touching regions). Ordering is always per-group
first, provider second, never the reverse, and never two per-group locks at once.

**Where the concurrency actually comes from is the lifting, not the narrowing.** Each
path now splits into a provider half and a network half — `ChatClient.prepareSend` /
`publishPreparedSend`, `prepareCrashSafeCommit` / publish, `fetchGroupJournal` /
`applyFetchedMessages` — so what different groups serialise on is local CPU plus one
snapshot write, never a 15 s DS publish or a 30 s long poll. The ADR-0040 §2 ordering
survives the split **structurally**: `prepareCrashSafeCommit` returning normally IS
the signal that both durable writes landed, and the publish is a separate call that
cannot run until it has returned, so a throw still means the publish is provably
un-run.

**`pollAllGroups` is now concurrent, bounded at 3.** The bound is not about CPU. Every
DS long poll goes to the same host and Ktor's OkHttp engine dispatches through
`Call.enqueue`, so OkHttp's `Dispatcher.maxRequestsPerHost` applies — default 5.
Filling all five slots with idle long polls would queue the user's next send behind
them in `readyAsyncCalls`, trading lock contention for connection starvation, which
no amount of lock narrowing could then fix. Three leaves at least two slots free (the
open chat screen's own continuous long poll, and one interactive publish) while
collapsing chat-list latency from `waitSecs * N` to `waitSecs * ceil(N / 3)`. The
fan-out is a `supervisorScope` with a per-leg `try/catch` — a plain `coroutineScope`
would propagate the first child failure and cancel its siblings, which is strictly
worse than the serial loop it replaces; `CancellationException` is re-thrown so a
cancelled poll stays cancelled.

**Residual, stated honestly.** Cross-group operations are **not** fully independent.
They still serialise on the provider region — local CPU plus one snapshot write — and
that is a direct consequence of the FFI's whole-provider persistence granularity, not
of the locking. What is now **outside** it is everything slow: the 15 s publish and
the 30 s long poll. Making groups genuinely independent would need per-group
persistence in `lakko-mls` / the FFI, which is an MLS-core change (invariant #3) and
is not attempted here.

**Two side-effects of removing the accidental global lock, recorded because they were
easy to miss.** `groups` is a `Collections.synchronizedMap`, whose *iterators* are not
self-synchronising; the repository-wide mutex made unsynchronised iteration
accidentally safe, so `persistMessageLogBestEffort` (and the group-id snapshot in
`pollAllGroups`) now take the map's own monitor explicitly. And the per-group mutex
map is cleared in `wipeInMemoryState` alongside the records it guards, so an account
switch does not carry a previous session's locks.

**The question this entry left open for the other two clients has been answered, and
the answer lives in G-38 below.** The original text said the same question had *not*
been asked of the iOS and web chat
repositories. It has now: they had the **opposite** exposure — no serialization at
all — and the residual "lock held across an unbounded network call" survives there in
a much smaller, per-group form, recorded under G-38 rather than duplicated here.

**Severity, unchanged in kind:** availability. No confidentiality or integrity
property was ever weakened by the lock being held — nothing about who may write, or
which write survives, changed. What was lost was liveness on a shared lock, and on
one path (ADR-0044's routing row, since insulated by `boundedOrNull`) liveness was
what a confidentiality guarantee rested on.

<a id="g-39"></a>
### G-39: The external cryptographic review of the MLS state machine has not happened

**Status:** **OPEN, and it is a release gate**

**Owner:** [ADR-0046](../adr/0046-external-crypto-review.md), which moved this obligation
here from where it had been scattered — two ADRs stating it as a blocker
([ADR-0040](../adr/0040-durable-pending-commit-outbox.md),
[ADR-0045](../adr/0045-remote-commit-epoch-fork.md)), four `C-` rows in
[`invariant-index.md`](./invariant-index.md) repeating it, and four further ADRs
mentioning it without acting on it. It is stated **once**, in this entry, and nowhere
else.

**What it is.** Security invariant #3's third clause: *"(after Phase 2) an external
crypto audit"* for changes to `lakko-mls`. The per-change half of that invariant — an ADR
plus property tests, before the change lands — is unchanged and is still enforced per
change. This is the other half.

**Why it is a gap rather than a blocker on any one document.** Carried inside each
MLS-touching ADR, it made every one of them permanently un-Acceptable regardless of how
finished it was, while specifying nothing an auditor could act on. A blocker that blocks
unconditionally and forever stops being read. ADR-0046 has the full argument.

**The gate, stated so it cannot be quietly routed around: no build reaches users while
this is open** — no store submission, no GA tag. That is where "after Phase 2" bites.

**Scoped for an engagement in
[`external-review-scope.md`](./external-review-scope.md)** (added 2026-09-02) — the
boundaries, the measured sizes, a reading order through ten thousand lines of security
documentation, what has already been verified and where that verification is thin, the
six questions ranked by where we think the risk is, and the five things a reviewer must
not assume. This entry states the obligation; that document states the engagement.

**The surface to be reviewed.** Larger than any one ADR, because the interactions are the
point — a per-change review would see each of these as a slice:

- **ADR-0045's core**, six sprints: the per-proposal authorisation gate (`authorise`, one
  rule at three layers, by-reference judged against the proposer and by-value against the
  committer); `owner_survives` (C-63); the staged sweep's stage / merge / abandon
  lifecycle and its per-group rollback; the `create_application_message` pending-set park;
  the committer election over the `0xFF02` role table; and S7's approval-as-proposal.
- **The invariants that arc minted or amended:** C-63 through C-69 — in particular C-66
  (the reserve past the pending cap, and the sweep's fifteen enumerated build conditions
  plus the exclusion search that backs them), C-67 (the cursor-may-lag-never-lead ordering
  rule, and the owed-artifact lifecycle) and C-68 (author identity as a signature key
  resolved at receipt).
- **ADR-0022's role table** and its `0xFF02` GroupContext extension, which everything
  above is layered on.
- **Three independent client implementations** of the ordering and rollback disciplines
  the core change depends on — the send lock, the verdict apply, the sweep adjudication,
  the durable-cursor ordering. These are not incidental: C-69 exists because a discipline
  can be correct in the core and attached to the wrong end in a client.
- **The `lakko-security` findings and their fixes** (2026-08-29, seven findings), which
  are the most recently changed part of the surface and therefore the least settled.

**What an internal review already found, as an argument for the external one rather than
against it.** The `lakko-security` pass on an already-shipped surface returned three HIGH
findings plus a confirmed impersonation. **None needed an attacker and four needed nobody
at all** — a network blip, a stale roster, two admins doing an ordinary thing, or enough
time passing. An external reviewer does not share the blind spots that let those ship.

**Residual risk while this is open.** Everything above is peer-verified and
property-tested but unaudited by anyone outside the project. The specific classes an
external review is best placed to catch, and which the work so far has *not* systematically
covered: cross-invariant interactions (each was reasoned about locally); the enumeration
in C-66 being a mirror of openmls rules Lakko does not own; and whether the three-layer
gate's by-reference/by-value split holds under compositions nobody has thought to write a
test for.

## Delivery service

<a id="g-42"></a>
### G-42: A journal's name is its only key

**Status:** A leaked `group_id` reads, writes and knocks; a mailbox is named by a public key anyone can compute. **CLOSING under [ADR-0053](../adr/0053-ds-journal-authorisation.md): S1-S4 built and deployed to the test stack 2026-09-27; a claimed journal refuses a request by name alone (walked live: unsigned read, unsigned write and a forged proof 401, a competing registration 409). Residuals: rendezvous mailboxes stay open by name by design, an evicted member keeps READ access to ciphertext until its epoch ages out of the retention window, and the S5 `lakko-security` pass is open**

**Symptom.** `services/ds` serves and accepts both journals to anyone who names them
(`services/ds/src/main.rs`: *"No authentication on Phase 1.3 - anyone with the right
pseudonym can publish or fetch"*). Three roads hand the name to people outside the
conversation: (1) a forward carries the origin conversation's `group_id` inside its
sealed payload ([ADR-0052](../adr/0052-reply-forward-pin.md) decision 2 - reproduced
live 2026-09-27: two recipients who share no conversation with the original author hold
the author-forwarder `group_id`); (2) a mailbox pseudonym is
`BLAKE3("lakko-pseudonym/v1/identity" || identity_pk)` and the identity key is
published by discovery, so a mailbox is derivable from a username; (3) the `/g/` joiner
reads the whole group journal and posts into it before it is a member, by design.

**What the name buys.** Not the content - every row is MLS- or HPKE-sealed. It buys the
metadata (row count, timing, sizes: the activity of a conversation, or whether and when a
person received a contact request or was added to a group), a write into the journal
(bytes every member fetches, fails to open and drops, at their cost), and for a group an
external-join proposal nobody invited ([ADR-0028](../adr/0028-group-join-ticket.md)
"ask, never in"). The pseudonym cannot be guessed (256 random bits), so every exposure is
one leaked name, one conversation - but ADR-0052 leaks one on purpose, and the mailbox
name was never secret.

**Why it went unrecorded.** The Phase 1.3 posture is stated in the service header and
relied on by [ADR-0045](../adr/0045-remote-commit-epoch-fork.md) (the case against a
DS-side epoch gate) and ADR-0028 (the joiner's unauthenticated read), but no gap named
what the posture costs once a name travels; ADR-0052 wrote *"an opaque conversation id
they cannot open; that is the whole disclosure"*, which is true of content and false of
the journal.

**Closing.** ADR-0053: a claimed journal is opened only by a request signed with a key
it was claimed under - for a group a per-epoch Ed25519 key every member derives from
`Group::export_secret("lakko/ds/access/v1", ...)` (rotation with the epoch, eviction
revokes at the commit), for a mailbox the identity key its pseudonym hashes; a failed
proof is answered as a nonexistent journal; the non-member's one act, asking to join,
moves to a PoW-gated knock whose response carries the epoch; the joiner's Welcome moves
to its mailbox (C-69 amended). Rendezvous mailboxes (pairing, recovery, courier, state
bundle: random names exchanged out of band, alive for minutes) stay capability-by-name
and are this gap's accepted residual. Until it lands: a leaked `group_id` is a read of
metadata and a write of garbage, and a forward from a one-to-one should carry no origin
(ADR-0052 amendment).


<a id="g-43"></a>
### G-43: `services/channels` authorised no publisher operation

**Status:** Anyone who knew a `channel_id` could rotate, deliver envelopes and read the subscriber list. **CLOSED in code 2026-10-01 ([ADR-0059](../adr/0059-channel-discover.md) section 9); not walked live** (recorded 2026-09-28 while designing ADR-0059)

**What the code does.** `create_channel` takes only `{channel_id}` and registers no key;
publishing a post, `POST /rotate`, `/rotate/stage`, `/rotate/commit`, delivering a seed
envelope, registering a subscriber and `GET /v1/channels/{id}/subscribers` take no
session, no signature and no owner key (`services/channels/src/api.rs`, the router;
`migrations/0001_init.sql`: `channels` is `id, current_epoch, next_cursor, created_at`).
ADR-0010's planned publisher-only distribute route was never built. Checked in the
tree on 2026-09-28; not measured against a deployment.

**What it buys an attacker.** Not post content: every post is AEAD-sealed under the
epoch seed and Ed25519-signed, and readers drop a post whose signature fails. It buys
availability and metadata: a rotation or a staged commit nobody authorised advances the
channel's epoch or files envelopes under it, which can leave subscribers without a seed
they can open; the full subscriber key list and its size are public; and, because
subscriber registration is free, the subscriber count is forgeable. A `channel_id` is
public to anyone who resolves the channel's handle.

**Closing.** ADR-0059 introduces a per-channel owner key registered at creation and uses
it for the directory, the subscriber list and a token-gated subscribe. Gating publish,
rotate and deliver on the same key (plus co-publisher keys the owner registers) is the
remaining change and is not part of ADR-0059.

**Closed (2026-10-01, ADR-0059 section 9).** `POST .../posts`, `POST .../rotate`,
`POST .../rotate/stage`, `POST .../rotate/commit` and `POST .../subscribers/<key>/envelope`
now go through `require_publisher` (`services/channels/src/owner_auth.rs`) before the body
is parsed: an owner proof, or a proof by a co-publisher key the owner appointed, over the
method, the service path, the query and the exact body bytes, inside the 120 s window,
nonce consumed once; every refusal is `401 owner-proof-required`. An unknown channel now
answers `401` instead of `404`. Web, iOS and Android sign all five writes and the subscriber
list with the role the device holds for the channel (an in-memory cache: owner on create,
co-publisher on a completed join, otherwise the first key the server accepts). Verified:
`services/channels` integration tests (42, including a refusal matrix per route: no proof,
foreign key, unappointed co-publisher, body mismatch, replay) and unit tests (72); the 6
SQL-store tests skipped without `LAKKO_CHANNELS_TEST_DATABASE_URL`; web vitest for channels and
discover (170); Android and iOS unit tests in scratch copies (this worktree does not compile
natively until the sealed-invite-link bindings are ported). Not exercised against a deployed
server. What remains open is the reads: posts, envelopes and the epoch stay readable by
`channel_id`, which is acceptable because posts and envelopes are sealed.


<a id="g-44"></a>
### G-44: Rejoining a call in the same MLS epoch reused SFrame `(key, nonce)` pairs

**Status:** **CLOSED in the core, web, iOS and Android libraries by [ADR-0069](../adr/0069-sframe-random-counter-start.md) (2026-10-04); not seen on a live call**

Found by a `lakko-security` pass on 2026-10-04. A sender's nonce is `nonce_salt XOR counter`,
key and salt are a function of `(seed, KID)`, and a fresh `ParticipantKey` started its
counter at zero. The seed of a chat-group call is `export_secret("lakko/call/v1", call_id)`
of the chat group, which changes only with the MLS epoch. Hanging up and joining again,
reloading the tab or reconnecting in the same epoch therefore sealed new frames under
nonces already used, on every stream, on all three clients; a camera track that ended and
was published again did the same for the video stream on web and Android. The SFU holds
both ciphertexts: keystream reuse and a forgeable tag, a break of invariant #2 for those
frames. The earlier "V1 false alarm" note covered link calls only.

**Closed.** The counter is 64 bits and starts at a random value in `[0, 2^62)`; receivers
accept any 64-bit counter. Two keys under one `(seed, KID)` collide with probability at most
`(L1 + L2) / 2^62`. A flag day for call media. Touchpoints: `crates/lakko-calls/src/sframe.rs`
(`random_counter_start`, `compose_nonce`), the C ABI, both FFI crates, the C++ bridge, the
web worker. Property tests in `sframe.rs` (injective nonce, two keys of one seed never share
a counter, a frame opens under an independently derived key, exhaustion, the golden vector
still opens). Joins the surface of G-39.


## Account key reset

<a id="g-45"></a>
### G-45: After a key reset the old identity's leaves stay in every conversation group until peers evict them, and the old devices keep decrypting group traffic - **NARROWED 2026-10-04 (ADR-0073 phase 2 built in code on all three clients, not walked live); OPEN for a group the old identity owned, a group with no role table, and a group where no admin is the person's contact** (minted 2026-10-03)

**What the code does.** [ADR-0073](../adr/0073-account-key-reset.md) phase 1
(built 2026-10-03, unit-tested, not walked live) makes a reset true of the
server side and of the account's own devices: every other device's session
is superseded by the key generation, the directory refuses the old key, the
DeviceGroup journal is re-rooted under the new seed and the old devices' epoch
keys are gone with the chain, and a superseded device wipes itself. Nothing in
phase 1 touches a **conversation** group. The DS, push-gw and channels never
see a session token (ADR-0053 proofs, anonymous tokens, owner signatures), so
an old device that still holds a leaf in a group reads and decrypts that
group's journal exactly as before, for as long as it remains an MLS member -
which, since the device wipes itself on the next refresh or landing, is in
the honest case minutes, and in the case this gap is about (a stolen or
compromised device that does not run the wipe, or a device kept offline) is
indefinite. The new identity is in no chat at all: every leaf of the account
is bound to the old account key (ADR-0050 device bindings), so a peer resolves
the old leaves to a key the directory no longer publishes.

**What it buys an attacker.** The one case where the maintainer's "every
other device is signed out and removed" is not yet true: a device the owner
lost, that an attacker keeps off the network until it is convenient, keeps
reading the groups it was in. Forward secrecy at the next commit of each group
is unaffected (RFC 9420: a member that holds the current epoch secrets reads
the current epoch; it does not learn what it was not sent), so the exposure is
"continues to read" rather than "reads the past".

**Closing: ADR-0073 phase 2 (design only).** A peer that observes a reset
(the pinned key differs from the published one, no supersession proof, the
KT-log entry for the new key carries `is_reset`) marks the contact
`reset-unverified`; in every group where the observer's account is owner or
admin it proposes (ADR-0045 path) the removal of every leaf whose binding
resolves to the previous key; a one-to-one with the old identity ends
(ADR-0062's `one_to_one_holds` refuses the removal); the new identity is
re-admitted only after an out-of-band safety-number re-verification; a group
the old identity **owned** cannot be committed out of (C-63 `owner_survives`,
the ownership transfer needs the gone key), which is ADR-0022's lost-owner
case made concrete and the one `lakko-mls` change phase 2 needs (an
admin-takes-the-owner-slot exception carrying verified reset evidence), hence
G-39. Open question 6 of the ADR (recreate the group versus the owner-slot
exception, or both) is the maintainer's.

**Built 2026-10-04 (phase 2, UX `identity-recovery.md` section 16), on web,
iOS and Android, unit-tested only (web vitest 3996, iOS `LakkoTests` 2637,
Android 3042 on the host JVM), nothing walked on a device or against a
server.** The maintainer decided open question 6 as **Recreate group only**,
so the owner-slot exception is not built and `lakko-mls` is untouched (G-39's
surface does not grow). What is in code: the trigger exactly as designed
(`proof-absent` and the KT log's `is_reset`; the fail-closed gate still
throws, the pin is never advanced by the client), the flagged contact with
`previousKey` / `pendingKey` found by account id across the key change, the
silent removal proposal of every leaf of the previous key in every role-table
group where the observer is owner or admin (at notice time and on each chat
open while the `applied` test says no), the suppression of the ADR-0050
re-admission ADD branch for a flagged contact (without it every member would
have proposed adding the NEW identity's devices to every shared group within
ten minutes, ahead of any verification), the closed one-to-one (nothing sent,
late rows dropped before storage), the out-of-band re-verification over the
new key, and for an owned group the owned-group notice plus Recreate group
with a `lakko-contacts::group_closed` envelope (the one new wire artefact,
fuzzed, admitted only from an owner or admin at receipt) closing the old
group.

**What stays open, stated per case.** (1) A group the old identity **owned**:
the old leaves are never removed; the members are told and an admin recreates
the group; the old group keeps working with the old devices in it until the
members leave it. (2) A group with **no role table**: no proposal path, the
contact is flagged, nothing else happens. (3) A group where **no owner or
admin is the person's contact**: nobody's client runs the trigger with a
stored pin for that account, so nobody proposes; a plain member who is their
contact sees the flag and can only ask an admin out of band. (4) The
detection is pull: a member learns of the reset at its next identity fetch
or at the re-admission arm's next run while a shared chat is open (about ten
minutes), not at the reset. (5) Verification is per device and does not
travel over the DeviceGroup. The exposure in every open case is unchanged
from the paragraph above: "continues to read", never "reads the past".

<a id="g-46"></a>
### G-46: A stolen session can reset the account keys - strip every passkey, sign out and wipe every other device, re-root the DeviceGroup - bounded only by the per-account limits; and two discovery writes stay open to a superseded token within the access TTL - **FIRST PART CLOSED for the session-only attacker (2026-10-04 reset, 2026-10-06 delete / add device / add passkey / change password under one fresh-auth rule; server DB-tested, clients unit-tested, not walked live); OPEN for session + password, for a grant lifted from device memory within its lifetime, and for the discovery second part** (minted 2026-10-03)

**Amended 2026-10-04, generalised 2026-10-06.** The reset, and with it
four more actions a bare session could run, now need a **fresh-auth grant**
(`lakko-auth-token` kind `k1`, its own derived tag key): proof that the
person authenticated by password or passkey within the last 600 s (default),
minted only by the password login, the passkey login and the signup finish.
`403 fresh-auth-required` otherwise, checked before any limiter, so a stolen
session can neither act nor exhaust the owner's budgets. Covered on the
server: `POST /account/keys/reset/{start,finish}` (`finish` re-checks the
generation under the row lock; the bump spends every older grant),
`DELETE /account` (a passkey-less account was deletable on the session
alone until this date; a passkey account's ADR-0014 gesture is its proof),
the three add-device routes (an unlocked device could otherwise confirm a
new device and hand over the recovery phrase and the account with it), and
passkey registration (a session could otherwise mint itself a credential
that mints grants). Covered on the clients only: changing the password on a
device that holds the keys, which asked for nothing but the new password
and now needs a live grant or the local three-word check (the server route
is authorised by the recovery seed the device holds, so a compromised
client is not stopped by this). No TOTP is asked for and a TOTP sign-in
mints no grant: the authenticator usually lives on the phone a
lost-everything person lost, and a code alone must not reset or delete; a
person holding only the authenticator is told so on the reset screen. Every
other device of the account shows a notice when a device joins its
DeviceGroup. What remains: an attacker holding the password (the reset is
then a legitimate sign-in, and with the new phrase they change the password
and lock the owner out; a recall from a surviving old device, or a delay,
was discussed and is undecided); a grant read out of a device's memory
together with the session inside its ten minutes (a recall from a surviving old device with the previous password was built on 2026-10-08, ADR-0074 T3-T4, unit-tested only, so "undecided" above is history); a stolen recovery phrase
or a passkey that carries keys, which need no reset at all. The second
part below was closed in code on 2026-10-08 (ADR-0074 T1: the directory
delete is under the generation floor, needs an account statement for
another device's row, and tombstones the id). The paragraphs that follow describe the state before
this amendment.

**What the code does.** `POST /account/keys/reset/{start,finish}` is
bearer-authenticated and needs **no ADR-0014 action grant** even when the
account has a passkey (`services/auth/src/key_reset.rs`): the route exists for
the lost-everything path, where the passkey may be gone too. Before ADR-0073 a
session could already mint a fresh identity (ADR-0017 `mintFreshIdentity`); it
can now also delete every passkey, vault, WebAuthn state and action grant,
every device grant and request, supersede every other session, and (through
the attestation) re-root the DeviceGroup journal and delete every KeyPackage
row. The password envelope and the TOTP secret survive (ADR-0073 open question
1), so an attacker holding the password or the authenticator can sign in again
after the owner resets, and reach the reset themselves. Limits:
`Scope::RecoveryReplace` (10 per hour) on both calls and `Scope::KeyReset` (3
per day) on `finish`. **Second part:** `DELETE /discovery/keypackage/device/{id}`
is not under discovery's generation floor (the ADR names the two identity
writes only, and the build gated exactly those), so within the access TTL
(default 900 s) an old-generation token can delete a KeyPackage row of the
account, including the resetting device's re-published one; the other devices
then retire that leaf (ADR-0050 re-admission) until the next re-publish. The
invite / call-invite ledgers and channel registration are likewise ungated,
as the ADR's section 3.4 lists.

**What it buys an attacker.** With a stolen access token (900 s) or refresh
credential (30 days, against auth only): a loud lockout rather than the
silent directory flip-flop it could cause before - every device of the owner
shows the signed-out screen within the access TTL, and the owner, holding the
password or the authenticator, can reset back. The attacker gains no key
material (invariant #2) and the owner's contacts see a safety-number change
either way. A compromise that also holds the password is the pre-existing
ACCESS threat ADR-0017's `lakko-security` pass adjudged no-worse. The
KeyPackage delete buys a brief denial of a device's chat membership, self-
healing at the next re-publish.

**Closing.** A narrower scope for the reset itself (ADR-0073 open question 3
took `Scope::KeyReset`; a passkey action grant when one exists, or a password
re-entry on the resetting device, would raise the bar for a stolen token
without touching the lost-everything path - undecided); forcing a password
change or deleting the TOTP secret on reset (open question 1, recommended
against); putting the KeyPackage delete under the generation floor (one line
in `services/discovery/src/keypackage.rs`, the same `floor_refuses` call the
upload makes). None is built.


## Client account binding and local stores

<a id="g-47"></a>
### G-47: A fresh identity's phrase rule (ADR-0077 decision 3) holds in full on Android only

**Status:** **PARTLY CLOSED** (2026-10-14: the phrase is now written before the server step on web, iOS and Android, and a typed phrase clears the owed marker on all three; host-tested only, not walked live. The text below the closing note is the entry as minted at `3af913ad`.)

**Closed 2026-10-14.** Web (`backup/staged-phrase.ts`, `auth/reset-phrase-stage.ts`) and iOS (`Auth/FreshPhraseKeep.swift`) now stage the new phrase in the account's phrase store before the ADR-0073 server reset, as Android's `StagedPhrase` does: a failed write refuses the reset with `restore_identity.lost.error.phrase_not_stored`, and a failed server step puts the previous row back while the row still holds this phrase. Web and iOS stage on the recall routes too; Android stages in `mintFreshIdentity` only. A phrase the person typed clears the owed marker on all three (web `PhraseSource`, iOS and Android `phraseHeldByPerson`); a phrase from the kept row, a vault or a passkey stays owed.

**Still open.** (1) A server reset that succeeded but whose answer was lost runs the rollback, so the account's new phrase is on no device (all three; the rule as written). (2) A process killed between the server's answer and the landing leaves the phrase in the row with no owed marker yet, and no path reads it back (iOS and Android; web keeps the staged phrase on its pending recall record). Marking the phrase owed together with the staging write, and clearing it on rollback, would close (2). (3) How the phrase is marked owed still differs: web and Android mark it and clear it at the next landing for an account that is not phrase-protected; iOS marks it only when ADR-0075 shows new words. (4) A recall whose phrase cannot be stored shows the generic recall error on web (no catalog text for it).


[ADR-0077](../adr/0077-account-binding-and-store-gate.md) decision 3 (the
maintainer's, 2026-10-13) says the new phrase of `mintFreshIdentity` is written
to the account's own protected phrase store before the irrevocable step, and
that a failed write refuses the mint. Since the account-recovery merge
(`1ae14935`) the irrevocable step is the ADR-0073 server reset.

- **Android:** holds. `StagedPhrase` stages the row before `resetAccountKeys`,
  refuses with `restore_identity.lost.error.phrase_not_stored` when it cannot,
  and puts the previous row back when the server step fails.
- **Web:** does not hold. At `c22ad3e3` `mintFreshIdentity` wrote the phrase
  before the identity publish and refused on a failed write; the merge replaced
  that flow with `adoptKeyResetOutcome`, which stores the phrase after
  `resetAccountKeys` answered (`storeDevicePhrase` logs a failed write and
  returns `false`, which the caller ignores).
- **iOS:** does not hold. `landKeyReset` stores the phrase
  (`DevicePhrase.store`, which does not throw) after `keyResetOperation`
  answered.

So on web and iOS a page or process that dies between the server's answer and
the local writes leaves a reset account whose new phrase is on no device. The
point of decision 3's first rule was to close that window.

A second divergence, smaller: decision 3 clears the owed marker when the person
typed the phrase (they hold it). Android does (`phraseHeldByPerson` on `recover`
and on the restore prompt); web and iOS do not, so a person who types the
phrase can be shown it again at the next landing. Also differing, by design or
not decided: web marks the phrase owed unconditionally, iOS only when ADR-0075
shows new words, Android marks it and clears it at the landing for an account
that is not phrase-protected; iOS also clears the marker when the owed phrase
is no longer readable.

**Closing:** stage the phrase before the server step on web and iOS as Android
does, and decide the typed-phrase clear for web and iOS.

<a id="g-48"></a>
### G-48: Account store gate coverage (ADR-0077 decision 4)

**Status:** **PARTLY CLOSED** (built and host-tested on web, iOS and Android by `16d6a27f`, `c22ad3e3`, `3af913ad` and the commit after it; not walked live)

The gate (C-73) refuses a sealed account's store writes and Forget device seals
before it deletes.

Closed by the commit after `3af913ad` (host-tested only):

- iOS sign-in key writes (device key, export key, account and device identity
  seeds, the phrase) take a wipe mark at the start of every sign-in route
  (`SignInKeyWrites`) and are refused only if the account was wiped after the
  route began; `replacePassword` gained the binding check it lacked.
- iOS `CoPublishedJournals`, `CoPublisher.markFounded`, `MediaPrefs` played
  marks and `ProfileDirectory.acceptReceived` take the account their work
  captured and write nothing when it is no longer the bound one.
- The Android phrase store is gated with the same wipe-mark semantics.
- The join request on all three clients, and the channel co-publisher request
  on web and Android, persist the proposal's keys fail-closed before the knock
  that carries the proposal; Android's five proposal publishes persist
  fail-closed first, as web's do.
- The web service worker caches navigations under one shell key, so no
  account URL is kept as a cache key.

Still not covered, or covered differently:

- Closed 2026-10-14: Android (`auth/SignInKeyWrites.kt`) and web (`WipeMark`,
  `flow.openStores`) sign-in key writes are under the route's wipe mark, as
  iOS's are; the iOS channel co-publisher request persists its KeyPackage keys
  (`CoPublishRequestKeys`) before the knock.
- Android: the seal ends at the account's next landing, so a stale coroutine
  that resumes after the same account signed in again is stopped by its binding
  check only.
- The web gate's ordering argument (a read-write transaction created before the
  wipe's delete commits first and is deleted by it) rests on the IndexedDB
  specification, not on a test.
- Web seals only the session-token row at logout (`sealAccountSession`; its
  argument is that a late write of the account's own rows belongs on disk); iOS
  and Android seal every store of the account at logout, and the durable MLS
  writes then throw. The two models were not reconciled.

**Closing:** gate each listed path; decide one logout model.

<a id="g-49"></a>
### G-49: An overtaken sign-in can already have changed server state; binding residuals

**Status:** **OPEN** (state at `3af913ad`, 2026-10-14)

ADR-0077 decision 2 drops a sign-in whose binding changed while it ran: nothing
lands on the device. What it cannot undo is what the server already did. An
abandoned recover has already changed the password on the server; an abandoned
one-time code (TOTP, recovery email) is already consumed. The person sees no
error (the flow was abandoned by design) and the next sign-in needs the new
password or a new code.

Binding residuals:

- iOS `JournalClaimOnOpen.ensure` writes its `settled` / `failedAtMs`
  bookkeeping after its await with no binding check. The claim it runs
  (`claimJournalAsOwner`) checks its own binding and answers `.failed` when
  overtaken, so a stale run cannot record a stale success; the wrapper itself
  is not guarded.
- Most guards on all three clients are covered by build and reading only; web
  `AuthContext` behaviour by pure modules and source pins.
- Sign-up is not dropped after the registration finished, by decision; web
  guest signup is still stopped by a sign-out during its landing
  (`registered: true` selects `captureSignOut`), while iOS and Android guest
  signup are not. Not decided whether that difference matters.

**Closing:** guard the wrapper; a live walk of two racing sign-ins and a
sign-out during each sign-in route on all three clients.

<a id="g-50"></a>
### G-50: Channel PublisherGroup commits publish before they persist

**Status:** **OPEN** (found on web 2026-10-14 by the residual pass after ADR-0077; iOS and Android not checked; not fixed)

ADR-0040 closed the device-ahead class for chat groups; the channel
PublisherGroup has the same shape and no outbox. Read at `3af913ad` on web:

- **Co-publisher approve.** `approveCoPublisher` (`apps/web/src/channels/copublisher.ts`,
  called from `CoPublishersPanel.tsx` `onApprove`) eager-merges the `Add` with
  `approveExternalJoin`, publishes the commit to the DS journal, delivers the
  Welcome and reseals every subscriber at the new seed. Only afterwards does the
  panel call `channelsActions.notePublisherGroupChanged()`, which schedules a
  write 150 ms later (`schedulePersist` in `apps/web/src/state/channels-state.ts`);
  `flushNow` catches a failed write and logs it.
- **Rotation.** `runRotationOnce` (`rotation-cron.ts`) calls
  `publisherGroup.rotate()`, registers the next DS epoch, reseals the new seed to
  every subscriber and commits the rotation on `services/channels`; the
  `onRotated` callback in `RotationPanel.tsx` then refreshes the publisher keys
  and calls the same debounced, error-swallowing `notePublisherGroupChanged()`.

A reload or crash inside the debounce, or a failed write, leaves the server and
the subscribers at the new epoch and seed while the device reopens the
PublisherGroup at the old one. Persisting first would not fix it (a persisted
N+1 with the publish never made is the device-ahead case ADR-0040 describes):
it needs an outbox-style record of the commit and the reseal, as ADR-0040 keeps
for chat groups.

**Closing:** an outbox-style record for PublisherGroup commits; check iOS and
Android.

<a id="g-51"></a>
### G-51: Channel publish counters restart at 0 after Forget device

**Status:** **NOT REAL AS REPORTED; two adjacent reuse paths fixed** (2026-10-14, by reading on all three clients; host-tested)

**Resolution 2026-10-14.** On web, iOS and Android the channel post signing key, which enters the post key and nonce, is deleted together with the publish counters (web: the channel log goes first; iOS: one `ChannelsUiState.forget`, both Keychain items share one lifetime and also survive app deletion together; Android: one encrypted prefs file, one editor), so after Forget device the device signs with a new key and the counter restarting at 0 is safe. Two adjacent paths were real and are fixed: Android handed out a counter whose write used `apply()` (a crash after the post went out could restore the old counter under the same key) and handed it out even when the gate refused the write; iOS read an unreadable counter item as no floor while the signing key still read. Each now hands a counter out only after a durable write, and refuses it otherwise. Web reads the counter row back after writing it.

The entry as reported:


The report: Forget device deletes the channel publish counters, a new sign-in
restarts them at 0, and if the device lands at the same PublisherGroup epoch
the channel content seed is unchanged, so a post could be sealed under an
earlier post's `(key, nonce)` (the G-33 class).

What the web code at `3af913ad` says: `compose_post` derives the key and nonce
from `(seed, channel_id, epoch_id, sender_pubkey, message_counter)`, and the
post signing key is random per device and channel (`ChannelSigningKey.generate`
in `apps/web/src/channels/composer.ts`), stored only in the encrypted channel
log. The wipe deletes the channel log and only then the counters
(`wipe-account.ts`, step `channel-log`), so the next post after a new sign-in
signs with a new key and its `sender_pubkey` differs. A reuse would need the
old signing seed to survive the wipe; no such path was found on web. iOS
(`ChannelsComposer.swift`) and Android (`ChannelsComposer.kt`) also generate the
signer randomly; whether their wipes remove the signing seed with the counters
was not checked.

**Closing:** check the iOS and Android wipes; close this entry if they match
web.
