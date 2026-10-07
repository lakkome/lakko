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
 * On `AuthProvider` mount we look both up, together with the
 * session pair and the AccountIdentity seed wrapped under the
 * device key, and restore the full session when every piece is
 * present — no password re-entry required — or land in `anon`
 * otherwise (`silent-restore.ts`).
 *
 * ## The session pair and the key generation (ADR-0065)
 *
 * Every landing installs a PAIR (`session-pair.ts`): a fifteen-minute
 * access token and a thirty-day refresh credential, issued under the
 * account's key generation. The one token provider
 * (`session-token-provider.ts`, reached by every bearer client through
 * `sessionFetch`) refreshes ahead of expiry and retries once on a 401; a
 * `401 key-generation-superseded` from anywhere means the account's keys
 * were reset on another device, and this device shows the signed-out
 * screen and wipes itself (`signOutAfterKeyReset`). Every landing reads the
 * published identity before it uploads anything (`landing-rule.ts`): the
 * old "heal" upload is gone. "Reset account keys" (`mintFreshIdentity`) is
 * the one path to a new identity: the server reset first, then the
 * attested publish, with a "publish owed" record (`key-reset-owed.ts`)
 * carrying the retry.
 *
 * `logout()` keeps both layers in place — re-auth via password
 * stays cheap. `forgetDevice()` wipes both — full reset, requires
 * the recovery passphrase to come back.
 *
 * ## What is wired and what is not
 *
 *   * Recover-from-mnemonic — DONE. `recover()` drives the
 *     `/recover` route + `AuthClient.recover`, addressed by name
 *     like `login()` — so recover works on a FRESH device given the
 *     username + phrase (the optional `username` arg is required only
 *     when no named account is cached).
 *   * Sign in and recover by name (ADR-0054 S5) — DONE. `login()` and
 *     every ADR-0018 recovery factor send the login handle of the typed
 *     name (`login-handle.ts`) in place of an account id; nothing is
 *     looked up before a session, and the account id comes back in the
 *     finish. A signup claims the name handle-first (auth, then
 *     discovery) and every sign-in by name re-binds the handle.
 *
 * Still not here:
 *
 *   * Multi-account on a single device.
 */
import {
  createContext,
  createEffect,
  createSignal,
  onCleanup,
  onMount,
  type Accessor,
  type JSX,
  useContext,
} from "solid-js";

import { AuthClient, AuthHttpError, AuthNetworkError } from "./client";
import { loadPersistedAccount, savePersistedAccount } from "./persisted-account";
import { wipeAccountLocalData } from "./wipe-account";
import { base64Decode, base64Encode, base64UrlDecodeNoPad } from "./account-id";
import { resolveRecoverUsername } from "./recover-resolution";
import {
  accessByName,
  claimUsername,
  loginHandleFor,
  nameTargetFor,
} from "./login-handle";
import { createLoginHandleSync } from "./login-handle-sync";
import { mergeSyncStateFromBackup } from "../devices/sync/sync-state-idb";
import { mergeRestoredFold } from "../devices/sync/synced-projection";
import { announceRestoredSyncedState } from "../devices/sync/synced-state-source";
import { decideSilentRestore } from "./silent-restore";
import { sessionTokens } from "./session-token-provider";
import { sessionPairOf, type SessionPair } from "./session-pair";
import { errorCodeOf } from "./session-401";
import {
  createFreshGrantHolder,
  FreshAuthRequiredError,
  freshGrantOf,
  isFreshAuthRequired,
  passwordChangeAllowed,
  ProofCancelledError,
  proofLoginFailure,
  ProofOtherAccountError,
  ProofPasskeyFailedError,
  ProofWrongPasswordError,
  type SessionLanding,
} from "./fresh-auth";
import {
  landingMayUpload,
  landingSignsOut,
  landingVerdict,
  type LandingVerdict,
} from "./landing-rule";
import { fetchDeviceAttestKey, pinnedDeviceAttestKey } from "./device-attest-key";
import { ensureDeviceGroup } from "../devices/lifecycle";
import { createKeyPackagePublisher } from "../chat/keypackage-publisher";
import {
  deriveDeviceRootKey,
  deriveDeviceRootKeyFromIdentitySeed,
} from "./device-key";
import { getDeviceKey, putDeviceKey } from "./key-store";
import {
  deleteSessionToken,
  getSessionPair,
  putSessionPair,
} from "./session-token-store";
import {
  getExportKey,
  putExportKey,
} from "./export-key-store";
import {
  ensureDeviceIdentitySeed,
  getDeviceBinding,
  putDeviceBinding,
} from "./device-identity-store";
import {
  ACCOUNT_IDENTITY_SEED_LENGTH,
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
import { DiscoveryClient, DiscoveryHttpError } from "../discovery/client";
import { bindContactsAccount } from "../contacts/local-store";
import { useChats } from "../chat/ChatContext";
import { deleteMlsSnapshot, saveMlsSnapshot } from "../chat/mls-store";
import { deleteOutboxForAccount } from "../chat/commit-outbox";
// ADR-0045 — the pending-request observation store follows the outbox's
// hygiene: a prior session's request ages must never be read by a later one.
import { clearObservationsForAccount } from "../chat/pending-requests";
// ADR-0045 S7 residual 1 — the `Not applied` rows follow the same hygiene:
// they name a signature key from a session that is being discarded.
import { clearUnappliedForAccount } from "../chat/unapplied-requests";
import { clearOwnRequestsForAccount } from "../chat/membership-retry";
// ADR-0045 UX §6.1.10 — the per-group last-observed-owner memory the
// new-owner notice compares against. Same recover / forgetDevice hygiene as
// the commit outbox and the two observation stores beside it: a stale entry
// on a re-signup would compare a fresh session against a previous account's
// role table.
import { clearLastObservedOwnerForAccount } from "../chat/ownership-transfer";
import { clearLeaveRequestedForAccount } from "../chat/membership-ended";
// ADR-0045 S4b — the same hygiene for the committer-beacon observation
// store: a stale beacon carried across a recover / forgetDevice would let
// this device defer to a candidate it has no current evidence about.
import { clearBeaconObservationsForAccount } from "../chat/committer-beacon";
import { rehydrateMlsState } from "../chat/rehydrate";
import { subscribeToPush, unsubscribeFromPush } from "../push/subscribe";
import { releaseWakeBinding } from "../push/wake-binding";
import { anonTokens } from "../tokens/anon-token-pool";
import {
  discoveryAccountLookup,
  runNameCheckedDeviceRestore,
} from "./device-restore-name-check";
import { bootstrapBackupFromPhraseString } from "../backup/bootstrap";
import { startAutoBackup } from "../backup/auto-backup-driver";
import {
  readDevicePhrase,
  readDevicePhraseWith,
  storeDevicePhrase,
} from "../backup/device-phrase";
import {
  clearBudget as clearThreeWordsBudget,
  isThreeWordsSuspended,
  markThreeWordsSuspended,
  offersThreeWords,
} from "./recover/three-words";
import {
  recoveryLoginFailureOf,
  ThreeWordsSuspendedError,
  storedPhraseVerdict,
} from "./recover/stale-phrase";
import {
  attestationGate,
  clearKeyResetOwed,
  deviceGroupResetOutcome,
  directoryResetOutcome,
  isKeyResetOwed,
  readKeyResetOwed,
  remainingKeyResetSteps,
  withKeyResetStepDone,
  writeKeyResetOwed,
  keyResetAttestationSignedAtMs,
  type KeyResetOwed,
  type KeyResetStep,
} from "./key-reset-owed";
import { clearRestored, markRestored } from "./restore-marker";
import {
  openPasskeyVault,
  passkeyKeysOutcome,
  PasskeyVaultUnusableError,
  PASSKEY_PRF_SALT,
  type PasskeyKeysOutcome,
} from "../recovery/passkey-vault";
import {
  clearSignupPhraseOwed,
  isSignupPhraseOwed,
  markSignupPhraseOwed,
} from "./phrase-gate";
import { restoreFromServerBackupFromPhraseString } from "../backup/restore";
import type { Identity as WasmIdentity } from "../../wasm/lakko_ffi_wasm";
import { KeyedError } from "../l10n/keyed-error";
import { t } from "../l10n";

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
      /** ADR-0065: the refresh credential was refused as invalid or had
       *  expired, so the session ended on its own; the sign-in screen says
       *  so. Nothing was wiped. */
      readonly sessionExpired?: true;
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
   *  renders the new 12-word phrase for the user to save before the
   *  session lands. `confirmFreshPhraseSaved()` completes the session
   *  from the stashed context. The user's contacts then see a changed
   *  key with no proof and re-verify out-of-band — the accepted,
   *  documented degradation. */
  | {
      readonly kind: "save-fresh-phrase";
      readonly account: SessionAccount;
    };

/** Result handed back to the signup form so it can display the
 *  freshly-generated 12-word recovery passphrase exactly once. */
export interface SignupOutcome {
  /** The 12-word BIP39 recovery phrase. The form must show this
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
  /** Where the refused seed came from: a phrase (typed, or opened from a
   *  passkey's stored keys), or another device. The screen renders its sentence from this (ADR-0061
   *  section 6: `restore_identity.error.phrase_mismatch` or
   *  `device_link.error.identity_mismatch`); `message` is for the logs. */
  readonly source: "phrase" | "device";
  constructor(source: "phrase" | "device" = "phrase") {
    super(
      source === "device"
        ? "The identity received from your other device doesn't match " +
            "this account's published identity — refusing to use it."
        : "That recovery phrase doesn't match this account's identity. " +
            "Check the phrase, or pair an existing device instead.",
    );
    this.name = "SupersessionPhraseMismatch";
    this.source = source;
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
  /** `missing`: no published identity to check against; `unreachable`: the
   *  directory could not be reached. The screen renders its sentence from
   *  this (ADR-0061), `message` stays for the logs. */
  readonly kind: "missing" | "unreachable";
  constructor(kind: "missing" | "unreachable", message: string) {
    super(message);
    this.name = "IdentityVerificationUnavailable";
    this.kind = kind;
  }
}

/** Identity secrets held in memory for the current session.
 *
 *  Populated whenever a session lands: an explicit signup / login /
 *  recover, and a page reload that restores the session from the
 *  secrets persisted under the device key (`silent-restore.ts` — the
 *  session token and the ADR-0017 AccountIdentity seed). A reload that
 *  cannot restore every piece lands in `anon`, so `state().kind ===
 *  "session"` with these secrets `null` is no longer a resting state.
 *
 *  Operations that need to sign on behalf of the user
 *  (`ContactsClient.sendContactRequest`, MLS commits, …) require
 *  this to be set.
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
  /** The access token of the session pair (ADR-0065). Fifteen minutes
   *  long: a consumer that captures it is not wrong, because every bearer
   *  request goes through `sessionFetch`, which swaps in the live token;
   *  this field is updated on every refresh all the same. */
  readonly sessionToken: string;
  /** Access-token expiry as a unix-millisecond timestamp. */
  readonly sessionExpiresAtMs: number;
  /** The key generation the pair was issued under: what a landing compares
   *  with the generation the directory publishes (`landing-rule.ts`). */
  readonly keyGeneration: bigint;
}

/**
 * ADR-0065 sections 3.5 and 4.2 (`docs/ux/identity-recovery.md` 15.2):
 * this device's keys are not the account's keys any more. One screen,
 * reached two ways:
 *
 *   - `keys-reset`: a `401 key-generation-superseded`, or a landing that
 *     found a higher generation published. The wipe runs by itself; the
 *     screen shows "Removing..." then one button, "Sign in".
 *   - `identity-mismatch`: a landing that found a different key at the same
 *     generation (an anomaly, not a known reset). The screen blocks and the
 *     wipe runs on the person's tap ("Sign out").
 */
export type SignedOutState =
  | {
      readonly reason: "keys-reset";
      readonly phase: "removing" | "removed";
      readonly username: string;
    }
  | { readonly reason: "identity-mismatch"; readonly phase: "blocked" | "removing" | "removed"; readonly username: string };

/** Public surface of the auth context. */
export interface AuthApi {
  readonly state: Accessor<AuthState>;
  /** In-memory Identity secrets (1.16d3c). `null` whenever the
   *  user has not just authenticated in the current page session
   *  — see [`IdentitySecrets`] for the full shape and lifetime
   *  story. */
  readonly identitySecrets: Accessor<IdentitySecrets | null>;
  /** ADR-0017 lost-everything fallback — the freshly-minted 12-word
   *  recovery phrase awaiting the user's save acknowledgement. Non-null
   *  ONLY while the state is `save-fresh-phrase` (between
   *  `mintFreshIdentity` and `confirmFreshPhraseSaved`); `null`
   *  otherwise. The phrase lives in memory only and is dropped when the
   *  session lands. Kept off the `AuthState` object so it is not
   *  threaded through the route guards. */
  readonly freshPhrase: Accessor<string | null>;
  /** ADR-0065 (UX 15.4): `true` while the keys were reset but the new
   *  identity is not yet published to the directory and the device list.
   *  The phrase screen then shows the "publish owed" explanation and "Try
   *  again" instead of the words; `retryFreshPublish` repeats only the
   *  publishing steps and never resets again. */
  readonly freshPublishOwed: Accessor<boolean>;
  /** The stored reset is older than the client-side bound and this page
   *  has not seen an explicit sign-in: the phrase screen asks for one
   *  ("Sign in again to finish publishing your new keys."). */
  readonly freshPublishSignInNeeded: Accessor<boolean>;
  /** Retry the owed publishing steps for the identity `mintFreshIdentity`
   *  reset. Never resets again. Throws what the step threw (offline, `429`,
   *  `503`, a refusal). */
  retryFreshPublish(): Promise<void>;
  /** The recovery phrase a signup just generated, until the user acknowledges
   *  it; `null` otherwise. While non-null BOTH route guards render the phrase
   *  screen instead of any route (see `phrase-gate.ts` for the defect this
   *  closes: the anonymous guard used to navigate home the moment signup
   *  landed the session, before the phrase was ever shown). Survives a reload
   *  and a sign-out/sign-in on this device: the fact that it is owed is kept in
   *  storage and the phrase is read back from the device-key-wrapped cache. */
  readonly signupPhrase: Accessor<string | null>;
  /** The user has written the signup phrase down. Clears it (memory and the
   *  owed marker); the guards then render the route they wrap. */
  acknowledgeSignupPhrase(): void;
  /** ADR-0065: this device's keys are not the account's keys any more
   *  (`SignedOutState`). While non-null both route guards render the
   *  signed-out screen instead of any route, whatever `state()` says. */
  readonly signedOut: Accessor<SignedOutState | null>;
  /** The signed-out screen's button: on `identity-mismatch` (blocked) runs
   *  the wipe; once `removed`, clears the screen so the sign-in page can
   *  show. */
  confirmSignedOut(): Promise<void>;
  /** UX 15.3: the keys were reset on THIS device in this page session, so
   *  Settings > Devices and Settings > Passkeys show their one-session
   *  note. */
  readonly keyResetNote: Accessor<boolean>;
  dismissKeyResetNote(): void;
  /** Publish a fresh MLS KeyPackage for the signed-in account: mint it from
   *  the canonical identity, persist the snapshot holding its private key
   *  (fail-closed — no durable key, no upload), then upload it. Single-flight:
   *  requests made while a publish runs collapse into one trailing publish.
   *  Called at every session start and after every mailbox Welcome attempt,
   *  because a Welcome consumes the KeyPackage it was built on (see
   *  `chat/keypackage-publisher.ts`). Rejects when there is no session or any
   *  step fails. */
  republishKeyPackage(): Promise<void>;
  /** Run the OPAQUE-PAKE registration flow.
   *
   *  Throws on network / server errors (the form surfaces them).
   *  On success, persists `(accountId, username)` to localStorage,
   *  derives the device root key + writes it to IndexedDB, and
   *  flips state to `session`. The returned [`SignupOutcome`]
   *  carries the BIP39 recovery phrase that the UI MUST display
   *  exactly once. */
  /** `options.signal` cancels the account-creation proof of work (ADR-0054
   *  S4) and everything after it; the abort rejects with an `AbortError`
   *  (`auth/pow.ts` `isAbortError`) and nothing is created server-side. */
  signup(
    username: string,
    password: string,
    options?: { readonly signal?: AbortSignal },
  ): Promise<SignupOutcome>;
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
  signupGuest(options?: { readonly signal?: AbortSignal }): Promise<SignupOutcome>;
  /** Run the OPAQUE-PAKE login flow.
   *
   *  ADR-0054 S5: signs in by name. The start carries the login handle of
   *  the typed `username`, nothing is looked up before the session, and
   *  the account id comes back in the finish — so a fresh browser can log
   *  in to any account whose handle is bound. Throws on unknown username
   *  or wrong password (both surface the same error — by design). */
  login(username: string, password: string): Promise<void>;
  /** Run the recover-from-mnemonic flow.
   *
   *  Decodes the typed phrase via `RecoveryMnemonic.fromPhrase`,
   *  drives `AuthClient.recover` to rotate the server's password
   *  envelope, derives a fresh device root key from the new
   *  export key, persists it via the IDB pipeline, and flips
   *  state to `session`.
   *
   *  Addressed by name like `login` (ADR-0054 S5): the typed `username`,
   *  else the account cached on this device; the account id comes back in
   *  the finish. `username` is required only when no named account is
   *  cached on this device.
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
   *  code. Addressed by `username`'s login handle like `login`
   *  (ADR-0054 S5), drives `AuthClient.totpRecover` for the session triple, and lands in
   *  the EXISTING ADR-0017 post-ACCESS identity-acquisition state
   *  machine: a cached AccountIdentity seed (decryptable under a
   *  stored device key) finishes the session directly; otherwise the
   *  state flips to `needs-recovery-phrase` and the
   *  RestoreIdentityPrompt offers another device / a passkey / the phrase.
   *  On a device that holds the keys the new-password step is offered.
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
  /** The account "Three words from your phrase" can sign in on this
   *  device, or `null` when the route is not offered here. A LOCAL fact
   *  (maintainer decision 2026-10-02): an account is cached on this
   *  device with a username, and its device key opens both the recovery
   *  phrase and the keys stored here. No server is asked, so the answer
   *  leaks nothing about any account. */
  threeWordsAccount(): Promise<{ readonly accountIdB64: string; readonly username: string } | null>;
  /** Three words, checked locally like a wallet does. `positions` are the
   *  three word numbers the form drew (`phraseCheckRandomPositions`) and
   *  `words[i]` answers `positions[i]`. The words are compared with the
   *  phrase this device holds; the server takes no part in the check and
   *  stores nothing about it.
   *
   *  Resolves `false` on wrong words (nothing is sent anywhere). On a
   *  match the device signs in with what it already holds: the recovery
   *  login (`/recovery/login/*`) run from the seed of the stored phrase,
   *  which yields a session without replacing the password. It then lands
   *  through the shared ACCESS-only landing with the new-password step
   *  owed, and resolves `true`.
   *
   *  The guess budget (five misses, then a 15-minute pause) is the
   *  form's, in `recover/three-words.ts`: a client-side gate in front of
   *  a credential this device already holds.
   *
   *  When the words match but the sign-in made with the stored seed is
   *  refused as invalid credentials (`recover/stale-phrase.ts`): throws
   *  `RecoveryCredentialPendingError` if this device still owes the server
   *  a recovery-credential replace (nothing changes), else suspends three
   *  words on this device, clears their budget (the stored phrase and
   *  everything else stay) and throws `ThreeWordsSuspendedError`. */
  recoverWithThreeWords(positions: Uint8Array, words: readonly string[]): Promise<boolean>;
  /** Whether this device still owes the directory or the device list the
   *  signed-in account's new keys (ADR-0065: a reset whose publish has not
   *  succeeded). While it does, the phrase is not shown anywhere. */
  publishOwed(): boolean;
  /** Retry the owed publishing steps for the signed-in account with the
   *  seed this session holds. Resolves when nothing is owed (any more).
   *  Throws what the step threw (offline, `429`, `503`, a refusal). */
  retryOwedPublish(): Promise<void>;
  /** The recovery phrase stored on this device for the signed-in account,
   *  or `null` (Settings > Security > Recovery, "Show recovery phrase").
   *  Read from the device, never from a server. */
  devicePhrase(): Promise<string | null>;
  /** Whether `password` is the signed-in account's password: the gate in
   *  front of "Show recovery phrase" on web. A password can only be checked
   *  with the server (one ordinary sign-in exchange); the session it yields
   *  is discarded. `false` on a wrong password; throws on a limit (`429`),
   *  when offline, or for an account without a username. */
  verifyPassword(password: string): Promise<boolean>;
  /** Passkey sign-in (ADR-0064 decision 6, UX spec section 4). The
   *  ceremony is discoverable: it needs no account cached on this device and
   *  no typed name. On a device that holds the account's keys it lands
   *  signed in. On a device without them it restores the keys when this
   *  passkey carries them (its PRF output opens the blob stored for it and
   *  the phrase inside matches the account's published identity,
   *  fail-closed), and otherwise parks in `needs-recovery-phrase` with
   *  `restoreNotice` saying why. `usernameHint` names the account on a
   *  device that has none cached; it is kept only when discovery resolves
   *  it to the signed-in account. `forgotPassword` marks the sign-in as a
   *  forgot-password route, so the new-password step is offered once the
   *  device holds the keys. */
  loginWithPasskey(
    usernameHint?: string,
    options?: { readonly forgotPassword?: boolean },
  ): Promise<void>;
  /** The restore prompt's passkey row: only valid in
   *  `needs-recovery-phrase`. Runs the passkey ceremony for the signed-in
   *  account and asks the passkey for the keys. `restored` lands the
   *  session; the other outcomes leave the prompt up and set
   *  `restoreNotice`. */
  restoreKeysWithPasskey(): Promise<PasskeyKeysOutcome>;
  /** Why the restore prompt is showing after a passkey sign-in, or `null`. */
  readonly restoreNotice: Accessor<RestoreNotice | null>;
  /** The session token of the sign-in parked in `needs-recovery-phrase`
   *  (the device is signed in, its keys are not here yet), else `null`. The
   *  restore prompt uses it to ask which passkeys carry keys. */
  readonly pendingSessionToken: Accessor<string | null>;
  /** ADR-0017 fresh-device fail-safe completion. Called only while
   *  the state is `needs-recovery-phrase`: the user typed their
   *  recovery phrase, we derive the BIP39 seed, cache it under the
   *  device key, rebuild the canonical AccountIdentity from it, and
   *  flip to a full `session`. Throws on a bad / mismatched phrase
   *  (the derived pubkey does not match the account's published
   *  bundle) so the user cannot silently fork their identity. */
  provideRecoveryPhrase(mnemonicPhrase: string): Promise<void>;
  /** Whether the "Choose a new password" step (UX spec 7.1) is owed: a
   *  forgot-password route succeeded and this device holds the keys. The
   *  guards render the step while this is true. */
  readonly newPasswordOffered: Accessor<boolean>;
  /** "Not now" on the new-password step. */
  dismissNewPasswordOffer(): void;
  /** Whether `replacePassword` can run: a session with a username on a
   *  device that holds the account's keys. */
  readonly canReplacePassword: Accessor<boolean>;
  /** Replace the account's password from a device that holds its keys,
   *  without typing the phrase (UX spec 7.1, ADR-0064 "Password replacement
   *  from a device that holds the keys"). The cached 64-byte identity seed
   *  is the recovery OPAQUE input `/password/reset/*` accepts, so the device
   *  proves it holds the keys and the person types only the new password.
   *  Unlike `recover`, nothing on this device is re-keyed or wiped: the
   *  stored device key is kept. */
  replacePassword(newPassword: string): Promise<void>;
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
   *  Recovery payload v2: what arrives is the 16-byte entropy of the
   *  recovery phrase. The phrase and the 64-byte seed are rebuilt from it,
   *  the seed is checked, and only then are the seed AND the phrase stored
   *  on this device.
   *
   *  CONSUMES `phraseEntropy` (zeroized here; the derived seed is zeroized
   *  on every refusal path). */
  completeFromPairing(
    phraseEntropy: Uint8Array,
    session?: SessionPair,
  ): Promise<void>;
  /** ADR-0018 factor (a), path (b) — the NO-PASSWORD entry of the
   *  recovery cascade. The fresh device ran the pairing
   *  anonymously, received `phraseEntropy` + a REDEEMED session token
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
    /** 16-byte entropy of the recovery phrase (recovery payload v2).
     *  CONSUMED: zeroized here, and the seed derived from it is zeroized
     *  on every refusal path. */
    readonly phraseEntropy: Uint8Array;
    /** The redeemed session pair (ADR-0065). */
    readonly session: SessionPair;
    /** Persist the pairing's in-memory DeviceIdentity seed +
     *  DeviceGroup snapshot under the freshly-bootstrapped device
     *  key (`AnonymousJoinSession.persistTo`). Best-effort. */
    readonly persistJoinState?: (deviceKey: CryptoKey) => Promise<void>;
    /** The restore was reached through "Forgot password?": offer the
     *  new-password step once the session lands (UX spec section 7). */
    readonly forgotPassword?: boolean;
  }): Promise<void>;
  /** "Reset account keys" (ADR-0065 section 2.3, UX 15.1): the one path
   *  that gives the account a new identity. Callable ONLY while
   *  `pendingRecovery() !== null` (the person recovered ACCESS and is
   *  parked in `needs-recovery-phrase` with no phrase and no other device)
   *  and only from the explicit, typed-confirmation button.
   *
   *  Order: generate the phrase and the seed, derive the identity; the
   *  server reset (`reset/start`, `reset/finish`: the point of no return -
   *  the account moves to a new key generation, every other device is
   *  signed out, every passkey deleted, the recovery credential replaced,
   *  the caller gets an attestation and a session pair at the new
   *  generation); persist the seed, the phrase, the pair and the
   *  attestation with a "publish owed" record BEFORE any further network
   *  call; then publish: the identity bundle with the attestation to the
   *  directory, the attested reset of the DeviceGroup journal. A failure in
   *  the server step changes nothing and discards the phrase; a failure in
   *  a publishing step leaves the record for `retryFreshPublish`.
   *
   *  Flips to `save-fresh-phrase`; the words are shown only once nothing is
   *  owed. Returns the new 12-word phrase. The contacts see a changed key
   *  and re-verify out of band.
   *
   *  The server refuses a reset without a fresh-auth grant (UX 17.2). The
   *  caller obtains one first (`proveWithPassword` / `proveWithPasskey`, or
   *  a sign-in by password or passkey in the last ten minutes on this
   *  page). Throws `FreshAuthRequiredError` when no live grant is held, or
   *  when the server answers `403 fresh-auth-required` (nothing was reset;
   *  the screen shows the proof block). */
  mintFreshIdentity(): Promise<string>;
  /** Whether a live fresh-auth proof is held for the account of this
   *  session (signed in, or parked in `needs-recovery-phrase`). Not
   *  reactive: it depends on the clock, so a surface reads it when it
   *  opens, on a timer and at submit. */
  freshProofLive(): boolean;
  /** The live fresh-auth grant for this session's account, or `null`.
   *  Opaque; sent as `fresh_grant` (or the `x-lakko-fresh-grant` header).
   *  Never store or log it. */
  freshProof(): string | null;
  /** The server refused `grant` (`403 fresh-auth-required`): drop it, so
   *  the surface asks again. A newer grant is kept. */
  dropFreshProof(grant: string): void;
  /** Obtain a proof by password: one ordinary password sign-in for THIS
   *  account, addressed by its username (`username` when the device does
   *  not know it). The pair it returns replaces the session's for the same
   *  account; nothing else about a sign-in happens (no landing rule, no
   *  wipe, no handle re-bind, no state change). Throws
   *  `ProofWrongPasswordError` (also for a name that is another
   *  account's), `FreshAuthRequiredError` when the sign-in minted no
   *  grant, and the transport's own errors for a limit or the network. */
  proveWithPassword(password: string, username?: string): Promise<void>;
  /** Obtain a proof by passkey: the passkey sign-in ceremony limited to
   *  this account's passkeys. Throws `ProofCancelledError`,
   *  `ProofOtherAccountError` (nothing installed), `ProofPasskeyFailedError`,
   *  or the transport's own errors for a limit or the network. */
  proveWithPasskey(): Promise<void>;
  /** How this page's session was landed (UX 17.6). */
  sessionLanding(): SessionLanding;
  /** The local three-word check for the SIGNED-IN account (no sign-in
   *  follows): `words[i]` answers `positions[i]`, compared with the phrase
   *  this device holds. Nothing is sent anywhere. A match is remembered in
   *  memory for ten minutes and is one of the two things that open the
   *  password change. Throws `recovery.words.error.unavailable` (keyed)
   *  when this device holds no phrase. */
  checkPhraseWords(positions: Uint8Array, words: readonly string[]): Promise<boolean>;
  /** Whether `replacePassword` may run right now: a live proof, or a local
   *  phrase check in the last ten minutes on this page. */
  passwordChangeAllowed(): boolean;
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

