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
 * ## The session pair and the key generation (ADR-0073)
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
  createMemo,
  createSignal,
  onCleanup,
  onMount,
  type Accessor,
  type JSX,
  useContext,
} from "solid-js";

import {
  AuthClient,
  AuthHttpError,
  AuthNetworkError,
  RecallWrongPasswordError,
  ACCOUNT_STATEMENT_KIND_SESSION_UPGRADE,
  type RecallPendingFinish,
  type RecallResult,
  type KeyResetResult,
} from "./client";
import { loadPersistedAccount, savePersistedAccount } from "./persisted-account";
import { accountStoreIds, wipeAccountLocalData, wipeAccountStaged } from "./wipe-account";
import { dropAccountMemory } from "./account-memory";
import {
  deleteRecallRecord,
  getRecallRecord,
  hasRecallRecord,
  listRecallAccounts,
  putRecallRecord,
  type RecallRecord,
} from "./recall-record";
import {
  recallAttemptFailure,
  recallExpired,
  recallStatusVerdict,
  signOutWipe,
  type RecallAttemptFailure,
  type RecallCheck,
  type RecallEndNotice,
} from "./recall";
import { base64Decode, base64Encode, base64UrlDecodeNoPad } from "./account-id";
import {
  installAccountStorageGate,
  openAccountStores,
  sealAccountSession,
} from "./account-store-gate";
import { resolveRecoverUsername } from "./recover-resolution";
import {
  accessByName,
  claimUsername,
  loginHandleFor,
  nameTargetFor,
  type LoginHandle,
  type NameTarget,
} from "./login-handle";
import { createLoginHandleSync } from "./login-handle-sync";
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
import { deleteMlsSnapshot } from "../chat/mls-store";
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
import { SessionBinding, StaleBindingError } from "./binding-epoch";
import { releaseWakeBinding } from "../push/wake-binding";
import { anonTokens } from "../tokens/anon-token-pool";
import {
  discoveryAccountLookup,
  runNameCheckedDeviceRestore,
} from "./device-restore-name-check";
import { bootstrapBackupFromPhraseString } from "../backup/bootstrap";
import { startAutoBackup } from "../backup/auto-backup-driver";
import { markBackupDirty } from "../backup/auto-backup";
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
import { writeContinuityMarker } from "../reach/continuity";
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
import {
  EMPTY_KEY_ROTATION_STATE,
  conversationSeed,
  deleteKeyRotationState,
  getKeyRotationState,
  updateKeyRotationState,
  predecessorSeeds,
  withPredecessorSeed,
  type KeyRotationState,
  type PreparedRotation,
} from "./key-rotation-store";
import { bindingVerdict, rebindsAfterStateBundle } from "../devices/own-conversation-key";
import { loadMlsSnapshot } from "../chat/mls-store";
import { stampPredecessorAdoption } from "../contacts/address-resolver";
import { markSpendOwed } from "../contacts/rotation-spent";
import {
  SUPERSESSION_WINDOW_SECS,
  clearRotatedNotice,
  clearRotation,
  markRotatedNotice,
  pendingFor,
  readRotation,
  rotatedNoticeOwed,
  rotationIntake,
  rotationWithholdsPhrase,
  supersededStep,
  upgradeVerdict,
  withPendingRotation,
  type CommitProbe,
  type DirectoryUploadMode,
  type PendingRotation,
  type UpgradeVerdict,
} from "../devices/key-rotation";
import { awaitKeyRotationPort } from "../devices/sync/key-rotation-port";
import { rootEndorsementFor } from "../devices/lifecycle";
import { transferRoot, transferRootWithAttestation } from "../ds/ownership";
import { DsClient } from "../ds/client";
import { journalHex } from "../ds/access";
import { deletePhrase } from "../backup/phrase-store";
import {
  clearGuestAccount,
  guestAccountState,
  guestDeletePlan,
  guestExpired,
  guestPhraseKey,
  guestRestoreAction,
  markGuestAccount,
  noteGuestUsernameClaimed,
  type GuestAccountState,
} from "./guest-account";
import {
  openPasskeyVault,
  passkeyKeysOutcome,
  PasskeyVaultUnusableError,
  PASSKEY_PRF_SALT,
  type PasskeyKeysOutcome,
} from "../recovery/passkey-vault";
import {
  clearSignupPhraseOwed,
  freshPhraseDisposition,
  isSignupPhraseOwed,
  owedPhraseCompletesPark,
  markSignupPhraseOwed,
} from "./phrase-gate";
import {
  clearProtectStepOwed,
  isPhraseProtected,
  isProtectStepOwed,
  markProtectStepOwed,
  newWordsShown,
  setPhraseProtected,
} from "./protect/account-protection";
import {
  clearFirstPasswordOwed,
  isFirstPasswordOwed,
  markFirstPasswordOwed,
  usernameClaimOffered,
} from "./username-claim";
import {
  registerUsername,
  rememberUsername,
  usernameRegisterAction,
  usernameWritePending,
} from "./username-register";
import {
  bindDecision,
  bindRefusal,
  deviceIdB64Url,
  genesisMintNeeded,
  genesisMintOutcome,
  landingDeviceId,
  readGenesisRecorded,
  readKnownDeviceId,
  writeGenesisRecorded,
  writeKnownDeviceId,
  type LandingTarget,
} from "./session-device";
import {
  clearLogoutOwed,
  isLogoutOwed,
  markLogoutOwed,
  runServerLogout,
  type LogoutAttempt,
} from "./logout-owed";
import { restoreAccountBackup, restoreOwedAccountBackup } from "../backup/restore";
import { isBackupRestoreOwed, markBackupRestoreOwed } from "../backup/restore-owed";
import {
  backupKnownGroupIds,
  pruneJoinedRestoredGroups,
} from "../backup/restored-known-groups";
import type { Identity as WasmIdentity } from "../../wasm/lakko_ffi_wasm";
import { KeyedError } from "../l10n/keyed-error";

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
      /** ADR-0073: the refresh credential was refused as invalid or had
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

/** Result of a signup. */
export interface SignupOutcome {
  /** The 12-word BIP39 recovery phrase the device generated and stored.
   *  ADR-0075 D2: a sign-up with a username does NOT show it (the "Protect
   *  your account" step follows instead, and the person may save a phrase
   *  later). The guest signup still shows it through its phrase screen. */
  recoveryPhrase: string;
}

/** ADR-0017 — thrown by `provideRecoveryPhrase` (and the ADR-0018
 *  pairing completions) when the supplied AccountIdentity seed
 *  derives a pubkey that does NOT match the account's published
 *  bundle. Surfaced to the UI as "doesn't match this account" — we
 *  refuse rather than cache a forked identity that would break
 *  every contact's pin. */
/**
 * A guest account cannot be kept because this device no longer holds its
 * recovery phrase (the browser's storage was cleared, or the device key
 * changed). Nothing can show the phrase again, and an account whose phrase
 * was never seen cannot be recovered, so the only thing left is to delete it.
 */
export class GuestPhraseUnavailableError extends Error {
  constructor() {
    super("the guest account's recovery phrase is not on this device");
    this.name = "GuestPhraseUnavailableError";
  }
}

/**
 * `Create account` was pressed for a guest account past its lifetime. The
 * account has been deleted and the session is over by the time this is
 * thrown.
 */
export class GuestAccountExpiredError extends Error {
  constructor() {
    super("the guest account has expired");
    this.name = "GuestAccountExpiredError";
  }
}

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
  /** ADR-0074 section 3.5: after a proven key rotation, the 64-byte seed of
   *  the account key the conversation leaves are still bound to (the proven
   *  predecessor this device holds). Absent when the account never rotated.
   *  Read through {@link conversationSeedOf}. */
  readonly conversationIdentitySeed?: Uint8Array;
  /** The access token of the session pair (ADR-0073). Fifteen minutes
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

/** The seed of the account identity to use INSIDE a conversation (the role
 *  table, the journal ownership): the predecessor's while one is held, else
 *  the account's (ADR-0074 section 3.5). */
export function conversationSeedOf(secrets: IdentitySecrets): Uint8Array {
  return secrets.conversationIdentitySeed ?? secrets.accountIdentitySeed;
}

/**
 * ADR-0073 sections 3.5 and 4.2 (`docs/ux/identity-recovery.md` 15.2):
 * this device's keys are not the account's keys any more. One screen,
 * reached two ways:
 *
 *   - `keys-reset`: a `401 key-generation-superseded`, or a landing that
 *     found a higher generation published. The wipe runs by itself; the
 *     screen shows "Removing..." then one button, "Sign in".
 *   - `identity-mismatch`: a landing that found a different key at the same
 *     generation (an anomaly, not a known reset). The screen blocks and the
 *     wipe runs on the person's tap ("Sign out").
 *   - `device-removed`: this device was removed from the account on another
 *     device (ADR-0074). The wipe runs by itself; "Removing..." then
 *     "Sign in".
 */
export type SignedOutState =
  | {
      readonly reason: "keys-reset";
      readonly phase: "removing" | "removed";
      readonly username: string;
      /** Set when the take-the-account-back form ended this way (UX 18.4:
       *  "No more tries", "Not possible any more"); drawn above the body. */
      readonly notice?: RecallEndNotice;
    }
  /** ADR-0074 section 1.1 (UX 18.4): the keys were reset on another device
   *  and this device held them. The staged wipe ran; only the recall record
   *  is left. The screen asks "Was it you?". Driven by the record, so it is
   *  up again after a reload. */
  | {
      readonly reason: "recall";
      readonly username: string;
      /** When this device learned of the reset, unix ms. */
      readonly learnedAtMs: number;
    }
  /** ADR-0074 section 2.3 (UX 18.2): another device of the account removed
   *  this one (auth answered `401 device-revoked`, or this device applied
   *  its own removal from the DeviceGroup). The wipe runs by itself, all of
   *  it, with no recall record; then one button, "Sign in". */
  | {
      readonly reason: "device-removed";
      readonly phase: "removing" | "removed";
      readonly username: string;
    }
  | { readonly reason: "identity-mismatch"; readonly phase: "blocked" | "removing" | "removed"; readonly username: string };

/** The "Was it you?" screen's actions (ADR-0074 part 1). */
export interface RecallApi {
  /** Ask the server whether the account can still be taken back from this
   *  device. Never throws; an expired record reads `closed` without a
   *  request. */
  check(): Promise<RecallCheck>;
  /** "Yes, it was me" confirmed, or taking back is not possible any more:
   *  destroy the record and finish the wipe (the signed-out screen, then
   *  "Sign in"). `notice` words why when the form was open. */
  end(notice: RecallEndNotice | null): Promise<void>;
  /** "Take the account back": the old password proves it, the account gets
   *  the new password, new keys and a new phrase. `null` when it landed
   *  (the phrase screen is up); otherwise why it did not. Never throws. */
  takeBack(
    oldPassword: string,
    newPassword: string,
    /** ADR-0075 section 8 (UX 19.7), "Use my recovery phrase instead": the
     *  12 words from before the reset prove it and `oldPassword` is not
     *  used. A phrase the core refuses reads as a wrong one. */
    options?: { readonly phrase?: string },
  ): Promise<RecallAttemptFailure | null>;
  /** UX 19.7, without an old device: ask whether `phrase` is `username`'s
   *  recovery phrase from before a key reset in the last month. Anonymous;
   *  the account is named by login handle and the statement is signed by
   *  the key the phrase derives. `open` only when the phrase arm is offered
   *  for it. Never throws. */
  checkByPhrase(username: string, phrase: string): Promise<RecallCheck>;
  /** UX 19.7: take the account back with the phrase alone. The account gets
   *  `newPassword`, new keys and a new phrase; this device lands on the new
   *  phrase screen. `null` when it landed. Never throws. */
  takeBackByPhrase(
    username: string,
    phrase: string,
    newPassword: string,
  ): Promise<RecallAttemptFailure | null>;
}

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
  /** ADR-0073 (UX 15.4): `true` while the keys were reset but the new
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
  /** New recovery words this device owes the person a look at, until they
   *  acknowledge them; `null` otherwise. Set after the account keys were
   *  changed on this device (a key reset, taking the account back, a key
   *  rotation, "Save a recovery phrase"), never by a sign-up (ADR-0075 D2:
   *  the phrase is not shown at sign-up). While non-null BOTH route guards
   *  render the phrase screen instead of any route (`phrase-gate.ts`).
   *  Survives a reload and a sign-out/sign-in on this device: the fact that
   *  it is owed is kept in storage and the phrase is read back from the
   *  device-key-wrapped cache. */
  readonly signupPhrase: Accessor<string | null>;
  /** The user has written the new words down. Clears them (memory and the
   *  owed marker); the guards then render the route they wrap. */
  acknowledgeSignupPhrase(): void;
  /** ADR-0075 (UX 19.1a): the sign-up's "Protect your account" step is
   *  owed. While `true` both route guards render that step instead of any
   *  route; it survives a reload (a marker in storage). */
  readonly protectStepOwed: Accessor<boolean>;
  /** The person chose on the "Protect your account" step (an email was
   *  added, a phrase is being saved, or "Continue without"). */
  finishProtectStep(): void;
  /** A guest account chooses its username (ADR-0075 pass A, part 2): the
   *  sign-up's claim, run later. Binds the login handle at auth, registers
   *  the name at discovery (releasing the handle again when that fails),
   *  saves the name in the local account marker and in the live session,
   *  and marks the account's first password as owed, which is what lets
   *  `replacePassword` run once without a proof. Only for a signed-in
   *  account that has no username. Throws `UsernameTakenError`, or what the
   *  bind or the register threw. Resolves the canonical name and the
   *  username certificate the register call returned.
   *
   *  This is the first half of keeping a guest account
   *  (`GuestAccountOffer`, asked after the call): a guest past its lifetime
   *  is deleted instead ({@link GuestAccountExpiredError}), one whose phrase
   *  this device no longer holds cannot be kept
   *  ({@link GuestPhraseUnavailableError}), and the name is recorded on the
   *  guest marker so `No, thanks` frees it again. The account stops being a
   *  guest when its first password is set
   *  (`replacePassword({ firstPassword: true })`), which also moves the
   *  phrase under the account's own key, uploads the first backup and owes
   *  the "Protect your account" step. The phrase is never shown. */
  claimGuestUsername(
    username: string,
  ): Promise<{ readonly username: string; readonly certificate: Uint8Array | null }>;
  /** ADR-0076: the account's username changed on THIS device (the rename's
   *  server steps are done): the local marker, the live session, the handle
   *  marked known for this session, and the own-username register, which
   *  carries the name to the account's other devices. */
  applyRenamedUsername(username: string): Promise<void>;
  /** Whether the signed-in account's FIRST password is still owed (a guest
   *  whose name was saved and whose password step did not finish). */
  firstPasswordOwed(): boolean;
  /** Offer the "Protect your account" step once (after a guest chose a
   *  username: it never saw the step). */
  offerProtectStep(): void;
  /** Land the session `POST /recovery-email/recover/password/finish`
   *  answered (ADR-0075 section 7, password mode): the password was already
   *  replaced by the mailed code, so no new-password step is owed and no
   *  fresh-auth proof is held. On a device that holds the account's keys the
   *  session lands; otherwise the guard shows "Restore your keys".
   *
   *  `whenOpened` runs once the account's local stores are open
   *  (`account-store-gate.ts`), before anything of the landing is awaited:
   *  the place for the screen's own writes of this account (a write made
   *  before it would be dropped when the account was wiped on this page
   *  earlier, and a pending write would be dropped by the landing). */
  landEmailPasswordRecovery(
    username: string,
    accountId: Uint8Array,
    session: SessionPair,
    whenOpened?: () => void,
  ): Promise<void>;
  /** URL-safe base64 of this device's id for a landing of `username`'s
   *  account, when this device already holds its keys; else `undefined`. */
  landingDeviceIdFor(username: string): string | undefined;
  /** ADR-0073: this device's keys are not the account's keys any more
   *  (`SignedOutState`). While non-null both route guards render the
   *  signed-out screen instead of any route, whatever `state()` says. */
  readonly signedOut: Accessor<SignedOutState | null>;
  /** The signed-out screen's button: on `identity-mismatch` (blocked) runs
   *  the wipe; once `removed`, clears the screen so the sign-in page can
   *  show. */
  confirmSignedOut(): Promise<void>;
  /** ADR-0074 part 1 (UX 18.4): the actions of the "Was it you?" screen.
   *  Each is a no-op (or `unknown` / `closed`) when no recall record is
   *  held. */
  readonly recall: RecallApi;
  /** The account was taken back on this device in this page session: the
   *  one line at the top of the chat list (`devices.recall.done`). */
  readonly recallDone: Accessor<boolean>;
  dismissRecallDone(): void;
  /** ADR-0074 section 2.3: this device applied its own removal from the
   *  account's DeviceGroup (`DeviceGroup.isActive` is `false`), which is
   *  proof of removal by a device of the account. Shows the removed screen
   *  and wipes everything by itself. Called by the DeviceGroup sync runtime;
   *  a no-op for an account that is not the signed-in one. */
  reportDeviceRemoved(accountIdB64: string): void;
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
  /** ADR-0074 (a device paired after a key rotation): after a pairing state
   *  bundle stored the account's earlier keys on a signed-in device, mint
   *  the device binding again under the conversation key and publish the
   *  KeyPackage again, but only while this device holds no conversation.
   *  Resolves whether the binding changed; never rejects. */
  rebindAfterStateBundle(accountIdB64: string): Promise<boolean>;
  /** Run the OPAQUE-PAKE registration flow.
   *
   *  Throws on network / server errors (the form surfaces them).
   *  On success, persists `(accountId, username)` to localStorage,
   *  derives the device root key + writes it to IndexedDB, and
   *  flips state to `session` with the "Protect your account" step owed
   *  (`protectStepOwed`). The recovery phrase is stored on the device and
   *  not shown (ADR-0075 D2). */
  /** `options.signal` cancels the account-creation proof of work (ADR-0054
   *  S4) and everything after it; the abort rejects with an `AbortError`
   *  (`auth/pow.ts` `isAbortError`) and nothing is created server-side. */
  signup(
    username: string,
    password: string,
    options?: { readonly signal?: AbortSignal },
  ): Promise<SignupOutcome>;
  /** ADR-0032 guest join — create a FULL account with NO username and no
   *  user-chosen password, so a visitor who opened a `/c/` link without an
   *  account goes straight into the call. Reuses the `signup` machinery:
   *  OPAQUE registration under a RANDOM 32-byte password nobody sees, the
   *  device root key, the first identity-bundle publish (with the genesis
   *  `leafAuth`) — but no username, and NO recovery-phrase screen: the
   *  session lands at once and the account is marked as a guest
   *  (`guest-account.ts`). The phrase is kept on this device, wrapped under
   *  the device key, until the person decides: {@link claimGuestUsername}
   *  and the first password keep the account,
   *  {@link discardGuestAccount} destroys it. */
  signupGuest(options?: { readonly signal?: AbortSignal }): Promise<void>;
  /** The session is a guest account (`guest-account.ts`): made to join a
   *  call from a link, not yet kept. */
  readonly isGuest: Accessor<boolean>;
  /** Delete a guest account at once: on the servers (in the order
   *  `guestDeletePlan` gives, which frees a username an unfinished
   *  `Create account` took), and every trace of it on this device. Lands
   *  signed out. Throws, with nothing removed locally, when a required
   *  delete failed. */
  discardGuestAccount(): Promise<void>;
  /** If the session is a guest account past its lifetime
   *  (`GUEST_LIFETIME_MS`), delete it the way `No, thanks` does, except that
   *  a server that cannot be reached does not stop the device wipe: the
   *  servers remove an unkept account on their own clock. Resolves `true`
   *  when the account was removed (the session is over). */
  expireGuestIfDue(): Promise<boolean>;
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
  /**
   * ADR-0074 part 3 (Stage A): changing the account keys with the other
   * devices kept. The auth layer's half; `devices/key-rotation.ts` decides
   * and `devices/key-rotation-runtime.ts` runs the operation.
   */
  readonly keyRotation: {
    /** Reactive: a kept device is fetching the new keys (UX 18.3). */
    readonly updating: Accessor<boolean>;
    /** Reactive: this device adopted keys changed on another device and the
     *  line saying so was not dismissed yet. */
    noticeOwed(): boolean;
    dismissNotice(): void;
    /** The rotation record changed (the runtime tells the readers). */
    recordChanged(): void;
    /** A `ServiceEnvelope` body from the account's own DeviceGroup: stores
     *  a verified `key-rotation` envelope as pending. Resolves `true` when
     *  the bytes were one (nothing else is to be done with them). */
    intake(envelope: Uint8Array): Promise<boolean>;
    /** Rotating device, step 3: the new phrase and the proof, sealed on this
     *  device; returns the envelope to post into the DeviceGroup. */
    prepare(deviceIdB64: string): Promise<{
      readonly envelope: Uint8Array;
      readonly newSignaturePubkeyB64: string;
      readonly preparedAtMs: number;
    }>;
    /** Steps 5-7: `rotate/{start,finish}`, then the new keys are stored and
     *  put into this session. Throws `AuthHttpError` / `AuthNetworkError`. */
    commit(
      grant: string,
      revokeDeviceIds: readonly Uint8Array[],
      /** ADR-0075 section 3: `phraseSaved` makes the account
       *  phrase-protected in the same transaction ("Save a recovery
       *  phrase"); the server deletes the email vault with it. */
      options?: { readonly phraseSaved?: boolean },
    ): Promise<{
      readonly keyGeneration: bigint;
      readonly attestationB64Url: string;
      readonly signedAtMs: number;
    }>;
    /** Whether a commit whose answer was lost landed (and adopt if so). */
    probeCommit(): Promise<CommitProbe>;
    /** Step 8. Throws `DiscoveryHttpError` on a refusal. */
    uploadIdentity(
      mode: DirectoryUploadMode,
      attestationB64Url: string | null,
      proofB64: string | null,
    ): Promise<void>;
    /** The prepared proof (standard base64) while it is held. */
    preparedProofB64(): Promise<string | null>;
    /** Step 9, the DeviceGroup journal. Throws the DS client's error. */
    transferDeviceGroupRoot(attestationB64Url: string | null): Promise<void>;
    /** The 64-byte seed of the key that was the account's before the last
     *  rotation (the channel owner handover and the journal takeovers sign
     *  with it), or `null`. */
    predecessorSigningSeed(): Promise<Uint8Array | null>;
    /** Step 12: show the new words (the existing new-phrase screen). */
    showNewPhrase(): Promise<void>;
    /** "Not now": forget what was prepared. */
    discardPrepared(): Promise<void>;
    /** A publishing step was told this session is of a superseded
     *  generation. */
    reportSuperseded(): void;
  };
  /** Whether this device still owes the directory or the device list the
   *  signed-in account's new keys (ADR-0073: a reset whose publish has not
   *  succeeded; ADR-0074: a key rotation with a publishing step owed). While
   *  it does, the phrase is not shown anywhere. */
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
  /** Passkey sign-in (ADR-0072 decision 6, UX spec section 4). The
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
   *  bundle) so the user cannot silently fork their identity.
   *
   *  Every way this route gets the phrase ends here (the typed phrase, a
   *  passkey that carries the keys), and like the restore screen it
   *  then fetches and applies the account's backup (`backup/restore.ts`):
   *  contacts, synced state and the list of conversations, never MLS
   *  state. A missing backup or a failed fetch does not fail the sign-in;
   *  a failed fetch is retried on the next session landing. */
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
   *  without typing the phrase (UX spec 7.1, ADR-0072 "Password replacement
   *  from a device that holds the keys"). The cached 64-byte identity seed
   *  is the recovery OPAQUE input `/password/reset/*` accepts, so the device
   *  proves it holds the keys and the person types only the new password.
   *  Unlike `recover`, nothing on this device is re-keyed or wiped: the
   *  stored device key is kept. */
  replacePassword(
    newPassword: string,
    /** ADR-0075 (UX 19.6): the "It was not me" screen of an account notice
     *  asks nothing but the new password, since this device holds the keys
     *  and the person was just told someone else changed or used the
     *  account's password. */
    options?: {
      readonly afterAccountNotice?: boolean;
      /** A guest that just chose a username sets its FIRST password: there
       *  is no password to prove. Honoured only while the account's
       *  first-password marker is set (`username-claim.ts`). */
      readonly firstPassword?: boolean;
    },
  ): Promise<void>;
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
    /** The pairing's own state to land under the device key BEFORE the
     *  session's identities are installed: the in-memory DeviceGroup leaf of
     *  an anonymous join, and the state bundle (ADR-0074: the account's
     *  earlier keys must be stored before the conversation identity is
     *  minted). Best effort. */
    persistJoinState?: (deviceKey: CryptoKey) => Promise<void>,
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
    /** The redeemed session pair (ADR-0073). */
    readonly session: SessionPair;
    /** Persist the pairing's in-memory DeviceIdentity seed +
     *  DeviceGroup snapshot under the freshly-bootstrapped device
     *  key (`AnonymousJoinSession.persistTo`). Best-effort. */
    readonly persistJoinState?: (deviceKey: CryptoKey) => Promise<void>;
    /** The restore was reached through "Forgot password?": offer the
     *  new-password step once the session lands (UX spec section 7). */
    readonly forgotPassword?: boolean;
  }): Promise<void>;
  /** "Reset account keys" (ADR-0073 section 2.3, UX 15.1): the one path
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
  /** The session pair at the NEW generation (ADR-0073). */
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

