// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * The ways to get back into an account, as the recovery screen lists them
 * (docs/ux/client-redesign.md section 5.6), and the subset a logged-in
 * device without its identity can use (`RestoreIdentityPrompt`). One
 * definition so the two screens name things the same way.
 */
import { t } from "../../l10n";
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

/** A row whose title and subtitle are catalog messages, rendered on each
 *  read so the list follows the language. */
function method(id: RecoverMethodId, icon: IconName, titleKey: string, subKey: string): RecoverMethod {
  return {
    id,
    icon,
    get title(): string {
      return t(titleKey);
    },
    get sub(): string {
      return t(subKey);
    },
  };
}

/** The recovery screen's rows, in display order. */
export const RECOVER_METHODS: readonly RecoverMethod[] = [
  method("phrase", "key", "recovery.method.phrase.title", "recovery.method.phrase.sub"),
  method("device", "devices", "recovery.method.device.title", "recovery.method.device.sub"),
  method("words", "lines", "recovery.method.words.title", "recovery.method.words.sub"),
  method("totp", "shield", "recovery.method.totp.title", "recovery.method.totp.sub"),
  method("contact", "mail", "recovery.method.contact.title", "recovery.method.contact.sub"),
  method("shares", "share", "recovery.method.shares.title", "recovery.method.shares.sub"),
];

/** The rows a logged-in device uses to restore its identity: the three that
 *  carry the phrase. The words row drops out when the shortcut is off. */
export const RESTORE_IDENTITY_METHODS: readonly RecoverMethod[] = [
  method("words", "lines", "recovery.method.words.title", "recovery.method.words.restore_sub"),
  method("phrase", "key", "recovery.method.phrase.title", "recovery.method.phrase.restore_sub"),
  method("device", "devices", "recovery.method.device.title", "recovery.method.device.sub"),
];

export function recoverMethod(id: RecoverMethodId): RecoverMethod {
  const found =
    RECOVER_METHODS.find((m) => m.id === id) ?? RESTORE_IDENTITY_METHODS.find((m) => m.id === id);
  if (found === undefined) throw new Error(`unknown recovery method ${id}`);
  return found;
}
