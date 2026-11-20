<!-- SPDX-License-Identifier: AGPL-3.0-or-later -->
<!-- SPDX-FileCopyrightText: 2026 Lakko Contributors -->

# ADR-0072: Recovery model simplification: a 12-word phrase, three words checked on the device, and a passkey that can carry keys

- **Status:** Accepted 2026-10-02 (maintainer decisions). Amended the same
  day: decision 5 was rewritten (the three words are checked on the device;
  the server-side verifier this ADR first specified was built and removed
  the same day) and decisions 8, 9 and 10 were added. Built on 2026-10-02 in
  the core, both FFI surfaces, `services/auth` and the web client, then on iOS
  and Android (both compile and pass their unit tests; see "The three clients").
  Verification is unit tests and type checks only: **nothing was walked
  live** in a browser or on a device against a server.
- **Date:** 2026-10-02
- **Amends:** [ADR-0017](0017-phrase-seeded-identity.md) (phrase length; the
  recovery credential after a fresh identity),
  [ADR-0018](0018-recovery-factor-cascade.md) (factor (d) removed, factor
  list; the Path B payload), [ADR-0049](0049-identity-recovery-word-vault.md)
  (sections 1, 3, 4, 6 and the alternatives). Supersedes none of them as a
  whole.
- **UX spec:** [`docs/ux/identity-recovery.md`](../ux/identity-recovery.md).

Renumbered from 0064 on 2026-10-13: the main branch had taken 0064 to 0071 for
other decisions. Commit messages before that date name this ADR as 0064, and so do the
migration files of this work, whose bytes are checksummed and were left as written.

## Context

### The seven routes that existed

