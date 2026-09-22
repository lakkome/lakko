<!-- SPDX-License-Identifier: AGPL-3.0-or-later -->
<!-- SPDX-FileCopyrightText: 2026 Lakko Contributors -->

# LAK-RC-06 — The web "device root key" is derived from the password with no device entropy, so local at-rest encryption does not survive password compromise

- **Severity:** Medium (finder's proposal; `audit-verify` calibrates)
- **Confidence:** CONFIRMED
- **Workstream:** recon
- **Tracking id:** LAK-RC-06
- **Status:** Open
- **Component:** `apps/web/src/auth/device-key.ts:71-105` and `:127-153`
- **Invariants touched:** #7 (hardware-backed device keys)
- **Vendor claim contradicted:** `docs/security/threat-model.md`, local-disk
  attacker row: *"Every at-rest piece is wrapped under a hardware-backed key
  (Secure Enclave / StrongBox / non-extractable WebCrypto). Plaintext is
  unreachable without the device's biometric / passcode."*

## Preconditions

An attacker who holds **the user's password** (phished, reused, breached
elsewhere, or coerced) and **a copy of the browser profile's IndexedDB** — a disk
image, a synced profile, a backup, another OS account, or forensic acquisition.
Neither the device nor any device-held secret is needed.

Alternatively: the recovery phrase plus the same IndexedDB, via the second
derivation.

## Mechanism

`apps/web/src/auth/device-key.ts:71-105`:

```ts
export async function deriveDeviceRootKey(
  exportKey: Uint8Array,
  accountIdB64: string,
): Promise<CryptoKey> {
  const baseKey = await crypto.subtle.importKey("raw", exportKey.slice(), { name: "HKDF" }, false, ["deriveKey"]);
  const info = new TextEncoder().encode(HKDF_INFO_PREFIX + accountIdB64);
  return crypto.subtle.deriveKey(
    { name: "HKDF", hash: "SHA-512", salt: HKDF_SALT.slice(), info: info.slice() },
    baseKey, { name: "AES-GCM", length: 256 }, /* extractable */ false, ["encrypt", "decrypt"],
  );
}
```

Every input is recoverable by someone who knows the password:

- `exportKey` is the OPAQUE password export key, a deterministic function of the
  password and the account;
- `HKDF_SALT` is the fixed ASCII constant `"lakko-device-root-v1"` (`:43`);
- `info` is a fixed prefix plus the base64 account id — both public.

There is **no device-generated random value anywhere in the derivation**. The
second entry point, `deriveDeviceRootKeyFromIdentitySeed` (`:127-153`), has the
identical shape over the 64-byte BIP39 phrase seed with a different `info` domain,
so the same holds for the phrase.

The module's own header states the intended property, and it is a narrower one
than the threat model's: *"JavaScript can use the key … but cannot read the raw
bytes — this is what makes `localStorage`-stuffing attacks impossible"* (`:26-30`).
`extractable: false` defends against **exfiltration by script running in the
page**. It does not, and cannot, defend against an attacker who computes the key
independently from inputs they already hold.

Consumers of this key, all of which inherit the property:
`chat/message-log-store.ts`, `chat/read-watermark-idb.ts`, `calls/call-log-idb.ts`,
`chat/group-name-idb.ts`, `auth/account-identity-seed-store.ts` (the
AccountIdentity seed itself), `auth/session-token-store.ts`, and the MLS snapshot
store.

## Reproduction

Conceptually, from the password and an IndexedDB copy: run OPAQUE login against
the recorded server exchange (or any server, since the export key is derived
client-side from the password and the envelope) to obtain `exportKey`; apply
HKDF-SHA-512 with the two constants above; AES-GCM-decrypt each stored blob.

I did not execute this. It requires an OPAQUE exchange I cannot stage read-only,
and the claim does not depend on executing it: the derivation is a pure function
of stated inputs, printed above.

## Impact

**What breaks.** On web, the at-rest encryption layer adds no security against an
attacker who has the password. That includes the MLS provider snapshot (and hence
past epoch secrets still held in it), the message log, the call log, the
AccountIdentity seed, and the session token. The property MLS provides —
forward secrecy, so that compromise at time *T* does not open messages from before
*T* — is bounded by what the local snapshot retains, and the wrapper over that
snapshot is password-derived.

**What does not break.** The `extractable: false` guard is real and does its
actual job: page-script compromise (XSS, a malicious extension acting through the
page) cannot read the key bytes out of WebCrypto. And this is confined to web —
iOS and Android use OS keystores whose keys are not derived from the password
(`01-surface.md` §3.3).

**Why this is Medium and not higher.** The reachability is privileged/local: it
needs both the password and the stored data. The severity comes from the gap
between that and what the threat model tells a reader — *"plaintext is unreachable
without the device's biometric / passcode"* — which is simply not true of this
platform. A user who reads the threat model would reasonably conclude that a
password breach does not expose their message history on a shared or seized
machine. It does.

## Recommendation

**Mix a device-held random value into the derivation.** Generate 32 bytes from
`crypto.getRandomValues` once per browser profile, store them in IndexedDB beside
the wrapped key, and fold them into the HKDF `salt` or `info`. The stored random
value is no more exfiltrable than the ciphertext it protects, and it breaks the
"password alone regenerates the key" property completely — an attacker then needs
the password *and* the device blob, rather than the password *or* nothing. The
cost is that the key no longer regenerates from the password alone, which is
presumably why it was built this way, so the change has to be paired with the
existing recovery paths: the identity-seed derivation already exists for exactly
the "fresh device, no password" case and is the natural place to re-provision.

**Separately, and cheaply: state the property per platform.** The threat model's
local-disk row should not present Secure Enclave, StrongBox and non-extractable
WebCrypto as equivalent, because against this adversary they are not. Invariant #7
lists them as alternatives; the threat model should say what each actually buys.

## Notes

- Invariant #7 as written is arguably *satisfied* — it names "non-extractable
  WebCrypto" as an accepted mechanism, and the key is non-extractable WebCrypto.
  The finding is against the threat model's local-disk row, which makes a stronger
  and platform-independent claim that the implementation does not support. A
  verification pass may reasonably re-file this as a documentation finding; I
  filed it as an implementation finding because the fix that closes it is a code
  change and the doc change alone leaves the exposure.
- I did **not** check whether iOS or Android derive their device keys from the
  password. §3.3 of the surface note records only *where* their keys live
  (Keychain, AndroidKeyStore) and that Android requests StrongBox for the device
  root key; the derivation inputs on those platforms are unexamined.
- `localStorage` on web holds the contact list, pins, sent requests and known
  group ids **in plaintext** (`01-surface.md` §2.4), with no wrapper at all. That
  is a separate and simpler exposure of the same social-graph data and is not
  covered by this finding.
