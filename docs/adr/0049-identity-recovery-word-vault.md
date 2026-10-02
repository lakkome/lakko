<!--
SPDX-License-Identifier: AGPL-3.0-or-later
SPDX-FileCopyrightText: 2026 Lakko Contributors
-->

# ADR-0049: Identity recovery with three of the twenty-four words, another device, or a passkey

> **Amended by ADR-0064 (2026-10-02).** The body below is left as written.
> What no longer holds:
>
> - **The word vault (section 1) is gone.** There is no `word_vaults` table,
>   no `/vault/enroll` or `/vault/recover/*` route, no positions, verifier
>   or sealed entropy on the server (migration `0029`). Three words are
>   checked locally by a device that already holds the phrase, and bring no
>   keys to a new device.
> - **The invariant-#2 exception proposed in section 6 is withdrawn**:
>   nothing derived from the phrase's words is stored server-side.
> - The phrase is **12 words** (section 4 is replaced), and the alternative
>   "Twelve words, or all twenty-four, everywhere" is no longer rejected.
> - **What stays:** the another-device confirmation (section 2, as amended
>   on 2026-09-28) and the passkey vault (section 3), which is now opt-in
>   per passkey, holds the 16-byte entropy, and has put / get / delete /
>   list routes.
>
> See [ADR-0064](0064-recovery-model-simplification.md).

- **Status:** Accepted (the maintainer accepted the direction and chose to build the word vault first, 2026-09-15; the guess-budget numbers in section 1 stand until changed)
- **Date:** 2026-09-15
- **Context:** Phase 3 account-recovery overhaul, Part 3 (after
  [ADR-0017](0017-phrase-seeded-identity.md) and
  [ADR-0018](0018-recovery-factor-cascade.md)). UX spec:
  [`docs/ux/identity-recovery.md`](../ux/identity-recovery.md).

## Context

### What exists

ADR-0017 made the 24-word BIP39 phrase the account identity: `AccountIdentity =
Identity::from_seed(phrase_seed)`. The phrase never reaches the server. A
device that holds the seed (cached under the hardware device key) needs nothing
else; a device that does not hold it must obtain it before it can act as the
account.

ADR-0018 split ACCESS from IDENTITY. Four factors grant ACCESS (a session
token): another device, passkey, TOTP, phone or email code. Only one of them
also carries IDENTITY: another device, which seals the seed to the fresh device
over the rendezvous channel (Path B). Every other factor lands the client in the
`needs-recovery-phrase` state, where the only ways out are typing all 24 words,
pairing with another device by scanning a QR code, or minting a fresh identity
that every contact must re-verify.

### Why this must change now

Live testing on `test.lakko.me` (findings 11 and 12 of the review) showed the
recovery experience is not usable as shipped:

- Typing 24 words is the common path, not the rare one. A password login on a
  fresh browser always ends in the 24-word prompt, because the password no
  longer contains the identity (ADR-0017 accepted this regression and it is
  the first thing a user meets on a second machine).
- QR pairing assumes one of the two devices has a camera pointed at the other.
  The maintainer's own setup is two laptops. There is no way to point a
  laptop's webcam at another laptop's screen comfortably, and desktop web has
  no scanner UI at all.
- A new device that does pair receives the DeviceGroup and a history bundle
  of old messages, but is not a member of any chat group, so it sees a
  read-only past and no present. The user reads this as "my chats are gone".

The maintainer set the bar: **three words out of the twenty-four, in three
numbered fields, on every recovery path**, with another device and a passkey
as alternative routes, and the full phrase kept only as the offline reserve.
Twelve words, or all twenty-four, were rejected as unacceptable UX. The
comparison point is a hardware crypto wallet that asks for three numbered
words.

### The constraint a subset of the phrase runs into

Three words cannot derive the seed. BIP39 turns 256 bits of entropy plus an
8-bit checksum into 24 words of 11 bits each; three words are 33 bits of a
264-bit string. Nothing can be computed from them offline. So a three-word
check is only possible if some party holds the rest of the secret, encrypted
under those three words, and releases it after a correct answer while
refusing to let anyone try many answers. That party is the server, and that
is the design Signal ships for its PIN.

### Precedent: Signal's Secure Value Recovery

