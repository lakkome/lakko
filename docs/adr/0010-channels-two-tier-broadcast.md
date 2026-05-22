<!--
SPDX-License-Identifier: AGPL-3.0-or-later
SPDX-FileCopyrightText: 2026 Lakko Contributors
-->

# ADR-0010: Channels — two-tier PublisherGroup + sender-key broadcast

- **Status:** Accepted (design only — implementation lands in Phase 4)
- **Date:** 2026-05-21
- **Context:** Phase 4 (channels) — extends ADR-0001 (§ "Channels >50k")

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
- **Cipher suite**: same as 1×1 / group chats (`MLS_256_DHKEMX25519_
  CHACHA20POLY1305_SHA256_Ed25519` fast path,
  `MLS_256_DHKEMP384_AES256GCM_SHA384_P384` FIPS path).
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

The seed is delivered to every subscriber via an HPKE-sealed envelope
to that subscriber's identity X25519 public key:

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
                             info  = "lakko/channel/v1/msg",
                             salt  = channel_id || epoch_id || sender_pubkey,
                             okm_len = 32)
message_nonce = HKDF-SHA-256(content_key_seed,
                             info  = "lakko/channel/v1/nonce",
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
- A scheduled rotation fires: every 24h **OR** every 100k posts,
  whichever first.
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
  100k posts cap even on busy ones).
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

### `schemas/channel/v1.proto` (new)

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
   to a moderator). Sketched out as a future ADR-0011 if the
   need surfaces in Phase 4 usability testing.
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
- [`~/.claude/plans/1-nested-wreath.md`](file:///Users/valentingoncharov/.claude/plans/1-nested-wreath.md)
  §3 "Crypto model" — the strategic plan this ADR derives from.
