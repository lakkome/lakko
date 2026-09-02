<!--
SPDX-License-Identifier: AGPL-3.0-or-later
SPDX-FileCopyrightText: 2026 Lakko Contributors
-->

# ADR-0010: Channels — two-tier PublisherGroup + sender-key broadcast

- **Status:** Accepted ~~(design only — implementation lands in Phase 4)~~ —
  **built** (Sprints A–D, 2026-07). See the amendment below.
- **Date:** 2026-05-21
- **Context:** Phase 4 (channels) — extends ADR-0001 (§ "Channels >50k")

> **AMENDED 2026-09-02 (documentation audit). The architecture below is what
> shipped. The API surface below is not — almost none of the names in
> § "Concrete additions" survived implementation, and this was never recorded.**
>
> **What holds, verified against the tree:** the two-tier split (subscribers are
> not MLS members), the `PublisherGroup` with its deterministic `group_id`, the
> exporter label `"lakko/channel/v1"`, the 32-byte content-key seed, the 16-byte
> `channel_id`, the monotone `u32` `epoch_id`, the per-message key/nonce salt
> binding, the AEAD AAD, Ed25519-signed posts, and retention `N = 4`. The
> forward-secrecy trade-off and the whole Context / Alternatives discussion are
> unchanged.
>
> **What does not hold** — each marked in place below: there is **no `Channel`
> type** and no `Channel::publish/receive/rotate`; the seed envelope is **not
> HPKE**; there is **no `channel_subscribe` module in `lakko-contacts`**; there
> is **no `schemas/channel/v1.proto`**; two of the four named endpoints do not
> exist and the others are prefixed `/v1`; the rotation post-cap is **1 000, not
> 100 000**; and two HKDF `info` labels are misquoted.
>
> **Extended by four later ADRs this document never links to:**
> [ADR-0027](0027-external-join-approval.md) (external-join approval),
> [ADR-0028](0028-group-join-ticket.md) (the join-ticket, whose OQ3 built the
> co-publisher path this ADR left open),
> [ADR-0030](0030-batched-rotation-fanout.md) (staged + atomic rotation fanout,
> which replaced the single-shot rotate for scale), and
> [ADR-0031](0031-multi-epoch-envelope-retention.md) (multi-epoch retention,
> which built the `N = 4` catch-up this ADR specified but the first
> implementation shipped as `N = 1`).
>
> **The residual is unchanged and is the real one:** nothing here has been
> proven at the >50k subscriber scale it was designed for, because that needs a
> deployed environment (G-1).

## Context

[`ADR-0001`](0001-mls-only.md) commits Lakko to MLS (RFC 9420) for
**every** confidential surface: 1×1 chats, groups, call media keys,
DeviceGroups. Phase 4 introduces **channels** — broadcast-style
surfaces (think Telegram channels) where one or a few publishers
write to a subscriber set that can reach **hundreds of thousands to
millions** of devices. Vanilla MLS does not scale to that membership
size: the ratchet tree is O(N), every Add/Remove triggers a commit
that fans out to N − 1 members, and a million-leaf tree is both an
attack surface and an unwieldy operational object.

Forces:

1. **Cryptographic floor must hold.** ADR-0001 invariant #4 — "no
   silent fallback to non-E2EE" — applies here too. The channel
   content the user sees on their device must be authenticated and
   confidential against the server even when the subscriber base is
   2 ⨯ 10⁶.

2. **Backward secrecy is a non-feature for broadcast.** Anyone who
   has been a channel subscriber at time T can record the
   ciphertext stream they receive at T; revoking their access at
   T+1 cannot retroactively re-encrypt the bytes already on their
   disk. This is the same property Telegram, Slack, Discord
   broadcast surfaces have — the trade-off is well understood by
   users and isn't a regression.

3. **Forward secrecy still matters within the active publisher
   set.** When a publisher's device is compromised, the recovered
   keys must not let the attacker decrypt the channel's **future**
   content. This is what MLS gives for free — we keep it for the
   publisher side.

4. **Operational simplicity.** A 100k-subscriber channel that
   requires 100k server-side ratchet-tree operations every time
   the owner adds a moderator is operationally indefensible. The
   design must keep server-side per-subscribe cost O(1) for joins
   and O(N) only during the rare content-key rotations.

