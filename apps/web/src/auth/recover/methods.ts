// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * The ways to get back into an account, as the recovery screen lists them
 * (docs/ux/client-redesign.md section 5.6), and the subset a logged-in
 * device without its identity can use (`RestoreIdentityPrompt`). One
 * definition so the two screens name things the same way.
 */
import type { IconName } from "../../ui/icons";

export type RecoverMethodId =
  | "phrase"
  | "device"
  | "words"
  | "totp"
  | "contact"
  | "shares";

export interface RecoverMethod {
  readonly id: RecoverMethodId;
  readonly icon: IconName;
  readonly title: string;
  readonly sub: string;
}

/** The recovery screen's rows, in display order. */
export const RECOVER_METHODS: readonly RecoverMethod[] = [
  {
    id: "phrase",
    icon: "key",
    title: "Recovery phrase",
    sub: "Type your 24 words and choose a new password.",
  },
  {
    id: "device",
    icon: "devices",
    title: "Another device",
    sub: "Confirm from a device that is still logged in.",
  },
  {
    id: "words",
    icon: "lines",
    title: "Three words",
    sub: "Three words from your phrase, if that shortcut is on.",
  },
  {
    id: "totp",
    icon: "shield",
    title: "Authenticator",
    sub: "A code from your authenticator app.",
  },
  {
    id: "contact",
    icon: "mail",
    title: "Email or phone",
    sub: "A code sent to the email or phone you set up.",
  },
  {
    id: "shares",
    icon: "share",
    title: "Social recovery shares",
    sub: "The shares you gave to people you trust.",
  },
];

/** The rows a logged-in device uses to restore its identity: the three that
 *  carry the phrase. The words row drops out when the shortcut is off. */
export const RESTORE_IDENTITY_METHODS: readonly RecoverMethod[] = [
  {
    id: "words",
    icon: "lines",
    title: "Three words",
    sub: "Three words from your recovery phrase.",
  },
  {
    id: "phrase",
    icon: "key",
    title: "Recovery phrase",
    sub: "Type all 24 words.",
  },
  {
    id: "device",
    icon: "devices",
    title: "Another device",
    sub: "Confirm from a device that is still logged in.",
  },
];

export function recoverMethod(id: RecoverMethodId): RecoverMethod {
  const found =
    RECOVER_METHODS.find((m) => m.id === id) ?? RESTORE_IDENTITY_METHODS.find((m) => m.id === id);
  if (found === undefined) throw new Error(`unknown recovery method ${id}`);
  return found;
}
