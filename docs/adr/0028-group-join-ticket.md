<!--
SPDX-License-Identifier: AGPL-3.0-or-later
SPDX-FileCopyrightText: 2026 Lakko Contributors
-->

# ADR-0028: Group join-ticket (invite-link for joiner-initiated external join)

- **Status:** Accepted
- **Date:** 2026-06-25
- **Context:** Phase 3/4 — the live consumer of ADR-0027 (external-join approval, G-3)

> **AMENDED 2026-09-28 — sealed links (maintainer's decision on the security
> pass below). Every invite link carries a secret after `#`, and the
> capability is encrypted under it.** Flag day, no dual format: a `/g/` link
> without its fragment no longer opens, and the ledger holds nothing a
> pre-amendment client can read.
>
> - **Link shapes.** `/i/`, `/c/` and `/g/` all read
>   `https://<host>/<k>/<8-char id>#<16-char secret>` (pointer), or
>   `https://<host>/<k>/<base64url sealed blob>#<16-char secret>` (the inline
>   fallback, used only when the upload fails on transport). A link whose
>   fragment is missing or is not 16 base64url characters is **damaged**, on
>   every kind.
> - **One construction for all three kinds** (`lakko_contacts::link_seal`):
>   a 12-byte CSPRNG secret (`LINK_SECRET_LEN`, the `/g/` width of the
>   short-link amendment, now shared); the sealed blob is
>   `kind(1) ‖ version(1) ‖ nonce(12) ‖ ChaCha20-Poly1305(key, nonce,
>   aad = kind ‖ version, plaintext = the WHOLE signed capability)`, with
>   `kind` the path letter (`i`, `c`, `g`) and
>   `key = HKDF-SHA256(ikm = secret, salt = nonce,
>   info = "lakko/invite-link/<k>/seal-key/v1\0")`. The per-link nonce as
>   the salt keeps a search over secrets from being amortised across links;
>   the per-kind `info` and the kind byte in the AAD refuse a blob behind the
>   wrong path. Every open failure (no secret, wrong secret, another kind,
>   truncated, tampered, substituted, a capability that does not verify
>   inside) is the one refusal `ContactError::LinkDamaged`.
> - **`/g/` keeps its sealed preview inside the capability** rather than
>   folding the names into the plaintext: the plaintext capability also
>   travels in the joiner's `group_join_attribution` to the group's knock
>   journal on the Delivery Service, where it is not encrypted to anyone, so
>   the names must stay sealed there. One secret serves both layers under
>   independent HKDF derivations (`info` differs). The capability wire (v2)
>   is unchanged.
> - **What the server stores and needs.** The ledger column holds the sealed
>   blob only (migration `0022_sealed_invite_links.sql`, widths derived from
>   each codec's `MAX_SEALED_LEN`: 254 / 4338 / 938 bytes). What the server
>   used to read out of the capability now arrives in clear upload fields:
>   `issuer_pubkey` (the key the issuer-signed revoke verifies against, all
>   three kinds), `expiry_ms` (fetch filter, pruner) and `max_uses` (consume
>   ceiling; for `/g/` it was a clear field already), plus, for `/i/` only,
>   `label` and `bootstrap_referral` for the owner's own list of live invites
>   (owner metadata the server could read before; sealing it for the owner
>   too is a possible follow-up). `owner_account_id` is the session's
>   account. The server no longer stores `group_id` or `call_id` at all. An
>   uploader who lies in a clear field only changes how its own link
>   behaves; naming someone else's `issuer_pubkey` gives that person, not the
>   uploader, the power to revoke the row.
> - **The joiner tag** is computed by the joiner over the PLAINTEXT
>   capability the link opens to, as before; the server needs nothing else
>   from the plaintext to count a consume, and it can no longer recompute a
>   candidate joiner's tag itself.
>
> **Closed** by this amendment (the first three items the security pass
> below left open): a 48-bit `/c/` pointer is no longer a bearer into an
> auto-admitted call (a fetched blob opens nothing without the 96-bit
> secret); a malicious discovery can no longer substitute `/i/` or `/c/`
> capabilities (a substituted blob fails the AEAD open); a `/g/` pointer that
> lost its fragment no longer accepts a substituted capability (it opens
> nothing). **Still open — the burn residual:** anyone holding a live id can
> spend an invite's uses with random joiner tags, one anonymous token and the
> per-IP limit per attempt, because the tag is opaque to the server. Binding
> a consume to knowledge of the secret (a consume key derived from it, its
> verifier stored beside the blob) would close it; not built.

> **SECURITY PASS 2026-09-28 (`lakko-security` on the short-link amendment
> below).** Fixed in the same pass:
>
> - **Per-IP limiter keyed on the full address (MEDIUM).** Discovery's
>   token buckets were keyed on the exact `IpAddr`, so one IPv6 subscriber's
>   /64 was 2^64 fresh buckets and the ceiling that now bounds id guessing did
>   not bind it; each new bucket also grew memory until the 5-minute sweep.
>   Buckets are now keyed by network (`rate_limit::network_key`: IPv4 per
>   address, IPv4-mapped IPv6 as its IPv4, other IPv6 per /64), and each
>   limiter holds at most `DEFAULT_MAX_BUCKETS` = 100 000 buckets: at the cap
>   it drops every idle bucket (one refilled to its burst, so nothing is lost)
>   and, if none is idle, the least-recently-touched eighth. Tests in
>   `services/discovery/src/rate_limit.rs`.
> - **The ledger refused long previews (INFO, functional).**
>   `group_invites.capability_bytes` was `VARBINARY(512)`, sized for v1, while
>   a v2 capability with a maximal sealed preview is 908 bytes
>   (1 + 2 + 256 + 32 + 8 + 2 + 543 + 64), so a link naming a long group or
>   inviter failed to upload and fell back to the long inline link.
>   `MAX_CAPABILITY_BYTES` is now derived from the codec's bounds
>   (`MAX_GROUP_ID_LEN`, `MAX_SEALED_PREVIEW_LEN`, the Ed25519 widths) and
>   migration `0021_group_invite_capability_width.sql` widens the column to
>   match; a test composes a maximal capability and checks it fits exactly and
>   survives upload and fetch against the database.
> - **Stale documentation (INFO).** `lakko_contacts::invite` still described
>   the ledger id as `SHA-256(capability)` and cited proof of work for the
>   joiner-tag probe (it is an anonymous `invite-consume` token plus the per-IP
>   limit since ADR-0054 S3); `services/discovery/src/group_invite.rs` said "no
>   revoke (v1)" while issuer-signed revocation exists. Corrected.
> - **The preview decoder was unfuzzed (INFO).** `group_invite_decode` never
>   reaches `GroupInvitePreview::decode` (random input neither verifies nor
>   opens the AEAD). New fuzz target `group_invite_preview_decode` feeds the
>   plaintext decoder directly through `lakko_contacts::fuzz_api` and asserts
>   decode / validate / re-encode agreement.
>
> **Still OPEN — design items this pass did not change**, recorded so the
> short-link trade is read with them. *(The first three are CLOSED by the
> sealed-link amendment above; the fourth remains.)*
>
> - **A `/c/` pointer link is a 48-bit bearer into a call that may
>   auto-admit.** The id is the only secret in the link; whoever fetches it
>   holds a valid capability, and under ADR-0032's auto admission that is a
>   seat in the call. Guessing is bounded only by the per-IP limit.
> - **A malicious discovery can substitute `/i/` and `/c/` capabilities
>   undetectably.** The id no longer hashes the bytes, and neither link carries
>   a secret that would bind the fetched capability, so any validly-signed
>   capability can be served under a live id.
> - **A `/g/` pointer link that lost its fragment accepts a substituted
>   capability.** The link secret is what binds the capability's group and
>   issuer; a link shared without the `#` part carries nothing to check the
>   fetched bytes against.
> - **Anyone holding an id can burn an invite's uses.** The joiner tag is
>   opaque to the server, so random tags each count as a distinct joiner
>   against `max_uses`; the cost is one anonymous token and the rate limit per
>   attempt. For `/g/` this is the soft anti-spam counter, not the admission
>   gate.

> **AMENDED 2026-09-27 — short links (maintainer's decision: a whole link of
> about 45 characters).** A pointer link read
> `https://<host>/g/<43-char id>#<43-char key>`; it now reads
> `https://<host>/g/<8-char id>#<16-char secret>`. Two changes, both flag day
> (no deployment, no link to keep working, the old lengths are refused):
>
> - **Ledger id (all three ledgers, `/i/`, `/c/`, `/g/`).** Was
>   `SHA-256(capability_bytes)`, 32 bytes / 43 characters, idempotent by
>   content. Now a **random 6-byte id** the discovery service mints from a
>   CSPRNG at upload (`lakko_contacts::invite::generate_blob_id`,
>   `BLOB_ID_LEN = 6`, 8 base64url characters). The insert is a plain `INSERT`
>   retried under a fresh id on a primary-key collision (at most 4 attempts,
>   then 500) and is never an upsert, so a collision cannot overwrite another
>   link's row; the same capability uploaded twice gets two ids. Fetch,
>   consume and revoke take the 8-character id and refuse any other width with
>   400; the revoke canonical bytes carry the 6-byte id. Migration
>   `services/discovery/migrations/0018_short_invite_ids.sql` recreates the six
>   ledger tables with `blob_id BINARY(6)`. **What this gives up:** the id is no
>   longer a hash of the bytes, so a joiner cannot re-hash a fetched capability
>   against it and a malicious discovery server could serve a different
>   validly-signed capability under a live id (no client ran that re-hash
>   check, so nothing that was enforced is lost). For `/g/` the link secret
>   below now covers this: the preview key binds the capability's group id and
>   issuer, so a substituted capability's preview does not open under the
>   link's secret. `/i/` and `/c/` links have no such secret. **Guessing live
>   ids** (2^48) is bounded by the per-IP token bucket: the six open fetch and
>   consume routes moved into discovery's rate-limited router (60/min, burst
>   10 per anonymous address by default), where before they were unlimited on
>   the reasoning that a 32-byte id could not be enumerated.
> - **Link secret (`/g/` only).** The URL fragment was the 32-byte AEAD key
>   (43 characters). It is now a **12-byte secret** (`LINK_SECRET_LEN`, 16
>   characters), and the 32-byte preview key is derived from it:
>   `HKDF-SHA256(ikm = secret, salt = empty, info = "lakko/group-invite/preview-key/v1\0"
>   ‖ u16be gid_len ‖ group_id ‖ issuer_pubkey)`. The secret is uniform random,
>   so HKDF only expands it; binding the group id and issuer is free (both sit
>   in the signed capability the opener holds) and makes every link's
>   derivation distinct, so a search over secrets cannot be amortised across
>   links and a secret moved onto another capability opens nothing. The inline
>   fallback form (`/g/<whole capability>#<secret>`, used only when the upload
>   fails) keeps its shape with the shorter fragment.

