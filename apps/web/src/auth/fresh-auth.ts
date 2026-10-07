// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * The fresh-auth proof (ADR-0065 as amended 2026-10-06;
 * docs/ux/identity-recovery.md section 17 and 15.1a).
 *
 * Resetting the account keys, deleting the account, adding a device, adding
 * a passkey and changing the password each need the person to prove who they
 * are NOW, not merely hold a signed-in session. The proof is a grant the
 * server mints on a password sign-in (`POST /login/finish`), a passkey
 * sign-in (`POST /webauthn/auth/finish`) and the signup finish
 * (`POST /signup/finish`), valid for ten minutes and reusable across those
 * actions within its lifetime (a key reset spends every grant of the old
 * generation). Without a usable one the server answers
 * `403 {"error":"fresh-auth-required"}`.
 *
 * The grant is an opaque string to the client. It is held IN MEMORY ONLY:
 * never written to any store and never logged. It is bound to the account
 * it was issued for, dropped on sign-out and on the account wipe, dropped
 * after a key reset, and not handed out once expired.
 *
 * This module is pure (the caller passes the clock): the holder, the shared
 * proof block's decision, each surface's "what is shown", the failure
 * mapping and the password-change gate are pinned by tests.
 */

/** A grant as a sign-in's finish returned it. */
export interface FreshGrant {
  /** Opaque; sent back verbatim as `fresh_grant`. Never parsed. */
  readonly grant: string;
  /** Absolute expiry, unix milliseconds. */
  readonly expiresAtMs: number;
}

/** The two fields the three minting routes add to their answer. */
export interface FreshGrantWire {
  fresh_grant?: unknown;
  fresh_grant_expires_at_ms?: unknown;
}

/** The machine code of the server's `403` when an action carries no usable
 *  grant (missing, malformed, expired, another account's, stale generation). */
export const CODE_FRESH_AUTH_REQUIRED = "fresh-auth-required";

/** The header `DELETE /account` carries the grant in (it has no body). */
export const FRESH_GRANT_HEADER = "x-lakko-fresh-grant";

/**
 * Read the grant out of a finish response, or `null` when the response
 * carries none or carries one of the wrong shape. Never throws: a sign-in
 * must not fail because the grant is absent; the surfaces then ask for a
 * proof.
 */
export function freshGrantOf(wire: FreshGrantWire | null | undefined): FreshGrant | null {
  if (wire === null || wire === undefined) return null;
  const grant = wire.fresh_grant;
  const expiresAtMs = wire.fresh_grant_expires_at_ms;
  if (typeof grant !== "string" || grant.length === 0) return null;
  if (typeof expiresAtMs !== "number" || !Number.isFinite(expiresAtMs) || expiresAtMs <= 0) {
    return null;
  }
  return { grant, expiresAtMs };
}

/** The in-memory holder of at most one grant. */
export interface FreshGrantHolder {
  /** Keep `grant` for `accountIdB64`, replacing whatever was held. */
  put(accountIdB64: string, grant: FreshGrant): void;
  /** The grant string for `accountIdB64` when one is held and not expired
   *  at `nowMs`; otherwise `null`. An expired grant, and a grant of another
   *  account, are dropped by the read. */
  live(accountIdB64: string, nowMs: number): string | null;
  /** Drop the grant if it is the one given (the server refused it). A newer
   *  grant put meanwhile is kept. */
  spend(grant: string): void;
  /** Drop whatever is held (sign-out, account wipe, after a key reset). */
  clear(): void;
}

export function createFreshGrantHolder(): FreshGrantHolder {
  let held: { readonly accountIdB64: string; readonly grant: FreshGrant } | null = null;
  return {
    put(accountIdB64, grant) {
      held = { accountIdB64, grant };
    },
    live(accountIdB64, nowMs) {
      if (held === null) return null;
      if (held.accountIdB64 !== accountIdB64 || held.grant.expiresAtMs <= nowMs) {
        held = null;
        return null;
      }
      return held.grant.grant;
    },
    spend(grant) {
      if (held !== null && held.grant.grant === grant) held = null;
    },
    clear() {
      held = null;
    },
  };
}

// ---------------------------------------------------------------------------
// How the session was landed (UX 17.6)
// ---------------------------------------------------------------------------

/** How this page's session was landed; kept in memory with the session. It
 *  decides which row of the reset screen opens first (17.2). */
export type SessionLanding = "password" | "passkey" | "authenticator" | "other";

