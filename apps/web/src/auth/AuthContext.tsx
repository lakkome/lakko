// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * Auth context — single source of truth for "is the user logged in?"
 *
 * 1.16b: stub that flipped state in-memory.
 * 1.16c3b: real OPAQUE-PAKE round-trips through `OpaqueRegister` /
 *          `OpaqueLogin`.
 * 1.16c3c1: BIP39 recovery passphrase enrolled at signup.
 * 1.16c3c2: session persistence via WebCrypto + IndexedDB.
 *
 * ## Persistence model (1.16c3c2)
 *
 * Two layers of state survive a page reload:
 *
 *   1. **`localStorage`** — `(accountId, username)`. Plaintext.
 *      Neither field is secret; both are routine markers.
 *   2. **IndexedDB** — the device root `CryptoKey`, derived from
 *      the OPAQUE password export key via HKDF-SHA-512 and
 *      stored as **non-extractable** AES-256-GCM. Only the
 *      WebCrypto handle survives the round-trip; the bytes
 *      never leave the keystore.
 *
 * On `AuthProvider` mount we look both up. If both are present,
 * we restore state directly to `session` — no password re-entry
 * required. The OPAQUE export key bytes themselves are NOT
 * persisted; they live in memory only for the duration of the
 * signup / login call and drop the moment the device key has
 * been derived.
 *
 * `logout()` keeps both layers in place — re-auth via password
 * stays cheap. `forgetDevice()` wipes both — full reset, requires
 * the recovery passphrase to come back.
 *
 * ## What is wired and what is not
 *
 *   * Recover-from-mnemonic — DONE. `recover()` drives the
 *     `/recover` route + `AuthClient.recover`, and resolves the
 *     target account_id the same way `login()` does: a local
 *     `(accountId, username)` cache hit wins, else a
 *     `discovery.lookup(username)` round-trip — so recover works
 *     on a FRESH device given the username + phrase (the optional
 *     `username` arg is required only when no account is cached).
 *   * `username → accountId` lookup for first-time login on a
 *     fresh device — DONE. `login()` resolves the typed username
 *     via `discovery.lookup()` on a local-cache miss. The same
 *     resolution is shared by every ADR-0018 recovery factor.
 *
 * Still not here:
 *
 *   * Multi-account on a single device.
 */
import {
  createContext,
  createSignal,
  onMount,
  type Accessor,
  type JSX,
  useContext,
} from "solid-js";

import { AuthClient } from "./client";
import type { ContactKind } from "./wire";
import { base64Decode, base64Encode } from "./account-id";
import { resolveRecoverAccount } from "./recover-resolution";
import {
  deriveDeviceRootKey,
  deriveDeviceRootKeyFromIdentitySeed,
} from "./device-key";
import { deleteDeviceKey, getDeviceKey, putDeviceKey } from "./key-store";
import {
  deleteSessionToken,
  getSessionToken,
  putSessionToken,
} from "./session-token-store";
import {
  deleteExportKey,
  getExportKey,
  putExportKey,
} from "./export-key-store";
import {
  deleteDeviceIdentitySeed,
  ensureDeviceIdentitySeed,
} from "./device-identity-store";
import {
  ACCOUNT_IDENTITY_SEED_LENGTH,
  deleteAccountIdentitySeed,
  getAccountIdentitySeed,
  putAccountIdentitySeed,
} from "./account-identity-seed-store";
import { deleteDeviceGroupSnapshot } from "../devices/device-group-snapshot-store";
import {
  WebauthnClient,
  WebauthnHttpError,
  WebauthnNetworkError,
  WebauthnUnsupportedError,
  WebauthnUserCancelledError,
} from "./webauthn-client";
import { loadWasm, type Wasm } from "../wasm";
import { accountKey as transparencyAccountKey } from "../transparency/account-key";
import {
  DiscoveryClient,
  DiscoveryHttpError,
} from "../discovery/client";
import { clearAll as clearContactsStore } from "../contacts/local-store";
import { useChats } from "../chat/ChatContext";
import { deleteMlsSnapshot } from "../chat/mls-store";
import { deleteMessageLog } from "../chat/message-log-store";
import { rehydrateMlsState } from "../chat/rehydrate";
import { subscribeToPush, unsubscribeFromPush } from "../push/subscribe";
import { bootstrapBackupFromPhraseString } from "../backup/bootstrap";
import { deletePhrase } from "../backup/phrase-store";
import { restoreFromServerBackupFromPhraseString } from "../backup/restore";
import type { Identity as WasmIdentity } from "../../wasm/lakko_ffi_wasm";

/** Account record exposed to the UI when a session is active. */
export interface SessionAccount {
  /** base64(account_id) — the canonical id we send to the server. */
  readonly accountId: string;
  /** Human-visible handle, e.g. `@alice`. Local-only until 1.16d. */
  readonly username: string;
}

/** Discriminated union for the auth state. */
export type AuthState =
  | { readonly kind: "loading" }
  | {
      readonly kind: "anon";
      /** If a previous session is cached on this device, expose
       *  the username so the login form can pre-fill it. */
      readonly cachedUsername?: string;
    }
  | { readonly kind: "session"; readonly account: SessionAccount }
  /** ADR-0017 fresh-device fail-safe. The user authenticated
   *  successfully (password or passkey ACCESS) but this device has
   *  NO cached AccountIdentity (phrase) seed, so we cannot rebuild
   *  the canonical identity. We MUST NOT derive it from the export
   *  key (wrong identity now) nor silently mint a fresh one (breaks
   *  every contact's pin). The UI renders a recovery-phrase prompt;
   *  `provideRecoveryPhrase(phrase)` completes the session. */
  | {
      readonly kind: "needs-recovery-phrase";
      readonly account: SessionAccount;
    }
  /** ADR-0017 lost-everything fallback (the ONLY sanctioned
   *  publish-without-a-supersession-proof path). Reached ONLY from
   *  `needs-recovery-phrase` (i.e. after ACCESS was granted by a
   *  factor) and ONLY via the explicit `mintFreshIdentity()` button —
   *  never automatically. A brand-new recovery phrase + AccountIdentity
   *  has just been minted + published for the SAME account; this state
   *  renders the new 24-word phrase for the user to save before the
   *  session lands. `confirmFreshPhraseSaved()` completes the session
   *  from the stashed context. The user's contacts then see a changed
   *  key with no proof and re-verify out-of-band — the accepted,
   *  documented degradation. */
  | {
      readonly kind: "save-fresh-phrase";
      readonly account: SessionAccount;
    };

/** Result handed back to the signup form so it can display the
 *  freshly-generated 24-word recovery passphrase exactly once. */
export interface SignupOutcome {
  /** The 24-word BIP39 recovery phrase. The form must show this
   *  to the user and require an explicit "I have written it down"
   *  acknowledgement before navigating away. */
  recoveryPhrase: string;
}

/** ADR-0017 — thrown by `provideRecoveryPhrase` (and the ADR-0018
 *  pairing completions) when the supplied AccountIdentity seed
 *  derives a pubkey that does NOT match the account's published
 *  bundle. Surfaced to the UI as "doesn't match this account" — we
 *  refuse rather than cache a forked identity that would break
 *  every contact's pin. */
export class SupersessionPhraseMismatch extends Error {
  constructor(message?: string) {
    super(
      message ??
        "That recovery phrase doesn't match this account's identity. " +
          "Check the phrase, or pair an existing device instead.",
    );
    this.name = "SupersessionPhraseMismatch";
  }
}

/** ADR-0017 (lakko-security finding #1) — thrown by
 *  `provideRecoveryPhrase` when the account's published bundle could
 *  NOT be fetched (404 / network) so the supplied phrase cannot be
 *  verified against it. We HARD-BLOCK rather than trust-on-first-use a
 *  possibly-wrong phrase: this path only runs for an already-established
 *  account, so a missing bundle is an anomaly (a withholding or
 *  unreachable directory), and a server-forced 404 must not be able to
 *  push a socially-engineered wrong phrase into the published identity.
 *  Surfaced as a retryable error, not a dead end. */
export class IdentityVerificationUnavailable extends Error {
  constructor(message: string) {
    super(message);
    this.name = "IdentityVerificationUnavailable";
  }
}

/** Identity secrets held in memory for the current session.
 *
 *  Populated whenever the user has just authenticated (signup,
 *  login, recover) — those flows have the OPAQUE export key in
 *  scope and can derive the Identity. **Empty after a
 *  page reload** that restored the session via the IDB device
 *  root key (1.16c3c2) — the export key is not persisted, so
 *  the secrets cannot be re-derived without a password.
 *
 *  Operations that need to sign on behalf of the user
 *  (`ContactsClient.sendContactRequest`, MLS commits, …) require
 *  this to be set; surface a "please re-enter your password"
 *  prompt when it's `null`.
 *
 *  The bytes themselves are held — not a `wasm.Identity`
 *  instance — because:
 *   * they're cheap (96 bytes total) and clone-friendly;
 *   * a WASM instance carries lifetime baggage and a heavy
 *     `free()` requirement;
 *   * downstream callers reconstruct an `Identity` on demand
 *     via `wasm.Identity.fromSeed(accountIdentitySeed)` when
 *     needed (ADR-0017 — was `exportKey` pre-ADR-0017) — each
 *     reconstruction is sub-millisecond.
 */
export interface IdentitySecrets {
  /** Raw 16-byte UUIDv7. */
  readonly accountId: Uint8Array;
  /** OPAQUE password export key (64 bytes for `LakkoCipherSuite`).
   *  ADR-0017: NO LONGER the seed for the canonical AccountIdentity
   *  — it is retained only as an auth factor and as the
   *  device-root-key seed (`deriveDeviceRootKey`). */
  readonly exportKey: Uint8Array;
  /** ADR-0017: the BIP39 recovery-phrase seed
   *  (`RecoveryMnemonic.toSeed()`, 64 bytes). This is the seed for
   *  the canonical AccountIdentity (`Identity.fromSeed`) — stable
   *  across password changes and recovery, so the pubkey contacts
   *  pin never churns on a routine recover. Every `fromSeed` site
   *  that builds the canonical identity reads THIS, not
   *  `exportKey`. */
  readonly accountIdentitySeed: Uint8Array;
  /** Phase 1.5b: HMAC-SHA256 bearer token. Thread through every
   *  protected `lakko-svc-discovery` write via the
   *  `Authorization: Bearer <token>` header. */
  readonly sessionToken: string;
  /** Token expiry as a unix-millisecond timestamp. */
  readonly sessionExpiresAtMs: number;
}

