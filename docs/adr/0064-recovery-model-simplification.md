<!-- SPDX-License-Identifier: AGPL-3.0-or-later -->
<!-- SPDX-FileCopyrightText: 2026 Lakko Contributors -->

# ADR-0064: Recovery model simplification: a 12-word phrase, sign-in-only three words, and a passkey that can carry keys on every client

- **Status:** Accepted 2026-10-02 (maintainer decisions); removals of Shamir
  shares and the email/phone factor and the 12-word phrase built the same
  day (unit-tested on all three clients, not walked live); the access-only
  three words and the native passkey vault are NOT built
- **Date:** 2026-10-02
- **Amends:** [ADR-0017](0017-phrase-seeded-identity.md) (phrase length),
  [ADR-0018](0018-recovery-factor-cascade.md) (factor (d) removed, factor
  list), [ADR-0049](0049-identity-recovery-word-vault.md) (sections 1, 3, 4,
  6 and the alternatives). Supersedes none of them as a whole.
- **UX spec:** [`docs/ux/identity-recovery.md`](../ux/identity-recovery.md)
  (rewritten the same day; it was written before this ADR and this ADR
  carries every state it needs).

Number check: `ls docs/adr` showed 0063 as the highest file in this worktree
on 2026-10-02, so 0064 was free here. Other branches were not checked.

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
route to the keys left, start over with a new identity.

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

### Facts about the current code this ADR relies on

Verified by reading the tree on 2026-10-02 (file references are to this
worktree):

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

The seven decisions are the maintainer's, made on 2026-10-02. They are
recorded here with their reasons and are not re-opened by this document.

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

### 5. Three words sign the person in, and nothing else

Three words at three fixed positions become an ACCESS factor for "I forgot my
password and this device still has my keys". The server keeps a verifier of
the three-word answer behind the guess budget and **no longer stores the
sealed entropy**. A correct answer returns the session triple the other
ACCESS factors return.

Consequences, stated explicitly:

- **The ADR-0049 invariant-#2 exception is withdrawn.** Nothing key-bearing
  is stored for the three words, so an operator who ignores its own counter
  and searches the answer space obtains no key material. It obtains a session
  it could mint anyway, and the three words themselves (see "Security
  consequences": this is a 33-bit leak about the phrase, not nothing).
- **The size of the secret.** Twelve words are 132 bits: 128 of entropy and 4
  of checksum, with the checksum inside the twelfth word (7 entropy bits + 4
  checksum bits). Three words are 3 x 11 = 33 bits of that string. To someone
  who knows none of the other words the answer space is 2^33 whichever
  positions were drawn: the four checksum bits are a hash of all 128 entropy
  bits and cannot be predicted from three words. When position 12 is one of
  the three (55 of the 220 possible position sets, one in four) the answer
  contains 29 entropy bits and the 4 checksum bits rather than 33 entropy
  bits; either way a party that learns the answer is left with about 95
  unknown bits of the phrase.
- **The online budget is kept as it is in the code:** 5 attempts per 15
  minutes per login handle (charged before the lookup, so a name nobody holds
  trips it alike) and a tombstone on the 10th consecutive miss. Reading the
  code gave no reason to change either number. The tombstone bounds a guesser
  to 10 tries per enrollment, a success chance of at most 10 / 2^33, about
  1.2 in a billion. The window alone would allow 480 tries a day. The cost of
  the tombstone is unchanged from ADR-0049: anyone who knows the username can
  switch the shortcut off by guessing wrong ten times; the owner turns it on
  again from Settings on a device that holds the phrase.
- **What the three words are worth on a device that holds the keys.** Such a
  device already holds the phrase seed under its device key (fact 11), and
  that seed is what `/recovery/login` and `/password/reset` accept (facts 2
  and 6). So on that device the three-word check is a knowledge gate the
  client places in front of a credential it already has; the server enforces
  it only as the condition for the session it issues on this route. It stops
  a person holding an unlocked device, not code running in the device's
  profile. This is the existing posture of every ACCESS factor on a warm
  device, recorded here because the route's purpose now rests on it.

### 6. A passkey is a separate way to recover keys, beside the phrase

On iOS and Android the platform passkey gets the PRF vault web has: the PRF
output wraps the phrase entropy into the opaque server-held blob, and on a new
device a synced passkey unwraps it. The fail-closed check of the restored
phrase against the published identity bundle still applies. Where the OS or
the credential provider gives no PRF output the passkey returns ACCESS only
and the UI says so.

The maintainer accepts that key safety then also rests on the user's Apple /
Google / password-manager credential sync: "the user decides how much they
trust Apple and the others". The choice must therefore be **informed and
explicit at enrollment**: storing the blob is opt-in per passkey, behind a
screen that says what it means (UX spec 10.2). Fact 12's automatic enrollment
on web is removed.

A passkey is not a replacement for the phrase: the phrase is still generated,
shown and confirmed at signup, and is the only route that depends on no
device and no third party.

### 7. Another device is the default route

Listed first on the recovery screen and on the restore prompt, with "if you
have one". Before a session the server must not reveal whether the account
has other devices (an existence oracle), so this is ordering and copy, not a
server lookup; the list is identical for every account.

### The resulting model

| Situation | Routes, in order |
|---|---|
| New device, keys needed | another device -> passkey (when it carries keys) -> 12-word phrase -> start over with a new identity (ADR-0017 `mintFreshIdentity`, after ACCESS) |
| Forgot the password, device still has keys | passkey, three words, authenticator code (ACCESS only; TOTP unchanged) |

Every route ends in the existing ADR-0017 identity-acquisition state machine;
an ACCESS-only route on a device without keys parks in
`needs-recovery-phrase` and the restore prompt offers another device, a
passkey and the phrase, never the three words.

## What changes, per component

### `lakko-mnemonic`

- `WORD_COUNT` 24 -> 12; `ENTROPY_LEN` 32 -> 16; `generate`, `from_phrase`,
  `from_entropy`, `to_entropy`, `SecretEntropy` and every doc comment follow.
  `SEED_LEN` stays 64.
- `vault.rs` splits in meaning:
  - **Word verifier** (was the word vault): `Positions` are three distinct
    ascending values in `1..=12`; `random_positions` keeps rejection sampling
    with the bound changed for 12 (252 = 21 x 12, was 240 = 10 x 24);
    `enroll` returns `{positions, salt, params, verifier}` and **no
    `sealed_entropy`**; `answer` returns the verifier only; `AnswerKeys::open`,
    `K_seal`, `SEAL_INFO`, `VaultError::{Seal, Open}` are deleted. The
    verify label is bumped (`"lakko/vault/verify/v2"`) so a value computed
    for a 24-word phrase can never match.
  - **Passkey vault** (`seal`/`open` under `HKDF(prf_output,
    "lakko/passkey-vault/seal/v1")`): unchanged construction; the plaintext is
    now 16 bytes, and `Mnemonic::from_entropy` refuses anything else, so an
    old 32-byte blob fails closed.
- `DEFAULT_PARAMS` (m = 256 MiB, t = 3, p = 1) is kept for the verifier. It
  no longer protects key material; it still sets what an operator pays per
  candidate to learn three words. Its cost on phones and in wasm was never
  measured (ADR-0049) and now has to be, because iOS and Android will run it.
- Tests and the `mnemonic_parse` fuzz target follow the new lengths.
  `lakko-mnemonic` is not `lakko-mls`; invariant #3 is not touched.

### Both FFI surfaces

- `RecoveryMnemonic` on UniFFI and wasm: same methods, 12 words, 16-byte
  entropy (`crates/lakko-ffi-uniffi/src/mnemonic.rs` tests assert 24 and 32
  today).
- wasm: `wordVaultEnroll` loses `sealedEntropy`; `WordVaultAnswer.open` is
  removed; `passkeyVaultSeal` / `passkeyVaultOpen` stay.
- UniFFI gains, for the first time, the word-verifier pair (enroll, answer)
  and `passkeyVaultSeal` / `passkeyVaultOpen`, with new `FfiError` variants
  **appended** (the ordinal rule).
- Not on `lakko-ffi-extension`.

### `services/auth`

A new migration under the next free number (0025 is the highest today; the
factor-(d) removal may take 0026 first):

```sql
-- flag day: rows hold values computed for 24-word phrases
DELETE FROM word_vaults;
DELETE FROM passkey_vaults;
ALTER TABLE word_vaults
    CHANGE body_enc verifier_enc VARBINARY(96) NULL
        COMMENT 'seal_with_key(vault_key, verifier(32)); NULL = off (tombstone)',
    ADD COLUMN off_reason TINYINT UNSIGNED NULL
        COMMENT '1 = turned off by the account, 2 = too many wrong tries';
```

- **The row** holds `positions` (three of 1..=12), `salt`, the Argon2 cost,
  `verifier_enc`, `failed_attempts`, `off_reason`. No ciphertext of anything
  the account owns.
- **`POST /vault/enroll`** (bearer): `{positions, salt_b64, argon2_m_kib,
  argon2_t, argon2_p, verifier_b64}`; `sealed_entropy_b64` is gone and a
  request carrying it is refused. Positions are validated against 1..=12.
- **`POST /vault/recover/start`**: unchanged shape. Decoys draw positions
  from 1..=12 (`decoy_for`, `word_vault.rs:557` uses `% 24` today).
- **`POST /vault/recover/finish`**: `{handle, verifier_b64}` -> on a match,
  the session triple `/totp/recover` returns (the exact field names are to
  be copied from `totp::RecoverRes`); a miss, an unknown handle, an account
  with no row and a tombstone all answer the same `401` after the same
  compare. The handle counter (5 per 15 minutes -> `429` with the retry
  time), the per-address bucket and the 10-miss tombstone are unchanged.
- **Turning the shortcut off** must leave a tombstone, not delete the row,
  for the reason migration `0023` records: a deleted row makes `/start`
  switch to decoys and tells a caller a row had existed. New bearer route
  `DELETE /vault` sets `verifier_enc = NULL, off_reason = 1`.
- **`GET /vault`** (bearer) returns `{enrolled, positions, off_reason}` so
  Settings can say "switched off after too many wrong tries".
- `vault_opens` keeps its meaning under its name: one row per successful
  three-word sign-in (renaming it is cosmetic and left to the sprint).
- **Passkey vault:** `PUT /vault/passkey` and `POST /vault/passkey/get` are
  unchanged on the wire (the blob is smaller). New: `DELETE /vault/passkey`
  (bearer, by credential id) for "Stop restoring keys", and a bearer list of
  the credential ids that have a blob, for the two labels in Settings.
  Whether deleting a WebAuthn credential already deletes its
  `passkey_vaults` row was not verified; it must.
- `LAKKO_VAULT_ENC_KEY` stays (it wraps the verifier and the passkey blobs);
  unset still means `503` on every `/vault/*` route.
- The module doc, migration `0017`'s header and the "stated invariant-#2
  exception" paragraphs are rewritten.

The removal of factor (d) (`recovery_contacts`, `otp_challenges`, the
`OtpSender`s, their routes and limits) is the same-day work this ADR records
but does not specify.

### Password replacement from a device that holds the keys

No server change. `/password/reset/*` already accepts a recovery proof made
from the 64-byte seed (fact 6). The clients gain one function, "replace the
password from the cached seed", used by the UX spec's 7.1 screen after any
ACCESS route on a warm device and after another-device or passkey restores
reached through "Forgot password?". Whether today's clients can already call
the reset with the cached seed rather than a typed phrase was not verified.

### The three clients

- **Phrase:** every 24-slot input, grid layout, paste handler and count
  constant becomes 12 (web `PHRASE_WORDS`, `phrase-grid.ts`, `WordSlots.tsx`
  paste check; iOS `PhraseHelpers.count`; the Android equivalents in
  `RecoverViewModel.kt` / `SaveFreshPhraseScreen.kt`), plus test vectors (iOS
  `PhraseSeededIdentityTests` uses the 24-word all-`abandon` vector).
- **Three words:** web turns its vault open into a sign-in
  (`recovery/word-vault.ts`, `WordsRecover.tsx`, `AuthContext.wordVaultOpen`)
  landing through `landAfterAccessRecovery`; the row leaves
  `RESTORE_IDENTITY_METHODS`. iOS and Android build the flow for the first
  time. Enrollment stays automatic at signup (the shortcut must exist before
  the password is forgotten) but a failed enroll **no longer fails signup**
  (ADR-0049 made it mandatory because it carried keys), and Settings gains
  on / off (UX 10.3).
- **Passkey:** iOS (`ASAuthorization` platform credentials) and Android
  (Credential Manager) request the PRF evaluation with the same fixed salt
  web uses, never send its output to the server, seal and open through the
  new UniFFI functions, and gain a passkey sign-in on a device with no cached
  account (the discoverable start exists server-side, fact 10). All three
  make storing the blob an explicit per-passkey choice and add "Stop
  restoring keys". A passkey route is added to the recovery list and the
  restore prompt.
- **Screens:** the recovery list and the restore prompt are reordered and
  re-captioned per the UX spec; Settings > Security > Recovery loses the
  contact row and gains the three-word row and the passkey labels.

### ADR-0017's derivation

Unchanged: `AccountIdentity = Identity::from_seed(bip39_seed)` with the
64-byte PBKDF2 seed and the empty BIP39 passphrase. The three seed stores,
the device-root-key derivation from the identity seed
(`apps/web/src/auth/device-key.ts`), the recovery OPAQUE envelope, the Path B
seed transfer and `verifySeedAgainstPublishedIdentity` all take the seed and
do not see the word count. What changes is the entropy behind the seed (256
-> 128 bits) and every place that handles the **entropy** or the **words**:
the passkey blob, the phrase caches (shorter string, no format change) and
the inputs listed above.

## Security consequences, per invariant

**#1 (no PII) improves.** With factor (d) gone the server stores no contact
hash at all; nothing derived from a phone number or an address remains in
`services/auth`.

**#2 (ciphertext and public artefacts only).**

- The word-vault exception is withdrawn (decision 5). The word verifier is in
  the trust class ADR-0018 already names for the TOTP secret: an ACCESS
  verifier, not key material.
- It is not free of information about the phrase. A party holding the
  database and `LAKKO_VAULT_ENC_KEY` can search the 2^33 answers offline at
  one Argon2id evaluation each (ADR-0049's arithmetic on an assumed cost
  still applies, and is still not a measurement) and learn three words. That
  leaves about 95 unknown bits. The same party can already test whole-phrase
  guesses offline against the public identity key and against the backup
  blob, so the phrase's strength against the operator is its entropy, and
  three known words lower it from 128 to about 95 bits. Recorded as accepted
  with the decision; open question 6 asks whether to keep it.
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

**#3** untouched (no `lakko-mls` change). **#4** every new decision point
fails closed: a wrong answer, an unknown name, a tombstone, a missing or
unopenable passkey blob, a restored phrase that does not match the published
identity. A passkey without PRF degrades to ACCESS only, visibly, never to a
weaker way of obtaining keys. **#5** untouched. **#6** no rotation: every
keys route returns the existing seed. **#7** untouched; the passkey's own
secret lives in the platform authenticator.

**A new session-level risk.** `POST /vault/enroll` replaces the verifier from
any session. Under ADR-0049 that was an availability problem; now a stolen
session can set three words of the thief's choosing and keep a way to sign in
after the session ends. TOTP enrollment has the same shape today. Open
question 4.

Flag for `lakko-security` before merge (auth wire change, a new native keys
route, removal of a stated invariant exception), on explicit request.

## Alternatives rejected

- **Email-only lookup** (keep the address as an account locator, send
  nothing key-bearing). The server still sees the plaintext address beside
  the account at enroll and at recover; the operator can map one to the
  other. Rejected by the maintainer.
- **Keep the sealed-entropy word vault.** Keeps three words as a keys route
  and keeps the invariant-#2 exception: a server-held ciphertext of the
  phrase behind 33 bits and a counter only the operator enforces. With a
  passkey and another device ahead of the phrase, the convenience no longer
  pays for the exception.
- **Passkey as the replacement for the phrase.** Would make the account's
  keys depend entirely on a third party's sync and on PRF support that is
  uneven and unverified on our target platforms; an account whose only
  passkey has no PRF would have no keys route at all.
- **Keep 24 words.** Twice the typing on the one route that needs no device
  and no third party, for entropy above what the derived keys provide.
- **Three words chosen independently of the phrase** (so a searched verifier
  says nothing about the phrase). A second thing to write down and keep; not
  what the maintainer decided. Kept visible as open question 6.

## Open questions

1. **PRF availability was not verified on any device.** Which iOS version
   exposes PRF for platform passkeys through `ASAuthorization`, which Android
   and Credential Manager versions and which providers (Google Password
   Manager, third-party managers) return it, and whether it is returned at
   creation or only at assertion: all to be verified on real devices before
   S5/S6 are planned in detail. Nothing in this ADR assumes an answer.
2. **Do the three clients get the same PRF output for one synced passkey and
   one salt?** The web API hashes the salt before the authenticator sees it;
   whether the iOS and Android APIs apply the same transformation is to be
   verified. If they differ, a blob stored on one client fails to open on
   another (closed, not unsafe) and the salt handling must be aligned in the
   core.
3. **Does a hybrid / cross-device passkey ceremony (a phone used through a QR
   code on a laptop) return PRF output?** Not verified. If not, that ceremony
   is ACCESS only and the restore prompt says so.
4. **Should replacing or enabling the three-word verifier require an
   ADR-0014 passkey action grant when the account has a passkey** (and
   likewise storing a passkey blob)? Recommended; not decided.
5. **What the restore prompt offers when the passkey has no PRF.** The UX
   spec's answer: the line "Your passkey signed you in. It does not carry
   your keys.", then another device and the phrase, with the passkey row
   hidden when the device has only the passkey just tried. Whether the client
   can know "only that one" on each platform is to be verified.
6. **Accept the 33-bit leak of an operator-searched verifier, or decouple the
   three words from the phrase?** Recorded as accepted with decision 5.
7. **The measured Argon2id cost** of `DEFAULT_PARAMS` on a mid-range phone
   and in wasm (open since ADR-0049), now also for iOS and Android.
8. **Relying-party setup across clients.** A passkey restores keys on a new
   device of another platform only if web, iOS and Android share the relying
   party (`assetlinks.json` exists under `apps/web/public/.well-known`; the
   Apple association file and both apps' entitlements were not verified).
9. **Whether automatic three-word enrollment at signup is still wanted** now
   that it is a convenience and leaks 33 bits to a searching operator, or
   whether it should be offered in Settings only. This ADR keeps it automatic
   with an off switch.
10. **The user-held recovery file** (decision 2's future work): undesigned.

## Amendments to earlier ADRs

To be written into those files in the docs sprint (S8); until then this
section is the record.

- **ADR-0017:** "24-word" -> "12-word" throughout; the derivation is
  unchanged. Its lost-everything fallback copy gains "every passkey that
  restores keys".
- **ADR-0018:** factor (d) is removed. The factor list becomes (a) another
  device, (b) passkey, (c) TOTP, plus the three-word verifier as a fourth
  ACCESS factor; (b) may also carry keys through the passkey vault on every
  client.
- **ADR-0049:** section 1 (the word vault) is replaced by decision 5 here:
  no sealed entropy, positions in 1..=12, finish returns a session. Section 3
  (passkey-carried identity) is extended to iOS and Android and made opt-in.
  Section 4 ("Twenty-four words stay, as the reserve") is replaced by
  decisions 3 and 4. Section 6's invariant-#2 exception is withdrawn.
  Alternative "Twelve words, or all twenty-four, everywhere" is no longer
  rejected. Sections 2 and 5 stand.
- **ADR-0056** is unaffected.
- `docs/ux/client-redesign.md` section 5.6 still lists "Email or phone" and
  the old method order.

## Sprint plan

| Sprint | Scope | Exit |
|---|---|---|
| S0 UX | `docs/ux/identity-recovery.md` rewritten (done 2026-10-02); `client-redesign.md` 5.6 aligned; catalog keys of UX section 11 added to `l10n/catalog/en.json` | spec accepted by the maintainer |
| S1 core | `lakko-mnemonic`: 12 words / 16 bytes; word verifier without sealed entropy; passkey seal over 16 bytes; tests, fuzz target | `cargo test -p lakko-mnemonic`, fuzz smoke build |
| S2 FFI | wasm changes; UniFFI word-verifier and passkey-vault functions; bindings, wasm, xcframework, Android `.so` rebuilt | both FFI crates' tests; clippy `-D warnings` |
| S3 services | migration; `word_vault.rs` wire (enroll, finish -> session, `DELETE /vault`, `GET /vault`), decoys over 12; passkey-vault delete and list; credential-delete cascade; module docs | the database-backed auth tests run against a local MariaDB (they skip silently without `LAKKO_AUTH_TEST_DATABASE_URL`) |
| S4 web | 12-word inputs; three words as sign-in; reordered lists; restore prompt without three words; opt-in passkey vault and "Stop restoring keys"; the new-password step; Settings rows | `tsc`, vitest, `vite build`; walked in a browser |
| S5 iOS | the same, plus the three-word flow, fresh-device passkey sign-in and PRF, all new on this client | `xcodebuild` build + tests; walked on a device with a synced passkey |
| S6 Android | the same as S5 | `:app:compileDebugKotlin`, unit tests; walked on a device |
| S7 device matrix | open questions 1-3 answered by measurement: PRF per OS version and provider, cross-client equality of the output, hybrid ceremonies; Argon2 cost measured | a table in this ADR replacing the "to be verified" lines |
| S8 docs and security | amendments above written into ADR-0017 / 0018 / 0049; `CLAUDE.md` ADR table and module map; invariant index and known gaps; `PRODUCT-READINESS.md`; a `lakko-security` pass on request | docs reconciled; findings closed or recorded |

S5 and S6 run in parallel after S4. S7 may start as soon as S2 produces
bindings and should precede the detailed design of S5/S6 where it can.

## Consequences

- **Positive:** no server-held ciphertext of the phrase behind a short
  secret; no contact hash; no guardian quorum; one phrase length that people
  can type; a keys route on phones that uses the device's own biometric
  prompt; the same routes in the same order on all three clients.
- **Negative:** three words no longer bring keys to a new device, so a person
  with no other device and no keys-carrying passkey types twelve words;
  phrase entropy halves to 128 bits; an opted-in passkey makes the account's
  keys as safe as the passkey's sync; an operator who searches the verifier
  learns three words; both native clients gain two flows they never had.
- **Neutral:** `LAKKO_VAULT_ENC_KEY` remains a secret to back up (losing it
  now costs three-word sign-in and passkey restores, never the phrase); the
  identity-acquisition state machine is unchanged; ADR-0017's supersession
  machinery stays dormant.

## References

- [ADR-0014](0014-webauthn-action-binding.md),
  [ADR-0017](0017-phrase-seeded-identity.md),
  [ADR-0018](0018-recovery-factor-cascade.md),
  [ADR-0049](0049-identity-recovery-word-vault.md),
  [ADR-0054](0054-anti-abuse-without-client-work.md),
  [ADR-0056](0056-pairing-names-account-by-handle.md)
- `crates/lakko-mnemonic/src/{mnemonic,vault}.rs`,
  `services/auth/src/{word_vault,limits,totp,opaque,webauthn}.rs`,
  `services/auth/migrations/{0017,0018,0020,0023}_*.sql`
- BIP-0039; W3C Web Authentication Level 3, the `prf` extension; FIDO CTAP2
  `hmac-secret`