/** Why the restore prompt opened after a passkey sign-in (UX spec section
 *  8, the optional first line). */
export type RestoreNotice =
  /** The passkey signed in and carries no keys (no PRF output, or nothing
   *  stored for it). */
  | "passkey-no-keys"
  /** Keys are stored for the passkey but did not open, or the phrase they
   *  gave is not the account's. Nothing was changed. */
  | "passkey-unusable";

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
  readonly session: SessionPair;
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
  /** The session pair at the NEW generation (ADR-0065). */
  readonly session: SessionPair;
  readonly username: string;
  /** The freshly-minted 12-word recovery phrase — re-exposed to the
   *  UI by the `save-fresh-phrase` render so the user can save it. */
  readonly phrase: string;
  /** The device key the new seed and phrase are stored under. */
  readonly deviceKey: CryptoKey;
}

/** `localStorage`, or `undefined` where there is none (tests, a blocked
 *  store): the owed markers then live for the page only. */
function plainStorage(): Storage | undefined {
  return typeof localStorage === "undefined" ? undefined : localStorage;
}

const loadPersisted = loadPersistedAccount;
const savePersisted = savePersistedAccount;

/** Encode a UTF-8 string into bytes — `password` enters
 *  OPAQUE-PAKE as raw bytes. */
function utf8(s: string): Uint8Array {
  return new TextEncoder().encode(s);
}