> **AMENDED 2026-09-02 by [ADR-0045](0045-remote-commit-epoch-fork.md) S7
> (`576ea14`). The ticket is unchanged; the far end of the flow it feeds is
> not.** In a group that carries a role table — every ordinary chat group —
> `approve_external_join` no longer commits. The approving admin publishes an
> `Add` **proposal**, and one elected committer later sweeps it and posts the
> `WelcomeRendezvous`. Three readings below need adjusting, and all three are
> in the *description* of ADR-0027's half rather than in this ADR's own
> decision:
>
> - **The Context** says the admin *"approves it (role-gated, reusing the
>   audited `add_member`), and ferries the Welcome back"*. Approving no longer
>   calls `add_member` and produces no Welcome.
> - **The § 4 flow diagram** shows `approve_external_join → commit +
>   WelcomeRendezvous` as one step. It is now two, by two possibly different
>   devices, with **no deadline** between them (ADR-0045 decision 2).
> - **The joiner's wait therefore has three states, not two:** *asked* →
>   *approved* (a proposal is on the wire) → *admitted* (a committer swept it).
>
> **Nothing in this ADR's own decision moves.** The capability codec, the
> no-epoch-in-the-ticket rule, the read-the-epoch-from-the-journal mechanism,
> the retry-on-stale loop, and — most importantly — *the security boundary is
> the admin approval gate, not a ticket secret* all hold exactly as written.
> **OQ3's co-publisher path is also unaffected and says so already** (see the
> note there): a `PublisherGroup` seeds no role table, so it keeps the direct
> commit and its load-bearing reseal-after-add.