/** Public surface of the auth context. */
export interface AuthApi {
  readonly state: Accessor<AuthState>;
  /** In-memory Identity secrets (1.16d3c). `null` whenever the
   *  user has not just authenticated in the current page session
   *  — see [`IdentitySecrets`] for the full shape and lifetime
   *  story. */
  readonly identitySecrets: Accessor<IdentitySecrets | null>;
  /** ADR-0017 lost-everything fallback — the freshly-minted 24-word
   *  recovery phrase awaiting the user's save acknowledgement. Non-null
   *  ONLY while the state is `save-fresh-phrase` (between
   *  `mintFreshIdentity` and `confirmFreshPhraseSaved`); `null`
   *  otherwise. The phrase lives in memory only and is dropped when the
   *  session lands. Kept off the `AuthState` object so it is not
   *  threaded through the route guards. */
  readonly freshPhrase: Accessor<string | null>;
  /** ADR-0017 defence-in-depth — a non-null value means this account's
   *  published identity did NOT match this device's at login (someone
   *  replaced it; the ACCESS-hijack signature). Surfaced as a dismissible
   *  security banner. `null` in the normal case. */
  readonly identityAlert: Accessor<IdentityAlert | null>;
  /** Dismiss the identity-changed security banner. */
  dismissIdentityAlert(): void;
  /** Run the OPAQUE-PAKE registration flow.
   *
   *  Throws on network / server errors (the form surfaces them).
   *  On success, persists `(accountId, username)` to localStorage,
   *  derives the device root key + writes it to IndexedDB, and
   *  flips state to `session`. The returned [`SignupOutcome`]
   *  carries the BIP39 recovery phrase that the UI MUST display
   *  exactly once. */
  signup(username: string, password: string): Promise<SignupOutcome>;
  /** ADR-0032 guest join — create a FULL account with NO username and no
   *  user-chosen password, so an anonymous `/c/<slug>` visitor can join a
   *  call without a login. Reuses the `signup` machinery verbatim: OPAQUE
   *  registration with a RANDOM 32-byte password (never shown to the user;
   *  recovery is phrase-only) + device-root-key derivation + the first-ever
   *  identity-bundle publish (with the genesis `leafAuth`) — but SKIPS
   *  `registerUsername`, yielding a username-less account. Lands in the
   *  `save-fresh-phrase` state (mirroring `mintFreshIdentity`) so the
   *  existing `SaveFreshPhrasePrompt` shows the recovery phrase;
   *  `confirmFreshPhraseSaved` then lands the session. Claiming a username
   *  (`registerUsername`) is a later onboarding step. Returns the recovery
   *  phrase (the caller shows it / it is surfaced by the prompt). */
  signupGuest(): Promise<SignupOutcome>;
  /** Run the OPAQUE-PAKE login flow.
   *
   *  Resolves the typed `username` to an account_id from the local
   *  `(accountId, username)` cache, falling back to a
   *  `discovery.lookup()` round-trip on a cache miss — so a fresh
   *  browser can log in to any account whose username is
   *  registered (1.16d2). Throws on unknown username or wrong
   *  password (both surface the same error — by design). */
  login(username: string, password: string): Promise<void>;
  /** Run the recover-from-mnemonic flow.
   *
   *  Decodes the typed phrase via `RecoveryMnemonic.fromPhrase`,
   *  drives `AuthClient.recover` to rotate the server's password
   *  envelope, derives a fresh device root key from the new
   *  export key, persists it via the IDB pipeline, and flips
   *  state to `session`.
   *
   *  Resolves the account_id like `login`: a local cache hit, else
   *  a `discovery.lookup(username)` round-trip — so recover works
   *  on a FRESH device given the username + phrase. `username` is
   *  required only when no account is cached on this device; when a
   *  cached account exists it is optional (and the cached id wins).
   *
   *  ADR-0017: the canonical AccountIdentity is derived from the
   *  (unchanged) BIP39 phrase seed, so a routine recover preserves
   *  the identity — the supersession proof emit is SKIPPED because
   *  the old and new account pubkeys are equal.
   *
   *  Throws on any failure — bad mnemonic, unknown username,
   *  network, server rejection, etc. */
  recover(
    mnemonicPhrase: string,
    newPassword: string,
    username?: string,
  ): Promise<void>;
  /** ADR-0018 factor (c) — recover ACCESS with a TOTP authenticator
   *  code. Resolves `username` → account_id like `login` (local
   *  cache hit, else `discovery.lookup`), drives
   *  `AuthClient.totpRecover` for the session triple, and lands in
   *  the EXISTING ADR-0017 post-ACCESS identity-acquisition state
   *  machine: a cached AccountIdentity seed (decryptable under a
   *  stored device key) finishes the session directly; otherwise the
   *  state flips to `needs-recovery-phrase` and the
   *  RestoreIdentityPrompt offers phrase / another-device.
   *
   *  TOTP grants ACCESS ONLY — there is no password, hence no
   *  OPAQUE export key and (on a fresh browser) no device root key
   *  yet; the completion paths bootstrap the device key from the
   *  verified identity seed.
   *
   *  Throws on unknown username, wrong/expired code (server 401),
   *  lockout (429), or TOTP disabled server-side (503) — the form
   *  maps the `AuthHttpError` statuses to friendly messages. */
  recoverWithTotp(username: string, code: string): Promise<void>;
  /** ADR-0018 factor (d), step 1 — request a one-time recovery code
   *  be sent to a previously-enrolled phone / email. Resolves
   *  `username` → account_id like `login` (local cache hit, else
   *  `discovery.lookup`), drives
   *  `AuthClient.recoveryContactRecoverStart`, and STASHES the
   *  resolved `(accountId, username, kind)` so `recoverWithContactFinish`
   *  can submit the code without re-resolving.
   *
   *  ENUMERATION-SAFE: the server returns the same `{ sent: true }`
   *  whether or not the contact matches a confirmed row, so this
   *  resolving without throwing only means "advance to the code
   *  step" — it does NOT confirm a code was actually delivered.
   *
   *  Throws on unknown username, malformed contact (server 400), or
   *  the factor being disabled server-side (503). */
  recoverWithContactStart(
    username: string,
    kind: ContactKind,
    contact: string,
  ): Promise<void>;
  /** ADR-0018 factor (d), step 2 — submit the 6-digit code that was
   *  delivered. Uses the account + kind stashed by
   *  `recoverWithContactStart`, drives
   *  `AuthClient.recoveryContactRecoverFinish` for the session triple,
   *  and lands in the EXISTING ADR-0017 post-ACCESS
   *  identity-acquisition state machine (cached-seed finish OR
   *  `needs-recovery-phrase` fail-safe) — identical to
   *  `recoverWithTotp`.
   *
   *  Throws when no start preceded it, on wrong/expired code (401),
   *  or lockout (429) — the form maps the `AuthHttpError` statuses to
   *  friendly messages. */
  recoverWithContactFinish(code: string): Promise<void>;
  /** Phase 2a2c2 — sign in with a WebAuthn passkey on the SAME
   *  device the user previously logged in / signed up from.
   *
   *  Requires both halves of the chain to be present:
   *
   *    - a `webauthn_credentials` row server-side bound to the
   *      account, registered earlier from this or another
   *      device.
   *    - a locally-cached export key wrapped under the device
   *      root key (Phase 2a2c2's `export-key-store`),
   *      populated by a prior signup / login / recover on this
   *      device.
   *
   *  Throws when the local cache is absent — passkey is an
   *  auth-convenience, not a fresh-device onboarding path. The
   *  recovery phrase remains the only fresh-device entry route. */
  loginWithPasskey(): Promise<void>;
  /** ADR-0017 fresh-device fail-safe completion. Called only while
   *  the state is `needs-recovery-phrase`: the user typed their
   *  recovery phrase, we derive the BIP39 seed, cache it under the
   *  device key, rebuild the canonical AccountIdentity from it, and
   *  flip to a full `session`. Throws on a bad / mismatched phrase
   *  (the derived pubkey does not match the account's published
   *  bundle) so the user cannot silently fork their identity. */
  provideRecoveryPhrase(mnemonicPhrase: string): Promise<void>;
  /** ADR-0018 factor (a), path (a) — finish the
   *  `needs-recovery-phrase` state with a seed transferred from
   *  another device over the E2EE pairing channel instead of a
   *  typed phrase. Runs the SAME fail-closed verification as
   *  `provideRecoveryPhrase` (derived pubkey must match the
   *  published bundle; hard-block + zeroize on mismatch /
   *  unverifiable), caches the seed under the existing device key,
   *  and completes the session — preferring the redeemed fresh
   *  `sessionToken` when present, else the pending login's token.
   *
   *  CONSUMES `phraseSeed` (zeroized on every refusal path). */
  completeFromPairing(
    phraseSeed: Uint8Array,
    sessionToken?: string,
    sessionExpiresAtMs?: number,
  ): Promise<void>;
  /** ADR-0018 factor (a), path (b) — the NO-PASSWORD entry of the
   *  recovery cascade. The fresh device ran the pairing
   *  anonymously, received `phraseSeed` + a REDEEMED session token
   *  (mandatory here — there is no other ACCESS), and has no
   *  device root key yet. This bootstraps the device: fail-closed
   *  bundle verification first (refuse + zeroize), then a device
   *  root key derived FROM the identity seed
   *  (`deriveDeviceRootKeyFromIdentitySeed`), the wrapped stores,
   *  the localStorage marker, the caller's join-state persistence
   *  hook, and finally the canonical session. */
  completeFromDeviceRestore(args: {
    readonly accountIdBytes: Uint8Array;
    readonly accountIdB64: string;
    readonly username: string;
    /** 64-byte AccountIdentity seed — CONSUMED (zeroized on every
     *  refusal path). */
    readonly phraseSeed: Uint8Array;
    readonly sessionToken: string;
    readonly sessionExpiresAtMs: number;
    /** Persist the pairing's in-memory DeviceIdentity seed +
     *  DeviceGroup snapshot under the freshly-bootstrapped device
     *  key (`AnonymousJoinSession.persistTo`). Best-effort. */
    readonly persistJoinState?: (deviceKey: CryptoKey) => Promise<void>;
  }): Promise<void>;
  /** ADR-0017 lost-everything fallback — the ONLY sanctioned path
   *  that publishes a CHANGED AccountIdentity key WITHOUT a
   *  supersession proof. Callable ONLY while `pendingRecovery() !==
   *  null` (i.e. the user recovered ACCESS via an ADR-0018 factor and
   *  is parked in `needs-recovery-phrase` with NO phrase and NO other
   *  device) — throws otherwise. It mints a BRAND-NEW recovery phrase
   *  + AccountIdentity for the SAME account, bootstraps a device key
   *  from the new seed when none is parked, publishes the new bundle
   *  WITHOUT a proof (none is mintable — there is no old seed), then
   *  STASHES the completion context and flips to `save-fresh-phrase`
   *  so the UI can display the new phrase for the user to save.
   *  Returns the new 24-word phrase string.
   *
   *  This does NOT immediately land the session — the caller MUST
   *  show the phrase, then call `confirmFreshPhraseSaved()`.
   *
   *  SECURITY: reachable ONLY post-ACCESS, behind the explicit user
   *  button. The publish-without-proof is correct ONLY here because no
   *  old seed exists. The user's contacts then see a changed key with
   *  no proof and re-verify out-of-band — the accepted degradation. */
  mintFreshIdentity(): Promise<string>;
  /** ADR-0017 lost-everything fallback, step 2 — only valid while the
   *  state is `save-fresh-phrase`. The user has saved the freshly-minted
   *  recovery phrase; this lands the canonical session from the stashed
   *  context (empty exportKey — the ACCESS-only convention) and clears
   *  the stash. Throws if called outside `save-fresh-phrase`. */
  confirmFreshPhraseSaved(): Promise<void>;
  /** Drop the in-memory session. Does NOT erase the localStorage
   *  cache or the IDB-stored device key — re-login on the same
   *  device should be cheap, and the device key remains usable
   *  for opening any encrypted local stores 1.16f wires up. Use
   *  [`forgetDevice`] for a clean wipe. */
  logout(): void;
  /** Wipe both the localStorage cache and the IDB-stored device
   *  key for this device, then drop the in-memory session. After
   *  this the only way back into the account is via the recovery
   *  passphrase (1.16c3c3 `/recover` flow). */
  forgetDevice(): Promise<void>;
}

const AuthContext = createContext<AuthApi>();

/** ADR-0017 fresh-device fail-safe: everything needed to finish a
 *  session once the user supplies their recovery phrase. The auth
 *  factor (password / passkey) already succeeded; only the
 *  AccountIdentity (phrase) seed is missing on this device. */
interface PendingRecovery {
  readonly accountIdBytes: Uint8Array;
  readonly accountIdB64: string;
  /** OPAQUE export key from the just-completed auth — retained for
   *  the device-key derivation + as the in-memory `exportKey`.
   *  ADR-0018 factor (c): EMPTY (length-0) on the TOTP recover path
   *  — no password was involved, so no export key exists (the
   *  established convention from `completeFromDeviceRestore`). */
  readonly exportKey: Uint8Array;
  /** The wrapping device key, already derived from `exportKey`, so
   *  the supplied phrase seed can be cached without re-deriving.
   *  ADR-0018 factor (c): `null` on the TOTP recover path when this
   *  browser has no stored device key — the completion paths
   *  (`provideRecoveryPhrase` / `completeFromPairing`) bootstrap one
   *  from the VERIFIED identity seed, exactly like
   *  `completeFromDeviceRestore`, and only AFTER the fail-closed
   *  verification succeeds. */
  readonly deviceKey: CryptoKey | null;
  readonly sessionToken: string;
  readonly sessionExpiresAtMs: number;
  readonly username: string;
}

/** ADR-0017 lost-everything fallback: everything needed to finish a
 *  session once the user has saved the freshly-minted recovery phrase.
 *  The new AccountIdentity has ALREADY been published (without a
 *  proof) by `mintFreshIdentity`; this just carries the bits
 *  `confirmFreshPhraseSaved` hands to `finishCanonicalSession`. The
 *  `exportKey` is always EMPTY here — this is an ACCESS-only path
 *  (recovered via an ADR-0018 factor, no password in scope). */
interface PendingFreshPhrase {
  /** The 64-byte BIP39 seed of the BRAND-NEW phrase, already cached
   *  under the device key by `mintFreshIdentity`. CONSUMED by
   *  `finishCanonicalSession`. */
  readonly seed: Uint8Array;
  readonly accountIdBytes: Uint8Array;
  readonly accountIdB64: string;
  readonly sessionToken: string;
  readonly sessionExpiresAtMs: number;
  readonly username: string;
  /** The freshly-minted 24-word recovery phrase — re-exposed to the
   *  UI by the `save-fresh-phrase` render so the user can save it. */
  readonly phrase: string;
}

/**
 * ADR-0017 defence-in-depth (lakko-security Phase-3A follow-up (i)) — a
 * security alert raised when this device's canonical AccountIdentity did
 * not match the account's currently-published bundle at login. The
 * load-bearing case is `published-identity-changed`: someone published a
 * DIFFERENT identity for this account (the ACCESS-hijack signature — an
 * attacker minted a fresh identity via the lost-everything path). Carried
 * as a dismissible banner so the owner finds out even if the attacker
 * acted first.
 */
export interface IdentityAlert {
  readonly kind: "published-identity-changed";
}

const STORAGE_KEY = "lakko.auth.v1";

interface PersistedSession {
  /** Schema marker — bump if the storage shape changes so old
   *  rows are recognised and dropped instead of mis-parsed. */
  v: 1;
  accountIdB64: string;
  username: string;
}

function loadPersisted(): PersistedSession | null {
  // SSR-safety: AuthProvider runs in the browser, but a defensive
  // check costs us nothing.
  if (typeof localStorage === "undefined") return null;
  const raw = localStorage.getItem(STORAGE_KEY);
  if (raw === null) return null;
  try {
    const parsed = JSON.parse(raw) as Partial<PersistedSession>;
    if (
      parsed.v === 1 &&
      typeof parsed.accountIdB64 === "string" &&
      typeof parsed.username === "string"
    ) {
      return {
        v: 1,
        accountIdB64: parsed.accountIdB64,
        username: parsed.username,
      };
    }
    return null;
  } catch {
    return null;
  }
}

function savePersisted(record: PersistedSession): void {
  if (typeof localStorage === "undefined") return;
  localStorage.setItem(STORAGE_KEY, JSON.stringify(record));
}

function clearPersisted(): void {
  if (typeof localStorage === "undefined") return;
  localStorage.removeItem(STORAGE_KEY);
}

/** Encode a UTF-8 string into bytes — `password` enters
 *  OPAQUE-PAKE as raw bytes. */
function utf8(s: string): Uint8Array {
  return new TextEncoder().encode(s);
}