Signal's PIN is at least four digits. The client stretches the PIN with Argon2
and derives a key that encrypts a 32-byte master secret; the encrypted secret
lives in a server-side enclave (SGX in SVR1 and SVR2; SGX, AWS Nitro and AMD
SEV-SNP in SVR3) that holds a per-account guess counter, releases the secret
only on a correct guess, and destroys it after the limit (Signal's writeup
gives five guesses as the example). Clients verify the enclave by remote
attestation, so even Signal cannot bypass the counter without changing the
attested code. The PIN protects profile, settings and the social graph, not
message history; Signal's 2025 Secure Backups use a separate 64-character
recovery key that never reaches the server and restore only at first setup.

The relevant lessons:

1. A short secret is safe **only** with a server-enforced guess limit, and
   the server has to be trusted to enforce it unless there is an enclave.
2. The encrypted value should be small and long-lived (a master secret), with
   everything else derived from it.
3. The guess budget is a hard cap that ends in destruction, not only a
   rate limit, or a patient attacker gets unlimited tries.

Three BIP39 words are 2^33 candidates, roughly 2^20 times a four-digit PIN,
so the same construction starts from a much better position than Signal's.

## Decision

Add a **word vault** to `services/auth`: the 32-byte phrase entropy, encrypted
by the client under a key stretched from three words at three fixed
positions chosen at registration, stored server-side with a guess counter and
a hard destruction cap. Add a **confirmation prompt on the existing device**
as the primary another-device route, with QR kept as an option. Add
**passkey-carried identity** through the WebAuthn PRF extension as a second
phase. Keep the 24 words as the offline last resort. Make joining the
account's chat groups part of every new-device flow.

The one-sentence version: the client keeps the crypto, the server keeps the
guess counter, and the user types three words.

### 1. The word vault

#### Enrollment (client, at signup and whenever a new phrase is minted)

1. Pick three distinct positions `p1 < p2 < p3` uniformly at random from
   1..=24. The positions are chosen once and never change while this phrase
   is in force.
2. Take the words at those positions, lower-case, joined by a single space:
   `answer = w[p1] " " w[p2] " " w[p3]`.
3. Draw a random 16-byte `salt`.
4. `K = argon2id(answer, salt)` with the vault parameters (section 1.4).
5. `K_verify = HKDF-SHA-256(K, "lakko/vault/verify/v1")`,
   `K_seal = HKDF-SHA-256(K, "lakko/vault/seal/v1")`.
6. `sealed_entropy = AEAD(K_seal, entropy_32)` using the existing
   `lakko_crypto::aead::seal_with_key` envelope. The value sealed is the
   BIP39 **entropy**, not the PBKDF2 seed, because `Mnemonic::from_entropy`
   reconstructs the full phrase from it and the seed does not reverse.
7. `POST /vault/enroll` (bearer-authed) with
   `{ positions: [p1,p2,p3], salt, verifier: SHA-256(K_verify), sealed_entropy }`.

The server wraps `verifier || sealed_entropy` once more under a server-static
`LAKKO_VAULT_ENC_KEY` (the `LAKKO_TOTP_ENC_KEY` pattern), so a database dump
without the service environment yields nothing to attack.

Enrollment is **mandatory at signup** and part of `mintFreshIdentity` and any
future phrase rotation. A failed enroll fails the signup closed rather than
leaving an account whose only recovery is 24 words the user was just told to
write down and will lose.

`POST /vault/enroll` on an account that already has a vault replaces it. The
risk is availability only: a stolen session can break the three-word shortcut
(the attacker learns nothing, the 24 words and another device still work). It
is accepted for v1 and listed under consequences; gating the replace behind
an ADR-0014 passkey action grant is the hardening if it is wanted.

#### Recovery (any device with no cached seed)

1. The client resolves the username to an `account_id` through the existing
   OPRF discovery lookup, exactly as the recovery OPAQUE login does today.
2. `POST /vault/recover/start` (anonymous, PoW-gated) with `{ account_id }`
   returns `{ positions, salt }`. For an unknown account the server returns
   positions and a salt derived deterministically from a server pepper over
   the presented id, so the response is indistinguishable from a real one.
3. The UI shows three fields labelled with the positions, in the order the
   server returned them (ascending). The user types three words. Each field
   autocompletes from the 2048-word list and refuses a word not in it, so a
   typo never costs a guess.
4. The client computes `K`, `K_verify`, `K_seal` as at enrollment and sends
   `POST /vault/recover/finish` (anonymous, PoW-gated) with
   `{ account_id, verifier: SHA-256(K_verify) }`.
5. The server unwraps the row, compares the verifier in constant time, and:
   - on a match resets the counter and returns `{ sealed_entropy }`;
   - on a mismatch increments `failed_attempts`, sets `last_failed_at`, and
     answers with the generic refusal;
   - for an unknown account performs a dummy compare and answers with the
     same generic refusal.