Before 2026-10-02 an account could be brought back seven ways. ADR-0018 named
the split every one of them falls on: ACCESS (a session token the server
grants) and IDENTITY (the account's keys, which the server never holds).

| Route | Returned | Where it was built |
|---|---|---|
| Another device (ADR-0018 (a), ADR-0049 section 2, ADR-0056) | ACCESS and keys | web, iOS, Android |
| The 24-word phrase (ADR-0017) | ACCESS (recovery login + password reset) and keys | web, iOS, Android |
| Three of the 24 words (ADR-0049 section 1, the word vault) | keys (the server released the sealed phrase entropy), then ACCESS through the phrase | web and `services/auth` only |
| Passkey (ADR-0018 (b), ADR-0049 section 3) | ACCESS; keys only on web when the PRF blob was enrolled | ACCESS on all three; the PRF vault on web only |
| Authenticator code (ADR-0018 (c)) | ACCESS | web, iOS, Android |
| Email / phone code (ADR-0018 (d)) | ACCESS | server and clients |
| Guardian shares (`lakko-shamir`) | keys (the phrase entropy from three or more shares) | crate and client flows |

Behind all of them sits ADR-0017's `mintFreshIdentity`: after ACCESS, with no
route to the keys left, the person resets the account keys. The interface
calls this action **Reset account keys**; the code calls it
`mintFreshIdentity`, and it mints a new identity.

### What was wrong with it

- Two routes handed keys to parties the user cannot hold to account. Any
  three guardians together held the account, and collecting shares needed a
  channel other than the lost account. The word vault put a ciphertext of the
  phrase on the server that an operator ignoring its own counter could search
  (the invariant-#2 exception ADR-0049 section 6 proposed).
- The email / phone factor required the server to see a plaintext address
  next to an account at enroll and at recover, and returned ACCESS only.
- Three words were presented as the everyday route on a new device while
  neither native client had the flow.
- A passkey, which on a phone is the route people will reach for first
  (Face ID, Touch ID, fingerprint), carried keys on web only.

### Facts about the code before the change

Verified by reading the tree on 2026-10-02, **before** any of this ADR was
built. They describe the starting point; several no longer hold (the word
vault, `word_vault.rs`, the 24-word constants and the Shamir crate are
gone), and the line references are to the tree as it was then:

1. `lakko-mnemonic` pins `WORD_COUNT = 24` and `ENTROPY_LEN = 32`
   (`crates/lakko-mnemonic/src/mnemonic.rs:23,35`); `from_phrase` refuses any
   other count (`:79`), `from_entropy` any other length (`:178`).
2. The identity is derived from the **64-byte BIP39 PBKDF2 seed**, not from
   the entropy: `to_seed` (`mnemonic.rs:137`), and the three seed stores pin
   64 bytes (`apps/web/src/auth/account-identity-seed-store.ts:70`, Android
   `AccountIdentitySeedStore.kt:25,40`). A BIP39 seed is 64 bytes for any
   word count, so nothing downstream of `to_seed` depends on 24 words. The
   recovery OPAQUE registration and login take the same 64-byte seed
   (`services/auth/src/opaque.rs:18,1031`).
3. The backup envelope key is `argon2id_kek(phrase, salt)`
   (`crates/lakko-backup/src/envelope.rs:122,190`), and each client caches
   the **phrase** under its device key for that purpose
   (`apps/web/src/backup/phrase-store.ts`, iOS `Backup/PhraseStore.swift`,
   Android `backup/PhraseStore.kt`). No length assumption in the envelope
   code; which exact bytes each client passes as `phrase` was not verified.
4. The word vault stores `seal_with_key(vault_key, verifier(32) ||
   sealed_entropy)` in `word_vaults.body_enc`; `/vault/recover/finish`
   returns `sealed_entropy_b64` on a match
   (`services/auth/src/word_vault.rs:179-181,417-420`). The 15-minute limit
   is `limits::Scope::VaultGuess`, `Window::new(5, 900)`, keyed on the login
   handle and charged before the lookup (`services/auth/src/limits.rs:215`,
   `word_vault.rs:343`); the tenth consecutive miss leaves a tombstone
   (`word_vault.rs:54,382-392`, migration `0023`). `word_vaults.last_failed_at`
   was dropped by migration `0020`.
5. `POST /totp/recover` returns the session triple with `session_token` and
   `session_expires_at_ms` (`services/auth/src/totp.rs:298-304`).
6. `/password/reset/{start,finish}` are anonymous and authorised by a
   recovery OPAQUE proof, that is, by the phrase seed
   (`services/auth/src/main.rs:659-666`, `opaque.rs:1149,1279,1321`).
7. The passkey vault has two routes, `PUT /vault/passkey` and `POST
   /vault/passkey/get` (`main.rs:535-536`); there is no delete route. The
   client side exists on web only: `apps/web/src/recovery/passkey-vault.ts`
   (fixed PRF salt `"lakko/passkey-vault/prf/v1"`),
   `apps/web/src/auth/webauthn-client.ts:333-347,431-447,772-806` (requests
   PRF, strips its output before the server sees the response), and wasm
   `passkeyVaultSeal` / `passkeyVaultOpen`
   (`crates/lakko-ffi-wasm/src/word_vault.rs:161,178`). The key is
   `HKDF-SHA-256(prf_output, "lakko/passkey-vault/seal/v1")`
   (`crates/lakko-mnemonic/src/vault.rs:69,286`).
8. `lakko-ffi-uniffi` has no word-vault or passkey-vault function, and
   neither native client references `/vault/*` (a search of
   `apps/ios/Lakko`, `apps/android/app/src/main` and
   `crates/lakko-ffi-uniffi/src` for `vault/enroll`, `vault/passkey`,
   `wordVault` found nothing). Neither native client requests PRF (a search
   for `prf` in both finds only OPRF).
9. Both native `RecoverScreen`s state that they have no three-word flow
   (`apps/android/.../ui/auth/RecoverScreen.kt:33-35`,
   `apps/ios/Lakko/UI/Auth/RecoverScreen.swift:10-13`); their mode lists are
   phrase, another device, authenticator (`RecoverViewModel.swift:38`,
   `RecoverViewModel.kt:402`).
10. Both native clients refuse a passkey sign-in on a device with no cached
    account (`apps/android/.../auth/AuthRepository.kt:2262-2280`,
    `apps/ios/Lakko/Auth/AuthRepository.swift:801-817`). The server supports a
    discoverable (no account id) passkey start
    (`services/auth/src/webauthn.rs:998-1006`).
11. A web device that holds its keys restores a session, and lands an
    ACCESS-only recovery, from the seed cached under the non-extractable
    device key without the password (`apps/web/src/auth/silent-restore.ts`,
    `AuthContext.tsx:1145-1227` `landAfterAccessRecovery`).
12. Web enrolls the word vault automatically (`ensureWordVaultEnrolled`,
    `AuthContext.tsx:1028,1065,1527,2541,2813`) and, per ADR-0049's status
    section, the passkey vault "whenever a ceremony yields a PRF output on a
    device holding the phrase".
13. The invariant-#2 exception text ADR-0049 section 6 proposed was never
    added to the invariant list in `CLAUDE.md`; it lives in ADR-0049, in the
    `word_vault.rs` module doc and in migration `0017`'s header.

## Decision

The decisions are the maintainer's, made on 2026-10-02. Decisions 1-4, 6 and
7 stand as first recorded. Decision 5 was changed later the same day, and
decisions 8-10 were added then. They are recorded here with their reasons
and are not re-opened by this document.

### 1. Guardian shares are removed

Any three guardians together hold the account, and collecting shares needs a
channel other than the lost account. `crates/lakko-shamir`, its FFI modules
and the client flows were deleted the same day.

### 2. The email / phone code (ADR-0018 factor (d)) is removed

Sending a code requires the server to see the plaintext address next to the
account, at enroll and again at recover, so the operator can always map an
address to an account; the maintainer does not accept that under a zero-trust
server. A code from an email can also only ever return ACCESS. An email-only
lookup variant was considered and rejected for the same first reason.

Left as future work, undesigned: a recovery file the user holds and may mail
to themself, about which the server knows nothing.

### 3. The recovery phrase is 12 words

BIP39, English, 128 bits of entropy plus a 4-bit checksum, 12 words. Flag
day: there is no production deployment and there are no users, so there is no
migration and no dual format; a 24-word phrase is refused after the change.
This reverses ADR-0049's rejection of typing twelve words: that rejection was
made when the phrase was the everyday route on a new device, and after this
ADR it is the third route behind another device and a passkey.

### 4. A new device gets its keys from the whole phrase

Typing words restores keys only when all twelve are typed. The phrase is
checked locally (BIP39 checksum), then against the account's published
identity bundle (ADR-0017, fail-closed), and the same phrase runs the
existing recovery login and password reset.

### 5. Three words are checked on the device that holds the phrase

"I forgot my password and this device still has my keys" is answered the way
a crypto wallet answers it: the device asks for three words of the phrase
("Type words 7, 3 and 12") and compares them with the phrase it holds. The
server takes no part and stores nothing about the three words.

- **Positions are random per prompt.** `lakko_mnemonic::word_check::
  random_check_positions` draws three distinct positions in `1..=12`,
  uniformly (rejection sampling, 252 = 21 x 12), in the order drawn, not
  sorted. A miss draws new positions. There are no per-account positions, so
  nobody learns to keep only three words.
- **The comparison is `check_words(mnemonic, positions, words)`:** typed
  words are trimmed and lower-cased; a wrong or unknown word is `false`;
  only malformed input (not three positions, a position outside `1..=12`, a
  repeated position, not three words) is an error. The rule is written once
  in the core so the three clients cannot disagree.
- **On a match** the client signs in with the credential it already holds:
  the 64-byte seed of the stored phrase runs the unchanged
  `/recovery/login/{start,finish}` for the login handle of the cached
  username, the returned account id must be the cached one, and the device
  lands through the shared access-only landing. "Choose a new password"
  follows, which uses the same seed through `/password/reset/*`.
- **The budget is local:** 5 misses, the fifth starts a 15-minute pause, the
  count restarts at zero after the pause, a clock set back never yields more
  than 15 minutes, the state is stored per account and survives a restart,
  and it is cleared on success, on forgetting the device and on account
  deletion. A failure after a match (rate limit, offline, a stale phrase)
  never charges it.
- **What the budget is.** It is a client-side gate in front of a credential
  the device already holds. The device has the phrase and the seed under its
  device key; the gate stops a person holding an unlocked device, not code
  running in the device's profile, which can read the phrase store through
  the same device key and skip the gate. No server enforces it and none
  could: the server cannot tell a sign-in that passed the gate from one that
  skipped it.
- **The row is offered only where it can work:** on a device that holds the
  phrase and the seed for the cached account, when that account has a
  username. This is a local fact, so showing or hiding the row reveals
  nothing about the account to anyone who does not hold the device.
- **No setting.** There is nothing to enroll, turn on or turn off.

Why this is better than the server-held verifier this ADR first specified:

- **Nothing about the phrase is on the server.** A verifier of three words
  let a party holding the database and the vault key search 2^33 answers
  offline and learn three words. Now there is no such value.
- **No enrollment** (so no signup step that can fail, no `POST /vault/enroll`
  a stolen session could use to plant its own three words), **no tombstone**
  (so nobody who knows a username can switch the shortcut off by guessing
  wrong ten times), no decoys, no Argon2id cost to measure on phones.
- **Three words no longer grant a session on a device without keys.** Under
  the server verifier, a correct answer returned a session token to whoever
  typed it, on any device. A session on a device without keys is enough to
  reach ADR-0017's "Reset account keys". So a party that knew a
  username and three words of the phrase could sign in elsewhere and replace
  the account's identity. That path is closed: three words now do something
  only on a device that already holds the whole phrase.
- **The ADR-0049 invariant-#2 exception is withdrawn** for a stronger reason
  than before: the word vault does not exist in any form.

What it costs: the phrase must be on every device with keys (decision 8),
and the guess budget is no longer enforced by anything the device's owner
does not control. Both are accepted: the device already holds the seed,
which is the stronger secret.

### 6. A passkey is a separate way to recover keys, beside the phrase

The PRF output of a passkey wraps the 16-byte phrase entropy into an opaque
server-held blob, and on a new device a synced passkey unwraps it. The
fail-closed check of the restored phrase against the published identity
bundle still applies. Where the OS or the credential provider gives no PRF
output the passkey returns ACCESS only and the UI says so.

The maintainer accepts that key safety then also rests on the user's Apple /
Google / password-manager credential sync: "the user decides how much they
trust Apple and the others". The choice must therefore be **informed and
explicit at enrollment**: storing the blob is opt-in per passkey, behind a
screen that says what it means (UX spec 10.2). Adding a passkey stores
nothing by itself. The earlier automatic enrollment on web is removed.

**Amended 2026-11-11 (maintainer): every passkey restores keys, with no
choice.** The opt-in screen above asked a question a person cannot weigh
("log in only, or restore keys too?") at the moment they add a passkey, and
the answer that kept the most people safe was the one it made them reach
for. Now every passkey added on a device that holds the phrase stores the
blob, in the same flow as its creation (from the creation result when the
provider returns a PRF output there, else with one more passkey prompt); a
passkey that cannot carry keys (no PRF output) logs in only and says so,
and a passkey that holds no keys (made before this amendment, refused the
second prompt, or the account keys changed) offers one action on its row,
"Restore keys with this passkey". There is no "Stop restoring keys": a
person who does not want a passkey to hold keys removes the passkey. The
cost is unchanged in kind and now applies to everyone with a passkey: key
safety also rests on the person's credential sync (Apple, Google, a password
manager). UX spec 10.2 as amended.

At the same time the server registers every passkey as a **discoverable
credential** (`residentKey: required`, `services/auth/src/webauthn.rs`
`require_discoverable`). webauthn-rs's passkey builder asked for a
non-discoverable one, and Chrome's built-in authenticator then made a
credential that only a sign-in naming the account could use, so a new
device, which names no account (decision 10), could not sign in with it.

A passkey is not a replacement for the phrase: the phrase is still generated,
shown and confirmed at signup, and is the only route that depends on no
device and no third party.

### 7. Another device is the default route

Listed first on the recovery screen and on the restore prompt, with "if you
have one". Before a session the server must not reveal whether the account
has other devices (an existence oracle), so this is ordering and copy, not a
server lookup; the list is identical for every account, except the
three-word row, which depends on a local fact (decision 5).

### 8. Every device with keys holds the phrase; the another-device transfer carries its entropy

Decision 5, the passkey opt-in, "Show recovery phrase" and handing keys to a
further device all need the phrase, not only the seed, and the phrase cannot
be recovered from the 64-byte seed. So every path that lands a device with
keys also stores the canonical phrase string under the device key: signup,
guest signup, phrase recovery, the phrase typed in the restore prompt (which
a passkey restore also ends in), another-device pairing and restore, and a
fresh identity.

The another-device transfer (ADR-0018 Path B) changes format to carry it:

- key = `DeviceGroup::export_secret("lakko/device-recovery/v2",
  rendezvous_pseudonym(32), 32)`
- row = ASCII `"lakko/recovery-payload/v2"` followed by
  `seal_with_key(key, grant_token(32) || phrase_entropy(16))`
- the plaintext is exactly 48 bytes.

Version 1 (label and magic ending `/v1`, 96 bytes: the grant and the 64-byte
seed) is refused. Flag day, no dual accept: a v1 payload would land a device
with keys and no phrase, which is the state this decision rules out.

The sender takes the entropy from its phrase store; a sender with no stored
phrase publishes no recovery row and tells the person. The receiver opens the
row, requires 48 bytes, redeems the grant, rebuilds the phrase and the seed
from the entropy, runs the fail-closed check against the published identity
**before any write**, then stores seed, phrase and session, and zeroes the
entropy, seed and phrase bytes on every refusal.

### 9. After a fresh identity the recovery credential is replaced

**The finding.** ADR-0017's `mintFreshIdentity` mints a new phrase for an
account that already has ACCESS. The server's recovery credential (the
OPAQUE registration `KIND_RECOVERY`, made from the phrase seed at signup)
was never touched by it. So after resetting the account keys, the **old** phrase still
ran `/recovery/login` and `/password/reset`, and the **new** phrase, the one
the person had just written down, ran neither.