export function AuthProvider(props: { children: JSX.Element }): JSX.Element {
  // Start in `loading` so the very first paint doesn't snap to
  // `/login`; the onMount hook reads localStorage + IDB, then
  // resolves to either `session` (full restore) or `anon`.
  const [state, setState] = createSignal<AuthState>({ kind: "loading" });
  // 1.16d3c: in-memory Identity secrets. Filled on signup /
  // login / recover, dropped on logout, NOT populated on a
  // session restore from IDB (the export key isn't persisted).
  const [identitySecrets, setIdentitySecrets] =
    createSignal<IdentitySecrets | null>(null);
  // ADR-0017 fresh-device fail-safe: when a password / passkey
  // auth succeeds but no AccountIdentity (phrase) seed is cached on
  // this device, we stash everything needed to finish the session
  // here and flip the state to `needs-recovery-phrase`. The UI then
  // calls `provideRecoveryPhrase`, which reads this back.
  const [pendingRecovery, setPendingRecovery] =
    createSignal<PendingRecovery | null>(null);
  // ADR-0017 lost-everything fallback: after `mintFreshIdentity`
  // has minted + published a brand-new AccountIdentity (without a
  // proof — none is mintable), it stashes the completion context
  // here and flips to `save-fresh-phrase`. The UI shows the new
  // 24-word phrase; `confirmFreshPhraseSaved` reads this back to
  // land the session, then clears it. The seed never leaves this
  // closure until the session is finished.
  const [pendingFreshPhrase, setPendingFreshPhrase] =
    createSignal<PendingFreshPhrase | null>(null);
  // ADR-0017 defence-in-depth (lakko-security Phase-3A follow-up (i)):
  // a non-null value means this device's canonical AccountIdentity did
  // NOT match the account's currently-published bundle at login — i.e.
  // someone published a DIFFERENT identity for this account (the
  // ACCESS-hijack signature: an attacker minted a fresh identity via the
  // lost-everything path). Surfaced as a dismissible security banner so
  // the owner sees it even if the attacker beat them to it.
  const [identityAlert, setIdentityAlert] =
    createSignal<IdentityAlert | null>(null);
  // ADR-0018 factor (d): two-step phone/email OTP recover. `…Start`
  // resolves the username → account_id, sends a code, and stashes the
  // resolved context here so `…Finish` can submit the code without
  // re-resolving. Enumeration-safe: a stash being present says nothing
  // about whether a code was actually delivered (the server returns
  // the same response either way).
  const [pendingContactRecover, setPendingContactRecover] = createSignal<{
    readonly accountIdBytes: Uint8Array;
    readonly accountIdB64: string;
    readonly username: string;
    readonly kind: ContactKind;
  } | null>(null);

  const client = new AuthClient();
  const discovery = new DiscoveryClient();
  // 1.16e5: drop active groups on logout/forgetDevice so MLS
  // state doesn't bleed across user sessions on the same tab.
  const chats = useChats();

  /**
   * ADR-0017 — finish a session once the canonical AccountIdentity
   * (phrase) seed is in hand: build the canonical Identity from the
   * seed, rehydrate MLS, refresh the published bundle, persist the
   * snapshot, set the in-memory secrets, and flip to `session`.
   * Shared by the warm same-device login / passkey paths and the
   * fresh-device `provideRecoveryPhrase` completion.
   */
  async function finishCanonicalSession(args: {
    wasm: Wasm;
    accountIdentitySeed: Uint8Array;
    accountIdBytes: Uint8Array;
    accountIdB64: string;
    exportKey: Uint8Array;
    sessionToken: string;
    sessionExpiresAtMs: number;
    username: string;
  }): Promise<void> {
    const canonical = args.wasm.Identity.fromSeed(args.accountIdentitySeed);
    chats.setIdentity(canonical);

    await rehydrateMlsState(chats, args.accountIdB64);

    // ADR-0017 defence-in-depth (lakko-security Phase-3A follow-up (i)) —
    // BEFORE re-publishing our own bundle, observe what is currently
    // published. If a DIFFERENT identity is on file for this account, it
    // means someone replaced it (the ACCESS-hijack signature: an attacker
    // minted a fresh identity via the lost-everything path). Raise a
    // dismissible alert so the owner finds out even if the attacker acted
    // first. Best-effort: a 404 (nothing published yet — fresh signup) or a
    // fetch error never alerts (the recover-path fail-closed gates own the
    // refuse-vs-alert distinction; this is a passive self-check). The
    // re-publish just below then heals the owner's published identity.
    setIdentityAlert(null);
    try {
      const current = await discovery.identityFetch(args.accountIdBytes);
      if (
        current !== null &&
        !bytesEqual(
          base64Decode(current.signature_pubkey),
          canonical.signaturePublicKey,
        )
      ) {
        setIdentityAlert({ kind: "published-identity-changed" });
      }
    } catch (err) {
      // eslint-disable-next-line no-console
      console.warn("self-identity consistency check failed", err);
    }

    try {
      await uploadIdentityBundle(discovery, args.accountIdBytes, canonical);
    } catch (err) {
      // eslint-disable-next-line no-console
      console.warn("identity bundle refresh failed", err);
    }
    try {
      await chats.persistSnapshot(args.accountIdB64);
    } catch (err) {
      // eslint-disable-next-line no-console
      console.warn("post-login MLS snapshot failed", err);
    }

    setIdentitySecrets({
      accountId: args.accountIdBytes,
      exportKey: args.exportKey,
      accountIdentitySeed: args.accountIdentitySeed,
      sessionToken: args.sessionToken,
      sessionExpiresAtMs: args.sessionExpiresAtMs,
    });
    setPendingRecovery(null);
    setState({
      kind: "session",
      account: { accountId: args.accountIdB64, username: args.username },
    });
    void subscribeToPush().catch((err) => {
      // eslint-disable-next-line no-console
      console.warn("push subscribe failed", err);
    });
  }

  /**
   * ADR-0018 ACCESS-only recovery landing (shared by factor (c) TOTP
   * and factor (d) phone/email OTP). The caller has already verified
   * an ACCESS factor server-side and holds the `(account_id,
   * session_token, expiry)` triple — but NO key material (these
   * factors carry no password, hence no OPAQUE export key). This runs
   * the EXISTING ADR-0017 identity-acquisition state machine:
   *
   *   - install the bearer token + persist the localStorage account
   *     marker;
   *   - WARM case — an already-stored device key decrypts a cached
   *     AccountIdentity seed: refresh the persisted token under it and
   *     `finishCanonicalSession` directly (exportKey empty — no
   *     password, the `completeFromDeviceRestore` convention);
   *   - COLD case — no stored device key OR no cached seed: park a
   *     `PendingRecovery` (exportKey empty; deviceKey carried when it
   *     exists so a later completion does not orphan its wrapped
   *     stores) and flip to `needs-recovery-phrase`, where the
   *     RestoreIdentityPrompt offers phrase / another-device.
   *
   * Extracted verbatim from the original `recoverWithTotp` tail — the
   * warm / cold branches are byte-for-byte the prior behavior.
   */
  async function landAfterAccessRecovery(
    username: string,
    accountIdBytes: Uint8Array,
    accountIdB64: string,
    triple: { sessionToken: string; sessionExpiresAtMs: number },
  ): Promise<void> {
    // Phase 1.5b: install the fresh bearer token on the
    // DiscoveryClient for any protected reads/writes that follow.
    discovery.setSessionToken(triple.sessionToken);
    // Persist the localStorage account marker so a reload knows
    // the account (important on a fresh browser where no row
    // existed).
    savePersisted({ v: 1, accountIdB64, username });

    // ADR-0017 identity acquisition. These factors carried NO
    // password, so there is no OPAQUE export key to derive a device
    // root key from — the ONLY same-device shortcut is an
    // already-stored device key that decrypts a cached
    // AccountIdentity seed.
    let deviceKey: CryptoKey | null = null;
    try {
      deviceKey = await getDeviceKey(accountIdB64);
    } catch (err) {
      // IDB unreachable — treat as "no key" and fall into the
      // fail-safe below.
      // eslint-disable-next-line no-console
      console.warn("device-key probe failed (access recover)", err);
      deviceKey = null;
    }
    let accountIdentitySeed: Uint8Array | null = null;
    if (deviceKey !== null) {
      // Refresh the persisted session token under the existing
      // device key. Best-effort, same posture as login.
      try {
        await putSessionToken(
          accountIdB64,
          triple.sessionToken,
          triple.sessionExpiresAtMs,
          deviceKey,
        );
      } catch (err) {
        // eslint-disable-next-line no-console
        console.warn("session-token persist (access recover) failed", err);
      }
      try {
        accountIdentitySeed = await getAccountIdentitySeed(
          accountIdB64,
          deviceKey,
        );
      } catch (err) {
        // Decrypt failure — treat as "no cached seed" and fall
        // into the fail-safe rather than the wrong-identity branch.
        // eslint-disable-next-line no-console
        console.warn(
          "account-identity-seed decrypt failed (access recover)",
          err,
        );
        accountIdentitySeed = null;
      }
    }

    if (deviceKey !== null && accountIdentitySeed !== null) {
      // Same-device case: the cached seed rebuilds the canonical
      // identity directly. `exportKey` is empty — these factors
      // involve no password (the completeFromDeviceRestore
      // convention).
      const wasm = await loadWasm();
      await finishCanonicalSession({
        wasm,
        accountIdentitySeed,
        accountIdBytes,
        accountIdB64,
        exportKey: new Uint8Array(0),
        sessionToken: triple.sessionToken,
        sessionExpiresAtMs: triple.sessionExpiresAtMs,
        username,
      });
      return;
    }

    // Fresh-device fail-safe (ADR-0017): ACCESS succeeded, identity
    // restore is pending. Park the context — exportKey EMPTY (no
    // password) and deviceKey possibly null (no stored key on this
    // browser; the completion paths bootstrap one from the verified
    // identity seed). When a stored device key DOES exist (e.g. a
    // prior password login on this browser whose seed cache is
    // missing), carry it — overwriting it with an identity-seed-
    // derived key would orphan every wrapped store.
    setPendingRecovery({
      accountIdBytes,
      accountIdB64,
      exportKey: new Uint8Array(0),
      deviceKey,
      sessionToken: triple.sessionToken,
      sessionExpiresAtMs: triple.sessionExpiresAtMs,
      username,
    });
    setState({
      kind: "needs-recovery-phrase",
      account: { accountId: accountIdB64, username },
    });
  }

  onMount(() => {
    // Phase 1.13c: hydrate both the AuthState AND the persisted
    // session token in one IDB round-trip — the token requires the
    // device key to decrypt, which `hydrateFromStorage` already
    // probes. Installing the token on the DiscoveryClient lets
    // protected reads (e.g. invite consume) work for a returning
    // user without forcing a password re-entry. The export key is
    // NOT persisted, so signing-requiring flows (sendContactRequest,
    // MLS commits) still surface the standard "please re-enter
    // password" prompt.
    void hydrateFromStorage().then(({ state, sessionToken }) => {
      setState(state);
      if (sessionToken !== null) {
        discovery.setSessionToken(sessionToken);
      }
    });
  });

  const api: AuthApi = {
    state,
    identitySecrets,
    freshPhrase: () => pendingFreshPhrase()?.phrase ?? null,
    identityAlert,
    dismissIdentityAlert: () => setIdentityAlert(null),
    async signup(username, password) {
      const trimmed = username.trim();
      if (trimmed.length === 0) {
        throw new Error("username must be non-empty");
      }
      const result = await client.signup(utf8(password));
      const accountIdB64 = base64Encode(result.accountId);
      // Phase 1.5b: install the fresh bearer token on the
      // DiscoveryClient so the registerUsername /
      // uploadIdentityBundle / uploadKeyPackage round-trip below
      // sails through the auth::require_session middleware.
      discovery.setSessionToken(result.sessionToken);

      // 1.16d2: claim the username server-side. We do this AFTER
      // the OPAQUE flow commits — if /discovery/register fails
      // the account still exists but the username is unbound,
      // which is recoverable on a future signup attempt with the
      // same desired handle (the user picks a different name)
      // or via a "set username" UI later. We ALSO surface the
      // failure rather than silently continuing — a 409 (taken)
      // is meaningful for the form.
      let normalisedUsername: string;
      try {
        const reg = await discovery.registerUsername(
          result.accountId,
          trimmed,
        );
        normalisedUsername = reg.normalisedUsername;
      } catch (err) {
        // Re-throw a friendlier message for the common case.
        if (err instanceof DiscoveryHttpError && err.status === 409) {
          throw new Error(
            `username @${trimmed} is already taken — pick another`,
          );
        }
        throw err;
      }

      savePersisted({
        v: 1,
        accountIdB64,
        username: normalisedUsername,
      });
      // Derive the device root key right after signup and persist
      // it — failing this AFTER the server commit would leave the
      // user with a working server-side account but no on-device
      // session restore. We surface the failure rather than
      // pretending signup succeeded silently.
      const deviceKey = await deriveDeviceRootKey(result.exportKey, accountIdB64);
      await putDeviceKey(accountIdB64, deviceKey);
      // Phase 1.13c: persist the session token under the device
      // key so a returning user can resume protected discovery
      // reads (e.g. invite consume) without re-typing the
      // password. Best-effort — a persistence failure does not
      // block the rest of signup; in-memory token still works
      // for this session.
      try {
        await putSessionToken(
          accountIdB64,
          result.sessionToken,
          result.sessionExpiresAtMs,
          deviceKey,
        );
      } catch (err) {
        // eslint-disable-next-line no-console
        console.warn("session-token persist (signup) failed", err);
      }

      // Phase 2a2c2 — cache the OPAQUE export key under the
      // device root key so a future sign-in-with-passkey on the
      // SAME device can reconstruct the full IdentitySecrets
      // without going through the password round-trip.
      // Best-effort: a persistence failure here just means the
      // user falls back to password login on this device.
      try {
        await putExportKey(accountIdB64, result.exportKey, deviceKey);
      } catch (err) {
        // eslint-disable-next-line no-console
        console.warn("export-key persist (signup) failed", err);
      }

      // Phase 2a3b0 (ADR-0007) — ensure a random per-device
      // DeviceIdentity seed exists on this device. Distinct
      // from the deterministic AccountIdentity above; used as
      // the MLS leaf in the user's DeviceGroup (2a3a) once the
      // add-device flow lands. Best-effort — a persistence
      // failure here doesn't block signup; the seed gets
      // generated on the next login.
      try {
        await ensureDeviceIdentitySeed(accountIdB64, deviceKey);
      } catch (err) {
        // eslint-disable-next-line no-console
        console.warn("device-identity ensure (signup) failed", err);
      }

      // ADR-0017: the canonical AccountIdentity is now derived
      // from the stable BIP39 recovery-phrase seed, NOT the
      // password export key — so a future recover / password
      // change preserves the identity. We need the phrase here,
      // so fail loud BEFORE deriving the identity if it is absent
      // (1.16c3c1 always enrols recovery; a missing phrase is a
      // code-path bypass we must not navigate the user past).
      if (result.recoveryPhrase === undefined) {
        throw new Error(
          "internal: signup completed without a recovery phrase",
        );
      }
      const wasm = await loadWasm();
      const mnemonic = wasm.RecoveryMnemonic.fromPhrase(
        result.recoveryPhrase,
      );
      const accountIdentitySeed = mnemonic.toSeed();
      mnemonic.free();

      // ADR-0017: cache the AccountIdentity (phrase) seed under the
      // device root key — same wrapping as the export-key cache — so
      // a same-device login / passkey-resume can rebuild the
      // canonical identity without re-entering the phrase.
      // Best-effort: a persistence failure just means a future
      // fresh-resume on this device will fall into the
      // "needs recovery phrase" fail-safe.
      try {
        await putAccountIdentitySeed(
          accountIdB64,
          accountIdentitySeed,
          deviceKey,
        );
      } catch (err) {
        // eslint-disable-next-line no-console
        console.warn("account-identity-seed persist (signup) failed", err);
      }

      // 1.16e6c1: build the canonical session-wide Identity
      // before uploading the KeyPackage so the freshly-minted
      // init/leaf private keys land in the provider we'll
      // snapshot. ChatProvider holds the reference for the
      // lifetime of the session and will free() it on
      // logout/forgetDevice. ADR-0017: seeded from the phrase seed.
      const canonical = wasm.Identity.fromSeed(accountIdentitySeed);
      chats.setIdentity(canonical);

      // 1.16e6c3: rehydrate prior MLS state if any. For signup
      // this is a no-op (brand-new accountId, no snapshot row),
      // but routing it through the same helper keeps the four
      // auth flows uniform.
      await rehydrateMlsState(chats, accountIdB64);

      // 1.16d3b2: derive the Identity deterministically from the
      // (phrase) seed, upload its public bundle to the discovery
      // directory. Because Identity::fromSeed is deterministic, the
      // SAME bytes will be uploaded on every future login — the
      // server upserts. We do this BEFORE flipping state so a failure
      // here surfaces as a signup error rather than a half-broken
      // session.
      //
      // G-30 #2 (ADR-0019): this is the FIRST-EVER bundle publish — a
      // brand-new `(account_key -> pubkey)` transparency-log binding.
      // Mint the genesis leaf self-signature and ship it as
      // `leafAuth` so the append-side authorisation check accepts the
      // new binding. Fail-loud: a mint failure rejects the signup
      // here rather than publishing a bundle the server will refuse.
      const signupLeafAuth = await mintLeafGenesisAuth(
        canonical,
        result.accountId,
      );
      await uploadIdentityBundle(
        discovery,
        result.accountId,
        canonical,
        undefined,
        signupLeafAuth,
      );

      // 1.16e6c1: persist the freshly-built provider state
      // (the just-uploaded KeyPackage's private key material)
      // so a peer's Welcome can be opened after a reload.
      // Best-effort — a snapshot failure here shouldn't block
      // the user from getting into a working session.
      try {
        await chats.persistSnapshot(accountIdB64);
      } catch (err) {
        // eslint-disable-next-line no-console
        console.warn("initial MLS snapshot failed", err);
      }

      // 1.16d3c: hand off the export key for in-memory use by
      // ContactsClient + similar. Cleared on logout.
      // ADR-0017: also carry the phrase seed — the canonical
      // identity seed downstream callers rebuild from.
      // Phase 1.5b: also stash the bearer token so DiscoveryClient
      // can thread it through every protected write.
      setIdentitySecrets({
        accountId: result.accountId,
        exportKey: result.exportKey,
        accountIdentitySeed,
        sessionToken: result.sessionToken,
        sessionExpiresAtMs: result.sessionExpiresAtMs,
      });
      setState({
        kind: "session",
        account: { accountId: accountIdB64, username: normalisedUsername },
      });
      // Phase 2a1c: post-auth backup bootstrap. Caches the
      // phrase under the device root key + posts a baseline
      // backup envelope to /backup. Best-effort end-to-end; the
      // helper logs failures and never throws so a transient
      // server hiccup does not block signup from completing.
      void bootstrapBackupFromPhraseString(
        {
          accountId: result.accountId,
          accountIdB64,
          ed25519Seed: canonical.ed25519Seed,
          deviceKey,
          sessionToken: result.sessionToken,
        },
        result.recoveryPhrase,
      );
      // 1.20b: best-effort push subscribe. The orchestrator is
      // a no-op when the build has no VAPID public key set or the
      // user denies the Notification permission prompt — fall
      // back to foreground polling without throwing.
      void subscribeToPush().catch((err) => {
        // eslint-disable-next-line no-console
        console.warn("push subscribe (signup) failed", err);
      });
      return { recoveryPhrase: result.recoveryPhrase };
    },
    async signupGuest() {
      // ADR-0032 guest join. This is the `signup` flow VERBATIM up to (but
      // NOT including) the username claim: OPAQUE registration under a
      // RANDOM password, a device root key derived from the resulting
      // export key, the first-ever identity-bundle publish, and the
      // AccountIdentity-seed cache. It then parks the recovery phrase and
      // flips to `save-fresh-phrase` (like `mintFreshIdentity`) instead of
      // landing the session directly, so the existing `SaveFreshPhrasePrompt`
      // + `confirmFreshPhraseSaved` render + land it unchanged.
      //
      // The password is 32 random bytes the user NEVER sees — recovery is
      // phrase-only, matching the ACCESS-only convention the recovery
      // cascade already uses. No username is claimed; the account is
      // username-less until the onboarding step below.
      const randomPassword = base64Encode(
        crypto.getRandomValues(new Uint8Array(32)),
      );
      const result = await client.signup(utf8(randomPassword));
      const accountIdB64 = base64Encode(result.accountId);
      // Install the fresh bearer token for the protected writes below.
      discovery.setSessionToken(result.sessionToken);

      // TODO(ADR-0032 onboarding): claiming a username is a LATER step —
      // the user calls `discovery.registerUsername(accountId, chosenName)`
      // (via a future "claim your username" UI) once they want to be
      // discoverable / recover cross-device. We intentionally SKIP
      // `registerUsername` here so the guest account stays username-less
      // and the user can claim their preferred (immutable) handle later.

      if (result.recoveryPhrase === undefined) {
        throw new Error(
          "internal: guest signup completed without a recovery phrase",
        );
      }

      // Device root key from the OPAQUE export key (the guest HAS a real,
      // random password, so this is the normal signup derivation) + persist
      // the key / session token / export key, exactly as `signup` does.
      const deviceKey = await deriveDeviceRootKey(result.exportKey, accountIdB64);
      await putDeviceKey(accountIdB64, deviceKey);
      try {
        await putSessionToken(
          accountIdB64,
          result.sessionToken,
          result.sessionExpiresAtMs,
          deviceKey,
        );
      } catch (err) {
        // eslint-disable-next-line no-console
        console.warn("session-token persist (signupGuest) failed", err);
      }
      try {
        await putExportKey(accountIdB64, result.exportKey, deviceKey);
      } catch (err) {
        // eslint-disable-next-line no-console
        console.warn("export-key persist (signupGuest) failed", err);
      }
      try {
        await ensureDeviceIdentitySeed(accountIdB64, deviceKey);
      } catch (err) {
        // eslint-disable-next-line no-console
        console.warn("device-identity ensure (signupGuest) failed", err);
      }

      // ADR-0017: derive the canonical AccountIdentity seed from the phrase
      // + cache it under the device key (same as signup).
      const wasm = await loadWasm();
      const mnemonic = wasm.RecoveryMnemonic.fromPhrase(result.recoveryPhrase);
      const accountIdentitySeed = mnemonic.toSeed();
      mnemonic.free();
      try {
        await putAccountIdentitySeed(
          accountIdB64,
          accountIdentitySeed,
          deviceKey,
        );
      } catch (err) {
        // eslint-disable-next-line no-console
        console.warn("account-identity-seed persist (signupGuest) failed", err);
      }

      // Persist the localStorage account marker — username EMPTY (unclaimed).
      savePersisted({ v: 1, accountIdB64, username: "" });

      // G-30 #2 (ADR-0019): publish the FIRST-EVER bundle WITH the genesis
      // leaf self-signature so the transparency-log append accepts the new
      // binding — identical to `signup`. Fail-loud: a mint / upload failure
      // rejects the guest signup rather than parking an unverifiable identity.
      const canonical = wasm.Identity.fromSeed(accountIdentitySeed);
      try {
        const guestLeafAuth = await mintLeafGenesisAuth(
          canonical,
          result.accountId,
        );
        await uploadIdentityBundle(
          discovery,
          result.accountId,
          canonical,
          undefined,
          guestLeafAuth,
        );
      } finally {
        canonical.free();
      }

      // Park the completion context + flip to `save-fresh-phrase` so the UI
      // shows the recovery phrase before the session lands (the
      // `SaveFreshPhrasePrompt` → `confirmFreshPhraseSaved` path builds the
      // canonical Identity, uploads the KeyPackage, and lands the session).
      setPendingFreshPhrase({
        seed: accountIdentitySeed,
        accountIdBytes: result.accountId,
        accountIdB64,
        sessionToken: result.sessionToken,
        sessionExpiresAtMs: result.sessionExpiresAtMs,
        username: "",
        phrase: result.recoveryPhrase,
      });
      setState({
        kind: "save-fresh-phrase",
        account: { accountId: accountIdB64, username: "" },
      });
      return { recoveryPhrase: result.recoveryPhrase };
    },
    async login(username, password) {
      const trimmed = username.trim();
      const cached = loadPersisted();

      // Resolve the typed username to an account_id. Two paths:
      //   1. Cache hit on this device — same username as cached:
      //      use the cached id directly. Saves one round-trip
      //      and works offline (well, against a cached id; the
      //      OPAQUE flow itself still needs the server).
      //   2. Cache miss OR mismatch — call discovery.lookup. The
      //      result either binds the typed username to its
      //      account_id (cross-device login from a fresh browser)
      //      or returns null (unknown username).
      let accountIdBytes: Uint8Array;
      let resolvedUsername: string;
      if (cached !== null && cached.username === trimmed) {
        accountIdBytes = base64Decode(cached.accountIdB64);
        resolvedUsername = cached.username;
      } else {
        const lookup = await discovery.lookup(trimmed);
        if (lookup.account === null) {
          // Indistinguishable from "wrong password" — we don't
          // help an attacker probe the username space.
          throw new Error("invalid credentials");
        }
        accountIdBytes = lookup.account;
        resolvedUsername = lookup.normalisedUsername;
      }

      const result = await client.login(accountIdBytes, utf8(password));
      const accountIdB64 = base64Encode(accountIdBytes);
      // Phase 1.5b: install the fresh bearer token on the
      // DiscoveryClient for any protected writes that follow.
      discovery.setSessionToken(result.sessionToken);

      // ADR-0018: PREFER an already-stored device root key over
      // re-deriving from the export key. On a password-bootstrapped
      // device the two are identical (HKDF of the deterministic
      // OPAQUE export key — so preferring stored is
      // behavior-compatible); on a device bootstrapped via
      // another-device restore (path (b), no password at bootstrap
      // time) the stored key is identity-seed-derived and a
      // re-derivation from the export key would orphan every
      // wrapped store on this browser.
      let deviceKey = await getDeviceKey(accountIdB64);
      if (deviceKey === null) {
        // OPAQUE export_key is deterministic for the same password,
        // so deriving here yields the same bytes every login.
        deviceKey = await deriveDeviceRootKey(result.exportKey, accountIdB64);
      }
      // Upsert is harmless; also persist the (accountId, username)
      // marker so subsequent reloads skip the lookup round-trip.
      await putDeviceKey(accountIdB64, deviceKey);
      // Phase 1.13c: refresh the persisted session token alongside
      // the device key. Same best-effort posture as signup —
      // persistence failure does not block login.
      try {
        await putSessionToken(
          accountIdB64,
          result.sessionToken,
          result.sessionExpiresAtMs,
          deviceKey,
        );
      } catch (err) {
        // eslint-disable-next-line no-console
        console.warn("session-token persist (login) failed", err);
      }
      // Phase 2a2c2 — refresh the cached export key. Same
      // best-effort posture.
      try {
        await putExportKey(accountIdB64, result.exportKey, deviceKey);
      } catch (err) {
        // eslint-disable-next-line no-console
        console.warn("export-key persist (login) failed", err);
      }
      // Phase 2a3b0 (ADR-0007) — ensure DeviceIdentity seed.
      // Idempotent: a returning user with a seed already on
      // file gets the cached one; first-login generates fresh
      // bytes.
      try {
        await ensureDeviceIdentitySeed(accountIdB64, deviceKey);
      } catch (err) {
        // eslint-disable-next-line no-console
        console.warn("device-identity ensure (login) failed", err);
      }
      savePersisted({ v: 1, accountIdB64, username: resolvedUsername });

      // ADR-0017: the canonical AccountIdentity is seeded from the
      // BIP39 phrase seed, NOT the export key. Read the cached seed
      // (warm same-device path). If present, finish the session
      // with it. If ABSENT (fresh device, or a device that only
      // ever held the export key), we MUST NOT (a) derive the
      // identity from the export key — that is the wrong identity
      // now — nor (b) silently mint a fresh one — that breaks every
      // contact's pin. Instead we flip to the `needs-recovery-phrase`
      // fail-safe state and let the user supply their phrase.
      const wasm = await loadWasm();
      let accountIdentitySeed: Uint8Array | null = null;
      try {
        accountIdentitySeed = await getAccountIdentitySeed(
          accountIdB64,
          deviceKey,
        );
      } catch (err) {
        // Decrypt failure (device key rotated, ciphertext tampered)
        // — treat as "no cached seed" and fall into the fail-safe
        // rather than the wrong-identity branch.
        // eslint-disable-next-line no-console
        console.warn("account-identity-seed decrypt failed (login)", err);
        accountIdentitySeed = null;
      }

      if (accountIdentitySeed === null) {
        // Fresh-device fail-safe (ADR-0017). Auth succeeded; identity
        // restore is pending the phrase. Stash the context and flip
        // state — `provideRecoveryPhrase` finishes from here.
        setPendingRecovery({
          accountIdBytes,
          accountIdB64,
          exportKey: result.exportKey,
          deviceKey,
          sessionToken: result.sessionToken,
          sessionExpiresAtMs: result.sessionExpiresAtMs,
          username: resolvedUsername,
        });
        setState({
          kind: "needs-recovery-phrase",
          account: { accountId: accountIdB64, username: resolvedUsername },
        });
        return;
      }

      await finishCanonicalSession({
        wasm,
        accountIdentitySeed,
        accountIdBytes,
        accountIdB64,
        exportKey: result.exportKey,
        sessionToken: result.sessionToken,
        sessionExpiresAtMs: result.sessionExpiresAtMs,
        username: resolvedUsername,
      });
    },
    async loginWithPasskey() {
      // Phase 2a2c2 — sign in with a WebAuthn passkey on a
      // device that has already been seen by this account at
      // least once (signup / login / recover ran here in the
      // past + populated the export-key cache).
      //
      // The fresh-device case stays UNSUPPORTED on purpose:
      // without a locally-cached export key the passkey gives
      // us only a session token, but no path to derive Identity
      // / device key / MLS bootstrap. The recovery phrase
      // remains the only fresh-device entry route. UI should
      // gate the affordance on cache presence.
      const webauthn = new WebauthnClient();
      let assertion;
      try {
        assertion = await webauthn.loginWithPasskey();
      } catch (err) {
        if (err instanceof WebauthnUserCancelledError) {
          throw new Error("Passkey sign-in cancelled.");
        }
        if (err instanceof WebauthnUnsupportedError) {
          throw new Error("This browser doesn't support passkeys.");
        }
        if (err instanceof WebauthnHttpError) {
          if (err.status === 503) {
            throw new Error("Server hasn't enabled passkey support.");
          }
          if (err.status === 404) {
            throw new Error(
              "No passkey is registered for this account. Sign in with " +
                "password first, then add a passkey from Home.",
            );
          }
          throw new Error(`Passkey sign-in failed (${err.status}).`);
        }
        if (err instanceof WebauthnNetworkError) {
          throw new Error("Network error during passkey sign-in.");
        }
        throw err;
      }

      const accountIdB64 = assertion.account_id_b64;
      const accountIdBytes = base64Decode(accountIdB64);
      const cached = loadPersisted();
      if (cached === null || cached.accountIdB64 !== accountIdB64) {
        // The passkey resolved to an account this device has
        // never seen — without a locally-cached export key we
        // can't reconstruct the session. Tell the user to log
        // in with password first; the password flow populates
        // the cache and the next passkey tap will work.
        throw new Error(
          "This passkey belongs to a different account than the one " +
            "cached on this device. Sign in with password first to " +
            "enable passkey on this device.",
        );
      }

      const deviceKey = await getDeviceKey(accountIdB64);
      if (deviceKey === null) {
        throw new Error(
          "No device key on this device. Sign in with password or " +
            "recover with your phrase to enable passkey sign-in.",
        );
      }

      let exportKey: Uint8Array | null;
      try {
        exportKey = await getExportKey(accountIdB64, deviceKey);
      } catch (err) {
        // Decrypt failure — typically device key rotated since
        // the cache was written. UX-wise indistinguishable from
        // "cache absent"; surface the same message.
        // eslint-disable-next-line no-console
        console.warn("export-key cache decrypt failed", err);
        exportKey = null;
      }
      if (exportKey === null) {
        throw new Error(
          "No cached credentials for passkey sign-in on this device. " +
            "Sign in with password or recover with your phrase first.",
        );
      }

      // From here on we have everything we need to reconstruct
      // a real IdentitySecrets-driven session. Mirrors the
      // tail of `login()` minus the OPAQUE round-trip.

      // Phase 1.5b: install the fresh bearer token from the
      // WebAuthn /finish response on the DiscoveryClient.
      discovery.setSessionToken(assertion.session_token);

      // ADR-0018: PREFER the stored device root key (we already
      // loaded it above — it just decrypted the export-key cache,
      // proving it is the right wrapping key). Pre-ADR-0018 this
      // path re-derived from the cached export key, which yields
      // identical bytes on a password-bootstrapped device — but on
      // an another-device-restored browser the stored key is
      // identity-seed-derived, and a re-derivation would clobber it
      // and orphan every wrapped store. The upsert keeps the IDB
      // freshness clock moving.
      const newDeviceKey = deviceKey;
      await putDeviceKey(accountIdB64, newDeviceKey);

      // Persist the new session token from the passkey /finish
      // under the device key. Best-effort.
      try {
        await putSessionToken(
          accountIdB64,
          assertion.session_token,
          assertion.session_expires_at_ms,
          newDeviceKey,
        );
      } catch (err) {
        // eslint-disable-next-line no-console
        console.warn("session-token persist (passkey login) failed", err);
      }

      // ADR-0017: build the canonical Identity from the cached
      // AccountIdentity (phrase) seed, NOT the export key. Passkey
      // resume only runs on a device that already cached the export
      // key, so the phrase seed is normally present too — but a
      // device that last logged in before this cache existed may
      // lack it, in which case we fail closed into the
      // `needs-recovery-phrase` state (never derive from exportKey,
      // never mint fresh).
      const wasm = await loadWasm();
      let accountIdentitySeed: Uint8Array | null = null;
      try {
        accountIdentitySeed = await getAccountIdentitySeed(
          accountIdB64,
          newDeviceKey,
        );
      } catch (err) {
        // eslint-disable-next-line no-console
        console.warn(
          "account-identity-seed decrypt failed (passkey login)",
          err,
        );
        accountIdentitySeed = null;
      }

      if (accountIdentitySeed === null) {
        setPendingRecovery({
          accountIdBytes,
          accountIdB64,
          exportKey,
          deviceKey: newDeviceKey,
          sessionToken: assertion.session_token,
          sessionExpiresAtMs: assertion.session_expires_at_ms,
          username: cached.username,
        });
        setState({
          kind: "needs-recovery-phrase",
          account: { accountId: accountIdB64, username: cached.username },
        });
        return;
      }

      await finishCanonicalSession({
        wasm,
        accountIdentitySeed,
        accountIdBytes,
        accountIdB64,
        exportKey,
        sessionToken: assertion.session_token,
        sessionExpiresAtMs: assertion.session_expires_at_ms,
        username: cached.username,
      });
    },
    async provideRecoveryPhrase(mnemonicPhrase) {
      // ADR-0017 fresh-device fail-safe completion. Only valid while
      // a `pendingRecovery` is parked (set by login / passkey when
      // the auth succeeded but no AccountIdentity seed was cached).
      const pending = pendingRecovery();
      if (pending === null) {
        throw new Error(
          "no pending identity recovery — sign in first",
        );
      }
      const wasm = await loadWasm();
      // Parse the phrase → derive the BIP39 seed → fail-closed
      // verification of the derived pubkey against the account's
      // PUBLISHED bundle (shared with the ADR-0018 pairing
      // completions; zeroizes the seed on every refusal).
      const mnemonic = wasm.RecoveryMnemonic.fromPhrase(mnemonicPhrase);
      const accountIdentitySeed = mnemonic.toSeed();
      mnemonic.free();
      await verifySeedAgainstPublishedIdentity({
        wasm,
        discovery,
        accountIdBytes: pending.accountIdBytes,
        accountIdentitySeed,
      });

      // ADR-0018 factor (c): a TOTP-recovered fresh browser parked
      // with NO device root key (no password ⇒ no export key to
      // derive one from). Bootstrap it from the VERIFIED identity
      // seed — exactly what `completeFromDeviceRestore` does — and
      // ONLY here, strictly AFTER the fail-closed verification
      // above: a refusal must leave this device untouched.
      let deviceKey = pending.deviceKey;
      let bootstrappedDeviceKey = false;
      if (deviceKey === null) {
        deviceKey = await deriveDeviceRootKeyFromIdentitySeed(
          accountIdentitySeed,
          pending.accountIdB64,
        );
        await putDeviceKey(pending.accountIdB64, deviceKey);
        bootstrappedDeviceKey = true;
      }

      // Cache the phrase seed under the device key so future warm
      // logins skip the prompt. Best-effort.
      try {
        await putAccountIdentitySeed(
          pending.accountIdB64,
          accountIdentitySeed,
          deviceKey,
        );
      } catch (err) {
        // eslint-disable-next-line no-console
        console.warn(
          "account-identity-seed persist (provideRecoveryPhrase) failed",
          err,
        );
      }
      // On a bootstrapped key the pending session token was never
      // persisted (there was no key at park time) — land it now so a
      // reload re-attaches. Best-effort.
      if (bootstrappedDeviceKey) {
        try {
          await putSessionToken(
            pending.accountIdB64,
            pending.sessionToken,
            pending.sessionExpiresAtMs,
            deviceKey,
          );
        } catch (err) {
          // eslint-disable-next-line no-console
          console.warn(
            "session-token persist (provideRecoveryPhrase) failed",
            err,
          );
        }
      }

      await finishCanonicalSession({
        wasm,
        accountIdentitySeed,
        accountIdBytes: pending.accountIdBytes,
        accountIdB64: pending.accountIdB64,
        exportKey: pending.exportKey,
        sessionToken: pending.sessionToken,
        sessionExpiresAtMs: pending.sessionExpiresAtMs,
        username: pending.username,
      });
    },
    async completeFromPairing(phraseSeed, sessionToken, sessionExpiresAtMs) {
      // ADR-0018 factor (a), path (a) — same precondition as
      // `provideRecoveryPhrase`: only valid while a login/passkey
      // auth is parked in the `needs-recovery-phrase` fail-safe.
      const pending = pendingRecovery();
      if (pending === null) {
        phraseSeed.fill(0);
        throw new Error("no pending identity recovery — sign in first");
      }
      if (phraseSeed.length !== ACCOUNT_IDENTITY_SEED_LENGTH) {
        phraseSeed.fill(0);
        throw new Error(
          "internal: transferred identity seed has the wrong length",
        );
      }
      const wasm = await loadWasm();
      // SAME fail-closed verification as the typed-phrase path
      // (ADR-0017 + lakko-security finding #1): the transferred seed
      // must reproduce the account's PUBLISHED AccountIdentity key;
      // hard-block + zeroize on mismatch or an unverifiable bundle.
      await verifySeedAgainstPublishedIdentity({
        wasm,
        discovery,
        accountIdBytes: pending.accountIdBytes,
        accountIdentitySeed: phraseSeed,
        mismatchMessage:
          "The identity received from your other device doesn't match " +
          "this account's published identity — refusing to use it.",
      });

      // ADR-0018 factor (c): a TOTP-recovered fresh browser parked
      // with NO device root key. Bootstrap it from the VERIFIED
      // transferred seed (the `completeFromDeviceRestore` recipe),
      // strictly AFTER the fail-closed verification above — a
      // refusal must leave this device untouched.
      let deviceKey = pending.deviceKey;
      let bootstrappedDeviceKey = false;
      if (deviceKey === null) {
        deviceKey = await deriveDeviceRootKeyFromIdentitySeed(
          phraseSeed,
          pending.accountIdB64,
        );
        await putDeviceKey(pending.accountIdB64, deviceKey);
        bootstrappedDeviceKey = true;
      }

      // Cache the seed under the device key (on the password /
      // passkey paths it already existed; on the TOTP path it was
      // just bootstrapped). Best-effort.
      try {
        await putAccountIdentitySeed(
          pending.accountIdB64,
          phraseSeed,
          deviceKey,
        );
      } catch (err) {
        // eslint-disable-next-line no-console
        console.warn(
          "account-identity-seed persist (completeFromPairing) failed",
          err,
        );
      }

      // Prefer the redeemed fresh session token when the pairing
      // carried one; the pending login's token otherwise.
      let effectiveToken = pending.sessionToken;
      let effectiveExpiry = pending.sessionExpiresAtMs;
      if (sessionToken !== undefined && sessionExpiresAtMs !== undefined) {
        effectiveToken = sessionToken;
        effectiveExpiry = sessionExpiresAtMs;
        discovery.setSessionToken(sessionToken);
      }
      // Persist the effective token when a fresh one arrived (the
      // pre-existing behavior) OR when the key was just bootstrapped
      // (the pending token was never persisted — there was no key at
      // park time). Best-effort.
      if (
        (sessionToken !== undefined && sessionExpiresAtMs !== undefined) ||
        bootstrappedDeviceKey
      ) {
        try {
          await putSessionToken(
            pending.accountIdB64,
            effectiveToken,
            effectiveExpiry,
            deviceKey,
          );
        } catch (err) {
          // eslint-disable-next-line no-console
          console.warn(
            "session-token persist (completeFromPairing) failed",
            err,
          );
        }
      }

      await finishCanonicalSession({
        wasm,
        accountIdentitySeed: phraseSeed,
        accountIdBytes: pending.accountIdBytes,
        accountIdB64: pending.accountIdB64,
        exportKey: pending.exportKey,
        sessionToken: effectiveToken,
        sessionExpiresAtMs: effectiveExpiry,
        username: pending.username,
      });
    },
    async completeFromDeviceRestore(restoreArgs) {
      // ADR-0018 factor (a), path (b) — NO-PASSWORD bootstrap. The
      // caller ran the anonymous pairing and holds the transferred
      // seed + the REDEEMED session token (the only ACCESS here).
      const {
        accountIdBytes,
        accountIdB64,
        username,
        phraseSeed,
        sessionToken,
        sessionExpiresAtMs,
        persistJoinState,
      } = restoreArgs;
      if (phraseSeed.length !== ACCOUNT_IDENTITY_SEED_LENGTH) {
        phraseSeed.fill(0);
        throw new Error(
          "internal: transferred identity seed has the wrong length",
        );
      }
      const wasm = await loadWasm();
      // FAIL CLOSED FIRST — before ANY persistence. A mismatched /
      // unverifiable seed must leave this browser untouched.
      await verifySeedAgainstPublishedIdentity({
        wasm,
        discovery,
        accountIdBytes,
        accountIdentitySeed: phraseSeed,
        mismatchMessage:
          "The identity received from your other device doesn't match " +
          "this account's published identity — refusing to use it.",
      });

      // Bootstrap the device root key FROM the identity seed — there
      // is no password (hence no OPAQUE export key) on this device
      // yet. Later password logins PREFER this stored key over
      // re-deriving from the export key, so the wrapped stores stay
      // readable.
      const deviceKey = await deriveDeviceRootKeyFromIdentitySeed(
        phraseSeed,
        accountIdB64,
      );
      await putDeviceKey(accountIdB64, deviceKey);
      try {
        await putAccountIdentitySeed(accountIdB64, phraseSeed, deviceKey);
      } catch (err) {
        // eslint-disable-next-line no-console
        console.warn(
          "account-identity-seed persist (completeFromDeviceRestore) failed",
          err,
        );
      }
      // The redeemed token is REQUIRED on this path — persist it so
      // a reload re-attaches without any credential. Best-effort.
      try {
        await putSessionToken(
          accountIdB64,
          sessionToken,
          sessionExpiresAtMs,
          deviceKey,
        );
      } catch (err) {
        // eslint-disable-next-line no-console
        console.warn(
          "session-token persist (completeFromDeviceRestore) failed",
          err,
        );
      }
      savePersisted({ v: 1, accountIdB64, username });
      // Land the pairing's in-memory DeviceIdentity seed +
      // DeviceGroup snapshot under the fresh device key. Best-effort
      // — a failure orphans the DeviceGroup leaf locally; a re-pair
      // recovers it.
      if (persistJoinState !== undefined) {
        try {
          await persistJoinState(deviceKey);
        } catch (err) {
          // eslint-disable-next-line no-console
          console.warn(
            "join-state persist (completeFromDeviceRestore) failed",
            err,
          );
        }
      }

      discovery.setSessionToken(sessionToken);
      setPendingRecovery(null);
      await finishCanonicalSession({
        wasm,
        accountIdentitySeed: phraseSeed,
        accountIdBytes,
        accountIdB64,
        // ADR-0018 path (b): no password on this device yet, so no
        // OPAQUE export key exists. The device root key is
        // identity-seed-derived; downstream consumers of
        // `identitySecrets.exportKey` only use it as an auth-factor
        // cache, which this device legitimately does not have.
        exportKey: new Uint8Array(0),
        sessionToken,
        sessionExpiresAtMs,
        username,
      });
    },
    async mintFreshIdentity() {
      // ADR-0017 lost-everything fallback — the ONLY sanctioned path
      // that publishes a CHANGED AccountIdentity key WITHOUT a
      // supersession proof. Reachable ONLY while a `pendingRecovery`
      // is parked: the user recovered ACCESS via an ADR-0018 factor
      // and is sitting in `needs-recovery-phrase` with NO phrase and
      // NO other device. The explicit user button is the ONLY trigger
      // (the RestoreIdentityPrompt gates it behind a collapsible
      // warning); this method never runs automatically.
      const pending = pendingRecovery();
      if (pending === null) {
        throw new Error(
          "no pending identity recovery — sign in first",
        );
      }
      const wasm = await loadWasm();

      // Mint a BRAND-NEW recovery phrase + derive its 64-byte BIP39
      // seed. This is a fresh random identity for the SAME account —
      // there is no old seed to bridge from, which is exactly why
      // publishing without a proof is correct here.
      const mnemonic = wasm.RecoveryMnemonic.generate();
      const phrase = mnemonic.phrase;
      const accountIdentitySeed = mnemonic.toSeed();
      mnemonic.free();

      // Bootstrap a device key from the new seed when the pending
      // context has none (the ACCESS-only fresh-browser case — no
      // password, hence no OPAQUE export key to derive one from).
      // When a device key IS parked (e.g. a prior password login on
      // this browser whose seed cache was missing), reuse it — minting
      // a fresh identity-seed-derived key would orphan its wrapped
      // stores. Mirrors the bootstrap recipe in provideRecoveryPhrase /
      // completeFromDeviceRestore.
      let deviceKey = pending.deviceKey;
      let bootstrappedDeviceKey = false;
      if (deviceKey === null) {
        deviceKey = await deriveDeviceRootKeyFromIdentitySeed(
          accountIdentitySeed,
          pending.accountIdB64,
        );
        await putDeviceKey(pending.accountIdB64, deviceKey);
        bootstrappedDeviceKey = true;
      }

      // Cache the new seed under the device key so a future warm
      // login skips the prompt. Best-effort.
      try {
        await putAccountIdentitySeed(
          pending.accountIdB64,
          accountIdentitySeed,
          deviceKey,
        );
      } catch (err) {
        // eslint-disable-next-line no-console
        console.warn(
          "account-identity-seed persist (mintFreshIdentity) failed",
          err,
        );
      }
      // On a freshly-bootstrapped key the pending session token was
      // never persisted (there was no key at park time) — land it now
      // so a reload re-attaches. Best-effort.
      if (bootstrappedDeviceKey) {
        try {
          await putSessionToken(
            pending.accountIdB64,
            pending.sessionToken,
            pending.sessionExpiresAtMs,
            deviceKey,
          );
        } catch (err) {
          // eslint-disable-next-line no-console
          console.warn(
            "session-token persist (mintFreshIdentity) failed",
            err,
          );
        }
      }

      // Publish the NEW bundle WITHOUT a supersession proof — none is
      // mintable (there is no old seed). This is the single accepted
      // publish-without-proof case: the user's contacts will see a
      // changed key with no proof, fail closed, and re-verify
      // out-of-band. We surface a publish failure rather than landing
      // a session whose published identity nobody can verify.
      //
      // G-30 #2 (ADR-0019): the fresh-start mints a brand-new key, so
      // the transparency log treats it as a NEW binding (a "fresh
      // start", not a rotation — there is no old key to sign a
      // supersession proof). Authorise it with a genesis leaf
      // self-signature instead. Minted from `canonical` BEFORE the
      // upload (and before the `finally` free); a mint failure rejects
      // the whole mint flow rather than publishing an unauthorised
      // bundle the server will refuse.
      const canonical = wasm.Identity.fromSeed(accountIdentitySeed);
      try {
        const freshLeafAuth = await mintLeafGenesisAuth(
          canonical,
          pending.accountIdBytes,
        );
        await uploadIdentityBundle(
          discovery,
          pending.accountIdBytes,
          canonical,
          undefined,
          freshLeafAuth,
        );
      } finally {
        canonical.free();
      }

      // Stash the completion context + flip to `save-fresh-phrase`.
      // We do NOT finish the session here — the UI must show the new
      // phrase first and call `confirmFreshPhraseSaved`. `pendingRecovery`
      // is cleared now; the flow is committed past the point of
      // returning to the recovery prompt.
      setPendingFreshPhrase({
        seed: accountIdentitySeed,
        accountIdBytes: pending.accountIdBytes,
        accountIdB64: pending.accountIdB64,
        sessionToken: pending.sessionToken,
        sessionExpiresAtMs: pending.sessionExpiresAtMs,
        username: pending.username,
        phrase,
      });
      setPendingRecovery(null);
      setState({
        kind: "save-fresh-phrase",
        account: {
          accountId: pending.accountIdB64,
          username: pending.username,
        },
      });
      return phrase;
    },
    async confirmFreshPhraseSaved() {
      // ADR-0017 lost-everything fallback, step 2. Only valid while a
      // `pendingFreshPhrase` is parked (set by `mintFreshIdentity`
      // after the new bundle was published). The user has saved the
      // new phrase; land the canonical session from the stashed
      // context (empty exportKey — ACCESS-only) and clear the stash.
      const pending = pendingFreshPhrase();
      if (pending === null) {
        throw new Error(
          "no pending fresh-identity phrase to confirm",
        );
      }
      const wasm = await loadWasm();
      // CONSUME the stash so the parked seed cannot be replayed.
      setPendingFreshPhrase(null);
      await finishCanonicalSession({
        wasm,
        accountIdentitySeed: pending.seed,
        accountIdBytes: pending.accountIdBytes,
        accountIdB64: pending.accountIdB64,
        // ACCESS-only convention (no password ⇒ no OPAQUE export key).
        exportKey: new Uint8Array(0),
        sessionToken: pending.sessionToken,
        sessionExpiresAtMs: pending.sessionExpiresAtMs,
        username: pending.username,
      });
    },
    async recover(mnemonicPhrase, newPassword, username) {
      // Resolve the account_id the same way `login` does so recover works on a
      // FRESH device: a local cache hit wins; otherwise a discovery.lookup on
      // the supplied username binds it (extracted to `resolveRecoverAccount`
      // so the cross-device behaviour is unit-tested — G-29).
      const { accountIdB64, resolvedUsername } = await resolveRecoverAccount({
        cached: loadPersisted(),
        username,
        lookup: (u) => discovery.lookup(u),
        encodeAccountId: base64Encode,
      });
      // Parse the phrase up front so a bad mnemonic surfaces
      // before we burn a PoW solve. `fromPhrase` collapses
      // unknown-word and bad-checksum into one error message —
      // by design (preserves BIP39's no-probing property).
      const wasm = await loadWasm();
      const mnemonic = wasm.RecoveryMnemonic.fromPhrase(mnemonicPhrase);
      const seed = mnemonic.toSeed();
      // ADR-0017: the canonical AccountIdentity is derived from this
      // BIP39 phrase seed. `seed` itself is zeroed in the `finally`
      // (it doubles as the OPAQUE recovery input), so keep a private
      // copy for the identity cache + in-memory secrets, which must
      // outlive this call.
      const accountIdentitySeed = seed.slice();
      mnemonic.free();

      // G-4 (ADR-0016 / ADR-0017) — capture the OLD AccountIdentity
      // signature pubkey BEFORE the recover rotates anything, so the
      // emit path below can compare old-vs-new and SKIP the
      // supersession proof when they are equal (the ADR-0017 routine
      // recover case — the phrase is unchanged, so the new account
      // pubkey equals the old one). The OLD identity is itself
      // phrase-seeded now, so the old pubkey is exactly
      // `fromSeed(accountIdentitySeed).signaturePublicKey` — which is
      // what the new identity derives too. We snapshot it here for an
      // explicit equality assertion rather than assuming it.
      const oldIdentity = wasm.Identity.fromSeed(accountIdentitySeed);
      const oldAccountPubkey = oldIdentity.signaturePublicKey;
      oldIdentity.free();

      try {
        const accountIdBytes = base64Decode(accountIdB64);
        const result = await client.recover(
          accountIdBytes,
          seed,
          utf8(newPassword),
        );
        // Phase 1.5b: install the fresh bearer token on the
        // DiscoveryClient.
        discovery.setSessionToken(result.sessionToken);
        // Re-derive the device root key from the NEW export key.
        // The previous device key (if any) is no longer reachable
        // from the new password; the upsert in IDB drops it.
        const deviceKey = await deriveDeviceRootKey(
          result.exportKey,
          accountIdB64,
        );
        await putDeviceKey(accountIdB64, deviceKey);
        // Persist the localStorage account marker — important on the
        // fresh-device recover path where there was no cached row.
        savePersisted({ v: 1, accountIdB64, username: resolvedUsername });
        // Phase 1.13c: persist the new session token under the
        // new device key — the old token (if any) was wrapped
        // under the prior device key and now sits in IDB as
        // undecryptable bytes; the next decrypt attempt drops it.
        try {
          await putSessionToken(
            accountIdB64,
            result.sessionToken,
            result.sessionExpiresAtMs,
            deviceKey,
          );
        } catch (err) {
          // eslint-disable-next-line no-console
          console.warn("session-token persist (recover) failed", err);
        }
        // Phase 2a2c2 — refresh the cached export key under the
        // freshly-rotated device key. Any prior cached value was
        // wrapped under the OLD device key and is now
        // undecryptable; the upsert here overwrites it cleanly.
        try {
          await putExportKey(accountIdB64, result.exportKey, deviceKey);
        } catch (err) {
          // eslint-disable-next-line no-console
          console.warn("export-key persist (recover) failed", err);
        }
        // ADR-0017: cache the AccountIdentity (phrase) seed under
        // the freshly-rotated device key. This is what makes the
        // warm same-device path (and a later passkey resume) rebuild
        // the SAME canonical identity without re-entering the phrase.
        try {
          await putAccountIdentitySeed(
            accountIdB64,
            accountIdentitySeed,
            deviceKey,
          );
        } catch (err) {
          // eslint-disable-next-line no-console
          console.warn(
            "account-identity-seed persist (recover) failed",
            err,
          );
        }
        // Phase 2a3b0 (ADR-0007) — DeviceIdentity seed is
        // decoupled from the OPAQUE password (ADR-0007 §
        // "Lifecycle"), so recovery does NOT rotate it. But
        // any prior cached seed was wrapped under the OLD
        // device key — `ensureDeviceIdentitySeed` falls
        // through to generate-fresh on decrypt failure, which
        // gives us a clean new local DeviceIdentity. The prior
        // leaf in the user's DeviceGroup becomes orphaned and
        // gets cleaned up by 2a3c's remove-device flow on the
        // user's other devices (or by an eventual
        // server-driven prune).
        try {
          await ensureDeviceIdentitySeed(accountIdB64, deviceKey);
        } catch (err) {
          // eslint-disable-next-line no-console
          console.warn("device-identity ensure (recover) failed", err);
        }

        // 1.16e6c1: post-recovery the user's password (and so
        // the export key) has changed, which means any
        // previously-persisted MLS snapshot was wrapped under
        // a device key derived from the OLD password and is
        // now unreadable. Drop it; the user is starting MLS
        // state fresh on this device. Active groups will
        // re-bootstrap via the discovery directory's KeyPackage
        // upserts on the peer's next refresh.
        try {
          await deleteMlsSnapshot(accountIdB64);
        } catch (err) {
          // eslint-disable-next-line no-console
          console.warn("deleteMlsSnapshot during recover failed", err);
        }

        // ADR-0017: build the new canonical Identity from the
        // (UNCHANGED) BIP39 phrase seed — NOT the rotated export
        // key. The recovered identity is byte-for-byte the same as
        // before the recover, which is the whole point: contacts
        // never re-verify. ChatProvider free()s the prior
        // (pre-recovery) Identity if anything was lingering.
        const canonical = wasm.Identity.fromSeed(accountIdentitySeed);
        chats.setIdentity(canonical);

        // 1.16e6c3: rehydrate path is a guaranteed no-op here —
        // we just deleted the snapshot row above — but routing
        // through the same helper keeps the auth flows uniform
        // and resilient to future re-orderings.
        await rehydrateMlsState(chats, accountIdB64);

        // Re-publish the identity bundle + a fresh KeyPackage.
        //
        // G-4 (ADR-0016) emit path — now DORMANT per ADR-0017. With
        // a phrase-seeded AccountIdentity, a routine recover does NOT
        // rotate the account key: the phrase is unchanged, so the new
        // account signature pubkey EQUALS the old one. We compare
        // them explicitly and, when equal (the only case on a routine
        // recover), SKIP the supersession proof mint + upload
        // entirely — emitting a proof for a key nobody's pin changed
        // would be spurious. The "old == new account pubkey ⇒ skip"
        // rule is the load-bearing ADR-0017 emit invariant.
        //
        // The bundle is still re-published WITHOUT a proof: the
        // public keys are identical, but the fresh KeyPackage's init
        // key is new and its private bytes live in the canonical
        // provider, so the upsert keeps the directory's KeyPackage
        // current. A plain (proof-less) upload also clears any stale
        // server-side proof column. We deliberately do NOT mint a
        // proof even if the keys somehow differed here — a genuine
        // identity rotation is a separate explicit action (ADR-0017
        // "rotate my identity"), out of scope for Part 1.
        const newAccountPubkey = canonical.signaturePublicKey;
        const accountKeyUnchanged = bytesEqual(
          oldAccountPubkey,
          newAccountPubkey,
        );
        if (!accountKeyUnchanged) {
          // Defence-in-depth: on a phrase-seeded recover this can
          // never happen (same seed ⇒ same key). If it ever does, it
          // means the phrase that authenticated the OPAQUE recover
          // differs from the one seeding the identity — a logic bug.
          // Fail loud rather than silently publish a changed key with
          // no proof (which would break every contact's pin).
          throw new Error(
            "internal: recover produced a different AccountIdentity " +
              "pubkey than expected (ADR-0017 invariant: same phrase ⇒ " +
              "same identity)",
          );
        }
        try {
          // Proof-less re-publish (account key unchanged). Refreshes
          // the KeyPackage + clears any stale server proof column.
          await uploadIdentityBundle(discovery, accountIdBytes, canonical);
        } catch (err) {
          // eslint-disable-next-line no-console
          console.warn("post-recover identity bundle upload failed", err);
        }
        try {
          await chats.persistSnapshot(accountIdB64);
        } catch (err) {
          // eslint-disable-next-line no-console
          console.warn("post-recover MLS snapshot failed", err);
        }

        // 1.16d3c: stash the new export key so ContactsClient
        // works without a re-login round-trip.
        // ADR-0017: also carry the phrase seed (the canonical
        // identity seed). Cleared on logout.
        // Phase 1.5b: also stash the bearer token.
        setIdentitySecrets({
          accountId: accountIdBytes,
          exportKey: result.exportKey,
          accountIdentitySeed,
          sessionToken: result.sessionToken,
          sessionExpiresAtMs: result.sessionExpiresAtMs,
        });
        // Clear any pending fresh-device fail-safe — recover just
        // restored the identity directly.
        setPendingRecovery(null);
        setState({
          kind: "session",
          account: {
            accountId: accountIdB64,
            username: resolvedUsername,
          },
        });
        // Phase 2a1d: post-recovery the prior accepted-contacts
        // list only lives inside the encrypted backup envelope
        // the previous device uploaded. Fetch + decrypt + apply
        // BEFORE the bootstrap re-uploads, so the new baseline
        // carries the merged set instead of overwriting the
        // server blob with an empty list.
        //
        // Phase 2a1c: post-recovery backup bootstrap. Same
        // shape as the signup-side call above — re-caches the
        // phrase under the freshly-rotated device root key and
        // posts a baseline backup envelope. Best-effort end-to-end;
        // each step logs and continues on failure.
        //
        // G-10 (ADR-0015 area / known-gaps): this is AWAITED, not
        // fire-and-forget. The baseline re-seals the backup envelope
        // under the freshly-rotated export-key material; until it
        // lands, the server still holds the pre-recover envelope.
        // Awaiting folds the rewrite into the recover round-trip so
        // the "stale envelope" window collapses to the upload latency
        // instead of "whenever the next baseline happens to fire".
        // The bootstrap is wrapped best-effort so a backup hiccup
        // still does not fail the recover itself.
        await (async () => {
          // Phase 2a3d4 — capture the restored MLS snapshot (if
          // any) so the post-restore bootstrap re-uploads the
          // same state instead of nuking the server-side
          // snapshot with an empty one.
          let restoredSnapshot: Uint8Array = new Uint8Array(0);
          try {
            const restored = await restoreFromServerBackupFromPhraseString(
              { sessionToken: result.sessionToken },
              mnemonicPhrase,
            );
            if (restored.kind === "restored") {
              if (restored.wholeStorageSnapshot.length > 0) {
                // Apply to the freshly-derived canonical Identity
                // so the new device lands with the same MLS group
                // state the upload-side device had. The state
                // mutates the provider's storage in place;
                // `chats.persistSnapshot` below picks up the
                // updated bytes via `id.snapshotStorage()`.
                try {
                  canonical.restoreStorage(
                    restored.wholeStorageSnapshot,
                  );
                  restoredSnapshot = restored.wholeStorageSnapshot;
                  // Re-persist the IDB snapshot so the on-device
                  // copy carries the restored state instead of
                  // the just-bootstrapped empty one.
                  try {
                    await chats.persistSnapshot(accountIdB64);
                  } catch (err) {
                    // eslint-disable-next-line no-console
                    console.warn(
                      "post-restore MLS persistSnapshot failed",
                      err,
                    );
                  }
                } catch (err) {
                  // eslint-disable-next-line no-console
                  console.warn(
                    "post-restore Identity.restoreStorage threw",
                    err,
                  );
                }
              }
            } else if (restored.kind !== "no-backup") {
              // eslint-disable-next-line no-console
              console.warn(
                "restoreFromServerBackup outcome (recover):",
                restored.kind,
              );
            }
          } catch (err) {
            // eslint-disable-next-line no-console
            console.warn("restoreFromServerBackup threw (recover)", err);
          }
          try {
            await bootstrapBackupFromPhraseString(
              {
                accountId: accountIdBytes,
                accountIdB64,
                ed25519Seed: canonical.ed25519Seed,
                deviceKey,
                sessionToken: result.sessionToken,
                wholeStorageSnapshot: restoredSnapshot,
              },
              mnemonicPhrase,
            );
          } catch (err) {
            // eslint-disable-next-line no-console
            console.warn("post-recover backup bootstrap failed", err);
          }
        })();
        // 1.20b: best-effort push subscribe after recovery —
        // the new password changes the device key, but the
        // device push pseudonym persists in localStorage so
        // the server's existing token row simply gets re-keyed.
        void subscribeToPush().catch((err) => {
          // eslint-disable-next-line no-console
          console.warn("push subscribe (recover) failed", err);
        });
      } finally {
        // Best-effort wipe of the seed buffer. It's a Uint8Array
        // we own; zeroing it now means the next GC pass cannot
        // hand it to a leaked closure.
        seed.fill(0);
      }
    },
    async recoverWithTotp(username, code) {
      // ADR-0018 factor (c) — authenticator-code recovery. ACCESS
      // only: the server verifies the TOTP code and issues the
      // session triple; identity acquisition runs the ADR-0017
      // state machine below.
      const trimmed = username.trim();
      if (trimmed.length === 0) {
        throw new Error(
          "enter the username of the account to recover",
        );
      }
      // Resolve the username → account_id exactly like `login` /
      // `recover`: a local cache hit wins, else a discovery.lookup
      // round-trip binds it.
      const cached = loadPersisted();
      let accountIdBytes: Uint8Array;
      let resolvedUsername: string;
      if (cached !== null && cached.username === trimmed) {
        accountIdBytes = base64Decode(cached.accountIdB64);
        resolvedUsername = cached.username;
      } else {
        const lookup = await discovery.lookup(trimmed);
        if (lookup.account === null) {
          throw new Error("username not found");
        }
        accountIdBytes = lookup.account;
        resolvedUsername = lookup.normalisedUsername;
      }

      const result = await client.totpRecover(accountIdBytes, code);
      const accountIdB64 = base64Encode(accountIdBytes);
      // Shared ADR-0018 ACCESS-only landing (warm cached-seed finish
      // OR cold fail-safe park) — identical to the cascade's other
      // ACCESS factor (factor (d) phone/email OTP).
      await landAfterAccessRecovery(resolvedUsername, accountIdBytes, accountIdB64, {
        sessionToken: result.sessionToken,
        sessionExpiresAtMs: result.sessionExpiresAtMs,
      });
    },
    async recoverWithContactStart(username, kind, contact) {
      // ADR-0018 factor (d), step 1 — send a one-time code to a
      // previously-enrolled phone / email. ACCESS only: the finish
      // step verifies the code and issues the session triple; identity
      // acquisition runs the ADR-0017 state machine afterward.
      const trimmed = username.trim();
      if (trimmed.length === 0) {
        throw new Error("enter the username of the account to recover");
      }
      // Resolve the username → account_id exactly like `login` /
      // `recover` / `recoverWithTotp`: a local cache hit wins, else a
      // discovery.lookup round-trip binds it.
      const cached = loadPersisted();
      let accountIdBytes: Uint8Array;
      let resolvedUsername: string;
      if (cached !== null && cached.username === trimmed) {
        accountIdBytes = base64Decode(cached.accountIdB64);
        resolvedUsername = cached.username;
      } else {
        const lookup = await discovery.lookup(trimmed);
        if (lookup.account === null) {
          throw new Error("username not found");
        }
        accountIdBytes = lookup.account;
        resolvedUsername = lookup.normalisedUsername;
      }

      // Send the code. ENUMERATION-SAFE server-side: the response is
      // the same `{ sent: true }` whether or not the contact matches a
      // confirmed row, so a non-throwing return here only means
      // "advance to the code step".
      await client.recoveryContactRecoverStart(accountIdBytes, kind, contact);

      // Stash the resolved context so `…Finish` can submit the code
      // without re-resolving. The kind is carried verbatim so the
      // finish targets the exact (account, kind) the code was sent for.
      setPendingContactRecover({
        accountIdBytes,
        accountIdB64: base64Encode(accountIdBytes),
        username: resolvedUsername,
        kind,
      });
    },
    async recoverWithContactFinish(code) {
      // ADR-0018 factor (d), step 2 — submit the delivered code.
      const pending = pendingContactRecover();
      if (pending === null) {
        throw new Error(
          "request a code first (no pending phone / email recovery)",
        );
      }
      const result = await client.recoveryContactRecoverFinish(
        pending.accountIdBytes,
        pending.kind,
        code,
      );
      // Consume the stash — the code is single-use server-side; a
      // retry must re-send.
      setPendingContactRecover(null);
      // Shared ADR-0018 ACCESS-only landing — identical to the
      // cascade's other ACCESS factor (factor (c) TOTP).
      await landAfterAccessRecovery(
        pending.username,
        pending.accountIdBytes,
        pending.accountIdB64,
        {
          sessionToken: result.sessionToken,
          sessionExpiresAtMs: result.sessionExpiresAtMs,
        },
      );
    },
    logout() {
      // Keep localStorage + IDB device key intact. Re-login on
      // this device remains cheap, and the device key stays
      // available for any encrypted local stores 1.16f wires up.
      // Drop the in-memory Identity secrets — the user just
      // signed out.
      const cached = loadPersisted();
      setIdentitySecrets(null);
      // ADR-0017: drop any parked recovery / fresh-phrase context so a
      // stashed seed cannot linger past a sign-out. Zeroize the
      // fresh-phrase seed before dropping the reference.
      setPendingRecovery(null);
      const fresh = pendingFreshPhrase();
      if (fresh !== null) {
        fresh.seed.fill(0);
        setPendingFreshPhrase(null);
      }
      // Phase 1.5b: drop the bearer token so a subsequent
      // protected write surfaces a clean 401 instead of silently
      // using a stale token.
      discovery.setSessionToken(null);
      // Phase 1.13c: ALSO drop the IDB-persisted token. Keeping
      // it would defeat the user's "I just logged out" intent on
      // the next page reload (we'd silently re-attach to a live
      // session-token even with no in-memory state). The device
      // key stays in IDB so the next login is still cheap; only
      // the per-session bearer dies. Fire-and-forget — the API
      // contract is sync and the deletion is best-effort.
      if (cached !== null) {
        void deleteSessionToken(cached.accountIdB64).catch(() => {
          /* best-effort */
        });
      }
      // 1.20b: drop the push registration too. Logout intent is
      // "no more pushes on this device until I re-auth" — the
      // device key + push pseudonym stay on this browser for a
      // cheap re-login, but the server-side token row goes.
      // Fire-and-forget; failures are non-blocking.
      void unsubscribeFromPush().catch((err) => {
        // eslint-disable-next-line no-console
        console.warn("push unsubscribe (logout) failed", err);
      });
      chats.clear();
      setIdentityAlert(null);
      setState({
        kind: "anon",
        ...(cached !== null ? { cachedUsername: cached.username } : {}),
      });
    },
    async forgetDevice() {
      const cached = loadPersisted();
      // Order matters: clear the IDB key BEFORE clearing the
      // localStorage marker. If we crash between, an IDB key
      // without a localStorage marker is harmless on next mount;
      // a localStorage marker without an IDB key would loop
      // through the broken-restore path forever.
      if (cached !== null) {
        try {
          await deleteDeviceKey(cached.accountIdB64);
        } catch {
          // Ignore — best-effort wipe; localStorage clear below
          // is still worth running.
        }
        // 1.16e6c1: drop the encrypted MLS snapshot for this
        // account. After `deleteDeviceKey` above the snapshot
        // is unreadable anyway (the wrapping AES-256-GCM key is
        // gone), but leaving the row behind means a later
        // signup on the same accountId would surface stale
        // ciphertext on hydration — clean wipe is the
        // contract `forgetDevice` advertises.
        try {
          await deleteMlsSnapshot(cached.accountIdB64);
        } catch {
          // Ignore — same best-effort policy as above.
        }
        // Phase 2a3f1-web — drop the plaintext message log
        // too. Same wipe-on-forgetDevice rationale as the MLS
        // snapshot: the encryption key (device root key) just
        // got wiped, so the ciphertext is already unreadable;
        // a stale IDB row would confuse a future signup on
        // the same account_id.
        try {
          await deleteMessageLog(cached.accountIdB64);
        } catch {
          // best-effort
        }
        // Phase 1.13c: drop the persisted session token too. The
        // ciphertext is unreadable without the device key just
        // wiped, but a stale row would confuse the rehydrate
        // probe on a future signup with the same accountId.
        try {
          await deleteSessionToken(cached.accountIdB64);
        } catch {
          // best-effort
        }
        // Phase 2a1c: drop the cached recovery phrase. Wrapped
        // under the same device-root key that's now gone, so
        // the ciphertext is already undecryptable — but a stale
        // row would confuse a future bootstrap on the same
        // accountId. Clean wipe matches `forgetDevice`'s
        // "leave nothing per-account behind" contract.
        try {
          await deletePhrase(cached.accountIdB64);
        } catch {
          // best-effort
        }
        // Phase 2a2c2: drop the cached export key. Same posture
        // as the snapshot / session-token rows above — leaving
        // ciphertext behind is safe (the wrap key is gone) but a
        // future signup on the same accountId would trip over
        // the stale row.
        try {
          await deleteExportKey(cached.accountIdB64);
        } catch {
          // best-effort
        }
        // Phase 2a3b0: drop the cached DeviceIdentity seed.
        // The leaf in the user's DeviceGroup that this seed
        // signed for is orphaned by this wipe — other devices
        // will see it as unreachable until 2a3c's
        // remove-device flow runs from a sibling device.
        try {
          await deleteDeviceIdentitySeed(cached.accountIdB64);
        } catch {
          // best-effort
        }
        // ADR-0017: drop the cached AccountIdentity (phrase) seed.
        // It is the highest-sensitivity on-device secret; the
        // ciphertext is unreadable once the device key is gone, but
        // a stale row would confuse a future signup on the same
        // accountId. After this wipe the only way back into the
        // account is the recovery phrase (or a paired device).
        try {
          await deleteAccountIdentitySeed(cached.accountIdB64);
        } catch {
          // best-effort
        }
        // Phase 2a3b2-bootstrap: drop the cached DeviceGroup
        // MLS snapshot. Ciphertext is unreadable without the
        // device key just wiped, but a stale row would
        // confuse a future signup on the same accountId.
        try {
          await deleteDeviceGroupSnapshot(cached.accountIdB64);
        } catch {
          // best-effort
        }
      }
      clearPersisted();
      // 1.16d3d: drop the on-device contacts cache too —
      // "forget device" should leave nothing per-account
      // behind on this browser.
      clearContactsStore();
      // 1.20b: full wipe of push state too — unsubscribe on the
      // browser side, drop the push pseudonym from localStorage,
      // tell `push-gw` to drop the token. Awaited because
      // forgetDevice's contract is "leave NOTHING per-account
      // behind"; a fire-and-forget here would race against the
      // immediate state flip below.
      try {
        await unsubscribeFromPush();
      } catch {
        // best-effort — the localStorage pseudonym is dropped
        // regardless inside `unsubscribeFromPush`.
      }
      // 1.16e5: drop active MLS group state.
      // 1.16e6c1: also free()s the canonical Identity.
      chats.clear();
      setIdentitySecrets(null);
      // ADR-0017: drop any parked recovery / fresh-phrase context.
      // Zeroize the fresh-phrase seed before dropping the reference.
      setPendingRecovery(null);
      const fresh = pendingFreshPhrase();
      if (fresh !== null) {
        fresh.seed.fill(0);
        setPendingFreshPhrase(null);
      }
      // Phase 1.5b: drop the bearer token.
      discovery.setSessionToken(null);
      setState({ kind: "anon" });
    },
  };

  return (
    <AuthContext.Provider value={api}>{props.children}</AuthContext.Provider>
  );
}