// ---------------------------------------------------------------------------
// The shared proof block (UX 17.1)
// ---------------------------------------------------------------------------

export type ProofPhase =
  /** Nothing is running. */
  | "idle"
  /** A proof (password sign-in) or the surface's own action is running. */
  | "checking"
  /** The platform's passkey sheet is up. */
  | "passkey";

export interface ProofBlockInput {
  /** A live proof is held for this account right now. */
  readonly proofLive: boolean;
  /** The block was already drawn on this surface (it stays once shown: a
   *  field must not vanish under the person's hands; a completed proof
   *  hides it again). */
  readonly revealed: boolean;
  /** The surface keeps the block folded away until its main button is
   *  first pressed without a proof (Settings cards, where a password field
   *  standing open on the page would be noise). The press reveals it. */
  readonly collapsed: boolean;
  /** The account has at least one passkey AND this browser can use
   *  passkeys. */
  readonly passkeyUsable: boolean;
  /** The surface leads with the passkey when one is usable (17.4): a notice
   *  instead of the field, the main button opens the sheet. */
  readonly passkeyFirst: boolean;
  /** The person chose `Use your password instead` on a passkey-first
   *  surface. */
  readonly passwordChosen: boolean;
  /** The account's username is known on this device. */
  readonly usernameKnown: boolean;
  readonly password: string;
  readonly username: string;
  readonly phase: ProofPhase;
  /** A rate limit is being waited out. */
  readonly waiting: boolean;
}

/** What the surface's main button does about the proof when pressed. */
export type ProofRoute =
  /** A proof is live: go straight on. */
  | "none"
  /** Sign in with the typed password first. */
  | "password"
  /** Open the passkey sheet first. */
  | "passkey";

export interface ProofBlockView {
  /** The block is drawn at all. */
  readonly visible: boolean;
  /** `You will be asked for your passkey.` in place of the field. */
  readonly showPasskeyNotice: boolean;
  readonly showPassword: boolean;
  /** The username field above the password (only with the password field). */
  readonly showUsername: boolean;
  /** The ghost button `Use a passkey instead`. Absent, never disabled, when
   *  no passkey is usable. */
  readonly showUsePasskey: boolean;
  /** The ghost button `Use your password instead` (passkey-first only). */
  readonly showUsePassword: boolean;
  /** The field and the proof's own buttons are disabled (checking, sheet
   *  up, or waiting out a limit for the field). */
  readonly fieldDisabled: boolean;
  /** `Use a passkey instead` is disabled (checking or sheet up only: it
   *  stays usable through a rate limit). */
  readonly passkeyButtonDisabled: boolean;
  /** What the main button does about the proof. */
  readonly route: ProofRoute;
  /** The proof side allows the main button: a proof is live, or the route
   *  it would take has what it needs, and nothing is running. */
  readonly ready: boolean;
}

/**
 * What the proof block shows and whether the surface's main button may be
 * pressed, as far as the proof is concerned. With a live proof (and nothing
 * revealed) the block is absent and the surface is exactly what it was
 * before section 17.
 */
export function proofBlockView(input: ProofBlockInput): ProofBlockView {
  const running = input.phase !== "idle";
  if (input.proofLive && !input.revealed) {
    return {
      visible: false,
      showPasskeyNotice: false,
      showPassword: false,
      showUsername: false,
      showUsePasskey: false,
      showUsePassword: false,
      fieldDisabled: running,
      passkeyButtonDisabled: running,
      route: "none",
      ready: !running,
    };
  }
  if (input.collapsed && !input.revealed) {
    // Not asked yet: the main button is what opens the block.
    return {
      visible: false,
      showPasskeyNotice: false,
      showPassword: false,
      showUsername: false,
      showUsePasskey: false,
      showUsePassword: false,
      fieldDisabled: running,
      passkeyButtonDisabled: running,
      route: "password",
      ready: !running,
    };
  }
  const passkeyLed = input.passkeyFirst && input.passkeyUsable && !input.passwordChosen;
  const showPassword = !passkeyLed;
  const showUsername = showPassword && !input.usernameKnown;
  const filled =
    input.password.length > 0 && (!showUsername || input.username.trim().length > 0);
  // A proof that went live while the block is still drawn (another surface
  // obtained it) needs nothing more from this one.
  const route: ProofRoute = input.proofLive ? "none" : passkeyLed ? "passkey" : "password";
  const ready =
    !running &&
    (route === "none" || route === "passkey" || (filled && !input.waiting));
  return {
    visible: true,
    showPasskeyNotice: passkeyLed,
    showPassword,
    showUsername,
    showUsePasskey: showPassword && input.passkeyUsable,
    showUsePassword: passkeyLed,
    fieldDisabled: running || input.waiting,
    passkeyButtonDisabled: running,
    route,
    ready,
  };
}

