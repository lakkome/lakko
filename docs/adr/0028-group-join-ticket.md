<!--
SPDX-License-Identifier: AGPL-3.0-or-later
SPDX-FileCopyrightText: 2026 Lakko Contributors
-->

# ADR-0028: Group join-ticket (invite-link for joiner-initiated external join)

- **Status:** Accepted
- **Date:** 2026-06-25
- **Context:** Phase 3/4 — the live consumer of ADR-0027 (external-join approval, G-3)

## Context

[ADR-0027](0027-external-join-approval.md) closed G-3 on the **admin** side:
an owner/admin sees a non-member's MLS external `Add` proposal
(`Incoming::ExternalJoinProposal`), approves it (role-gated, reusing the
audited `add_member`), and ferries the Welcome back over a `WelcomeRendezvous`
journal envelope. The **joiner-side core** also landed:
`Identity::create_external_join_proposal(group_id, epoch)` lets a non-member
craft the external `Add` proposal, and a full in-process e2e test proves the
whole loop (propose → approve → rendezvous → `join_from_welcome` → member).

What is still missing is the **live consumer**: a non-member has no sanctioned
way to learn

1. the target **`group_id`** (a random 32-byte handle — not guessable, never
   on the wire in cleartext for a group it is not in), and
2. the group's **current `epoch`** (an MLS external `JoinProposal` is bound to
   a specific epoch; a stale-epoch proposal is rejected by OpenMLS),

nor any signal that the group *accepts* joiner-initiated requests at all.