## Context

[ADR-0027](0027-external-join-approval.md) closed G-3 on the **admin** side:
an owner/admin sees a non-member's MLS external `Add` proposal
(`Incoming::ExternalJoinProposal`), approves it (role-gated, reusing the
audited `add_member`), and ferries the Welcome back over a `WelcomeRendezvous`
journal envelope. *(Both halves of that sentence changed under ADR-0045 S7 —
see the head note. The `WelcomeRendezvous` envelope itself is unchanged; only
who posts it, and when, moved.)* The **joiner-side core** also landed:
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

**Amended 2026-09-26 (v2, a flag day - v1 is refused):** the capability gained a
sealed preview between `expiry_ms` and the signature:

```text
version(1)=2 ‖ group_id_len(2 BE) ‖ group_id ‖ issuer_pubkey(32)
             ‖ expiry_ms(8 BE) ‖ preview_len(2 BE) ‖ sealed_preview ‖ sig(64)
sealed_preview = nonce(12) ‖ ChaCha20-Poly1305(link_key,
                 version(1)=1 ‖ group_name_len(1) ‖ group_name
                 ‖ inviter_len(1) ‖ inviter_name ‖ member_count(2 BE))
```

The person opening a link saw only the issuer's fingerprint, which tells them
nothing about what they are joining (UX spec
[`group-invite-page.md`](../ux/group-invite-page.md)). The preview - the
group's name, the inviter's display name and the member count at mint time - is
sealed under a key derived from a random **link secret** carried in the URL
**fragment** (`https://lakko.me/g/<slug>#<secret>`; 12 bytes since the
2026-09-27 amendment above, 32 before), which a browser never sends and an app
never uploads, so the discovery ledger holding pointer-link capabilities and
every access log stay ciphertext-only (invariants #1, #2, #5). Whoever holds
the whole link reads the preview, which is the point of a link one shares. The
preview sits inside the signature, so it cannot be swapped under a genuine
capability; it remains the issuer's claim (the inviter name is self-asserted,
like a profile name), which is why the fingerprint stays on the page as the
out-of-band check. `preview_len = 0` is a link without a preview, and a page
whose fragment is missing or whose key does not open the preview shows the
page without one, never a guess. Both names follow the display-name rule;
`MAX_SEALED_PREVIEW_LEN` bounds the field before any allocation. Core:
`lakko-contacts::group_invite::{compose_with_preview, generate_link_key,
GroupInviteCapability::open_preview, GroupInvitePreview}`; FFI
`composeGroupInviteCapabilityWithPreview` -> `{wire, linkKey}` and
`GroupInviteCapability.openPreview(linkKey)` / `hasPreview` on both surfaces.

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
- **Since v2** both forms carry the link key as the URL fragment
  (`#<base64url key>`); the fragment is never part of the request path, so a
  pointer link's capability on the ledger and an inline link's capability in an
  access log are equally unable to reveal the preview.

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