/** The `recall-status` kind of an account statement (ADR-0074 section 0.3). */
const ACCOUNT_STATEMENT_RECALL_STATUS = 1;
/** How long a kept device waits before it asks again for the new keys. */
const ADOPTION_RETRY_MS = 20_000;
/** How long the adoption waits for the DeviceGroup handle of a page that
 *  just restored its session. */
const KEY_ROTATION_PORT_WAIT_MS = 20_000;
/** How long an unanswered `recall/finish` is asked again before a new
 *  attempt is opened: the server keeps the outcome for the attempt's own ten
 *  minutes (ADR-0074 T5 item 25); a little under it. */
const RECALL_FINISH_RETRY_WINDOW_MS = 9 * 60 * 1000;

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
    if (next.kind !== "anon" && next.kind !== "loading") {
      // An account landed (a session, or its restore / new-phrase prompt):
      // every sign-in round trip that started before this is overtaken.
      binding.landed();
      openStoresFor(next.account.accountId);
      // Whatever was parked for a DIFFERENT account goes with it. A parked
      // new phrase is not lost by this: it was cached and marked owed before
      // its screen was shown (`mintFreshIdentity`).
      const landed = next.account.accountId;
      const parked = pendingRecovery();
      if (parked !== null && parked.accountIdB64 !== landed) setPendingRecovery(null);
      const fresh = pendingFreshPhrase();
      if (fresh !== null && fresh.accountIdB64 !== landed) {
        fresh.seed.fill(0);
        setPendingFreshPhrase(null);
      }
    }
    bindContactsAccount(next.kind === "session" ? next.account.accountId : null);
    setStateSignal(next);
  };
  // The storage-level backstop of the binding below (`account-store-gate.ts`):
  // Forget device seals an account's stores before it deletes them, a
  // sign-out seals its session-token row, and a landing opens them again.
  onCleanup(installAccountStorageGate());
  /** `accountIdB64` is landing on this browser: its local stores accept
   *  writes again. Called where a flow first holds the account id of a
   *  session it was just given, and at every landing. */
  function openStoresFor(accountIdB64: string): void {
    openAccountStores(accountStoreIds(accountIdB64));
  }
  /** The binding of this browser to an account (`binding-epoch.ts`): ended by
   *  every sign-out (`logout`, `forgetDevice`) and by every landing
   *  (`setState` above). A sign-in flow captures it before its first await
   *  and stops, writing nothing, once it has moved. */
  const binding = new SessionBinding();
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
  // ADR-0075 (UX 19.1a): bumped when the "Protect your account" marker of
  // the signed-in account is written or cleared.
  const [protectVersion, setProtectVersion] = createSignal(0);
  const protectStepOwed = (): boolean => {
    protectVersion();
    const s = state();
    return s.kind === "session" && isProtectStepOwed(plainStorage(), s.account.accountId);
  };
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
  // ADR-0073: this device's keys are not the account's keys any more (a
  // superseded session, or a landing that found other keys published). The
  // guards render the signed-out screen while this is set.
  const [signedOut, setSignedOut] = createSignal<SignedOutState | null>(null);
  // UX 15.3: the keys were reset on this device in this page session.
  const [keyResetNote, setKeyResetNote] = createSignal(false);
  // ADR-0074 part 1: the recall record this page holds open (the "Was it
  // you?" screen), with the device key that seals it; and whether the
  // account was taken back in this page session.
  let recallHeld: { readonly record: RecallRecord; readonly deviceKey: CryptoKey } | null = null;
  const [recallDone, setRecallDone] = createSignal(false);
  // ADR-0073 section 4.2, last paragraph: an attestation older than the
  // client-side bound is applied only after an explicit sign-in in this page
  // (a silent restore is not one).
  let explicitSignInThisPage = false;
  // The guest marker lives in `localStorage`, which is not reactive: every
  // write below bumps this so `isGuest` re-reads it.
  const [guestMarkerVersion, setGuestMarkerVersion] = createSignal(0);
  const guestStorage = plainStorage;
  const guestState = createMemo(() => {
    guestMarkerVersion();
    const s = state();
    if (s.kind !== "session") return null;
    return guestAccountState(guestStorage(), s.account.accountId);
  });

  /** Counts the flows that installed something (a bearer token, identities),
   *  so a landing refused after a sign-out undoes only its own installs. */
  let landingSeq = 0;
  /** Accounts owed their phrase screen whose stored marker could not be
   *  written (storage blocked); holds for this page's lifetime. */
  const phraseOwedInMemory = new Set<string>();
  /** One sign-in flow's view of the binding it started under. */
  interface FlowBinding {
    /** The binding is still the one the flow started under. */
    current(): boolean;
    /** Throws `StaleBindingError` once it is not: the flow is abandoned. */
    require(): void;
  }
  /** Call BEFORE the flow's first await; `require()` after every await,
   *  before any write or state change. */
  function startFlow(): FlowBinding {
    const current = binding.capture();
    return {
      current,
      require: () => {
        if (!current()) throw new StaleBindingError();
      },
    };
  }
  /** The login-handle re-bind of `accessByName`, skipped once the flow that
   *  asked for it was overtaken. */
  function bindIfCurrent(flow: FlowBinding): (token: string, handle: string) => Promise<void> {
    return (token, handle) => (flow.current() ? bindByName(token, handle) : Promise.resolve());
  }
  /** Record that `accountIdB64` is owed its recovery-phrase screen: its next
   *  session landing on this browser shows the phrase from the device-key
   *  wrapped cache before any route (`restoreOwedSignupPhrase`). */
  function owePhraseScreen(accountIdB64: string): void {
    markSignupPhraseOwed(guestStorage(), accountIdB64);
    if (!isSignupPhraseOwed(guestStorage(), accountIdB64)) {
      // eslint-disable-next-line no-console
      console.warn("the owed-phrase marker could not be stored; it holds until this page is closed");
      phraseOwedInMemory.add(accountIdB64);
    }
  }
  /** The account the auth state names, `null` for `anon` / `loading`. */
  function boundAccountId(): string | null {
    const s = state();
    return s.kind === "anon" || s.kind === "loading" ? null : s.account.accountId;
  }
  /** The account the push subscription was asked for is still signed in. */
  function subscribePushFor(accountIdB64: string): Promise<unknown> {
    return subscribeToPush(accountIdB64, () => {
      const s = state();
      return s.kind === "session" && s.account.accountId === accountIdB64;
    });
  }
  /** `parked` is still the context that is parked. A refreshed session pair
   *  replaces the parked object with a copy that carries the new pair
   *  (`sessionTokens` listener, `adoptProofSession`), so the comparison is on
   *  the account-id buffer every copy shares, never on the object. */
  function stillParked(parked: PendingRecovery): boolean {
    return pendingRecovery()?.accountIdBytes === parked.accountIdBytes;
  }
  /** A completion of the restore prompt found its parked context gone: the
   *  person signed out (or another completion landed) while it ran. */
  function requireStillParked(pending: PendingRecovery, flow: FlowBinding): void {
    if (!stillParked(pending)) throw new StaleBindingError();
    flow.require();
  }

  const client = new AuthClient();
  const discovery = new DiscoveryClient();
  /**
   * Install (or drop) the session everywhere a request draws on it: the
   * session token provider (ADR-0073: refresh ahead of expiry, one retry on
   * a 401, the superseded signal), the discovery client's bearer header and
   * the anonymous-token pool (ADR-0054 S3), which draws its tokens WITH this
   * session and spends them without it. A different account swaps the pool
   * to that account's own tokens; `null` stops drawing but keeps what is
   * held until the sign-out has spent its push unregister.
   */
  // ADR-0073 (amended 2026-10-06): the fresh-auth proof. Memory only, never
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
    // A new session for the account: whatever logout was still owed for an
    // earlier one must not sign THIS one out at the next launch.
    clearLogoutOwed(plainStorage(), accountIdB64);
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
  /** ADR-0074 section 0.4: the `device_id_b64` a landing for `target`
   *  asserts, or `undefined` when this browser does not know the device
   *  identity of that account yet (the session is bound afterwards). */
  function landingDevice(target: LandingTarget): string | undefined {
    return landingDeviceId({
      cached: loadPersisted(),
      target,
      knownDeviceId: (accountIdB64) => readKnownDeviceId(plainStorage(), accountIdB64),
    });
  }
  /**
   * ADR-0074 section 0.4: make the held session name THIS device (its
   * conversation leaf key, the id of its directory row). A session that
   * landed before the device identity existed (a signup, a first sign-in on
   * this browser) is replaced once through `bind-device`; one that already
   * names the device costs nothing. `force` skips the token hint (discovery
   * answered `device-mismatch`).
   *
   * Throws what the bind threw when the session could not be bound; the
   * refusals that end the session are acted on here first.
   */
  async function ensureSessionDeviceBound(accountIdB64: string, force = false): Promise<void> {
    const own = chats.identity()?.signaturePublicKey;
    if (own === undefined || sessionTokens.accountIdB64() !== accountIdB64) return;
    const ownId = deviceIdB64Url(own);
    // A due refresh first, so the bearer and the refresh credential sent
    // together are the newest pair.
    await sessionTokens.current();
    const pair = sessionTokens.pair();
    if (pair === null || sessionTokens.accountIdB64() !== accountIdB64) return;
    if (bindDecision({ sessionToken: pair.sessionToken, ownDeviceIdB64: ownId, force }) === "bound") {
      return;
    }
    let bound: SessionPair;
    try {
      bound = await client.sessionBindDevice(pair.sessionToken, pair.refreshToken, ownId);
    } catch (err) {
      if (err instanceof AuthHttpError) {
        const refusal = bindRefusal(err.status, err.errorCode);
        // eslint-disable-next-line no-console
        console.warn(`session bind-device refused (${refusal})`);
        if (refusal === "device-revoked") sessionTokens.reportDeviceRevoked();
        else if (refusal === "superseded") sessionTokens.reportSuperseded();
        else if (refusal === "sign-in-again" || refusal === "session-ended") endExpiredSession();
      }
      throw err;
    }
    // A sign-out or another landing while the bind was out owns the state.
    if (sessionTokens.accountIdB64() !== accountIdB64) return;
    // The unbound pair is dropped: the bound one replaces it everywhere.
    await adoptProofSession(accountIdB64, bound);
  }
  /**
   * ADR-0074 section 0.1: auth records the account's identity key on the
   * FIRST genesis mint and attests only that key afterwards; without a
   * recorded key no device can be removed. So the mint runs at signup and on
   * every landing until it has succeeded once for the key this device holds
   * (a per-account mark in plain storage, keyed on the key, so a key reset
   * mints again for the new one). Returns the attestation when minted.
   */
  async function mintGenesisAndRecord(
    accountIdB64: string,
    sessionToken: string,
    identity: WasmIdentity,
  ): Promise<Uint8Array | undefined> {
    const ownKeyB64 = base64Encode(identity.signaturePublicKey);
    try {
      const attestation = await client.genesisAttest(sessionToken, identity.signaturePublicKey);
      writeGenesisRecorded(plainStorage(), accountIdB64, ownKeyB64);
      return attestation;
    } catch (err) {
      const outcome = genesisMintOutcome({
        ok: false,
        ...(err instanceof AuthHttpError ? { status: err.status, errorCode: err.errorCode } : {}),
      });
      // eslint-disable-next-line no-console
      console.warn(
        outcome === "mismatch"
          ? "genesis-attest: auth holds another identity key for this account"
          : "genesis-attest mint failed; it is tried again at the next landing",
        err instanceof AuthHttpError ? err.status : err,
      );
      return undefined;
    }
  }
  /** The landing half of the rule above: mint unless already recorded for
   *  this key. Never throws. */
  async function ensureGenesisRecorded(accountIdB64: string, canonical: WasmIdentity): Promise<void> {
    try {
      const ownKeyB64 = base64Encode(canonical.signaturePublicKey);
      if (!genesisMintNeeded(readGenesisRecorded(plainStorage(), accountIdB64), ownKeyB64)) return;
      const token = await sessionTokens.current();
      if (token === null || sessionTokens.accountIdB64() !== accountIdB64) return;
      await mintGenesisAndRecord(accountIdB64, token, canonical);
    } catch (err) {
      // eslint-disable-next-line no-console
      console.warn("genesis-attest (landing) failed", err);
    }
  }
  /** `POST /auth/session/logout` for `pair` (ADR-0074 section 2.1), with a
   *  refresh first when the access token expired. Never throws. */
  function serverLogout(pair: SessionPair | null): Promise<LogoutAttempt> {
    return runServerLogout(pair, {
      now: () => Date.now(),
      refresh: async (refreshToken) => {
        try {
          return { token: (await client.sessionRefresh(refreshToken)).sessionToken };
        } catch (err) {
          return { status: err instanceof AuthHttpError ? err.status : null };
        }
      },
      logout: async (sessionToken) => {
        try {
          await client.sessionLogout(sessionToken);
          return 204;
        } catch (err) {
          return err instanceof AuthHttpError ? err.status : null;
        }
      },
    });
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
        case "device-revoked":
          void signOutAfterDeviceRemoved(event.accountIdB64);
          return;
        case "refresh-invalid":
          endExpiredSession();
          return;
      }
    }),
  );
  /** A sign-in flow installing the session it was just given. Counted, so a
   *  landing refused after a sign-out does not take this session down with
   *  its own installs (`finishCanonicalSession`). */
  function installFlowSession(accountIdB64: string, pair: SessionPair): void {
    landingSeq += 1;
    installSession(accountIdB64, pair);
  }
  // ADR-0054 S5: sign-in by name needs the account's login handle bound at
  // auth. Signup and a sign-in by name bind it themselves (and record that
  // here); every other way a session lands - reload restore, passkey, device
  // pairing, another-device confirmation, recovery phrase - is
  // covered by the effect below, which binds the handle of the session's
  // username once per session in this page. Best-effort and invisible: a
  // 409, a 503 or a network error changes nothing for the user.
  const loginHandleSync = createLoginHandleSync({
    handleFor: loginHandleFor,
    check: (token, handle) => client.checkLoginHandle(token, handle),
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

  // ADR-0074 section 0.4: a landing may hand the secrets the pair it was
  // given while `bind-device` has already replaced it. Every reader of
  // `identitySecrets().sessionToken` then gets the live (device-bound) one.
  createEffect(() => {
    const secrets = identitySecrets();
    if (secrets === null) return;
    const live = sessionTokens.pair();
    if (live === null || live.sessionToken === secrets.sessionToken) return;
    if (sessionTokens.accountIdB64() !== base64Encode(secrets.accountId)) return;
    if (!sessionTokens.manages(secrets.sessionToken)) return;
    setIdentitySecrets({
      ...secrets,
      sessionToken: live.sessionToken,
      sessionExpiresAtMs: live.sessionExpiresAtMs,
      keyGeneration: live.keyGeneration,
    });
  });

  // Whatever path installed the session, bind its username's login handle
  // (see `loginHandleSync` above). Runs when the session lands and whenever
  // the token or the username changes; a guest (no username) binds nothing.
  createEffect(() => {
    const s = state();
    const secrets = identitySecrets();
    if (s.kind !== "session" || secrets === null) return;
    const accountIdB64 = s.account.accountId;
    const username = s.account.username;
    void loginHandleSync.ensure(secrets.sessionToken, username).then((outcome) => {
      // The account holds the handle of this name: the name is current.
      if (outcome === "same" || outcome === "bound") {
        setConfirmedUsername({ accountIdB64, username });
      }
    });
  });

  // ADR-0076: the own-username register. A name auth confirmed, with an
  // empty register, seeds it once; a register that names another name than
  // the local marker is adopted (the account was renamed, or its name
  // chosen, on another device).
  const [confirmedUsername, setConfirmedUsername] = createSignal<{
    readonly accountIdB64: string;
    readonly username: string;
  } | null>(null);
  /** The name is the account's on this device from now on: the local
   *  marker, the live session, and the handle marked known for this session
   *  (nothing is bound: whoever changed the name bound it). */
  async function applyOwnUsername(accountIdB64: string, username: string): Promise<void> {
    const s = state();
    if (s.kind !== "session" || s.account.accountId !== accountIdB64) return;
    savePersisted({ v: 1, accountIdB64, username });
    setConfirmedUsername({ accountIdB64, username });
    setState({ kind: "session", account: { accountId: accountIdB64, username } });
    try {
      const token = sessionTokens.peek() ?? identitySecrets()?.sessionToken ?? null;
      if (token !== null) loginHandleSync.markBound(token, (await loginHandleFor(username)).wire);
    } catch {
      // A name the rule refuses cannot be a registered name; nothing to mark.
    }
  }
  createEffect(() => {
    const s = state();
    if (s.kind !== "session") return;
    const accountIdB64 = s.account.accountId;
    const confirmed = confirmedUsername();
    const action = usernameRegisterAction({
      local: s.account.username,
      register: registerUsername(accountIdB64),
      writePending: usernameWritePending(accountIdB64),
      localConfirmed:
        confirmed !== null &&
        confirmed.accountIdB64 === accountIdB64 &&
        confirmed.username === s.account.username,
    });
    if (action.kind === "seed") rememberUsername(accountIdB64, action.username);
    else if (action.kind === "adopt") void applyOwnUsername(accountIdB64, action.username);
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
      // startup is never mistaken for "left every group". The list is the
      // groups this device holds plus the ones a restored backup named that
      // it has not joined yet, so the first upload after a restore does not
      // erase the account's record of its conversations.
      knownGroupIds: () => {
        const id = identitySecrets()?.accountId;
        if (id === undefined || chats.identity() === null) return null;
        return backupKnownGroupIds(
          base64Encode(id),
          chats.groups().map((g) => g.groupIdB64),
        );
      },
      accountIdB64: () => {
        const id = identitySecrets()?.accountId;
        return id === undefined ? null : base64Encode(id);
      },
      // The ACCOUNT identity's signing seed, as the signup and recover
      // baselines carry it - never `chats.identity()`, which since ADR-0050
      // is this device's own leaf identity and belongs to no backup.
      ed25519Seed: () => chats.accountIdentity()?.ed25519Seed ?? null,
      deviceKey: async () => {
        const id = identitySecrets()?.accountId;
        if (id === undefined) return null;
        return getDeviceKey(base64Encode(id));
      },
      sessionToken: () => identitySecrets()?.sessionToken ?? null,
    }),
  );

  // A conversation a restored backup named leaves the "not joined yet" set
  // the moment this device holds it (`backup/restored-known-groups.ts`). The
  // automatic backup prunes on its own tick too; this keeps the set exact in
  // between, so a chat joined and then deleted is not listed again.
  createEffect(() => {
    const id = identitySecrets()?.accountId;
    const held = chats.groups().map((g) => g.groupIdB64);
    if (id === undefined || held.length === 0) return;
    pruneJoinedRestoredGroups(base64Encode(id), held);
  });

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
    // ADR-0050: the row is keyed by this device's leaf key. ADR-0074
    // section 2.2: the session must name that same device, so it is bound
    // first; `device-mismatch` means the hint was wrong (bind for real, try
    // once more), and discovery's `device-revoked` is not proof of removal:
    // auth is asked, and its answer is acted on by the session listener.
    upload: async (ctx, keyPackage) => {
      const send = (): Promise<void> =>
        discovery.uploadKeyPackage(ctx.accountIdBytes, keyPackage, ctx.identity.signaturePublicKey);
      await ensureSessionDeviceBound(ctx.accountIdB64);
      try {
        await send();
      } catch (err) {
        if (err instanceof DiscoveryHttpError && err.status === 403) {
          if (err.errorCode === "device-mismatch") {
            await ensureSessionDeviceBound(ctx.accountIdB64, true);
            await send();
            return;
          }
          if (err.errorCode === "device-revoked") {
            void sessionTokens.refreshOnce();
          }
        }
        throw err;
      }
    },
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
   * No publish-back (ADR-0073 section 4.2): read what the directory
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
    // Compared with the ACCOUNT identity: since ADR-0050 `chats.identity()`
    // is the device identity, never this instance.
    if (chats.accountIdentity() !== canonical) return "unknown";
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
    if (!isSignupPhraseOwed(storage, accountIdB64) && !phraseOwedInMemory.has(accountIdB64)) return;
    // ADR-0075 (alignment I): new words are shown only to a phrase-protected
    // account. The owner of any other account was never shown words, so none
    // are shown after a rotation, a reset or a recall either, and a guest
    // sign-up shows none: the owed marker is dropped, nothing is withheld.
    const st = state();
    const knownUsername =
      st.kind === "session" && st.account.accountId === accountIdB64
        ? st.account.username
        : (loadPersisted()?.username ?? "");
    if (!newWordsShown(isPhraseProtected(storage, accountIdB64), knownUsername.trim().length > 0)) {
      clearSignupPhraseOwed(storage, accountIdB64);
      phraseOwedInMemory.delete(accountIdB64);
      return;
    }
    // A reset whose publish is owed: the identity the phrase belongs to is
    // not findable yet, so the words are withheld until both publishing
    // steps landed (`settleOwedKeyResetPublish` calls back here then).
    if (isKeyResetOwed(storage, accountIdB64)) return;
    // ADR-0074 part 3: the same while a key rotation still owes a step.
    if (readRotation(storage, accountIdB64) !== null) return;
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
    /** ADR-0074 — `true` when the session is already running (a pairing
     *  state bundle just stored the account's earlier keys). The device
     *  identity the conversations use is then replaced only when the
     *  binding is minted again, which happens only while this device holds
     *  no conversation; `null` is returned when nothing was re-minted. */
    live = false,
    /** The caller's binding still holds. Asked after the awaits below: once
     *  it does not, the identities installed by now are not this caller's
     *  (a sign-out freed them, or another landing replaced them), nothing
     *  more is installed and `null` is returned. */
    stillBound: () => boolean = () => true,
  ): Promise<Uint8Array | null> {
    // A live session that already answers to an earlier key learns nothing
    // here, and its predecessor identity may hold state (the account's
    // PublisherGroups after an in-session rotation): left untouched.
    if (live && chats.predecessorAccountIdentity() !== null) return null;
    if (!live) chats.setAccountIdentity(canonical);
    let device: WasmIdentity | null = null;
    // In a live session: whether the device identity is left as it is.
    let keepLiveDevice = live;
    // ADR-0074 section 3.5 (Stage A): after a proven key rotation the
    // conversation leaves stay bound to the account key of before it, and a
    // re-bound device would misjudge its own standing in every group it is
    // in (fact 14). So the binding this device mints is under the OLDEST
    // proven predecessor it holds the seed of; `canonical` (the new key) is
    // the directory identity. A binding it already holds is kept when it
    // names any of the account's own keys, with one exception: a binding
    // under the CURRENT key although predecessors are held (minted before a
    // pairing state bundle brought them) is minted again while this device
    // holds no conversation (`devices/own-conversation-key.ts`).
    let conversationSeedBytes: Uint8Array | null = null;
    let bindingAccount: WasmIdentity = canonical;
    try {
      const deviceKey = await getDeviceKey(accountIdB64);
      if (deviceKey !== null) {
        // ADR-0074 section 3.5a - own predecessors are a chain, oldest
        // first: the conversation key, then the keys held between it and
        // the current one.
        let seeds: Uint8Array[] = [];
        try {
          seeds = predecessorSeeds(await getKeyRotationState(accountIdB64, deviceKey));
        } catch (err) {
          // eslint-disable-next-line no-console
          console.warn("key-rotation state unreadable; treating the account as never rotated", err);
        }
        const predecessors: { identity: WasmIdentity; seed: Uint8Array; keyB64: string }[] = [];
        const currentKeyB64 = base64Encode(canonical.signaturePublicKey);
        for (const [index, seed] of seeds.entries()) {
          const identity = wasm.Identity.fromSeed(seed);
          const keyB64 = base64Encode(identity.signaturePublicKey);
          // The "predecessor" is the key this session holds: nothing rotated.
          if (index === 0 && keyB64 === currentKeyB64) {
            identity.free();
            break;
          }
          if (keyB64 === currentKeyB64 || predecessors.some((p) => p.keyB64 === keyB64)) {
            identity.free();
            continue;
          }
          predecessors.push({ identity, seed, keyB64 });
        }
        const seed = await ensureDeviceIdentitySeed(accountIdB64, deviceKey);
        const stored = await getDeviceBinding(accountIdB64);
        let boundKeyB64: string | null = null;
        /** Overtaken: nothing parsed here was handed to the chats yet. */
        const abandon = (): null => {
          for (const p of predecessors) p.identity.free();
          for (const s of seeds) s.fill(0);
          device?.free();
          device = null;
          return null;
        };
        if (!stillBound()) return abandon();
        if (stored !== null) {
          try {
            device = wasm.Identity.fromSeedWithBinding(seed, stored);
            const bound = device.accountPublicKey;
            boundKeyB64 = bound === undefined ? null : base64Encode(bound);
          } catch (err) {
            // A binding that no longer verifies: re-mint below.
            // eslint-disable-next-line no-console
            console.warn("stored device binding rejected, re-minting", err);
            device = null;
          }
        }
        let holdsConversations = chats.groups().length > 0;
        if (!holdsConversations) {
          try {
            const snapshot = await loadMlsSnapshot(accountIdB64, deviceKey);
            holdsConversations = (snapshot?.knownGroupIds.length ?? 0) > 0;
          } catch {
            // Unreadable: the cautious answer keeps the binding.
            holdsConversations = true;
          }
        }
        if (!stillBound()) return abandon();
        const verdict = bindingVerdict({
          boundKeyB64,
          currentKeyB64,
          predecessorKeysB64: predecessors.map((p) => p.keyB64),
          holdsConversations,
        });
        // The own key the conversation leaves are bound to: the one a kept
        // binding names, else the oldest predecessor; `-1` is the current key.
        let conversationIndex = predecessors.length > 0 ? 0 : -1;
        if (verdict === "keep") {
          conversationIndex = predecessors.findIndex((p) => p.keyB64 === boundKeyB64);
        } else if (verdict === "keep-under-current-key") {
          conversationIndex = -1;
          // eslint-disable-next-line no-console
          console.warn(
            "the device binding names the current account key and this device holds conversations; kept",
          );
        }
        const remint = live ? rebindsAfterStateBundle(verdict) : verdict === "mint" || verdict === "remint";
        if (remint || live) {
          // ADR-0073 section 2.3 step 5 covers `mint`: a binding minted
          // before a key reset names the OLD key (a reset keeps no
          // predecessor) and would publish a KeyPackage nobody can resolve
          // to this account. In a live session the parsed copy is dropped
          // either way: the running identity is kept or replaced below.
          if (device !== null && remint && !live) {
            // eslint-disable-next-line no-console
            console.warn("stored device binding names another account key, re-minting");
          }
          device?.free();
          device = null;
        }
        const conversation = conversationIndex >= 0 ? predecessors[conversationIndex] : undefined;
        if (conversation !== undefined) {
          bindingAccount = conversation.identity;
          conversationSeedBytes = conversation.seed.slice();
        }
        // The predecessor slot first: clearing it clears the chain too.
        chats.setPredecessorAccountIdentity(conversation?.identity ?? null);
        chats.setIntermediatePredecessorIdentities(
          predecessors.filter((p) => p !== conversation).map((p) => p.identity),
        );
        for (const s of seeds) s.fill(0);
        if (remint) {
          keepLiveDevice = false;
          device = wasm.Identity.fromSeedBound(
            seed,
            bindingAccount.ed25519Seed,
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
    if (!stillBound()) {
      (device as WasmIdentity | null)?.free();
      conversationSeedBytes?.fill(0);
      return null;
    }
    if (live && (keepLiveDevice || device === null)) return null;
    if (device === null) {
      // The canonical identity now serves both roles; `setIdentity` would
      // free the instance `setAccountIdentity` holds, so hand the chats a
      // second instance of the same seed.
      chats.setIdentity(wasm.Identity.fromSeed(canonical.ed25519Seed));
    } else {
      chats.setIdentity(device);
    }
    // ADR-0074 section 0.4: the id this device's sessions name (its leaf
    // key, the id of its directory row), recorded so the next landing for
    // this account can assert it before any wrapped store is opened.
    const installed = chats.identity()?.signaturePublicKey;
    if (installed !== undefined) {
      writeKnownDeviceId(plainStorage(), accountIdB64, deviceIdB64Url(installed));
    }
    return conversationSeedBytes;
  }

  /**
   * ADR-0073: a reset whose publishing steps did not all succeed leaves a
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
   * The publishing half of a key reset (ADR-0073 section 2.3 steps 3-5,
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
    // attestation (soft-fail), plus the ADR-0073 attestation that lets
    // discovery accept a key change without a supersession proof.
    const leafAuth = await mintLeafGenesisAuth(canonical, accountIdBytes);
    // ADR-0074 section 0.1: `reset/finish` already recorded the new key at
    // auth; this mint is the idempotent call for it and writes the mark.
    const genesisAttest = await mintGenesisAndRecord(
      base64Encode(accountIdBytes),
      sessionTokens.peek() ?? "",
      canonical,
    );
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
  // ADR-0073 section 3.5: the old device signs out and wipes
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
  async function wipeAfterSignOut(
    accountIdB64: string,
    mode: "full" | "staged" = "full",
  ): Promise<void> {
    // A sign-out like any other: a session landing still in flight must not
    // land after this.
    binding.signedOut();
    phraseOwedInMemory.delete(accountIdB64);
    dropSession();
    // ADR-0042, "A wake is shown only for the signed-in account": the local
    // service-worker wake binding goes first (no network), so every wake
    // from here on is dropped and this account's notifications leave the
    // screen with it.
    await releaseWakeBinding();
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
    // ADR-0074 section 1.1: the staged wipe is the same list minus the
    // recall record and the device key that seals it.
    if (mode === "staged") await wipeAccountStaged(accountIdB64);
    else await wipeAccountLocalData(accountIdB64);
    try {
      await unsubscribeFromPush();
    } catch {
      // best-effort; the localStorage pseudonym is dropped regardless.
    }
    anonTokens.forgetAccount(accountIdB64);
    setState({ kind: "anon" });
  }

  /**
   * The recall record a superseded device keeps (ADR-0074 section 1.1), or
   * `null` when it keeps none: no device key, no account key on this device,
   * or the record could not be written. An existing record (another tab of
   * this account wrote it, or an earlier page) is used as it is, unless it
   * has outlived the window. Must run BEFORE the wipe: the account seed the
   * record is made from is among what the wipe deletes.
   */
  async function prepareRecallRecord(
    accountIdB64: string,
    username: string,
  ): Promise<{ readonly record: RecallRecord; readonly deviceKey: CryptoKey } | null> {
    let deviceKey: CryptoKey | null = null;
    try {
      deviceKey = await getDeviceKey(accountIdB64);
    } catch {
      deviceKey = null;
    }
    if (deviceKey === null) return null;
    try {
      const existing = await getRecallRecord(accountIdB64, deviceKey);
      if (existing !== null) {
        return recallExpired(existing.learnedAtMs, Date.now()) ? null : { record: existing, deviceKey };
      }
    } catch {
      // An unreadable row is replaced below, or wiped with everything else.
    }
    const secrets = identitySecrets();
    let seed: Uint8Array | null =
      secrets !== null &&
      base64Encode(secrets.accountId) === accountIdB64 &&
      secrets.accountIdentitySeed.length === ACCOUNT_IDENTITY_SEED_LENGTH
        ? secrets.accountIdentitySeed.slice()
        : null;
    if (seed === null) {
      try {
        seed = await getAccountIdentitySeed(accountIdB64, deviceKey);
      } catch {
        seed = null;
      }
    }
    if (
      signOutWipe({ cause: "keys-reset", holdsAccountKey: seed !== null, deviceKeyPresent: true }) !==
        "staged" ||
      seed === null
    ) {
      return null;
    }
    try {
      const wasm = await loadWasm();
      const identity = wasm.Identity.fromSeed(seed);
      let oldSeed: Uint8Array;
      try {
        oldSeed = identity.ed25519Seed;
      } finally {
        identity.free();
      }
      const record: RecallRecord = { accountIdB64, oldSeed, username, learnedAtMs: Date.now() };
      await putRecallRecord(record, deviceKey);
      return { record, deviceKey };
    } catch (err) {
      // eslint-disable-next-line no-console
      console.warn(
        "the recall record could not be written; wiping everything",
        err instanceof Error ? err.name : "unknown",
      );
      return null;
    } finally {
      seed.fill(0);
    }
  }

  /** Whether the session pair STORED for the account is of a higher key
   *  generation than the one this page was just refused with. Any doubt
   *  reads `false` (the wipe runs). */
  async function newerKeysStoredByAnotherTab(
    accountIdB64: string,
    supersededGeneration: bigint | null,
  ): Promise<boolean> {
    if (supersededGeneration === null) return false;
    try {
      const deviceKey = await getDeviceKey(accountIdB64);
      if (deviceKey === null) return false;
      const stored = await getSessionPair(accountIdB64, deviceKey);
      return stored !== null && stored.keyGeneration > supersededGeneration;
    } catch {
      return false;
    }
  }

  /**
   * A `401 key-generation-superseded`, or a landing that found a higher
   * generation published. The screen, then the wipe, by itself, once per
   * page: STAGED on a device that held the account's keys (everything goes
   * but the recall record, then "Was it you?"), FULL on one that never did
   * (the signed-out screen, then "Sign in").
   */
  async function signOutAfterKeyReset(accountIdB64: string): Promise<void> {
    const current = signedOut();
    if (current !== null && (current.reason === "keys-reset" || current.reason === "recall")) return;
    // ADR-0074 section 3.3: "superseded" is also what a kept device is told
    // after the keys were changed WITH the devices kept. It adopts the new
    // keys when it holds (or finds in its DeviceGroup journal) the envelope
    // for them; only otherwise is this a reset.
    const adoption = await adoptRotationOnce(accountIdB64);
    if (adoption === "adopted") return;
    if (adoption === "device-revoked") {
      await signOutAfterDeviceRemoved(accountIdB64);
      return;
    }
    if (adoption === "sign-in") {
      setKeysUpdating(false);
      endExpiredSession();
      return;
    }
    if (adoption === "later") {
      // No verdict (offline, the journal not read yet): never a wipe.
      setKeysUpdating(true);
      if (adoptionRetryTimer !== null) clearTimeout(adoptionRetryTimer);
      adoptionRetryTimer = setTimeout(() => {
        adoptionRetryTimer = null;
        void signOutAfterKeyReset(accountIdB64);
      }, ADOPTION_RETRY_MS);
      return;
    }
    setKeysUpdating(false);
    const username = signedOutUsername(accountIdB64);
    // eslint-disable-next-line no-console
    console.warn("the account's keys were reset on another device; this device is signed out and wiped");
    const supersededGeneration =
      (sessionTokens.pair() ?? sessionTokens.supersededPair(accountIdB64))?.keyGeneration ?? null;
    setSignedOut({ reason: "keys-reset", phase: "removing", username });
    if (await newerKeysStoredByAnotherTab(accountIdB64, supersededGeneration)) {
      // Another tab of this browser already took the account back (or
      // reset its keys) and stored the new generation: this page is only
      // stale. Wiping here would destroy that tab's new keys.
      // eslint-disable-next-line no-console
      console.warn("a newer key generation is stored on this device; reloading instead of wiping");
      dropSession();
      if (typeof window !== "undefined") window.location.reload();
      return;
    }
    let kept: Awaited<ReturnType<typeof prepareRecallRecord>> = null;
    try {
      kept = await prepareRecallRecord(accountIdB64, username);
      await wipeAfterSignOut(accountIdB64, kept === null ? "full" : "staged");
    } finally {
      if (kept !== null) {
        recallHeld = kept;
        setSignedOut({
          reason: "recall",
          username: kept.record.username,
          learnedAtMs: kept.record.learnedAtMs,
        });
      } else {
        setSignedOut({ reason: "keys-reset", phase: "removed", username });
      }
    }
  }

  /** One recall action at a time across the tabs of this browser, where the
   *  platform has Web Locks; within the page otherwise. */
  let recallQueue: Promise<unknown> = Promise.resolve();
  function withRecallLock<T>(accountIdB64: string, run: () => Promise<T>): Promise<T> {
    const queued = recallQueue.then(
      () => runUnderRecallWebLock(accountIdB64, run),
      () => runUnderRecallWebLock(accountIdB64, run),
    );
    recallQueue = queued.catch(() => undefined);
    return queued;
  }
  function runUnderRecallWebLock<T>(accountIdB64: string, run: () => Promise<T>): Promise<T> {
    const locks = typeof navigator === "undefined" ? undefined : navigator.locks;
    if (locks === undefined) return run();
    return locks.request(`lakko-recall:${accountIdB64}`, run) as Promise<T>;
  }

  /**
   * The record is gone although this page still shows its screen: another
   * tab of this browser answered (it took the account back, or finished the
   * wipe). Nothing is wiped from here; the page reads the stores again.
   */
  function recallResolvedElsewhere(): void {
    recallHeld?.record.oldSeed.fill(0);
    recallHeld = null;
    setSignedOut(null);
    setState({ kind: "loading" });
    void restoreSessionOnLoad();
  }

  /** Destroy the recall record and finish the wipe (the device key and
   *  whatever a failed step left), then the signed-out screen's last state. */
  async function finishRecallWipe(notice: RecallEndNotice | null): Promise<void> {
    const held = recallHeld;
    if (held === null) return;
    const accountIdB64 = held.record.accountIdB64;
    await withRecallLock(accountIdB64, async () => {
      if (recallHeld !== held) return;
      let present = true;
      try {
        present = await hasRecallRecord(accountIdB64);
      } catch {
        present = true;
      }
      if (!present) {
        recallResolvedElsewhere();
        return;
      }
      const username = held.record.username;
      const ending = notice === null ? {} : { notice };
      setSignedOut({ reason: "keys-reset", phase: "removing", username, ...ending });
      held.record.oldSeed.fill(0);
      recallHeld = null;
      try {
        await wipeAccountLocalData(accountIdB64);
      } finally {
        setSignedOut({ reason: "keys-reset", phase: "removed", username, ...ending });
      }
    });
  }

  /** `POST /account/recall/status` for the held record. */
  async function recallCheck(): Promise<RecallCheck> {
    const held = recallHeld;
    if (held === null) return { kind: "unknown", offline: false };
    if (recallExpired(held.record.learnedAtMs, Date.now())) return { kind: "closed" };
    try {
      const wasm = await loadWasm();
      const accountIdBytes = base64Decode(held.record.accountIdB64);
      // T5 item 24: `statement-stale` is re-signed at the server's time; it
      // never closes the record.
      const body = await client.recallStatus((signAtMs) =>
        signAtMs === null
          ? wasm.signAccountStatement(
              held.record.oldSeed,
              ACCOUNT_STATEMENT_RECALL_STATUS,
              accountIdBytes,
              new Uint8Array(0),
            )
          : wasm.signAccountStatementAt(
              held.record.oldSeed,
              ACCOUNT_STATEMENT_RECALL_STATUS,
              accountIdBytes,
              new Uint8Array(0),
              BigInt(signAtMs),
            ),
      );
      return recallStatusVerdict({ status: 200, errorCode: null, body }, Date.now());
    } catch (err) {
      if (err instanceof AuthHttpError) {
        return recallStatusVerdict(
          { status: err.status, errorCode: err.errorCode, body: null },
          Date.now(),
        );
      }
      if (!(err instanceof AuthNetworkError)) {
        // eslint-disable-next-line no-console
        console.warn("recall status failed", err instanceof Error ? err.name : "unknown");
      }
      return { kind: "unknown", offline: err instanceof AuthNetworkError };
    }
  }

  /** A `recall/finish` that was sent and not (yet) answered, with the phrase
   *  it would make the account's. In memory only: a reload loses it, and the
   *  next attempt is a new one. */
  let recallPending: {
    readonly accountIdB64: string;
    readonly finish: RecallPendingFinish;
    readonly seed: Uint8Array;
    readonly phrase: string;
    readonly newPubkey: Uint8Array;
    readonly preparedAtMs: number;
    /** The anchor's phrase arm the attempt was checked against: recorded
     *  once the recall landed (the account's stores are sealed until then). */
    readonly phraseArm: boolean;
  } | null = null;

  /** Take the account back (ADR-0074 part 1). From the server's answer on
   *  it is the key reset's client order (`adoptKeyResetOutcome`). */
  function recallTakeBack(
    oldPassword: string,
    newPassword: string,
    options?: { readonly phrase?: string },
  ): Promise<RecallAttemptFailure | null> {
    const held = recallHeld;
    if (held === null) return Promise.resolve({ kind: "closed" });
    return runRecall(
      {
        accountIdB64: held.record.accountIdB64,
        username: held.record.username,
        oldSeed: held.record.oldSeed,
        deviceKey: held.deviceKey,
        check: recallCheck,
        isCurrent: () => recallHeld === held,
        afterPersist: async () => {
          // The server answered 200 and the new keys are stored: the record
          // has done its work.
          held.record.oldSeed.fill(0);
          recallHeld = null;
          try {
            await deleteRecallRecord(held.record.accountIdB64);
          } catch (err) {
            // eslint-disable-next-line no-console
            console.warn(
              "the recall record could not be deleted",
              err instanceof Error ? err.name : "unknown",
            );
          }
          setRecallDone(true);
          setSignedOut(null);
        },
      },
      oldPassword,
      newPassword,
      options?.phrase ?? null,
    );
  }

  /** What one recall runs against: the recall record of an old device, or
   *  (ADR-0075 section 8) the key a typed phrase derives. */
  interface RecallTarget {
    readonly accountIdB64: string;
    readonly username: string;
    /** The OLD account identity's 32-byte Ed25519 seed. */
    readonly oldSeed: Uint8Array;
    /** The account's device key on this device, when it has one. */
    readonly deviceKey: CryptoKey | null;
    readonly check: () => Promise<RecallCheck>;
    /** Whether the target is still the one to act on. */
    readonly isCurrent: () => boolean;
    readonly afterPersist: () => Promise<void>;
  }

  /** The 64-byte seed of a typed phrase, or `null` when the words are not a
   *  recovery phrase. */
  async function seedOfTypedPhrase(phrase: string): Promise<Uint8Array | null> {
    const wasm = await loadWasm();
    try {
      const mnemonic = wasm.RecoveryMnemonic.fromPhrase(phrase);
      try {
        return mnemonic.toSeed();
      } finally {
        mnemonic.free();
      }
    } catch {
      return null;
    }
  }

  /** `oldPhrase` non-null is the phrase arm: the login is made from the
   *  phrase's seed and `oldPassword` is not used. */
  function runRecall(
    target: RecallTarget,
    oldPassword: string,
    newPassword: string,
    oldPhrase: string | null,
  ): Promise<RecallAttemptFailure | null> {
    const accountIdB64 = target.accountIdB64;
    return withRecallLock(accountIdB64, async (): Promise<RecallAttemptFailure | null> => {
      if (!target.isCurrent()) return { kind: "closed" };
      let oldPhraseSeed: Uint8Array | null = null;
      if (oldPhrase !== null) {
        oldPhraseSeed = await seedOfTypedPhrase(oldPhrase);
        // Not a phrase at all: the same line as a wrong one, nothing sent.
        if (oldPhraseSeed === null) return { kind: "wrong-password" };
      }
      const accountIdBytes = base64Decode(accountIdB64);
      const failureOf = (err: unknown): RecallAttemptFailure => {
        if (err instanceof RecallWrongPasswordError) {
          return recallAttemptFailure({
            wrongPasswordLocally: true,
            status: null,
            errorCode: null,
            retryAfterSecs: null,
          });
        }
        if (err instanceof AuthHttpError) {
          return recallAttemptFailure({
            wrongPasswordLocally: false,
            status: err.status,
            errorCode: err.errorCode,
            retryAfterSecs: err.retryAfter,
          });
        }
        if (err instanceof AuthNetworkError) {
          return recallAttemptFailure({
            wrongPasswordLocally: false,
            status: null,
            errorCode: null,
            retryAfterSecs: null,
          });
        }
        // eslint-disable-next-line no-console
        console.warn("recall failed", err instanceof Error ? err.name : "unknown");
        return { kind: "error" };
      };
      const dropPending = (): void => {
        const p = recallPending;
        if (p === null) return;
        p.seed.fill(0);
        p.finish.dispose();
        recallPending = null;
      };
      let reset: RecallResult | null = null;
      let seed: Uint8Array;
      let phrase: string;
      let newPubkey: Uint8Array;
      let phraseArm: boolean;
      // ADR-0074 T5 item 25: a finish that was sent and never answered may
      // have LANDED. Ask it again with the same bytes (the server answers an
      // identical finish 200 again for the attempt's ten minutes) before
      // anything new is generated: the phrase of that attempt is the
      // account's if it did.
      const earlier = recallPending;
      if (earlier !== null && earlier.accountIdB64 === accountIdB64) {
        if (Date.now() - earlier.preparedAtMs < RECALL_FINISH_RETRY_WINDOW_MS) {
          try {
            reset = await client.recallFinish(earlier.finish);
          } catch (err) {
            if (err instanceof AuthNetworkError || (err instanceof AuthHttpError && err.status >= 500)) {
              // Still no answer: keep the request and the phrase.
              return failureOf(err);
            }
            // The server chose an answer: that attempt did not land (or its
            // ten minutes are over). A new attempt follows.
          }
        }
      }
      if (reset !== null && earlier !== null) {
        seed = earlier.seed.slice();
        phrase = earlier.phrase;
        newPubkey = earlier.newPubkey;
        phraseArm = earlier.phraseArm;
        dropPending();
      } else {
        dropPending();
        // The anchor to prove against, confirmed now.
        const check = await target.check();
        if (check.kind === "closed" || check.kind === "exhausted") return { kind: "closed" };
        if (check.kind === "unknown") return check.offline ? { kind: "offline" } : { kind: "error" };
        // The phrase proves nothing on an anchor without the phrase arm
        // (the account was mail-protected): nothing is sent.
        if (oldPhraseSeed !== null && !check.status.phraseArm) return { kind: "wrong-password" };
        // Whether the account had a saved phrase when its keys were reset
        // (the anchor's phrase arm): only then are the NEW words shown after
        // the recall. Taken before anything is sent and kept with the
        // attempt, so an attempt whose answer is lost and found later reads
        // the same. Written to the store once the recall landed: the staged
        // wipe sealed the account's stores (`account-store-gate.ts`), and a
        // write made now would be dropped.
        phraseArm = check.status.phraseArm;
        const wasm = await loadWasm();
        // A brand-new phrase and identity: the old key authorises the recall
        // and does not return (ADR-0074 section 1.2).
        const mnemonic = wasm.RecoveryMnemonic.generate();
        phrase = mnemonic.phrase;
        seed = mnemonic.toSeed();
        mnemonic.free();
        const canonical = wasm.Identity.fromSeed(seed);
        newPubkey = canonical.signaturePublicKey;
        canonical.free();
        let prepared: RecallPendingFinish;
        try {
          prepared = await client.recallPrepare({
            accountId: accountIdBytes,
            oldAccountSeed: target.oldSeed,
            anchorSeq: check.status.anchorSeq,
            oldPassword: utf8(oldPassword),
            ...(oldPhraseSeed === null ? {} : { oldPhraseSeed }),
            newPassword: utf8(newPassword),
            newSeed: seed.slice(),
            newSignaturePubkey: newPubkey,
          });
        } catch (err) {
          seed.fill(0);
          return failureOf(err);
        }
        // From here the request may land without its answer reaching this
        // device: the request bytes and the phrase are kept until it does.
        recallPending = {
          accountIdB64,
          finish: prepared,
          seed: seed.slice(),
          phrase,
          newPubkey,
          preparedAtMs: Date.now(),
          phraseArm,
        };
        try {
          reset = await client.recallFinish(prepared);
        } catch (err) {
          seed.fill(0);
          if (!(err instanceof AuthNetworkError || (err instanceof AuthHttpError && err.status >= 500))) {
            // An answer the server chose: the attempt is over.
            dropPending();
          }
          return failureOf(err);
        }
        dropPending();
      }
      // The account is this device's again: the stores the staged wipe
      // sealed open here, before the first write of the landing. The staged
      // wipe dropped the restore marker; a reload must find the account.
      oldPhraseSeed?.fill(0);
      openStoresFor(accountIdB64);
      setPhraseProtected(plainStorage(), accountIdB64, phraseArm);
      savePersisted({ v: 1, accountIdB64, username: target.username });
      explicitSignInThisPage = true;
      if (reset.freshGrant !== null) freshGrants.put(accountIdB64, reset.freshGrant);
      await adoptKeyResetOutcome({
        accountIdB64,
        accountIdBytes,
        username: target.username,
        deviceKey: target.deviceKey,
        seed,
        phrase,
        newPubkey,
        reset,
        afterPersist: target.afterPersist,
      });
      return null;
    });
  }

  /** A statement of `kind` by the key `oldSeed` signs with, for a device
   *  that names the account by login handle: the account field carries the
   *  first 16 bytes of the handle (ADR-0075 section 8). */
  async function recallStatusByHandle(
    handle: LoginHandle,
    oldSeed: Uint8Array,
  ): Promise<RecallCheck> {
    try {
      const wasm = await loadWasm();
      const stated = handle.bytes.slice(0, 16);
      const body = await client.recallStatus(
        (signAtMs) =>
          signAtMs === null
            ? wasm.signAccountStatement(oldSeed, ACCOUNT_STATEMENT_RECALL_STATUS, stated, new Uint8Array(0))
            : wasm.signAccountStatementAt(
                oldSeed,
                ACCOUNT_STATEMENT_RECALL_STATUS,
                stated,
                new Uint8Array(0),
                BigInt(signAtMs),
              ),
        handle.wire,
      );
      return recallStatusVerdict({ status: 200, errorCode: null, body }, Date.now());
    } catch (err) {
      if (err instanceof AuthHttpError) {
        return recallStatusVerdict(
          { status: err.status, errorCode: err.errorCode, body: null },
          Date.now(),
        );
      }
      if (!(err instanceof AuthNetworkError)) {
        // eslint-disable-next-line no-console
        console.warn("recall status by handle failed", err instanceof Error ? err.name : "unknown");
      }
      return { kind: "unknown", offline: err instanceof AuthNetworkError };
    }
  }

  /** The login handle of a typed name and the OLD account key's Ed25519
   *  seed a typed phrase derives; `null` when either is not what it claims
   *  to be. */
  async function phraseRecallInputs(
    username: string,
    phrase: string,
  ): Promise<{ target: NameTarget; oldSeed: Uint8Array } | null> {
    const seed = await seedOfTypedPhrase(phrase);
    if (seed === null) return null;
    try {
      const wasm = await loadWasm();
      return { target: await nameTargetFor(username), oldSeed: ed25519SeedOf(wasm, seed) };
    } catch {
      return null;
    } finally {
      seed.fill(0);
    }
  }

  /** UX 19.7: is the typed phrase the account's from before a reset? */
  async function recallCheckByPhrase(username: string, phrase: string): Promise<RecallCheck> {
    const inputs = await phraseRecallInputs(username, phrase);
    if (inputs === null) return { kind: "closed" };
    try {
      const check = await recallStatusByHandle(inputs.target.handle, inputs.oldSeed);
      // Only an anchor with the phrase arm, and an answer that names the
      // account, can be acted on from here.
      if (check.kind === "open" && (!check.status.phraseArm || check.status.accountIdB64Url === null)) {
        return { kind: "closed" };
      }
      return check;
    } finally {
      inputs.oldSeed.fill(0);
    }
  }

  /** UX 19.7: the recall with the phrase alone, from a device that holds
   *  nothing of the account. */
  async function recallTakeBackByPhrase(
    username: string,
    phrase: string,
    newPassword: string,
  ): Promise<RecallAttemptFailure | null> {
    const inputs = await phraseRecallInputs(username, phrase);
    if (inputs === null) return { kind: "wrong-password" };
    const check = () => recallStatusByHandle(inputs.target.handle, inputs.oldSeed);
    const first = await check();
    if (first.kind === "unknown") {
      inputs.oldSeed.fill(0);
      return first.offline ? { kind: "offline" } : { kind: "error" };
    }
    if (first.kind !== "open" || !first.status.phraseArm || first.status.accountIdB64Url === null) {
      inputs.oldSeed.fill(0);
      return { kind: "closed" };
    }
    const accountIdBytes = base64UrlDecodeNoPad(first.status.accountIdB64Url);
    const accountIdB64 = base64Encode(accountIdBytes);
    // A device key this browser already holds for the account is reused
    // (minting another would orphan what it wraps); else one is derived
    // from the new seed.
    let deviceKey: CryptoKey | null = null;
    try {
      deviceKey = await getDeviceKey(accountIdB64);
    } catch {
      deviceKey = null;
    }
    try {
      return await runRecall(
        {
          accountIdB64,
          username: inputs.target.username,
          oldSeed: inputs.oldSeed,
          deviceKey,
          check,
          isCurrent: () => true,
          afterPersist: async () => {
            setRecallDone(true);
          },
        },
        "",
        newPassword,
        phrase,
      );
    } finally {
      inputs.oldSeed.fill(0);
    }
  }

  /**
   * ADR-0074 section 2.3 (UX 18.2): this device was removed from the account
   * on another device. Auth said so (`401 device-revoked`), or this device
   * applied its own removal from the DeviceGroup. Stop every writer, show
   * the removed screen, and wipe EVERYTHING by itself: no button, and no
   * recall record (a removal changes no keys, so there is nothing to take
   * back). The same wipe as after a key reset, which already makes no call
   * that needs a session. Once per page.
   */
  async function signOutAfterDeviceRemoved(accountIdB64: string): Promise<void> {
    const current = signedOut();
    if (current !== null && current.reason !== "identity-mismatch") return;
    const username = signedOutUsername(accountIdB64);
    // eslint-disable-next-line no-console
    console.warn("this device was removed from the account; it is signed out and wiped");
    setSignedOut({ reason: "device-removed", phase: "removing", username });
    try {
      await wipeAfterSignOut(accountIdB64);
    } finally {
      setSignedOut({ reason: "device-removed", phase: "removed", username });
    }
  }

  /** A different key at the same generation (ADR-0073 4.2 rule 4): block,
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
    /** The recovery phrase, when this landing came from the phrase prompt
     *  (`provideRecoveryPhrase`): the account-backup restore it owes runs
     *  with it and is awaited. Every other landing leaves it out and retries
     *  an owed restore, if any, from the device's phrase cache. */
    restorePhrase?: string;
    /** A guest account landing in its call (`guest-account.ts`): no push
     *  registration. A person who was sent a call link is not asked whether
     *  this site may send notifications. */
    guest?: boolean;
    /** The landing of an account whose registration has just finished on the
     *  server (guest signup). It is completed whatever else landed meanwhile;
     *  only a sign-out stops it. */
    registered?: boolean;
  }): Promise<void> {
    // A landing runs across many awaits, and Sign out is on screen during
    // some of them (the restore prompt). A sign-out that lands in one must
    // win: nothing below may install a session after it. So must another
    // account's landing: this one was started for a binding that is gone.
    const notSignedOut = binding.captureSignOut();
    const stillBound = args.registered === true ? notSignedOut : binding.capture();
    const mine = ++landingSeq;
    const requireNotSignedOut = (): void => {
      if (stillBound()) return;
      // Undo what THIS landing installed after the sign-out cleared the
      // session - unless a later flow has installed since, whose installs
      // these would be, or an account is on screen (it landed meanwhile and
      // owns what is installed now).
      if (!notSignedOut() && landingSeq === mine && boundAccountId() === null) {
        keyPackageAccount = null;
        chats.clear();
        dropSession();
      }
      throw new StaleBindingError();
    };
    openStoresFor(args.accountIdB64);
    const canonical = args.wasm.Identity.fromSeed(args.accountIdentitySeed);
    // The keys are on this device now; nothing is left to explain.
    setRestoreNotice(null);
    const conversationIdentitySeed = await installSessionIdentities(
      args.wasm,
      canonical,
      args.accountIdB64,
      false,
      stillBound,
    );
    requireNotSignedOut();
    keyPackageAccount = {
      accountIdBytes: args.accountIdBytes,
      accountIdB64: args.accountIdB64,
    };
    installSession(args.accountIdB64, args.session);

    // Rehydrate BEFORE any KeyPackage is minted: `restoreStorage` REPLACES the
    // provider storage, so a KeyPackage created earlier would lose its private
    // key to it. This is the device's OWN locally persisted MLS state; an
    // account backup never supplies any (`backup/restore.ts`).
    await rehydrateMlsState(chats, args.accountIdB64);
    requireNotSignedOut();

    await restoreOwedSignupPhrase(args.accountIdB64);
    requireNotSignedOut();
    settleOwedKeyResetPublish(args.accountIdB64, args.accountIdBytes, args.accountIdentitySeed);

    const secrets: IdentitySecrets = {
      accountId: args.accountIdBytes,
      exportKey: args.exportKey,
      accountIdentitySeed: args.accountIdentitySeed,
      ...(conversationIdentitySeed !== null ? { conversationIdentitySeed } : {}),
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
      void restoreOwedBackupOnLanding(args);
      void (async (): Promise<void> => {
        // ADR-0073 section 4.2: the published identity is read BEFORE any
        // upload, a reload included; a stale device shows the signed-out
        // screen over the session that just landed.
        const verdict = await checkPublished(true);
        if (chats.accountIdentity() === canonical && !landingSignsOut(verdict)) {
          // ADR-0074: the restored session names this device (it was bound
          // when it landed; a no-op then), and the account key is recorded
          // at auth (until it has succeeded once).
          await ensureSessionDeviceBound(args.accountIdB64).catch((err: unknown) => {
            // eslint-disable-next-line no-console
            console.warn("session bind-device (session restore) failed", err);
          });
          await ensureGenesisRecorded(args.accountIdB64, canonical);
        }
        // (The account identity, as in `refreshPublishedIdentity`.)
        if (chats.accountIdentity() !== canonical || landingSignsOut(verdict)) return;
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
    // the separate post-login snapshot write used to do. The upload binds the
    // session to this device first (ADR-0074 section 0.4).
    await publishKeyPackageBestEffort("sign-in");
    requireNotSignedOut();
    // The bind may have ended the session (this device was removed, or the
    // session named another device): nothing lands then.
    if (sessionTokens.accountIdB64() !== args.accountIdB64) return;
    // ADR-0074 section 0.1: record the account key at auth, on every landing
    // until it has succeeded once.
    await ensureGenesisRecorded(args.accountIdB64, canonical);
    requireNotSignedOut();

    setIdentitySecrets(secrets);
    setPendingRecovery(null);
    setState({
      kind: "session",
      account: { accountId: args.accountIdB64, username: args.username },
    });
    if (args.guest !== true) {
      void subscribePushFor(args.accountIdB64).catch((err) => {
        // eslint-disable-next-line no-console
        console.warn("push subscribe failed", err);
      });
    }
    // The account backup, on every route that restores the identity from
    // the phrase: awaited when this landing holds the phrase (the prompt,
    // an email code, a passkey that carries the keys), a background retry
    // otherwise. Never fails the sign-in.
    const owedRestore = restoreOwedBackupOnLanding(args);
    if (args.restorePhrase !== undefined) await owedRestore;
  }

  /**
   * The session-landing step of the account-backup restore
   * (`backup/restore.ts`). A no-op unless a fetch of this account's backup
   * is owed: a route that restores the identity marks it just before it
   * lands the session, and a fetch that failed (or a tab closed
   * mid-restore) leaves it set, so the next landing - reload, sign-in,
   * passkey - runs it once more. Never throws. Contacts, synced state and
   * the list of conversations only: MLS state is never loaded from a backup.
   */
  function restoreOwedBackupOnLanding(args: {
    accountIdBytes: Uint8Array;
    accountIdB64: string;
    session: SessionPair;
    restorePhrase?: string;
  }): Promise<void> {
    return restoreOwedAccountBackup({
      accountIdBytes: args.accountIdBytes,
      accountIdB64: args.accountIdB64,
      sessionToken: args.session.sessionToken,
      deviceKey: () => getDeviceKey(args.accountIdB64),
      ...(args.restorePhrase !== undefined ? { phrase: args.restorePhrase } : {}),
    }).then(() => undefined);
  }

  /**
   * Upload a baseline of the account backup under `phrase` (contacts, synced
   * state and the list of conversations; never MLS state). Run by every
   * route that ends with the keys on this device under a phrase the server's
   * blob is not sealed with yet: the first sign-up, a key reset, a key
   * rotation, a recall, a guest account that was just kept. Skipped while a
   * restore of the account's backup is still owed, so the blob that restore
   * has to read is not replaced. Best effort: never throws.
   */
  async function uploadBackupBaseline(args: {
    accountIdBytes: Uint8Array;
    accountIdB64: string;
    accountIdentitySeed: Uint8Array;
    deviceKey: CryptoKey;
    sessionToken: string;
    phrase: string;
    context: string;
  }): Promise<void> {
    try {
      if (isBackupRestoreOwed(args.accountIdB64)) return;
      const wasm = await loadWasm();
      const identity = wasm.Identity.fromSeed(args.accountIdentitySeed);
      let ed25519Seed: Uint8Array;
      try {
        ed25519Seed = identity.ed25519Seed;
      } finally {
        identity.free();
      }
      try {
        await bootstrapBackupFromPhraseString(
          {
            accountId: args.accountIdBytes,
            accountIdB64: args.accountIdB64,
            ed25519Seed,
            deviceKey: args.deviceKey,
            sessionToken: args.sessionToken,
            knownGroupIds: backupKnownGroupIds(
              args.accountIdB64,
              chats.groups().map((g) => g.groupIdB64),
            ),
          },
          args.phrase,
        );
      } finally {
        ed25519Seed.fill(0);
      }
    } catch (err) {
      // eslint-disable-next-line no-console
      console.warn(`backup baseline (${args.context}) failed`, err);
    }
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
    /** The binding the recovery round trip started under; the caller has
     *  just checked it. */
    flow: FlowBinding,
    restoreKeys?: () => Promise<boolean>,
  ): Promise<void> {
    openStoresFor(accountIdB64);
    // Install the fresh session everywhere a request draws on it.
    installFlowSession(accountIdB64, session);
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
    flow.require();
    let accountIdentitySeed: Uint8Array | null = null;
    if (deviceKey !== null) {
      // Refresh the persisted session pair under the existing device key.
      // Best-effort, same posture as login.
      await persistSessionPair(accountIdB64, session, "access recover");
      flow.require();
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
    if (!flow.current()) {
      accountIdentitySeed?.fill(0);
      throw new StaleBindingError();
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
      if (!flow.current()) {
        accountIdentitySeed.fill(0);
        throw new StaleBindingError();
      }
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
    await parkForRestore(
      {
        accountIdBytes,
        accountIdB64,
        exportKey: new Uint8Array(0),
        deviceKey,
        session,
        username,
      },
      flow,
      restoreKeys,
    );
  }

  /**
   * Park an authenticated account whose AccountIdentity seed is not readable
   * on this device, and show the restore prompt - unless this device itself
   * still holds a phrase that account was never shown.
   *
   * That is the account whose phrase screen is owed (`owePhraseScreen`): the
   * phrase sits in its device-key-wrapped cache, and asking the person to
   * type a phrase they have not seen would lock them out of an identity this
   * browser can restore. So the park is completed from the kept phrase through
   * the ordinary `provideRecoveryPhrase` path, with its fail-closed check
   * against the published identity; the landing then shows the phrase first.
   * Anything short of that (no marker, no device key, an unreadable row, a
   * phrase the directory does not confirm, a network failure) falls back to
   * the prompt.
   *
   * `restoreKeys` is the passkey route's hook (`landAfterAccessRecovery`): it
   * runs after the park and before the prompt, and returns `true` when it
   * landed the session itself.
   */
  async function parkForRestore(
    parked: PendingRecovery,
    flow: FlowBinding,
    restoreKeys?: () => Promise<boolean>,
  ): Promise<void> {
    setPendingRecovery(parked);
    if (await completeParkFromOwedPhrase(parked, flow)) return;
    if (restoreKeys !== undefined) {
      const landed = await restoreKeys();
      if (landed) return;
      // Overtaken while the passkey's keys were being opened: what this flow
      // parked goes (a sign-out or the other landing has usually cleared it).
      if (!flow.current()) {
        if (stillParked(parked)) setPendingRecovery(null);
        throw new StaleBindingError();
      }
    }
    setState({
      kind: "needs-recovery-phrase",
      account: { accountId: parked.accountIdB64, username: parked.username },
    });
  }
  async function completeParkFromOwedPhrase(
    parked: PendingRecovery,
    flow: FlowBinding,
  ): Promise<boolean> {
    const deviceKey = parked.deviceKey;
    const owed =
      isSignupPhraseOwed(guestStorage(), parked.accountIdB64) ||
      phraseOwedInMemory.has(parked.accountIdB64);
    if (deviceKey === null || !owedPhraseCompletesPark({ owed, deviceKeyPresent: true })) {
      return false;
    }
    let phrase: string | null = null;
    try {
      phrase = await readDevicePhraseWith(parked.accountIdB64, deviceKey);
    } catch (err) {
      // eslint-disable-next-line no-console
      console.warn("reading the owed recovery phrase for a parked sign-in failed", err);
    }
    // Overtaken while the cache was read: what this flow parked goes.
    if (!flow.current()) {
      if (stillParked(parked)) setPendingRecovery(null);
      throw new StaleBindingError();
    }
    if (phrase === null) return false;
    try {
      await api.provideRecoveryPhrase(phrase);
      return true;
    } catch (err) {
      if (err instanceof StaleBindingError) throw err;
      // eslint-disable-next-line no-console
      console.warn(
        "completing a parked sign-in from the owed recovery phrase failed; showing the restore prompt",
        err instanceof Error ? err.name : "unknown",
      );
      if (!flow.current()) {
        if (stillParked(parked)) setPendingRecovery(null);
        throw new StaleBindingError();
      }
      // `provideRecoveryPhrase` leaves the parked context in place when it
      // refuses; park again if it did not.
      if (!stillParked(parked)) setPendingRecovery(parked);
      return false;
    }
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
    // ADR-0075 section 11: three words exist only for a phrase-protected
    // account (the flag this device keeps; `protect/account-protection.ts`).
    if (!isPhraseProtected(plainStorage(), cached.accountIdB64)) return null;
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
      // ADR-0074 section 0.4: a discoverable ceremony does not know the
      // account before it finishes, so it asserts no device (the session is
      // bound afterwards); one limited to an account asserts that account's.
      const deviceIdB64 = accountIdB64 === undefined ? undefined : landingDevice({ accountIdB64 });
      return await webauthn.loginWithPasskey(
        accountIdB64 === undefined
          ? { prfSalt: PASSKEY_PRF_SALT }
          : {
              accountIdB64,
              prfSalt: PASSKEY_PRF_SALT,
              ...(deviceIdB64 !== undefined ? { deviceIdB64 } : {}),
            },
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
   * parked (ADR-0072 decision 6). The PRF output opens the blob stored for
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

  /**
   * A recall record on this browser (ADR-0074 section 1.1): the "Was it
   * you?" screen is owed, whatever else the stores say. A row that cannot be
   * opened (no device key, damaged) can take nothing back and is dropped; a
   * record past the window ends here (the rest of the wipe).
   */
  async function restoreOwedRecall(): Promise<boolean> {
    let accounts: string[];
    try {
      accounts = await listRecallAccounts();
    } catch {
      return false;
    }
    for (const accountIdB64 of accounts) {
      let deviceKey: CryptoKey | null = null;
      let record: RecallRecord | null = null;
      try {
        deviceKey = await getDeviceKey(accountIdB64);
        record = deviceKey === null ? null : await getRecallRecord(accountIdB64, deviceKey);
      } catch {
        record = null;
      }
      if (deviceKey === null || record === null) {
        try {
          await deleteRecallRecord(accountIdB64);
        } catch {
          // best-effort
        }
        continue;
      }
      if (state().kind !== "loading") return true;
      recallHeld = { record, deviceKey };
      setState({ kind: "anon" });
      if (recallExpired(record.learnedAtMs, Date.now())) {
        await finishRecallWipe(null);
      } else {
        setSignedOut({
          reason: "recall",
          username: record.username,
          learnedAtMs: record.learnedAtMs,
        });
      }
      return true;
    }
    return false;
  }

  /** The recovery phrase of a guest account, from the guest's own row of the
   *  phrase store, else the account's row (a keep that was interrupted after
   *  the phrase moved). `null` when this device cannot read it. */
  async function readGuestPhrase(accountIdB64: string): Promise<string | null> {
    let deviceKey: CryptoKey | null = null;
    try {
      deviceKey = await getDeviceKey(accountIdB64);
    } catch {
      deviceKey = null;
    }
    if (deviceKey === null) return null;
    return (
      (await readDevicePhraseWith(guestPhraseKey(accountIdB64), deviceKey)) ??
      (await readDevicePhraseWith(accountIdB64, deviceKey))
    );
  }

  /**
   * A guest account has its username and its first password: it is an
   * account from here on. The phrase moves under the account's own key (a
   * device that holds the keys holds the phrase), the "Protect your account"
   * step is owed exactly as after a sign-up (the phrase is NOT shown), the
   * guest marker goes, the first backup is uploaded and push is asked for.
   * The protect marker is written before the guest marker is cleared, so the
   * guards go from the guest screen straight to that step.
   */
  async function keepGuestAccount(args: {
    accountIdBytes: Uint8Array;
    accountIdB64: string;
    accountIdentitySeed: Uint8Array;
    sessionToken: string;
    phrase: string;
  }): Promise<void> {
    const { accountIdB64 } = args;
    let deviceKey: CryptoKey | null = null;
    try {
      deviceKey = await getDeviceKey(accountIdB64);
    } catch {
      deviceKey = null;
    }
    let moved = false;
    if (deviceKey !== null) {
      moved = await storeDevicePhrase(accountIdB64, args.phrase, deviceKey, "guest-kept");
    }
    if (moved) {
      try {
        await deletePhrase(guestPhraseKey(accountIdB64));
      } catch (err) {
        // The device wipe removes it.
        // eslint-disable-next-line no-console
        console.warn("removing the guest recovery phrase row failed", err);
      }
    }
    markProtectStepOwed(plainStorage(), accountIdB64);
    clearGuestAccount(guestStorage(), accountIdB64);
    setGuestMarkerVersion((v) => v + 1);
    setProtectVersion((v) => v + 1);
    if (deviceKey !== null) {
      void uploadBackupBaseline({
        accountIdBytes: args.accountIdBytes,
        accountIdB64,
        accountIdentitySeed: args.accountIdentitySeed,
        deviceKey,
        sessionToken: args.sessionToken,
        phrase: args.phrase,
        context: "guest kept",
      });
    }
    void subscribePushFor(accountIdB64).catch((err) => {
      // eslint-disable-next-line no-console
      console.warn("push subscribe (guest kept) failed", err);
    });
  }

  /**
   * Delete a guest account on the servers, in the order and with the
   * strictness `guestDeletePlan` gives. `strict: false` (an expired guest)
   * swallows every failure: both deletes are idempotent and the servers
   * sweep an unkept account themselves.
   *
   * Auth deletes an account that has no login handle and no passkey on its
   * session alone, and releases the handle of an account younger than the
   * unclaimed lifetime the same way. So a guest whose half-finished claim
   * bound a handle releases it FIRST, then deletes. No fresh-auth grant is
   * held for a guest. An unexpected `403 fresh-auth-required` does not stop
   * the delete on this device: auth removes an account nobody named on its
   * own clock.
   */
  async function deleteGuestOnServers(
    guest: GuestAccountState,
    sessionToken: string,
    strict: boolean,
  ): Promise<void> {
    for (const step of guestDeletePlan(guest)) {
      try {
        if (step.service === "auth") {
          if (guest.claimedUsername !== null) {
            await client.releaseLoginHandle(sessionToken).catch((releaseErr: unknown) => {
              // The delete below then answers for it.
              // eslint-disable-next-line no-console
              console.warn("guest account: login handle not released", releaseErr);
            });
          }
          const webauthn = new WebauthnClient();
          webauthn.setSessionToken(sessionToken);
          try {
            await webauthn.deleteAccount(null);
          } catch (err) {
            const refused =
              err instanceof WebauthnHttpError &&
              isFreshAuthRequired({ status: err.status, errorCode: errorCodeOf(err.body) });
            if (!refused) throw err;
            // eslint-disable-next-line no-console
            console.warn("guest account: auth asked for a fresh proof; left to the server's sweep");
          }
        } else {
          discovery.setSessionToken(sessionToken);
          await discovery.deleteAccount();
        }
      } catch (err) {
        if (strict && step.required) throw err;
        // eslint-disable-next-line no-console
        console.warn(`guest account: ${step.service} rows not deleted`, err);
      }
    }
  }

  async function restoreSessionOnLoad(): Promise<void> {
    if (await restoreOwedRecall()) return;
    const probe = await probeStoredSecrets();
    // ADR-0074 section 2.3: a logout that could not reach the server left a
    // marker and the stored pair. That pair is not a session to restore; it
    // is what the ONE retry signs out with, and it goes whatever the answer.
    const logoutOwed =
      probe.cachedAccount !== null && isLogoutOwed(plainStorage(), probe.cachedAccount.accountIdB64);
    if (logoutOwed && probe.cachedAccount !== null) {
      const account = probe.cachedAccount.accountIdB64;
      const pair = probe.sessionPair;
      clearLogoutOwed(plainStorage(), account);
      void serverLogout(pair).finally(() => {
        // Unless a sign-in installed a new session meanwhile.
        if (sessionTokens.accountIdB64() === account) return;
        void deleteSessionToken(account).catch(() => {
          /* best-effort */
        });
      });
    }
    const decision = decideSilentRestore({
      cachedAccount: probe.cachedAccount,
      deviceKeyPresent: probe.deviceKey !== null,
      sessionPair: probe.sessionPair,
      identitySeedPresent: probe.identitySeed !== null,
      logoutOwed,
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
    // A guest account (`guest-account.ts`) is never left as a cached,
    // signed-out account, and never outlives its day: nobody knows its
    // password, so a guest that cannot be restored, or is too old, leaves
    // the device here, before any state lands.
    if (probe.cachedAccount !== null) {
      const guestId = probe.cachedAccount.accountIdB64;
      const guest = guestAccountState(guestStorage(), guestId);
      const action = guestRestoreAction({
        guest,
        restorable:
          decision.kind === "restore" && probe.deviceKey !== null && probe.identitySeed !== null,
        nowMs: Date.now(),
      });
      if (guest !== null && action !== "none") {
        probe.identitySeed?.fill(0);
        if (action === "discard" && decision.kind === "restore") {
          await deleteGuestOnServers(guest, decision.sessionPair.sessionToken, false);
          discovery.setSessionToken(null);
        }
        await wipeAccountLocalData(guestId);
        anonTokens.forgetAccount(guestId);
        setGuestMarkerVersion((v) => v + 1);
        if (state().kind === "loading") setState({ kind: "anon" });
        return;
      }
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
    // ADR-0073 section 3.3: a reload refreshes with the credential instead
    // of reusing the persisted access token. `superseded` and `invalid` are
    // handled by the provider's listener (the signed-out screen and wipe /
    // the sign-in screen); a network failure keeps the persisted pair, and
    // the app opens on local data with whatever it still allows.
    explicitSignInThisPage = false;
    sessionTokens.install(decision.accountIdB64, decision.sessionPair);
    const refresh = await sessionTokens.refreshOnce();
    if (refresh === "superseded" || refresh === "device-revoked" || refresh === "invalid") {
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
      // A sign-out won the race: it already cleared everything, and what is
      // installed now (if anything) is not this restore's to undo.
      if (err instanceof StaleBindingError) {
        seed.fill(0);
        return;
      }
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

  /**
   * What a device does once the server answered a key reset (ADR-0073
   * section 2.3 from step 3 on), shared by "Reset account keys"
   * (`mintFreshIdentity`) and by taking the account back (ADR-0074 part 1:
   * from here a recall IS a reset). The new pair, the seed and the phrase
   * are stored and the "publish owed" record written before any further
   * network call; then the phrase screen's state, then the attested
   * publishing steps. Never a second code path for the recall.
   */
  async function adoptKeyResetOutcome(ctx: {
    readonly accountIdB64: string;
    readonly accountIdBytes: Uint8Array;
    readonly username: string;
    /** The account's device key when this device has one; `null` mints one
     *  from the new seed. */
    readonly deviceKey: CryptoKey | null;
    /** The 64-byte seed of the NEW phrase. Owned by the parked phrase from
     *  here on. */
    readonly seed: Uint8Array;
    readonly phrase: string;
    readonly newPubkey: Uint8Array;
    readonly reset: KeyResetResult;
    /** Runs once everything local is stored, before the state moves. */
    readonly afterPersist?: () => void | Promise<void>;
  }): Promise<void> {
    // 3. Persist immediately, before any further network call: the new
    // pair (the old one is superseded by the same transaction), the seed
    // and the phrase under the device key, and the "publish owed" record
    // with the attestation. From here on a crash or a failure is a retry
    // of the publishing steps, never a second ctx.reset.
    //
    // Another account may be signed in (or sit in its own prompt) by now:
    // the reset happened on the server, so everything of THIS account is
    // still stored below, but the other account's session, chats and screen
    // are left alone. The new keys then wait for this account's next
    // landing, which retries the publish (`settleOwedKeyResetPublish`) and
    // shows the words first (`restoreOwedSignupPhrase`).
    const overtaken = (): boolean =>
      freshPhraseDisposition({ mintedFor: ctx.accountIdB64, boundAccount: boundAccountId() }) ===
      "owe";
    openStoresFor(ctx.accountIdB64);
    if (!overtaken()) installFlowSession(ctx.accountIdB64, ctx.reset.session);
    let deviceKey = ctx.deviceKey;
    if (deviceKey === null) {
      // The ACCESS-only fresh-browser case (no password, hence no OPAQUE
      // export key): bootstrap a device key from the new seed, as
      // `provideRecoveryPhrase` / `completeFromDeviceRestore` do. When a
      // device key IS parked, reuse it: minting a new one would orphan
      // its wrapped stores.
      deviceKey = await deriveDeviceRootKeyFromIdentitySeed(
        ctx.seed,
        ctx.accountIdB64,
      );
      try {
        await putDeviceKey(ctx.accountIdB64, deviceKey);
      } catch (err) {
        // eslint-disable-next-line no-console
        console.warn("device-key persist (reset) failed", err);
      }
    }
    try {
      await putAccountIdentitySeed(ctx.accountIdB64, ctx.seed, deviceKey);
    } catch (err) {
      // eslint-disable-next-line no-console
      console.warn("account-identity-seed persist (reset) failed", err);
    }
    await persistSessionPair(ctx.accountIdB64, ctx.reset.session, "reset");
    // The device holds the new keys, so it holds the new phrase (the local
    // three-word check, the hand-over to a new device, "Show recovery
    // phrase"). The signup-phrase marker makes a reload show the words
    // first, once nothing is owed.
    await storeDevicePhrase(ctx.accountIdB64, ctx.phrase, deviceKey, "fresh-identity");
    // A reset (and a recall, which is one) ends with new keys under a new
    // phrase: the account backup is uploaded again under it by the automatic
    // backup once the session lands (no MLS state, as always).
    markBackupDirty(ctx.accountIdB64);
    owePhraseScreen(ctx.accountIdB64);
    const record: KeyResetOwed = {
      v: 1,
      attestationB64Url: ctx.reset.attestationB64Url,
      keyGeneration: ctx.reset.session.keyGeneration.toString(),
      newSignaturePubkeyB64: base64Encode(ctx.newPubkey),
      signedAtMs: keyResetAttestationSignedAtMs(ctx.reset.attestation),
      done: { directory: false, deviceGroup: false },
    };
    writeKeyResetOwed(plainStorage(), ctx.accountIdB64, record);
    // ADR-0073 R.4 - part of the reset's client order: from now until the
    // expiry this account's reach sync keeps the continuity key registered
    // on the NEW mailbox (`reach/reach-service.ts`), so a former contact
    // whose device accepted the reset can deliver a Welcome. The marker
    // outlives the owed record above; the registration itself rides the
    // reach sync, which is retried until it lands.
    writeContinuityMarker(plainStorage(), ctx.accountIdB64, {
      keyGeneration: Number(ctx.reset.session.keyGeneration),
      signedAtMs: record.signedAtMs,
    });
    // ADR-0074 section 3.5: a reset makes every predecessor, proven or not,
    // a superseded key, and ends a rotation this device was finishing.
    clearRotation(plainStorage(), ctx.accountIdB64);
    clearRotatedNotice(plainStorage(), ctx.accountIdB64);
    if (!overtaken()) chats.setPredecessorAccountIdentity(null);
    try {
      await deleteKeyRotationState(ctx.accountIdB64);
    } catch (err) {
      // eslint-disable-next-line no-console
      console.warn("key-rotation state not cleared after a reset", err);
    }
    // The old DeviceGroup, its registrar state and the chats' routing
    // belong to the old generation: the re-founding below replaces them.
    try {
      await deleteOutboxForAccount(ctx.accountIdB64);
      clearObservationsForAccount(ctx.accountIdB64);
      clearUnappliedForAccount(ctx.accountIdB64);
      clearOwnRequestsForAccount(ctx.accountIdB64);
      clearBeaconObservationsForAccount(ctx.accountIdB64);
      clearLastObservedOwnerForAccount(ctx.accountIdB64);
      clearLeaveRequestedForAccount(ctx.accountIdB64);
    } catch (err) {
      // eslint-disable-next-line no-console
      console.warn("clearing the old generation's chat bookkeeping failed", err);
    }
    if (overtaken()) {
      ctx.seed.fill(0);
      await ctx.afterPersist?.();
      throw new StaleBindingError();
    }
    setKeyResetNote(true);

    // Stash the completion context and flip to `save-fresh-phrase`: the
    // flow is committed past the restore prompt. The words are withheld
    // while the publish is owed; `confirmFreshPhraseSaved` lands the
    // session once the person acknowledged them.
    setPendingFreshPhrase({
      seed: ctx.seed,
      accountIdBytes: ctx.accountIdBytes,
      accountIdB64: ctx.accountIdB64,
      session: ctx.reset.session,
      username: ctx.username,
      phrase: ctx.phrase,
      deviceKey,
    });
    setPendingRecovery(null);
    // The new keys are stored: a recall drops its record and its screen
    // here, before the phrase screen is put up.
    await ctx.afterPersist?.();
    setState({
      kind: "save-fresh-phrase",
      account: {
        accountId: ctx.accountIdB64,
        username: ctx.username,
      },
    });

    // 4-5. Publish: the directory (with the attestation), then the
    // DeviceGroup journal (the attested reset). A failure leaves the
    // record for "Try again" and the next session start; the screen says
    // what happened.
    try {
      await retryKeyResetPublish(
        ctx.accountIdB64,
        ctx.accountIdBytes,
        ctx.seed,
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
  }

  // -------------------------------------------------------------------
  // ADR-0074 part 3 (Stage A): the account keys changed WITH the devices
  // kept. `devices/key-rotation.ts` decides; this is the auth layer's half.
  // -------------------------------------------------------------------

  /** A kept device is fetching the new keys (UX 18.3: `Updating keys...`). */
  const [keysUpdating, setKeysUpdating] = createSignal(false);
  /** Bumped when the rotation record or the "keys were changed" line moves. */
  const [rotationVersion, setRotationVersion] = createSignal(0);
  let adoptionRetryTimer: ReturnType<typeof setTimeout> | null = null;
  onCleanup(() => {
    if (adoptionRetryTimer !== null) clearTimeout(adoptionRetryTimer);
  });

  /** The phrase and the 64-byte seed of a 16-byte entropy. */
  function phraseOfEntropy(wasm: Wasm, entropy: Uint8Array): { phrase: string; seed: Uint8Array } {
    const mnemonic = wasm.RecoveryMnemonic.fromEntropy(entropy);
    try {
      return { phrase: mnemonic.phrase, seed: mnemonic.toSeed() };
    } finally {
      mnemonic.free();
    }
  }

  function publicKeyOfSeed(wasm: Wasm, seed: Uint8Array): Uint8Array {
    const identity = wasm.Identity.fromSeed(seed);
    try {
      return identity.signaturePublicKey;
    } finally {
      identity.free();
    }
  }

  function ed25519SeedOf(wasm: Wasm, seed: Uint8Array): Uint8Array {
    const identity = wasm.Identity.fromSeed(seed);
    try {
      return identity.ed25519Seed;
    } finally {
      identity.free();
    }
  }

  /**
   * Adopt account keys that were changed by a PROVEN rotation (on this
   * device or on another one): the new seed and phrase replace the old ones,
   * ALL local data is kept, and the old seed is kept as a predecessor (the
   * conversation leaves stay bound to it until Stage B).
   *
   * The predecessor is written BEFORE the account seed is overwritten: a
   * crash in between leaves a device that still holds the old key and can
   * adopt again, never one that lost the key its leaves are bound to.
   */
  async function adoptRotatedKeys(ctx: {
    readonly accountIdB64: string;
    readonly deviceKey: CryptoKey;
    /** The 64-byte seed held before the rotation. */
    readonly oldSeed: Uint8Array;
    readonly newEntropy: Uint8Array;
    readonly newPubkeyB64: string;
  }): Promise<{ readonly phrase: string; readonly seed: Uint8Array; readonly conversationSeed: Uint8Array }> {
    const wasm = await loadWasm();
    const { phrase, seed } = phraseOfEntropy(wasm, ctx.newEntropy);
    if (base64Encode(publicKeyOfSeed(wasm, seed)) !== ctx.newPubkeyB64) {
      seed.fill(0);
      throw new Error("key rotation: the entropy does not derive the expected account key");
    }
    const next = await updateKeyRotationState(ctx.accountIdB64, ctx.deviceKey, (state) => ({
      ...withPredecessorSeed(state, ctx.oldSeed),
      pending: state.pending.filter((p) => p.newSignaturePubkeyB64 !== ctx.newPubkeyB64),
    }));
    await putAccountIdentitySeed(ctx.accountIdB64, seed, ctx.deviceKey);
    await storeDevicePhrase(ctx.accountIdB64, phrase, ctx.deviceKey, "key-rotation");
    // The account backup on the server is sealed under the phrase just left:
    // the automatic backup uploads it again under the new one (no MLS state,
    // as always) once the session runs on the new keys.
    markBackupDirty(ctx.accountIdB64);
    // ADR-0074 section 3.5a rule 4: the mailbox of the key just left is read
    // for 30 days from now, then left. Its own window: an earlier
    // predecessor's is not started again.
    stampPredecessorAdoption(
      ctx.accountIdB64,
      journalHex(publicKeyOfSeed(wasm, ctx.oldSeed)),
      Date.now(),
    );
    return { phrase, seed, conversationSeed: conversationSeed(next) ?? ctx.oldSeed };
  }

  /**
   * ADR-0074 (a device paired after a key rotation) — a pairing state bundle
   * just stored the account's earlier keys on a device whose session is
   * already running (it was signed in, then paired by showing its code).
   * While it holds no conversation its binding is minted again under the
   * conversation key and its KeyPackage published again, in this session,
   * so no conversation admits it under the current key. A device that
   * already holds conversations keeps its binding. Resolves whether the
   * binding changed; never rejects.
   */
  async function rebindAfterStateBundle(accountIdB64: string): Promise<boolean> {
    try {
      const account = keyPackageAccount;
      const canonical = chats.accountIdentity();
      if (account === null || account.accountIdB64 !== accountIdB64 || canonical === null) {
        return false;
      }
      const wasm = await loadWasm();
      const conversationSeedBytes = await installSessionIdentities(wasm, canonical, accountIdB64, true);
      if (conversationSeedBytes === null) return false;
      const secrets = identitySecrets();
      if (secrets !== null && base64Encode(secrets.accountId) === accountIdB64) {
        setIdentitySecrets({ ...secrets, conversationIdentitySeed: conversationSeedBytes });
      }
      // eslint-disable-next-line no-console
      console.info("the device binding was minted again under the account's earlier key");
      await publishKeyPackageBestEffort("state bundle");
      return true;
    } catch (err) {
      // eslint-disable-next-line no-console
      console.warn("re-binding after the state bundle failed", err instanceof Error ? err.name : "unknown");
      return false;
    }
  }

  /** Put adopted keys into the live session without a reload (the rotating
   *  device: its sheet goes on). The device identity and every group are
   *  untouched. */
  async function swapSessionAccountKeys(
    accountIdB64: string,
    seed: Uint8Array,
    conversationSeedBytes: Uint8Array,
  ): Promise<void> {
    const wasm = await loadWasm();
    // The identity that was the account's moves into the predecessor slot as
    // the same object (its provider holds the account's PublisherGroups).
    chats.rotateAccountIdentity(wasm.Identity.fromSeed(seed));
    const secrets = identitySecrets();
    const pair = sessionTokens.pair();
    if (secrets !== null && base64Encode(secrets.accountId) === accountIdB64) {
      setIdentitySecrets({
        ...secrets,
        accountIdentitySeed: seed,
        conversationIdentitySeed: conversationSeedBytes,
        ...(pair !== null
          ? {
              sessionToken: pair.sessionToken,
              sessionExpiresAtMs: pair.sessionExpiresAtMs,
              keyGeneration: pair.keyGeneration,
            }
          : {}),
      });
    }
  }

  /** The session upgrade with the statement signed by `seed`'s key. */
  async function upgradeSessionWith(
    accountIdBytes: Uint8Array,
    refreshToken: string,
    seed: Uint8Array,
  ): Promise<{ readonly verdict: UpgradeVerdict; readonly pair: SessionPair | null }> {
    const wasm = await loadWasm();
    const signingSeed = ed25519SeedOf(wasm, seed);
    try {
      const pair = await client.sessionRefreshUpgrade(refreshToken, (signAtMs) =>
        signAtMs === null
          ? wasm.signSessionUpgradeStatement(signingSeed, accountIdBytes, refreshToken)
          : wasm.signAccountStatementAt(
              signingSeed,
              ACCOUNT_STATEMENT_KIND_SESSION_UPGRADE,
              accountIdBytes,
              wasm.sessionUpgradeStatementBody(refreshToken),
              BigInt(signAtMs),
            ),
      );
      return { verdict: "accepted", pair };
    } catch (err) {
      if (err instanceof AuthHttpError) {
        return { verdict: upgradeVerdict(err.status, err.errorCode), pair: null };
      }
      return { verdict: "later", pair: null };
    } finally {
      signingSeed.fill(0);
    }
  }

  /** The seed a pending envelope carries. The envelope was verified when it
   *  was stored; this reads it again by shape only (its proof may since have
   *  expired, which the server's own check of the key supersedes). */
  async function seedOfPending(pending: PendingRotation): Promise<{ entropy: Uint8Array; seed: Uint8Array } | null> {
    const wasm = await loadWasm();
    try {
      const decoded = wasm.decodeKeyRotationEnvelope(base64Decode(pending.envelopeB64));
      try {
        const entropy = decoded.newEntropy;
        const { seed } = phraseOfEntropy(wasm, entropy);
        if (base64Encode(publicKeyOfSeed(wasm, seed)) !== pending.newSignaturePubkeyB64) return null;
        return { entropy, seed };
      } finally {
        decoded.free();
      }
    } catch {
      return null;
    }
  }

  type AdoptionOutcome = "adopted" | "reset" | "later" | "device-revoked" | "sign-in";

  let adoptionInFlight: Promise<AdoptionOutcome> | null = null;
  function adoptRotationOnce(accountIdB64: string): Promise<AdoptionOutcome> {
    if (adoptionInFlight !== null) return adoptionInFlight;
    const run = adoptRotation(accountIdB64)
      .catch((err: unknown): AdoptionOutcome => {
        // A failure of this client's own plumbing is never read as a reset.
        // eslint-disable-next-line no-console
        console.warn("key rotation: adoption failed", err instanceof Error ? err.name : "unknown");
        return "later";
      })
      .finally(() => {
        adoptionInFlight = null;
      });
    adoptionInFlight = run;
    return run;
  }

  /**
   * The order on a superseded device (ADR-0074 section 3.3,
   * `supersededStep`): exchange the refresh credential with a statement by
   * the held key, then by each pending envelope's key; the key the server
   * accepts is the account's. With nothing accepted, read the DeviceGroup
   * journal to its head ONCE and try what it brought. Only then is it a
   * reset.
   */
  async function adoptRotation(accountIdB64: string): Promise<AdoptionOutcome> {
    let deviceKey: CryptoKey | null = null;
    try {
      deviceKey = await getDeviceKey(accountIdB64);
    } catch {
      deviceKey = null;
    }
    // A device that never held the keys has nothing to adopt with.
    if (deviceKey === null) return "reset";
    let heldSeed: Uint8Array | null = null;
    try {
      heldSeed = await getAccountIdentitySeed(accountIdB64, deviceKey);
    } catch {
      heldSeed = null;
    }
    if (heldSeed === null) return "reset";
    let pair = sessionTokens.pair() ?? sessionTokens.supersededPair(accountIdB64);
    if (pair === null) {
      try {
        pair = await getSessionPair(accountIdB64, deviceKey);
      } catch {
        pair = null;
      }
    }
    // No refresh credential at all: nothing can be exchanged.
    if (pair === null) return "reset";
    // UX 18.3: told its session is out of date, not yet holding the new keys.
    setKeysUpdating(true);
    const wasm = await loadWasm();
    const accountIdBytes = base64Decode(accountIdB64);
    const heldKeyB64 = base64Encode(publicKeyOfSeed(wasm, heldSeed));
    const tried: string[] = [];
    let journal: "not-read" | "read" | "unreadable" = "not-read";
    for (;;) {
      let state: KeyRotationState;
      try {
        state = await getKeyRotationState(accountIdB64, deviceKey);
      } catch {
        state = EMPTY_KEY_ROTATION_STATE;
      }
      const pending = withPendingRotation(state.pending, null, Date.now(), (preparedAtMs, nowMs) =>
        wasm.keyRotationPendingExpired(BigInt(Math.max(0, preparedAtMs)), BigInt(nowMs)),
      );
      const step = supersededStep({ heldKeyB64, pending, tried, journal });
      if (step.kind === "reset") return "reset";
      if (step.kind === "read-journal") {
        const port = await awaitKeyRotationPort(KEY_ROTATION_PORT_WAIT_MS);
        if (port === null) {
          // No DeviceGroup handle in this page. Inside a session that is a
          // loading order, not a verdict; outside one nothing will load it.
          return state_isSessionFor(accountIdB64) ? "later" : "reset";
        }
        const read = await port.catchUp();
        if (read === "offline") return "later";
        journal = read;
        continue;
      }
      for (const keyB64 of step.keysB64) {
        let seed: Uint8Array;
        let entropy: Uint8Array | null = null;
        if (keyB64 === heldKeyB64) {
          seed = heldSeed;
        } else {
          const found = pendingFor(pending, keyB64);
          const carried = found === null ? null : await seedOfPending(found);
          if (carried === null) {
            tried.push(keyB64);
            continue;
          }
          seed = carried.seed;
          entropy = carried.entropy;
        }
        const upgraded = await upgradeSessionWith(accountIdBytes, pair.refreshToken, seed);
        if (upgraded.verdict === "not-accepted") {
          tried.push(keyB64);
          continue;
        }
        if (upgraded.verdict === "device-revoked") return "device-revoked";
        if (upgraded.verdict === "sign-in") return "sign-in";
        if (upgraded.verdict !== "accepted" || upgraded.pair === null) return "later";
        // This key is the account's.
        if (entropy !== null) {
          await adoptRotatedKeys({
            accountIdB64,
            deviceKey,
            oldSeed: heldSeed,
            newEntropy: entropy,
            newPubkeyB64: keyB64,
          });
          markRotatedNotice(plainStorage(), accountIdB64);
          // ADR-0074 section 3.5a rule 5: this KEPT device's own links and
          // unanswered requests were granted under the key just left. The
          // marker keeps their revoke and re-send owed across reloads.
          markSpendOwed(accountIdB64, Date.now(), plainStorage());
        }
        await persistSessionPair(accountIdB64, upgraded.pair, "key-rotation");
        installSession(accountIdB64, upgraded.pair);
        setKeysUpdating(false);
        setRotationVersion((v) => v + 1);
        const live = identitySecrets();
        if (entropy !== null || (live !== null && !bytesEqual(live.accountIdentitySeed, seed))) {
          // The page's identities are of the old key: land again with the
          // new one. Every chat and contact is still on this device.
          // eslint-disable-next-line no-console
          console.warn("the account keys were changed on another device; this device has the new ones");
          if (typeof window !== "undefined") window.location.reload();
        }
        return "adopted";
      }
    }
  }

  function state_isSessionFor(accountIdB64: string): boolean {
    const s = state();
    return s.kind === "session" && s.account.accountId === accountIdB64;
  }

  /**
   * A `KeyRotation` envelope from the account's own DeviceGroup (the only
   * place it means anything; the caller is the DeviceGroup receive loop).
   * Verified against the key this device holds, or against a key a pending
   * envelope already names (a device that was away for two rotations), and
   * stored as PENDING. Nothing else changes. Returns whether the bytes were
   * a key-rotation envelope at all (so the caller files nothing else).
   */
  async function intakeKeyRotation(envelope: Uint8Array): Promise<boolean> {
    const wasm = await loadWasm();
    let isRotation = false;
    try {
      const dispatched = wasm.dispatchIncomingEnvelope(envelope);
      try {
        isRotation = dispatched.kind === wasm.IncomingEnvelopeKind.KeyRotation;
      } finally {
        dispatched.free();
      }
    } catch {
      return false;
    }
    if (!isRotation) return false;
    const s = state();
    const secrets = identitySecrets();
    if (s.kind !== "session" || secrets === null) return true;
    const accountIdB64 = s.account.accountId;
    try {
      const deviceKey = await getDeviceKey(accountIdB64);
      if (deviceKey === null) return true;
      const heldKey = publicKeyOfSeed(wasm, secrets.accountIdentitySeed);
      const nowMs = Date.now();
      const stateNow = await getKeyRotationState(accountIdB64, deviceKey);
      const candidates = [heldKey, ...stateNow.pending.map((p) => base64Decode(p.newSignaturePubkeyB64))];
      let verified: { newKeyB64: string; preparedAtMs: number } | null = null;
      for (const key of candidates) {
        try {
          const v = wasm.verifyKeyRotationEnvelope(envelope, key, BigInt(Math.floor(nowMs / 1000)));
          try {
            verified = {
              newKeyB64: base64Encode(v.newSignaturePubkey),
              preparedAtMs: Number(v.preparedAtMs),
            };
          } finally {
            v.free();
          }
          break;
        } catch {
          // Not by this key; the next candidate.
        }
      }
      const intake = rotationIntake({
        fromOwnDeviceGroup: true,
        verified: verified !== null,
        newKeyIsHeldKey: verified !== null && verified.newKeyB64 === base64Encode(heldKey),
      });
      if (intake === "drop" || verified === null) return true;
      const row: PendingRotation = {
        newSignaturePubkeyB64: verified.newKeyB64,
        envelopeB64: base64Encode(envelope),
        preparedAtMs: verified.preparedAtMs,
      };
      await updateKeyRotationState(accountIdB64, deviceKey, (st) => ({
        ...st,
        pending: withPendingRotation(st.pending, row, nowMs, (preparedAtMs, now) =>
          wasm.keyRotationPendingExpired(BigInt(Math.max(0, preparedAtMs)), BigInt(now)),
        ),
      }));
      // The commit may already have landed: ask the directory once. If it
      // has not, this device learns it at its next refresh.
      void adoptIfDirectoryShowsPending(accountIdB64);
    } catch (err) {
      // eslint-disable-next-line no-console
      console.warn("key rotation: the envelope could not be stored", err instanceof Error ? err.name : "unknown");
    }
    return true;
  }

  /** Adopt when the published identity bundle carries a pending key. */
  async function adoptIfDirectoryShowsPending(accountIdB64: string): Promise<void> {
    try {
      const deviceKey = await getDeviceKey(accountIdB64);
      if (deviceKey === null) return;
      const stateNow = await getKeyRotationState(accountIdB64, deviceKey);
      if (stateNow.pending.length === 0) return;
      const published = await discovery.identityFetch(base64Decode(accountIdB64));
      if (published === null) return;
      if (pendingFor(stateNow.pending, published.signature_pubkey) === null) return;
      setKeysUpdating(true);
      // The directory shows the key; the session exchange confirms it.
      void signOutAfterKeyReset(accountIdB64);
    } catch (err) {
      // eslint-disable-next-line no-console
      console.warn("key rotation: directory check failed", err instanceof Error ? err.name : "unknown");
    }
  }

  // ----- The rotating device (ADR-0074 section 3.2) ---------------------

  function rotationSession(): {
    accountIdB64: string;
    accountIdBytes: Uint8Array;
    secrets: IdentitySecrets;
  } {
    const s = state();
    const secrets = identitySecrets();
    if (s.kind !== "session" || secrets === null) throw new KeyedError("common.error.session_not_ready");
    return { accountIdB64: s.account.accountId, accountIdBytes: secrets.accountId, secrets };
  }

  async function rotationDeviceKey(accountIdB64: string): Promise<CryptoKey> {
    const deviceKey = await getDeviceKey(accountIdB64);
    if (deviceKey === null) throw new KeyedError("common.error.session_not_ready");
    return deviceKey;
  }

  const keyRotationApi: AuthApi["keyRotation"] = {
    updating: keysUpdating,
    noticeOwed() {
      rotationVersion();
      const s = state();
      return s.kind === "session" && rotatedNoticeOwed(plainStorage(), s.account.accountId);
    },
    dismissNotice() {
      const s = state();
      if (s.kind === "session") clearRotatedNotice(plainStorage(), s.account.accountId);
      setRotationVersion((v) => v + 1);
    },
    recordChanged() {
      setRotationVersion((v) => v + 1);
    },
    intake: intakeKeyRotation,
    async prepare(deviceIdB64) {
      // Step 3: the new phrase, and the proof signed by the OLD key. Kept
      // sealed until the commit has its answer, so a retry re-sends the SAME
      // key (a second envelope for another key would leave the kept devices
      // holding one that never becomes the account's).
      const { accountIdB64, secrets } = rotationSession();
      const deviceKey = await rotationDeviceKey(accountIdB64);
      const wasm = await loadWasm();
      const nowMs = Date.now();
      const nowSecs = Math.floor(nowMs / 1000);
      const held = (await getKeyRotationState(accountIdB64, deviceKey)).prepared;
      let prepared: PreparedRotation;
      if (
        held !== null &&
        held.deviceIdB64 === deviceIdB64 &&
        held.proofExpiresAtSecs - nowSecs > SUPERSESSION_WINDOW_SECS / 2
      ) {
        prepared = held;
      } else {
        const mnemonic = wasm.RecoveryMnemonic.generate();
        let entropy: Uint8Array;
        let seed: Uint8Array;
        try {
          entropy = mnemonic.toEntropy();
          seed = mnemonic.toSeed();
        } finally {
          mnemonic.free();
        }
        const oldSigningSeed = ed25519SeedOf(wasm, secrets.accountIdentitySeed);
        const newSigningSeed = ed25519SeedOf(wasm, seed);
        try {
          const expiresAt = nowSecs + SUPERSESSION_WINDOW_SECS;
          const proof = wasm.signSupersessionProof(
            oldSigningSeed,
            newSigningSeed,
            BigInt(nowSecs),
            BigInt(expiresAt),
          );
          prepared = {
            deviceIdB64,
            entropyB64: base64Encode(entropy),
            proofB64: base64Encode(proof),
            newSignaturePubkeyB64: base64Encode(publicKeyOfSeed(wasm, seed)),
            preparedAtMs: nowMs,
            proofExpiresAtSecs: expiresAt,
          };
        } finally {
          oldSigningSeed.fill(0);
          newSigningSeed.fill(0);
          seed.fill(0);
          entropy.fill(0);
        }
        const toStore = prepared;
        await updateKeyRotationState(accountIdB64, deviceKey, (st) => ({ ...st, prepared: toStore }));
      }
      return {
        envelope: wasm.composeKeyRotationEnvelope(
          BigInt(prepared.preparedAtMs),
          base64Decode(prepared.entropyB64),
          base64Decode(prepared.proofB64),
        ),
        newSignaturePubkeyB64: prepared.newSignaturePubkeyB64,
        preparedAtMs: prepared.preparedAtMs,
      };
    },
    async commit(grant, revokeDeviceIds, options) {
      const { accountIdB64, secrets } = rotationSession();
      const deviceKey = await rotationDeviceKey(accountIdB64);
      const prepared = (await getKeyRotationState(accountIdB64, deviceKey)).prepared;
      if (prepared === null) throw new Error("key rotation: nothing was prepared");
      const wasm = await loadWasm();
      const entropy = base64Decode(prepared.entropyB64);
      const { seed } = phraseOfEntropy(wasm, entropy);
      const token = sessionTokens.peek();
      if (token === null) throw new KeyedError("common.error.session_not_ready");
      let reset: KeyResetResult;
      try {
        reset = await client.rotateAccountKeys({
          sessionToken: token,
          freshGrant: grant,
          newSeed: seed.slice(),
          newSignaturePubkey: base64Decode(prepared.newSignaturePubkeyB64),
          proof: base64Decode(prepared.proofB64),
          revokeDeviceIds,
          phraseSaved: options?.phraseSaved === true,
        });
      } catch (err) {
        seed.fill(0);
        if (
          err instanceof AuthHttpError &&
          isFreshAuthRequired({ status: err.status, errorCode: err.errorCode })
        ) {
          freshGrants.spend(grant);
        }
        throw err;
      }
      seed.fill(0);
      // The grant was of the previous generation and is spent; the answer
      // carries none (T5 item 12).
      freshGrants.clear();
      // Persist at once, before any further network call. The KEYS first,
      // then the pair: a crash in between leaves a device that holds the new
      // key with a superseded session, which the session exchange repairs
      // (its statement is by the key it holds), and `probeCommit` reads "the
      // held key is the prepared key" as landed. The other order would leave
      // a current session over the old key, which nothing could tell apart
      // from a commit that never happened.
      const oldSeed = secrets.accountIdentitySeed.slice();
      const adopted = await adoptRotatedKeys({
        accountIdB64,
        deviceKey,
        oldSeed,
        newEntropy: entropy,
        newPubkeyB64: prepared.newSignaturePubkeyB64,
      });
      installSession(accountIdB64, reset.session);
      await persistSessionPair(accountIdB64, reset.session, "key-rotation");
      // The new words are shown once nothing is owed.
      markSignupPhraseOwed(plainStorage(), accountIdB64);
      await swapSessionAccountKeys(accountIdB64, adopted.seed, adopted.conversationSeed);
      return {
        keyGeneration: reset.session.keyGeneration,
        attestationB64Url: reset.attestationB64Url,
        signedAtMs: keyResetAttestationSignedAtMs(reset.attestation),
      };
    },
    async probeCommit() {
      // The commit's answer was lost. A refresh with a statement by the
      // PREPARED key tells: a pair of a higher generation means the commit
      // landed (the server read the statement only because the credential
      // was of a lower generation, and accepted it only under the new key).
      const { accountIdB64, accountIdBytes, secrets } = rotationSession();
      const deviceKey = await rotationDeviceKey(accountIdB64);
      const prepared = (await getKeyRotationState(accountIdB64, deviceKey)).prepared;
      const pair = sessionTokens.pair() ?? sessionTokens.supersededPair(accountIdB64);
      if (prepared === null || pair === null) return { kind: "unknown" };
      const wasm = await loadWasm();
      // The keys are stored before the pair (`commit`), so a device that
      // already holds the prepared key as its account key committed.
      if (
        base64Encode(publicKeyOfSeed(wasm, secrets.accountIdentitySeed)) === prepared.newSignaturePubkeyB64
      ) {
        markSignupPhraseOwed(plainStorage(), accountIdB64);
        return { kind: "landed", keyGeneration: pair.keyGeneration };
      }
      const entropy = base64Decode(prepared.entropyB64);
      const { seed } = phraseOfEntropy(wasm, entropy);
      try {
        const upgraded = await upgradeSessionWith(accountIdBytes, pair.refreshToken, seed);
        if (upgraded.verdict === "not-accepted") return { kind: "not-landed" };
        if (upgraded.verdict !== "accepted" || upgraded.pair === null) return { kind: "unknown" };
        if (upgraded.pair.keyGeneration <= pair.keyGeneration) {
          // The credential was not of a lower generation, so the server did
          // not read the statement: the account is where it was.
          installSession(accountIdB64, upgraded.pair);
          await persistSessionPair(accountIdB64, upgraded.pair, "key-rotation");
          return { kind: "not-landed" };
        }
        const adopted = await adoptRotatedKeys({
          accountIdB64,
          deviceKey,
          oldSeed: secrets.accountIdentitySeed.slice(),
          newEntropy: entropy,
          newPubkeyB64: prepared.newSignaturePubkeyB64,
        });
        installSession(accountIdB64, upgraded.pair);
        await persistSessionPair(accountIdB64, upgraded.pair, "key-rotation");
        markSignupPhraseOwed(plainStorage(), accountIdB64);
        await swapSessionAccountKeys(accountIdB64, adopted.seed, adopted.conversationSeed);
        return { kind: "landed", keyGeneration: upgraded.pair.keyGeneration };
      } finally {
        seed.fill(0);
      }
    },
    async uploadIdentity(mode, attestationB64Url, proofB64) {
      // Step 8: the bundle of the NEW identity with the proof AND the
      // attestation (the proven-rotation arm), or one of the two fallbacks.
      const { accountIdBytes, secrets } = rotationSession();
      const wasm = await loadWasm();
      const canonical = wasm.Identity.fromSeed(secrets.accountIdentitySeed);
      try {
        const attestation =
          mode === "proof-only" || attestationB64Url === null
            ? undefined
            : base64UrlDecodeNoPad(attestationB64Url);
        const proof = mode === "attestation-only" || proofB64 === null ? undefined : base64Decode(proofB64);
        // No genesis self-signature for a rotation: discovery forwards the
        // proof as the log leaf's authorisation (T5 item 19). The attested
        // reset fallback is a NEW binding and carries one.
        const leafAuth =
          mode === "attestation-only" ? await mintLeafGenesisAuth(canonical, accountIdBytes) : undefined;
        await uploadIdentityBundle(
          discovery,
          accountIdBytes,
          canonical,
          proof,
          leafAuth,
          undefined,
          attestation,
        );
      } finally {
        canonical.free();
      }
    },
    async preparedProofB64() {
      const { accountIdB64 } = rotationSession();
      const deviceKey = await rotationDeviceKey(accountIdB64);
      const stateNow = await getKeyRotationState(accountIdB64, deviceKey);
      return stateNow.prepared?.proofB64 ?? null;
    },
    async transferDeviceGroupRoot(attestationB64Url) {
      // Step 9: the OLD root signs a RootTransfer naming the root of the new
      // seed; with the attestation, the new account key endorses that root
      // and the journal records the generation.
      const { accountIdB64, accountIdBytes, secrets } = rotationSession();
      const deviceKey = await rotationDeviceKey(accountIdB64);
      const stateNow = await getKeyRotationState(accountIdB64, deviceKey);
      const oldSeedB64 = stateNow.predecessorSeedsB64[stateNow.predecessorSeedsB64.length - 1];
      if (oldSeedB64 === undefined) throw new Error("key rotation: no predecessor seed is held");
      const wasm = await loadWasm();
      const journalId = wasm.deviceGroupJournalId(accountIdBytes);
      const oldAccount = wasm.Identity.fromSeed(base64Decode(oldSeedB64));
      const newAccount = wasm.Identity.fromSeed(secrets.accountIdentitySeed);
      const oldRoot = wasm.dsJournalRootSigner(oldAccount, journalId);
      const newRoot = wasm.dsJournalRootSigner(newAccount, journalId);
      try {
        const sender = new DsClient();
        if (attestationB64Url === null) {
          await transferRoot(sender, journalId, oldRoot, newRoot);
          return;
        }
        const attestStdB64 = base64Encode(base64UrlDecodeNoPad(attestationB64Url));
        await transferRootWithAttestation(
          sender,
          journalId,
          oldRoot,
          newRoot,
          attestStdB64,
          rootEndorsementFor(wasm, secrets.accountIdentitySeed, journalId, attestStdB64, newRoot.publicKey()),
        );
      } finally {
        oldRoot.free();
        newRoot.free();
        oldAccount.free();
        newAccount.free();
      }
    },
    async predecessorSigningSeed() {
      const { accountIdB64 } = rotationSession();
      const deviceKey = await rotationDeviceKey(accountIdB64);
      const stateNow = await getKeyRotationState(accountIdB64, deviceKey);
      const oldSeedB64 = stateNow.predecessorSeedsB64[stateNow.predecessorSeedsB64.length - 1];
      return oldSeedB64 === undefined ? null : base64Decode(oldSeedB64);
    },
    async showNewPhrase() {
      // Step 12: the existing new-phrase screen, once nothing is owed.
      // The proof and the entropy were kept for the publishing steps; every
      // one of them has its answer now.
      const { accountIdB64 } = rotationSession();
      try {
        const deviceKey = await rotationDeviceKey(accountIdB64);
        await updateKeyRotationState(accountIdB64, deviceKey, (st) => ({ ...st, prepared: null }));
      } catch (err) {
        // eslint-disable-next-line no-console
        console.warn("key rotation: the prepared material was not cleared", err);
      }
      setRotationVersion((v) => v + 1);
      await restoreOwedSignupPhrase(accountIdB64);
    },
    async discardPrepared() {
      const { accountIdB64 } = rotationSession();
      const deviceKey = await rotationDeviceKey(accountIdB64);
      await updateKeyRotationState(accountIdB64, deviceKey, (st) => ({ ...st, prepared: null }));
      setRotationVersion((v) => v + 1);
    },
    reportSuperseded() {
      sessionTokens.reportSuperseded();
    },
  };

  const api: AuthApi = {
    keyRotation: keyRotationApi,
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
        phraseOwedInMemory.delete(s.account.accountId);
      }
      setSignupPhrase(null);
    },
    async claimGuestUsername(username) {
      const s = state();
      const secrets = identitySecrets();
      if (s.kind !== "session" || secrets === null || !usernameClaimOffered(s.account.username)) {
        throw new KeyedError("common.error.session_not_ready");
      }
      const accountIdB64 = s.account.accountId;
      const guest = guestState() !== null;
      if (guest) {
        // An account past its lifetime is not kept: the servers are about to
        // delete it, or already have.
        if (await api.expireGuestIfDue()) throw new GuestAccountExpiredError();
        // The phrase first: an account whose phrase this device cannot read
        // cannot be kept, so no name is taken for it.
        if ((await readGuestPhrase(accountIdB64)) === null) throw new GuestPhraseUnavailableError();
      }
      const token = sessionTokens.peek() ?? secrets.sessionToken;
      // The sign-up's order (ADR-0054 S5): the login handle at auth FIRST,
      // then the name at discovery; a discovery failure releases the handle.
      const handle = await loginHandleFor(username);
      const reg = await claimUsername({
        username,
        bind: async () => {
          await client.bindLoginHandle(token, handle.wire);
          loginHandleSync.markBound(token, handle.wire);
        },
        register: () => discovery.registerUsername(secrets.accountId, username),
        release: async () => {
          await client.releaseLoginHandle(token);
        },
      });
      // The name is the account's: the local marker, the first-password
      // marker (before the state moves, so a reload in between retries only
      // the password), then the live session.
      savePersisted({ v: 1, accountIdB64, username: reg.normalisedUsername });
      if (guest) {
        // `No, thanks` after this point frees the name at the directory
        // first (`guestDeletePlan`).
        noteGuestUsernameClaimed(guestStorage(), accountIdB64, reg.normalisedUsername);
        setGuestMarkerVersion((v) => v + 1);
      }
      markFirstPasswordOwed(plainStorage(), accountIdB64);
      setConfirmedUsername({ accountIdB64, username: reg.normalisedUsername });
      rememberUsername(accountIdB64, reg.normalisedUsername);
      setState({ kind: "session", account: { accountId: accountIdB64, username: reg.normalisedUsername } });
      return { username: reg.normalisedUsername, certificate: reg.usernameCertificate };
    },
    async applyRenamedUsername(username) {
      const s = state();
      if (s.kind !== "session") throw new KeyedError("common.error.session_not_ready");
      // The register first: the adoption rule never overrules a write of
      // this device that is on its way.
      rememberUsername(s.account.accountId, username);
      await applyOwnUsername(s.account.accountId, username);
    },
    firstPasswordOwed() {
      protectVersion();
      const s = state();
      return s.kind === "session" && isFirstPasswordOwed(plainStorage(), s.account.accountId);
    },
    offerProtectStep() {
      const s = state();
      if (s.kind !== "session") return;
      markProtectStepOwed(plainStorage(), s.account.accountId);
      setProtectVersion((v) => v + 1);
    },
    protectStepOwed,
    finishProtectStep() {
      const s = state();
      if (s.kind === "session") clearProtectStepOwed(plainStorage(), s.account.accountId);
      setProtectVersion((v) => v + 1);
    },
    async landEmailPasswordRecovery(username, accountId, session, whenOpened) {
      // Called with the session a mailed code just returned; from here the
      // landing is overtaken like any other sign-in.
      const flow = startFlow();
      const accountIdB64 = base64Encode(accountId);
      // The first place that holds the account id of the session it was
      // just given: open its stores, then let the screen write.
      openStoresFor(accountIdB64);
      whenOpened?.();
      setRestoreNotice(null);
      explicitSignInThisPage = true;
      // The mailed code already replaced the password.
      setNewPasswordOwed(false);
      // A mailed code is not a fresh-auth proof (the finish mints none).
      sessionLanding = "other";
      freshGrants.clear();
      await landAfterAccessRecovery(username, accountId, accountIdB64, session, flow);
    },
    landingDeviceIdFor(username) {
      return landingDevice({ username });
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
      // "Was it you?" has no such button: its own actions end it.
      if (current.reason !== "recall" && current.phase === "removed") setSignedOut(null);
    },
    recall: {
      check: recallCheck,
      end: finishRecallWipe,
      takeBack: recallTakeBack,
      checkByPhrase: recallCheckByPhrase,
      takeBackByPhrase: recallTakeBackByPhrase,
    },
    recallDone,
    dismissRecallDone: () => setRecallDone(false),
    reportDeviceRemoved(accountIdB64) {
      const s = state();
      const live =
        s.kind !== "anon" && s.kind !== "loading" ? s.account.accountId : sessionTokens.accountIdB64();
      if (live !== accountIdB64) return;
      // Off the caller's stack: the sync runtime calls this from inside its
      // own poll step, and the wipe tears that runtime down.
      void Promise.resolve().then(() => signOutAfterDeviceRemoved(accountIdB64));
    },
    keyResetNote,
    dismissKeyResetNote: () => setKeyResetNote(false),
    republishKeyPackage: () => keyPackagePublisher.publish(),
    rebindAfterStateBundle,
    async signup(username, password, options) {
      const trimmed = username.trim();
      if (trimmed.length === 0) {
        throw new Error("username must be non-empty");
      }
      const result = await client.signup(
        utf8(password),
        options?.signal !== undefined ? { signal: options.signal } : {},
      );
      // The commit point: the registration has finished on the server. From
      // here the account is completed and landed whatever happened on this
      // device meanwhile (a sign-out, another landing), so nothing below
      // asks the binding.
      const accountIdB64 = base64Encode(result.accountId);
      explicitSignInThisPage = true;
      // Install the fresh session so the registerUsername /
      // uploadIdentityBundle / uploadKeyPackage round-trip below
      // sails through the auth::require_session middleware.
      openStoresFor(accountIdB64);
      installFlowSession(accountIdB64, result.session);
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
      // ADR-0074 section 0.1: this first mint is also what records the
      // account key at auth.
      const signupGenesisAttest = await mintGenesisAndRecord(
        accountIdB64,
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
      // ADR-0075 D2: the phrase is generated and stored exactly as before,
      // and NOT shown. A device that holds the keys holds the phrase (the
      // hand-over to a new device, the email vault's seal, a later "Save a
      // recovery phrase").
      await storeDevicePhrase(accountIdB64, result.recoveryPhrase, deviceKey, "signup");
      // UX 19.1a: the "Protect your account" step is owed BEFORE the session
      // lands, so the route guards never see a signed-in state without it
      // (`/signup` sits in `<RequireAnonymous>`, which navigates home on
      // this very transition). The marker keeps the step coming back across
      // a reload until the person chose.
      markProtectStepOwed(plainStorage(), accountIdB64);
      setProtectVersion((v) => v + 1);
      // ADR-0076: the claimed name goes into the own-username register
      // (applied when the synced state of the new account is loaded).
      setConfirmedUsername({ accountIdB64, username: normalisedUsername });
      rememberUsername(accountIdB64, normalisedUsername);
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
      void subscribePushFor(accountIdB64).catch((err) => {
        // eslint-disable-next-line no-console
        console.warn("push subscribe (signup) failed", err);
      });
      return { recoveryPhrase: result.recoveryPhrase };
    },
    async signupGuest(options) {
      // ADR-0032 guest join. This is the `signup` flow up to (but NOT
      // including) the username claim: OPAQUE registration under a RANDOM
      // password, a device root key derived from the resulting export key,
      // the first-ever identity-bundle publish, and the AccountIdentity-seed
      // cache. Then the session lands at once: no recovery-phrase screen
      // stands between the person and the call they were invited to
      // (2026-10-02; the first version showed the phrase first).
      //
      // The password is 32 random bytes nobody sees, and no username is
      // claimed. Whether this becomes an account is asked after the call
      // (`claimGuestUsername` / `discardGuestAccount`); the servers delete an
      // account that never got a username a day after it was made.
      const randomPassword = base64Encode(
        crypto.getRandomValues(new Uint8Array(32)),
      );
      const result = await client.signup(
        utf8(randomPassword),
        options?.signal !== undefined ? { signal: options.signal } : {},
      );
      // The commit point, as in `signup`: the registration has finished on
      // the server, so the account is completed and landed whatever else
      // landed on this device meanwhile. Only a sign-out during the landing
      // itself stops it (`registered` below).
      const accountIdB64 = base64Encode(result.accountId);
      explicitSignInThisPage = true;
      // Install the fresh session for the protected writes below.
      openStoresFor(accountIdB64);
      installFlowSession(accountIdB64, result.session);
      // The grant the sign-up finish minted is NOT kept: nobody proved
      // anything for a guest, and its delete needs none.
      sessionLanding = "other";
      freshGrants.clear();

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
        const guestGenesisAttest = await mintGenesisAndRecord(
          accountIdB64,
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

      // The phrase stays on this device, wrapped under the device key, under
      // the guest's own key in the phrase store and NOT the account's: the
      // backup reads it by the account id, finds nothing, and so keeps
      // nothing on a server for an account the person has not decided to
      // keep. Fail loud: an account whose phrase is not kept could never be
      // kept afterwards (the email vault, a new device and "Save a recovery
      // phrase" all need it). It moves under the account's own key when the
      // first password is set (`replacePassword`).
      const guestPhraseKept = await storeDevicePhrase(
        guestPhraseKey(accountIdB64),
        result.recoveryPhrase,
        deviceKey,
        "signup-guest",
      );
      if (!guestPhraseKept) {
        throw new Error("the guest account's recovery phrase could not be kept on this device");
      }
      markGuestAccount(guestStorage(), accountIdB64, Date.now());
      setGuestMarkerVersion((v) => v + 1);

      await finishCanonicalSession({
        wasm,
        accountIdentitySeed,
        accountIdBytes: result.accountId,
        accountIdB64,
        exportKey: result.exportKey,
        session: result.session,
        username: "",
        guest: true,
        registered: true,
      });
    },
    isGuest: () => guestState() !== null,
    async discardGuestAccount() {
      const current = state();
      const secrets = identitySecrets();
      const guest = guestState();
      if (current.kind !== "session" || secrets === null || guest === null) {
        throw new Error("no guest account to delete");
      }
      // The servers first. A required delete that fails leaves everything in
      // place so the person can try again; the order frees a username an
      // unfinished Create account took (`guestDeletePlan`).
      await deleteGuestOnServers(guest, sessionTokens.peek() ?? secrets.sessionToken, true);
      // Everything on this device, the guest marker and the phrase row
      // included (`wipe-account.ts`). Lands signed out.
      await api.forgetDevice();
      setGuestMarkerVersion((v) => v + 1);
    },
    async expireGuestIfDue() {
      const current = state();
      const secrets = identitySecrets();
      const guest = guestState();
      if (current.kind !== "session" || secrets === null || guest === null) return false;
      if (!guestExpired(guest, Date.now())) return false;
      await deleteGuestOnServers(guest, sessionTokens.peek() ?? secrets.sessionToken, false);
      await api.forgetDevice();
      setGuestMarkerVersion((v) => v + 1);
      return true;
    },
    async login(username, password) {
      // ADR-0054 S5: sign in by name. The start carries the login handle
      // of the typed name, never an account id, and nothing is looked up
      // before the session exists: the account id comes back in the finish.
      // An unknown name reads as a wrong password (the server's stand-in).
      // The handle is re-bound once the session exists (idempotent).
      //
      // The binding this sign-in starts under. If the person signs out, or
      // another session lands on this device (a guest account from a call
      // link, a passkey sign-in), before the round trip returns, the late
      // response is dropped: nothing is installed, persisted or shown.
      const flow = startFlow();
      const target = await nameTargetFor(username);
      flow.require();
      const resolvedUsername = target.username;
      // A password sign-in owes no new-password step and explains nothing.
      setNewPasswordOwed(false);
      setRestoreNotice(null);
      const result = await accessByName({
        target,
        run: (handle) =>
          client.login(handle, utf8(password), landingDevice({ username: target.username })),
        bind: bindIfCurrent(flow),
      });
      flow.require();
      const accountIdBytes = result.accountId;
      const accountIdB64 = base64Encode(accountIdBytes);
      explicitSignInThisPage = true;
      // Install the fresh session for any protected writes that follow.
      openStoresFor(accountIdB64);
      installFlowSession(accountIdB64, result.session);
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
      flow.require();
      // Upsert is harmless; also persist the (accountId, username)
      // marker so subsequent reloads skip the lookup round-trip.
      await putDeviceKey(accountIdB64, deviceKey);
      flow.require();
      // Refresh the persisted session pair alongside the device key. Same
      // best-effort posture as signup: a persistence failure does not block
      // login.
      try {
        await putSessionPair(accountIdB64, result.session, deviceKey);
      } catch (err) {
        // eslint-disable-next-line no-console
        console.warn("session-pair persist (login) failed", err);
      }
      flow.require();
      // Phase 2a2c2 — refresh the cached export key. Same
      // best-effort posture.
      try {
        await putExportKey(accountIdB64, result.exportKey, deviceKey);
      } catch (err) {
        // eslint-disable-next-line no-console
        console.warn("export-key persist (login) failed", err);
      }
      flow.require();
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
      flow.require();
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
      flow.require();
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
      if (!flow.current()) {
        accountIdentitySeed?.fill(0);
        throw new StaleBindingError();
      }

      if (accountIdentitySeed === null) {
        // Fresh-device fail-safe (ADR-0017). Auth succeeded; identity
        // restore is pending the phrase. Stash the context and flip
        // state — `provideRecoveryPhrase` finishes from here.
        await parkForRestore(
          {
            accountIdBytes,
            accountIdB64,
            exportKey: result.exportKey,
            deviceKey,
            session: result.session,
            username: resolvedUsername,
          },
          flow,
        );
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
      // ADR-0072 decision 6 / UX spec section 4. Discoverable: no account
      // has to be cached on this device and no name typed. The assertion
      // grants ACCESS; what happens next is the shared ACCESS-only landing,
      // with one addition on a device without the keys: this passkey is
      // asked for them before the restore prompt is shown.
      //
      // Overtaken like `login`: the ceremony can sit behind the browser's
      // passkey sheet for a long time, and whatever landed on this device in
      // the meantime stays.
      const flow = startFlow();
      const assertion = await runPasskeyCeremony();
      flow.require();
      const accountIdB64 = assertion.account_id_b64;
      const accountIdBytes = base64Decode(accountIdB64);
      const assertionSession = sessionPairOf(assertion);
      setRestoreNotice(null);
      explicitSignInThisPage = true;
      openStoresFor(accountIdB64);
      installFlowSession(accountIdB64, assertionSession);
      const cached = loadPersisted();
      const username =
        cached !== null && cached.accountIdB64 === accountIdB64
          ? cached.username
          : await usernameNamingAccount(usernameHint, accountIdBytes);
      flow.require();
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
        flow,
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
      const flow = startFlow();
      const assertion = await runPasskeyCeremony(pending.accountIdB64);
      // Signed out, or another account landed, behind the passkey sheet.
      requireStillParked(pending, flow);
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
    async replacePassword(newPassword, options) {
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
      const ungated =
        options?.afterAccountNotice === true ||
        (options?.firstPassword === true && isFirstPasswordOwed(plainStorage(), s.account.accountId));
      if (!ungated && !api.passwordChangeAllowed()) {
        throw new FreshAuthRequiredError();
      }
      const accountIdB64 = s.account.accountId;
      // A guest keeping its account (`GuestAccountOffer`): its phrase is
      // read BEFORE the password is set, so an account that cannot be kept
      // is refused with its random password still in place.
      const keptGuest = options?.firstPassword === true && guestState() !== null;
      let guestPhrase: string | null = null;
      if (keptGuest) {
        if (await api.expireGuestIfDue()) throw new GuestAccountExpiredError();
        guestPhrase = await readGuestPhrase(accountIdB64);
        if (guestPhrase === null) throw new GuestPhraseUnavailableError();
      }
      const target = await nameTargetFor(s.account.username);
      // `AuthClient.recover` uses the seed as the OPAQUE recovery input; hand
      // it a copy so the in-memory secret is never the buffer that is wiped.
      const seed = secrets.accountIdentitySeed.slice();
      try {
        const result = await accessByName({
          target,
          run: (handle) =>
            client.recover(handle, seed, utf8(newPassword), landingDevice({ accountIdB64 })),
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
        if (guestPhrase !== null) {
          await keepGuestAccount({
            accountIdBytes: secrets.accountId,
            accountIdB64,
            accountIdentitySeed: secrets.accountIdentitySeed,
            sessionToken: result.session.sessionToken,
            phrase: guestPhrase,
          });
        }
        // A guest's first password is set: the ungated change is over.
        clearFirstPasswordOwed(plainStorage(), s.account.accountId);
        setProtectVersion((v) => v + 1);
        // A new password ends what the old proof and the phrase check
        // stood for.
        freshGrants.clear();
        phraseCheckedAtMs = null;
      } finally {
        seed.fill(0);
      }
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
      const flow = startFlow();
      const wasm = await loadWasm();
      requireStillParked(pending, flow);
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
      // Signed out, or another account landed, while the bundle was being
      // fetched: persist nothing.
      if (!stillParked(pending) || !flow.current()) {
        accountIdentitySeed.fill(0);
        throw new StaleBindingError();
      }

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
      // (The account-backup restore below reads it again from here if its
      // fetch fails, and it is what lets this device keep the backup current
      // afterwards.)
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

      // This route restores the identity from the phrase, so it owes the
      // account's backup (contacts, synced state, the list of conversations)
      // like the restore screen does. Marked BEFORE the session lands: the
      // automatic backup uploads nothing while it is owed, so it cannot
      // replace the blob the restore is about to read.
      if (!stillParked(pending) || !flow.current()) {
        accountIdentitySeed.fill(0);
        throw new StaleBindingError();
      }
      markBackupRestoreOwed(pending.accountIdB64);

      await finishCanonicalSession({
        wasm,
        accountIdentitySeed,
        accountIdBytes: pending.accountIdBytes,
        accountIdB64: pending.accountIdB64,
        exportKey: pending.exportKey,
        session: pending.session,
        username: pending.username,
        restorePhrase: canonicalPhrase,
      });
    },
    async completeFromPairing(phraseEntropy, session, persistJoinState) {
      // ADR-0018 factor (a), path (a) — same precondition as
      // `provideRecoveryPhrase`: only valid while a login/passkey
      // auth is parked in the `needs-recovery-phrase` fail-safe.
      const pending = pendingRecovery();
      if (pending === null) {
        phraseEntropy.fill(0);
        throw new Error("no pending identity recovery — sign in first");
      }
      const flow = startFlow();
      const wasm = await loadWasm();
      if (!stillParked(pending) || !flow.current()) {
        phraseEntropy.fill(0);
        throw new StaleBindingError();
      }
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
      // Signed out, or another account landed, while the bundle was being
      // fetched: persist nothing.
      if (!stillParked(pending) || !flow.current()) {
        phraseSeed.fill(0);
        throw new StaleBindingError();
      }

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

      // The anonymous join (no device key at pairing time) held its
      // DeviceIdentity seed in memory and fetched the history and state
      // bundles without applying them: land all of it under the device key
      // that now exists, before the session does. Best-effort, as in
      // `completeFromDeviceRestore` - a failure orphans the DeviceGroup leaf
      // locally and a re-pair recovers it.
      if (persistJoinState !== undefined) {
        try {
          await persistJoinState(deviceKey);
        } catch (err) {
          // eslint-disable-next-line no-console
          console.warn(
            "join-state persist (completeFromPairing) failed",
            err instanceof Error ? err.name : "unknown",
          );
        }
      }

      if (!stillParked(pending) || !flow.current()) {
        phraseSeed.fill(0);
        throw new StaleBindingError();
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
      // The binding this restore starts under. Overtaken (a sign-out, or
      // another account landing while the gates below ran): the transferred
      // secret is zeroed and nothing is persisted, attached or landed.
      const flow = startFlow();
      const wasm = await loadWasm();
      if (!flow.current()) {
        phraseEntropy.fill(0);
        throw new StaleBindingError();
      }
      // Recovery payload v2: rebuild the phrase and the 64-byte seed from
      // the transferred entropy (which is zeroized here). Nothing is
      // written before the gates below.
      const { phrase: transferredPhrase, seed: phraseSeed } = phraseFromTransferredEntropy(
        wasm,
        phraseEntropy,
      );
      const requireCurrent = (): void => {
        if (flow.current()) return;
        phraseSeed.fill(0);
        throw new StaleBindingError();
      };
      // The persistence and the session landing; runs only after every
      // gate below passed.
      const landDeviceRestore = async (): Promise<void> => {
        requireCurrent();
        openStoresFor(accountIdB64);
        // Bootstrap the device root key FROM the identity seed — there
        // is no password (hence no OPAQUE export key) on this device
        // yet. Later password logins PREFER this stored key over
        // re-deriving from the export key, so the wrapped stores stay
        // readable.
        const deviceKey = await deriveDeviceRootKeyFromIdentitySeed(
          phraseSeed,
          accountIdB64,
        );
        requireCurrent();
        await putDeviceKey(accountIdB64, deviceKey);
        requireCurrent();
        try {
          await putAccountIdentitySeed(accountIdB64, phraseSeed, deviceKey);
        } catch (err) {
          // eslint-disable-next-line no-console
          console.warn(
            "account-identity-seed persist (completeFromDeviceRestore) failed",
            err,
          );
        }
        requireCurrent();
        // This device now holds the phrase like the one that confirmed it.
        await storeDevicePhrase(accountIdB64, transferredPhrase, deviceKey, "device-restore");
        requireCurrent();
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
        requireCurrent();
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
        requireCurrent();

        installFlowSession(accountIdB64, session);
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
      try {
        await runNameCheckedDeviceRestore({
          username,
          accountIdBytes,
          phraseSeed,
          sessionToken: session.sessionToken,
          verifySeed: async () => {
            await verifySeedAgainstPublishedIdentity({
              wasm,
              discovery,
              accountIdBytes,
              accountIdentitySeed: phraseSeed,
              source: "device",
            });
            // Before the redeemed session is attached to the token pool.
            requireCurrent();
          },
          attachSession: (token) => anonTokens.setSessionToken(token),
          dropSession: () => {
            // Overtaken: the session installed now is another account's.
            if (!flow.current()) return;
            dropSession();
            // The account's tokens stay stored for its next sign-in.
            anonTokens.unbind();
          },
          lookup: discoveryAccountLookup(discovery),
          land: landDeviceRestore,
        });
      } catch (err) {
        // Whatever a gate answered after the binding ended, the person has
        // moved on: an abandonment, not a refusal to show.
        if (!flow.current() && !(err instanceof StaleBindingError)) {
          phraseSeed.fill(0);
          throw new StaleBindingError();
        }
        throw err;
      }
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
        // The pair this yields replaces the session's, so it must name the
        // same device (ADR-0074 section 0.4).
        signedIn = await client.login(
          target.handle.wire,
          utf8(password),
          landingDevice({ accountIdB64: account.accountIdB64 }),
        );
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
        const deviceIdB64 = landingDevice({ accountIdB64: account.accountIdB64 });
        assertion = await new WebauthnClient().loginWithPasskey({
          accountIdB64: account.accountIdB64,
          ...(deviceIdB64 !== undefined ? { deviceIdB64 } : {}),
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
      // "Reset account keys" (ADR-0073 section 2.3, UX 15.1). Reachable ONLY
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
      const flow = startFlow();
      const wasm = await loadWasm();
      requireStillParked(pending, flow);

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

      // 3-5. Persist, then publish: the one path a recall shares.
      await adoptKeyResetOutcome({
        accountIdB64: pending.accountIdB64,
        accountIdBytes: pending.accountIdBytes,
        username: pending.username,
        deviceKey: pending.deviceKey,
        seed: accountIdentitySeed,
        phrase,
        newPubkey,
        reset,
      });
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
      // Signed out during the load: `logout` zeroed the seed and dropped
      // the stash.
      if (pendingFreshPhrase()?.seed !== pending.seed) throw new StaleBindingError();
      // A guest account has no username and so no password to replace; a
      // reset reached through "Forgot password?" keeps the offer (its
      // recovery credential now belongs to the new phrase, so the reset
      // works).
      if (pending.username === "") setNewPasswordOwed(false);
      // CONSUME the stash so the parked seed cannot be replayed.
      setPendingFreshPhrase(null);
      // The person has just confirmed the phrase is saved: it is no longer
      // owed, and the landing below must not show it a second time.
      clearSignupPhraseOwed(plainStorage(), pending.accountIdB64);
      phraseOwedInMemory.delete(pending.accountIdB64);
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
      // Every route on /recover that holds the whole phrase ends here (the
      // typed phrase, three words checked on this device, an email code).
      // The password is replaced on this path, so no new-password step is
      // owed.
      setNewPasswordOwed(false);
      explicitSignInThisPage = true;
      // ADR-0054 S5: the reset is addressed by the name's login handle (the
      // typed name, else the account cached on this device) and the account
      // id comes back in the finish. No lookup runs before the session.
      //
      // Overtaken like `login`: a reset whose round trip returns after a
      // sign-out or another landing changes nothing on this device. (The
      // password it set on the server stays set; that part cannot be undone
      // from here.)
      const flow = startFlow();
      const target = await nameTargetFor(
        resolveRecoverUsername({ cached: loadPersisted(), username }),
      );
      flow.require();
      const resolvedUsername = target.username;
      // Parse the phrase up front so a bad mnemonic surfaces
      // before we spend a limited attempt. `fromPhrase` collapses
      // unknown-word and bad-checksum into one error message —
      // by design (preserves BIP39's no-probing property).
      const wasm = await loadWasm();
      flow.require();
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
          run: (handle) =>
            client.recover(
              handle,
              seed,
              utf8(newPassword),
              landingDevice({ username: target.username }),
            ),
          bind: bindIfCurrent(flow),
        });
        flow.require();
        const accountIdBytes = result.accountId;
        const accountIdB64 = base64Encode(accountIdBytes);
        openStoresFor(accountIdB64);
        installFlowSession(accountIdB64, result.session);
        // Re-derive the device root key from the NEW export key.
        // The previous device key (if any) is no longer reachable
        // from the new password; the upsert in IDB drops it.
        const deviceKey = await deriveDeviceRootKey(
          result.exportKey,
          accountIdB64,
        );
        flow.require();
        await putDeviceKey(accountIdB64, deviceKey);
        flow.require();
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
        flow.require();
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
        flow.require();
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
        flow.require();
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
        flow.require();

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
        flow.require();
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
        flow.require();
        // Re-cache the phrase under the new device key so the backup keeps
        // working from this device.
        try {
          await storeDevicePhrase(accountIdB64, canonicalPhrase, deviceKey, "recover");
        } catch (err) {
          // eslint-disable-next-line no-console
          console.warn("phrase cache (recover) failed", err);
        }
        flow.require();
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
        flow.require();

        // ADR-0017: build the new canonical Identity from the
        // (UNCHANGED) BIP39 phrase seed — NOT the rotated export
        // key. The recovered identity is byte-for-byte the same as
        // before the recover, which is the whole point: contacts
        // never re-verify. ChatProvider free()s the prior
        // (pre-recovery) Identity if anything was lingering.
        const canonical = wasm.Identity.fromSeed(accountIdentitySeed);
        await installSessionIdentities(wasm, canonical, accountIdB64, false, flow.current);
        flow.require();
        keyPackageAccount = { accountIdBytes, accountIdB64 };

        // 1.16e6c3: rehydrate path is a guaranteed no-op here —
        // we just deleted the snapshot row above — but routing
        // through the same helper keeps the auth flows uniform
        // and resilient to future re-orderings.
        await rehydrateMlsState(chats, accountIdB64);
        flow.require();

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
        flow.require();
        // Fresh KeyPackage: create -> durable persist -> upload.
        await publishKeyPackageBestEffort("recover");
        // The last question before this reset lands. From the landing on,
        // what follows (the backup restore and baseline, the vault, push)
        // is this account's own work and names the account explicitly.
        flow.require();

        // The account's backup is fetched and applied below. Marked owed
        // BEFORE the session lands: the automatic backup uploads nothing
        // while it is, so it cannot replace the blob the restore is about
        // to read; and a tab closed before the restore ran leaves the flag
        // for the next landing to retry.
        markBackupRestoreOwed(accountIdB64);

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
        // The account's backup: fetch it, run the ADR-0037 head gate, and
        // apply the contacts, the synced state and the list of
        // conversations (`backup/restore.ts`, the one function every
        // phrase route runs) BEFORE the baseline below re-uploads, so the
        // new baseline carries the merged set instead of overwriting the
        // server blob with an empty one. No MLS state comes from a backup:
        // this device's MLS state is whatever it held locally, which on
        // this route is nothing (the snapshot was dropped above), and the
        // other members admit it to its chats (ADR-0050 section 2.5a).
        //
        // G-10: AWAITED, not fire-and-forget. The baseline re-seals the
        // backup envelope; until it lands the server still holds the
        // pre-recover one. Awaiting folds the rewrite into the recover
        // round-trip. Both halves are best-effort: a backup hiccup never
        // fails the recover itself.
        const restoreOutcome = await restoreAccountBackup({
          accountIdBytes,
          accountIdB64,
          deviceKey,
          sessionToken: result.session.sessionToken,
          phrase: canonicalPhrase,
        });
        if (restoreOutcome.kind === "deferred") {
          // The backup could not be fetched. Uploading a baseline now would
          // replace it with this device's empty state before the retry on
          // the next landing can read it. The phrase is already cached
          // above, so the retry and the later automatic backup both work.
          // eslint-disable-next-line no-console
          console.warn("post-recover backup baseline skipped: the restore is still owed");
        } else {
          try {
            await bootstrapBackupFromPhraseString(
              {
                accountId: accountIdBytes,
                accountIdB64,
                ed25519Seed: canonical.ed25519Seed,
                deviceKey,
                sessionToken: result.session.sessionToken,
                // The groups this device holds plus the ones the restored
                // backup named that it has not joined yet.
                knownGroupIds: backupKnownGroupIds(
                  accountIdB64,
                  chats.groups().map((g) => g.groupIdB64),
                ),
              },
              mnemonicPhrase,
            );
          } catch (err) {
            // eslint-disable-next-line no-console
            console.warn("post-recover backup bootstrap failed", err);
          }
        }
        // 1.20b: best-effort push subscribe after recovery —
        // the new password changes the device key, but the
        // device push pseudonym persists in localStorage so
        // the server's existing token row simply gets re-keyed.
        void subscribePushFor(accountIdB64).catch((err) => {
          // eslint-disable-next-line no-console
          console.warn("push subscribe (recover) failed", err);
        });
      } catch (err) {
        // Abandoned before it landed: the identity seed copy never reached
        // the in-memory secrets, so it is this call's to zero.
        if (err instanceof StaleBindingError) accountIdentitySeed.fill(0);
        throw err;
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
      const flow = startFlow();
      const target = await nameTargetFor(trimmed);
      flow.require();
      const resolvedUsername = target.username;
      const result = await accessByName({
        target,
        run: (handle) =>
          client.totpRecover(handle, code, landingDevice({ username: target.username })),
        bind: bindIfCurrent(flow),
      });
      // Overtaken while the code was being verified: dropped, like `login`.
      flow.require();
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
      await landAfterAccessRecovery(resolvedUsername, accountIdBytes, accountIdB64, result.session, flow);
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
      const flow = startFlow();
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
              return await client.recoveryLogin(
                handle,
                seed,
                landingDevice({ accountIdB64: found.accountIdB64 }),
              );
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
          bind: bindIfCurrent(flow),
        });
        // Overtaken while the sign-in was out: dropped, like `login`.
        flow.require();
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
        await landAfterAccessRecovery(target.username, result.accountId, found.accountIdB64, result.session, flow);
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
      rotationVersion();
      return (
        st.kind === "session" &&
        (isKeyResetOwed(plainStorage(), st.account.accountId) ||
          rotationWithholdsPhrase(readRotation(plainStorage(), st.account.accountId)))
      );
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
      // A session landing still in flight must not land after this.
      binding.signedOut();
      // Keep localStorage + IDB device key intact. Re-login on
      // this device remains cheap, and the device key stays
      // available for any encrypted local stores 1.16f wires up.
      // Drop the in-memory Identity secrets — the user just
      // signed out.
      const cached = loadPersisted();
      // ADR-0074 section 2.1 (UX 18.2, "Log out"): sign this device's
      // session out at the server too, so a copy of what it held cannot keep
      // using the account. Best effort and invisible: the local sign-out
      // below never waits for it. The marker is set first; when the server
      // could not be told, the stored pair stays (unusable as a session,
      // `decideSilentRestore`) for ONE retry at the next launch.
      const heldPair = sessionTokens.pair();
      const heldAccount = sessionTokens.accountIdB64();
      const serverSide: Promise<LogoutAttempt> =
        heldPair !== null && heldAccount !== null
          ? (markLogoutOwed(plainStorage(), heldAccount), serverLogout(heldPair))
          : Promise.resolve("done");
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
      //
      // Sealed first (`account-store-gate.ts`): a session write still in
      // flight when this runs (a sign-in's best-effort persist) must not
      // put the row back after the delete. Nothing else of the account is
      // closed by a sign-out; its next landing opens the row again.
      {
        const bound = boundAccountId();
        const ended = new Set<string>();
        if (bound !== null) ended.add(bound);
        if (cached !== null) ended.add(cached.accountIdB64);
        if (heldAccount !== null) ended.add(heldAccount);
        for (const account of ended) {
          sealAccountSession(account);
          // The page-lifetime caches that are cheap to re-read and would
          // otherwise outlive the session (`account-memory.ts`). The account's
          // stored data stays.
          dropAccountMemory(accountStoreIds(account), "sign-out");
        }
      }
      // ADR-0074: the row is deleted once the server holds the logout; while
      // that is owed it stays for the one retry, and the marker keeps a
      // reload from restoring a session out of it.
      const storedFor = heldAccount ?? cached?.accountIdB64 ?? null;
      if (storedFor !== null) {
        void serverSide.then((outcome) => {
          if (outcome !== "done") return;
          // Unless a sign-in installed a new session meanwhile (it cleared
          // the marker and owns the row now).
          if (!isLogoutOwed(plainStorage(), storedFor) && heldAccount !== null) return;
          clearLogoutOwed(plainStorage(), storedFor);
          void deleteSessionToken(storedFor).catch(() => {
            /* best-effort */
          });
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
      // A session landing still in flight must not land after this.
      binding.signedOut();
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
      // ADR-0074 section 2.1: the session is signed out at the server too,
      // so a copy of the credentials this device held stops working. Best
      // effort, once: the wipe below leaves nothing to retry it with. (The
      // row delete above tombstoned this device's id; the wipe removes the
      // device seed, so the next setup on this browser mints a new one.)
      void serverLogout(sessionTokens.pair());
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
        phraseOwedInMemory.delete(wipeTarget);
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
 * ADR-0073: `keyResetAttest`, when present, is the key-reset attestation
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
