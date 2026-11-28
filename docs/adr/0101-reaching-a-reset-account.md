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
  account keys, and a lock per passkey that keeps the chats, with no subscriber or publisher taking part; open points
  settled the same day); sections 10 and 11 added the same day (a reset moves
  anything only under a reset seal the account published before it; an
  unsealed reset is a new account under the same username), with the
  ADR-0075 email vault removed; the build plan for
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
     lock**, which the account keys open; and, since the decision on open
     question 6 (2026-11-26), under a **passkey lock** for each passkey that
     keeps the chats (section 10). A device that holds the keys uses the
     keys lock; the device that reset the keys uses the lock of what sealed
     its reset: the password lock, or the passkey lock of the passkey used.

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
        lineage seed: the resetting device makes a new keys lock (item 8).
        **3a. The passkey locks** *(decided 2026-11-26 on open question 6: it
        follows from the maintainer's rule that a passkey with a seal brings
        everything back).* For each passkey that keeps the chats, `lock_pk =
        BLAKE3-derive-key("lakko/channels/owned-lock-passkey/v1", prf_output
        ‖ S)`, the `prf_output` being the one its seal key comes from
        (section 10), so one evaluation serves both. `S` is in the input for
        the reason it is in the password lock: the passkey alone, a stolen
        one included, does not open the list; it takes the password and a
        factor (which the passkey can be) to get `S`, as for the reset.
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
        | keys lock | `lock_keys` | `R(32) ‖ S(32)` |
        | passkey lock, one per passkey that keeps the chats | `lock_pk` | `R(32)` |
        | list | `R` | the lock roots, then the list |

        The keys lock carries `S`, so a device with the keys can make the
        password lock or a passkey lock again without a second factor. The
        list names the current lock roots, so whoever holds `R` can delete a
        stale lock.
        Chosen over a new route because these routes already do all of this;
        the ADR-0064 account vaults are not used because their names come
        from the current account seed, which the reset replaces.

        The list's plaintext: `version(1)=0x02 ‖ password_lock_root(32) ‖
        passkey_lock_count(1, 0..=8) ‖ passkey_lock_count × (tag(16) ‖
        root(32)) ‖ count(1, 0..=64) ‖ count × entry` (the `tag` is the
        passkey's tag in the seal announcement), entry = `channel_id(16) ‖ read_key_len(1, 0|32) ‖ read_key ‖
        handle_seed_len(1, 0|32) ‖ handle_seed ‖
        admission_count(1, 0..=8) ‖ admission_count × (admission_version(4
        BE) ‖ seed(32)) ‖ epoch_count(1, 0..=4) ‖ epoch_count × (epoch_id(4
        BE) ‖ seed(32))`, bounded and exact on decode, with a fuzz target. Each
        entry carries the channel's ADR-0087 handle write seed, so the
        retaking device signs `rekey` from the old handle key to the one its
        new root derives. An
        entry is at most 516 bytes, 64 entries at most 33,024 bytes, the lock
        roots at most 417 bytes; the list is always padded to one size, 64 KiB, so the storage service learns
        no count. The 64 channels, 8 admission versions, 4 epoch seeds and
        64 KiB are chosen, not measured. Four epoch seeds is the ADR-0031
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
        holds the lineage seed. When a passkey that keeps the chats is added
        on a device with the keys (which has its `prf` output from the
        creation and `S` from the keys lock), that device makes its passkey
        lock and records the root in the list; when a passkey is removed,
        the device that removed it deletes its lock.
     7. **A password change.** Whichever device first holds both the account
        keys and the new password's export key makes the new password lock
        (`lock_pw` from the new `export_key` and the `S` of the keys lock),
        writes `R` into it, records its root in the list and deletes the old
        one. That is the device that changed the password when it
        has the keys (Settings, `/password/reset/*` on the phrase; recall,
        ADR-0074); otherwise (a mailed code on a device without the keys,
        ADR-0075 section 7) that device keeps the new export key under its
        device key and does it once it holds the keys, and any device with
        the keys does it at its next password sign-in. A device knows the
        lock is stale when the password lock its export key names does not
        exist.
     8. **After a reset.** The resetting device signed in with the password
        and confirmed a factor (ADR-0073, amended 2026-11-26). Before the
        reset spends the factor grant it fetches `S`; it then opens the lock
        of what sealed the reset (the password lock, or the passkey lock of
        the passkey whose `prf` output signed the seal), `R` and the list,
        and for each entry: derives the
        recovery key, signs an owner change to the owner key its new account
        seed derives, and sends it (a `401` means the channel's recovery key
        is not this one: the entry is dropped); re-founds the PublisherGroup
        at the id the new seed derives (ADR-0090 decision A), rotates,
        reseals to every registered subscriber (a private channel's tickets
        checked with the listed admission seeds), re-issues the channel
        metadata, records the channel as `Owner` in its subscription
        register, and writes the list with the new epoch seed. It makes a new
        keys lock under its new lineage seed, makes the password lock again
        if the reset was sealed by a passkey and its export key names no
        password lock, and deletes every passkey lock the list names (the
        reset deleted the passkeys). The old keys lock stays on the
        storage service: only the old seed opens it, and a recall device
        (ADR-0074), which holds that seed, uses it. Co-publishers are invited
        again (decision A). No subscriber or publisher takes part, no
        mailbox is written, no member card is needed.
     9. **Account deletion** deletes the list and every lock (signed) before
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
     | Lost every device and the phrase, knows the password the list was locked with, has a factor: a sealed reset (item 10) | password | yes |
     | ~~Mail-protected account ... the mailed code restores the keys from the email vault~~ | | Withdrawn 2026-11-26 with the email vault (ADR-0075, amended) |
     | Lost the phrase, every device and the password: the mailed code gives a new password only, the password lock was made with the old one, and the reset is unsealed (item 11) | none | **lost**, and the chats and groups do not come back either |
     | A passkey that keeps the chats seals the reset, also when the password was changed without the keys | that passkey's lock | yes (decided on open question 6) |
     | Only the password, no factor | none | no: a password sign-in does not release `S` |
     | Auth alone | none | no: it holds `S`, not the export key |

     **The operator gains nothing it did not already have.** The password
     lock needs the export key, which only the password gives; an operator
     holding the auth database (the OPAQUE server state) can guess passwords
     offline, as against any OPAQUE server, but with a guessed password and
     the TOTP secrets and mail it already controls it could already reset
     the account and become it toward every contact; opening the list then
     adds nothing. The keys lock needs the account keys, which the operator
     does not have: the email vault, through which it could reach them, is
     removed (2026-11-26). Compared with the 2026-11-25 text this removes a
     capability: the auth OPRF key let the operator take any channel whose
     id it knew. The same guessed password also makes a password seal
     (item 10); that is the one capability the operator keeps, at the cost
     of an offline search, and a passkey or phrase seal does not depend on
     it.

     The storage service learns that a few small vaults exist (the list
     always 64 KiB, three locks plus one per passkey that keeps the chats), when each is written (channel created or deleted, a
     rotation, an admission seed, a password change, a reset) and read, and
     from which address; it cannot tell they belong together or to whom. An
     attacker who reset the account and then replaced a channel's recovery
     key keeps that channel after a recall.

     *As built in the core, 2026-11-27 (S6; `lakko-channels::owned`,
     `lakko-vault`, both FFI surfaces; host-tested only, no client uses it
     yet).* Chosen where this section left a detail open: the encoded list
     is always exactly 65 532 bytes (the content, then zero bytes), so with
     the vault's 4-byte length frame it fills the 64 KiB bucket exactly
     (32 KiB until 2026-11-27, when the handle seed made the fullest list
     33 443 bytes);
     decode refuses any other length and any non-zero padding byte. The list
     is canonical: passkey locks strictly ascending by tag, entries by
     channel id, admission seeds by `(admission_version, seed)` (two seeds of
     one version may coexist, as ADR-0090 allows), epoch seeds by epoch;
     anything else is refused. The lock heads carry no version byte: exactly
     `R` (32 bytes) or `R ‖ S` (64 bytes). The merge after a `409`
     (`OwnedChannelsList::merge`) keeps the writing device's value on a tie
     (an epoch seed of the same epoch, the read key, the password lock root,
     a passkey lock of the same tag) and, past 64 entries, the writing
     device's own entries first. `lakko-vault` names these vaults by
     `VaultRoot::from_root` over reference tag `0x04`. The recovery key is
     `BLAKE3-derive-key("lakko/channels/recovery-key/v2", R ‖ channel_id)`.

     *As built in the services, 2026-11-27 (S7; `services/auth`,
     DB-tested against a local MariaDB, not deployed).* `S` is
     `owned_channels.rs`: `LAKKO_AUTH_OWNED_CHANNELS_SECRET` (64 hex) is
     required at startup; `/signup/finish` answers `owned_channels_secret`
     (base64url, 32 bytes) beside the session pair and the fresh-auth grant;
     `POST /account/owned-channels-secret {reset_grant}` (bearer) answers
     `{owned_channels_secret}`, refuses a missing or unusable grant with
     `403 {"error":"reset-factor-required"}` before anything is charged, and
     counts accepted calls with `Scope::OwnedChannelsSecret` (27, 10 a day,
     `LAKKO_AUTH_LIMIT_OWNED_CHANNELS_SECRET`). The channel-recovery OPRF
     route, `LAKKO_AUTH_CHANNEL_RECOVERY_SECRET` and `Scope::ChannelRecovery`
     (25, retired) are deleted; `services/channels` changed only in a test
     (`recovery_owner.rs` derives its recovery keys with
     `ChannelRecoveryKey::from_list_key`).

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

10. **The reset seal** *(maintainer's decision, 2026-11-26; designed, not
   built).*

   **Why.** Sections 1 to 9 move an account's conversations, history,
   profile and channel subscriptions to its new keys on a reset that a peer
   accepts by ADR-0073 R.2: the auth service's `KeyResetAttestation`, the
   generation, and the KT log entry the operator writes. All three are the
   operator's. The operator holds the attest signing key, so it can mint a
   reset for any account, without the password and without a factor,
   publish a key of its own, and every contact's device then silently hands
   it the account's chats, groups and recent history (sections 4, 7, 9).
   The second factor of ADR-0073's 2026-11-26 amendment does not help: it is
   checked by the same server. Separately, the ADR-0075 email vault was
   sealed under the login handle, the address and a server-held salt; the
   operator sees the address at enrolment and at recovery and can guess it
   (usernames are public), so it could open the vault: in effect protected
   by server secrets only. ADR-0075 is amended the same day to remove the
   vault.

   **Rule.** A peer moves anything (sections 4, 5, 7, 8, 9) only for a
   **sealed** reset: one whose new key is signed by a **seal key** the
   account announced, under its old account key, before the reset. A reset
   that passes R.2 without a valid seal is **unsealed** (item 11).

   **Seal keys.** Ed25519, each from a secret the server never holds:

   | Kind | Seed | Who can make it |
   |---|---|---|
   | 1 password | `BLAKE3-derive-key("lakko/account/reset-seal/password/v1", export_key)`, the 64-byte OPAQUE export key of `KIND_PASSWORD` | the device that ran the password registration or a password sign-in |
   | 2 passkey | `BLAKE3-derive-key("lakko/account/reset-seal/passkey/v1", prf_output)`, the WebAuthn `prf` output for a salt of its own, `SHA-256("lakko/account/reset-seal/passkey-prf/v1")`, distinct from the passkey vault's salt | the passkey's authenticator, when it supports `prf` |
   | 3 phrase | `BLAKE3-derive-key("lakko/account/reset-seal/phrase/v1", phrase_seed)`, the 64-byte BIP39 seed | whoever holds the 12 words |

   *Password.* The operator cannot derive the export key without the
   password, but it holds the OPAQUE server state (the OPRF key and the
   envelope), so it can run an **offline guessing attack**: each guess costs
   one OPRF evaluation and the client's key stretching, Argon2id with the
   `argon2` 0.5 defaults (19 MiB of memory, 2 passes, 1 lane; read from
   `crates/lakko-auth-client/src/cipher_suite.rs` and the crate source; the
   time per guess is not measured here). A long password resists it; a
   passkey seal or a phrase seal does not depend on the password at all.
   *Passkey.* Only a passkey whose authenticator returns a `prf` output at
   creation can seal. Expected from platform documentation, not measured:
   Apple platform passkeys from iOS 18 / macOS 15; Android from API 34
   (below it the request is dropped at registration, ADR-0072); browsers
   depending on the browser and the authenticator. Lakko learns it when the
   passkey is created; one that returned none still signs in and confirms a
   reset, and UX `identity-recovery.md` 19.1d says it does not keep the
   chats. *Phrase.* A phrase holder normally restores the keys instead of
   resetting; the phrase seal serves a person who has the phrase but wants
   new keys (keys suspected leaked) with continuity.

   **The seal announcement**, signed by the current account key:

   ```text
   "lakko/account/reset-seals/v1\0" ‖ version(1)=0x01 ‖ account_ref(32)
     ‖ key_generation(8 BE) ‖ seq(8 BE) ‖ issued_at_ms(8 BE)
     ‖ count(1, 1..=10) ‖ count × (kind(1) ‖ tag(16) ‖ seal_pk(32))
     ‖ sig(64)
   ```

   `account_ref` is the key-reset attestation's (`BLAKE3-derive-key(
   "lakko/key-reset/account-ref/v1", account_id)`). Exactly one kind-1
   entry; up to 8 kind-2 entries, `tag` = the first 16 bytes of `SHA-256`
   of the credential id, so a removed passkey's seal can be dropped; at most
   one kind-3 entry; `tag` all zero for kinds 1 and 3. `seq` strictly
   increases per account across generations. At most 641 bytes. The bounds
   (8 passkeys, 641 bytes) are chosen. Bounded, exact, fail-closed codec in
   `lakko-attest` with a fuzz target.

   *Published* in the identity bundle (`services/discovery`, opaque column
   `reset_seals`, never parsed, uploaded with the bundle under the session)
   and appended to the KT log as a leaf of its own kind for the account's
   `username_hash`, so its history is witnessed (ADR-0039). Discovery keeps
   the newest announcement per generation and, after a reset, also serves the
   newest of the previous generation (`previous_reset_seals`).

   *Republished* by a device that holds the account keys: after the password
   changes on it (it has the new export key), after a passkey is added (with
   its `prf` output) or removed, after a phrase is saved, after an ADR-0074
   rotation (the new key signs, `seq` continues), and after a reset (the new
   key at generation `g`, with the password seal; the passkeys were deleted
   and the new phrase is not saved). A password changed on a device without
   the keys (a mailed code, ADR-0075 section 7) is not announced until a
   device with the keys takes it over at its next password sign-in.

   **The reset seal**, signed by one announced seal key:

   ```text
   "lakko/account/reset-seal/v1\0" ‖ version(1)=0x01 ‖ account_ref(32)
     ‖ announcement_seq(8 BE) ‖ from_generation(8 BE) ‖ to_generation(8 BE)
     ‖ new_signature_pubkey(32) ‖ kind(1) ‖ seal_pk(32) ‖ sig(64)
   ```

   Carried in the reset's attested publish (discovery, opaque column
   `reset_seal` beside `key_reset_attest`) and in the KT reset leaf. The
   server can strip it, which makes the reset unsealed: an availability
   loss, never a takeover (the ADR-0016 carrier argument).

   *Made by the resetting device* before `POST /account/keys/reset/finish`,
   from what the reset already asks for (ADR-0073, amended 2026-11-26): the
   export key of the password sign-in (kind 1); the `prf` output of the
   passkey factor's assertion, requested in the same gesture (kind 2); or a
   typed phrase (kind 3). It reads its own current announcement from
   discovery, checks that its seal key is listed, and signs over the next
   generation and the new key it minted.

   **The peer's check**, ADR-0073 R.2 rule 4 (amended there): holding the
   old key `K` and the generation `g-1` it accepted,
   1. the previous announcement verifies under `K`, names this account's
      `account_ref` and generation `g-1`, its `seq` is at least the highest
      this device has seen for the account (kept per contact), and the KT
      log shows it as the newest announcement leaf of `g-1` before the reset
      leaf (the verified path, the witness gate when pinned);
   2. the reset seal names that `account_ref` and `seq`, `from = g-1`,
      `to = g` (the attested generation) and the fetched key; its `seal_pk`
      is listed in the announcement under its `kind`; the signature
      verifies.
   Both hold: **sealed**, sections 4 to 9 run. R.2 holds and these do not:
   **unsealed** (item 11). Without a log in the build, the announcement
   rests on `K`'s signature and the `seq` floor this device kept; rolling
   back to an older announcement then needs a seal key from it (an older
   password or a removed passkey) and is the stated residual.

   **The account's own devices** apply the same check to their own account
   on a reset they did not make: an unsealed reset keeps ADR-0074's "Was it
   you?" and recall as today; a sealed one is the owner's (password and a
   factor).

   *As built in the core, 2026-11-27 (S6; `lakko-attest::reset_seal`,
   `lakko-transparency::reset_seals`, `lakko-contacts::reset_verdict`, both
   FFI surfaces; host-tested only, no client uses it yet).* Chosen where this
   section left a detail open: the domain separators are part of the wire
   (the only reading under which the announcement's 641-byte maximum adds
   up), so an announcement is 200 to 641 bytes and a reset seal 214. An
   announcement's entries are strictly ascending by `(kind, tag)`, so it has
   one encoding and two passkey entries cannot share a tag; a seal key, and
   the reset's new key, that is not a valid Ed25519 point or is of small
   order is refused. A reset seal's `to_generation` must be exactly
   `from_generation + 1`. The KT announcement leaf is
   `"lakko/transparency/reset-seals-leaf/v1\0" ‖ username_hash ‖
   key_generation(8 BE) ‖ seq(8 BE) ‖ BLAKE3(announcement)` (119 bytes): it
   commits the announcement by hash, and its ASCII separator keeps it apart
   from an identity leaf, which opens with a 32-byte hash. The check proves
   the served announcement is included, for this account and generation, at
   an index below the reset leaf's; "the newest" rests on the operator, the
   witnesses and the device's `seq` floor, as stated above. The R.2 table
   with rule 4 is written once, in `lakko_contacts::reset_verdict`, and the
   three clients' copies are to call it (`resetVerdict` on both FFI
   surfaces).

   *As built in the services, 2026-11-27 (S7; `services/discovery`,
   `services/transparency`, DB-tested against a local MariaDB, not
   deployed).* Chosen where this section left a detail open, and one place
   where it is reversed: **discovery decodes and checks both artefacts**
   instead of storing them unparsed, because the KT leaf needs the
   announcement's generation and `seq`, which only the announcement carries.
   `POST /discovery/identity` takes `reset_seals` and `reset_seal` (standard
   base64). An announcement must verify under the uploaded identity key,
   name the session account's `account_ref`, be at the generation the row is
   at after the upload, and have a `seq` above every one the row has held
   (`400 reset-seals-invalid`, `409 reset-seals-stale`; the same bytes again
   are a no-op; absent keeps the stored one). A reset seal is read only on an
   attested key change that is a reset (not a proven rotation) and must
   verify under its own seal key, name the account and the uploaded key,
   seal exactly `stored generation -> attested generation`, and name the
   row's current announcement by `seq`, which lists its key under its kind
   (`400 reset-seal-invalid`); a reset without one stores none. When the
   generation moves (a reset or a proven rotation) the current announcement
   becomes `previous_reset_seals`. Migration `0043` adds `reset_seals`,
   `reset_seals_seq`, `previous_reset_seals`, `reset_seal` and
   `logged_seals_seq`; `GET /discovery/identity/{id}` serves `reset_seals`,
   `previous_reset_seals` and `reset_seal` (standard base64, each omitted
   when none). The KT log: every newly stored announcement is appended
   (`POST /transparency/announcement/append`, service-secret gated) after the
   identity leaf it is signed by; on a reset the previous generation's
   announcement is appended BEFORE the reset's identity leaf when the log has
   not confirmed it yet; the reset seal rides the identity append as a
   sidecar (`reset_seal_b64`, `log_entries.reset_seal`, served by
   `/transparency/latest` as `reset_seal_b64`); the reconciler appends the
   current announcement of an account whose identity leaf is logged. The
   transparency service checks an announcement under the account's latest
   identity key in the log (`403`) and refuses a `seq` not above the logged
   ones (`409`); the leaves sit in `log_announcements` (migration `0006`,
   append-only, sharing the index sequence with `log_entries`; the replay at
   boot merges the two by index); `GET /transparency/announcement/{username_hash}/{generation}`
   answers the newest leaf of that generation `{idx, key_generation, seq,
   announcement_b64}` and `GET /transparency/proof/{idx}` serves either leaf
   kind's bytes and audit path. `lakko-transparency::InMemoryLog` gained
   `append_announcement` and `leaf_bytes` for it (`entry` answers
   `NotAnIdentityLeaf` at an announcement's index).

11. **An unsealed reset is a new account under the same username** *(maintainer,
   2026-11-26).* The reset still works (password and a factor, ADR-0073):
   the server gives the account new keys and the username stays. Peers do
   not re-pin silently, move no conversation, return no history, profile,
   epoch seeds or channel subscriptions, and admit nobody; the person's
   devices start empty and reach contacts by ordinary contact requests,
   which contacts accept or not (UX 16.11). The owned channels do not come
   back either: the password lock of section 7 was made with the password
   the account had when it was last announced (an unsealed reset is, in
   practice, one after a password changed without the keys), and the keys
   lock needs the old keys. The new account publishes its own announcement
   at generation `g`, so its next reset can be sealed.

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
- **The reset seal (2026-11-26, section 10).** The operator can no longer
  give itself an account's conversations by minting a reset: a reset without
  a seal moves nothing. What it keeps: an offline guessing attack on the
  password from the OPAQUE state (each guess one OPRF evaluation and an
  Argon2id stretch), whose success gives a password seal; it cannot seal
  with a passkey or the phrase. What it learns: the seal announcements are
  public artefacts (public keys, a count of passkeys that keep the chats,
  whether a phrase is saved, and when each changed), served by discovery
  and logged in the KT log, so anyone who can look up the account learns
  them too. It still sees the unsealed reset happen and can strip a seal to
  turn a sealed reset into an unsealed one (an availability loss for the
  person, not a takeover).

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
- **Amended 2026-11-26 (second): the reset seal.** Positive: a reset moves
  conversations only with the account's own password, passkey or phrase, so
  neither the operator nor whoever only reads the mailbox can take an
  account's contacts with it; the email vault, the one place the operator
  could open, is gone. Negative: forgetting the password without a phrase or
  a passkey that keeps the chats now loses the chats too (the 19.1d notice
  says so in advance); a seal announcement tells anyone who looks up the
  account whether it saved a phrase and how many sealing passkeys it has.
  Neutral: one more signed artefact per account in the bundle and the log,
  republished on a few account changes.

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
6. ~~(2026-11-26) A third lock per passkey that keeps the chats.~~
   **Decided 2026-11-26: yes.** Decided by the coordinator as following from
   the maintainer's rule that a passkey with a seal brings everything back:
   a reset sealed by a passkey after the password was changed without the
   keys would otherwise bring the chats back but not the owned channels.
   Written into section 7 (item 3a, the table of item 4, items 6, 8, 9 and
   the case table); `S` is in the lock's input so a stolen passkey alone
   opens nothing.
7. ~~(2026-11-26) What contacts see after an unsealed reset.~~ **Decided
   2026-11-26 (maintainer): shown.** After an **unsealed** reset the old
   one-to-one shows one closed line in place of the composer, and the new
   contact request carries a note that the device could not confirm it is
   the same person (UX 16.11). This is the only exception to the rule that
   a contact's reset shows nothing (UX 16.1): a sealed reset stays silent.

## Plan

| Sprint | Content |
|---|---|
| S1 core | `lakko-contacts`: continuity mailbox seed and binding codec (sign, verify, expiry); channel return envelope (channel id, role, the old card's binding signature and profile, invitation, epoch seeds); history request envelope; profile v5 signed by the identity key; goldens, proptests, fuzz targets; `lakko-channels`: recovery-key owner proof; both FFI surfaces; NSE recognisers. |
| S2 services | Discovery: `continuity_binding` (migration, verified upload, served while unexpired, swept). Auth: channel-recovery OPRF evaluation under `k_acct` (session, rate-limited). Channels: register a recovery key, `PUT owner` with a recovery proof (migration). Route tests. |
| S3 web | Reset order (continuity mailbox, binding, reach key); read loop for the window; peer side for chats, groups, subscribers, publishers, owners; profile return; history request and answer; channel recovery. |
| S4 iOS, Android | The same, in parallel. |
| S5 walk | Base scenario 4b with the return of a chat, a group, a subscription, an owned channel, the profile and history, on two clients. |

## Plan amended 2026-11-26

The decisions of 2026-11-26 are planned here, in one place, rather than in a
new ADR: the second factor for the reset exists because of this ADR's return
(whoever resets now gets the account's conversations, channels and history
back), the factor at sign-up exists so every account can pass that reset,
the owned-channels list is this ADR's section 7, and the reset seal
(section 10) decides when this ADR's return happens at all. The mechanisms
are written where they belong: the reset factor and the seal check in
ADR-0073 ("Amended 2026-11-26" and "Amended 2026-11-26 (second)"), the
sign-up factor and the removal of the email vault in ADR-0075, the list and
the seal in sections 7, 10 and 11 above. S1 to S4 above were built in
commit `fa44cb8cb`; S5 was walked on the test stack on 2026-11-26 (web and
iOS), where the owned channel did not come back. Section 7 item 10 lists
what of S1 to S4 is now removed.

| Sprint | Content |
|---|---|
| S6 core | `lakko-auth-token`: the fresh-auth grant names how it was minted (password or passkey); the reset factor grant (`f1`). `lakko-attest`: the seal announcement and the reset seal (sign, verify, bounds, goldens, proptests, fuzz targets `reset_seals_open` and `reset_seal_open`), the three seal-key derivations. `lakko-vault`: reference tag `0x04` and a `VaultRoot` from a given root. `lakko-channels`: the three lock derivations (password, keys, passkey), the lock head codecs, the owned-channels list codec with its lock-root header (bounds, proptests, fuzz target `owned_channels_decode`), `ChannelRecoveryKey` from `R`, the OPRF input removed. `lakko-transparency`: the announcement leaf kind and the "newest announcement of a generation before the reset" check. `lakko-contacts`: the owner role removed from the channel return; the R.2 verdict gains `Sealed` / `Unsealed`. `lakko-mnemonic`: `seal_entropy_under_email` / `open_entropy_under_email` removed. Both FFI surfaces (the email vault functions and the OPRF blinder removed; seal sign / verify and the verdict added). |
| S7 services | `services/auth`: the reset factor routes (authenticator, email with a reset purpose, passkey through an ADR-0014 reset action), the reset routes on the factor grant, the authenticator setup under the fresh-auth grant, the last-factor refusals, the authenticator key required at startup, the channel-recovery route and scope removed; `S` (the environment secret required at startup, the `/signup/finish` field, `POST /account/owned-channels-secret`, its scope); the email vault removed (ADR-0075, amended: tables, columns, routes, scopes). `services/discovery`: the opaque `reset_seals` (current and previous generation) and `reset_seal` columns, carried to the KT append. `services/transparency`: the announcement leaf. `services/storage` and `services/channels`: no change. DB-gated route tests for every refusal. |
| S8 web, desktop | UX 19.1a to 19.1d (the required step, the authenticator setup, the landing rule, the last-factor dialog, the honest notice) and 15.1b, 15.1c (the reset factor section, sealed or unsealed before the word, the "cannot reset" panel); 16.11 (unsealed reset at a contact: the closed line in place of the composer, the note on the new request); the seal announcement published at sign-up and on every change that alters it; the reset seal made from the password, the passkey `prf` or the phrase; the owned-channels list and its locks (made at sign-up or at a guest's claim; a passkey lock made when a passkey that keeps the chats is added and deleted when it is removed; the list written on create, delete, rotation and admission seed; recovery keys registered from `R`; the password lock made again after a password change; after a reset `S`, the lock of what sealed it, the retake, a new keys lock and the old passkey locks deleted; deleted with the account); owner-return sending, the OPRF call and every email-vault path removed. |
| S9 iOS, Android | The same, in parallel. |
| S10 walk | Sign-up with each of the three factors on each client; a sealed reset (password; a passkey that keeps the chats) and an unsealed one (after a mailed-code password change), and what a contact sees for each; a reset with a wrong code and the lockout; an account with no factor; a public and a private owned channel back after a reset sealed by the password and after one sealed by a passkey following a mailed-code password change, with every subscriber device closed; the honest notice appearing and going. |

Security invariants touched: **#2** (strengthened: the ADR-0075 exception
is withdrawn with the email vault, the maintainer edits its text in
CLAUDE.md; the owned-channels list is sealed by the client, the
password-derived root, the recovery keys and the seal secrets never reach a
server; the reset's email factor shows the address to the server when the
letter is sent, as ADR-0075 states), **#4** (an attested reset without a
seal moves nothing), **#5** (improved: owner returns no longer cross
mailboxes; the owned-channels vaults name no account), **#6** (changed: an
identity rotation by reset is honoured by peers only under a seal the
account published before it; an unsealed reset is a new account, not a
rotation), **#7** (unchanged; `R` and `S` are kept only on the storage
service under the two locks, and in memory while used). No `lakko-mls`
change; G-39 does not grow. Known gap **G-60** (minted 2026-11-26) tracks
the problem until this is built. `services/auth`, discovery, the KT log and
the reset acceptance change: a `lakko-security` pass is due before merge.

### As built on Android, 2026-11-27 (S9; `apps/android`)

Built against the S6 core and the S7 services; the host unit suite passes
(`:app:testDebugUnitTest`), nothing was run on a device, an emulator or
against a server, and the native library was not rebuilt (`just
build-android` is needed for a device run). Where the spec or this ADR was
silent, the smallest consistent thing was chosen:

- **A passkey's seal secret.** Asked as the PRF extension's `eval.second`
  (the salt of section 10) beside the passkey vault's `eval.first`, at
  passkey creation and at "Restore keys with this passkey"; the reset's own
  passkey ceremony asks it as `eval.first`. Below API 34 Android drops the
  request at creation (ADR-0072), so such a passkey confirms a reset and
  never seals one; its row says "Does not keep your chats.".
- **Which password seal a device asserts.** Its own export key's only when
  the account has no announcement of this generation, when it is the
  announced one, or when the device obtained the export key after the
  announcement was signed (the time is kept beside the export key); a device
  with an older cached key carries the announced seal forward. The
  comparison is between two devices' clocks (chosen, not measured). The
  owned-channels password lock follows the same answer: it is made only
  from the export key whose seal the announcement lists.
- **When the announcement is published.** At every session landing, when
  the "Protect your account" step is finished (a sign-up's landing can run
  before the account's first directory row exists), after a password change,
  a passkey added or removed, a saved phrase, a key rotation and a reset's
  publish.
- **An unsealed reset at a contact.** The gate answers a distinct
  "started over" (a subclass of the inconclusive answer, so no caller treats
  it as the same person); the ledger row keeps the old key as closed. The
  new account's key is pinned only when the person accepts its request. The
  old contact row and one-to-one stay, so Contacts can show the person
  twice. Adopting the new account from an outgoing add by name is not built.
- **A phrase-sealed reset and the owned channels.** The phrase opens the
  keys lock (item 1 of section 7) through the identity the phrase derives,
  which is the lineage identity only for an account that never rotated its
  keys; after a rotation a phrase-sealed reset returns the chats and not the
  owned channels.
- **The retake.** `R`, `S` and the list are read before the reset's server
  step and kept in the device's encrypted file until the new keys lock
  holds them (the web client writes the new keys lock first; the result
  after a crash is the same). A retaken private channel lets in every
  currently listed subscriber row, as the S4 owner return did; tickets are
  not checked again against the listed admission seeds. The passkey locks of
  before the reset are deleted at the first pass after the new announcement
  was read.
- **The landing rule** is applied once the account's factors were read:
  the app is on screen for that moment. Factors that cannot be read (offline)
  show no step.
- **The reset panel.** A factor row's control (the code field, `Send code`,
  `Use passkey`) waits until the password is typed or proven; the `RESET`
  field is enabled once a factor is confirmed. The web client had not built
  this panel when this was written.
- **Not exercised:** the key-log half of rule 4 (no build pins a key log),
  the bindings to the native core (`NativeSealCore`, `NativeOwnedCore`,
  `NativeResetRule`), and every screen.

*Alignment pass, 2026-11-27 (after S8/S9; host unit suite only, nothing run
on a device or against a server).* Several S9 choices above are replaced:

- **Own messages are translated** like received ones (UX 3.1, 3.7 as
  amended): no ownership argument reaches the offer rule; own messages are
  detected and translated automatically; only incoming messages feed the
  conversation's languages and the bar.
- **The password seal** is put forward only from an export key a password
  ceremony produced in this run (`ExportKeyStore.producedThisRun`, memory
  only); the clock comparison is gone. A cached key never fills in a missing
  announcement. The export key a reset writes back counts only when it came
  from the proof block.
- **The account's own devices** judge a reset made elsewhere by the core
  rule over the account's own row (`ContactsClient.ownResetVerdict`, the
  held key, the session's generation, this device's announcement floor):
  sealed is the owner's (full wipe, no "Was it you?"); anything else,
  including no answer, keeps the recall.
- **A recall** keeps the old keys lock's root in the recall record (record
  version 2; the root is read from the lineage identity before the staged
  wipe), and the channel pass opens that lock and takes the owned channels
  back through the old `R` and `S`; the recall keeps the new password's
  export key, so the announcement published after the recall carries its
  seal.
- **Durability:** a passkey's lock secret and its not-yet-announced seal
  entry are kept (encrypted file, plain public entry) until written; a lock
  write that loses is retried and, still lost, fails the pass (the list
  never names a missing lock). `R`, `S` and the retake state were already
  kept in the encrypted file.
- **The handle seed:** each owned channel with an `@name` carries its
  handle write seed in the list; a retake records it as an owed `rekey`,
  sent by the publisher pass once the channel's record is read.
- **A retaken private channel** (admission seeds listed, no read key) judges
  every row by its ticket from mark 0 (web's rule); a reader with no valid
  ticket is removed.
- **Started-over person:** adding by `@name` adopts the new account's key
  and sends the request to it; the Sent row carries the 16.11 note; the old
  one-to-one stays closed. What waited to be sent into the old one-to-one
  turns "Not sent" at once, is never sent, and the closed chat offers no
  Retry.
- **The pairing state bundle** carries `sealFloors` and `startedOver`
  (absent when empty); a receiving device keeps the higher floor and adds a
  closed key it does not hold.
- **The landing rule** waits for the factor read once per landing, at most
  3 s, behind a plain cover; a guest does not wait.
- **One safety number per conversation** (16.11): already holds on Android,
  since each contact row, the old and the new account's, computes its number
  over its own key.
- **Not exercised:** the recall retake and the own-device verdict against a
  server; the owed handle `rekey` after a retake (no host test drives the
  publisher pass).

### As built on iOS, 2026-11-27 (S9; `apps/ios`)

Built against the S6 core and the S7 services, with `LakkoCore.xcframework`
and the extension framework rebuilt first. The app builds and the unit suite
was run on a simulator (counts in the commit that lands this); nothing was
run on a device or against a server, no passkey ceremony was executed, and
no screen was walked. Where the spec or this ADR was silent, the smallest
consistent thing was chosen (the web client had built none of this when it
was written, so nothing could be matched):

- **A passkey's seal secret.** Asked as a second PRF salt (the salt of
  section 10) beside the passkey vault's, at passkey creation and at
  "Restore keys with this passkey", and as the only salt in the reset's own
  passkey ceremony. The PRF extension exists from iOS 18; below it a passkey
  signs in and confirms a reset and never seals one, its row says "Does not
  keep your chats.", and a reset on such a device is sealed by the password
  or not at all. A secret that could not be published when it was given is
  kept in memory and published at the next successful look; it is never
  stored, so a process that ends first leaves that passkey not keeping the
  chats.
- **Which password seal a device asserts.** A mark beside the export key
  says "made by the password just now" (set by sign-up, a password sign-in
  and a password change; cleared once the announcement and the owned-channels
  password lock both took it). A landing replaces the announced password seal
  only while the mark stands; otherwise it only fills in an account that has
  no announcement at its generation. Residual: a device whose cached export
  key is stale can fill in a missing announcement with an old password's
  seal (a reset from the current password is then unsealed until a device
  that holds the keys signs in with it).
- **When the announcement is published.** At every session landing, when
  the "Protect your account" step is left, after a password change, a
  passkey that gave its secret or was removed, a saved phrase, a key rotation
  (once the directory holds the new key: the password and passkey seals of
  the previous generation are carried, the phrase is not) and a reset's
  publish. The new words of a reset or a rotation are listed once the person
  confirmed them on the device (an owed mark survives a restart).
- **The reset seal.** Signed by the password's export key when the
  announcement lists it, else by the passkey used as the factor when it
  lists that one. A reset sealed by a typed recovery phrase is **not built**
  (the reset panel has no phrase field); the phrase seal is published and
  not yet usable from iOS. If discovery refuses the seal (`400
  reset-seal-invalid`) the publish goes again without it, as an unsealed
  reset, instead of leaving the account without a published key.
- **An unsealed reset at a contact.** The gate answers a distinct "started
  over" that no caller treats as the same person. The pin stays on the old
  key; the old key is recorded as closed (not as a superseded key, so
  nothing is returned on its strength). The new account's key becomes the
  pin only when the person accepts a contact request signed by it; a request
  from any other key of that account is refused. A group member known only
  by a verified binding records nothing. The old contact row and one-to-one
  stay, so Contacts can show the person twice.
- **The announcement floor** is kept for accounts this device pins, and
  survives the adoption of a new account's key.
- **The key-log half of rule 4** is asked (`GET
  /transparency/announcement/...`) and handed to the core's rule; no build
  pins a key log, so it has never run.
- **The owned channels.** `S` from sign-up is kept under the device's
  Keychain until the locks exist (a guest makes them when it chooses its
  password), and the reset's `S` with what sealed it likewise from the reset
  until the channels were taken back: section 7 says "in memory while used",
  which a crash between the two steps would turn into owned channels that
  never come back. The password lock is made again with the password under
  the same mark as the announcement. A list entry a storage or channels
  failure could not take back stays in the list; one the channels service
  refuses is dropped. A retaken private channel is not walked.
- **The landing rule** is applied once the account's factors were read; the
  app is on screen for that moment, and factors that cannot be read show no
  step. Once the step stands only its `Continue` takes it away.
- **A saved phrase on the account's other devices.** The server keeps no
  notice for it any more: a device says "A recovery phrase was saved" once
  when the account's announcement first lists the phrase.
- **Removing a passkey** had no confirmation on iOS (the passkey sheet is
  one); the app's own dialog is shown only when the removal would leave the
  authenticator app as the only second way in, carrying that note.
- **`Open in authenticator app`** hands the `otpauth://` link to the
  system; when no app takes it nothing is shown (no copy exists for it).
- **Not built on iOS:** the reset sealed by a typed phrase (above); the
  account's own devices judging a reset they did not make by its seal (every
  such reset still shows ADR-0074's "Was it you?", sealed or not); a recall
  device taking the owned channels back through the old keys lock (a
  recalled account's owned channels do not come back on iOS); the retry of a
  passkey's owned-channels lock after a failed write; adopting a
  started-over account from an outgoing add by name.

*Alignment pass on iOS, 2026-11-27 (later the same day).* Unit-tested
only (the counts are in the commit that lands this); nothing was run on a
device or against a server and no screen was walked. What changed against
the list above:

- **Own messages are translated** like received ones (UX 3.1, 3.7): the
  menu row, the language line and automatic translation; the conversation
  bar and the conversation's usual language still count incoming messages
  only (`MessageTranslationCenter`).
- **The password seal and lock come only from this run's export key**
  (`RunExportKey`, in memory): the persisted "made just now" mark is gone, a
  landing with no password proven in this run only reads the announcement,
  and the owned-channels password lock (also at a guest's or a pending
  sign-up's lock making) uses the same key.
- **A reset of this account made elsewhere is judged by its seal**: the
  device writes its recall record first, then asks the core's rule
  (`ContactsClient.judgeOwnAccountReset`, at most 10 s, chosen); sealed: the
  ADR-0073 sign-out and wipe, the record destroyed, no "Was it you?";
  unsealed, refused or no answer: the recall as before. The announcement
  floor used for the device's own account is 0 (the device keeps no `seq`
  of its own account's announcements).
- **A recall takes the owned channels back** through the old keys lock: the
  recall record keeps that lock's 32-byte root (taken from the lineage seed
  before the staged wipe); after the account is taken back the device keeps
  the new password's export key (the announcement follows at the landing)
  and a carry naming the old keys lock; the retake opens it (it holds `R`
  and `S`), makes the new keys lock and the password lock under the new
  password. A recall by phrase uses the phrase's own identity as the
  lineage, so an account that rotated its keys before gets nothing back
  that way.
- **Durability**: the reset's carry (`S` and what sealed it) is written
  before the server step and removed if nothing was reset; the retake makes
  the new keys lock first (a later attempt opens it again), records each
  channel taken back or dropped (Keychain, removed when all settled) and
  keeps the carry while one waits; a passkey lock to make or delete is owed
  under the Keychain until written.
- **The handle seed**: each list entry carries the channel's handle write
  seed (`ChannelSettings`' derivation; the predecessor's while a rotation's
  rekey is owed); after a retake the device owes the `rekey` from it to the
  key its new root derives, waits for the channel's metadata record to name
  the `@name`, then writes the name's row again under the new key and posts
  the record with the new handle block; `401` or `404` ends it.
- **A retaken private channel** whose entry lists admission seeds is judged
  row by row by its tickets (mark 0, the admission pass removes a reader
  with no valid ticket); a public one, or one with no listed seed, keeps
  every listed reader.
- **Adding a started-over person by `@name`** (typed, or from search) takes
  their new account: the pin moves to the new keys, the old one-to-one
  stays closed, and the Sent row carries the 16.11 note. Adding them to a
  group by name still refuses.
- **The old conversation's person card** (a key an unsealed reset left
  behind) offers no `Add to contacts`, also after the contact was removed
  (`StartedOverCard`): that account no longer exists.
- **What waits for the old one-to-one** is `Not sent`, never sent, with no
  Retry (waiting rows are settled when the person is met as started over,
  and at every bootstrap).
- **The pairing state bundle** carries `sealFloors` and `startedOver` (the
  agreed JSON, absent when empty); the receiver keeps the higher `seq` under
  a pin it holds and adds a closed key it lacks; a malformed entry refuses
  the whole bundle (the ADR-0080 `deviceCards` rule, as on Android). The time of a closed key is the reset's attestation time, 0 for
  a key recorded before this build.
- **The landing rule without a flash**: the app's content waits until the
  factor read answered (at most 3 s, chosen); the step is decided only from
  an answer, so a failed read shows the app and no step.
- **One safety number per conversation** (UX 16.11, last bullet) holds on
  iOS by construction: the old and the new account are two contact rows,
  each showing the number of its own key.

## As built on web (S8, 2026-11-27)

Web and the desktop app (the same code; the desktop host forwards every
`/api/<service>/` path already, so it needed no change). Unit-tested only:
`npx tsc --noEmit` clean and `npx vitest run` 848 files, 10118 tests, all
passing, on 2026-11-27. Nothing was walked on screen, against a server or in
a second browser, and no passkey ceremony was executed. iOS and Android are
S9.

**Where it is.** `apps/web/src/auth/seal/` (the announcement and the reset
seal over the wasm core, `seal-announcement.ts`; publishing and reading,
`seal-runtime.ts`; when a device publishes and touches the owned-channels
locks, `account-seal-jobs.ts`; the reset's factor rules,
`reset-factor.ts`; what the account's published protection lists, for the
page, `own-seals.ts`); `apps/web/src/channels/owned/` (the list, its locks,
the retake); `auth/ResetFactorSection.tsx` and `auth/RestoreIdentityPrompt.tsx`
(UX 15.1b, 15.1c); `auth/protect/` (UX 19.1a to 19.1d);
`contacts/reset-verdict.ts`, `contacts/started-over.ts` and
`transparency/verify.ts` (the peer side, UX 16.11).

**Chosen where sections 7, 10 and 11 or the UX spec left a detail open.**

1. *The password entry is made only from an export key a password
   registration or sign-in produced on that page* (sign-up, a password
   sign-in, the proof block, a password change, a mailed-code password change
   on a device that holds the keys). The export key a device caches may be a
   previous password's, so a plain landing only reads the announcement. A
   password sign-in on a device without the keys keeps its export key in
   memory and takes the password over if the keys arrive on that page;
   otherwise the next password sign-in on a device with the keys does.
2. *Entries are carried only from an announcement that opened under the
   account's own key.* The next announcement is the directory's current one
   (opened under the current key) with the entries this device can speak for
   replaced. After a rotation the directory has none under the new key, so
   the device carries from the announcement it kept, opened under a
   predecessor key it holds; whichever device of the account lands first
   does it, and a listed phrase entry is made again from the phrase the
   device holds now. An announcement that does not open contributes only its
   `seq`, as a floor.
3. *`seq`* is one above the greatest of the directory's current and
   previous announcements and the one this device kept (plain storage,
   `lakko:seal-announcement:<account>`; public bytes).
4. *A deleted passkey whose seal could not be withdrawn* (the directory was
   unreachable) is owed (`lakko:seal-removals:<account>`) and withdrawn at
   the next landing.
5. *The server half `S` is kept under the device key*
   (`lakko:owned-server-half:<account>`) from the moment it arrives until
   the vaults are made; a guest keeps it until its first password. After a
   reset it is kept the same way until the retake answered.
6. *The reset* asks for `S` first, then makes the seal, then resets. The
   device holds no account key, so it opens the account's current
   announcement under the key the directory publishes: that decides the
   line of UX 15.1c and which secret signs, never what a peer accepts. The
   seal is kept in the "publish owed" record so a retry sends the same one.
   When the directory answers `400 reset-seal-invalid` the new keys are
   published without it (an unsealed reset rather than a publish that never
   lands).
7. *After the reset's publish* the new generation's announcement is
   published at once when the page holds the password's export key, else at
   the next password sign-in. The retake runs when the new account lands.
   After a reload, a password-sealed retake continues with the cached export
   key and the kept `S`; a passkey-sealed one cannot (its secret was in
   memory only) and the owned channels then stay with the old account.
8. *The reset section* (UX 15.1b). The word and the button are usable only
   once a factor is confirmed and the 15.1c line is on screen; while the
   published protection cannot be read the section shows the offline line
   with `Try again` and claims neither outcome. The email row needs the
   account's username on this device (the code is addressed by its login
   handle). A mailed code is counted on the device: after five refusals the
   row returns to `Send code`, and a code past its lifetime is not sent.
   `proof.reset.cannot.title` now reads "...without your password" (a
   passkey no longer stands in for it).
9. *The peer side.* A proven rotation raises the accepted generation (the
   core requires the previous announcement's generation to equal it). A
   later fetch that shows a valid seal upgrades an unsealed verdict. An
   outgoing request to a started-over account is refused with the existing
   "could not confirm" line; only an incoming request can be accepted.
10. *The owned-channels list.* Writes are a reconcile by the publisher pass
   (it compares what the device holds with the list as last read); only
   epochs the channel reached are listed; a listed read key means public and
   the writing device's value wins, absence included; `404` and `410` on the
   owner change also drop the entry; at a retake of a private channel every
   listed row is judged by its ticket; more than 64 channels are ranked by
   this device's own rotation record.
11. *The landing rule* (UX 19.1c) reads the factors after the session is on
   screen, so an account with none sees the app for one request before the
   step replaces it.
12. *"A phrase is saved"* is the phrase entry of the account's announcement;
   the per-device flag follows it. On the restore prompt, where the device
   holds no keys, it is read from the announcement opened under the
   published key, for display only.

**Not built on web.**

- The pairing state bundle does not carry a contact's `seq` floor or its
  started-over record (a change to a format the three clients share). A
  newly paired device starts with floor 0, and an old one-to-one with a
  person whose new account was already accepted is not closed there.
- One safety number per conversation in the contact view (UX 16.11, last
  bullet): the view computes one per contact.
- Messages already queued for the old one-to-one before an unsealed reset
  was noticed are still sent into it.
- A reset made elsewhere and seen by the account's own old device is handled
  as before ("Was it you?" and recall); the sealed case is not told apart.
- A recall (ADR-0074) publishes no announcement by itself; the next password
  sign-in does.
- A retaken channel's `@name` is held by a handle key derived from the old
  account seed; the list carries that handle write seed (section 7 item 4), so the
  retaking device signs `rekey` to the key its new root derives (decided
  2026-11-27).
- An account made before this build has no list and no announcement until a
  password sign-in on a device with its keys (the announcement) and never
  gets a list unless `S` is in hand (sign-up, or a reset).

### As built on web, alignment of 2026-11-27

The same day's alignment of the three clients; web and the desktop app.
Unit-tested only (`npx tsc --noEmit`, `npx vitest run`; nothing walked on
screen or against a server). Five items of the "Not built on web" list
above are built now; the list stays as the S8 record.

- **Own messages are translated like received ones** (UX 3.1 / 3.2 / 3.7 as
  amended): the `isOwn` input is gone from the shared offer rule and from
  every web call; the conversation bar still counts incoming messages only.
- **The handle write seed in the owned-channels list** (list v2, 64 KiB):
  every entry carries the seed discovery holds the channel's `@name` under;
  while a retake still owes its `rekey` that is the listed (old) seed, else
  the one this device derives. A retake records the `rekey` from the listed
  seed to the new root's key as an owed `@name` write (`handle-owed.ts`
  `rekeyFromSeed`, in the sealed per-account row), run by the `@name`
  runtime once the channel's record has been read and its PublisherGroup
  founded again, retried until it lands.
- **A sealed reset made elsewhere is the owner's**
  (`auth/own-reset-verdict.ts` over `contacts/reset-judge.ts`, the core's
  table with this device's old key and generation): no recall record, no
  "Was it you?", the full wipe. Unsealed, refused or unreadable keeps the
  recall; the recall screen judges again at every check, so a reset that
  could not be read at sign-out still ends without the question once it can.
- **A recall takes the owned channels back** through the keys lock of the
  OLD keys: the recall record (version 2, `recall-record.ts`) keeps that
  lock's root, a phrase recall derives it from the phrase, and the retake
  opens it (`OwnedChannelsStore.openWithKeysRoot`) where no seal secret does.
  The recall publishes the new password's seal announcement with its new
  keys (the export key of the new password's OPAQUE registration).
- **Durability.** What sealed a reset, the export key in force and the old
  keys-lock root are kept under the device key until the retake answered
  (`auth/seal/owed-secrets-store.ts`), so a passkey-sealed retake survives a
  reload. A passkey's seal or lock, a deleted passkey's lock and the
  password lock that failed are owed and tried again at every landing until
  they land; an owed password lock is used again only while the account's
  announcement lists that password. An unsealed reset (no seal, no old
  keys root) now brings nothing back at once instead of staying owed.
- **Adding someone who started over by `@name` or from a search result**
  adopts the new account as the contact (the pin moves; the old one-to-one
  stays closed), and the request's row under Sent carries the 16.11 note.
- **Messages waiting to be sent into the old one-to-one** read `Not sent`
  (without `Retry`) once its person started over, and are never sent.
- **The pairing state bundle** carries `sealFloors` and `startedOver`
  (additive inside v4, absent when empty; shared JSON vector in
  `devices/state-bundle-seals.test.ts`).
- **The landing rule without a flash:** after a landing the app waits for
  the factor read (at most 3 s, chosen) before it is shown; the step is
  shown only when the read answered with no factor.
- **One safety number per conversation** holds by construction: after the
  new account is accepted the old one-to-one finds no contact row, so its
  header opens the card of its own peer key (the old number); the new chat
  and the contact show the new one. That card (over a key the account left
  behind) offers no `Add to contacts`: the key's account no longer exists.
- **Own messages' detections never feed the conversation's languages**
  (spec 3.1, as on iOS and Android): an own message is detected and
  translated, but only messages from others are counted, so a short own
  "ok" never shifts the guess.

## References

- ADR-0073 (R.2 verdict, R.4 continuity key, D2 expiry), ADR-0078 (sections 1 and 7, open question), ADR-0060 (reach keys), ADR-0054 (tokens)
- `docs/ux/identity-recovery.md` sections 15 and 16