/**
 * Inspect localStorage + IndexedDB and resolve the appropriate
 * initial `AuthState`:
 *
 *   * No localStorage record → fresh device, `anon`.
 *   * localStorage but no IDB key → cached identity, password
 *     re-entry required, `anon` with `cachedUsername`.
 *   * Both present → resume `session` directly, no password.
 */
/**
 * Probes localStorage + IDB to recover the most-complete state
 * we can without prompting for a password:
 *
 *   - `(accountId, username)` cache → drives the auth state branch.
 *   - Device key in IDB → required for any AES-GCM-wrapped material.
 *   - Persisted session token in IDB → returned alongside the state
 *     so the caller can install it on the DiscoveryClient. Phase
 *     1.13c added this; pre-1.13c builds drop straight to
 *     `sessionToken: null`.
 */
async function hydrateFromStorage(): Promise<{
  state: AuthState;
  sessionToken: string | null;
}> {
  const cached = loadPersisted();
  if (cached === null) {
    return { state: { kind: "anon" }, sessionToken: null };
  }
  let key: CryptoKey | null = null;
  try {
    key = await getDeviceKey(cached.accountIdB64);
  } catch {
    // IDB unreachable (private mode in some browsers, quota
    // exhaustion, …). Treat as "no key cached" and ask for the
    // password again rather than crash the app.
    key = null;
  }
  if (key === null) {
    return {
      state: { kind: "anon", cachedUsername: cached.username },
      sessionToken: null,
    };
  }

  // Phase 1.13c: try to recover the persisted session token. The
  // store handles its own expiry probe (cheap unencrypted column
  // + post-decrypt belt-and-suspenders) so we just consume the
  // result. A decrypt failure (device key rotated, ciphertext
  // tampered) surfaces as null — same UX as a missing row.
  let sessionToken: string | null = null;
  try {
    const persisted = await getSessionToken(cached.accountIdB64, key);
    sessionToken = persisted?.sessionToken ?? null;
  } catch {
    sessionToken = null;
  }

  return {
    state: {
      kind: "session",
      account: { accountId: cached.accountIdB64, username: cached.username },
    },
    sessionToken,
  };
}