**Update (2026-09-19) — two corrections from the first live run.** Step 1 as
shipped read the journal once with `after=0`, and the DS pages at 100 rows by
default: on a 370-row journal the "head" was a row from hours earlier, the
proposal carried a stale epoch, and every member refused it in silence (the
step-4 retry never fired, because nothing tells a non-member its proposal was
refused). The joiner now pages to the end of the journal (`hasMore`, the DS
maximum page size) before reading. And step 2 was off by one whenever the head
is a Commit: a Commit is framed at the epoch it was created in, while the group
it leaves behind is one higher, so the joiner uses `epoch_after_message` (the
framed epoch, plus one for a Commit; the content type rides the same cleartext
framing). `epoch_of_message` keeps its framed-epoch meaning for the other
callers. The joiner's poll for the admin's Welcome (step 4 of section 4) read
the journal the same single-page way and so never saw a Welcome on a journal
longer than one page; it now walks the journal through the same paged scan.

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
approve_external_                                                                 ─► Add proposal
  join                                                                               (authored by the
                                                                                      approving admin)
   ⋮  (no deadline — ADR-0045 decision 2)
elected committer                                                                 ─► commit
  sweeps                                                                          ─► WelcomeRendezvous
                                            welcomeRendezvousFor(bytes) ◄──────── (poll)
                                            join_from_welcome → member