**The routes** (bearer; the session names the account, nothing in the body
does):

- `POST /recovery/credential/start {recovery_request}` ->
  `{recovery_response}`. Stateless, like the recovery half of
  `/signup/start`.
- `POST /recovery/credential/finish {recovery_upload}` -> `{ok: true}`.
  One transaction: the account's in-flight recovery-login and password-reset
  states are deleted and the `KIND_RECOVERY` registration is upserted, so
  there is no moment at which neither or both phrases are registered. The
  password registration is untouched.

The messages are built exactly as signup builds its recovery half from the
64-byte seed.

**Why there is no passkey gate** (ADR-0014 action grant), even when the
account has a passkey: the route exists for the lost-everything path, where
the passkey may be gone too, and the same session can already mint the fresh
identity this credential belongs to. A gate would protect less than what the
session token already allows.

**The limit.** Both calls are charged to a per-account counter
(`limits::Scope::RecoveryReplace`, default 10 per hour,
`LAKKO_AUTH_LIMIT_RECOVERY_REPLACE`; one whole replace costs two), answered
`429 too-many-attempts` with a retry time; `503 rate-limiter-unavailable`
when the counter store fails (closed).

**Client order** (web `AuthContext.mintFreshIdentity`): generate the phrase
and seed, publish the identity bundle (a failure here changes nothing
locally), write locally, mark "recovery credential owed", call the replace,
clear the marker on success, and only then show the new phrase. While the
replace is owed the words are not shown, because they would not sign the
person in; the screen offers "Try again" (which repeats only the replace and
never mints again) and "Log out", and the next session landing retries.