/**
 * Publish everything the directory needs to know about this
 * account: the identity bundle (Ed25519 + X25519 pubkeys,
 * deterministic from the export key — 1.16d3b2) and the MLS
 * KeyPackage (random per call — 1.16e2). Both upserts run in
 * parallel; the calls are independent and the server is
 * idempotent on either side.
 *
 * **Pre-1.16e6c1 footgun fixed.** This used to spin up a
 * throwaway `Identity.fromSeed(exportKey)` and drop it after
 * the upload. That worked for the public-key fields (Ed25519
 * + X25519 are deterministic, so re-derivation reproduces them
 * byte-for-byte), but `createKeyPackage` mints **new private
 * key material per call** and writes it into the provider's
 * storage. Discarding that provider meant any peer who later
 * sent a Welcome targeting our published KeyPackage would hit
 * "init key not in keystore" on our side. We now require the
 * caller to pass the **canonical** Identity that ChatProvider
 * is holding — so the freshly-generated init / leaf private
 * keys land in the provider we'll snapshot at persist time.
 *
 * G-4 (ADR-0016): `supersessionProof`, when present, is the
 * old-key-signed proof bridging an AccountIdentity rotation
 * (recover). It threads straight into the discovery
 * `uploadIdentityBundle` call so the new keys + proof ship in the
 * same request (ADR-0016 rule #1, atomic emit). The signup /
 * login / passkey callers pass nothing — a normal upload clears
 * any stale server-side proof.
 *
 * G-30 #2 (ADR-0019): `leafAuth`, when present, is the genesis leaf
 * self-signature (from `signLeafGenesis`) authorising a NEW
 * `(account_key -> pubkey)` transparency-log binding. Only the two
 * publish sites that introduce a new key supply it — the first-ever
 * signup and the lost-everything `mintFreshIdentity` fresh-start. The
 * routine same-key paths (login / passkey / recover) omit it: the log
 * append is idempotent there, so no authorisation is needed.
 */