6. The client opens `sealed_entropy` with `K_seal`, rebuilds the phrase with
   `RecoveryMnemonic.fromEntropy`, and hands the phrase to the **existing**
   `provideRecoveryPhrase` path, which derives the seed, verifies it against
   the published identity bundle (fail-closed, ADR-0017 Part 1 finding 1)
   and caches it. If the device also lacks ACCESS, the same phrase runs the
   existing recovery OPAQUE login (`/recovery/login/*`, kind RECOVERY) and
   password reset. Nothing downstream of the vault open is new.

The server never sees the words, the key, the entropy or the seed. It sees
three positions, a salt, a hash of a hash, and a ciphertext.

#### Guess budget

Two mechanisms, both server-side, modelled on the TOTP table (migration 0012):

- **Lockout:** 5 failed attempts within 15 minutes refuse further attempts
  for 15 minutes (`LOCKOUT_THRESHOLD = 5`, `LOCKOUT_WINDOW_SECS = 900`,
  the constants `totp.rs` already uses). A locked account answers with the
  same generic refusal plus a `Retry-After`.
- **Destruction cap:** 10 consecutive failures since the last success delete
  the row. After that the three-word path answers like an unknown account,
  and the user falls back to the 24 words or another device. A successful
  recovery resets the count to zero.

The two numbers are proposals for the maintainer to confirm. The lockout slows
an online attacker to a rate that makes 2^33 unreachable in any human
lifetime; the cap bounds the total to a number a legitimate user with a bad
memory can still afford (ten tries at three words each) while a guesser gets
2^-30 odds before the vault is gone.

Every successful open is written to a durable per-account audit row
(`vault_opens(account_id, opened_at)`) so a later "recent recovery" notice on
the account's other devices is possible; the notice itself is not in scope.

#### Argon2 parameters

The vault answer has 33 bits of entropy. The existing `argon2id_kek`
(m = 64 MiB, t = 3, p = 4) is tuned for an interactive backup unlock. The vault
is opened at most a handful of times in an account's life, so it can afford a
heavier setting. The decision is a **cost budget**, not a parameter set:

- Target: one evaluation takes on the order of one second on a current
  mid-range phone, measured, not assumed.
- Memory: as high as the weakest supported client tolerates without being
  killed. The iOS NotificationServiceExtension is not a client of this path,
  so its 30 MB budget does not apply; the app process does.

The parameters are carried in the vault row (`argon2_m_kib`, `argon2_t`,
`argon2_p`) so they can be raised later without breaking existing vaults.

**Shipped starting value** (`lakko_mnemonic::vault::DEFAULT_PARAMS`):
m = 256 MiB, t = 3, p = 1. `p = 1` because the web client runs Argon2 in
single-threaded wasm, where extra lanes only add sequential work. Measured
once, natively, release build, on the maintainer's Apple-silicon laptop
(one evaluation, not an average):

| m | t | p | time |
|---|---|---|---|
| 64 MiB | 3 | 4 | 0.13 s |
| 128 MiB | 3 | 1 | 0.19 s |
| 256 MiB | 3 | 1 | 0.40 s |
| 512 MiB | 3 | 1 | 0.87 s |

The wasm and phone figures are not measured yet; the ADR's one-second
target on a mid-range phone is still an open item, and 256 MiB may prove too
much memory for low-end mobile browsers, in which case the default moves
and existing rows keep working under their recorded cost.

The residual this budget bounds is stated in the consequences: an attacker
who holds the database **and** `LAKKO_VAULT_ENC_KEY` **and** ignores the
server's own counter (that is, the operator, or someone who has fully
compromised the host) can brute-force 2^33 evaluations offline. At one second
per evaluation that is 2^33 seconds, about 270 core-years; at one hundred
milliseconds, about 27 core-years. Argon2id is memory-hard, so a GPU or ASIC
gains far less than it would against a plain hash. These figures are
arithmetic on an assumed per-evaluation cost, not a measurement.

#### Schema (auth migration `0017_word_vault.sql`; the tables of sections 2 and 3 land with their own sprints as the following migrations)