ADR-0027's table row called this "the channel join-ticket of
[ADR-0010](0010-channels-two-tier-broadcast.md)", but reconnaissance shows
that framing is imprecise: ADR-0010 channel **subscribers are not MLS members**
of the PublisherGroup — they receive an HPKE-sealed content-key seed via
sender-key broadcast, never an MLS external join. The external-join path is for
adding a **member/publisher** to an MLS group (a regular conversation group, or
a channel's PublisherGroup) by the *joiner's* initiative. So the missing piece
is a **general group invite-link / join-ticket**, not an ADR-0010-specific
artefact.

Forces:

1. **The admin gate is the security boundary, not the ticket.** ADR-0027
   already requires an owner/admin to approve every external join (role-gated,
   fail-closed, peer-verified on receipt). Whatever conveys the group params to
   the joiner does **not** need to *authorize* the join — it only needs to
   convey public params. A leaked ticket lets a stranger *ask*; it never lets
   them *in*.
2. **Epoch staleness.** A group's epoch advances on every membership change. A
   ticket that hard-codes an epoch goes stale the moment anyone is added or
   removed, and the joiner's proposal is rejected. The carrier must not bind an
   epoch.
3. **The server stays opaque (invariant #2).** The ticket is a client artefact;
   the DS only relays the resulting proposal + rendezvous bytes, which it
   already stores opaque.
4. **Reuse, don't reinvent.** [ADR-0009](0009-link-based-call-invitations.md)
   already shipped a capability-based link protocol for a joiner outside the
   owner's contacts (`CallInviteCapability` + `/c/` URL + content-addressed
   blob ledger). The group join-ticket should mirror it.

## Decision

A **capability-based group invite-link** that conveys only the **`group_id`**
(plus integrity metadata), mirroring ADR-0009; the joiner reads the **current
epoch from the group journal's public MLS framing** at request time, so the
ticket is epoch-independent and long-lived.

### 1. Wire format — `GroupInviteCapability`

A compact, bounded, fail-closed blob (new `crates/lakko-contacts` module
`group_invite.rs`, mirroring `call_invite.rs`):

```text
version(1) ‖ group_id_len(2 BE) ‖ group_id ‖ issuer_pubkey(32)
           ‖ expiry_ms(8 BE) ‖ sig(64)
```

- `group_id` — the target group's opaque MLS `GroupId` bytes the joiner passes
  to `create_external_join_proposal`. **Length-prefixed** (an MLS `GroupId` is
  variable-length; Lakko's `Group::group_id()` is a `Vec<u8>`), bounded by
  `MAX_GROUP_ID_LEN` (256).
- `issuer_pubkey` — the owner/admin Ed25519 signature key that minted the link;
  lets the joiner render *who* invited them and lets the admin UI show *which*
  link a pending request came in on (defence-in-depth, not an auth gate).
- `expiry_ms` — UNIX **milliseconds** (the `invite.rs` / `call_invite.rs`
  convention); `0` = never (honest sentinel). UI surfaces a non-expiring link.
- `sig` — Ed25519 over everything before it (`version ‖ group_id_len ‖
  group_id ‖ issuer_pubkey ‖ expiry_ms`), signed by the issuer. Anti-tamper + binds the `group_id` to a real member of
  the group (the admin can verify `issuer_pubkey` is in the group's roster
  before approving — a forged link names a key that is not a member).

**No epoch field** — see §3.

### 2. URL plumbing

Mirror ADR-0009's `/c/` exactly, under a new `/g/` prefix:

- **Inline** — `<origin>/g/<base64url(capability_bytes)>` (the blob is ~137 B,
  well under ADR-0009's 400-byte inline ceiling, so inline is always used; no
  server blob ledger needed in v1).
- The joiner-side parser (`decode_and_verify`) checks version + length +
  expiry + the issuer signature, fail-closed.

### 3. Epoch acquisition — the joiner reads it from the journal

The MLS epoch is **public**: every `MlsMessage` (PrivateMessage or
PublicMessage) carries `group_id` + `epoch` in its cleartext framing, ahead of
any encrypted content. So a non-member can read the group's current epoch
without any group secret:

1. The joiner fetches the group journal head
   (`GET /ds/group/:group_id/messages?after=<n-1>` for the latest row).
2. A new `lakko-mls` helper `epoch_of_message(bytes) -> Option<u64>` extracts
   the epoch from the latest message's public framing. An **empty** journal
   (a freshly-created group, no commits yet) ⇒ epoch `0`.
3. The joiner crafts `create_external_join_proposal(group_id, that_epoch)` and
   posts it to the journal (PoW-gated, anonymous — the joiner is not a member).
4. **Retry-on-stale:** if the epoch advances between read and approve, OpenMLS
   rejects the proposal at the admin's `process_incoming`; the joiner re-reads
   the epoch and re-posts. Epochs advance only on membership churn, so the race
   is rare; the joiner's poll-loop simply re-derives on each pass until a
   Welcome arrives or the ticket expires.

### 4. The full live flow

```text
Owner/admin           Link (OOB)          Joiner (non-member)         DS journal
-----------           ----------          -------------------         ----------
mint GroupInvite  ──► /g/<cap>  ──────────► decode_and_verify
Capability                                  read epoch from journal ◄── (head)
                                            create_external_join_                ─► proposal
                                              proposal(gid, epoch)
process_incoming_  ◄──────────────────────────────────────────────────────────── (poll)
  verifying_roles
  → pending list (ADR-0027 Phase-4 UI)
approve_external_                                                                 ─► commit
  join → commit +                                                                 ─► WelcomeRendezvous
  WelcomeRendezvous
                                            welcomeRendezvousFor(bytes) ◄──────── (poll)
                                            join_from_welcome → member
```

Steps the admin side (`process_incoming_verifying_roles` + the Phase-4
members-panel approve/reject UI) and `WelcomeRendezvous` transport are **already
shipped** (ADR-0027). This ADR adds: the `GroupInviteCapability` codec + FFI;
`epoch_of_message`; the owner-side "create invite link" UI; and the joiner-side
"open invite link → request to join → poll → join" UI on all three clients.

### 5. Anti-abuse

- The external-join POST is **PoW-gated** + anonymous (the joiner is not a
  member, so no session-token gate applies — the PoW is the cost).
- The per-group retained-pending cap (`MAX_PENDING_EXTERNAL_JOINS`) + dedup by
  signature key (both ADR-0027) bound how many requests reach the admin.
- The admin manually approves/rejects each — the human gate is the final
  authorization.

## Alternatives

1. **Epoch baked into the ticket.** Simpler joiner (no journal read), but the
   ticket goes stale on the first membership change and every reissue races the
   next commit. Rejected — §3 (epoch-from-journal) makes the ticket
   epoch-independent and long-lived at the cost of one extra public read.
2. **Capability-MAC-gated proposals (the DS pre-filters).** The ticket carries
   a secret the joiner echoes in the proposal so the DS drops unauthorized
   posts before they reach the admin. Rejected for v1: the admin approval +
   PoW + pending-cap already bound spam; a DS-side MAC check would make the
   *zero-trust DS* enforce a policy it should not interpret (invariant #2), and
   only reduces spam *reaching* the admin, not spam *admitted*.
3. **Content-addressed server blob ledger (full ADR-0009 parity).** Upload the
   capability to `services/storage`, link by `blob_id`, track `max_uses` +
   per-joiner dedup server-side. Rejected for v1 — the capability is ~137 B, so
   the inline link is always small enough; the blob ledger + a single-use /
   max-uses counter is a clean Phase-2 hardening but not needed to close the
   loop.
4. **Reuse the ADR-0005 `InviteCapability` verbatim.** That capability seeds a
   *contact* relationship (pseudonym + HPKE handshake), not a group join, and
   carries fields (rendezvous pseudonym) the group path does not want.
   Rejected — a dedicated `GroupInviteCapability` is leaner and the signature
   binding to `group_id` is the point.

## Consequences

- **Positive:** closes G-3 end-to-end with a *live* requester — the whole
  ADR-0027 machinery (admin UI, role gate, rendezvous) gets a real consumer. A
  "share a link to let people request to join this group" feature, the inverse
  of the existing admin-adds-by-username path. Epoch-independent ticket → mint
  once, share widely, no reissue churn.
- **Negative:** a new client artefact + UI on three platforms. The epoch-read
  adds one unauthenticated public journal read on the joiner side (the DS
  already serves the journal opaque; no new endpoint). Retry-on-stale is a
  (rare) extra round trip.
- **Neutral:** the link leaks the `group_id` to anyone who holds it — but
  `group_id` alone admits nobody (the admin gate). A holder can *ask* to join
  and consume PoW + a pending slot until the admin rejects them; the pending
  cap bounds this.

## Open questions

1. **Single-use / max-uses tickets + revocation.** ~~v1 mints a non-revocable,
   expiry-bounded link. Revocation + a use counter want the ADR-0009
   content-addressed blob ledger (alternative 3) — a Phase-2 hardening.~~
   **RESOLVED.** The blob ledger landed (`services/discovery/src/group_invite.rs`
   + migrations `0014`/`0015`) with a soft `max_uses` counter, and **revocation**
   is now implemented end-to-end: `lakko_contacts::group_invite`
   (`group_revoke_canonical_bytes` + `group_sign_revocation`, domain
   `lakko-group-invite-revoke/v1`, ±10-min replay window — cross-kind-isolated
   from the contact-/call-invite revoke domains) + `DELETE
   /discovery/group-invite/{blob_id}` (issuer-signed; the server recovers the
   issuer key from the stored capability's `issuer_signature_pubkey`, verifies,
   and DELETEs — the FK CASCADE reaps the consumer side-table; 404-folds a
   wrong-signer / missing row) + FFI `signGroupInviteRevocation` (UniFFI + wasm)
   + a "Revoke link" affordance on the owner's members panel (web + iOS +
   Android), revocable for the server-backed pointer form (an inline link has no
   ledger row). A leaked pointer link can now be killed before any stranger asks.
2. **Issuer-not-in-roster rejection.** ~~The admin UI *can* check
   `issuer_pubkey ∈ roster` before approving (a forged link names a non-member
   issuer); whether to enforce it or just surface it is a Phase-2 UX call.~~
   **RESOLVED (surface, not enforce) — implemented end-to-end on web + iOS +
   Android.** The blocker was transport: the admin's
   `Incoming::ExternalJoinProposal { joiner_signature_key }` carries only the
   joiner's leaf key — NOT the `issuer_pubkey`, which lives in the capability
   only the joiner holds. The fix is a journal side-channel mirroring the
   `WelcomeRendezvous` pattern: a new `lakko_contacts::group_join_attribution`
   envelope (`version ‖ joiner_sig_key_len ‖ joiner_sig_key ‖ cap_len ‖
   capability`, bounded + fail-closed + fuzzed) the joiner posts alongside its
   external-join proposal. The admin's members panel reads the journal, decodes
   each entry (`parse_group_join_attribution` re-verifies the embedded
   capability's Ed25519 signature — a joiner cannot fabricate an issuer who did
   not sign), keeps only attributions whose `group_id` matches this group, and
   surfaces per pending request "via &lt;issuer fingerprint&gt; · member / not a
   current member" (checking `issuer_pubkey ∈ group.members()`). It is
   **advisory** — the role-gated approval gate (ADR-0027) stays the security
   boundary; a missing attribution just shows "invite source unknown" and never
   blocks or grants. **Anti-spoof (v2):** the envelope is **self-signed by the
   joiner** — `compose_attribution` takes the joiner's 32-byte Ed25519 identity
   seed and signs a domain-separated (`lakko/group-join-attribution/v1`) prefix;
   the embedded `joiner_signature_key` is the matching verifying key, which in
   Lakko **is** the MLS leaf key (identity signature key, same seed) and exactly
   the key the admin matches against the proposal's KeyPackage leaf. So
   `decode_attribution` verifies the signature against the embedded key, binding
   the attribution to the actual proposing leaf — a stranger cannot post an
   attribution claiming a joiner key whose private half they do not hold. The
   wire bumped to v2 (`version ‖ joiner_sig_key(32) ‖ cap_len ‖ capability ‖
   sig(64)`); an unsigned v1 envelope is refused.
3. **Channel PublisherGroup reuse.** ~~When ADR-0010 channels land, adding a
   *publisher* (an MLS member of the PublisherGroup) reuses this ticket
   verbatim; the channel *subscriber* path stays sender-key (ADR-0010) and does
   not touch this.~~ **RESOLVED — proven + enabled in the core + FFI.** A
   `lakko_mls::PublisherGroup` is a thin wrapper around a vanilla `Group`
   (`inner: Group`) with a deterministic `group_id =
   publisher_group_id_bytes(channel_id)`, so the join-ticket reuses verbatim: an
   owner mints a `GroupInviteCapability` over `PublisherGroup::group_id()`, a
   non-member crafts an external-join proposal (ADR-0027) for that
   `group_id`+epoch, the owner approves through the inner Group, and the joiner
   reconstructs the PublisherGroup under the same deterministic id. Landed:
   `PublisherGroup::group_id()` + `epoch()` accessors; a property test
   (`co_publisher_joins_via_external_join_ticket_and_shares_channel_seed`)
   proving the full loop **and the payoff** — both publishers derive the
   identical `channel_seed` at the shared post-add epoch, so the joined
   co-publisher can immediately post; and the owner-side external-join surface
   (`groupId` / `processIncomingVerifyingRoles` / `pendingExternalJoins` /
   `approveExternalJoin` / `rejectExternalJoin`) exposed on the FFI
   `PublisherGroup` (UniFFI + wasm), delegating to the inner Group. The
   subscriber path stays sender-key (ADR-0010), untouched. **Owner-side web UI
   landed** (`apps/web/src/channels/copublisher.ts` + a `CoPublishersPanel` in
   `Channels.tsx`): the channel owner opens the persistent `PublisherGroup`
   handle, mints a `/g/` co-publisher link over its `group_id`, runs a 5 s DS
   **journal poll loop** (`processIncomingVerifyingRoles` — the first
   DS-journal/membership transport the channel client ever had; channels were
   single-publisher before), and approves / rejects pending requests from a
   panel. **Reseal-after-add (load-bearing):** `approveExternalJoin` commits the
   Add → the MLS epoch advances → the channel content-key seed changes, so the
   approve path RESEALS the new-epoch seed to every subscriber via the audited
   `sealSubscriberSeedEnvelope` + atomic `POST /v1/channels/:id/rotate` fanout
   (the rotation-cron path, but WITHOUT a second `rotate()` — the Add already
   advanced the epoch), else existing subscribers would break. `tsc` clean.
   **Joiner-side web landed** — `copublisher.ts` `requestToCoPublish` (decode the
   `/g/` capability → `channelIdFromPublisherGroupId` recovers the channel id
   from the PublisherGroup `group_id` → read the epoch from the journal →
   `createExternalJoinProposal` + post + self-signed attribution) +
   `pollForCoPublishWelcome` (poll → `welcomeRendezvousFor` → `joinFromWelcome` →
   `freshPublisherKeys` mints local publisher keys at the post-approve epoch, the
   same the subscribers were resealed to); a "Join as co-publisher" affordance on
   the New-channel screen drives request → poll → open the channel. The
   `channel_id_from_group_id` inverse derivation landed in `lakko-mls` (+ a
   round-trip/reject unit test) and on the FFI (`channelIdFromPublisherGroupId`,
   UniFFI + wasm). `tsc` clean, web vitest 222 green. **Residual:** the iOS /
   Android mirrors (owner + joiner) + a live 2-publisher e2e of the reseal (no
   deployed channels env, the same gate every other live-channel path carries).

## References

- [ADR-0027](0027-external-join-approval.md) — external-join approval (the
  admin side + the joiner-side core this ADR makes live).
- [ADR-0009](0009-link-based-call-invitations.md) — the capability-link
  protocol this ADR mirrors (`CallInviteCapability`, `/c/` URL).
- [ADR-0005](0005-contact-invitations.md) — `InviteCapability` + the OPRF /
  capability patterns.
- [ADR-0010](0010-channels-two-tier-broadcast.md) — channels; clarifies that
  subscribers are not MLS members (so this ticket is for publishers/members).
- RFC 9420 §12.1.8.3 — MLS external `Add` proposals (`NewMemberProposal`).