async function uploadIdentityBundle(
  discovery: DiscoveryClient,
  accountId: Uint8Array,
  identity: WasmIdentity,
  supersessionProof?: Uint8Array | string,
  leafAuth?: Uint8Array | string,
): Promise<void> {
  // 1.16e2: generate a fresh KeyPackage. RFC 9420 forward
  // secrecy on new groups argues for one-shot init_keys; in
  // practice we publish a freshly-randomised one on every
  // login, and the Phase-1 server just upserts.
  const keyPackageBytes = identity.createKeyPackage();

  // Run both uploads in parallel. They're independent — the
  // identity bundle and the KeyPackage live in different
  // tables, neither depends on the other's success. If one
  // fails the caller gets the failure (Promise.all rejects on
  // first); we deliberately do not swallow either error
  // silently because both are required for a usable account.
  await Promise.all([
    discovery.uploadIdentityBundle(
      accountId,
      identity.signaturePublicKey,
      identity.x25519PublicKey,
      supersessionProof,
      leafAuth,
    ),
    discovery.uploadKeyPackage(accountId, keyPackageBytes),
  ]);
}

/**
 * G-30 #2 (ADR-0019): mint the genesis leaf self-signature
 * authorising a NEW `(account_key -> pubkey)` transparency-log
 * binding.
 *
 * The transparency append now REFUSES a new binding that carries
 * neither a `leaf_auth` (a fresh-start self-signature by the leaf's
 * own identity key) nor a rotation supersession proof. The two web
 * publish sites that introduce a new key — the first-ever signup and
 * the lost-everything `mintFreshIdentity` fresh-start — call this and
 * attach the result.
 *
 * The signature binds `account_key -> identity_pubkey` and is
 * version-INDEPENDENT (the operator-assigned log version is unknown
 * to a fresh-start minter), so it is signed over the BLAKE3
 * `account_key` only — the same key the S3/S4 transparency code
 * already derives.
 *
 * FAIL-LOUD: any failure here (seed unavailable, account-key
 * derivation, signing) rejects so the caller surfaces a clear local
 * error rather than publishing a bundle the server will refuse — a
 * publish that cannot succeed is worse than a visible failure.
 */