**The stated residual.** A stolen session can call these routes with a seed
of the thief's choosing. That retires the owner's phrase as an ACCESS
credential (it still derives the owner's keys) and gives the thief a
recovery login that outlives the session. Existing sessions are not revoked
by a replace. This is in the class ADR-0017 already accepted for
`mintFreshIdentity` (a stolen session can replace the identity itself), and
is recorded, not closed. A recovery login that read the old registration
just before the commit can still finish within the login-state lifetime.

### 10. The phrase grid

Three columns by four rows at phone width, four columns by three rows on
wide screens, numbered **row-major** (1 2 3 / 4 5 6 / ...), identical for
display, confirmation and entry on every client. Slot numbers are Latin
digits in every language.

### The resulting model

| Situation | Routes, in order |
|---|---|
| New device, keys needed | another device -> passkey (when it carries keys) -> 12-word phrase -> Reset account keys (ADR-0017 `mintFreshIdentity`, after ACCESS; decision 9 follows it) |
| Forgot the password, device still has keys | passkey, three words (checked on the device), authenticator code. Each signs the person in; the client then offers "Choose a new password" from the seed the device holds |

Every route ends in the existing ADR-0017 identity-acquisition state machine;
an ACCESS-only route on a device without keys parks in
`needs-recovery-phrase` and the restore prompt offers another device, a
passkey and the phrase, never the three words or the authenticator.

## What changes, per component

As built on 2026-10-02 unless a line says otherwise.

### `lakko-mnemonic`

- `WORD_COUNT` 12, `ENTROPY_LEN` 16, `SEED_LEN` 64 unchanged; `generate`,
  `from_phrase`, `from_entropy`, `to_entropy` follow.
- `word_check.rs` (new): `CHECK_WORDS = 3`, `random_check_positions`,
  `check_words`, `WordCheckError` with stable kebab codes
  (`wrong-position-count`, `position-out-of-range`, `repeated-position`,
  `wrong-word-count`).
- `vault.rs` is the passkey vault only: `seal_entropy_under_secret` /
  `open_entropy_under_secret` under `HKDF-SHA-256(prf_output,
  "lakko/passkey-vault/seal/v1")`; the sealed blob of a 12-word phrase is 44
  bytes; `Mnemonic::from_entropy` refuses anything but 16 bytes, so a blob
  sealed for a 24-word phrase fails closed. The word vault (positions, salt,
  Argon2id stretch, verifier, sealed entropy) is deleted.
- `lakko-mnemonic` is not `lakko-mls`; invariant #3 is not touched.

### Both FFI surfaces

- `RecoveryMnemonic`: 12 words, `toEntropy()` 16 bytes, `fromEntropy`,
  `toSeed()` 64 bytes.
- `phraseCheckRandomPositions()` (3 bytes, distinct, `1..=12`, drawn order)
  and `phraseCheckWords(mnemonic, positions, words) -> bool` on UniFFI and
  wasm (`phrase_check.rs` in each).
- `passkeyVaultSeal(mnemonic, prfOutput)` (44 bytes) and
  `passkeyVaultOpen(prfOutput, sealed) -> RecoveryMnemonic` on UniFFI (new
  there) and wasm (`passkey_vault.rs` in each).
- UniFFI `FfiError` gains, **appended** (the ordinal rule), `PhraseCheck`,
  `PasskeyVaultNotOpened` and `PasskeyVault`; wasm throws an `Error` (the
  phrase check with a `"<code>: <reason>"` message).
- Removed from both: every Shamir function and `FfiError::Shamir`, every word-vault
  function.
- Not on `lakko-ffi-extension`.
- The native libraries (`LakkoCore.xcframework`, the Android `.so`) have to
  be rebuilt from this tree.

### `services/auth`

Four migrations:

| Migration | What it does |
|---|---|
| `0026_drop_recovery_contacts.sql` | Drops `otp_challenges` and `recovery_contacts` (factor (d)). |
| `0027_word_verifier.sql` | The first form of decision 5: emptied `word_vaults` and `passkey_vaults` (flag day, values computed for 24-word phrases), turned `word_vaults.body_enc` into a nullable `verifier_enc` with an `off_reason`, and widened the `passkey_vaults.body_enc` comment. Its `word_vaults` half is moot since 0029; its `passkey_vaults` half stands. Left as written because migration files are checksummed. |
| `0028_login_handles_account_pk.sql` | Re-keys `login_handles` on `account_id` (primary key) with a unique index on `handle_key`. Fixes a deadlock (MariaDB 1213) found while testing concurrent binds: with `handle_key` as the primary key and a unique index on `account_id`, whose entries for new accounts are neighbours because account ids are UUIDv7, (1) two first binds each ran `DELETE ... WHERE account_id = ?` for a row that did not exist, each took the same gap lock and waited for the other's insert, and (2) a change of handle deleted and re-inserted the row, and the duplicate-key check on the unique `account_id` index took next-key locks reaching the neighbouring account. Keyed on the account, a first bind is a plain insert and a change is an update of one row by primary key. Not specific to recovery; it landed in this arc. |
| `0029_drop_word_vaults.sql` | Drops `word_vaults` and `vault_opens` and deletes `auth_name_limits` rows of scope 5. |

- **Removed routes:** every `/recovery-contact/*`; `GET /vault`,
  `POST /vault/enroll`, `DELETE /vault`, `POST /vault/recover/start`,
  `POST /vault/recover/finish`. `word_vault.rs` is deleted.
- **Passkey vault** (`passkey_vault.rs`, all bearer):
  `PUT /vault/passkey {credential_id_b64, sealed_entropy_b64}` (the blob must
  be exactly 44 bytes and the credential one of the account's passkeys) ->
  `{stored: true}`; `POST /vault/passkey/get {credential_id_b64}` ->
  `{sealed_entropy_b64: string | null}`; `DELETE /vault/passkey` (JSON body
  `{credential_id_b64}`) -> `{deleted: bool}`; `GET /vault/passkeys` ->
  `{credential_ids_b64: [...]}`. `DELETE /webauthn/credentials/{id}` also
  deletes that passkey's blob. `LAKKO_VAULT_ENC_KEY` wraps the blobs at
  rest; unset means `503` on the passkey-vault routes and nothing else.
- **Passkey sign-in with no cached account:** `POST /webauthn/auth/start`
  with body `{}` starts a discoverable ceremony (this existed before the
  ADR); the passkey-vault tests cover a device with no cached account
  signing in and fetching its blob.
- **Recovery credential:** the two routes of decision 9, in `opaque.rs`.
- **Limits** (`limits.rs`): scope 5 (`VaultGuess`) is removed and its number
  is not reused, like 6-8 of factor (d); scope 12 `RecoveryReplace` is new.
  The surviving name scopes are 1-4, 9, 10, 11, 12.
- **Unchanged and used by the flows:** `/recovery/login/{start,finish}`,
  `/password/reset/{start,finish}`, `/totp/recover`, `/device/*`.

### Password replacement from a device that holds the keys

No server change. `/password/reset/*` accepts a recovery proof made from the
64-byte seed. The web client's `replacePassword` runs it from the cached
seed for the session's username, refuses if the returned account differs,
and installs the new session token and export key under the **existing**
device key (the device key is not re-derived and no MLS or DeviceGroup
snapshot is deleted). It is offered after a sign-in-only route on a device
with keys and as a Password card in Settings > Security; it needs a username
and is not offered to a guest account.

### The three clients

- **Web: built** (type checks and unit tests; not walked in a browser
  against a server). 12-word inputs and the grid of decision 10; the
  recovery list in two groups; the local three-word flow
  (`auth/recover/three-words.ts`, `WordsRecover.tsx`); passkey sign-in from
  the list and from Login with PRF requested and never sent to the server;
  the restore prompt without three words or the authenticator; the opt-in
  passkey vault and "Stop restoring keys"; the signup passkey step;
  "Choose a new password"; Settings > Security > Recovery
  (`settings/RecoveryPage.tsx`); the phrase on every landing path
  (`backup/device-phrase.ts`, with a test that fails when a landing path
  does not store it); the v2 transfer (`devices/recovery-payload.ts`); the
  fresh-identity credential replace with its pending state.
- **iOS and Android: built on 2026-10-02** to the same model; they compile
  and pass their unit tests (iOS `LakkoTests` on a simulator with the core
  rebuilt; Android on the host JVM, which does not load the native core, and
  the Android `.so` was not rebuilt). Nothing was walked on a device and no
  passkey ceremony or PRF evaluation was executed: PRF on iOS needs iOS 18
  (`ASAuthorizationPlatformPublicKeyCredential*Request.prf`); on Android
  below API 34 the Play Services bridge drops a PRF request at registration.
  Android gates the three-word button on the BIP39 list, from a generated
  copy checked against the web list by a test.
  Both got, for the first time, the local
  three-word flow, PRF with the same fixed salt
  (`lakko/passkey-vault/prf/v1`), and a passkey sign-in on a device with no
  cached account.

### ADR-0017's derivation

Unchanged: `AccountIdentity = Identity::from_seed(bip39_seed)` with the
64-byte PBKDF2 seed and the empty BIP39 passphrase. What changes is the
entropy behind the seed (256 -> 128 bits), every place that handles the
**entropy** or the **words** (the passkey blob, the phrase store, the v2
transfer, the inputs), and the recovery credential after a fresh identity
(decision 9).

## Security consequences, per invariant

**#1 (no PII) improves.** With factor (d) gone the server stores no contact
hash at all; nothing derived from a phone number or an address remains in
`services/auth`.

**#2 (ciphertext and public artefacts only).**

- **The invariant-#2 exception ADR-0049 section 6 proposed is withdrawn.**
  The word vault no longer exists in any form: no sealed entropy, no
  verifier, no positions, no salt. Nothing about the three words is stored
  on or sent to a server, so there is nothing for an operator to search.
  (The exception text was never added to the invariant list in `CLAUDE.md`;
  it lived in ADR-0049, in the deleted `word_vault.rs` and in migration
  `0017`'s header, which stays as written because migrations are
  checksummed.)
- The passkey vault stores a ciphertext under a key derived from a 32-byte
  PRF output, wrapped again under the server key. The server cannot search
  it. Whoever can use the passkey can: they sign in with it, fetch the blob
  and open it. For a synced passkey that includes the sync provider to the
  extent the provider can use the user's passkeys. That is the trust the
  maintainer accepted in decision 6 and the reason enrollment is opt-in.
- **128-bit phrase entropy.** The identity keys are Ed25519 / X25519, about
  128-bit security, so the phrase no longer exceeds the keys it seeds; it
  matches them. BIP39's PBKDF2 uses a constant salt, so a search across N
  accounts' public keys finds one of them in about 2^128 / N work; with any
  plausible N this stays out of reach. The backup KEK is
  `argon2id_kek(phrase, salt)`: against a 128-bit input the Argon2 cost is
  irrelevant to feasibility (it matters only for weak inputs), and the KEK's
  strength is the phrase's.

**#3** untouched (no `lakko-mls` change; the v2 transfer uses the existing
`DeviceGroup::export_secret` with a new label). **#4** every decision point
fails closed: a missing or unopenable passkey blob, a restored or
transferred phrase that does not match the published identity, a v1 transfer
payload, a three-word sign-in whose account id is not the cached one. A
passkey without PRF degrades to ACCESS only, visibly, never to a weaker way
of obtaining keys. **#5** untouched. **#6** no rotation on any keys route:
each returns the existing seed; the one rotation path stays
`mintFreshIdentity`, now followed by decision 9. **#7** the phrase on a
device is wrapped under the same hardware-backed device key as the seed; the
passkey's own secret lives in the platform authenticator.

**The phrase at rest on every device with keys.** Each such device already
held the 64-byte seed, which is what derives the keys and what
`/recovery/login` and `/password/reset` accept. The phrase adds the
human-readable form and the ability to hand keys onward; it does not add a
capability over the account that the seed lacked. It does mean that "Show
recovery phrase" can display it, behind re-authentication (the account
password on web; the device unlock on native, when built).

