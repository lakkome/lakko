<!--
SPDX-License-Identifier: AGPL-3.0-or-later
SPDX-FileCopyrightText: 2026 Lakko Contributors
-->

# ADR-0005: Capability-based contact invitations + 6 scenarios + pseudonym lifecycle

- **Status:** Accepted — **the model shipped; most of the concrete struct
  definitions below did not survive implementation** and are marked in place
  (documentation audit, 2026-09-02).
- **Date:** 2026-04-28
- **Context:** Phase 0

> **AMENDED 2026-09-02 (documentation audit).** The decisions here — invites as
> signed capabilities, pseudonymous routing, HPKE-sealed envelopes, PoW +
> Privacy Pass gating, no address-book upload, no global presence — are what
> Lakko built, and the six-scenario framing is still the right map. **The
> `struct` blocks are not the shipped types.** Each is marked where it appears;
> the ones worth knowing before you read on:
>
> - **`InviteCapability`** carries the owner's **full pubkey**, not a hash, and
>   has neither a KT proof nor a Bloom counter.
> - **`QrPayload`** does not exist. Scenario D shipped as *verify*, not *add*.
> - **`GroupInvite`** became `GroupInviteCapability` with a **joiner-initiated,
>   admin-approved** accept model — the inversion that makes a leaked link
>   harmless ([ADR-0028](0028-group-join-ticket.md) /
>   [ADR-0027](0027-external-join-approval.md)).
> - The **fourth pseudonym type** and the **`InviteOnly` discoverability
>   default** were never built; the latter is listed here as a *settled
>   trade-off*, which makes it the most misleading line in the document.
>
> `ContactRequest` / `ContactAck` / the three pseudonym types / safety numbers /
> `compose` / `decode_and_verify` all exist as named.

## Context

Discovery in Lakko works only by `@username` — phone / email are never stored (see ADR-0001). The social graph is **not materialised** on the server: the server routes ContactRequests by encrypted envelope without seeing the identity of either sender or recipient.

We need a complete invitation scheme that covers six scenarios:

| # | Scenario | Initiator |
|---|---|---|
| A | I know `@username` — want to add | Initiator |
| B | Hand someone a "card" of myself | Owner |
| C | Invite a **new** user into Lakko | Owner |
| D | In-person meeting (in-person scan) | Both |
| E | Invite into a **group** | Group admin |
| F | Subscribe to a **channel** | Subscriber |

While preserving the anti-stalking, anti-spam and privacy invariants.

## Decision

The full specification lives in `~/.claude/plans/1-nested-wreath.md` §13 ("Contact Invitations"). This ADR captures the key architectural decisions.

### Base ContactRequest flow (scenario A)

```
Alice → OPRF(@bob) → discovery service → KeyPackageBundle(Bob) + KT proof
Alice builds ContactRequest {
    from_pubkey, from_pseudonym (fresh per recipient),
    HPKE_seal(B_pubkey, intro_message),
    PoW (Equix), Privacy Pass token,
    signature
}
→ DS into Bob's stranger-inbox mailbox.
Bob: pending UI → Accept | Decline | Block | Ignore
On Accept: both sides open an MLS group of 2.
```

The server only sees `(pseudonym_A → pseudonym_B, opaque_envelope, PoW_token)`.

### Capability-based invite links (scenarios B/C)

URL: ~~`https://lakko.app/i/<base64url-pointer>`~~ **`https://lakko.me/i/<base64url>`** — ~~only a pointer + MAC are in the URL; the **capability itself** is an encrypted blob in SeaweedFS~~:

> **The struct below is the design sketch, not the shipped type (marked
> 2026-09-02).** `crates/lakko-contacts/src/invite.rs` documents every
> divergence honestly in its module header; it is this ADR that was never
> updated. The differences that change how you should read the design:
>
> | Sketched here | Shipped `InviteCapability` |
> |---|---|
> | `owner_identity_hash: [u8; 32]` (BLAKE3 of the pubkey) | `owner_signature_pubkey: VerifyingKey` — **the full pubkey**, not a hash, so the link is self-verifying without a lookup |
> | `owner_transparency_proof: KTLogProof` | **absent** — `KTLogProof` exists nowhere; KT verification happens on the identity bundle, not in the link |
> | `ephemeral_prekey: HpkePublicKey` | `owner_x25519_pubkey` — **long-term**, not ephemeral |
> | `blob_id: Sha256` | **not a struct field** — the blob indirection is a server-side transport choice, and the codec emits the full capability |
> | `consumed_pubkey_log: HmacBloom` | **absent** — `HmacBloom` exists nowhere. Server-blind accounting is `compute_joiner_tag` + an atomic consume, which is **stronger**: `max_uses` is server-enforced with per-joiner dedup rather than a probabilistic counter |
> | `bootstrap_referral: Option<bool>` | `bootstrap_referral: bool` |
> | `signature: Ed25519Sig` | not a field — the signature is a trailing element of the wire encoding |
> | — | `label: String`, added |
>
> **Three more corrections in the sentence above.** The host is **`lakko.me`**,
> not `lakko.app`. **SeaweedFS is not used** — invite blobs live in the
> discovery service's SQL store (`services/discovery/src/invite.rs`, migration
> `0004_invites.sql`); the only SeaweedFS mention in the tree is an aspirational
> future-migration comment in `services/storage`. And the URL carries **the
> capability bytes, not "a pointer + MAC"** — integrity comes from the trailing
> Ed25519 signature; the short blob-pointer form is an *optional* transport
> that ADR-0028 later reused for group invites.