async function mintLeafGenesisAuth(
  identity: WasmIdentity,
  accountId: Uint8Array,
): Promise<Uint8Array> {
  const wasm = await loadWasm();
  const usernameHash = await transparencyAccountKey(accountId);
  return Uint8Array.from(
    wasm.signLeafGenesis(identity.ed25519Seed, usernameHash),
  );
}

/**
 * FAIL-CLOSED AccountIdentity-seed verification (ADR-0017 +
 * lakko-security finding #1, shared by `provideRecoveryPhrase` and
 * the ADR-0018 factor (a) pairing completions).
 *
 * The supplied seed MUST reproduce the account's PUBLISHED
 * AccountIdentity signature pubkey. These paths only run for an
 * ALREADY-ESTABLISHED account, so the bundle must exist — a missing
 * bundle (404) or an unreachable directory is an anomaly, NOT a
 * first-publish. We therefore HARD-BLOCK on any failure-to-verify
 * (mismatch, 404, or network) rather than trust-on-first-use a
 * possibly-wrong seed: a server-forced 404 must not let a
 * socially-engineered wrong seed overwrite the published identity.
 *
 * On EVERY refusal path the seed is zeroized before the throw.
 * Throws `SupersessionPhraseMismatch` (pubkey mismatch) or
 * `IdentityVerificationUnavailable` (bundle missing / unreachable).
 */
