// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * "Was it you?" and taking the account back: every decision, as pure
 * functions (ADR-0066 part 1 and "T3 as built";
 * `docs/ux/identity-recovery.md` section 18.4).
 *
 * The network, the wasm core and the stores are the caller's
 * (`AuthContext.tsx`, `client.ts`, `recall-record.ts`); nothing here reads a
 * clock or a store by itself.
 */
import { MIN_PASSWORD_LENGTH } from "./password-rules";

// ---------------------------------------------------------------------------
// Which wipe a device runs when it stops being a device of the account
// ---------------------------------------------------------------------------

/** Why this device is no longer a device of the account. */
export type SignOutCause =
  /** `401 key-generation-superseded`, or a landing that found a higher
   *  generation published: the keys were reset on another device. */
  | "keys-reset"
  /** `401 device-revoked`, or the device read its own removal from the
   *  DeviceGroup: a removal changes no keys, so there is nothing to take
   *  back. */
  | "device-removed"
  /** Another key at the same generation: the blocking screen, the wipe on
   *  the person's tap. */
  | "identity-mismatch";

export type SignOutWipe =
  /** Everything the account kept here EXCEPT the recall record and the
   *  device key that seals it; then "Was it you?". */
  | "staged"
  /** Everything, the recall record included; the signed-out screen. */
  | "full";

/**
 * The rule. Only a key reset can leave a recall record, and only on a device
 * that held the account key (and still holds the device key to seal the
 * record under): a device that never held the keys keeps nothing.
 */
export function signOutWipe(input: {
  readonly cause: SignOutCause;
  /** The device holds the account identity seed (in memory or stored). */
  readonly holdsAccountKey: boolean;
  /** The account's device root key is on this device. */
  readonly deviceKeyPresent: boolean;
}): SignOutWipe {
  if (input.cause !== "keys-reset") return "full";
  return input.holdsAccountKey && input.deviceKeyPresent ? "staged" : "full";
}

// ---------------------------------------------------------------------------
// The record's own end
// ---------------------------------------------------------------------------

/** The window by the device's clock: 30 days from when it learned of the
 *  reset (the server's own window is what `status` reports; this one ends
 *  the record on a device that never reaches the server again). */
export const RECALL_LOCAL_WINDOW_MS = 30 * 24 * 60 * 60 * 1000;

/** Whether the record has outlived the window. A clock that went backwards
 *  never expires a record. */
export function recallExpired(learnedAtMs: number, nowMs: number): boolean {
  return nowMs - learnedAtMs >= RECALL_LOCAL_WINDOW_MS;
}

// ---------------------------------------------------------------------------
// `POST /account/recall/status`
// ---------------------------------------------------------------------------

/** The server's refusal that is not a wrong password. */
export const CODE_RECALL_REFUSED = "recall-refused";
export const CODE_WRONG_PASSWORD = "wrong-password";
export const CODE_TOO_MANY_ATTEMPTS = "too-many-attempts";
/** `DELETE /account` while a window is open. */
export const CODE_RECALL_WINDOW_OPEN = "recall-window-open";

/** A `200` from the status route. */
export interface RecallStatus {
  readonly anchorSeq: number;
  /** Until when the account can be taken back, unix ms. */
  readonly expiresAtMs: number;
  /** When the keys were reset, unix ms. */
  readonly changedAtMs: number;
  /** Wrong old passwords still allowed on this anchor. */
  readonly attemptsRemaining: number;
  /** ADR-0067 section 8: `start` accepts `proof: "phrase"` for this anchor
   *  (the account had a saved phrase and no email vault when the anchor was
   *  captured). `false` when the server did not say. */
  readonly phraseArm: boolean;
  /** The account id (URL-safe base64, no padding), for a device that named
   *  the account by login handle; `null` when the server did not say. */
  readonly accountIdB64Url: string | null;
}

/** What the device knows about taking the account back. */
export type RecallCheck =
  /** The server confirmed it is possible. */
  | { readonly kind: "open"; readonly status: RecallStatus }
  /** It is not possible (any more): destroy the record, finish the wipe. */
  | { readonly kind: "closed" }
  /** The anchor's wrong-password budget is spent: as `closed`, with its own
   *  line when the form was open. */
  | { readonly kind: "exhausted" }
  /** No verdict: offline, a limit, a server error. The record stays and the
   *  check is repeated. */
  | { readonly kind: "unknown"; readonly offline: boolean };