5. **Anti-abuse asymmetry.** Subscribers vastly outnumber
   publishers; the abuse surfaces are different. Subscribe rate
   limiting + spam detection on the subscriber side is loose
   (every device wants to subscribe to every channel it sees);
   publisher rate limiting is tight (a publisher posting to 10⁶
   devices is a megaphone with non-trivial network cost).

## Decision

**Two-tier architecture**, scoped per channel:

### Tier 1 — PublisherGroup (vanilla MLS, ≤ 500 members)

Every channel has exactly one **PublisherGroup** — a vanilla MLS
group whose members are the owner + appointed moderators +
explicitly-trusted publishers. This group's purpose is to hold the
**current content key** and rotate it under standard MLS membership
churn semantics.

- **Membership ceiling: 500.** Picked empirically to keep ratchet
  tree size + commit fanout cheap; larger publisher sets are
  vanishingly rare in practice (broadcast channels almost always
  have ≤ 50 active publishers).
- **Cipher suite**: same as 1×1 / group chats (~~`MLS_256_DHKEMX25519_
  CHACHA20POLY1305_SHA256_Ed25519`~~ **`MLS_128_DHKEMX25519_CHACHA20POLY1305_SHA256_Ed25519`**
  fast path, `MLS_256_DHKEMP384_AES256GCM_SHA384_P384` FIPS path).
  *(Corrected 2026-09-02 — the `MLS_256` X25519 suite is not registered in
  RFC 9420; this ADR inherited the naming error from
  [ADR-0001](0001-mls-only.md), where it is explained. Same suite, right name.)*
- **Forward secrecy**: full MLS — each Add/Remove/Update commit
  produces a new epoch with a fresh root key; a compromised
  publisher device cannot decrypt future content after the next
  commit.

### Tier 2 — Sender-key broadcast for subscribers

Subscribers are **NOT cryptographic members** of the PublisherGroup.
They do not appear in the ratchet tree. The PublisherGroup exports
a 32-byte **content key seed** via the MLS exporter

```
content_key_seed = mls_exporter("lakko/channel/v1", channel_id, 32)
```

The seed is delivered to every subscriber via an ~~HPKE~~-sealed envelope
to that subscriber's identity X25519 public key:

> **Not HPKE (corrected 2026-09-02).** What shipped is a hand-rolled ECIES —
> ephemeral X25519 → HKDF-SHA-256 → ChaCha20-Poly1305, wire
> `eph_pub(32) || ct || tag`, labels
> `"lakko/channel/v1/seed-envelope/{key,nonce}"`
> ([`subscribe.rs`](../../crates/lakko-channels/src/subscribe.rs)). Sound and
> domain-separated, but not RFC 9180 and not interoperable with it. The
> pseudocode below is therefore illustrative of the *shape*, not of the
> primitive or the `info` string. ADR-0030 and ADR-0031 inherited the "HPKE"
> label from here.

```
envelope = HPKE_seal(
    subscriber_x25519_pubkey,
    info     = "lakko/channel/v1/key" || channel_id || epoch_id,
    aad      = channel_id || epoch_id,
    plaintext = content_key_seed,
)
```

Channel content (text post, attachment ciphertext, media frames) is
encrypted under a **per-message AEAD** keyed off the seed:

```
message_key   = HKDF-SHA-256(content_key_seed,
                             info  = "lakko/channel/v1/msg-key",   # ADR said ".../msg"
                             salt  = channel_id || epoch_id || sender_pubkey,
                             okm_len = 32)
message_nonce = HKDF-SHA-256(content_key_seed,
                             info  = "lakko/channel/v1/msg-nonce", # ADR said ".../nonce"
                             salt  = channel_id || epoch_id || sender_pubkey
                                     || message_counter_be,
                             okm_len = 12)
ciphertext    = ChaCha20-Poly1305-Encrypt(
                    key   = message_key,
                    nonce = message_nonce,
                    aad   = channel_id || epoch_id || sender_pubkey
                            || message_counter_be,
                    pt    = post_plaintext,
                )
```

The salt binds each message to its sender's pubkey, so a member
who knows the seed cannot impersonate a different publisher
without a separate Ed25519 signature on the post body
(which we add — see "Authenticity" below).

### Epoch boundaries — when the seed rotates

A new `epoch_id` (monotone u32 counter) is emitted when:

- Any PublisherGroup membership change (Add, Remove, Update) merges
  a commit. The new MLS group epoch derives a new `content_key_seed`
  via the exporter.