```rust
struct InviteCapability {
    owner_identity_hash:        [u8; 32],          // BLAKE3(A_pubkey)
    owner_transparency_proof:   KTLogProof,
    ephemeral_prekey:           HpkePublicKey,
    blob_id:                    Sha256,
    expiry:                     UnixTimestamp,
    max_uses:                   u32,
    consumed_pubkey_log:        HmacBloom,         // server-side counter, ciphertext-blind
    bootstrap_referral:         Option<bool>,       // C: signup deep-link
    signature:                  Ed25519Sig,
}
```

Benefits: revocable (delete the blob), a screenshot of the URL does not leak identity, the server counts downloads without knowing the content.

UI variants of a link:
- "Single-use, 7 days" — default
- "Up to 10 uses, 7 days" — for teams
- "Up to 30 days" — for a social-media bio
- "Permanent" — locked behind a verified account (anti-stalking)

### QR / safety numbers (scenario D)

```rust
struct QrPayload {
    identity_fingerprint:   [u8; 32],     // BLAKE3(pubkey)
    one_time_prekey:        HpkePublicKey,
    transparency_position:  u64,
    signature:              Ed25519Sig,
}
```

Dual purpose: add a new contact OOB-secure + verify an existing contact (Signal-style safety numbers).

> **Half built (marked 2026-09-02). `QrPayload` does not exist** — nor do
> `identity_fingerprint`, `one_time_prekey` or `transparency_position`, in any
> form, anywhere in the tree. **The verify half shipped**: safety numbers are
> `safety_number` / `safety_number_bytes` in
> [`safety.rs`](../../crates/lakko-contacts/src/safety.rs), and comparing them
> out of band is the working MITM check. **The add-a-new-contact-by-QR half did
> not**, and `safety.rs` records why: it is gated on a per-platform
> camera-permission UX sprint. So scenario D today means "verify a contact you
> already have", not "add one".

### Group invite (scenario E)

```rust
struct GroupInvite {
    group_id_pseudonym,
    group_name_ciphertext,            // HPKE_seal with invite-key
    welcome_blob_id,                   // ephemeral KeyPackage from the admin
    inviter_signature,
    expiry, max_uses,
}
```

On accept: the joiner downloads the Welcome → MLS Add proposal → the joiner becomes a leaf.

> **Built as something meaningfully different — see
> [ADR-0028](0028-group-join-ticket.md) (marked 2026-09-02).** The shipped type
> is `GroupInviteCapability`
> ([`group_invite.rs`](../../crates/lakko-contacts/src/group_invite.rs)) with
> **three** fields: `group_id` (the opaque MLS `GroupId` itself, **not** a
> pseudonym), `issuer_signature_pubkey`, `expiry_ms`. No
> `group_name_ciphertext`, no `welcome_blob_id`, no `max_uses` in the signed
> capability — the last is a deliberate split, since a `max_uses` inside the
> signature could not be tightened after minting, so it became a **soft
> server-side ceiling** instead.
>
> **The accept model inverted, and that is the substantive change.** Here the
> admin pre-places a Welcome the joiner downloads. What shipped is
> **joiner-initiated**: the joiner reads the group's current epoch from the
> journal's public framing, crafts an MLS **external-join proposal**, and an
> admin approves it ([ADR-0027](0027-external-join-approval.md)) — so **a
> leaked link lets a stranger *ask*, never *in***, and the security boundary is
> the admin gate rather than the secrecy of the link. That property is why
> ADR-0028 could make the link short and public.

### Pseudonym lifecycle

| Type | Lifecycle | Purpose |
|---|---|---|
| Identity pubkey | Permanent | Primary key, recorded in KT log; rotation via supersession proof |
| Per-relationship pseudonym | Lifetime of the contact | Used by the server for routing; rotates on demand |
| Stranger-inbox pseudonym | Long-lived, optionally rotatable | Receives cold ContactRequests; rotation tears down undelivered invites |
| ~~Per-invite-link pseudonym~~ | ~~Lifetime of the capability~~ | ~~Tied to blob_id~~ — **never built** (2026-09-02). [`pseudonym.rs`](../../crates/lakko-contacts/src/pseudonym.rs) defines exactly the three types above. Server-blind per-joiner accounting on an invite is `compute_joiner_tag`, which is a derived tag rather than a routable pseudonym. |