function finiteInt(value: unknown): number | null {
  return typeof value === "number" && Number.isFinite(value) ? Math.trunc(value) : null;
}

/** The fields of a status body, or `null` when it is not one. */
export function parseRecallStatus(body: unknown): RecallStatus | null {
  if (typeof body !== "object" || body === null) return null;
  const b = body as Record<string, unknown>;
  const anchorSeq = finiteInt(b["anchor_seq"]);
  const expiresAtMs = finiteInt(b["expires_at_ms"]);
  const changedAtMs = finiteInt(b["changed_at_ms"]);
  const attemptsRemaining = finiteInt(b["attempts_remaining"]);
  if (anchorSeq === null || anchorSeq < 0 || expiresAtMs === null || changedAtMs === null) return null;
  if (attemptsRemaining === null) return null;
  const accountId = b["account_id_b64"];
  return {
    anchorSeq,
    expiresAtMs,
    changedAtMs,
    attemptsRemaining,
    phraseArm: b["phrase_arm"] === true,
    accountIdB64Url:
      typeof accountId === "string" && /^[A-Za-z0-9_-]{22}$/.test(accountId) ? accountId : null,
  };
}

/**
 * What a status answer means. `status` is `null` when no answer arrived.
 *
 * Only the server's own `401 recall-refused` closes: closing destroys the
 * one thing that can take the account back, so a body this client cannot
 * read, another `401`, a limit or a server error all leave the record alone.
 */
export function recallStatusVerdict(
  answer: {
    readonly status: number | null;
    readonly errorCode: string | null;
    readonly body: unknown;
  },
  nowMs: number,
): RecallCheck {
  if (answer.status === null) return { kind: "unknown", offline: true };
  if (answer.status === 200) {
    const status = parseRecallStatus(answer.body);
    if (status === null) return { kind: "unknown", offline: false };
    if (status.expiresAtMs <= nowMs) return { kind: "closed" };
    if (status.attemptsRemaining <= 0) return { kind: "exhausted" };
    return { kind: "open", status };
  }
  if (answer.status === 401 && answer.errorCode === CODE_RECALL_REFUSED) return { kind: "closed" };
  return { kind: "unknown", offline: false };
}

// ---------------------------------------------------------------------------
// `POST /account/recall/{start,finish}`
// ---------------------------------------------------------------------------

/** Why an attempt to take the account back did not land. */
export type RecallAttemptFailure =
  /** The OLD password is wrong (the OPAQUE login did not finish locally, or
   *  the server's `401 wrong-password`). */
  | { readonly kind: "wrong-password" }
  /** `401 recall-refused`: not possible any more. */
  | { readonly kind: "closed" }
  /** `429 too-many-attempts`. */
  | { readonly kind: "rate-limited"; readonly retryAfterSecs: number | null }
  /** No answer. */
  | { readonly kind: "offline" }
  /** `400`, a server error, anything unexpected. */
  | { readonly kind: "error" };

/** The mapping of every answer of `start` and `finish`. `status` is `null`
 *  when no answer arrived; `wrongPasswordLocally` when the OPAQUE login
 *  finish failed on this device (no request was sent for it). */
export function recallAttemptFailure(input: {
  readonly wrongPasswordLocally: boolean;
  readonly status: number | null;
  readonly errorCode: string | null;
  readonly retryAfterSecs: number | null;
}): RecallAttemptFailure {
  if (input.wrongPasswordLocally) return { kind: "wrong-password" };
  if (input.status === null) return { kind: "offline" };
  if (input.status === 401 && input.errorCode === CODE_WRONG_PASSWORD) return { kind: "wrong-password" };
  if (input.status === 401 && input.errorCode === CODE_RECALL_REFUSED) return { kind: "closed" };
  if (input.status === 429) return { kind: "rate-limited", retryAfterSecs: input.retryAfterSecs };
  return { kind: "error" };
}