export function AuthProvider(props: { children: JSX.Element }): JSX.Element {
  // Start in `loading` so the very first paint doesn't snap to
  // `/login`; the onMount hook reads localStorage + IDB, then
  // resolves to either `session` (full restore) or `anon`.
  const [state, setStateSignal] = createSignal<AuthState>({ kind: "loading" });
  // The contact list belongs to the signed-in account (`contacts/local-store`):
  // bind that account the moment a session lands and unbind it on every other
  // state, BEFORE the signal moves, so nothing the new state renders can read
  // the previous account's contacts. Every state change goes through here.
  const setState = (next: AuthState): void => {
    bindContactsAccount(next.kind === "session" ? next.account.accountId : null);
    setStateSignal(next);
  };
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
  // 12-word phrase; `confirmFreshPhraseSaved` reads this back to
  // land the session, then clears it. The seed never leaves this
  // closure until the session is finished.
  const [pendingFreshPhrase, setPendingFreshPhrase] =
    createSignal<PendingFreshPhrase | null>(null);
  // The signup recovery phrase awaiting acknowledgement — see
  // `AuthApi.signupPhrase` and `phrase-gate.ts`.
  const [signupPhrase, setSignupPhrase] = createSignal<string | null>(null);
  // UX spec section 8: why the restore prompt is up after a passkey sign-in.
  const [restoreNotice, setRestoreNotice] = createSignal<RestoreNotice | null>(null);
  // UX spec 7.1: a forgot-password route succeeded, so the new-password step
  // is owed once the device holds the keys. In memory only: a reload drops
  // the offer, and Settings > Security keeps the same form.
  const [newPasswordOwed, setNewPasswordOwed] = createSignal(false);
  /** A session with a username whose identity seed is in memory: what
   *  `replacePassword` needs. A guest (no username) has no login handle to
   *  address the reset by. */
  const canReplacePassword = (): boolean => {
    const s = state();
    const secrets = identitySecrets();
    return (
      s.kind === "session" &&
      s.account.username.trim().length > 0 &&
      secrets !== null &&
      secrets.accountIdentitySeed.length === ACCOUNT_IDENTITY_SEED_LENGTH
    );
  };
  const newPasswordOffered = (): boolean => newPasswordOwed() && canReplacePassword();
  // ADR-0065: this device's keys are not the account's keys any more (a
  // superseded session, or a landing that found other keys published). The
  // guards render the signed-out screen while this is set.
  const [signedOut, setSignedOut] = createSignal<SignedOutState | null>(null);
  // UX 15.3: the keys were reset on this device in this page session.
  const [keyResetNote, setKeyResetNote] = createSignal(false);
  // ADR-0065 section 4.2, last paragraph: an attestation older than the
  // client-side bound is applied only after an explicit sign-in in this page
  // (a silent restore is not one).
  let explicitSignInThisPage = false;

  const client = new AuthClient();
  const discovery = new DiscoveryClient();
  /**
   * Install (or drop) the session everywhere a request draws on it: the
   * session token provider (ADR-0065: refresh ahead of expiry, one retry on
   * a 401, the superseded signal), the discovery client's bearer header and
   * the anonymous-token pool (ADR-0054 S3), which draws its tokens WITH this
   * session and spends them without it. A different account swaps the pool
   * to that account's own tokens; `null` stops drawing but keeps what is
   * held until the sign-out has spent its push unregister.
   */
  // ADR-0065 (amended 2026-10-06): the fresh-auth proof. Memory only, never
  // persisted, never logged (`fresh-auth.ts`).
  const freshGrants = createFreshGrantHolder();
  // How this page's session was landed (UX 17.6); with the session, in
  // memory.
  let sessionLanding: SessionLanding = "other";
  // When the recovery phrase was last checked locally on this page (the
  // password-change gate), unix ms.
  let phraseCheckedAtMs: number | null = null;
  /** The account this page is signed in to: a full session, or the one
   *  parked in `needs-recovery-phrase`. */
  function proofAccount(): { readonly accountIdB64: string; readonly username: string } | null {
    const s = state();
    if (s.kind === "session" || s.kind === "needs-recovery-phrase") {
      return { accountIdB64: s.account.accountId, username: s.account.username };
    }
    const pending = pendingRecovery();
    return pending === null
      ? null
      : { accountIdB64: pending.accountIdB64, username: pending.username };
  }
  /** Replace this session's pair with the one a proof sign-in returned, for
   *  the SAME account at the same generation: no landing rule, no re-bind
   *  (the handle sync is keyed on the account), no state change. */
  async function adoptProofSession(accountIdB64: string, pair: SessionPair): Promise<void> {
    installSession(accountIdB64, pair);
    await persistSessionPair(accountIdB64, pair, "proof sign-in");
    const secrets = identitySecrets();
    if (secrets !== null && base64Encode(secrets.accountId) === accountIdB64) {
      setIdentitySecrets({
        ...secrets,
        sessionToken: pair.sessionToken,
        sessionExpiresAtMs: pair.sessionExpiresAtMs,
        keyGeneration: pair.keyGeneration,
      });
    }
    const parked = pendingRecovery();
    if (parked !== null && parked.accountIdB64 === accountIdB64) {
      setPendingRecovery({ ...parked, session: pair });
    }
  }
  function installSession(accountIdB64: string, pair: SessionPair): void {
    sessionTokens.install(accountIdB64, pair);
    discovery.setSessionToken(pair.sessionToken);
    anonTokens.setSessionToken(pair.sessionToken);
  }
  function dropSession(): void {
    // Sign-out and the account wipe both pass through here.
    freshGrants.clear();
    sessionLanding = "other";
    phraseCheckedAtMs = null;
    sessionTokens.clear();
    discovery.setSessionToken(null);
    anonTokens.setSessionToken(null);
  }
  /** Persist `pair` under the account's device key. Best effort: logged. */
  async function persistSessionPair(accountIdB64: string, pair: SessionPair, context: string): Promise<void> {
    try {
      const deviceKey = await getDeviceKey(accountIdB64);
      if (deviceKey !== null) await putSessionPair(accountIdB64, pair, deviceKey);
    } catch (err) {
      // eslint-disable-next-line no-console
      console.warn(`session-pair persist (${context}) failed`, err);
    }
  }
  sessionTokens.configure({
    refresh: async (refreshToken) => {
      try {
        return { kind: "pair", pair: await client.sessionRefresh(refreshToken) };
      } catch (err) {
        if (err instanceof AuthHttpError) {
          return { kind: "refused", status: err.status, errorCode: err.errorCode };
        }
        throw err;
      }
    },
    persist: (accountIdB64, pair) => persistSessionPair(accountIdB64, pair, "refresh"),
  });
  onCleanup(
    sessionTokens.subscribe((event) => {
      switch (event.kind) {
        case "pair": {
          // A refreshed pair: every reader of the token sees the new one.
          discovery.setSessionToken(event.pair.sessionToken);
          anonTokens.setSessionToken(event.pair.sessionToken);
          const secrets = identitySecrets();
          if (secrets !== null && base64Encode(secrets.accountId) === event.accountIdB64) {
            setIdentitySecrets({
              ...secrets,
              sessionToken: event.pair.sessionToken,
              sessionExpiresAtMs: event.pair.sessionExpiresAtMs,
              keyGeneration: event.pair.keyGeneration,
            });
          }
          const parkedRecovery = pendingRecovery();
          if (parkedRecovery !== null && parkedRecovery.accountIdB64 === event.accountIdB64) {
            setPendingRecovery({ ...parkedRecovery, session: event.pair });
          }
          const parkedPhrase = pendingFreshPhrase();
          if (parkedPhrase !== null && parkedPhrase.accountIdB64 === event.accountIdB64) {
            setPendingFreshPhrase({ ...parkedPhrase, session: event.pair });
          }
          return;
        }
        case "superseded":
          void signOutAfterKeyReset(event.accountIdB64);
          return;
        case "refresh-invalid":
          endExpiredSession();
          return;
      }
    }),
  );
  // ADR-0054 S5: sign-in by name needs the account's login handle bound at
  // auth. Signup and a sign-in by name bind it themselves (and record that
  // here); every other way a session lands - reload restore, passkey, device
  // pairing, another-device confirmation, recovery phrase - is
  // covered by the effect below, which binds the handle of the session's
  // username once per session in this page. Best-effort and invisible: a
  // 409, a 503 or a network error changes nothing for the user.
  const loginHandleSync = createLoginHandleSync({
    handleFor: loginHandleFor,
    bind: (token, handle) => client.bindLoginHandle(token, handle),
  });
  /** The re-bind a sign-in by name runs, recorded so the effect does not
   *  repeat it for the same session. */
  function bindByName(token: string, handle: string): Promise<void> {
    loginHandleSync.markBound(token, handle);
    return client.bindLoginHandle(token, handle);
  }
  // 1.16e5: drop active groups on logout/forgetDevice so MLS
  // state doesn't bleed across user sessions on the same tab.
  const chats = useChats();

  // Whatever path installed the session, bind its username's login handle
  // (see `loginHandleSync` above). Runs when the session lands and whenever
  // the token or the username changes; a guest (no username) binds nothing.
  createEffect(() => {
    const s = state();
    const secrets = identitySecrets();
    if (s.kind !== "session" || secrets === null) return;
    void loginHandleSync.ensure(secrets.sessionToken, s.account.username);
  });

  // Keep the encrypted backup current. Until this ran, the blob was written
  // exactly twice in an account's life - an empty baseline at signup and one
  // at recover - so a restore on a new device landed the person back at
  // registration day. The driver is inert while nothing has changed; see
  // `backup/auto-backup.ts` for what counts as a change and why.
  onCleanup(
    startAutoBackup({
      accountId: () => identitySecrets()?.accountId ?? null,
      // `null` until the chat layer has loaded, so an empty list during
      // startup is never mistaken for "left every group".
      knownGroupIds: () =>
        chats.identity() === null ? null : chats.groups().map((g) => g.groupIdB64),
      accountIdB64: () => {
        const id = identitySecrets()?.accountId;
        return id === undefined ? null : base64Encode(id);
      },
      // The signing seed lives on the canonical Identity, not in the auth
      // secrets: the backup envelope is sealed under the recovery phrase but
      // signed with this.
      ed25519Seed: () => chats.identity()?.ed25519Seed ?? null,
      deviceKey: async () => {
        const id = identitySecrets()?.accountId;
        if (id === undefined) return null;
        return getDeviceKey(base64Encode(id));
      },
      sessionToken: () => identitySecrets()?.sessionToken ?? null,
      snapshot: () => {
        const id = chats.identity();
        return id === null ? null : id.snapshotStorage();
      },
    }),
  );

  // The account the KeyPackage publisher publishes for. Set the moment a
  // canonical identity is installed for a session, cleared on logout /
  // forgetDevice. Held outside `identitySecrets` because the session-start
  // publish runs BEFORE those secrets are exposed to the UI.
  let keyPackageAccount: {
    readonly accountIdBytes: Uint8Array;
    readonly accountIdB64: string;
  } | null = null;
  const keyPackagePublisher = createKeyPackagePublisher<{
    readonly accountIdBytes: Uint8Array;
    readonly accountIdB64: string;
    readonly identity: WasmIdentity;
  }>({
    resolveContext: () => {
      const account = keyPackageAccount;
      const identity = chats.identity();
      if (account === null || identity === null) {
        throw new Error("no active session to publish a KeyPackage for");
      }
      return { ...account, identity };
    },
    createKeyPackage: (ctx) => ctx.identity.createKeyPackage(),
    persistDurable: (ctx) =>
      chats.persistSnapshotDurable(ctx.accountIdB64, ctx.identity),
    // ADR-0050: the row is keyed by this device's leaf key.
    upload: (ctx, keyPackage) =>
      discovery.uploadKeyPackage(
        ctx.accountIdBytes,
        keyPackage,
        ctx.identity.signaturePublicKey,
      ),
  });

  /** Session-start KeyPackage publish on the explicit auth flows. Awaited
   *  there (those flows already waited on the KeyPackage upload before), but
   *  never fatal: the session still lands, and the next session start or
   *  Welcome retries it. */
  async function publishKeyPackageBestEffort(context: string): Promise<void> {
    try {
      await keyPackagePublisher.publish();
    } catch (err) {
      // eslint-disable-next-line no-console
      console.warn(`KeyPackage publish (${context}) failed`, err);
    }
  }

  /**
   * No publish-back (ADR-0065 section 4.2): read what the directory
   * publishes for this account BEFORE anything is uploaded, and decide with
   * `landing-rule.ts`:
   *
   *   - the published key is this device's: re-publish the bundle (skipped
   *     in the silent-reload mode when the directory already serves exactly
   *     these keys, since the bundle is deterministic from the phrase seed);
   *   - nothing is published: publish, as a first sign-in always did;
   *   - a different key of a HIGHER generation: the account's keys were
   *     reset on another device; this device shows the signed-out screen
   *     and wipes itself, uploading nothing;
   *   - a different key at the SAME generation: an anomaly; the blocking
   *     screen with the wipe on the person's tap, uploading nothing;
   *   - the directory could not be read: upload nothing, land, check again
   *     on the next landing.
   *
   * The dismissible "published identity changed" banner and the re-upload
   * that healed the directory after it are gone: that re-upload was the
   * publish-back, and discovery refuses an unattested key change now.
   *
   * Returns the verdict; the caller lands only when it does not sign out.
   */
  async function refreshPublishedIdentity(
    canonical: WasmIdentity,
    accountIdBytes: Uint8Array,
    accountIdB64: string,
    username: string,
    ownKeyGeneration: bigint,
    skipUploadIfUnchanged: boolean,
  ): Promise<LandingVerdict> {
    let current: Awaited<ReturnType<DiscoveryClient["identityFetch"]>> | undefined;
    try {
      current = await discovery.identityFetch(accountIdBytes);
    } catch (err) {
      // eslint-disable-next-line no-console
      console.warn("published-identity check failed", err);
      current = undefined;
    }
    // A logout (or another sign-in) while the fetch was out: this identity is
    // no longer the session's, so it must neither sign out nor publish.
    if (chats.identity() !== canonical) return "unknown";
    const verdict = landingVerdict(
      current === undefined
        ? undefined
        : current === null
          ? null
          : {
              keyMatches: bytesEqual(
                base64Decode(current.signature_pubkey),
                canonical.signaturePublicKey,
              ),
              keyGeneration: BigInt(current.key_generation),
            },
      ownKeyGeneration,
    );
    if (verdict === "superseded") {
      void signOutAfterKeyReset(accountIdB64);
      return verdict;
    }
    if (verdict === "mismatch") {
      // eslint-disable-next-line no-console
      console.warn("published identity differs at the same key generation; blocking, uploading nothing");
      blockOnIdentityMismatch(accountIdB64, username);
      return verdict;
    }
    if (!landingMayUpload(verdict)) return verdict;
    if (skipUploadIfUnchanged) {
      if (
        verdict === "matches" &&
        current !== undefined &&
        current !== null &&
        bytesEqual(base64Decode(current.x25519_pubkey), canonical.x25519PublicKey)
      ) {
        return verdict;
      }
    }
    try {
      await uploadIdentityBundle(discovery, accountIdBytes, canonical);
    } catch (err) {
      // eslint-disable-next-line no-console
      console.warn("identity bundle refresh failed", err);
    }
    return verdict;
  }

  /**
   * ADR-0017 — finish a session once the canonical AccountIdentity
   * (phrase) seed is in hand: build the canonical Identity from the
   * seed, rehydrate MLS, refresh the published bundle, persist the
   * snapshot, set the in-memory secrets, and flip to `session`.
   * Shared by the warm same-device login / passkey paths, the
   * fresh-device `provideRecoveryPhrase` completion, and the silent
   * reload restore (`silentRestore`).
   *
   * `silentRestore` changes only what happens AFTER the canonical identity
   * and its MLS state are in place: the session lands at once, and the
   * directory self-check, the bundle re-publish (skipped when unchanged) and
   * the KeyPackage publish run in the background. A reload must not sit on a
   * "Restoring session" screen for a proof-of-work solve, and it must not
   * prompt for notification permission without a user gesture, so push
   * subscription is left to the explicit sign-in flows (a subscription the
   * browser already holds survives the reload anyway).
   */
  /**
   * If this account still owes the signup phrase screen (the user reloaded, or
   * signed out, before acknowledging it), load the phrase back from the
   * device-key-wrapped cache so the guards show it again before any route.
   *
   * Runs BEFORE a session lands, so there is no moment in which the app renders
   * past an owed phrase. Best-effort in the one direction it can be: a phrase
   * that is owed but no longer readable (the backup bootstrap never cached it,
   * or the device key changed) cannot be shown, and is logged rather than
   * blocking the account forever.
   */
  async function restoreOwedSignupPhrase(accountIdB64: string): Promise<void> {
    const storage = typeof localStorage === "undefined" ? undefined : localStorage;
    if (!isSignupPhraseOwed(storage, accountIdB64)) return;
    // A reset whose publish is owed: the identity the phrase belongs to is
    // not findable yet, so the words are withheld until both publishing
    // steps landed (`settleOwedKeyResetPublish` calls back here then).
    if (isKeyResetOwed(storage, accountIdB64)) return;
    try {
      const deviceKey = await getDeviceKey(accountIdB64);
      const phrase =
        deviceKey === null ? null : await readDevicePhraseWith(accountIdB64, deviceKey);
      if (phrase === null) {
        // eslint-disable-next-line no-console
        console.warn(
          "signup recovery phrase is owed but not readable on this device; it cannot be shown again",
        );
        return;
      }
      setSignupPhrase(phrase);
    } catch (err) {
      // eslint-disable-next-line no-console
      console.warn("reading the owed signup recovery phrase failed", err);
    }
  }

  /**
   * ADR-0050 — install the session's two identities. `canonical` is the
   * phrase-seeded ACCOUNT identity (contacts, discovery, safety number); the
   * DEVICE identity — a per-install random seed bound to the account key by a
   * signed binding carried in its leaf credential — is what every
   * conversation group and every KeyPackage uses from now on, so a person's
   * devices are distinct MLS leaves that all resolve to one account.
   *
   * The binding is minted once and persisted so its `issued_at` is stable;
   * a missing device key (a half-reset device) falls back to an unbound
   * canonical identity for the chats, which is the pre-ADR-0050 behaviour.
   */
  async function installSessionIdentities(
    wasm: Wasm,
    canonical: WasmIdentity,
    accountIdB64: string,
  ): Promise<void> {
    chats.setAccountIdentity(canonical);
    let device: WasmIdentity | null = null;
    try {
      const deviceKey = await getDeviceKey(accountIdB64);
      if (deviceKey !== null) {
        const seed = await ensureDeviceIdentitySeed(accountIdB64, deviceKey);
        const stored = await getDeviceBinding(accountIdB64);
        if (stored !== null) {
          try {
            device = wasm.Identity.fromSeedWithBinding(seed, stored);
            // ADR-0065 section 2.3 step 5: a binding is signed by the
            // account key, so a cached one minted before a key reset names
            // the OLD key and would publish a KeyPackage nobody can resolve
            // to this account. Re-mint whenever the bound key is not the
            // session's canonical key.
            const bound = device.accountPublicKey;
            if (bound === undefined || !bytesEqual(bound, canonical.signaturePublicKey)) {
              // eslint-disable-next-line no-console
              console.warn("stored device binding names another account key, re-minting");
              device.free();
              device = null;
            }
          } catch (err) {
            // A binding that no longer verifies: re-mint below.
            // eslint-disable-next-line no-console
            console.warn("stored device binding rejected, re-minting", err);
            device = null;
          }
        }
        if (device === null) {
          device = wasm.Identity.fromSeedBound(
            seed,
            canonical.ed25519Seed,
            BigInt(Math.floor(Date.now() / 1000)),
          );
          const binding = device.deviceBinding;
          if (binding !== undefined) await putDeviceBinding(accountIdB64, binding);
        }
      }
    } catch (err) {
      // eslint-disable-next-line no-console
      console.warn("device identity install failed; chats fall back to the account identity", err);
      device = null;
    }
    if (device === null) {
      // The canonical identity now serves both roles; `setIdentity` would
      // free the instance `setAccountIdentity` holds, so hand the chats a
      // second instance of the same seed.
      chats.setIdentity(wasm.Identity.fromSeed(canonical.ed25519Seed));
      return;
    }
    chats.setIdentity(device);
  }

  /**
   * ADR-0065: a reset whose publishing steps did not all succeed leaves a
   * record (`key-reset-owed.ts`). When a session for that account lands,
   * the device holds the new seed, so the steps are tried again here, in
   * the background. Success clears the record and shows the phrase if it
   * was never acknowledged; a failure is logged and waits for the next
   * session start (or "Try again" on the phrase screen / in Settings).
   */
  function settleOwedKeyResetPublish(
    accountIdB64: string,
    accountIdBytes: Uint8Array,
    accountIdentitySeed: Uint8Array,
  ): void {
    const record = readKeyResetOwed(plainStorage(), accountIdB64);
    if (record === null || remainingKeyResetSteps(record.done).length === 0) return;
    const seed = accountIdentitySeed.slice();
    void (async (): Promise<void> => {
      try {
        const deviceKey = await getDeviceKey(accountIdB64);
        if (deviceKey === null) return;
        await runKeyResetPublish({ accountIdBytes, accountIdB64, seed, deviceKey, record });
        // The new keys are findable now: show the phrase if it was never
        // acknowledged.
        await restoreOwedSignupPhrase(accountIdB64);
      } catch (err) {
        // eslint-disable-next-line no-console
        console.warn(
          "owed key-reset publish failed; it is retried at the next session start",
          err instanceof Error ? err.name : "unknown",
        );
      } finally {
        seed.fill(0);
      }
    })();
  }

  /** The person asked to finish publishing (the phrase screen, Settings):
   *  the owed steps, awaited; throws what the failing step threw. */
  async function retryKeyResetPublish(
    accountIdB64: string,
    accountIdBytes: Uint8Array,
    accountIdentitySeed: Uint8Array,
    deviceKey: CryptoKey,
  ): Promise<void> {
    const record = readKeyResetOwed(plainStorage(), accountIdB64);
    if (record === null || remainingKeyResetSteps(record.done).length === 0) return;
    const seed = accountIdentitySeed.slice();
    try {
      await runKeyResetPublish({ accountIdBytes, accountIdB64, seed, deviceKey, record });
    } finally {
      seed.fill(0);
    }
  }

  /**
   * The publishing half of a key reset (ADR-0065 section 2.3 steps 3-5,
   * contract section 5), run from the stored record so a retry after a
   * crash or a failure repeats exactly the steps that did not land, with
   * the SAME attestation:
   *
   *   1. the client-side gate: an attestation older than 30 days is applied
   *      only after an explicit sign-in in this page;
   *   2. the attestation is verified locally against the pinned attest key
   *      before it is used (fail-closed when a key is pinned; with no pin
   *      the JWKS key is used, and when that cannot be fetched the local
   *      check is skipped with a warning: discovery and the DS verify under
   *      their own pinned key regardless);
   *   3. `POST /discovery/identity` with the new bundle, the genesis
   *      self-signature and attestation, and the key-reset attestation
   *      (`409 key-generation-not-newer` = already landed);
   *   4. the DeviceGroup: the old snapshot is dropped, a new group founded
   *      and its journal re-rooted with the attested `PUT .../reset`
   *      (`409` + the root probe answering 204 = already landed).
   *
   * Each step marks itself done in the record as it lands; the record is
   * cleared once both have. Throws the step's error (an `AuthHttpError` /
   * `DiscoveryHttpError` / `DsHttpError` / a network error, or a
   * `KeyedError` for a refusal of the attestation itself).
   */
  let keyResetPublishInFlight: Promise<void> | null = null;
  function runKeyResetPublish(ctx: {
    readonly accountIdBytes: Uint8Array;
    readonly accountIdB64: string;
    readonly seed: Uint8Array;
    readonly deviceKey: CryptoKey;
    readonly record: KeyResetOwed;
  }): Promise<void> {
    // One run at a time per page: the background settle on a landing and a
    // "Try again" must not found the DeviceGroup twice.
    if (keyResetPublishInFlight !== null) return keyResetPublishInFlight;
    const run = runKeyResetPublishOnce(ctx).finally(() => {
      keyResetPublishInFlight = null;
    });
    keyResetPublishInFlight = run;
    return run;
  }

  async function runKeyResetPublishOnce(ctx: {
    readonly accountIdBytes: Uint8Array;
    readonly accountIdB64: string;
    readonly seed: Uint8Array;
    readonly deviceKey: CryptoKey;
    readonly record: KeyResetOwed;
  }): Promise<void> {
    let record = ctx.record;
    const storage = plainStorage();
    if (attestationGate(record.signedAtMs, Date.now(), explicitSignInThisPage) === "sign-in-needed") {
      throw new KeyedError("recovery_phrase.fresh.sign_in_to_finish");
    }
    const wasm = await loadWasm();
    const attestation = base64UrlDecodeNoPad(record.attestationB64Url);
    const attestationStdB64 = base64Encode(attestation);
    const canonical = wasm.Identity.fromSeed(ctx.seed);
    try {
      if (!bytesEqual(base64Decode(record.newSignaturePubkeyB64), canonical.signaturePublicKey)) {
        // The record names another identity than the seed this session
        // holds: never publish it.
        throw new KeyedError("recovery_phrase.fresh.publish_refused");
      }
      await verifyKeyResetAttestationLocally(wasm, attestation, ctx.accountIdBytes, canonical.signaturePublicKey);

      const persist = (next: KeyResetOwed): void => {
        record = next;
        writeKeyResetOwed(storage, ctx.accountIdB64, next);
      };
      for (const step of remainingKeyResetSteps(record.done)) {
        if (step === "directory") {
          await publishResetIdentity(canonical, ctx.accountIdBytes, attestation);
        } else {
          await refoundDeviceGroupAfterReset(ctx, attestationStdB64);
        }
        persist(withKeyResetStepDone(record, step satisfies KeyResetStep));
      }
      clearKeyResetOwed(storage, ctx.accountIdB64);
    } finally {
      canonical.free();
    }
  }

  /** Step 3: the attested identity upload. */
  async function publishResetIdentity(
    canonical: WasmIdentity,
    accountIdBytes: Uint8Array,
    attestation: Uint8Array,
  ): Promise<void> {
    // G-30 #2 (ADR-0019): a new key is a NEW transparency-log binding, so
    // the genesis self-signature authorises it, plus the ADR-0036
    // attestation (soft-fail), plus the ADR-0065 attestation that lets
    // discovery accept a key change without a supersession proof.
    const leafAuth = await mintLeafGenesisAuth(canonical, accountIdBytes);
    const genesisAttest = await mintGenesisAttestation(client, sessionTokens.peek() ?? "", canonical);
    try {
      await uploadIdentityBundle(
        discovery,
        accountIdBytes,
        canonical,
        undefined,
        leafAuth,
        genesisAttest,
        attestation,
      );
    } catch (err) {
      if (err instanceof DiscoveryHttpError) {
        const outcome = directoryResetOutcome({ status: err.status, errorCode: err.errorCode });
        if (outcome === "done") return;
        if (outcome === "superseded") {
          sessionTokens.reportSuperseded();
          throw err;
        }
        if (outcome === "refused") throw new KeyedError("recovery_phrase.fresh.publish_refused");
      }
      throw err;
    }
  }

  /** Step 5: drop the old DeviceGroup, found a new one, re-root its journal
   *  with the attestation. The handles are freed; the sync runtime loads
   *  the persisted snapshot when the session lands. */
  async function refoundDeviceGroupAfterReset(
    ctx: {
      readonly accountIdBytes: Uint8Array;
      readonly accountIdB64: string;
      readonly seed: Uint8Array;
      readonly deviceKey: CryptoKey;
    },
    attestationStdB64: string,
  ): Promise<void> {
    try {
      await deleteDeviceGroupSnapshot(ctx.accountIdB64);
    } catch (err) {
      // eslint-disable-next-line no-console
      console.warn("deleteDeviceGroupSnapshot before the re-founding failed", err);
    }
    const deviceSeed = await ensureDeviceIdentitySeed(ctx.accountIdB64, ctx.deviceKey);
    try {
      let ensured;
      try {
        ensured = await ensureDeviceGroup(
          ctx.accountIdBytes,
          ctx.accountIdB64,
          deviceSeed,
          ctx.deviceKey,
          ctx.seed,
          { keyResetAttestB64: attestationStdB64 },
        );
      } catch (err) {
        const status = (err as { status?: unknown }).status;
        const outcome = deviceGroupResetOutcome({
          status: typeof status === "number" ? status : 0,
          errorCode: typeof (err as { body?: unknown }).body === "string"
            ? errorCodeOf((err as { body: string }).body)
            : null,
          probeConfirmedRoot: null,
        });
        if (outcome === "refused") throw new KeyedError("recovery_phrase.fresh.publish_refused");
        throw err;
      }
      try {
        ensured.group.free();
        ensured.identity.free();
      } catch {
        /* already freed */
      }
    } finally {
      deviceSeed.fill(0);
    }
  }

  /**
   * Verify the key-reset attestation against the attest key this build
   * trusts before using it (contract section 5 step 6, defence in depth:
   * discovery and the DS verify it again under their own pin). Pinned
   * (`VITE_LAKKO_DEVICE_ATTEST_KEY`): a failure refuses, fail-closed. Not
   * pinned: the JWKS key is used, which trusts the auth origin that minted
   * the attestation (the ADR-0037 F1 posture), and when it cannot be fetched
   * the local check is skipped with a warning rather than stranding a reset
   * the servers will verify themselves.
   */
  async function verifyKeyResetAttestationLocally(
    wasm: Wasm,
    attestation: Uint8Array,
    accountIdBytes: Uint8Array,
    newPubkey: Uint8Array,
  ): Promise<void> {
    const pinned = pinnedDeviceAttestKey();
    let key: Uint8Array | null = pinned;
    if (key === null) {
      // eslint-disable-next-line no-console
      console.warn("no pinned attest key (VITE_LAKKO_DEVICE_ATTEST_KEY); verifying the reset attestation against the served JWKS key");
      try {
        key = await fetchDeviceAttestKey();
      } catch (err) {
        // eslint-disable-next-line no-console
        console.warn("attest key unavailable; the local attestation check is skipped (the servers verify it)", err);
        return;
      }
    }
    try {
      const verified = wasm.verifyKeyResetAttestation(attestation, key, accountIdBytes, newPubkey);
      verified.free();
    } catch (err) {
      // eslint-disable-next-line no-console
      console.error("key-reset attestation did not verify locally; refusing to publish it", err);
      throw new KeyedError("recovery_phrase.fresh.publish_refused");
    }
  }

  // -------------------------------------------------------------------
  // ADR-0065 section 3.5: the old device signs out and wipes
  // -------------------------------------------------------------------

  /** The name to show on the signed-out screen. */
  function signedOutUsername(accountIdB64: string): string {
    const s = state();
    if (s.kind !== "anon" && s.kind !== "loading" && s.account.accountId === accountIdB64) {
      return s.account.username;
    }
    const cached = loadPersisted();
    return cached !== null && cached.accountIdB64 === accountIdB64 ? cached.username : "";
  }

  /**
   * Stop every writer and poll loop, show the blocking screen, then run the
   * full local wipe (`wipe-account.ts`) with no call that needs a session or
   * the old root and nothing published first: the KeyPackage delete and the
   * wake-binding release that Forget device makes are skipped (the reset
   * already deleted the row; the old root is a stranger to the re-founded
   * journal). The push-gw rows are unsubscribed (anonymous tokens, best
   * effort), because nothing else can tear them down (G-36).
   */
  async function wipeAfterSignOut(accountIdB64: string): Promise<void> {
    dropSession();
    // ADR-0042, "A wake is shown only for the signed-in account": the local
    // service-worker wake binding goes first (no network), so every wake
    // from here on is dropped and this account's notifications leave the
    // screen with it.
    await releaseWakeBinding();
    clearRestored();
    setSignupPhrase(null);
    bindContactsAccount(null);
    chats.clear();
    keyPackageAccount = null;
    setIdentitySecrets(null);
    setNewPasswordOwed(false);
    setRestoreNotice(null);
    setPendingRecovery(null);
    const fresh = pendingFreshPhrase();
    if (fresh !== null) {
      fresh.seed.fill(0);
      setPendingFreshPhrase(null);
    }
    await wipeAccountLocalData(accountIdB64);
    try {
      await unsubscribeFromPush();
    } catch {
      // best-effort; the localStorage pseudonym is dropped regardless.
    }
    anonTokens.forgetAccount(accountIdB64);
    setState({ kind: "anon" });
  }

  /** A `401 key-generation-superseded`, or a landing that found a higher
   *  generation published: the screen, then the automatic wipe. Once per
   *  account per page. */
  async function signOutAfterKeyReset(accountIdB64: string): Promise<void> {
    const current = signedOut();
    if (current !== null && current.reason === "keys-reset") return;
    const username = signedOutUsername(accountIdB64);
    // eslint-disable-next-line no-console
    console.warn("the account's keys were reset on another device; this device is signed out and wiped");
    setSignedOut({ reason: "keys-reset", phase: "removing", username });
    try {
      await wipeAfterSignOut(accountIdB64);
    } finally {
      setSignedOut({ reason: "keys-reset", phase: "removed", username });
    }
  }

  /** A different key at the same generation (ADR-0065 4.2 rule 4): block,
   *  wipe on the tap. */
  function blockOnIdentityMismatch(accountIdB64: string, username: string): void {
    if (signedOut() !== null) return;
    dropSession();
    chats.clear();
    keyPackageAccount = null;
    setIdentitySecrets(null);
    setSignedOut({ reason: "identity-mismatch", phase: "blocked", username });
    mismatchAccount = accountIdB64;
  }
  let mismatchAccount: string | null = null;

  /** `refresh-invalid`: the session ended on its own. The sign-in screen
   *  with the session-expired line; nothing wiped. */
  function endExpiredSession(): void {
    if (signedOut() !== null) return;
    const cached = loadPersisted();
    api.logout();
    setState({
      kind: "anon",
      sessionExpired: true,
      ...(cached !== null ? { cachedUsername: cached.username } : {}),
    });
  }

  async function finishCanonicalSession(args: {
    wasm: Wasm;
    accountIdentitySeed: Uint8Array;
    accountIdBytes: Uint8Array;
    accountIdB64: string;
    exportKey: Uint8Array;
    session: SessionPair;
    username: string;
    silentRestore?: boolean;
  }): Promise<void> {
    const canonical = args.wasm.Identity.fromSeed(args.accountIdentitySeed);
    // The keys are on this device now; nothing is left to explain.
    setRestoreNotice(null);
    await installSessionIdentities(args.wasm, canonical, args.accountIdB64);
    keyPackageAccount = {
      accountIdBytes: args.accountIdBytes,
      accountIdB64: args.accountIdB64,
    };
    installSession(args.accountIdB64, args.session);

    // Rehydrate BEFORE any KeyPackage is minted: `restoreStorage` REPLACES the
    // provider storage, so a KeyPackage created earlier would lose its private
    // key to it.
    await rehydrateMlsState(chats, args.accountIdB64);

    await restoreOwedSignupPhrase(args.accountIdB64);
    settleOwedKeyResetPublish(args.accountIdB64, args.accountIdBytes, args.accountIdentitySeed);

    const secrets: IdentitySecrets = {
      accountId: args.accountIdBytes,
      exportKey: args.exportKey,
      accountIdentitySeed: args.accountIdentitySeed,
      sessionToken: args.session.sessionToken,
      sessionExpiresAtMs: args.session.sessionExpiresAtMs,
      keyGeneration: args.session.keyGeneration,
    };
    const checkPublished = (skipUploadIfUnchanged: boolean): Promise<LandingVerdict> =>
      refreshPublishedIdentity(
        canonical,
        args.accountIdBytes,
        args.accountIdB64,
        args.username,
        args.session.keyGeneration,
        skipUploadIfUnchanged,
      );

    if (args.silentRestore === true) {
      setIdentitySecrets(secrets);
      setPendingRecovery(null);
      setState({
        kind: "session",
        account: { accountId: args.accountIdB64, username: args.username },
      });
      void (async (): Promise<void> => {
        // ADR-0065 section 4.2: the published identity is read BEFORE any
        // upload, a reload included; a stale device shows the signed-out
        // screen over the session that just landed.
        const verdict = await checkPublished(true);
        if (chats.identity() !== canonical || landingSignsOut(verdict)) return;
        // A KeyPackage on every session start, reload included: the one the
        // directory serves may have been consumed by a Welcome this device
        // processed in an earlier page lifetime.
        await publishKeyPackageBestEffort("session restore");
      })();
      return;
    }

    const verdict = await checkPublished(false);
    if (landingSignsOut(verdict)) {
      // The signed-out screen is up; this device's keys are not the
      // account's. Nothing lands.
      return;
    }
    // Create -> durable persist -> upload (see `chat/keypackage-publisher.ts`).
    // The persist inside it also captures the rehydrated state, which is what
    // the separate post-login snapshot write used to do.
    await publishKeyPackageBestEffort("sign-in");

    setIdentitySecrets(secrets);
    setPendingRecovery(null);
    setState({
      kind: "session",
      account: { accountId: args.accountIdB64, username: args.username },
    });
    void subscribeToPush(args.accountIdB64).catch((err) => {
      // eslint-disable-next-line no-console
      console.warn("push subscribe failed", err);
    });
  }

  /**
   * ADR-0018 ACCESS-only recovery landing (factor (c) TOTP). The caller
   * has already verified
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
   *     RestoreIdentityPrompt offers another device / a passkey / the
   *     phrase.
   *
   * Shared by every ACCESS-only route: the authenticator code, a passkey,
   * and the sign-in that follows the local three-word check
   * (`recoverWithThreeWords`). "This device holds the keys"
   * is decided here and nowhere else: a stored device key that decrypts a
   * cached AccountIdentity seed.
   *
   * `restoreKeys` is the passkey route's hook for the COLD case: it runs
   * after the recovery context is parked and before the restore prompt is
   * shown, and returns `true` when it landed the session itself (the
   * passkey carried the keys).
   */
  async function landAfterAccessRecovery(
    username: string,
    accountIdBytes: Uint8Array,
    accountIdB64: string,
    session: SessionPair,
    restoreKeys?: () => Promise<boolean>,
  ): Promise<void> {
    // Install the fresh session everywhere a request draws on it.
    installSession(accountIdB64, session);
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
      // Refresh the persisted session pair under the existing device key.
      // Best-effort, same posture as login.
      await persistSessionPair(accountIdB64, session, "access recover");
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
      // identity directly. These factors involve no password, so the
      // export key is whatever an earlier password sign-in cached on this
      // device, else empty (the completeFromDeviceRestore convention).
      let cachedExportKey: Uint8Array | null = null;
      try {
        cachedExportKey = await getExportKey(accountIdB64, deviceKey);
      } catch (err) {
        // eslint-disable-next-line no-console
        console.warn("export-key cache decrypt failed (access recover)", err);
      }
      const wasm = await loadWasm();
      await finishCanonicalSession({
        wasm,
        accountIdentitySeed,
        accountIdBytes,
        accountIdB64,
        exportKey: cachedExportKey ?? new Uint8Array(0),
        session,
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
      session,
      username,
    });
    if (restoreKeys !== undefined && (await restoreKeys())) return;
    setState({
      kind: "needs-recovery-phrase",
      account: { accountId: accountIdB64, username },
    });
  }

  /**
   * What "Three words from your phrase" needs, all of it local: the account
   * cached on this device, its username, and the phrase its device key
   * opens. `null` when the route is not offered (`offersThreeWords`): no
   * cached account, no username to address the sign-in by, no stored
   * phrase, or no stored keys (the sign-in would only reach the restore
   * prompt).
   */
  async function threeWordsContext(): Promise<{
    accountIdB64: string;
    username: string;
    phrase: string;
  } | null> {
    const cached = loadPersisted();
    if (cached === null) return null;
    let deviceKey: CryptoKey | null = null;
    try {
      deviceKey = await getDeviceKey(cached.accountIdB64);
    } catch {
      deviceKey = null;
    }
    const phrase =
      deviceKey === null ? null : await readDevicePhraseWith(cached.accountIdB64, deviceKey);
    let keysOnDevice = false;
    if (deviceKey !== null) {
      try {
        const seed = await getAccountIdentitySeed(cached.accountIdB64, deviceKey);
        keysOnDevice = seed !== null;
        seed?.fill(0);
      } catch {
        keysOnDevice = false;
      }
    }
    if (
      phrase === null ||
      !offersThreeWords({
        cachedUsername: cached.username,
        phraseOnDevice: true,
        keysOnDevice,
        suspended: isThreeWordsSuspended(plainStorage(), cached.accountIdB64),
      })
    ) {
      return null;
    }
    return { accountIdB64: cached.accountIdB64, username: cached.username, phrase };
  }

  /** Run the passkey ceremony (discoverable when `accountIdB64` is absent,
   *  else limited to that account's passkeys), asking for the PRF
   *  evaluation that protects the keys. Failures are rethrown as catalog
   *  keys. */
  async function runPasskeyCeremony(
    accountIdB64?: string,
  ): Promise<Awaited<ReturnType<WebauthnClient["loginWithPasskey"]>>> {
    const webauthn = new WebauthnClient();
    try {
      return await webauthn.loginWithPasskey(
        accountIdB64 === undefined
          ? { prfSalt: PASSKEY_PRF_SALT }
          : { accountIdB64, prfSalt: PASSKEY_PRF_SALT },
      );
    } catch (err) {
      if (err instanceof WebauthnUserCancelledError) {
        throw new KeyedError("auth.passkey.error.cancelled");
      }
      if (err instanceof WebauthnUnsupportedError) {
        throw new KeyedError("auth.passkey.error.unsupported");
      }
      if (err instanceof WebauthnHttpError) {
        if (err.status === 503) {
          throw new KeyedError("auth.passkey.error.server_disabled");
        }
        if (err.status === 404) {
          throw new KeyedError("auth.passkey.error.not_registered");
        }
        throw new KeyedError("auth.passkey.error.failed_status", { status: String(err.status) });
      }
      if (err instanceof WebauthnNetworkError) {
        throw new KeyedError("auth.passkey.error.network");
      }
      throw err;
    }
  }

  /**
   * Ask a passkey for the account's keys, while a recovery context is
   * parked (ADR-0064 decision 6). The PRF output opens the blob stored for
   * the credential into the 12-word phrase, and the phrase goes through
   * `provideRecoveryPhrase`, whose check against the published identity is
   * fail-closed. `restored` means the session landed. The PRF output stays
   * on this device.
   *
   * Throws only what is neither "no keys" nor "unusable": the vault fetch
   * failing, or the published identity being unreachable.
   */
  async function keysFromPasskey(
    assertion: {
      readonly credentialId: Uint8Array;
      readonly prfOutput: Uint8Array | null;
    },
    sessionToken: string,
  ): Promise<PasskeyKeysOutcome> {
    let blobStored = false;
    let phrase: string | null = null;
    if (assertion.prfOutput !== null) {
      try {
        phrase = await openPasskeyVault({
          wasm: await loadWasm(),
          client,
          sessionToken,
          credentialId: assertion.credentialId,
          prfOutput: assertion.prfOutput,
        });
        blobStored = phrase !== null;
      } catch (err) {
        if (!(err instanceof PasskeyVaultUnusableError)) throw err;
        // eslint-disable-next-line no-console
        console.warn("the keys stored for this passkey did not open");
        blobStored = true;
      }
    }
    let matched = false;
    if (phrase !== null) {
      try {
        await api.provideRecoveryPhrase(phrase);
        matched = true;
      } catch (err) {
        if (!(err instanceof SupersessionPhraseMismatch)) throw err;
        // eslint-disable-next-line no-console
        console.warn("the keys stored for this passkey are not this account's");
      }
    }
    return passkeyKeysOutcome({
      prfOutput: assertion.prfOutput !== null,
      blobStored,
      opened: phrase !== null,
      matched,
    });
  }

  /** `hint` when discovery resolves it to `accountIdBytes`, else the empty
   *  name. A passkey assertion carries only the account id, and a typed name
   *  must not be saved for an account it does not name. Runs with the
   *  session already installed (the lookup spends a token drawn with it). */
  async function usernameNamingAccount(
    hint: string | undefined,
    accountIdBytes: Uint8Array,
  ): Promise<string> {
    const typed = (hint ?? "").trim().toLowerCase();
    if (typed.length === 0) return "";
    try {
      const found = await discoveryAccountLookup(discovery)(typed);
      return found !== null && bytesEqual(found, accountIdBytes) ? typed : "";
    } catch (err) {
      // eslint-disable-next-line no-console
      console.warn("could not check the typed name against the passkey's account", err);
      return "";
    }
  }

  onMount(() => {
    // A reload restores the whole session from what this browser already
    // holds under the device key — session token, AccountIdentity seed, MLS
    // snapshot — or lands in `anon`. Never the old half state (`session` with
    // no secrets in memory), which rendered a "re-enter your password" panel
    // whose link `/login` bounced straight back home. The decision rule and
    // the security trade-off live in `silent-restore.ts`.
    //
    // The state stays `loading` until this settles, so no route renders a
    // signed-out or half-signed-in view in between.
    void restoreSessionOnLoad();
  });

  async function restoreSessionOnLoad(): Promise<void> {
    const probe = await probeStoredSecrets();
    const decision = decideSilentRestore({
      cachedAccount: probe.cachedAccount,
      deviceKeyPresent: probe.deviceKey !== null,
      sessionPair: probe.sessionPair,
      identitySeedPresent: probe.identitySeed !== null,
      nowMs: Date.now(),
    });
    // An explicit sign-in that completed while the probe was reading IDB
    // owns the state now; a late restore must not overwrite it.
    if (state().kind !== "loading") {
      probe.identitySeed?.fill(0);
      return;
    }
    if (decision.kind === "anon") {
      setState({ kind: "anon" });
      return;
    }
    if (
      decision.kind === "anon-cached" ||
      probe.deviceKey === null ||
      probe.identitySeed === null
    ) {
      probe.identitySeed?.fill(0);
      // (The handle checks only narrow the types: a `restore` decision is
      // made only when both are present.)
      setState({ kind: "anon", cachedUsername: decision.username });
      return;
    }
    const seed = probe.identitySeed;
    // ADR-0065 section 3.3: a reload refreshes with the credential instead
    // of reusing the persisted access token. `superseded` and `invalid` are
    // handled by the provider's listener (the signed-out screen and wipe /
    // the sign-in screen); a network failure keeps the persisted pair, and
    // the app opens on local data with whatever it still allows.
    explicitSignInThisPage = false;
    sessionTokens.install(decision.accountIdB64, decision.sessionPair);
    const refresh = await sessionTokens.refreshOnce();
    if (refresh === "superseded" || refresh === "invalid") {
      seed.fill(0);
      return;
    }
    const session = sessionTokens.pair() ?? decision.sessionPair;
    try {
      // The cached export key is an auth-factor cache only (ADR-0017); it is
      // restored when present so the in-memory secrets match what a password
      // sign-in on this device would hold, and left empty otherwise — the
      // ACCESS-only convention the recovery landings already use.
      let exportKey: Uint8Array = new Uint8Array(0);
      try {
        exportKey =
          (await getExportKey(decision.accountIdB64, probe.deviceKey)) ?? exportKey;
      } catch {
        exportKey = new Uint8Array(0);
      }
      const wasm = await loadWasm();
      await finishCanonicalSession({
        wasm,
        accountIdentitySeed: seed,
        accountIdBytes: base64Decode(decision.accountIdB64),
        accountIdB64: decision.accountIdB64,
        exportKey,
        session,
        username: decision.username,
        silentRestore: true,
      });
    } catch (err) {
      // Never a half state: undo whatever the restore installed and land
      // signed out, with the username pre-filled for `/login`.
      // eslint-disable-next-line no-console
      console.warn("session restore on load failed; signing out", err);
      keyPackageAccount = null;
      chats.clear();
      setIdentitySecrets(null);
      dropSession();
      seed.fill(0);
      setState({ kind: "anon", cachedUsername: decision.username });
    }
  }

  // The "publish owed" facts of the parked fresh phrase, re-read from the
  // record on every access (the record moves as steps land).
  const [owedVersion, setOwedVersion] = createSignal(0);
  const freshOwedRecord = (): KeyResetOwed | null => {
    owedVersion();
    const pending = pendingFreshPhrase();
    if (pending === null) return null;
    const record = readKeyResetOwed(plainStorage(), pending.accountIdB64);
    return record !== null && remainingKeyResetSteps(record.done).length > 0 ? record : null;
  };

  const api: AuthApi = {
    state,
    identitySecrets,
    freshPhrase: () => pendingFreshPhrase()?.phrase ?? null,
    freshPublishOwed: () => freshOwedRecord() !== null,
    freshPublishSignInNeeded: () => {
      const record = freshOwedRecord();
      return (
        record !== null &&
        attestationGate(record.signedAtMs, Date.now(), explicitSignInThisPage) === "sign-in-needed"
      );
    },
    async retryFreshPublish() {
      const pending = pendingFreshPhrase();
      if (pending === null) {
        throw new Error("no pending fresh-identity phrase");
      }
      // The keys are reset: only the publishing steps are retried.
      try {
        await retryKeyResetPublish(
          pending.accountIdB64,
          pending.accountIdBytes,
          pending.seed,
          pending.deviceKey,
        );
      } finally {
        setOwedVersion((v) => v + 1);
      }
    },
    signupPhrase,
    acknowledgeSignupPhrase() {
      const s = state();
      if (s.kind === "session") {
        clearSignupPhraseOwed(
          typeof localStorage === "undefined" ? undefined : localStorage,
          s.account.accountId,
        );
      }
      setSignupPhrase(null);
    },
    signedOut,
    async confirmSignedOut() {
      const current = signedOut();
      if (current === null) return;
      if (current.reason === "identity-mismatch" && current.phase === "blocked") {
        const account = mismatchAccount ?? loadPersisted()?.accountIdB64 ?? null;
        setSignedOut({ ...current, phase: "removing" });
        try {
          if (account !== null) await wipeAfterSignOut(account);
          else setState({ kind: "anon" });
        } finally {
          setSignedOut({ ...current, phase: "removed" });
        }
        return;
      }
      if (current.phase === "removed") setSignedOut(null);
    },
    keyResetNote,
    dismissKeyResetNote: () => setKeyResetNote(false),
    republishKeyPackage: () => keyPackagePublisher.publish(),
    async signup(username, password, options) {
      const trimmed = username.trim();
      if (trimmed.length === 0) {
        throw new Error("username must be non-empty");
      }
      const result = await client.signup(
        utf8(password),
        options?.signal !== undefined ? { signal: options.signal } : {},
      );
      const accountIdB64 = base64Encode(result.accountId);
      explicitSignInThisPage = true;
      // Install the fresh session so the registerUsername /
      // uploadIdentityBundle / uploadKeyPackage round-trip below
      // sails through the auth::require_session middleware.
      installSession(accountIdB64, result.session);
      // The password was typed just now: the signup finish mints a
      // fresh-auth grant, so the passkey offer of the onboarding asks for
      // nothing more.
      sessionLanding = "password";
      if (result.freshGrant !== null) freshGrants.put(accountIdB64, result.freshGrant);
      else freshGrants.clear();

      // 1.16d2 + ADR-0054 S5: claim the username server-side, AFTER the
      // OPAQUE flow commits, in the load-bearing order: bind the login
      // handle at auth FIRST, then register the name at discovery. An auth
      // 409 stops before discovery; a discovery failure releases the handle
      // again (so a retry is not refused by our own half claim). Either
      // "taken" surfaces as the form's taken copy.
      const handle = await loginHandleFor(trimmed);
      const reg = await claimUsername({
        username: trimmed,
        bind: async () => {
          await client.bindLoginHandle(result.session.sessionToken, handle.wire);
          loginHandleSync.markBound(result.session.sessionToken, handle.wire);
        },
        register: () => discovery.registerUsername(result.accountId, trimmed),
        release: async () => {
          await client.releaseLoginHandle(result.session.sessionToken);
        },
      });
      const normalisedUsername = reg.normalisedUsername;

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
      // Persist the session pair under the device key so a returning user
      // resumes without re-typing the password. Best-effort: a persistence
      // failure does not block the rest of signup; the in-memory pair still
      // serves this page.
      try {
        await putSessionPair(accountIdB64, result.session, deviceKey);
      } catch (err) {
        // eslint-disable-next-line no-console
        console.warn("session-pair persist (signup) failed", err);
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
      await installSessionIdentities(wasm, canonical, accountIdB64);
      keyPackageAccount = { accountIdBytes: result.accountId, accountIdB64 };

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
      // ADR-0036: also mint the auth-attested genesis (anti-squatting).
      // Soft-fail — the append accepts the self-sig alone while the
      // operator flag is default-OFF, so this never blocks signup.
      const signupGenesisAttest = await mintGenesisAttestation(
        client,
        result.session.sessionToken,
        canonical,
      );
      await uploadIdentityBundle(
        discovery,
        result.accountId,
        canonical,
        undefined,
        signupLeafAuth,
        signupGenesisAttest,
      );

      // The first KeyPackage: create -> durable persist -> upload, so a
      // published package always has its private key on disk and a peer's
      // Welcome can be opened after a reload. Never fatal — the account
      // already exists server-side, so failing the signup here would strand
      // a username the user can no longer claim; the next session start (or
      // Welcome) republishes.
      await publishKeyPackageBestEffort("signup");

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
        sessionToken: result.session.sessionToken,
        sessionExpiresAtMs: result.session.sessionExpiresAtMs,
        keyGeneration: result.session.keyGeneration,
      });
      // The phrase becomes pending BEFORE the session lands, so the route
      // guards never see a signed-in state without it: `/signup` sits in
      // `<RequireAnonymous>`, which used to navigate home on this very
      // transition and unmount the phrase panel before it rendered. The owed
      // marker keeps the screen coming back across a reload until the user
      // acknowledges it (see `phrase-gate.ts`).
      // A device that holds the keys holds the phrase (the local three-word
      // check, the hand-over to a new device, "Show recovery phrase").
      // Awaited so the owed-phrase reload below can always read it back.
      await storeDevicePhrase(accountIdB64, result.recoveryPhrase, deviceKey, "signup");
      markSignupPhraseOwed(
        typeof localStorage === "undefined" ? undefined : localStorage,
        accountIdB64,
      );
      setSignupPhrase(result.recoveryPhrase);
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
          sessionToken: result.session.sessionToken,
        },
        result.recoveryPhrase,
      );
      // 1.20b: best-effort push subscribe. The orchestrator is
      // a no-op when the build has no VAPID public key set or the
      // user denies the Notification permission prompt — fall
      // back to foreground polling without throwing.
      void subscribeToPush(accountIdB64).catch((err) => {
        // eslint-disable-next-line no-console
        console.warn("push subscribe (signup) failed", err);
      });
      return { recoveryPhrase: result.recoveryPhrase };
    },
    async signupGuest(options) {
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
      const result = await client.signup(
        utf8(randomPassword),
        options?.signal !== undefined ? { signal: options.signal } : {},
      );
      const accountIdB64 = base64Encode(result.accountId);
      explicitSignInThisPage = true;
      // Install the fresh session for the protected writes below.
      installSession(accountIdB64, result.session);

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
        await putSessionPair(accountIdB64, result.session, deviceKey);
      } catch (err) {
        // eslint-disable-next-line no-console
        console.warn("session-pair persist (signupGuest) failed", err);
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
      await storeDevicePhrase(accountIdB64, result.recoveryPhrase, deviceKey, "signup-guest");
      // A reload before the phrase is acknowledged lands a session from the
      // stored keys; the owed marker makes that session show the phrase
      // first (it is read back from the device).
      markSignupPhraseOwed(plainStorage(), accountIdB64);

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
        // ADR-0036: auth-attested genesis alongside the self-sig
        // (soft-fail — never blocks the guest signup).
        const guestGenesisAttest = await mintGenesisAttestation(
          client,
          result.session.sessionToken,
          canonical,
        );
        await uploadIdentityBundle(
          discovery,
          result.accountId,
          canonical,
          undefined,
          guestLeafAuth,
          guestGenesisAttest,
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
        session: result.session,
        username: "",
        phrase: result.recoveryPhrase,
        deviceKey,
      });
      setState({
        kind: "save-fresh-phrase",
        account: { accountId: accountIdB64, username: "" },
      });
      return { recoveryPhrase: result.recoveryPhrase };
    },
    async login(username, password) {
      // ADR-0054 S5: sign in by name. The start carries the login handle
      // of the typed name, never an account id, and nothing is looked up
      // before the session exists: the account id comes back in the finish.
      // An unknown name reads as a wrong password (the server's stand-in).
      // The handle is re-bound once the session exists (idempotent).
      const target = await nameTargetFor(username);
      const resolvedUsername = target.username;
      // A password sign-in owes no new-password step and explains nothing.
      setNewPasswordOwed(false);
      setRestoreNotice(null);
      const result = await accessByName({
        target,
        run: (handle) => client.login(handle, utf8(password)),
        bind: bindByName,
      });
      const accountIdBytes = result.accountId;
      const accountIdB64 = base64Encode(accountIdBytes);
      explicitSignInThisPage = true;
      // Install the fresh session for any protected writes that follow.
      installSession(accountIdB64, result.session);
      // The password was proven just now: keep the fresh-auth grant (memory
      // only). A sign-in that brought none drops whatever an earlier one
      // left.
      sessionLanding = "password";
      if (result.freshGrant !== null) freshGrants.put(accountIdB64, result.freshGrant);
      else freshGrants.clear();

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
      // Refresh the persisted session pair alongside the device key. Same
      // best-effort posture as signup: a persistence failure does not block
      // login.
      try {
        await putSessionPair(accountIdB64, result.session, deviceKey);
      } catch (err) {
        // eslint-disable-next-line no-console
        console.warn("session-pair persist (login) failed", err);
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
          session: result.session,
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
        session: result.session,
        username: resolvedUsername,
      });
    },
    async loginWithPasskey(usernameHint, options) {
      // ADR-0064 decision 6 / UX spec section 4. Discoverable: no account
      // has to be cached on this device and no name typed. The assertion
      // grants ACCESS; what happens next is the shared ACCESS-only landing,
      // with one addition on a device without the keys: this passkey is
      // asked for them before the restore prompt is shown.
      const assertion = await runPasskeyCeremony();
      const accountIdB64 = assertion.account_id_b64;
      const accountIdBytes = base64Decode(accountIdB64);
      const assertionSession = sessionPairOf(assertion);
      setRestoreNotice(null);
      explicitSignInThisPage = true;
      installSession(accountIdB64, assertionSession);
      const cached = loadPersisted();
      const username =
        cached !== null && cached.accountIdB64 === accountIdB64
          ? cached.username
          : await usernameNamingAccount(usernameHint, accountIdBytes);
      // A passkey sign-in is a fresh-auth proof like a password one.
      sessionLanding = "passkey";
      const passkeyGrant = freshGrantOf(assertion);
      if (passkeyGrant !== null) freshGrants.put(accountIdB64, passkeyGrant);
      else freshGrants.clear();
      if (options?.forgotPassword === true) setNewPasswordOwed(true);
      await landAfterAccessRecovery(
        username,
        accountIdBytes,
        accountIdB64,
        assertionSession,
        async () => {
          try {
            const outcome = await keysFromPasskey(assertion, assertionSession.sessionToken);
            if (outcome === "restored") return true;
            setRestoreNotice(outcome === "no-keys" ? "passkey-no-keys" : "passkey-unusable");
          } catch (err) {
            // The vault or the directory could not be reached. The person
            // is signed in; the restore prompt's passkey row retries.
            // eslint-disable-next-line no-console
            console.warn("asking the passkey for the keys failed", err);
          }
          return false;
        },
      );
    },
    async restoreKeysWithPasskey() {
      const pending = pendingRecovery();
      if (pending === null) {
        throw new KeyedError("restore_identity.error.nothing");
      }
      const assertion = await runPasskeyCeremony(pending.accountIdB64);
      if (assertion.account_id_b64 !== pending.accountIdB64) {
        throw new KeyedError("auth.passkey.error.wrong_account");
      }
      const outcome = await keysFromPasskey(assertion, assertion.session_token);
      if (outcome !== "restored") {
        setRestoreNotice(outcome === "no-keys" ? "passkey-no-keys" : "passkey-unusable");
      }
      return outcome;
    },
    restoreNotice,
    pendingSessionToken: () => pendingRecovery()?.session.sessionToken ?? null,
    newPasswordOffered,
    dismissNewPasswordOffer() {
      setNewPasswordOwed(false);
    },
    canReplacePassword,
    async replacePassword(newPassword) {
      // UX spec 7.1. `/password/reset/*` is authorised by a recovery OPAQUE
      // proof made from the 64-byte BIP39 seed (`AuthClient.recover`). This
      // device holds that seed (cached under its device key, in memory for
      // the session), so the reset runs with no typed phrase.
      //
      // Deliberately NOT `recover`: that path is the fresh-device one, which
      // re-derives the device key from the new export key and drops every
      // store wrapped under the old one. Here the stored device key is kept
      // (`login` prefers a stored key over re-deriving), so the chats, the
      // cached seed and the cached phrase stay readable; only the session
      // token and the export-key cache are refreshed.
      const s = state();
      const secrets = identitySecrets();
      if (s.kind !== "session" || secrets === null || !canReplacePassword()) {
        throw new KeyedError("common.error.session_not_ready");
      }
      // UX 17.8: the server authorises this by the keys the device holds,
      // so the bare session would do. This client does not allow it: the
      // person proves the current password or a passkey, or checks three
      // words of the recovery phrase, first.
      if (!api.passwordChangeAllowed()) throw new FreshAuthRequiredError();
      const accountIdB64 = s.account.accountId;
      const target = await nameTargetFor(s.account.username);
      // `AuthClient.recover` uses the seed as the OPAQUE recovery input; hand
      // it a copy so the in-memory secret is never the buffer that is wiped.
      const seed = secrets.accountIdentitySeed.slice();
      try {
        const result = await accessByName({
          target,
          run: (handle) => client.recover(handle, seed, utf8(newPassword)),
          bind: bindByName,
        });
        if (!bytesEqual(result.accountId, secrets.accountId)) {
          // The name's handle answered for another account. The proof was
          // made from this account's seed, so this cannot verify; refuse
          // rather than adopt a session for an account that is not ours.
          throw new Error("internal: the password reset answered for another account");
        }
        installSession(accountIdB64, result.session);
        try {
          const deviceKey = await getDeviceKey(accountIdB64);
          if (deviceKey !== null) {
            await putSessionPair(accountIdB64, result.session, deviceKey);
            await putExportKey(accountIdB64, result.exportKey, deviceKey);
          }
        } catch (err) {
          // eslint-disable-next-line no-console
          console.warn("persisting the session after a password change failed", err);
        }
        setIdentitySecrets({
          accountId: secrets.accountId,
          exportKey: result.exportKey,
          accountIdentitySeed: secrets.accountIdentitySeed,
          sessionToken: result.session.sessionToken,
          sessionExpiresAtMs: result.session.sessionExpiresAtMs,
          keyGeneration: result.session.keyGeneration,
        });
        setNewPasswordOwed(false);
        // A new password ends what the old proof and the phrase check
        // stood for.
        freshGrants.clear();
        phraseCheckedAtMs = null;
      } finally {
        seed.fill(0);
      }
    },
    async provideRecoveryPhrase(mnemonicPhrase) {
      markRestored("phrase");
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
      // The canonical form (12 lower-case words, single spaces), which is
      // what the device stores once the check below has passed.
      const canonicalPhrase = mnemonic.phrase;
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
      // Strictly after the fail-closed check: the phrase is the account's.
      await storeDevicePhrase(pending.accountIdB64, canonicalPhrase, deviceKey, "provide-phrase");
      // On a bootstrapped key the pending session token was never
      // persisted (there was no key at park time) — land it now so a
      // reload re-attaches. Best-effort.
      if (bootstrappedDeviceKey) {
        try {
          await putSessionPair(pending.accountIdB64, pending.session, deviceKey);
        } catch (err) {
          // eslint-disable-next-line no-console
          console.warn(
            "session-pair persist (provideRecoveryPhrase) failed",
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
        session: pending.session,
        username: pending.username,
      });
    },
    async completeFromPairing(phraseEntropy, session) {
      markRestored("device");
      // ADR-0018 factor (a), path (a) — same precondition as
      // `provideRecoveryPhrase`: only valid while a login/passkey
      // auth is parked in the `needs-recovery-phrase` fail-safe.
      const pending = pendingRecovery();
      if (pending === null) {
        phraseEntropy.fill(0);
        throw new Error("no pending identity recovery — sign in first");
      }
      const wasm = await loadWasm();
      // Recovery payload v2: rebuild the phrase and the 64-byte seed from
      // the transferred entropy (which is zeroized here). Nothing is
      // written before the check below.
      const { phrase: transferredPhrase, seed: phraseSeed } = phraseFromTransferredEntropy(
        wasm,
        phraseEntropy,
      );
      // SAME fail-closed verification as the typed-phrase path
      // (ADR-0017 + lakko-security finding #1): the transferred seed
      // must reproduce the account's PUBLISHED AccountIdentity key;
      // hard-block + zeroize on mismatch or an unverifiable bundle.
      await verifySeedAgainstPublishedIdentity({
        wasm,
        discovery,
        accountIdBytes: pending.accountIdBytes,
        accountIdentitySeed: phraseSeed,
        source: "device",
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
      // This device now holds the phrase like the one that confirmed it.
      await storeDevicePhrase(pending.accountIdB64, transferredPhrase, deviceKey, "device-pairing");

      // Prefer the redeemed fresh session when the pairing carried one; the
      // pending login's otherwise.
      let effective = pending.session;
      if (session !== undefined) {
        effective = session;
        installSession(pending.accountIdB64, session);
      }
      // Persist the effective pair when a fresh one arrived (the
      // pre-existing behavior) OR when the key was just bootstrapped
      // (the pending pair was never persisted — there was no key at park
      // time). Best-effort.
      if (session !== undefined || bootstrappedDeviceKey) {
        try {
          await putSessionPair(pending.accountIdB64, effective, deviceKey);
        } catch (err) {
          // eslint-disable-next-line no-console
          console.warn(
            "session-pair persist (completeFromPairing) failed",
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
        session: effective,
        username: pending.username,
      });
    },
    async completeFromDeviceRestore(restoreArgs) {
      markRestored("device");
      // ADR-0018 factor (a), path (b) — NO-PASSWORD bootstrap. The
      // caller ran the anonymous pairing and holds the transferred
      // seed + the REDEEMED session token (the only ACCESS here).
      const {
        accountIdBytes,
        accountIdB64,
        username,
        phraseEntropy,
        session,
        persistJoinState,
        forgotPassword,
      } = restoreArgs;
      explicitSignInThisPage = true;
      const wasm = await loadWasm();
      // Recovery payload v2: rebuild the phrase and the 64-byte seed from
      // the transferred entropy (which is zeroized here). Nothing is
      // written before the gates below.
      const { phrase: transferredPhrase, seed: phraseSeed } = phraseFromTransferredEntropy(
        wasm,
        phraseEntropy,
      );
      // The persistence and the session landing; runs only after every
      // gate below passed.
      const landDeviceRestore = async (): Promise<void> => {
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
        // This device now holds the phrase like the one that confirmed it.
        await storeDevicePhrase(accountIdB64, transferredPhrase, deviceKey, "device-restore");
        // The redeemed pair is REQUIRED on this path — persist it so a
        // reload re-attaches without any credential. Best-effort.
        try {
          await putSessionPair(accountIdB64, session, deviceKey);
        } catch (err) {
          // eslint-disable-next-line no-console
          console.warn(
            "session-pair persist (completeFromDeviceRestore) failed",
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

        installSession(accountIdB64, session);
        setPendingRecovery(null);
        if (forgotPassword === true) setNewPasswordOwed(true);
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
          session,
          username,
        });
      };
      // FAIL CLOSED FIRST — before ANY persistence. A mismatched /
      // unverifiable seed must leave this browser untouched. Then
      // (ADR-0056 security follow-up) the typed name must resolve, at
      // discovery, to the account the grant redeem answered for: a
      // squatted auth login handle can route the request to another
      // account, and discovery is a registry auth does not control. The
      // lookup spends an anonymous token drawn with the redeemed session;
      // a mismatch, an unknown name or a lookup that cannot run refuses
      // with the seed zeroed and the session dropped, nothing persisted.
      await runNameCheckedDeviceRestore({
        username,
        accountIdBytes,
        phraseSeed,
        sessionToken: session.sessionToken,
        verifySeed: () =>
          verifySeedAgainstPublishedIdentity({
            wasm,
            discovery,
            accountIdBytes,
            accountIdentitySeed: phraseSeed,
            source: "device",
          }),
        attachSession: (token) => anonTokens.setSessionToken(token),
        dropSession: () => {
          dropSession();
          // The account's tokens stay stored for its next sign-in.
          anonTokens.unbind();
        },
        lookup: discoveryAccountLookup(discovery),
        land: landDeviceRestore,
      });
    },
    freshProofLive() {
      return api.freshProof() !== null;
    },
    freshProof() {
      const account = proofAccount();
      return account === null ? null : freshGrants.live(account.accountIdB64, Date.now());
    },
    dropFreshProof(grant) {
      freshGrants.spend(grant);
    },
    async proveWithPassword(password, username) {
      const account = proofAccount();
      if (account === null) throw new KeyedError("common.error.session_not_ready");
      if (password.length === 0) throw new FreshAuthRequiredError();
      const name = account.username.trim().length > 0 ? account.username : (username ?? "");
      if (name.trim().length === 0) throw new KeyedError("recovery.username_required");
      // Throws the username rule's copy for a name it refuses.
      const target = await nameTargetFor(name);
      let signedIn;
      try {
        // Not `accessByName`: nothing is re-bound.
        signedIn = await client.login(target.handle.wire, utf8(password));
      } catch (err) {
        const failure = proofLoginFailure({
          status: err instanceof AuthHttpError ? err.status : null,
          network: err instanceof AuthNetworkError,
        });
        if (failure === "wrong-password") throw new ProofWrongPasswordError();
        throw err;
      }
      if (base64Encode(signedIn.accountId) !== account.accountIdB64) {
        // The typed name is another account's: not this account's password.
        throw new ProofWrongPasswordError();
      }
      if (signedIn.freshGrant === null) throw new FreshAuthRequiredError();
      freshGrants.put(account.accountIdB64, signedIn.freshGrant);
      await adoptProofSession(account.accountIdB64, signedIn.session);
    },
    async proveWithPasskey() {
      const account = proofAccount();
      if (account === null) throw new KeyedError("common.error.session_not_ready");
      let assertion;
      try {
        assertion = await new WebauthnClient().loginWithPasskey({
          accountIdB64: account.accountIdB64,
        });
      } catch (err) {
        if (err instanceof WebauthnUserCancelledError) throw new ProofCancelledError();
        // A limit and the network keep their own shape for the block's
        // shared lines; everything else is "could not be checked".
        if (err instanceof WebauthnNetworkError) throw err;
        if (err instanceof WebauthnHttpError && err.status === 429) throw err;
        // eslint-disable-next-line no-console
        console.warn(
          "passkey proof failed",
          err instanceof WebauthnHttpError ? err.status : err instanceof Error ? err.name : "unknown",
        );
        throw new ProofPasskeyFailedError();
      }
      if (assertion.account_id_b64 !== account.accountIdB64) {
        // Another account's passkey answered: install nothing.
        throw new ProofOtherAccountError();
      }
      const grant = freshGrantOf(assertion);
      if (grant === null) throw new ProofPasskeyFailedError();
      freshGrants.put(account.accountIdB64, grant);
      await adoptProofSession(account.accountIdB64, sessionPairOf(assertion));
    },
    sessionLanding() {
      return sessionLanding;
    },
    async checkPhraseWords(positions, words) {
      const st = state();
      if (st.kind !== "session") throw new KeyedError("common.error.session_not_ready");
      const phrase = await readDevicePhrase(st.account.accountId);
      if (phrase === null) throw new KeyedError("recovery.words.error.unavailable");
      const wasm = await loadWasm();
      const mnemonic = wasm.RecoveryMnemonic.fromPhrase(phrase);
      try {
        // Trims and lower-cases; a wrong or unknown word is `false`.
        if (!wasm.phraseCheckWords(mnemonic, positions, [...words])) return false;
      } finally {
        mnemonic.free();
      }
      phraseCheckedAtMs = Date.now();
      return true;
    },
    passwordChangeAllowed() {
      return passwordChangeAllowed({
        proofLive: api.freshProofLive(),
        phraseCheckedAtMs,
        nowMs: Date.now(),
      });
    },
    async mintFreshIdentity() {
      // "Reset account keys" (ADR-0065 section 2.3, UX 15.1). Reachable ONLY
      // while a `pendingRecovery` is parked: the user recovered ACCESS via
      // an ADR-0018 factor and is sitting in `needs-recovery-phrase` with
      // NO phrase and NO other device. The explicit, typed-confirmation
      // button is the ONLY trigger; this method never runs automatically.
      const pending = pendingRecovery();
      if (pending === null) {
        throw new Error(
          "no pending identity recovery — sign in first",
        );
      }
      const wasm = await loadWasm();

      // 0. The proof (UX 17.2): the server refuses a reset without a
      // fresh-auth grant. The screen obtained one (password or passkey)
      // before calling this; without a live one nothing is sent.
      const grant = freshGrants.live(pending.accountIdB64, Date.now());
      if (grant === null) throw new FreshAuthRequiredError();

      // 1. A BRAND-NEW recovery phrase, its 64-byte BIP39 seed, and the
      // identity derived from it: a fresh random identity for the SAME
      // account. Nothing is persisted and nothing is sent until step 2.
      const mnemonic = wasm.RecoveryMnemonic.generate();
      const phrase = mnemonic.phrase;
      const accountIdentitySeed = mnemonic.toSeed();
      mnemonic.free();
      const canonical = wasm.Identity.fromSeed(accountIdentitySeed);
      const newPubkey = canonical.signaturePublicKey;
      canonical.free();

      // 2. The server reset: the point of no return. One transaction
      // server-side (the generation bump, the recovery credential, every
      // passkey and vault and grant deleted, the attestation, the new pair).
      // A failure here changes nothing anywhere: the generated phrase is
      // discarded and the person is back on the restore prompt.
      let reset;
      try {
        reset = await client.resetAccountKeys(
          pending.session.sessionToken,
          grant,
          accountIdentitySeed.slice(),
          newPubkey,
        );
      } catch (err) {
        accountIdentitySeed.fill(0);
        if (
          err instanceof AuthHttpError &&
          isFreshAuthRequired({ status: err.status, errorCode: err.errorCode })
        ) {
          // The grant expired (or was otherwise refused) on the way: nothing
          // was reset. Drop it; the screen asks for a proof.
          freshGrants.spend(grant);
          throw new FreshAuthRequiredError();
        }
        throw err;
      }
      // A successful reset spends every grant of the old generation, and the
      // pair it returns carries none.
      freshGrants.clear();

      // 3. Persist immediately, before any further network call: the new
      // pair (the old one is superseded by the same transaction), the seed
      // and the phrase under the device key, and the "publish owed" record
      // with the attestation. From here on a crash or a failure is a retry
      // of the publishing steps, never a second reset.
      installSession(pending.accountIdB64, reset.session);
      let deviceKey = pending.deviceKey;
      if (deviceKey === null) {
        // The ACCESS-only fresh-browser case (no password, hence no OPAQUE
        // export key): bootstrap a device key from the new seed, as
        // `provideRecoveryPhrase` / `completeFromDeviceRestore` do. When a
        // device key IS parked, reuse it: minting a new one would orphan
        // its wrapped stores.
        deviceKey = await deriveDeviceRootKeyFromIdentitySeed(
          accountIdentitySeed,
          pending.accountIdB64,
        );
        try {
          await putDeviceKey(pending.accountIdB64, deviceKey);
        } catch (err) {
          // eslint-disable-next-line no-console
          console.warn("device-key persist (reset) failed", err);
        }
      }
      try {
        await putAccountIdentitySeed(pending.accountIdB64, accountIdentitySeed, deviceKey);
      } catch (err) {
        // eslint-disable-next-line no-console
        console.warn("account-identity-seed persist (reset) failed", err);
      }
      await persistSessionPair(pending.accountIdB64, reset.session, "reset");
      // The device holds the new keys, so it holds the new phrase (the local
      // three-word check, the hand-over to a new device, "Show recovery
      // phrase"). The signup-phrase marker makes a reload show the words
      // first, once nothing is owed.
      await storeDevicePhrase(pending.accountIdB64, phrase, deviceKey, "fresh-identity");
      markSignupPhraseOwed(plainStorage(), pending.accountIdB64);
      const record: KeyResetOwed = {
        v: 1,
        attestationB64Url: reset.attestationB64Url,
        keyGeneration: reset.session.keyGeneration.toString(),
        newSignaturePubkeyB64: base64Encode(newPubkey),
        signedAtMs: keyResetAttestationSignedAtMs(reset.attestation),
        done: { directory: false, deviceGroup: false },
      };
      writeKeyResetOwed(plainStorage(), pending.accountIdB64, record);
      // The old DeviceGroup, its registrar state and the chats' routing
      // belong to the old generation: the re-founding below replaces them.
      try {
        await deleteOutboxForAccount(pending.accountIdB64);
        clearObservationsForAccount(pending.accountIdB64);
        clearUnappliedForAccount(pending.accountIdB64);
        clearOwnRequestsForAccount(pending.accountIdB64);
        clearBeaconObservationsForAccount(pending.accountIdB64);
        clearLastObservedOwnerForAccount(pending.accountIdB64);
        clearLeaveRequestedForAccount(pending.accountIdB64);
      } catch (err) {
        // eslint-disable-next-line no-console
        console.warn("clearing the old generation's chat bookkeeping failed", err);
      }
      setKeyResetNote(true);

      // Stash the completion context and flip to `save-fresh-phrase`: the
      // flow is committed past the restore prompt. The words are withheld
      // while the publish is owed; `confirmFreshPhraseSaved` lands the
      // session once the person acknowledged them.
      setPendingFreshPhrase({
        seed: accountIdentitySeed,
        accountIdBytes: pending.accountIdBytes,
        accountIdB64: pending.accountIdB64,
        session: reset.session,
        username: pending.username,
        phrase,
        deviceKey,
      });
      setPendingRecovery(null);
      setState({
        kind: "save-fresh-phrase",
        account: {
          accountId: pending.accountIdB64,
          username: pending.username,
        },
      });

      // 4-5. Publish: the directory (with the attestation), then the
      // DeviceGroup journal (the attested reset). A failure leaves the
      // record for "Try again" and the next session start; the screen says
      // what happened.
      try {
        await retryKeyResetPublish(
          pending.accountIdB64,
          pending.accountIdBytes,
          accountIdentitySeed,
          deviceKey,
        );
      } catch (err) {
        // eslint-disable-next-line no-console
        console.warn(
          "publishing the reset keys did not finish; the phrase screen offers a retry",
          err instanceof Error ? err.name : "unknown",
        );
      } finally {
        setOwedVersion((v) => v + 1);
      }
      return phrase;
    },
    async confirmFreshPhraseSaved() {
      // Step 2 of the reset (and of a guest signup). Only valid while a
      // `pendingFreshPhrase` is parked. The user has saved the new phrase;
      // land the canonical session from the stashed context (empty
      // exportKey — ACCESS-only) and clear the stash.
      const pending = pendingFreshPhrase();
      if (pending === null) {
        throw new Error(
          "no pending fresh-identity phrase to confirm",
        );
      }
      if (freshOwedRecord() !== null) {
        // The new keys are not published yet; the phrase screen offers
        // "Try again" and does not show the words.
        throw new KeyedError("recovery_phrase.fresh.publish_owed");
      }
      const wasm = await loadWasm();
      // The phrase was shown and acknowledged on this screen.
      clearSignupPhraseOwed(plainStorage(), pending.accountIdB64);
      // A guest account has no username and so no password to replace; a
      // reset reached through "Forgot password?" keeps the offer (its
      // recovery credential now belongs to the new phrase, so the reset
      // works).
      if (pending.username === "") setNewPasswordOwed(false);
      // CONSUME the stash so the parked seed cannot be replayed.
      setPendingFreshPhrase(null);
      await finishCanonicalSession({
        wasm,
        accountIdentitySeed: pending.seed,
        accountIdBytes: pending.accountIdBytes,
        accountIdB64: pending.accountIdB64,
        // ACCESS-only convention (no password ⇒ no OPAQUE export key).
        exportKey: new Uint8Array(0),
        session: pending.session,
        username: pending.username,
      });
    },
    async recover(mnemonicPhrase, newPassword, username) {
      // The full phrase on /recover ends here, so this is where a
      // phrase-based restore is recorded for the chat list's honest empty
      // state (ADR-0049 section 5). The password is replaced on this path,
      // so no new-password step is owed.
      markRestored("phrase");
      setNewPasswordOwed(false);
      explicitSignInThisPage = true;
      // ADR-0054 S5: the reset is addressed by the name's login handle (the
      // typed name, else the account cached on this device) and the account
      // id comes back in the finish. No lookup runs before the session.
      const target = await nameTargetFor(
        resolveRecoverUsername({ cached: loadPersisted(), username }),
      );
      const resolvedUsername = target.username;
      // Parse the phrase up front so a bad mnemonic surfaces
      // before we spend a limited attempt. `fromPhrase` collapses
      // unknown-word and bad-checksum into one error message —
      // by design (preserves BIP39's no-probing property).
      const wasm = await loadWasm();
      const mnemonic = wasm.RecoveryMnemonic.fromPhrase(mnemonicPhrase);
      const canonicalPhrase = mnemonic.phrase;
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
        const result = await accessByName({
          target,
          run: (handle) => client.recover(handle, seed, utf8(newPassword)),
          bind: bindByName,
        });
        const accountIdBytes = result.accountId;
        const accountIdB64 = base64Encode(accountIdBytes);
        installSession(accountIdB64, result.session);
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
        // Persist the new session pair under the new device key — the old
        // pair (if any) was wrapped under the prior device key and now sits
        // in IDB as undecryptable bytes; the next decrypt attempt drops it.
        try {
          await putSessionPair(accountIdB64, result.session, deviceKey);
        } catch (err) {
          // eslint-disable-next-line no-console
          console.warn("session-pair persist (recover) failed", err);
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
        // The DeviceGroup snapshot was wrapped under the old device key too,
        // and the device identity above was regenerated, so the old leaf is
        // orphaned either way: drop the snapshot rather than leave a row the
        // next add-device flow cannot open.
        try {
          await deleteDeviceGroupSnapshot(accountIdB64);
        } catch (err) {
          // eslint-disable-next-line no-console
          console.warn("deleteDeviceGroupSnapshot during recover failed", err);
        }
        // Re-cache the phrase under the new device key so the backup keeps
        // working from this device.
        try {
          await storeDevicePhrase(accountIdB64, canonicalPhrase, deviceKey, "recover");
        } catch (err) {
          // eslint-disable-next-line no-console
          console.warn("phrase cache (recover) failed", err);
        }
        // ADR-0040 (G-35) — purge any pending-commit outbox entries for the old
        // session too; they reference the now-discarded MLS snapshot and must
        // never be swept into the fresh state.
        try {
          await deleteOutboxForAccount(accountIdB64);
          clearObservationsForAccount(accountIdB64);
          clearUnappliedForAccount(accountIdB64);
          clearOwnRequestsForAccount(accountIdB64);
          clearBeaconObservationsForAccount(accountIdB64);
          clearLastObservedOwnerForAccount(accountIdB64);
          // ADR-0045 UX §6.2.3 — the durable `leaveRequestedAt` record follows
          // the same discipline. It is never pruned with the pending sweep (it
          // has to outlive it, by construction) so an account reset is the
          // only thing that discards it besides `Delete this chat`.
          clearLeaveRequestedForAccount(accountIdB64);
        } catch (err) {
          // eslint-disable-next-line no-console
          console.warn("deleteOutboxForAccount during recover failed", err);
        }

        // ADR-0017: build the new canonical Identity from the
        // (UNCHANGED) BIP39 phrase seed — NOT the rotated export
        // key. The recovered identity is byte-for-byte the same as
        // before the recover, which is the whole point: contacts
        // never re-verify. ChatProvider free()s the prior
        // (pre-recovery) Identity if anything was lingering.
        const canonical = wasm.Identity.fromSeed(accountIdentitySeed);
        await installSessionIdentities(wasm, canonical, accountIdB64);
        keyPackageAccount = { accountIdBytes, accountIdB64 };

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
          // Proof-less re-publish (account key unchanged).
          await uploadIdentityBundle(discovery, accountIdBytes, canonical);
        } catch (err) {
          // eslint-disable-next-line no-console
          console.warn("post-recover identity bundle upload failed", err);
        }
        // Fresh KeyPackage: create -> durable persist -> upload.
        await publishKeyPackageBestEffort("recover");

        // 1.16d3c: stash the new export key so ContactsClient
        // works without a re-login round-trip.
        // ADR-0017: also carry the phrase seed (the canonical
        // identity seed). Cleared on logout.
        // Phase 1.5b: also stash the bearer token.
        setIdentitySecrets({
          accountId: accountIdBytes,
          exportKey: result.exportKey,
          accountIdentitySeed,
          sessionToken: result.session.sessionToken,
          sessionExpiresAtMs: result.session.sessionExpiresAtMs,
          keyGeneration: result.session.keyGeneration,
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
              {
                sessionToken: result.session.sessionToken,
                // ADR-0037 §6: arm the rollback gate — verify the auth-signed
                // backup head against the pinned attest key before trusting
                // the served envelope. Fresh install (no floor) → freshness
                // window; a returning device with a persisted floor → hard
                // monotonic floor. A stale head prompts the user; a tampered
                // head refuses outright.
                gate: {
                  accountId: accountIdBytes,
                  accountIdB64,
                  deviceKey,
                },
              },
              mnemonicPhrase,
            );
            if (restored.kind === "rollback-blocked") {
              // ADR-0037 §6: refuse + alarm. `hard` = an unambiguous tamper /
              // corruption; non-hard = the user was offered the stale override
              // and declined. Either way we do NOT load the served state.
              if (restored.hard) {
                // eslint-disable-next-line no-console
                console.error(
                  "restoreFromServerBackup BLOCKED a rolled-back / tampered " +
                    "backup (recover):",
                  restored.reason,
                );
                if (typeof window !== "undefined" && typeof window.alert === "function") {
                  window.alert(t("backup.rollback_refused.body"));
                }
              } else {
                // eslint-disable-next-line no-console
                console.warn(
                  "restoreFromServerBackup: user declined a stale backup " +
                    "(recover):",
                  restored.reason,
                );
              }
            } else if (restored.kind === "restored") {
              // ADR-0058 section 5.2 — seed the synced state (pins, drafts,
              // folders) before the sync runtime's first poll. Only when
              // this device holds none; best effort.
              try {
                await mergeSyncStateFromBackup(
                  accountIdB64,
                  deviceKey,
                  restored.syncedState,
                  (localBytes, backupBytes) => {
                    const backupFold = wasm.SyncedState.fromBytes(backupBytes);
                    if (localBytes === null) {
                      backupFold.free();
                      return backupBytes;
                    }
                    let localFold: InstanceType<typeof wasm.SyncedState> | null = null;
                    try {
                      localFold = wasm.SyncedState.fromBytes(localBytes);
                    } catch {
                      // A stored row this build cannot read holds nothing to keep.
                      backupFold.free();
                      return backupBytes;
                    }
                    mergeRestoredFold(localFold, backupFold);
                    const out = localFold.toBytes();
                    localFold.free();
                    backupFold.free();
                    return out;
                  },
                );
                // The session landed before this restore, so the sync
                // runtime may already be running on an empty fold.
                announceRestoredSyncedState(accountIdB64, restored.syncedState);
              } catch (err) {
                // eslint-disable-next-line no-console
                console.warn("post-restore synced-state seed failed", err);
              }
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
                    // NOT `chats.persistSnapshot`, which writes the group
                    // index from the in-memory list - empty on a fresh
                    // device, because nothing has rehydrated yet. That left
                    // the restored provider holding every group while
                    // `rehydrate` (whose only source is this index) rebuilt
                    // none of them, so the person saw no chats at all. The
                    // backup carries the index for exactly this moment.
                    await saveMlsSnapshot(
                      accountIdB64,
                      deviceKey,
                      canonical.snapshotStorage(),
                      restored.knownGroupIds,
                      base64Encode(canonical.signaturePublicKey),
                    );
                  } catch (err) {
                    // eslint-disable-next-line no-console
                    console.warn(
                      "post-restore MLS snapshot write failed",
                      err,
                    );
                  }
                  // `restoreStorage` REPLACED the provider, so the
                  // KeyPackage published a moment ago lost its private
                  // key — the directory is serving a package this device
                  // can no longer open. Publish a new one.
                  void publishKeyPackageBestEffort("post-restore");
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
                sessionToken: result.session.sessionToken,
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
        void subscribeToPush(accountIdB64).catch((err) => {
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
      // ADR-0054 S5: addressed by the name's login handle; the account id
      // comes back in the response, after the code verified. No lookup.
      const target = await nameTargetFor(trimmed);
      const resolvedUsername = target.username;
      const result = await accessByName({
        target,
        run: (handle) => client.totpRecover(handle, code),
        bind: bindByName,
      });
      const accountIdBytes = result.accountId;
      const accountIdB64 = base64Encode(accountIdBytes);
      setRestoreNotice(null);
      explicitSignInThisPage = true;
      // A forgot-password route: the new-password step is owed once this
      // device holds the keys (UX spec section 7).
      setNewPasswordOwed(true);
      // An authenticator code is not a fresh-auth proof (UX 17.1).
      sessionLanding = "authenticator";
      freshGrants.clear();
      // ADR-0018 ACCESS-only landing (warm cached-seed finish OR cold
      // fail-safe park).
      await landAfterAccessRecovery(resolvedUsername, accountIdBytes, accountIdB64, result.session);
    },
    async threeWordsAccount() {
      const found = await threeWordsContext();
      return found === null
        ? null
        : { accountIdB64: found.accountIdB64, username: found.username };
    },
    async recoverWithThreeWords(positions, words) {
      // Maintainer decision 2026-10-02: the three words are checked HERE,
      // against the phrase this device holds, like a wallet does. The
      // server takes no part in the check and stores nothing about it.
      const found = await threeWordsContext();
      if (found === null) {
        throw new KeyedError("recovery.words.error.unavailable");
      }
      const wasm = await loadWasm();
      const mnemonic = wasm.RecoveryMnemonic.fromPhrase(found.phrase);
      let seed: Uint8Array;
      try {
        // Trims and lower-cases; a wrong or unknown word is `false`.
        if (!wasm.phraseCheckWords(mnemonic, positions, [...words])) return false;
        seed = mnemonic.toSeed();
      } finally {
        mnemonic.free();
      }
      try {
        // The words matched. Sign in with what this device already holds:
        // the recovery login takes the 64-byte seed of the phrase and
        // answers with a session; the password is left as it is (the
        // "Choose a new password" step follows and replaces it).
        const target = await nameTargetFor(found.username);
        const result = await accessByName({
          target,
          run: async (handle) => {
            try {
              return await client.recoveryLogin(handle, seed);
            } catch (err) {
              // Only the sign-in itself is judged here, never the name
              // lookup before it or the re-bind after it.
              const verdict = storedPhraseVerdict({
                failure: recoveryLoginFailureOf(err),
              });
              if (verdict === "suspend-three-words") {
                // Most likely the account's phrase was replaced on another
                // device, but a `401` is not proof: nothing is deleted. The
                // route is suspended on this device and its budget cleared.
                markThreeWordsSuspended(plainStorage(), found.accountIdB64);
                clearThreeWordsBudget(plainStorage(), found.accountIdB64);
                throw new ThreeWordsSuspendedError();
              }
              throw err;
            }
          },
          bind: bindByName,
        });
        if (base64Encode(result.accountId) !== found.accountIdB64) {
          // The name answered for another account than the one cached
          // here. Adopt nothing.
          dropSession();
          throw new KeyedError("recovery.words.error.rejected");
        }
        setRestoreNotice(null);
        explicitSignInThisPage = true;
        setNewPasswordOwed(true);
        // Three words are not a fresh-auth proof for the server's routes;
        // they are the local phrase check that opens the password change.
        sessionLanding = "other";
        freshGrants.clear();
        phraseCheckedAtMs = Date.now();
        await landAfterAccessRecovery(target.username, result.accountId, found.accountIdB64, result.session);
        return true;
      } finally {
        seed.fill(0);
      }
    },
    async verifyPassword(password) {
      const st = state();
      if (st.kind !== "session" || st.account.username.trim().length === 0) {
        throw new KeyedError("common.error.session_not_ready");
      }
      const target = await nameTargetFor(st.account.username);
      try {
        // Not `accessByName`: nothing is re-bound and the session this
        // exchange yields is not installed anywhere.
        const result = await client.login(target.handle.wire, utf8(password));
        return base64Encode(result.accountId) === st.account.accountId;
      } catch (err) {
        // The network and the server's own refusals (a limit, an outage)
        // are not an answer about the password.
        if (err instanceof AuthNetworkError) throw err;
        if (err instanceof AuthHttpError && err.status !== 401 && err.status !== 404) throw err;
        // A wrong password fails the exchange on this side (the core
        // refuses to finish it) or at the server (`401`).
        return false;
      }
    },
    async devicePhrase() {
      const st = state();
      if (st.kind !== "session") return null;
      return readDevicePhrase(st.account.accountId);
    },
    publishOwed() {
      const st = state();
      return st.kind === "session" && isKeyResetOwed(plainStorage(), st.account.accountId);
    },
    async retryOwedPublish() {
      const st = state();
      const secrets = identitySecrets();
      if (st.kind !== "session" || secrets === null) {
        throw new KeyedError("common.error.session_not_ready");
      }
      const accountIdB64 = st.account.accountId;
      if (!isKeyResetOwed(plainStorage(), accountIdB64)) return;
      const deviceKey = await getDeviceKey(accountIdB64);
      if (deviceKey === null) {
        throw new KeyedError("common.error.session_not_ready");
      }
      // Same steps as `settleOwedKeyResetPublish`, awaited: the seed this
      // session holds is the one the record was written for.
      await retryKeyResetPublish(accountIdB64, secrets.accountId, secrets.accountIdentitySeed, deviceKey);
      await restoreOwedSignupPhrase(accountIdB64);
    },
    logout() {
      // ADR-0042, "A wake is shown only for the signed-in account": the wake
      // binding goes FIRST, so the service worker drops every wake from here
      // on, and this account's notifications leave the screen with it.
      void releaseWakeBinding();
      clearRestored();
      // Keep localStorage + IDB device key intact. Re-login on
      // this device remains cheap, and the device key stays
      // available for any encrypted local stores 1.16f wires up.
      // Drop the in-memory Identity secrets — the user just
      // signed out.
      const cached = loadPersisted();
      keyPackageAccount = null;
      setIdentitySecrets(null);
      // ADR-0017: drop any parked recovery / fresh-phrase context so a
      // stashed seed cannot linger past a sign-out. Zeroize the
      // fresh-phrase seed before dropping the reference.
      setNewPasswordOwed(false);
      setRestoreNotice(null);
      setPendingRecovery(null);
      const fresh = pendingFreshPhrase();
      if (fresh !== null) {
        fresh.seed.fill(0);
        setPendingFreshPhrase(null);
      }
      // The phrase leaves memory with the session. The owed marker stays: the
      // phrase is still unacknowledged, and the next sign-in on this device
      // shows it again (`restoreOwedSignupPhrase`).
      setSignupPhrase(null);
      // Drop the session pair so a subsequent protected write surfaces a
      // clean 401 instead of silently using a stale token.
      dropSession();
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
      const unsubscribed = unsubscribeFromPush().catch((err) => {
        // eslint-disable-next-line no-console
        console.warn("push unsubscribe (logout) failed", err);
      });
      // ADR-0054 S3: the token pool lets go of the account on logout, once
      // the push unregister above has spent its token (bounded, so a hung
      // request cannot keep the pool bound). Only memory is dropped: the
      // account's stored tokens stay for its next sign-in (2026-10-01). A
      // sign-in in the meantime installs a new session, and
      // `unbindIfNoSession` then leaves that one alone.
      void Promise.race([
        unsubscribed,
        new Promise<void>((resolve) => setTimeout(resolve, 10_000)),
      ]).finally(() => anonTokens.unbindIfNoSession());
      chats.clear();
      setKeyResetNote(false);
      setState({
        kind: "anon",
        ...(cached !== null ? { cachedUsername: cached.username } : {}),
      });
    },
    async forgetDevice() {
      // ADR-0042 — as in `logout`: the wake binding first, then this app's
      // delivered notifications.
      await releaseWakeBinding();
      clearRestored();
      const cached = loadPersisted();
      const current = state();
      const sessionAccountIdB64 =
        current.kind === "session" ? current.account.accountId : null;
      // ADR-0050 section 2.10 — take this device's KeyPackage out of the
      // directory BEFORE the wipe, while the bearer token still exists: the
      // account's other devices retire this device's leaves from every chat
      // when their scheduled check sees the row gone (`device-readmission.ts`),
      // and nobody proposes a Welcome to a device that no longer exists.
      {
        const own = chats.identity()?.signaturePublicKey;
        const secrets = identitySecrets();
        if (own !== undefined && secrets !== null) {
          try {
            await discovery.deleteKeyPackage(own);
          } catch (err) {
            // eslint-disable-next-line no-console
            console.warn("forgetDevice: directory KeyPackage not deleted", err);
          }
        }
      }
      // The device-key-wrapped phrase cache is wiped below, so an owed phrase
      // can no longer be shown on this device; drop the in-memory copy (the
      // stored marker goes with the wipe).
      setSignupPhrase(null);
      // 2026-09-30 — every piece of this account's local data, in ONE list
      // (`wipe-account.ts`), shared with Delete account. Best effort per
      // store; a failing store is logged there and never stops the others.
      // Another account's data on this browser is its own.
      //
      // Stop the session's writers FIRST. The wipe clears stores one by one
      // while the session is still live, and a session-wide receiver that
      // reacts to one of those clears writes its own store back: the
      // one-to-one contact adoption (`OneToOneContactAdoption.tsx`) re-ran
      // when the profiles store was cleared and re-added the peer of every
      // live one-to-one to the contact list the wipe had just emptied (seen
      // live 2026-10-01). Unbinding the contact list makes such a write a
      // no-op (the wipe names its account explicitly), and dropping the
      // live groups (1.16e5; 1.16e6c1 also frees the canonical Identity)
      // leaves those receivers nothing to act on.
      bindContactsAccount(null);
      chats.clear();
      const wipeTarget = cached?.accountIdB64 ?? sessionAccountIdB64;
      if (wipeTarget !== null) {
        await wipeAccountLocalData(wipeTarget);
      }
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
      keyPackageAccount = null;
      setIdentitySecrets(null);
      // ADR-0017: drop any parked recovery / fresh-phrase context.
      // Zeroize the fresh-phrase seed before dropping the reference.
      setNewPasswordOwed(false);
      setRestoreNotice(null);
      setPendingRecovery(null);
      const fresh = pendingFreshPhrase();
      if (fresh !== null) {
        fresh.seed.fill(0);
        setPendingFreshPhrase(null);
      }
      dropSession();
      setKeyResetNote(false);
      // ADR-0054 S3: nothing of this account's token pool survives. The wipe
      // removed its stored rows; the push unregister above may have spent a
      // token and written the rest back, so delete this account's rows again
      // (after that write) and drop the pool's memory. Another account's
      // stored tokens stay.
      if (wipeTarget !== null) anonTokens.forgetAccount(wipeTarget);
      else anonTokens.unbind();
      setState({ kind: "anon" });
    },
  };

  return (
    <AuthContext.Provider value={api}>{props.children}</AuthContext.Provider>
  );
}

/**
 * Read, without prompting for anything, every stored secret a page-load
 * restore needs: the plaintext `(accountId, username)` marker, the
 * non-extractable device root key, the persisted session token (the store
 * already drops expired rows) and the ADR-0017 AccountIdentity seed. Every
 * read that fails — IDB unreachable (private mode, quota), a decrypt failure
 * after a device-key rotation, a tampered row — reads as "absent", so the
 * decision in `silent-restore.ts` lands in `anon` rather than crashing the
 * app or restoring from a partial set.
 *
 * The caller owns the returned seed bytes and must zero them when it does not
 * use them.
 */
async function probeStoredSecrets(): Promise<{
  cachedAccount: { accountIdB64: string; username: string } | null;
  deviceKey: CryptoKey | null;
  sessionPair: SessionPair | null;
  identitySeed: Uint8Array | null;
}> {
  const cached = loadPersisted();
  if (cached === null) {
    return { cachedAccount: null, deviceKey: null, sessionPair: null, identitySeed: null };
  }
  const cachedAccount = {
    accountIdB64: cached.accountIdB64,
    username: cached.username,
  };
  let deviceKey: CryptoKey | null = null;
  try {
    deviceKey = await getDeviceKey(cached.accountIdB64);
  } catch {
    deviceKey = null;
  }
  if (deviceKey === null) {
    return { cachedAccount, deviceKey: null, sessionPair: null, identitySeed: null };
  }
  let sessionPair: SessionPair | null = null;
  try {
    sessionPair = await getSessionPair(cached.accountIdB64, deviceKey);
  } catch {
    sessionPair = null;
  }
  let identitySeed: Uint8Array | null = null;
  if (sessionPair !== null) {
    try {
      identitySeed = await getAccountIdentitySeed(cached.accountIdB64, deviceKey);
    } catch {
      identitySeed = null;
    }
  }
  return { cachedAccount, deviceKey, sessionPair, identitySeed };
}

/**
 * Publish the account's public identity bundle (Ed25519 + X25519 pubkeys,
 * deterministic from the phrase seed — 1.16d3b2, ADR-0017).
 *
 * This used to publish the MLS KeyPackage too, in parallel, BEFORE the
 * snapshot holding the KeyPackage's private key was persisted — and from
 * temporary identities on the guest-signup and fresh-mint paths, whose
 * providers were freed right after. The KeyPackage now has its own
 * fail-closed, single-flight path (`chat/keypackage-publisher.ts`, reached
 * through `keyPackagePublisher` in `AuthProvider`): create from the
 * CANONICAL identity, persist durably, then upload.
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
 *
 * ADR-0036 (G-30 finding 2): `genesisAttest`, when present, is the
 * auth-attested genesis attestation (from `AuthClient.genesisAttest`)
 * riding alongside `leafAuth` on those same genesis publish sites — the
 * proof that only the OPAQUE-authenticated account could have published
 * this genesis key (closes the first-entry-squatting residual the
 * self-signature only DETECTS). Omitted on every non-genesis path.
 *
 * ADR-0065: `keyResetAttest`, when present, is the key-reset attestation
 * (from `AuthClient.resetAccountKeys`) under which discovery accepts a
 * changed key without a supersession proof. Supplied ONLY by the
 * key-reset publish (`publishResetIdentity`).
 */
async function uploadIdentityBundle(
  discovery: DiscoveryClient,
  accountId: Uint8Array,
  identity: WasmIdentity,
  supersessionProof?: Uint8Array | string,
  leafAuth?: Uint8Array | string,
  genesisAttest?: Uint8Array | string,
  keyResetAttest?: Uint8Array | string,
): Promise<void> {
  await discovery.uploadIdentityBundle(
    accountId,
    identity.signaturePublicKey,
    identity.x25519PublicKey,
    supersessionProof,
    leafAuth,
    genesisAttest,
    keyResetAttest,
  );
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
 * ADR-0036 (G-30 finding 2): mint the AUTH-attested genesis attestation
 * — the anti-squatting proof that only the OPAQUE-authenticated account
 * could have published this genesis key.
 *
 * Where `mintLeafGenesisAuth` self-signs the binding with the leaf's own
 * key (honest TOFU that only DETECTS squatting), this asks the auth
 * service — bound to the live `sessionToken`, which is proof the caller
 * OPAQUE-authenticated as `account_id` — to sign a
 * `(account_id, username_hash, pubkey, expiry)` binding, so a squatter
 * who cannot authenticate as the victim can never obtain one. It rides
 * alongside the self-signature on the same genesis publish.
 *
 * SOFT-FAIL by design (unlike `mintLeafGenesisAuth`, which is fail-loud):
 * per ADR-0036 the transparency append still accepts the genesis leaf on
 * the self-signature alone while the operator-side
 * `LAKKO_TRANSPARENCY_REQUIRE_GENESIS_ATTEST` flag is default-OFF, so a
 * mint failure must NOT block signup. We log and return `undefined` — the
 * attestation is attached only when successfully minted. The flag flips on
 * once all three clients mint (ADR-0036 §"Consequences").
 */
async function mintGenesisAttestation(
  client: AuthClient,
  sessionToken: string,
  identity: WasmIdentity,
): Promise<Uint8Array | undefined> {
  try {
    return await client.genesisAttest(sessionToken, identity.signaturePublicKey);
  } catch (err) {
    // eslint-disable-next-line no-console
    console.warn("genesis-attest mint failed — publishing on self-sig alone", err);
    return undefined;
  }
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
  /** Where the seed came from, carried on the mismatch error so the screen
   *  says "phrase" or "your other device". Defaults to the phrase. */
  readonly source?: "phrase" | "device";
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
        "missing",
        "No published identity was found to verify the recovered " +
          "identity. Try again, or restore from a device that still " +
          "has your account.",
      );
    }
    const published = base64Decode(bundle.signature_pubkey);
    if (!bytesEqual(published, derivedPubkey)) {
      args.accountIdentitySeed.fill(0);
      throw new SupersessionPhraseMismatch(args.source ?? "phrase");
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
      "unreachable",
      "Could not reach the directory to verify the recovered identity. " +
        "Check your connection and try again.",
    );
  }
}

/** Entropy length of the 12-word phrase (recovery payload v2). */
const PHRASE_ENTROPY_LENGTH = 16;

/**
 * Recovery payload v2 (`devices/recovery-payload.ts`): another device sent
 * the 16-byte entropy of the recovery phrase. Rebuild the phrase and derive
 * the 64-byte AccountIdentity seed from it.
 *
 * CONSUMES `entropy`: it is zeroized before this returns or throws. The
 * returned seed is the caller's to verify against the published identity
 * (which zeroizes it on refusal) BEFORE anything is stored. The phrase is a
 * JS string and cannot be zeroized; it is dropped by the caller on refusal
 * and reaches storage only after the check.
 */
function phraseFromTransferredEntropy(
  wasm: Wasm,
  entropy: Uint8Array,
): { phrase: string; seed: Uint8Array } {
  try {
    if (entropy.length !== PHRASE_ENTROPY_LENGTH) {
      throw new Error("internal: transferred phrase entropy has the wrong length");
    }
    const mnemonic = wasm.RecoveryMnemonic.fromEntropy(entropy);
    try {
      const seed = mnemonic.toSeed();
      if (seed.length !== ACCOUNT_IDENTITY_SEED_LENGTH) {
        seed.fill(0);
        throw new Error("internal: the transferred phrase gave a seed of the wrong length");
      }
      return { phrase: mnemonic.phrase, seed };
    } finally {
      mnemonic.free();
    }
  } finally {
    entropy.fill(0);
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