**The three-word gate is not a server control.** See decision 5: it stops a
person at an unlocked device, not code in the device's profile.

**A stolen session and the recovery credential.** See decision 9: a stolen
session can retire the owner's phrase as an ACCESS credential, and existing
sessions are not revoked. Recorded as a residual.

**A stolen session and the passkey vault.** `PUT /vault/passkey` accepts a
blob for any passkey of the account from any session. The blob is useful
only to whoever can produce that passkey's PRF output, so a planted blob
cannot give the thief keys; it can replace a working blob with one that does
not open (the passkey then signs in without keys, visibly). Not gated by an
ADR-0014 action grant today.

Flag for `lakko-security` before merge (auth wire change, a new bearer route
that replaces a credential, a new native keys route, removal of a stated
invariant exception), on explicit request.

## Alternatives rejected

- **Email-only lookup** (keep the address as an account locator, send
  nothing key-bearing). The server still sees the plaintext address beside
  the account at enroll and at recover; the operator can map one to the
  other. Rejected by the maintainer.
- **Keep the sealed-entropy word vault** (ADR-0049). Keeps three words as a
  keys route and keeps the invariant-#2 exception: a server-held ciphertext
  of the phrase behind 33 bits and a counter only the operator enforces.
- **A server-held verifier of three fixed words, returning a session.** This
  was decision 5 as first written, and it was **built and removed the same
  day** (`0027_word_verifier.sql` is its trace; `0029` removes it). It
  stored no key material, but: a party with the database and the vault key
  could search 2^33 answers and learn three words of the phrase; it needed
  enrollment, a tombstone anyone knowing the username could trigger, decoys
  and an unmeasured Argon2id cost on phones; a stolen session could plant
  its own three words; and a correct answer gave a session on a device
  without keys, from which "Reset account keys" was reachable.
  The local check has none of these and needs no server.