/**
 * ADR-0067 section 8 (UX 19.7): whether the phrase may stand in for the old
 * password on this anchor. Only when the server said so: for an account
 * that was mail-protected, whoever came in by email holds the same words, so
 * they prove nothing.
 */
export function phraseRecallOffered(check: RecallCheck | null): boolean {
  return check !== null && check.kind === "open" && check.status.phraseArm;
}

/**
 * What Get back in > Recovery phrase does when the typed words were refused
 * as "not this account's": ask whether they are the account's phrase from
 * before a reset (UX 19.7). Only a refusal of the credentials leads there:
 * the server's `401`, or the OPAQUE exchange failing to finish on this
 * device (which is how a wrong phrase shows itself: `localFailure`). A
 * limit, a lost connection or a server error do not.
 */
export function asksPhraseRecall(refusal: {
  /** The HTTP status, or `null` when the failure was not an answer. */
  readonly status: number | null;
  readonly offline: boolean;
  /** The failure happened on this device, with no answer and no network
   *  error. */
  readonly localFailure: boolean;
}): boolean {
  if (refusal.offline) return false;
  if (refusal.status !== null) return refusal.status === 401;
  return refusal.localFailure;
}

/** Below this many tries left, the form says how many (UX 18.4). */
export const TRIES_LEFT_SHOWN_BELOW = 5;

// ---------------------------------------------------------------------------
// The form
// ---------------------------------------------------------------------------

/** Whether the main button is enabled: the three fields are filled, the new
 *  password passes the sign-up rule, and the two match. With `phrase`
 *  (UX 19.7, "Use my recovery phrase instead") the 12 words stand in for the
 *  old password. */
export function recallFormReady(fields: {
  readonly oldPassword: string;
  readonly newPassword: string;
  readonly repeatPassword: string;
  /** The typed phrase when the form is on the phrase arm; else absent. */
  readonly phrase?: string | null;
}): boolean {
  const proofFilled =
    fields.phrase === undefined || fields.phrase === null
      ? fields.oldPassword.length > 0
      : fields.phrase.trim().split(/\s+/).filter((w) => w.length > 0).length === 12;
  return (
    proofFilled &&
    fields.newPassword.length >= MIN_PASSWORD_LENGTH &&
    fields.newPassword === fields.repeatPassword
  );
}

// ---------------------------------------------------------------------------
// The screen
// ---------------------------------------------------------------------------

/** The line under the form. */
export type RecallFormError =
  | { readonly kind: "wrong-password"; readonly triesLeft: number | null }
  | { readonly kind: "rate-limited"; readonly retryAfterSecs: number | null }
  | { readonly kind: "offline" }
  | { readonly kind: "error" };

/** Why the screen ended without the account taken back, when the form was
 *  open (on the question nothing says why). */
export type RecallEndNotice = "exhausted" | "closed";

export type RecallScreen =
  /** "Was it you?" with the two buttons. */
  | { readonly step: "question"; readonly check: RecallCheck | "checking" }
  /** The confirmation of "Yes, it was me", inside the same screen. */
  | { readonly step: "confirm-yes"; readonly check: RecallCheck | "checking" }
  /** The form. `status` is what the last check confirmed. */
  | {
      readonly step: "form";
      readonly status: RecallStatus;
      readonly working: boolean;
      readonly error: RecallFormError | null;
    }
  /** The record is to be destroyed and the wipe finished (UX 15.2). */
  | { readonly step: "ending"; readonly notice: RecallEndNotice | null }
  /** The account was taken back; the phrase screen follows. */
  | { readonly step: "landed" };

export type RecallEvent =
  /** A status answer arrived. */
  | { readonly type: "checked"; readonly check: RecallCheck }
  /** The record outlived the window by the device's clock. */
  | { readonly type: "expired" }
  | { readonly type: "yes" }
  | { readonly type: "yes-back" }
  | { readonly type: "yes-confirmed" }
  | { readonly type: "no" }
  | { readonly type: "form-back" }
  | { readonly type: "submitted" }
  /** An attempt failed. For `wrong-password`, `check` is the status asked
   *  for right after it (`null` when that could not be asked). */
  | {
      readonly type: "attempt-failed";
      readonly failure: RecallAttemptFailure;
      readonly check: RecallCheck | null;
    }
  | { readonly type: "landed" };