// ---------------------------------------------------------------------------
// Failure mapping
// ---------------------------------------------------------------------------

/** The action needs a proof and none is usable: no grant is held, or the
 *  server refused the one sent with `403 fresh-auth-required`. Nothing was
 *  done. The surface shows the block; it is not an error. */
export class FreshAuthRequiredError extends Error {
  override name = "FreshAuthRequiredError";
  constructor() {
    super("this action needs a fresh proof of the password or a passkey");
  }
}

/** The password typed into the block did not sign in. Nothing was done. */
export class ProofWrongPasswordError extends Error {
  override name = "ProofWrongPasswordError";
  constructor() {
    super("the password did not match");
  }
}

/** The passkey sheet was dismissed. Nothing was done. */
export class ProofCancelledError extends Error {
  override name = "ProofCancelledError";
  constructor() {
    super("the passkey gesture was cancelled");
  }
}

/** The passkey that answered belongs to another account. Nothing was
 *  installed. */
export class ProofOtherAccountError extends Error {
  override name = "ProofOtherAccountError";
  constructor() {
    super("the passkey belongs to another account");
  }
}

/** The passkey check failed for any other reason (an HTTP refusal, an
 *  unsupported browser, a sign-in that minted no grant). */
export class ProofPasskeyFailedError extends Error {
  override name = "ProofPasskeyFailedError";
  constructor() {
    super("the passkey check failed");
  }
}

/** `true` for the server's `403 {"error":"fresh-auth-required"}`. */
export function isFreshAuthRequired(input: {
  readonly status: number | null;
  readonly errorCode: string | null;
}): boolean {
  return input.status === 403 && input.errorCode === CODE_FRESH_AUTH_REQUIRED;
}

/** The `error` code of a JSON refusal body, or `null`. */
export function errorCodeOfBody(body: string): string | null {
  try {
    const parsed = JSON.parse(body) as { error?: unknown };
    return typeof parsed.error === "string" ? parsed.error : null;
  } catch {
    return null;
  }
}

/** How the password sign-in in front of an action ended, when it failed. */
export type ProofLoginFailure =
  /** The password is wrong (or the name is nobody's: the same answer). */
  | "wrong-password"
  /** Not an answer about the password: a limit, an outage, the network.
   *  The caller rethrows the original error for the surface's usual lines. */
  | "other";

/**
 * Classify a failed password sign-in. A wrong password fails the exchange
 * on this side (the core refuses to finish it: an error that is neither an
 * HTTP status nor a network failure) or at the server (`401`; `404` from a
 * login state that is gone is read the same way, as `verifyPassword` does).
 */
export function proofLoginFailure(input: {
  /** The HTTP status when the server answered, else `null`. */
  readonly status: number | null;
  /** The request never got an answer. */
  readonly network: boolean;
}): ProofLoginFailure {
  if (input.network) return "other";
  if (input.status === null) return "wrong-password";
  return input.status === 401 || input.status === 404 ? "wrong-password" : "other";
}

/** What a failed proof (or a failed gated action) is, for the block's one
 *  line (the shared states table of 17.1). */
export type ProofFailure =
  | { readonly kind: "required" }
  | { readonly kind: "wrong-password" }
  | { readonly kind: "cancelled" }
  | { readonly kind: "other-account" }
  | { readonly kind: "passkey-failed" }
  | { readonly kind: "rate-limited"; readonly retryAfterSecs: number | null }
  | { readonly kind: "offline" }
  | { readonly kind: "other" };

/** The facts `proofFailureOf` reads off an error (the caller extracts them,
 *  so this module imports no error class of the transport). */
export interface ProofErrorFacts {
  readonly required: boolean;
  readonly wrongPassword: boolean;
  readonly cancelled: boolean;
  readonly otherAccount: boolean;
  readonly passkeyFailed: boolean;
  readonly status: number | null;
  readonly retryAfterSecs: number | null;
  readonly network: boolean;
}