- **Passkey as the replacement for the phrase.** Would make the account's
  keys depend entirely on a third party's sync and on PRF support that is
  uneven and unverified on our target platforms; an account whose only
  passkey has no PRF would have no keys route at all.
- **Keep 24 words.** Twice the typing on the one route that needs no device
  and no third party, for entropy above what the derived keys provide.
- **Keep carrying the 64-byte seed in the another-device transfer.** The
  receiving device would hold keys and no phrase (decision 8).
- **A passkey action grant on the recovery-credential replace.** See
  decision 9.

## Open questions

1. **PRF availability was not verified on any device.** Which iOS version
   exposes PRF for platform passkeys through `ASAuthorization`, which Android
   and Credential Manager versions and which providers (Google Password
   Manager, third-party managers) return it, and whether it is returned at
   creation or only at assertion. Nothing in this ADR assumes an answer.
2. **Do the three clients get the same PRF output for one synced passkey and
   one salt?** The web API hashes the salt before the authenticator sees it;
   whether the iOS and Android APIs apply the same transformation is to be
   verified. If they differ, a blob stored on one client fails to open on
   another (closed, not unsafe).
3. **Does a hybrid / cross-device passkey ceremony (a phone used through a QR
   code on a laptop) return PRF output?** Not verified. If not, that ceremony
   is ACCESS only and the restore prompt says so.