export const RECALL_SCREEN_START: RecallScreen = { step: "question", check: "checking" };

/** Whether "No, take the account back" is enabled. */
export function recallNoEnabled(screen: RecallScreen): boolean {
  return screen.step === "question" && screen.check !== "checking" && screen.check.kind === "open";
}

function ended(screen: RecallScreen, notice: RecallEndNotice): RecallScreen {
  // On the question (and its confirmation) nothing says why.
  return { step: "ending", notice: screen.step === "form" ? notice : null };
}

/** The screen's state machine. Total: an event that does not apply to the
 *  current step leaves the screen as it is. */
export function recallReduce(screen: RecallScreen, event: RecallEvent): RecallScreen {
  if (screen.step === "ending" || screen.step === "landed") return screen;
  switch (event.type) {
    case "expired":
      return ended(screen, "closed");
    case "checked": {
      const check = event.check;
      if (check.kind === "closed") return ended(screen, "closed");
      if (check.kind === "exhausted") return ended(screen, "exhausted");
      if (screen.step === "form") {
        // A later confirmation refreshes what the form submits against; no
        // verdict leaves it alone.
        return check.kind === "open" ? { ...screen, status: check.status } : screen;
      }
      return { ...screen, check };
    }
    case "yes":
      return screen.step === "question" ? { step: "confirm-yes", check: screen.check } : screen;
    case "yes-back":
      return screen.step === "confirm-yes" ? { step: "question", check: screen.check } : screen;
    case "yes-confirmed":
      return screen.step === "confirm-yes" ? { step: "ending", notice: null } : screen;
    case "no":
      if (screen.step !== "question" || screen.check === "checking" || screen.check.kind !== "open") {
        return screen;
      }
      return { step: "form", status: screen.check.status, working: false, error: null };
    case "form-back":
      if (screen.step !== "form" || screen.working) return screen;
      return { step: "question", check: { kind: "open", status: screen.status } };
    case "submitted":
      return screen.step === "form" && !screen.working
        ? { ...screen, working: true, error: null }
        : screen;
    case "attempt-failed": {
      if (screen.step !== "form") return screen;
      const failure = event.failure;
      if (failure.kind === "closed") return ended(screen, "closed");
      if (failure.kind === "wrong-password") {
        const check = event.check;
        // The wrong password spent the last try, or the anchor is gone.
        if (check !== null && (check.kind === "exhausted" || check.kind === "closed")) {
          return ended(screen, "exhausted");
        }
        if (check !== null && check.kind === "open") {
          const left = check.status.attemptsRemaining;
          return {
            step: "form",
            status: check.status,
            working: false,
            error: { kind: "wrong-password", triesLeft: left < TRIES_LEFT_SHOWN_BELOW ? left : null },
          };
        }
        return { ...screen, working: false, error: { kind: "wrong-password", triesLeft: null } };
      }
      if (failure.kind === "rate-limited") {
        return {
          ...screen,
          working: false,
          error: { kind: "rate-limited", retryAfterSecs: failure.retryAfterSecs },
        };
      }
      return { ...screen, working: false, error: { kind: failure.kind } };
    }
    case "landed":
      return screen.step === "form" ? { step: "landed" } : screen;
  }
}

// ---------------------------------------------------------------------------
// Account deletion while a window is open
// ---------------------------------------------------------------------------

/**
 * `DELETE /account` answered `409 {"error":"recall-window-open",
 * "until_ms"}`: the date until which the account cannot be deleted, unix ms.
 * `null` for every other answer (and for a `409` without a usable date).
 */
export function deleteBlockedUntilMs(status: number, body: string): number | null {
  if (status !== 409) return null;
  try {
    const parsed = JSON.parse(body) as { error?: unknown; until_ms?: unknown };
    if (parsed.error !== CODE_RECALL_WINDOW_OPEN) return null;
    const until = finiteInt(parsed.until_ms);
    return until !== null && until > 0 ? until : null;
  } catch {
    return null;
  }
}
