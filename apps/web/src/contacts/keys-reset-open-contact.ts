// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * ADR-0065 phase 2 (UX 16.1 "what is blocked", first row) — the
 * `Open contact` action beside the refusal line `Verify {name}'s new
 * safety number first.`
 *
 * Starting a one-to-one with a contact whose reset keys this device has not
 * verified is refused before anything is sent ({@link KeysResetUnverified}).
 * The line alone tells the person what to do but not where; the action
 * takes them to the contact screen, where the banner and the safety-number
 * section are. This resolves WHICH contact screen from the failure itself
 * (the refusal carries the account id; the contact route is keyed on the
 * row's signature key), so a surface needs nothing but the error it caught.
 *
 * Pure: no Solid, no wasm, no storage. The caller passes the accepted rows.
 */
import { base64Decode, base64UrlEncodeNoPad } from "../auth/account-id";
import { paths } from "../routes/paths";
import { KeysResetUnverified } from "./client";
import { accountIdB64OfHex } from "./local-store";

/** The account id (base64) a keys-reset refusal names, searching the cause
 *  chain the way `contactsErrorText` does; `null` for any other failure. */
export function keysResetRefusalAccount(err: unknown): string | null {
  let current: unknown = err;
  for (let depth = 0; depth < 8 && current !== null && current !== undefined; depth += 1) {
    if (current instanceof KeysResetUnverified) return current.accountIdB64;
    if (typeof current !== "object") return null;
    const wrapped = current as { cause?: unknown; variant?: { cause?: unknown } };
    current = wrapped.variant?.cause ?? wrapped.cause;
  }
  return null;
}

/**
 * The contact screen to open for a keys-reset refusal, or `null` when the
 * failure is something else or the person is not a contact on this device
 * (then there is no contact screen to open, and the line stands alone).
 */
export function keysResetOpenContactHref(
  err: unknown,
  accepted: readonly { readonly accountIdHex?: string | undefined; readonly signaturePubkeyB64: string }[],
): string | null {
  const accountIdB64 = keysResetRefusalAccount(err);
  if (accountIdB64 === null) return null;
  const row = accepted.find((c) => accountIdB64OfHex(c.accountIdHex) === accountIdB64);
  if (row === undefined) return null;
  try {
    return paths.contact(base64UrlEncodeNoPad(base64Decode(row.signaturePubkeyB64)));
  } catch {
    return null;
  }
}

/**
 * Whether the action still belongs to the line on screen. A surface keeps
 * `{ text, href }` from the failure it caught and shows the action only
 * while its error line is that very sentence, so a later, unrelated error
 * in the same slot never inherits the button.
 */
export function openContactFor(
  shown: string | null,
  kept: { readonly text: () => string; readonly href: string } | null,
): string | null {
  if (shown === null || kept === null) return null;
  return kept.text() === shown ? kept.href : null;
}