export function proofFailureOf(facts: ProofErrorFacts): ProofFailure {
  if (facts.required) return { kind: "required" };
  if (facts.wrongPassword) return { kind: "wrong-password" };
  if (facts.cancelled) return { kind: "cancelled" };
  if (facts.otherAccount) return { kind: "other-account" };
  if (facts.network) return { kind: "offline" };
  if (facts.status === 429) {
    const secs = facts.retryAfterSecs;
    return {
      kind: "rate-limited",
      retryAfterSecs: secs !== null && Number.isFinite(secs) && secs > 0 ? secs : null,
    };
  }
  if (facts.passkeyFailed) return { kind: "passkey-failed" };
  return { kind: "other" };
}

/** The per-surface lines of the block. */
export interface ProofCopy {
  /** The helper under the field; names the action. `null` for a surface
   *  with no helper of its own (none today). */
  readonly helpKey: string | null;
  readonly wrongPasswordKey: string;
  /** The surface's line for a failure that is none of the shared rows. */
  readonly genericKey: string;
  /** The line after a dismissed passkey sheet, or `null` when the surface
   *  offers no passkey route. */
  readonly cancelledKey: string | null;
}

export const PROOF_COPY = {
  reset: {
    helpKey: "restore_identity.reset.password_help",
    wrongPasswordKey: "restore_identity.reset.error.wrong_password",
    cancelledKey: "proof.reset.cancelled",
    genericKey: "restore_identity.reset.error.generic",
  },
  deleteAccount: {
    helpKey: "proof.delete.help",
    wrongPasswordKey: "proof.delete.wrong_password",
    cancelledKey: null,
    genericKey: "settings.account.delete.error.generic",
  },
  addDeviceRequest: {
    helpKey: "proof.add_device.help",
    wrongPasswordKey: "proof.add_device.request.wrong_password",
    cancelledKey: "proof.add_device.request.cancelled",
    genericKey: "device_link.request.error.generic",
  },
  addDeviceScan: {
    helpKey: "proof.add_device.help",
    wrongPasswordKey: "proof.add_device.scan.wrong_password",
    cancelledKey: "proof.add_device.scan.cancelled",
    genericKey: "device_link.request.error.generic",
  },
  // The catalog has no surface-specific wrong-password line for these two;
  // the generic one is reused.
  addPasskey: {
    helpKey: "proof.add_passkey.help",
    wrongPasswordKey: "restore_identity.reset.error.wrong_password",
    cancelledKey: "proof.add_passkey.cancelled",
    genericKey: "settings.passkeys.error.generic",
  },
  changePassword: {
    helpKey: "proof.change_password.help",
    wrongPasswordKey: "restore_identity.reset.error.wrong_password",
    cancelledKey: "proof.change_password.cancelled",
    genericKey: "recovery.new_password.error.generic",
  },
} as const satisfies Record<string, ProofCopy>;

/** The catalog key (and whether it is an error line) for a failure the
 *  block words. `null`: no line (`required` shows the block with no error;
 *  a dismissed sheet on a surface without a cancelled line; a rate limit
 *  WITH a retry time is the countdown notice). */
export function proofFailureLine(
  failure: ProofFailure,
  copy: ProofCopy,
): { readonly key: string; readonly error: boolean } | null {
  switch (failure.kind) {
    case "wrong-password":
      return { key: copy.wrongPasswordKey, error: true };
    case "cancelled":
      // Nothing went wrong: no error styling.
      return copy.cancelledKey === null ? null : { key: copy.cancelledKey, error: false };
    case "other-account":
      return { key: "proof.passkey.other_account", error: true };
    case "passkey-failed":
      return { key: "proof.passkey.failed", error: true };
    case "offline":
      return { key: "common.error.offline", error: true };
    case "rate-limited":
      return failure.retryAfterSecs === null
        ? { key: "common.error.too_many_attempts", error: true }
        : null;
    case "other":
      return { key: copy.genericKey, error: true };
    case "required":
      return null;
  }
}

// ---------------------------------------------------------------------------
// Reset account keys (UX 17.2)
// ---------------------------------------------------------------------------

export interface ResetSurfaceInput {
  readonly proofLive: boolean;
  readonly passkeyUsable: boolean;
  readonly landing: SessionLanding;
  /** `I know my password` was chosen on the "cannot reset" panel. */
  readonly knowsPassword: boolean;
  /** `I do not know my password` was chosen under the block. */
  readonly noPassword: boolean;
}