- A scheduled rotation fires: every 24h **OR** every ~~100k~~ **1 000**
  posts, whichever first. *(Corrected 2026-09-02 — `DEFAULT_MAX_POSTS = 1000`
  in [`rotation_policy.rs`](../../crates/lakko-channels/src/rotation_policy.rs).
  The shipped cap is 100x tighter than this ADR specified, i.e. it rotates far
  MORE often — the safe direction, and the code believes it is following this
  ADR. The 24 h half matches.)*
- The owner manually triggers a rotation (UI: "force a rotation").

On a rotation:

1. Publisher group commits the membership change (or a no-op
   self-update if the rotation is scheduled).
2. New `content_key_seed` is derived from the new epoch's exporter.
3. Each current subscriber receives a fresh
   `HPKE_seal(subscriber_pubkey, info, aad, new_seed)` envelope.
   The envelope set is the only O(N) operation in the system; the
   server fanout endpoint streams these to subscriber mailboxes
   without ever seeing a plaintext.
4. Posts published under the previous epoch remain decryptable by
   anyone who still has the previous seed (backward secrecy
   trade-off documented above).

### Authenticity — Ed25519 signature on every post

Every channel post is **Ed25519-signed** by the publishing
identity's MLS leaf credential. The signature covers
`(channel_id || epoch_id || sender_pubkey || message_counter ||
ciphertext)`. The signed envelope is what lands in the subscriber's
mailbox.

This is what lets a subscriber distinguish "post from owner Alice"
from "post from moderator Bob" even though both share the same
content key — they have different signing identities. It also
defends against a compromised subscriber who learns the seed
trying to forge a post: forging requires the publisher's
identity Ed25519 private key, which never leaves the publisher
device.

## Trade-offs

### Backward secrecy is degraded for channel content

Once `content_key_seed_epoch_N` has reached a subscriber, revoking
that subscriber AT epoch N+1 cannot retroactively prevent them
from decrypting messages they already received under epoch N. We
mitigate by:

- Rotating seeds aggressively (24h cap even on quiet channels;
  ~~100k~~ **1 000** posts cap even on busy ones — see the correction above).
- Documenting this publicly in the privacy policy: "a channel
  subscriber's revocation is enforced going forward only".
- Offering the publisher an "emergency rotation" UI affordance.

This is **the same trade-off** as Telegram channels, Slack
public channels, Discord broadcast servers — none of which have
backward secrecy. The audit pack explicitly lists this as an
accepted protocol limitation (G-N tracked in
[`known-gaps.md`](../security/known-gaps.md) when Phase 4 lands).

### Forward secrecy holds for publishers only

