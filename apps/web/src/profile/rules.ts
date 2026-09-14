// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * ADR-0048 — the UI decisions over the two name rules.
 *
 * The rules themselves live in the Rust core and reach this file through the
 * wasm surface: `validateUsername` (ADR-0048 §4 — ASCII `[a-z0-9_]`, 3 to 32
 * characters after normalisation) and `normalizeDisplayName` (trimmed, 1 to 64
 * characters, one line, no control or format characters bar ZWJ / ZWNJ and
 * well-formed flag tag sequences). Neither is restated here. What this module
 * owns is the part a screen needs and the core cannot know:
 *
 *   * which state a field is in while the user types (blank / valid / invalid,
 *     and "unchecked" while the core is still loading);
 *   * the copy for each refusal, keyed on the stable reason code the core puts
 *     on the thrown error (`usernameError` / `displayNameError`), never on the
 *     error's message;
 *   * whether a lookup may be sent at all.
 *
 * The validators are INJECTED so the decisions are testable against the real
 * wasm core without a DOM, and so a screen that has not loaded the core yet
 * cannot accidentally treat "not checked" as "valid".
 *
 * Fail-closed throughout: an error the core threw without a recognised code is
 * an invalid name with a generic reason, and an unchecked name blocks a lookup.
 */

/** The four stable `usernameError` codes the core throws. */
export type UsernameErrorCode = "empty" | "tooShort" | "tooLong" | "invalidCharacters";

/** State of a username field. */
export type UsernameCheck =
  /** Nothing typed (after trimming). No reason is shown while blank. */
  | { readonly kind: "blank" }
  /** The core has not loaded yet: nothing can be decided, and nothing may be
   *  sent. */
  | { readonly kind: "unchecked" }
  /** Passes the rule. `normalised` is the canonical form (`@Alice` -> `alice`),
   *  which is what a screen shows back. */
  | { readonly kind: "valid"; readonly normalised: string }
  /** Fails the rule; `reason` is the copy to show under the field. */
  | {
      readonly kind: "invalid";
      readonly code: UsernameErrorCode | "unknown";
      readonly reason: string;
    };

/** UX spec §4 — the reason copy, one string per code. */
export const USERNAME_REASON: Readonly<Record<UsernameErrorCode | "unknown", string>> = {
  empty: "Enter a username",
  tooShort: "At least 3 characters",
  tooLong: "At most 32 characters",
  invalidCharacters: "Only Latin letters, digits and _",
  unknown: "This username can't be used",
};

const USERNAME_CODES: ReadonlySet<string> = new Set([
  "empty",
  "tooShort",
  "tooLong",
  "invalidCharacters",
]);

/** Read the stable reason code off a thrown value, or `undefined`. */
function codeOf(err: unknown, property: string): string | undefined {
  if (typeof err !== "object" || err === null) return undefined;
  const value = (err as Record<string, unknown>)[property];
  return typeof value === "string" ? value : undefined;
}

/**
 * Decide the state of a username field.
 *
 * @param input     what the user typed
 * @param validate  the core's `validateUsername`, or `null` while the core is
 *                  loading
 */
export function checkUsername(
  input: string,
  validate: ((input: string) => string) | null,
): UsernameCheck {
  if (input.trim().length === 0) return { kind: "blank" };
  if (validate === null) return { kind: "unchecked" };
  try {
    return { kind: "valid", normalised: validate(input) };
  } catch (err) {
    const raw = codeOf(err, "usernameError");
    const code: UsernameErrorCode | "unknown" =
      raw !== undefined && USERNAME_CODES.has(raw) ? (raw as UsernameErrorCode) : "unknown";
    return { kind: "invalid", code, reason: USERNAME_REASON[code] };
  }
}

/** The reason to show under a username field, or `null` for none. A blank
 *  field shows nothing while the user has not typed. */
export function usernameReason(check: UsernameCheck): string | null {
  return check.kind === "invalid" ? check.reason : null;
}

/** Whether a lookup (or a registration) may be sent for this field. Only a
 *  name the core accepted may leave the device — never a blank, an invalid or
 *  an unchecked one. */
export function mayLookUp(check: UsernameCheck): check is { kind: "valid"; normalised: string } {
  return check.kind === "valid";
}

// ---------------------------------------------------------------------------
// Display name
// ---------------------------------------------------------------------------

/** The stable `displayNameError` codes the core throws. */
export type DisplayNameErrorCode =
  | "lineBreak"
  | "controlOrFormatCharacter"
  | "tooLong"
  | "tooManyBytes";

/** State of a "Your name" field. */
export type DisplayNameCheck =
  /** Empty after trimming — a valid choice meaning "no name". */
  | { readonly kind: "none" }
  | { readonly kind: "unchecked" }
  | { readonly kind: "valid"; readonly name: string }
  | {
      readonly kind: "invalid";
      readonly code: DisplayNameErrorCode | "unknown";
      readonly reason: string;
    };

/** UX spec §4 — display-name refusal copy. */
export const DISPLAY_NAME_REASON: Readonly<Record<DisplayNameErrorCode | "unknown", string>> = {
  lineBreak: "One line only",
  controlOrFormatCharacter: "Remove invisible or text-direction characters",
  tooLong: "At most 64 characters",
  tooManyBytes: "This name is too long",
  unknown: "This name can't be used",
};

const DISPLAY_NAME_CODES: ReadonlySet<string> = new Set([
  "lineBreak",
  "controlOrFormatCharacter",
  "tooLong",
  "tooManyBytes",
]);

/**
 * Decide the state of a display-name field.
 *
 * @param normalize the core's `normalizeDisplayName`, or `null` while loading
 */
export function checkDisplayName(
  input: string,
  normalize: ((input: string) => string | undefined) | null,
): DisplayNameCheck {
  if (normalize === null) {
    return input.trim().length === 0 ? { kind: "none" } : { kind: "unchecked" };
  }
  try {
    const name = normalize(input);
    return name === undefined ? { kind: "none" } : { kind: "valid", name };
  } catch (err) {
    const raw = codeOf(err, "displayNameError");
    const code: DisplayNameErrorCode | "unknown" =
      raw !== undefined && DISPLAY_NAME_CODES.has(raw) ? (raw as DisplayNameErrorCode) : "unknown";
    return { kind: "invalid", code, reason: DISPLAY_NAME_REASON[code] };
  }
}

/** Whether the field may be saved: "no name" and a valid name may, an invalid
 *  or unchecked one may not. */
export function maySaveDisplayName(check: DisplayNameCheck): boolean {
  return check.kind === "none" || check.kind === "valid";
}

/** The name a save writes: `null` for "no name". Only meaningful when
 *  `maySaveDisplayName` holds. */
export function savedDisplayName(check: DisplayNameCheck): string | null {
  return check.kind === "valid" ? check.name : null;
}