```sql
CREATE TABLE word_vaults (
    account_id       BINARY(16)       NOT NULL PRIMARY KEY,
    positions        BINARY(3)        NOT NULL,   -- three distinct 1..=24
    salt             BINARY(16)       NOT NULL,
    argon2_m         INT UNSIGNED     NOT NULL,   -- KiB
    argon2_t         TINYINT UNSIGNED NOT NULL,
    argon2_p         TINYINT UNSIGNED NOT NULL,
    body_enc         VARBINARY(128)   NOT NULL,   -- seal_with_key(verifier || sealed_entropy)
    failed_attempts  TINYINT UNSIGNED NOT NULL DEFAULT 0,
    last_failed_at   TIMESTAMP(3)     NULL,
    created_at       TIMESTAMP(3)     NOT NULL DEFAULT CURRENT_TIMESTAMP(3),
    CONSTRAINT fk_word_vaults_account
        FOREIGN KEY (account_id) REFERENCES accounts(id) ON DELETE CASCADE
);

CREATE TABLE vault_opens (
    account_id  BINARY(16)   NOT NULL,
    opened_at   TIMESTAMP(3) NOT NULL DEFAULT CURRENT_TIMESTAMP(3),
    INDEX idx_vault_opens_account (account_id, opened_at),
    CONSTRAINT fk_vault_opens_account
        FOREIGN KEY (account_id) REFERENCES accounts(id) ON DELETE CASCADE
);
```