export interface ResetSurfaceView {
  /** The "cannot reset" panel replaces the block, the word and the button. */
  readonly cannot: boolean;
  /** The link `I do not know my password` under the block. */
  readonly showNoPasswordLink: boolean;
}

export function resetSurfaceView(input: ResetSurfaceInput): ResetSurfaceView {
  if (input.proofLive || input.passkeyUsable) {
    return { cannot: false, showNoPasswordLink: false };
  }
  // No proof and no usable passkey: the password is the only way.
  if (input.noPassword) return { cannot: true, showNoPasswordLink: false };
  if (input.landing === "authenticator" && !input.knowsPassword) {
    return { cannot: true, showNoPasswordLink: false };
  }
  return { cannot: false, showNoPasswordLink: true };
}

/** Whether `Reset account keys` is enabled. */
export function resetCanSubmit(input: {
  readonly confirmed: boolean;
  readonly proofReady: boolean;
  readonly cannot: boolean;
  readonly blocked: boolean;
}): boolean {
  return input.confirmed && input.proofReady && !input.cannot && !input.blocked;
}

// ---------------------------------------------------------------------------
// Delete account (UX 17.3)
// ---------------------------------------------------------------------------

export interface DeleteSurfaceInput {
  /** The account has a passkey (ADR-0014: the gesture is always asked). */
  readonly hasPasskey: boolean;
  readonly proofLive: boolean;
  /** The block was already drawn in this dialog. */
  readonly revealed: boolean;
  readonly noPassword: boolean;
}

export type DeleteSurfaceView =
  /** No field; `You will be asked for your passkey.`; the button opens the
   *  sheet for this deletion. A live proof does not skip it. */
  | { readonly kind: "passkey" }
  /** Body and buttons, as before section 17. */
  | { readonly kind: "plain" }
  /** The block with `I do not know my password` under the field. */
  | { readonly kind: "block" }
  /** `proof.delete.cannot` and `I know my password`; no field, no delete
   *  button. */
  | { readonly kind: "cannot" };

export function deleteSurfaceView(input: DeleteSurfaceInput): DeleteSurfaceView {
  if (input.hasPasskey) return { kind: "passkey" };
  if (input.proofLive && !input.revealed) return { kind: "plain" };
  if (input.noPassword && !input.proofLive) return { kind: "cannot" };
  return { kind: "block" };
}

/** What `Delete account` runs before `DELETE /account`. */
export interface DeleteAccountSteps {
  /** Obtain a fresh-auth proof first (the block's route). */
  readonly ensureProof: boolean;
  /** Run the passkey gesture bound to this deletion (ADR-0014). */
  readonly actionGesture: boolean;
}

/**
 * The steps of a deletion per surface. An account with a passkey runs ONE
 * gesture: the ADR-0014 action gesture, which is itself the proof at the
 * server; no passkey sign-in precedes it, and the fresh-auth grant is sent
 * only when one happens to be live. An account without a passkey proves
 * the password (or reuses a live proof) and runs no gesture. `null`: the
 * surface has no delete button.
 */
export function deleteAccountSteps(kind: DeleteSurfaceView["kind"]): DeleteAccountSteps | null {
  switch (kind) {
    case "passkey":
      return { ensureProof: false, actionGesture: true };
    case "plain":
    case "block":
      return { ensureProof: true, actionGesture: false };
    case "cannot":
      return null;
  }
}

// ---------------------------------------------------------------------------
// Change password on a device that holds the keys (UX 10.3, 17.8)
// ---------------------------------------------------------------------------

/** How long a local check of the recovery phrase counts for the password
 *  change. The same ten minutes as the server's grant. */
export const PHRASE_CHECK_TTL_MS = 600_000;

/**
 * Whether the password may be replaced from this device right now. The
 * replace itself is authorised at the server by the keys this device holds,
 * so the bare session would be enough for it; this client-side gate asks
 * for one of: a live fresh-auth proof (the current password or a passkey),
 * or a local check of the recovery phrase made in the last ten minutes on
 * this page (for the person who forgot the password).
 */
export function passwordChangeAllowed(input: {
  readonly proofLive: boolean;
  /** When the phrase was last checked locally on this page, unix ms. */
  readonly phraseCheckedAtMs: number | null;
  readonly nowMs: number;
}): boolean {
  if (input.proofLive) return true;
  const at = input.phraseCheckedAtMs;
  return at !== null && at <= input.nowMs && input.nowMs - at < PHRASE_CHECK_TTL_MS;
}