4. **Relying-party setup across clients.** A passkey restores keys on a new
   device of another platform only if web, iOS and Android share the relying
   party (`assetlinks.json` exists under `apps/web/public/.well-known`; the
   Apple association file and both apps' entitlements were not verified).
5. **A stale phrase on another device: handled by suspension, not by
   removal** (built on all three clients the same day). When the recovery
   login made with the stored seed is refused as invalid credentials and no
   recovery-credential replace is owed on the device, the three-word route
   is suspended for the account (a per-account marker next to the guess
   budget): the row is not offered, the form says the saved phrase did not
   log in, and Settings shows the paused line. The phrase is NOT deleted: a
   `401` is not proof of staleness (an unbound login handle answers the same
   way) and a deleted phrase cannot be recovered on that device. The marker
   is lifted when a landing path writes the phrase store again and on forget
   device / account deletion. A network error, `429` or `503` never
   suspends. What remains open: nothing tells the stale device that its
   IDENTITY seed is stale too; its another-device transfer would hand over
   entropy the receiver refuses at the published-identity check, and the
   passkey keys choice now refuses to seal a stored phrase whose seed differs
   from the session's identity seed.
6. **"Show recovery phrase" while a recovery-credential replace is owed:**
   withheld on all three clients (built the same day): Settings shows the
   pending explanation with Try again, which runs the replace, and reveals
   the words only once nothing is owed.
7. **Should a recovery-credential replace revoke the account's other
   sessions, and should storing a passkey blob need an ADR-0014 action
   grant?** Not decided.
8. **The user-held recovery file** (decision 2's future work): undesigned.

Questions the first version of this ADR asked and that are now moot: the
Argon2id cost of the verifier, the 33-bit leak of a searched verifier,
whether three-word enrollment at signup should be automatic, and whether
replacing the verifier should need a passkey grant. There is no verifier.

## Amendments to earlier ADRs

Written into those files as dated notes at the top on 2026-10-02; their
bodies are left as they were.

- **ADR-0017:** the phrase is 12 words; the derivation is unchanged. After
  `mintFreshIdentity` the recovery credential is replaced (decision 9).
- **ADR-0018:** factor (d) is removed. The server-issued ACCESS factors are
  (a) another device, (b) passkey, (c) TOTP; (b) may also carry keys through
  the passkey vault. The three words are not a server factor: they are a
  local gate in front of the recovery login. Path B carries the 16-byte
  phrase entropy (decision 8).
- **ADR-0049:** section 1 (the word vault) is gone; three words are checked
  on the device (decision 5). Section 3 (passkey-carried identity) is
  opt-in. Section 4 ("Twenty-four words stay, as the reserve") is replaced
  by decisions 3 and 4. Section 6's invariant-#2 exception is withdrawn.
  Alternative "Twelve words, or all twenty-four, everywhere" is no longer
  rejected. Sections 2 (the another-device confirmation) and 5 stand.
- **ADR-0056** is unaffected.

## Sprint plan

| Sprint | Scope | State on 2026-10-02 |
|---|---|---|
| S0 UX | `docs/ux/identity-recovery.md`; `client-redesign.md` 5.6; catalog keys | Done; the spec was brought in line with the build the same day |
| S1 core | `lakko-mnemonic`: 12 words / 16 bytes; `word_check`; passkey seal over 16 bytes; Shamir crate deleted | Built; unit tests in the crate |
| S2 FFI | wasm and UniFFI: `phraseCheck*`, `passkeyVault*`; Shamir and word-vault functions removed; bindings regenerated | Built. The native libraries must be rebuilt from this tree |
| S3 services | migrations 0026-0029; routes removed; passkey-vault put / get / delete / list and the credential-delete cascade; recovery-credential replace; limits | Built; database-backed tests exist (they skip silently without `LAKKO_AUTH_TEST_DATABASE_URL`) |
| S4 web | everything under "The three clients" | Built; `tsc` and vitest. **Not walked in a browser** |
| S5 iOS | the same model, plus the three-word flow, fresh-device passkey sign-in and PRF, all new on this client | Built; unit-tested on a simulator; not walked |
| S6 Android | the same as S5 | Built; host unit tests; native core not rebuilt; not walked |
| S7 device matrix | open questions 1-4 answered by measurement: PRF per OS version and provider, cross-client equality of the output, hybrid ceremonies, relying party | Unbuilt |
| S8 walk | every route walked on two devices per `docs/PRODUCT-READINESS.md` | Not done on any client |
| S9 docs and security | amendments in ADR-0017 / 0018 / 0049; `CLAUDE.md`; security docs; `PRODUCT-READINESS.md` | Docs done 2026-10-02; a `lakko-security` pass on request is open |

Also unbuilt: open questions 5 and 6.

## Consequences

- **Positive:** nothing about the phrase on the server except an opt-in
  passkey blob the server cannot open; no contact hash; no guardian quorum;
  one phrase length that people can type; three words that need no setup and
  cannot be used away from a device that holds the keys; a keys route on
  phones that uses the device's own biometric prompt; a new phrase that
  works after resetting the account keys, and an old one that stops.
- **Negative:** three words do nothing on a new device, so a person with no
  other device and no keys-carrying passkey types twelve words; phrase
  entropy halves to 128 bits; an opted-in passkey makes the account's keys
  as safe as the passkey's sync; the three-word budget is enforced only by
  the client; every device with keys holds the phrase; a stolen session can
  retire the owner's phrase as an ACCESS credential.
- **Neutral:** `LAKKO_VAULT_ENC_KEY` remains a secret to back up (losing it
  costs passkey restores, never the phrase); the identity-acquisition state
  machine is unchanged; ADR-0017's supersession machinery stays dormant.

## References

- [ADR-0014](0014-webauthn-action-binding.md),
  [ADR-0017](0017-phrase-seeded-identity.md),
  [ADR-0018](0018-recovery-factor-cascade.md),
  [ADR-0049](0049-identity-recovery-word-vault.md),
  [ADR-0054](0054-anti-abuse-without-client-work.md),
  [ADR-0056](0056-pairing-names-account-by-handle.md)
- `crates/lakko-mnemonic/src/{mnemonic,word_check,vault}.rs`,
  `crates/lakko-ffi-uniffi/src/{phrase_check,passkey_vault}.rs`,
  `crates/lakko-ffi-wasm/src/{phrase_check,passkey_vault}.rs`,
  `services/auth/src/{passkey_vault,opaque,limits,login_handle,webauthn}.rs`,
  `services/auth/migrations/{0026,0027,0028,0029}_*.sql`
- `apps/web/src/auth/recover/three-words.ts`,
  `apps/web/src/devices/recovery-payload.ts`,
  `apps/web/src/backup/device-phrase.ts`,
  `apps/web/src/settings/RecoveryPage.tsx`
- BIP-0039; W3C Web Authentication Level 3, the `prf` extension; FIDO CTAP2
  `hmac-secret`
