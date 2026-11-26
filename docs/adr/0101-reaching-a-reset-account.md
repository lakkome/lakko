<!--
SPDX-License-Identifier: AGPL-3.0-or-later
SPDX-FileCopyrightText: 2026 Lakko Contributors
-->

# ADR-0101: Reaching an account after a key reset

- **Status:** Accepted (maintainer, 2026-11-25: everything returns by itself after a reset; chats, groups, channels, the profile and the history)
- **Date:** 2026-11-25
- **Context:** ADR-0073 (key reset, silent continuity), ADR-0078 (servers cannot link accounts), base scenario 4b
- **Amends:** ADR-0073 R.4, ADR-0078 section 1 (the continuity carrier left open there)
- **Amended 2026-11-26 (maintainer; designed, not built):** section 7
  "Owner" rewritten (owned channels come back from a list under two locks,
  one opened by the password, a server half and a second factor, one by the
  account keys, with no subscriber or publisher taking part; open points
  settled the same day); the build plan for
  that and for the two decisions taken with it (a second factor at sign-up,
  ADR-0075 amendment; a second factor for the reset, ADR-0073 amendment) is
  "Plan amended 2026-11-26" below.

## Context

After an ADR-0073 key reset the person's devices hold a new identity with a
new account seed, and with it a new mailbox. ADR-0073 R.4 let peers reach it
with a continuity reach key derived from `(account_id, key_generation)`,
registered on a mailbox whose address peers could compute from the account.
ADR-0078 section 1 made a mailbox address `BLAKE3-derive-key(mailbox_pk)`
with the mailbox key derived from the account seed, so that no server copy
joins an account to its mailbox. Since then a peer cannot compute the new
address, the continuity Welcome is a recorded no-op, and ADR-0078 left the
carrier as an open question.

Seen on 2026-11-25 (base scenario 4b on test.lakko.me): an account reset its
keys on web; its contact's device accepted the reset and found no way to
deliver the successor's Welcome. The reset screen says "Your contacts keep
their chats with you"; nothing came back. The order defects found beside it
(lineage recorded before delivery, an undelivered Welcome marked delivered)
are fixed separately on all three clients; this ADR is about the missing
carrier.

The constraint that shapes every option: a peer and the reset identity share
no secret. The person lost everything (ADR-0073 is the lost-everything path),
so the new identity knows none of its contacts, and a peer knows only what it
knew before plus what the directory serves. The operator knows the same
public facts (the account id, the attested new key from the KT log, the
generation). **Any address a peer can find without a shared secret, the
operator can find too.** The design question is therefore not whether the
operator can link the account to the place peers write, but what that place
reveals and for how long.

## Decision

Proposed (Variant A below): **a continuity mailbox, separate from the real
one, announced in the attested identity bundle for a window.**

1. **Key.** The resetting device derives a second mailbox key from the new
   account seed:
   `continuity_mailbox_seed = BLAKE3-derive-key("lakko/mailbox/continuity/v1", account_seed)`.
   Its address is `mailbox_address(continuity_mailbox_pk)` as for any
   mailbox. It is never the real mailbox and never shares a key with it.
2. **Announcement.** The reset's client order (after the attested publish)
   uploads a continuity binding to discovery beside the identity bundle:
   `version ‖ continuity_mailbox_pk(32) ‖ expires_at_ms(8 BE) ‖ sig(64)`, the
   signature by the new identity key over a domain of its own. Discovery
   stores it opaque, serves it on `GET /discovery/identity/{account_id}` while
   unexpired, and deletes it at expiry (a sweep). The window is the DS
   journal retention, 30 days today (ADR-0073 D2; chosen, not measured).
3. **Gate.** The resetting device claims the continuity mailbox as any
   mailbox and registers on it only the ADR-0073 R.4 continuity reach key
   (`continuity_reach_seed(account_id, key_generation)`), already in
   `lakko-contacts`. The DS learns nothing new: an ordinary mailbox with one
   registered reach key.