```

> **Updated 2026-09-02 for [ADR-0045](0045-remote-commit-epoch-fork.md) S7.**
> Before S7 the last admin step was a single `approve_external_join → commit +
> WelcomeRendezvous`. It is now two steps by two possibly different devices,
> because two admins approving two joiners against one epoch would otherwise
> fork the group. The sweeping device cannot tell which adds came from a `/g/`
> link, so it posts a `WelcomeRendezvous` for **every** added key — pinned as
> **C-69**. The joiner's poll loop below is unchanged: it still matches the
> rendezvous by its own signature key and ignores the rest.

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
   panel. **Unchanged by [ADR-0045](0045-remote-commit-epoch-fork.md) S7 (`576ea14`),
   and that is a decision rather than an oversight.** S7 turned a chat group's
   approve into a proposal swept by an elected committer, but a `PublisherGroup`
   is built by `Group::create_with_id` and seeds **no role table**, so the
   proposal path refuses it by construction — there is no committer to elect.
   `PublisherGroup::approve_external_join` therefore keeps the direct arm and is
   **the only place it survives**: the underlying `Group` method is now
   `pub(crate)`, has one caller, and refuses a group that carries a role table,
   so it cannot become a second ungated route into a group that has a committer.
   The two return types differ — proposal bytes for a chat group, the merged
   Commit + Welcome here — so confusing them is a compile error. The reseal
   below therefore still holds **verbatim**, and it holds *because* the approve
   here still advances the epoch synchronously.
   **Reseal-after-add (load-bearing):** `approveExternalJoin` commits the
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
   UniFFI + wasm). `tsc` clean, web vitest 222 green. **iOS + Android co-publisher
   landed (owner + joiner)** — `apps/ios/Lakko/Channels/CoPublisher.swift` +
   a `CoPublishersPanelView` + a `JoinCoPublisherScreen` route in
   `ChannelsScreen.swift`, and `apps/android/.../channels/CoPublisher.kt` +
   a `CoPublishersPanel` + a `JoinCoPublisherScreen` route in the Compose
   `ChannelsScreen.kt`: the same owner shape (open the persistent
   `PublisherGroup`, mint a `/g/` link over its `group_id`, 5 s DS-journal poll
   via `processIncomingVerifyingRoles`, approve/reject with the load-bearing
   reseal-after-add — the rotation-cron fanout WITHOUT a second `rotate()`) and
   joiner shape (`requestToCoPublish` → recover the channel id via
   `channelIdFromPublisherGroupId`, read the epoch from the journal head,
   external-join proposal + a self-signed OQ2 attribution; `pollForCoPublishWelcome`
   → match the owner's `WelcomeRendezvous` by leaf key, `joinFromWelcome`, then
   `freshPublisherKeys` at the post-approve epoch the subscribers were resealed
   to) as web. Verified iOS `xcodebuild` BUILD SUCCEEDED + Android
   `:app:compileDebugKotlin` BUILD SUCCESSFUL (the native `.so` needs a
   cargo-ndk rebuild for the OQ3 FFI symbols at CI/deploy — local NDK unset).
   **OQ3 is now implemented on all three clients. Residual:** a live 2-publisher
   e2e of the reseal (no deployed channels env, the same gate every other
   live-channel path carries).

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