A subscriber whose key is compromised at T leaks the seeds they've
received but cannot derive future seeds (those are fresh from the
MLS exporter for a group they're not a member of). A publisher
whose key is compromised at T leaks the current PublisherGroup
state and therefore both the current seed AND the ability to read
future content — until the publisher is removed and the next
commit ratchets the group forward.

### Server can correlate subscriber → channel via subscription mailbox

The fanout endpoint that delivers HPKE-sealed seed envelopes to
subscriber mailboxes IS server-observable. The server sees
`(subscriber_pseudonym, channel_id_pseudonym, envelope)` tuples.
Mitigations:

- The subscriber pseudonym is **per-channel** — a long-lived
  random ID derived from the subscriber's identity seed +
  channel-specific salt. Two channels see two unrelated
  pseudonyms for the same subscriber. Server cannot correlate
  "Alice subscribes to channel X AND channel Y" without
  additional cross-channel side-channels.
- The channel_id is also a pseudonym, not a human-readable
  channel name. Discovery (looking up a channel by `@channelname`)
  uses the OPRF lookup path from ADR-0005 §13.2 — the server
  never sees the channel name in plaintext.
- Server retention: only the most-recent N seed envelopes per
  subscriber are kept (default N = 4, covering ~96h at the
  24h rotation cadence). A subscriber who's offline for a week
  catches up via a re-subscribe handshake that re-seeds them.

## Wire surfaces

Concrete additions Phase 4 lands across the codebase:

> **This whole section is the plan, not the build (marked 2026-09-02).** It
> shipped with different names and a different decomposition; the table below is
> the map from what is written here to what exists. The design *above* this
> section is what to trust; this section is what to translate.
>
> | Planned here | What was built |
> |---|---|
> | `Channel` handle + `publish` / `receive` / `rotate` | **No `Channel` type at all.** `crates/lakko-channels` is a stateless codec of free functions — `compose_post` / `decode_post` / `verify_and_decrypt` (`post.rs`) plus `RotationPolicy` / `RotationState` / `should_rotate` (`rotation_policy.rs`). The stateful handle is `lakko_mls::PublisherGroup`; the rotation *driver* is a per-client `RotationCron` (web/iOS/Android), deliberately outside the crate so the cadence decision stays one pure shared function. `ChannelPost` is the one planned name that survived. |
> | `lakko-contacts::channel_subscribe` | `crates/lakko-channels/src/subscribe.rs` — different crate, and `seal_seed` / `open_seed` rather than a `subscribe` request composer. |
> | `POST /channels/:id/posts`, `GET …/posts` | Exist, prefixed **`/v1`**. |
> | `POST /channels/:id/subscribe` | Renamed **`POST /v1/channels/{id}/subscribers`**. |
> | `POST /channels/:id/seeds/distribute` | **Does not exist.** Superseded by the rotation endpoints: `/rotate` and — per [ADR-0030](0030-batched-rotation-fanout.md) — `/rotate/stage` + `/rotate/commit`. |
> | (unplanned) | `POST /v1/channels` (create), `/epoch`, `/subscribers/{pk}/envelope`, and — per [ADR-0031](0031-multi-epoch-envelope-retention.md) — `/subscribers/{pk}/envelopes`. |
>
> **The seed envelope is not HPKE.** This ADR (and, inheriting the phrase,
> ADR-0030 and ADR-0031) calls it "HPKE-sealed". It is a hand-rolled ECIES:
> ephemeral X25519 `diffie_hellman` → HKDF-SHA-256 → ChaCha20-Poly1305, on the
> wire as `eph_pub(32) || ct || tag`
> ([`subscribe.rs`](../../crates/lakko-channels/src/subscribe.rs)). The
> construction is sound and domain-separated, but it is **not** RFC 9180 HPKE
> and does not interoperate with one, so the label should not be read as a
> statement about the primitive. Its HKDF labels are
> `"lakko/channel/v1/seed-envelope/{key,nonce}"`, not the
> `"lakko/channel/v1/key"` quoted earlier in this ADR.

### `crates/lakko-channels` (new)

- `Channel` — handle wrapping a PublisherGroup + cached current
  `(epoch_id, content_key_seed)`.
- `Channel::publish(text, attachments) -> ChannelPost` — composes
  a signed encrypted post.
- `Channel::receive(envelope) -> Post` — verifies signature,
  decrypts, returns plaintext.
- `Channel::rotate() -> RotationResult` — drives the scheduled /
  manual rotation. Produces a fresh seed + the envelope set to
  re-seed all subscribers.

### `crates/lakko-contacts::channel_subscribe` (new module)

- `subscribe(channel_id, subscriber_x25519_pubkey,
  one_time_kp)` — composes an HPKE-sealed subscribe request.
  The publisher group's owner unwraps it and emits the first
  seed envelope back.

### `services/channels` (new service)

- `POST /channels/:id/posts` — publisher uploads a signed
  ciphertext envelope.
- `GET /channels/:id/posts?after=<cursor>` — subscriber long-
  polls. Returns signed envelopes; client does the rest locally.
- `POST /channels/:id/subscribe` — anonymous + PoW-gated. Posts
  the HPKE-sealed subscribe request into the publisher
  inbox.
- `POST /channels/:id/seeds/distribute` — publisher-only. Posts
  a batch of HPKE-sealed seed envelopes for the fanout layer to
  deliver to subscriber mailboxes.

### ~~`schemas/channel/v1.proto` (new)~~ — NOT BUILT

> **Corrected 2026-09-02.** No channel protobuf exists; `schemas/` holds only
> `media/`, `mls-app/` and `wire/`. `ChannelPost` is a **hand-written
> big-endian binary codec** (`ChannelPost::to_bytes` / `decode_post` in
> [`post.rs`](../../crates/lakko-channels/src/post.rs)), and its `post_decode`
> fuzz target fuzzes that codec, not a prost decoder. The field set below is
> nonetheless accurate field-for-field — only the encoding differs. One thing
> the schema below omits that the code does: the post signature covers a
> domain separator, `"lakko/channel/v1/post" || 0x00 || …`, which is a
> strengthening this ADR under-specified.

```protobuf
message ChannelPost {
  uint32 epoch_id = 1;
  bytes  sender_pubkey = 2;             // 32 bytes Ed25519
  uint64 message_counter = 3;           // monotone per (channel_id, epoch_id, sender)
  bytes  ciphertext = 4;
  bytes  signature = 5;                 // 64 bytes Ed25519 over
                                        // channel_id || epoch_id || sender_pubkey
                                        // || message_counter || ciphertext
}

message ChannelSeedEnvelope {
  uint32 epoch_id = 1;
  bytes  hpke_kem_output = 2;           // 32 bytes X25519
  bytes  hpke_ciphertext = 3;           // seed + 16-byte Poly1305 tag
  bytes  channel_id = 4;                // 16 bytes — the channel pseudonym
}
```

## Phased rollout

Phase 4 ships in three sub-sprints:

- **4a — Publisher path.** Owner can create a channel, generate the
  initial PublisherGroup (just themselves), publish encrypted posts.
  No subscribers yet — the publisher signs+encrypts to their own
  mailbox for verification.
- **4b — Subscriber path.** `subscribe(channel_id)` flow. The owner
  receives subscribe requests, decides accept/reject, emits the
  initial seed envelope. Subscribers receive posts.
- **4c — Rotation + scale-out.** Scheduled + manual rotation. Fanout
  service. Audit pack adds C-N invariants pinning the rotation
  cadence + seed envelope wire shape. Re-subscribe handshake for
  catch-up after extended offline periods.

## Open questions

These were considered during the design pass and explicitly
deferred to implementation-time decisions:

1. **Initial subscriber rate limit.** PoW difficulty + Privacy Pass
   token cost for subscribe is TBD; depends on Phase 4 operational
   load testing. Default: same difficulty as ContactRequest cold-
   start (ADR-0005 §13.9).
2. **Subscriber unsubscribe acks.** A subscriber wishing to leave
   either drops the channel client-side (server still considers
   them subscribed but they ignore inbound seeds) OR explicitly
   posts an unsubscribe message. The latter is cleaner for the
   publisher's UI ("see who left") but lights up a server-
   observable signal. Phase 4b decides which.
3. **Channel ownership transfer.** ADR-0007 covers individual
   identity rotation; channel ownership is a separate concept
   (the channel's owner can resign and pass the PublisherGroup
   to a moderator). ~~Sketched out as a future ADR-0011~~ — **that number was
   taken by [ADR-0011](0011-rust-sfu.md) (the Rust SFU); channel ownership
   transfer still has no ADR and is unbuilt** (corrected 2026-09-02). Note
   *group* ownership transfer was built under
   [ADR-0045](0045-remote-commit-epoch-fork.md) / ADR-0022, and a
   `PublisherGroup` is a thin wrapper over a `Group` — so the mechanism now
   exists even though no channel-level decision or affordance does.
4. **Replies / threaded comments.** Out of scope for Phase 4.
   Either layered on top (each top-level post spawns a per-post
   sub-channel) or punted to Phase 5.

## Cross-links

- [`ADR-0001`](0001-mls-only.md) — MLS as the substrate. This ADR
  is the formal expansion of ADR-0001's "Channels > 50k" line.
- [`ADR-0005`](0005-contact-invitations.md) — the OPRF lookup +
  HPKE envelope patterns this ADR reuses for channel discovery and
  subscribe.
- [`docs/security/calls.md`](../security/calls.md) §"Exporter
  labels" — the MLS exporter contract this ADR extends with
  `"lakko/channel/v1"`.
- [`ROADMAP.md`](../ROADMAP.md) Phase 4 — implementation
  decomposition (4a / 4b / 4c sub-sprints).
- **The four ADRs that extend this one** (added 2026-09-02 — the links were
  one-directional; all four cite this ADR and none was cited back):
  [ADR-0027](0027-external-join-approval.md) — external-join approval, the
  prerequisite for adding a publisher;
  [ADR-0028](0028-group-join-ticket.md) — the group join-ticket, whose OQ3
  reuses it verbatim to add a **co-publisher** to a `PublisherGroup`;
  [ADR-0030](0030-batched-rotation-fanout.md) — staged + atomic rotation
  fanout, which replaced the single-shot rotate this ADR implies at 50k-1M
  subscriber scale;
  [ADR-0031](0031-multi-epoch-envelope-retention.md) — multi-epoch retention,
  which built the `N = 4` catch-up specified here after the first
  implementation shipped `N = 1`.
- [`~/.claude/plans/1-nested-wreath.md`](file:///Users/valentingoncharov/.claude/plans/1-nested-wreath.md)
  §3 "Crypto model" — the strategic plan this ADR derives from.