4. **Peer side.** A peer that accepted the reset (ADR-0073 R.2 verdict)
   verifies the binding under the attested new key and its expiry, then
   writes the successor Welcome (and a contact request where that is the
   path) to the continuity mailbox, signed with the continuity reach key, as
   an ordinary sealed mailbox row (ADR-0078 phase 1), spending a
   `mailbox-write` token. `newIdentityReachable` (the gate the clients now
   have) is true when the peer holds an ordinary grant for the new identity
   or a verified, unexpired continuity binding.
5. **Reset side.** For the window, the reset identity's devices read the
   continuity mailbox beside the real one, with the same mailbox code. Once
   a conversation is back, the identity's profile envelope hands its peers
   the ordinary `contact` / `member` grants and the real mailbox; the
   continuity mailbox is not written to again by that peer. After the
   window the devices stop reading it and the DS prunes it like any idle
   mailbox.
6. **Expiry is a refusal.** A peer that comes back after the window finds no
   binding; the conversation stays on its old session and its sends end in
   the ordinary "Not sent" state (ADR-0073 D2, unchanged).

7. **Channels (maintainer, 2026-11-25: channels come back too).** ADR-0073
   left them out ("a subscriber who reset is not re-admitted by anyone").
   A subscriber and a publisher return through the same continuity
   mailbox; an owner, since 2026-11-26, returns by itself from its own list:
   - **Subscriber.** A device of the channel's owner or a publisher holds the
     subscriber list with each member card's account binding (ADR-0060
     section 3: the card opens to the account identity key and its signature
     over `channel_id ‖ subscriber_pk ‖ BLAKE3(profile)`). When the reset
     check accepts a reset for an account named in that list, the device
     sends a channel return to the continuity mailbox: the channel's
     ordinary invitation (for a private channel with an admission ticket)
     plus the old card's binding signature and profile. The reset device
     checks that signature against its own previous account key (from the
     KT log, which it reads for its own account anyway) and re-subscribes
     with no prompt; a return without a valid old signature is shown as an
     ordinary invitation, never accepted silently, so knowing the account id
     is not enough to subscribe someone to a channel. A subscriber that never
     uploaded a card (it chose not to be listed) cannot be found and does not
     come back.
   - **Publisher.** An owner device retires the old identity's leaves and
     admits the new identity's devices on the PublisherGroup (the ADR-0090
     admit path), and re-sends the read key and the newest epoch seeds; the
     Welcome goes to the continuity mailbox.
   - **Owner.** *Rewritten 2026-11-26 (maintainer's decision: owned
     channels come back to their owner without subscribers or publishers
     taking part), and settled the same day (maintainer's decisions on the
     open points: a server-held half `S` in the password lock, a second
     lock opened by the account keys). Designed, not built. The text of
     2026-11-25 (a recovery key seeded by an auth OPRF under `k_acct`, the
     channel ids learned from an owner return a subscriber or publisher
     sends) is replaced by what follows. Found on the walk of 2026-11-26
     (finding D1): that design never fired, because an owner uploads no
     member card, so no subscriber device could see that the owner reset.*

     The owner key is derived from the account seed, so a reset loses it and
     nobody else holds it. The account therefore keeps, on the storage
     service, a **list of the channels it owns**, sealed under a random
     **list key** `R`, and each channel's **recovery key** is derived from
     `R`. `R` is kept under **two locks**: a **password lock**, which the
     account password and a second factor open together, and a **keys
     lock**, which the account keys open. A device that holds the keys uses
     the keys lock; the device that reset the keys, which holds nothing but
     the password and a factor, uses the password lock.

     1. **The server half `S`.** Per account, never stored: `S =
        BLAKE3-derive-key("lakko/auth/owned-channels/v1", master ‖
        account_id(16))`, `master` a 32-byte environment secret of
        `services/auth` (`LAKKO_AUTH_OWNED_CHANNELS_SECRET`, 64 hex, required
        at startup), recomputed per request like the removed `k_acct`, no
        table. Auth releases it only:
        - in the answer of `/signup/finish` (`owned_channels_secret`, base64url,
          32 bytes), where the account was created in that very exchange;
        - from `POST /account/owned-channels-secret {reset_grant}` (bearer),
          which requires the ADR-0073 reset factor grant of 2026-11-26 (the
          password sign-in plus one second factor, the same proof the reset
          needs) and does not spend it, so the resetting device asks for `S`
          and then resets.

        Nothing else returns it: not a password sign-in, not a passkey
        sign-in, not a refresh. What auth learns: that this account asked
        for `S` and when (a reset is near), nothing about channels. A
        refused call charges nothing; an accepted one is counted by a new
        `Scope::OwnedChannelsSecret` per account, default 10 per day
        (chosen, not measured).
     2. **The password lock.** `lock_pw = BLAKE3-derive-key(
        "lakko/channels/owned-lock-password/v1", export_key ‖ S)`, where
        `export_key` is the 64-byte OPAQUE export key of the account's
        password credential (`KIND_PASSWORD`). Checked in the code on
        2026-11-26: every client receives it from `lakko-auth-client` at
        sign-up (`RegistrationOutput::export_key`) and at a password sign-in
        (`LoginOutput::export_key`), through UniFFI (iOS `AuthClient.swift`,
        Android `AuthClient.kt`) and wasm (`exportKey`), and caches it under
        the device key (`ExportKeyStore`); auth never learns it. **A plain
        password sign-in therefore no longer yields the list key**: it gives
        `export_key` but not `S`. Neither the password alone nor auth alone
        opens this lock. A key reset keeps the password envelope (ADR-0073
        section 2), so the lock survives the reset; a password change
        replaces `export_key`, so the lock must be made again (item 6).
     3. **The keys lock.** `lock_keys = BLAKE3-derive-key(
        "lakko/channels/owned-lock-keys/v1", lineage_seed)`, where
        `lineage_seed` is the seed ADR-0078 phase 6 names the DeviceGroup
        with: the oldest own predecessor after an ADR-0074 rotation, else the
        current account seed. Chosen because every device that can be in
        the account's DeviceGroup already holds it (the backup since version
        7 and the pairing state bundle carry the predecessors), and a
        rotation, which keeps the DeviceGroup, then keeps this lock too and
        re-locks nothing. A key reset clears the predecessors and gives a new
        lineage seed: the resetting device makes a new keys lock (item 5).
     4. **Where they live.** Three ADR-0064 vaults on `services/storage`,
        through the existing routes, unchanged (a claim spends one
        `vault-claim` token, writes at version current + 1, every read and
        write signed by the vault's write key, nothing names an account).
        `lakko-vault` gains a `VaultRoot` built from a given 32-byte root
        (beside the one from the account seed) and reference tag `0x04`
        (owned channels); each vault's id, AEAD key and write key are the
        ADR-0064 derivations over `root ‖ 0x04`:

        | Vault | Root | Head plaintext |
        |---|---|---|
        | password lock | `lock_pw` | `R(32)` |
        | keys lock | `lock_keys` | `R(32) ‖ S(32) ‖ the password lock's root (32)` |
        | list | `R` | the list |

        The keys lock carries `S`, so a device with the keys can make the
        password lock again after a password change without a second factor,
        and the current password lock's root, so it can delete the old one.
        Chosen over a new route because these routes already do all of this;
        the ADR-0064 account vaults are not used because their names come
        from the current account seed, which the reset replaces.

        The list's plaintext: `version(1)=0x01 ‖ count(1, 0..=64) ‖ count ×
        entry`, entry = `channel_id(16) ‖ read_key_len(1, 0|32) ‖ read_key ‖
        admission_count(1, 0..=8) ‖ admission_count × (admission_version(4
        BE) ‖ seed(32)) ‖ epoch_count(1, 0..=4) ‖ epoch_count × (epoch_id(4
        BE) ‖ seed(32))`, bounded and exact on decode, with a fuzz target. An
        entry is at most 483 bytes, 64 entries at most 30,914 bytes; the list
        is always padded to one size, 32 KiB, so the storage service learns
        no count. The 64 channels, 8 admission versions, 4 epoch seeds and
        32 KiB are chosen, not measured. Four epoch seeds is the ADR-0031
        retention, so the owner reads what a returning subscriber reads, and
        the newest epoch opens the channel's name and description, which the
        publisher re-issues after every rotation (ADR-0051). The admission
        seeds are what ADR-0090 decision A needs to reseal a private
        channel's earlier subscribers. An account owning more than 64
        channels lists the 64 it rotated most recently.

        The device that signs up makes all three at once (it holds the
        export key and `S` at that moment); a guest (ADR-0032) keeps `S`
        from its sign-up and makes them at the claim, when it sets its first
        password.
     5. **The recovery key.** `recovery_seed = BLAKE3-derive-key(
        "lakko/channels/recovery-key/v2", R ‖ channel_id(16))`, an Ed25519
        seed, registered with `PUT /v1/channels/{id}/recovery-key` (owner
        proof) when an owner device creates a channel and, for a channel
        whose registered key differs (every channel made before this
        change), at its next publisher pass. `R` never changes, so neither
        a password change nor a rotation re-registers anything. The owner
        change (`PUT /v1/channels/{id}/owner` with
        `x-lakko-channel-recovery-proof`) and the registration route of
        `services/channels` are unchanged.
     6. **Writes.** A device with the keys opens the keys lock, then the
        list, and writes the list when it creates or deletes a channel,
        after each rotation it makes, and when it mints an admission seed.
        Two owner devices writing at once: the second gets `409
        vault-version-conflict`, reads, merges (by channel id; newer epochs
        and admission versions kept, a deleted channel dropped) and writes
        again. No DeviceGroup message is needed: every device of the account
        holds the lineage seed.
     7. **A password change.** Whichever device first holds both the account
        keys and the new password's export key makes the new password lock
        (`lock_pw` from the new `export_key` and the `S` of the keys lock),
        writes `R` into it, records its root in the keys lock and deletes
        the old one. That is the device that changed the password when it
        has the keys (Settings, `/password/reset/*` on the phrase; recall,
        ADR-0074); otherwise (a mailed code on a device without the keys,
        ADR-0075 section 7) that device keeps the new export key under its
        device key and does it once it holds the keys, and any device with
        the keys does it at its next password sign-in. A device knows the
        lock is stale when the password lock its export key names does not
        exist.
     8. **After a reset.** The resetting device signed in with the password
        and confirmed a factor (ADR-0073, amended 2026-11-26). Before the
        reset spends the factor grant it fetches `S`; it then opens the
        password lock, `R` and the list, and for each entry: derives the
        recovery key, signs an owner change to the owner key its new account
        seed derives, and sends it (a `401` means the channel's recovery key
        is not this one: the entry is dropped); re-founds the PublisherGroup
        at the id the new seed derives (ADR-0090 decision A), rotates,
        reseals to every registered subscriber (a private channel's tickets
        checked with the listed admission seeds), re-issues the channel
        metadata, records the channel as `Owner` in its subscription
        register, and writes the list with the new epoch seed. It makes a new
        keys lock under its new lineage seed. The old keys lock stays on the
        storage service: only the old seed opens it, and a recall device
        (ADR-0074), which holds that seed, uses it. Co-publishers are invited
        again (decision A). No subscriber or publisher takes part, no
        mailbox is written, no member card is needed.
     9. **Account deletion** deletes the three vaults (signed) before
        `DELETE /account`.
     10. **Removed** (built on 2026-11-25, flag day, nothing deployed for
        users): `POST /auth/channel-recovery/evaluate`,
        `LAKKO_AUTH_CHANNEL_RECOVERY_SECRET`, `Scope::ChannelRecovery`
        (number 25 retired, not reused), `channel_recovery_oprf_input`,
        `ChannelRecoveryKey::from_oprf_output` and the blinder
        `blind_channel_recovery` on both FFI surfaces, the owner role (3) of
        the channel return envelope (the decoder refuses it), and the
        clients' sending of owner returns.

     What opens the list, case by case:

     | Situation | Lock | Owned channels |
     |---|---|---|
     | A device of the account with the keys | keys | yes |
     | Lost every device and the phrase, knows the password, has a factor: a reset | password | yes |
     | Mail-protected account (no phrase saved), lost every device, forgot the password: the mailed code restores the keys from the email vault (ADR-0075 section 7) | keys | yes, without a reset |
     | Phrase-protected account, lost the phrase, every device and the password: the mailed code gives a new password only, and the password lock was made with the old one | none | **lost**; the account's chats and groups still come back from contacts after the reset (sections 1 to 6, 9) |
     | Only the password, no factor | none | no: a password sign-in does not release `S` |
     | Auth alone | none | no: it holds `S`, not the export key |

     **The operator gains nothing it did not already have.** The password
     lock needs the export key, which only the password gives; an operator
     holding the auth database (the OPAQUE server state) can guess passwords
     offline, as against any OPAQUE server, but with a guessed password and
     the TOTP secrets and mail it already controls it could already reset
     the account and become it toward every contact; opening the list then
     adds nothing. The keys lock needs the account keys, which the operator
     can reach only for a mail-protected account through the email vault,
     the stated exception to invariant #2 of ADR-0075, where it already gets
     the whole account. Compared with the 2026-11-25 text this removes a
     capability: the auth OPRF key let the operator take any channel whose
     id it knew.

     The storage service learns that three small vaults exist (the list
     always 32 KiB), when each is written (channel created or deleted, a
     rotation, an admission seed, a password change, a reset) and read, and
     from which address; it cannot tell they belong together or to whom. An
     attacker who reset the account and then replaced a channel's recovery
     key keeps that channel after a recall.

8. **The profile comes back.** Profile envelope version 5 (flag day) adds
   an Ed25519 signature by the account identity key over the envelope. A
   peer that returns a conversation includes the reset person's newest
   profile it holds; the new identity's device keeps one only if it verifies
   under its own previous account key (read from the KT log), takes the
   newest stamp, and republishes it under its new key. The `@username`
   never left (it belongs to the account and was typed at sign-in).
9. **The history comes back, newest first.** Once the new identity's device
   is in a returned conversation it asks, inside it, for history (a field-less
   request carrying a fresh ADR-0091 gap key and a "before" message id). The
   other person in a one-to-one, or an owner or admin in a group, answers
   with the ADR-0091 gap-history construction: rows sealed to that gap key,
   signed by the sender's leaf, posted to the gap-history rendezvous, the
   newest page first (its size chosen, not measured), and the device asks for
   the next page until the sender has no more. The receiver applies the same
   signer rule as ADR-0091 (`gap_history_signer_accepted`). Channels: the
   return an owner or publisher device sends to a returning subscriber or
   publisher carries the newest 64 epoch seeds (`channel_epoch_seeds`), so
   the posts the channels service retains open again; a returning owner
   takes the newest 4 from its own list (section 7, amended 2026-11-26).
   Only what the sender still holds comes back.

No `lakko-mls` change. No DS change. A discovery migration (one nullable
column and a sweep), one codec in `lakko-contacts` with a fuzz target, the
binding sign / verify on both FFI surfaces, the three clients.

## What the operator learns (Variant A)

- That this account has a continuity mailbox, for the window. That the
  account reset is already public in the KT log (`is_reset`).
- The arrival times and count of rows in the continuity mailbox: roughly how
  many conversations came back and when. Rows are sealed (sender and kind
  hidden) and padded to buckets, so not who wrote or what.
- Not the real mailbox: no record joins it to the account, as ADR-0078
  requires. After the window discovery deletes the binding; a copy of the
  databases taken during the window keeps the join to the continuity
  mailbox only.
- Inherited from ADR-0073 R.4: anyone who knows the account id (former
  contacts and co-members, including a co-member the person had hidden from
  under ADR-0060, and the operator) can write to the continuity mailbox for
  the window. Each write costs a token.
- **Owned channels (amended 2026-11-26).** The operator gains nothing it did
  not already have (section 7, "The operator gains nothing"): the password
  lock needs the export key and a server half `S` released only after the
  password and a second factor, the keys lock needs the account keys. The
  auth OPRF key of the 2026-11-25 text, which let the operator take any
  channel whose id it knew, is gone. Auth learns when an account asks for
  `S`, which happens at sign-up and just before a reset. The storage service
  holds three small vaults per account, unlinked by name to each other and
  to the account; it sees when they are written and read and from which
  address. No owner return crosses a mailbox, so the continuity mailbox no
  longer counts owned channels.

## Alternatives

- **B. Shared buckets.** No binding. A peer writes the Welcome, sealed to the
  new identity's X25519 key, into a shared rendezvous chosen by a short
  prefix of the new identity key (for example 12 bits); the reset devices
  read their whole bucket for the window. The operator learns only which
  bucket, not which of the accounts in it the rows are for, so the count of
  returning conversations is hidden among the bucket. Costs: every reset
  device downloads every row of its bucket for 30 days, and the gain exists
  only when many accounts reset in one window; at test scale and early
  production a bucket holds one account and hides nothing. Can be layered on
  later without changing the peer's verdict or the reset order. Not
  recommended now.
- **C. Publish the real mailbox in the bundle.** The simplest; joins the
  account to its real mailbox for good, which is exactly what ADR-0078
  section 1 removed. Rejected.
- **D. Derive the address from the account id** (ADR-0073 R.4 as first
  built, ADR-0078's "continuity mailbox derivable from account id and
  generation"). Same disclosure as A but with no expiry under the identity's
  control and a mailbox anyone who knows the id can compute and therefore
  try to claim first. A's binding is signed by the new key, so it cannot be
  squatted. Rejected in favour of A.
- **E. No automatic return** (the person re-adds contacts by hand). Rejected
  by the maintainer on 2026-11-25: the reset screen promises the contacts
  keep their chats.
- **F. The backup survives the reset** (ADR-0078's open question): the reset
  device would read its contacts from the backup and write to them itself.
  Still undecided and orthogonal: if it is ever built, it reaches peers from
  the reset side, and A stays the path for peers the backup does not name.

## Consequences

- **Positive:** chats come back by themselves after a reset, as the reset
  screen says, with no record anywhere joining the account to its real
  mailbox. Uses pieces already built (the continuity reach seed, sealed
  mailbox rows, the reach verdict and the reachability gate).
- **Negative:** invariant #5 is weakened for reset accounts during the
  window: the operator sees the count and timing of returning conversations.
  Stated, not hidden. The ADR-0060 hidden-from co-member residual of R.4
  stays.
- **Neutral:** one more field in the identity bundle; one more mailbox read
  per reset device for 30 days.
- **Amended 2026-11-26.** Positive: an owner gets its channels back alone,
  with every subscriber offline, and no other device learns that the owner
  reset; the password alone does not open the list, and the operator gains
  no capability. Negative: a phrase-protected account that loses the phrase,
  every device and the password loses its owned channels (its chats and
  groups still come back); the list hangs on the password and a factor for
  the lost-everything case. Neutral: three small vaults per account on the
  storage service, the list written about once a day per channel (each
  rotation); one per-account derivation and one route in auth.

## Open questions

1. Whether the reset screen's line keeps its wording or says the chats come
   back "when your contacts open Lakko" (a peer must be online within the
   window for its conversation to return).
2. Whether a peer that missed the window should get a visible state on the
   old conversation (today: ordinary "Not sent").
3. Logging an owner change made with a channel recovery key, so a takeover is
   visible (where, and to whom).
4. ~~(2026-11-26) A factor-gated server half for the owned-channels root.~~
   **Decided 2026-11-26 (maintainer): adopted.** The password lock is made
   from the export key and a server half `S` that auth releases only at
   sign-up and after the password plus a second factor; and the list has a
   second lock, opened by the account keys. Both are written into section 7.
5. **Decided 2026-11-26 (maintainer):** the authenticator app may be an
   account's only second factor, with a warning that losing it loses the
   account (UX `identity-recovery.md` 19.1a and 19.1c).

## Plan

| Sprint | Content |
|---|---|
| S1 core | `lakko-contacts`: continuity mailbox seed and binding codec (sign, verify, expiry); channel return envelope (channel id, role, the old card's binding signature and profile, invitation, epoch seeds); history request envelope; profile v5 signed by the identity key; goldens, proptests, fuzz targets; `lakko-channels`: recovery-key owner proof; both FFI surfaces; NSE recognisers. |
| S2 services | Discovery: `continuity_binding` (migration, verified upload, served while unexpired, swept). Auth: channel-recovery OPRF evaluation under `k_acct` (session, rate-limited). Channels: register a recovery key, `PUT owner` with a recovery proof (migration). Route tests. |
| S3 web | Reset order (continuity mailbox, binding, reach key); read loop for the window; peer side for chats, groups, subscribers, publishers, owners; profile return; history request and answer; channel recovery. |
| S4 iOS, Android | The same, in parallel. |
| S5 walk | Base scenario 4b with the return of a chat, a group, a subscription, an owned channel, the profile and history, on two clients. |

## Plan amended 2026-11-26

The three decisions of 2026-11-26 are planned here, in one place, rather
than in a new ADR: the second factor for the reset exists because of this
ADR's return (whoever resets now gets the account's conversations, channels
and history back), the factor at sign-up exists so every account can pass
that reset, and the owned-channels list is this ADR's section 7. The
mechanisms are written where they belong: the reset factor in ADR-0073
("Amended 2026-11-26"), the sign-up factor in ADR-0075 ("Amended
2026-11-26"), the list in section 7 above. S1 to S4 above were built in
commit `fa44cb8cb`; S5 was walked on the test stack on 2026-11-26 (web and
iOS), where the owned channel did not come back. Section 7 item 8 lists what
of S1 to S4 is now removed.

| Sprint | Content |
|---|---|
| S6 core | `lakko-auth-token`: the fresh-auth grant names how it was minted (password or passkey); the reset factor grant (`f1`). `lakko-vault`: reference tag `0x04` and a `VaultRoot` from a given root. `lakko-channels`: the two lock derivations, the keys-lock and password-lock head codecs, the owned-channels list codec (bounds, proptests, fuzz target `owned_channels_decode`), `ChannelRecoveryKey` from `R`, the OPRF input removed. `lakko-contacts`: the owner role removed from the channel return. Both FFI surfaces, the OPRF blinder removed. |
| S7 services | `services/auth`: the reset factor routes (authenticator, email with a reset purpose, passkey through an ADR-0014 reset action), the reset routes on the factor grant, the authenticator setup under the fresh-auth grant, the last-factor refusals, the authenticator key required at startup, the channel-recovery route and scope removed; `S` (the environment secret required at startup, the `/signup/finish` field, `POST /account/owned-channels-secret`, its scope). `services/storage` and `services/channels`: no change. DB-gated route tests for every refusal. |
| S8 web, desktop | UX 19.1a to 19.1c (the required step, the authenticator setup, the landing rule, the last-factor dialog) and 15.1b (the reset factor section, the "cannot reset" panel); the authenticator-only warning (19.1a, 19.1c); the three owned-channels vaults (made at sign-up or at a guest's claim; the list written on create, delete, rotation and admission seed; recovery keys registered from `R`; the password lock made again after a password change; after a reset `S`, the password lock, the retake and a new keys lock; deleted with the account); owner-return sending and the OPRF call removed. |
| S9 iOS, Android | The same, in parallel. |
| S10 walk | Sign-up with each of the three factors on each client; a reset with each factor, a wrong code and the lockout; an account with no factor; a public and a private owned channel back after a reset with every subscriber device closed; a password change, then a reset. |

Security invariants touched: **#2** (holds: the list is sealed by the
client; the password-derived root and the recovery keys never reach a
server; the reset's email factor shows the address to the server when the
letter is sent, as ADR-0075 already states), **#5** (improved: owner
returns no longer cross mailboxes; the owned-channels vaults name no
account), **#6** and **#7** (unchanged; `R` and `S` are kept only on the
storage service under the two locks, and in memory while used). No `lakko-mls` change; G-39 does not grow.
`services/auth` and the channel recovery derivation change: a
`lakko-security` pass is due before merge.

## References

- ADR-0073 (R.2 verdict, R.4 continuity key, D2 expiry), ADR-0078 (sections 1 and 7, open question), ADR-0060 (reach keys), ADR-0054 (tokens)
- `docs/ux/identity-recovery.md` sections 15 and 16