No PII, no key material in the clear, nothing that identifies a person
(invariant #1 holds). `positions` and `salt` are public by construction.

### 2. Another device: a confirmation prompt, not only a QR code

> **Amended 2026-09-28 (security pass): the code is typed, not compared.**
> The comparison code below was `SHA-256("lakko/device-request/sas/v1" ||
> pairing_payload)` shown on both screens. The payload carries a free 32-byte
> rendezvous pseudonym, so a malicious relay (the server, or anyone who
> controls it) grinds its own payload P' until its code equals the victim's
> (about a million hashes, milliseconds), and the person's comparison passes
> for a device the relay controls: a full account takeover. The flow below
> is kept as history; what replaces it:
>
> 1. The old device **chooses** six uniformly random digits (rejection
>    sampling, no modulo bias) and shows them; the person **types** them on
>    the new device. The server never sees the code.
> 2. The devices run **SPAKE2** (Ed25519 group, RustCrypto `spake2` 0.4, new
>    crate `crates/lakko-device-link`) with the code as the password. The
>    old device is side B and moves first, since it knows the code first;
>    the new device is side A. Both identities and the transcript bind the
>    auth `request_id` and the login handle, so an exchange cannot be moved
>    to another request or another account's name.
> 3. HKDF over the SPAKE2 key and the transcript gives a payload key and two
>    confirmation keys. The new device seals the ADR-0056 pairing payload
>    under the payload key and sends it with `confirm_a` (an HMAC over the
>    transcript and the SHA-256 of the sealed payload); the old device
>    checks `confirm_a` in constant time, opens the payload, and only then
>    pairs. It returns `confirm_b`, which the new device checks before
>    trusting the Welcome that follows.
> 4. **One guess per request.** The old device's state is consumed by its
>    one verify; auth writes the old device's message once, the sealed
>    payload once and the outcome once. A wrong code ends the request
>    (`wrong_code`) and the person starts again. A passive relay learns
>    nothing it can test offline; an active one has one chance in a million
>    per request the person's device answers, and each attempt puts a
>    prompt on that device, which clients rate-limit.
>
> Relay routes (auth, migration `0024_device_link_pake.sql`, the table
> rebuilt: flag day): `POST /device/request {handle}`; `GET
> /device/requests`; `POST /device/requests/{id}/pake {pake_b_b64}`; `POST
> /device/request/status {request_id_b64}` -> `{pending, pake_b_b64,
> outcome, confirm_b_b64}`; `POST /device/request/payload {request_id_b64,
> pake_a_b64, sealed_payload_b64, confirm_a_b64}`; `POST
> /device/requests/{id}/result {outcome, confirm_b_b64?}`; `DELETE
> /device/requests/{id}` (decline). A request for a handle nobody holds is
> stored against its stand-in and answers every anonymous route exactly as a
> real request no device has answered yet. UX: `docs/ux/identity-recovery.md`
> section 4 (amended the same day). Residual stated plainly: the `spake2`
> crate has not been independently audited and does not zeroize its own
> copy of the password and its ephemeral scalar; recorded for G-39.
>
> **Amended again 2026-09-28 (second security pass): consent first, and a
> guess budget the server keeps.** The old device answered a request (posted
> `pake_b`) on its poll, before anyone tapped anything, so every anonymous
> request was a free one-in-a-million guess, and the only daily limit lived in
> the old device's local storage. Now the old device asks `Is this you?` and
> answers only after `Yes, it's me`; and `services/auth` counts wrong-code
> outcomes per account, pausing the account's requests after three in 24 h
> for an hour and after six for a day (chosen, not measured). A paused
> account's new requests answer exactly like any other and are stored
> against the stand-in, so the pause tells a requester nothing.

The existing flow (ADR-0018 factor (a)) already does the right cryptography:
a single-use `device_grants` token for ACCESS, the seed sealed over the
rendezvous channel for IDENTITY. What it lacks is a way to start it that
does not need a camera. The QR code carries the fresh device's pairing
payload (its public keys and the rendezvous pseudonym) to the old device.
This ADR adds a second carrier for that same payload: the auth service.

#### Flow

1. The fresh device, after the user enters the username, calls
   `POST /device/request` (anonymous, PoW-gated) with `{ account_id,
   pairing_payload }`, the exact bytes the QR would have carried. The server
   stores it as an opaque blob with a short TTL and returns a `request_id`.
2. The fresh device shows: "Confirm on one of your other devices" and a
   **six-digit comparison code** `SAS = SHA-256("lakko/device-request/sas/v1"
   || pairing_payload)`, the first four bytes taken modulo one million and
   rendered as three pairs of digits (SHA-256 because the web client has it
   in WebCrypto without a wasm round-trip; the hash only needs to bind the
   payload).
3. Every signed-in device of the account learns of the pending request. Web
   polls `GET /device/requests` (bearer-authed) while a session is open;
   native clients get a push wake from the existing push gateway (a new
   payload kind, carrying nothing but "check your requests"), and poll on
   foreground as well.
4. The old device shows a prompt: "Someone is signing in to your account on
   a new device. Code: 47 19 03. Does the new device show the same code?"
   with **Confirm** and **It is not me**.
5. On **Confirm** the old device fetches the pairing payload, recomputes the
   SAS locally, and runs the **existing** `prepareJoinFlow` path unchanged:
   DeviceGroup add, `/device/authorize/start` grant, seed sealed over the
   rendezvous channel (Path B), history bundle. The fresh device polls the
   rendezvous mailbox exactly as after a QR scan.
6. On **It is not me** the old device calls `DELETE /device/requests/{id}`;
   the fresh device sees the request vanish and shows "Declined on another
   device".

#### Why the comparison code is load-bearing

The server relays the pairing payload. A malicious server could substitute
its own device's payload and receive the seed. The SAS closes that: the code
the old device displays is a function of the payload it is about to seal the
seed to, and the code the new device displays is a function of its own
payload. If the server swapped payloads the two codes differ, and the user
was asked to compare them before confirming. The code is compared, not
typed, because the goal is binding the payload rather than proving presence;
six decimal digits give a substitution attempt a 1-in-a-million chance of a
lucky match per attempt, and the request is single-use. A request an
existing device declines is deleted; the fresh device polls the request's
status and reports "Declined on another device" (after a grace period, since
a confirming device also deletes the request, right after publishing the
Welcome).

What the server learns: that an account has a pending device request, and
the pairing payload, which is public material the DS would have seen at
rendezvous time anyway. No new social-graph edge (invariant #5).

#### Schema (its own migration, with the section-2 sprint)

```sql
CREATE TABLE device_requests (
    account_id       BINARY(16)     NOT NULL,
    request_id       BINARY(16)     NOT NULL,
    pairing_payload  VARBINARY(512) NOT NULL,   -- opaque, never parsed server-side
    expires_at       TIMESTAMP(3)   NOT NULL,
    PRIMARY KEY (account_id, request_id),
    INDEX idx_device_requests_expires (expires_at),
    CONSTRAINT fk_device_requests_account
        FOREIGN KEY (account_id) REFERENCES accounts(id) ON DELETE CASCADE
);
```

TTL 10 minutes; expired rows are swept on the next insert; at most one open
request per account at a time (the account id is the primary key, so a new
request replaces the old one and a spammer cannot fill a user's prompt list).
`/device/request` answers an unknown account exactly like a known one (the
insert fails on the account foreign key and is folded into the same 200);
the status endpoint takes only the 128-bit random request id.

### 3. Passkey-carried identity (phase 2 of this ADR)

A passkey grants ACCESS today. Synced passkeys (iCloud Keychain, Google
Password Manager, 1Password and the like) follow the user to a new device,
so a passkey could carry IDENTITY too if a secret could be derived from it.
The WebAuthn **PRF extension** (`hmac-secret` on the authenticator side)
returns a 32-byte value that is a function of the credential and a
client-chosen salt, stable across the devices the passkey syncs to.

Design: at passkey registration (and once, lazily, for existing passkeys on
their next assertion) the client requests `prf.eval.first = salt_prf`, derives
`K_pk = HKDF(prf_output, "lakko/passkey-vault/seal/v1")`, seals the 32-byte
entropy under it, and stores `{ credential_id, salt_prf, sealed_entropy }`
server-side (`passkey_vaults`, opaque). On a passkey login the client asks
for the same PRF evaluation, fetches the blob, opens it, and feeds the phrase
to `provideRecoveryPhrase` as in section 1. The server stores one more
ciphertext it cannot open; nothing in the login ceremony changes.

Why phase 2: neither the vendored `webauthn-rs-core` (which only knows the
`hmac-secret` response type in `interface.rs`) nor the web client requests or
parses PRF today, and authenticator support is uneven (browser and OS
version dependent; some password managers do not implement it). Until it
lands, a passkey login on a fresh device is followed by the three-word vault
or the another-device prompt for IDENTITY, which is already an acceptable
path. The `passkey_vaults` table lands with the phase-2 sprint.

### 4. Twenty-four words stay, as the reserve

The full phrase remains the offline root: it is what the vault protects, what
the passkey blob protects, and the only path that survives the loss of every
device, the destruction of the vault, and a passkey provider outage at once.
It is shown once at signup and stored nowhere but on paper. The recovery UI
moves it to a secondary link under the three-word fields ("Type all 24 words
instead") and the phrase input landed in the 2026-09 web batch (numbered
slots, autocomplete, paste-to-fill) is the input for that path.

### 5. A new device must get the account's chats (finding 12)

Every route above ends with a device that holds the seed. That device is the
account cryptographically but is not a leaf in any chat group, because MLS
membership is per device (ADR-0007) and forward secrecy means no seed can
re-derive a group's current epoch. So the flows differ in what else they can
deliver:

- **Another device (confirmation or QR):** the intent is that after the
  seed transfer the old device brings the new device into every chat group
  it is a member of, so the new device has the present and not only the
  history bundle's past. **Correction (2026-09-15, found while building):
  this cannot be done on the current chat-group model.** Every chat leaf is
  signed by the account identity (`Identity::from_seed(phrase_seed)`, the
  same key on every device), and RFC 9420 forbids two leaves with one
  signature key: OpenMLS refuses the Add (`DuplicateSignatureKey`) and the
  ADR-0045 sweep filter drops it as colliding with a staying member. The
  KeyPackage directory is one row per account, so there is not even a way
  to address a Welcome to "the other device". Per-device conversation
  leaves are the ADR-0007 Phase 2b work, named open in ADR-0007 and
  ADR-0022 and pinned by no ADR. What a paired device receives today is
  the DeviceGroup, the plaintext history bundle and the seed; it can read
  the past and cannot receive or send in the present. This requirement
  therefore moves to its own ADR (per-device leaves: a device identity per
  chat leaf, the device-to-account binding peers verify through the
  DeviceGroup, per-device KeyPackages, role tables and naming keyed on the
  account) and the UI states the limit plainly until it lands.
- **Vault or passkey, no other device alive:** the seed comes back, contacts
  come back from the encrypted backup (ADR-0037), chat membership cannot.
  Every 1x1 and group has to be re-established by a member adding the account
  again (or by an invite link). The UI must say this plainly on that path
  ("Your identity is restored. Your chats live on your other devices; if none
  is left, your contacts can add you back.") rather than showing an empty
  list with no explanation. This limit is inherent to MLS, not to Lakko, and
  is the same limit Signal states for its PIN.

The mechanics of the group fan-out (how the old device enumerates its groups,
how many adds one confirmation may trigger, the ADR-0040 outbox obligation
per add) belong to the implementation sprint and its own spec update to
`docs/ux/proposal-based-membership.md`; the requirement is fixed here.

### 6. What changes in the security model

**Invariant #2 is weakened for the recovery path, deliberately.** Today the
server holds no artefact from which the identity seed can be recovered at
any cost. After this ADR it holds `sealed_entropy`, which an operator who
ignores their own guess counter can brute-force with 2^33 stretched
evaluations. The seed is still not stored, not derivable in one step, and not
recoverable from a database dump alone (the environment key is needed too);
but "at no cost" becomes "at a measured, large, finite cost, by the operator".
The invariant's wording should gain an explicit exception rather than be read
around; proposed text for the maintainer to accept:

> 2. The server only sees ciphertext + public artefacts. No message keys,
>    chain keys, root keys, media keys. **Exception (ADR-0049): the word
>    vault stores the phrase entropy encrypted under a key stretched from
>    three of the user's twenty-four words, behind a server-enforced guess
>    limit. The server can never open it in one step; an operator ignoring
>    the limit faces a 2^33 memory-hard search per account.**

Everything else holds: no PII (#1), no MLS change (#3), fail-closed at every
new decision point (#4: a wrong answer, an unknown account, a destroyed
vault, an SAS mismatch and a PRF failure all refuse; nothing degrades to a
weaker path silently), no social graph (#5), rotation only via KT proof (#6:
the vault returns the existing seed, so no rotation happens), hardware device
keys untouched (#7).

**Flag for `lakko-security`:** this touches auth, adds a server-held
recovery artefact and a new device-authorisation entry point. Review before
merge, on explicit request.

### 7. What the enclave would add, and why not now

Signal's answer to "the operator can ignore the counter" is an attested
enclave; SVR3 spreads the secret over three vendors' enclaves so one vendor
compromise is not enough. Lakko runs on a single Hetzner VPS with no
attestation-capable hardware and no second operator. An enclave is the right
hardening once there is infrastructure to run one and a second party to run
it; it changes nothing in the client protocol above (the enclave replaces the
plain server as the holder of the row and the counter). Recorded as the
follow-up, not built.

## Alternatives

- **Twelve words, or all twenty-four, everywhere.** No server artefact, no
  weakening of #2. Rejected by the maintainer as unusable; the live test
  confirmed that typing 24 words is the everyday path, not the exception.
- **Three words with no server vault.** Impossible: 33 bits cannot derive a
  256-bit seed, and any scheme that let them would let anyone.
- **Store a hash of the three words server-side and release the full phrase
  from a backup.** The full phrase would then be a server-held plaintext or a
  server-decryptable ciphertext, which is a far larger weakening than a
  stretched-key vault.
- **Social recovery (`lakko-shamir`, guardians).** Was built as a crate and
  a client flow independent of this ADR; it recovered the entropy from three
  or more guardians instead of three words. Removed from the product on
  2026-10-02 by maintainer decision (any three guardians together hold the
  account, and reaching them needs a channel other than the lost account).
- **Client-chosen positions or user-chosen words.** Lets a user pick the
  three easiest words and correlates across accounts; random fixed positions
  keep the 33 bits honest.
- **Ask for the three words on the old device instead of a Confirm button.**
  Adds nothing: the old device already holds the seed and is the trust
  anchor; the SAS is what binds the new device, not a secret the old device
  already has.
- **QR only for another device.** Fails the two-laptop case and every
  desktop-to-desktop pairing; rejected by the maintainer.
- **Passkey PRF as phase 1.** Blocked on client and library work, and on
  authenticator support that cannot be assumed; the vault covers the same
  need immediately.

## Consequences

- **Positive:** recovery on a fresh device is three words in three fields,
  on every platform, with no camera; the two-laptop pairing works through a
  Confirm prompt; a passkey can carry identity once PRF lands; the 24 words
  become what they should be, a paper backup; a new device joins the
  account's chats rather than inheriting a read-only past.
- **Negative:** invariant #2 gains a stated exception; the operator becomes
  able to attempt an offline search against a stretched 33-bit secret; the
  auth service gains three tables and five endpoints; `LAKKO_VAULT_ENC_KEY`
  joins the secrets that must be backed up (losing it destroys every vault,
  and every user falls back to 24 words); a stolen session can overwrite a
  vault (availability only); a destroyed vault is not re-creatable without
  the full phrase.
- **Neutral:** enrollment adds one Argon2 evaluation to signup; the guess
  counter and lockout reuse the TOTP constants and code shape; the
  identity-acquisition state machine gains one more entry (vault open) that
  converges on `provideRecoveryPhrase`, so the fail-closed bundle check runs
  on every route; the recovery OPAQUE login is unchanged and reached with
  the reconstructed phrase.

## Implementation status (2026-09-15)

Built in the first sprint, the word vault end to end on web:

- `lakko-crypto`: `Argon2Params` + `argon2id_kek_with_params`.
- `lakko-mnemonic::vault`: `enroll` / `answer` / `AnswerKeys::open`,
  `random_positions`, `positions_are_valid`, `DEFAULT_PARAMS`; 8 tests.
- `lakko-ffi-wasm`: `wordVaultEnroll` / `wordVaultAnswer` +
  `WordVaultEnrollment` / `WordVaultAnswer`.
- `services/auth`: migration `0017_word_vault.sql` (`word_vaults`,
  `vault_opens`), `word_vault.rs` (`GET /vault`, `POST /vault/enroll`,
  `POST /vault/recover/{start,finish}`), `LAKKO_VAULT_ENC_KEY`; pure tests
  plus a database-backed state-machine test (enroll, status, decoys, a
  correct open, lockout at 5, destruction at 10, re-enroll).
- Web: `recovery/word-vault.ts`, `ui/WordSlots.tsx`; enrollment at signup,
  at fresh-phrase confirmation, on a phrase recover, and lazily on any
  sign-in from the device-cached phrase; the three-word route first on both
  the restore prompt and the forgot-password screen; the signup phrase
  screen highlights the three positions.
- Infra: the key in `.env.example`, docker compose, the test-stack setup.

Found in passing and fixed: `/totp/recover` decoded its `TIMESTAMP` and
`TINYINT UNSIGNED` columns as `NaiveDateTime` / `i16`, and the
recovery-contact OTP path decoded its `attempts` column as `i16`; sqlx 0.9
refuses both at runtime, so a confirmed authenticator's recover and an OTP
enrollment confirmation returned 500 on every call. Their database-backed
tests had never been run (they skip without
`LAKKO_AUTH_TEST_DATABASE_URL`). The columns now decode as `DateTime<Utc>`
/ `u8`, and the whole auth suite, the three database-backed tests included,
passes against a local MariaDB 11.

Found by the live two-device test of the confirmation route and fixed: a
password recover rotates the device key but left the DeviceGroup snapshot
wrapped under the old one, so the next add-device flow on that device died
with a bare `OperationError` (the recovered device could never add another
device again); recover now drops that snapshot and re-caches the phrase
under the new key, and the DeviceGroup loader treats an unreadable snapshot
as a fresh start. The new device also gave up on the Welcome after 60
seconds, the QR-scan budget; on the confirmation route it now waits as long
as its request lives (10 minutes).

Built in the second sprint (same day): the another-device confirmation
prompt (section 2: migration `0018`, `device_request.rs`, the web
`DeviceRequestPrompt` on every signed-in tab, the comparison code on both
sides, QR kept as the alternative) and the passkey vault (section 3:
`passkey_vaults`, `PUT /vault/passkey` + `POST /vault/passkey/get`, wasm
`passkeyVaultSeal` / `passkeyVaultOpen`, the PRF evaluation on the web
register and login ceremonies, enrollment whenever a ceremony yields a PRF
output on a device holding the phrase, and a fresh-device passkey login
that opens the blob into the phrase and lands through
`provideRecoveryPhrase`).

Not built: the new-device chat membership (section 5, blocked on per-device
conversation leaves as corrected there), the Settings "Recovery" row,
native (iOS / Android) clients, and running the client-side Argon2 off the
main thread (it blocks the page for the evaluation today).

## Open questions

1. The two guess numbers (5-in-15-minutes lockout, 10-strike destruction)
   need the maintainer's confirmation.
2. Whether vault replacement from a signed-in session should be gated behind
   a passkey action grant when the account has a passkey (ADR-0014 pattern),
   or left as the accepted availability residual.
3. The measured Argon2 budget per client platform.
4. Whether the "recent recovery" notice on other devices (the `vault_opens`
   consumer) is worth building in the same arc.
5. How the old device fans out the group adds on confirmation for an account
   with many groups, and what the user sees while that runs (spec follow-up).
6. PRF support matrix per platform and password manager before phase 2 is
   scheduled.

## References

- [ADR-0017](0017-phrase-seeded-identity.md), [ADR-0018](0018-recovery-factor-cascade.md),
  [ADR-0014](0014-webauthn-action-binding.md), [ADR-0037](0037-backup-head-rollback-commitment.md),
  [ADR-0045](0045-remote-commit-epoch-fork.md), [ADR-0007](0007-multi-device-identity.md)
- `services/auth/src/totp.rs` (lockout constants, `seal_with_key` at rest),
  `services/auth/migrations/0011_device_grants.sql`, `0012_totp.sql`
- `crates/lakko-crypto/src/kdf.rs` (`argon2id_kek`), `crates/lakko-mnemonic`
  (`Mnemonic::from_entropy`)
- Signal, "Technology preview: Secure Value Recovery" (2019),
  https://signal.org/blog/secure-value-recovery/
- Signal, "Signal PINs" (2020), https://signal.org/blog/signal-pins/
- Signal, Secure Value Recovery 3 announcement (2024) and the 2025 Secure
  Backups announcement, both on the Signal blog
- BIP-0039, Mnemonic code for generating deterministic keys
- W3C Web Authentication Level 3, the `prf` extension; FIDO CTAP2
  `hmac-secret`