async function verifySeedAgainstPublishedIdentity(args: {
  readonly wasm: Wasm;
  readonly discovery: DiscoveryClient;
  readonly accountIdBytes: Uint8Array;
  readonly accountIdentitySeed: Uint8Array;
  /** Override for the mismatch error text — the pairing path says
   *  "received from your other device" instead of "phrase". */
  readonly mismatchMessage?: string;
}): Promise<void> {
  const derivedIdentity = args.wasm.Identity.fromSeed(
    args.accountIdentitySeed,
  );
  const derivedPubkey = derivedIdentity.signaturePublicKey;
  derivedIdentity.free();

  try {
    const bundle = await args.discovery.identityFetch(args.accountIdBytes);
    if (bundle === null) {
      args.accountIdentitySeed.fill(0);
      throw new IdentityVerificationUnavailable(
        "No published identity was found to verify the recovered " +
          "identity. Try again, or restore from a device that still " +
          "has your account.",
      );
    }
    const published = base64Decode(bundle.signature_pubkey);
    if (!bytesEqual(published, derivedPubkey)) {
      args.accountIdentitySeed.fill(0);
      throw new SupersessionPhraseMismatch(args.mismatchMessage);
    }
  } catch (err) {
    if (
      err instanceof SupersessionPhraseMismatch ||
      err instanceof IdentityVerificationUnavailable
    ) {
      throw err;
    }
    // Could not reach the directory to verify (network/parse). We
    // cannot confirm the seed reproduces the published key, so we
    // refuse rather than cache a possibly-wrong identity.
    args.accountIdentitySeed.fill(0);
    // eslint-disable-next-line no-console
    console.warn("identity-seed verification fetch failed", err);
    throw new IdentityVerificationUnavailable(
      "Could not reach the directory to verify the recovered identity. " +
        "Check your connection and try again.",
    );
  }
}

/**
 * Constant-time-ish byte equality for two `Uint8Array`s. Used by
 * the recover flow's ADR-0017 emit gate to assert the old and new
 * AccountIdentity pubkeys are equal (so the supersession proof is
 * skipped). Length-first, then per-byte; we OR the diffs so the
 * loop does not early-exit on the first mismatch.
 */
function bytesEqual(a: Uint8Array, b: Uint8Array): boolean {
  if (a.length !== b.length) return false;
  let diff = 0;
  for (let i = 0; i < a.length; i++) {
    diff |= (a[i] ?? 0) ^ (b[i] ?? 0);
  }
  return diff === 0;
}

// ADR-0017: the G-4 / ADR-0016 supersession EMIT helpers
// (`captureOldExportKey`, `mintSupersessionProof`,
// `SUPERSESSION_EMIT_WINDOW_SECS`) were removed here. With a
// phrase-seeded AccountIdentity a routine recover does not rotate
// the account key, so no proof is ever minted on this path (the
// recover emit gate above compares old-vs-new and skips). The
// proof primitive itself (`wasm.signSupersessionProof` /
// `verifySupersession`) is retained in `lakko-transparency` as the
// substrate for a future explicit "rotate my identity" action.

/**
 * Hook — every component that needs to read or drive the session
 * calls this. Throws if invoked outside `<AuthProvider>` because
 * that is a wiring bug, not a runtime condition we want to paper
 * over.
 */
export function useAuth(): AuthApi {
  const ctx = useContext(AuthContext);
  if (ctx === undefined) {
    throw new Error("useAuth() called outside <AuthProvider>");
  }
  return ctx;
}