### Settled trade-offs

| Question | Decision |
|---|---|
| Cancel pending ContactRequest | Allowed before accept (client "delete-for-everyone" on pending) |
| Default discoverability scope for new users | ~~**InviteOnly** — secure by default~~ — **not implemented** (2026-09-02). No discoverability scope enum, column or setting exists in `crates/`, `services/` or any client; `ContactsOfContacts`, referenced later in this ADR, likewise does not exist. Presented here as settled, it is in fact unbuilt — and the practical default today is whatever username lookup permits, which is **not** InviteOnly. |
| Global "online" / "last seen" | **None.** Presence only inside MLS groups via blinded tokens |
| Auto-import of the device address book | **Forbidden.** The address book = PII; never uploaded |
| Contact subgroups ("colleagues", "family") | DeviceGroup-encrypted list (server-blind) |
| Identity key compromise / rotation | KT log supersession proof + 30-day bridge for old invites |

### Anti-abuse pipeline

- Cold-contact rate per sender: ≤5/day for new accounts, ≤50/day for warmed-up; PoW difficulty grows exponentially
- Privacy Pass token (App Attest / Play Integrity) on every ContactRequest and every InviteCapability generation
- Receiver-side priority binning: strangers go into a low-priority inbox without push
- Per-recipient silent mute (the sender is not aware of the block)
- Mass-complaint signal: N complaints → the server raises PoW for outgoing requests from that pseudonym

## Alternatives

### Phone-number-based discovery (à la Signal / WhatsApp)

Phone is a strong identity and gives a network effect via automatic contact resolution. Cons: PII on the server, easy linkage to a real identity, social-graph reconstruction via intersection attacks, requires a SIM card (a problem in RU / IR / CN).

**Rejected:** direct violation of Lakko's security invariants (no PII).

### Username + phone-hash for recovery

Hybrid: a phone hash is stored for recovery. Cons: hashes are easy to brute force (10^9 numbers), the social graph is reconstructible via intersection.

**Rejected:** hashing phone numbers is not a security primitive — it is security theatre.

### Public key as identity (DID-style)

Identity = pubkey, no usernames. Pros: zero PII. Cons: discovery UX is terrible (a 32-byte hash instead of @alice).

**Rejected:** a username handle matters for UX; we keep a username table with OPRF lookup (ADR-0001 + this ADR).

## Consequences

### Positive

- The server only sees pseudonym → pseudonym routing; the social graph cannot be reconstructed even on full compromise
- Six invitation scenarios cover every realistic UX case
- Capability-based links are revocable and rate-limited
- Anti-abuse is multi-layered (PoW + Privacy Pass + complaint signal)

### Negative

- Cold-contact UX is slower than "phone autofind" — the user has to know the `@username`
- Stranger-inbox pseudonym rotation tears down undelivered invites — fair trade-off
- "InviteOnly by default" can feel like "no one can find me" — needs an onboarding hint

### Neutral

- KT log inclusion proofs on every KeyPackage fetch — modest network overhead (~150–300 bytes per lookup)

## Open questions

- How to implement a **mutual-contact attestation chain** for the `ContactsOfContacts` scope without leaking transitive contacts to the server? Cryptographic proof + client-side verification — details in Phase 2
- Cross-device sync of the list of sent ContactRequests via DeviceGroup — Phase 2
- Spam detection on the mass-complaint signal without identity knowledge — risk: targeted brigading. Mitigate via proof-of-personhood (mature WebAuthn accounts)?

## References

- [RFC 9497 — Oblivious Pseudorandom Functions (OPRF)](https://www.rfc-editor.org/rfc/rfc9497)
- [RFC 9576 — Privacy Pass](https://www.rfc-editor.org/rfc/rfc9576)
- [draft-ietf-keytrans-architecture](https://datatracker.ietf.org/doc/draft-ietf-keytrans-architecture/)
- [Equix proof-of-work (Tor onion services)](https://gitlab.torproject.org/tpo/core/tor/-/issues/40729)
- ADR-0001 — MLS / OPRF discovery
- [ADR-0009](0009-link-based-call-invitations.md) — the call-invite link, built
  on this ADR's capability codec (added 2026-09-02)
- [ADR-0027](0027-external-join-approval.md) +
  [ADR-0028](0028-group-join-ticket.md) — what scenario E actually became
  (added 2026-09-02)
- Full specification — `~/.claude/plans/1-nested-wreath.md` §13 (outside the
  repository; nothing sourced only from it could be verified in this audit)
