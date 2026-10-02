// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * The ways to get back into an account, as the recovery screen lists them
 * (docs/ux/identity-recovery.md section 2, ADR-0064 decision 7), and the
 * subset a signed-in device without the account's keys can use
 * (`RestoreIdentityPrompt`, section 8). One definition so the two screens
 * name things the same way.
 *
 * Two kinds of route, and the order says which is which:
 *
 *   - routes that bring back the keys (and so the chats): another device
 *     first ("if you have one"), then a passkey, then the 12-word phrase;
 *   - routes that only sign the person in: three words from the phrase
 *     and an authenticator code.
 *
 * Nothing in the list depends on server state: before a session the server
 * must not say whether an account has other devices, a passkey or an
 * authenticator, so no row is hidden or badged from it.
 *
 * One row depends on a LOCAL fact. "Three words from your phrase"
 * (maintainer decision 2026-10-02, `three-words.ts`) is checked on the
 * device against the recovery phrase the device holds, so it is listed only
 * on a device that holds the phrase and the keys for its cached account,
 * and is absent everywhere else. The note under the list follows: it
 * mentions three words only where the row is.
 *
 * The restore prompt never offers a sign-in-only route: it cannot restore
 * keys.
 */
import { t } from "../../l10n";
import type { IconName } from "../../ui/icons";

export type RecoverMethodId =
  | "device"
  | "passkey"
  | "phrase"
  | "words"
  | "totp";

export interface RecoverMethod {
  readonly id: RecoverMethodId;
  readonly icon: IconName;
  readonly title: string;
  readonly sub: string;
}

/** A labelled run of rows on the recovery screen. */
export interface RecoverMethodGroup {
  readonly id: "keys" | "signin";
  /** The plain-text heading over the rows. */
  readonly label: string;
  readonly methods: readonly RecoverMethod[];
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

const DEVICE = method("device", "devices", "recovery.method.device.title", "recovery.method.device.sub");

const WORDS = method("words", "lines", "recovery.method.words.title", "recovery.method.words.sub");
const TOTP = method("totp", "shield", "recovery.method.totp.title", "recovery.method.totp.sub");

const KEYS_GROUP: RecoverMethodGroup = {
  id: "keys",
  get label(): string {
    return t("recovery.group.keys");
  },
  methods: [
    DEVICE,
    method("passkey", "lock", "recovery.method.passkey.title", "recovery.method.passkey.sub"),
    method("phrase", "key", "recovery.method.phrase.title", "recovery.method.phrase.sub"),
  ],
};

function signinGroup(threeWords: boolean): RecoverMethodGroup {
  return {
    id: "signin",
    get label(): string {
      return t("recovery.group.signin");
    },
    methods: threeWords ? [WORDS, TOTP] : [TOTP],
  };
}

/**
 * The recovery screen's rows for this device, grouped and in display order.
 * `threeWords` is the local fact of `offersThreeWords` (`three-words.ts`):
 * this device holds the recovery phrase and the keys of its cached account.
 */
export function recoverMethodGroups(device: {
  readonly threeWords: boolean;
}): readonly RecoverMethodGroup[] {
  return [KEYS_GROUP, signinGroup(device.threeWords)];
}

/** The catalog key of the note under the list: it names three words only
 *  where the row is offered, so it never promises a route that is absent. */
export function accessOnlyNoteKey(device: { readonly threeWords: boolean }): string {
  return device.threeWords ? "recovery.access_only_note_words" : "recovery.access_only_note";
}

/** Every row the recovery screen can show (the widest list), grouped. */
export const RECOVER_METHOD_GROUPS: readonly RecoverMethodGroup[] = recoverMethodGroups({
  threeWords: true,
});

/** Every row the recovery screen can show, as one list in display order. */
export const RECOVER_METHODS: readonly RecoverMethod[] = RECOVER_METHOD_GROUPS.flatMap(
  (g) => g.methods,
);

/** Whether a route only signs the person in (it never restores keys). */
export function isSignInOnly(id: RecoverMethodId): boolean {
  return id === "words" || id === "totp";
}

const RESTORE_PASSKEY = method(
  "passkey",
  "lock",
  "recovery.method.passkey.title",
  "restore_identity.method.passkey.sub",
);

/** Every row the restore prompt can show, in display order: another device,
 *  a passkey, the phrase. Never a route that only signs in. */
export const RESTORE_IDENTITY_METHODS: readonly RecoverMethod[] = [
  DEVICE,
  RESTORE_PASSKEY,
  method("phrase", "key", "recovery.method.phrase.title", "recovery.method.phrase.restore_sub"),
];

/**
 * The restore prompt's rows for this device and account.
 *
 * The passkey row is dropped in two cases, both things the signed-in client
 * knows: the browser has no passkey support, or the account has no passkey
 * that carries keys (`passkeysWithKeys` is the count from `GET
 * /vault/passkeys`; `null` while it is not known, which keeps the row).
 * This is how "hidden when the passkey just tried is the only one" is
 * decided on web, which cannot count the passkeys a device holds: a passkey
 * that did not carry keys is not in that count.
 */
export function restoreIdentityMethods(args: {
  readonly passkeysSupported: boolean;
  readonly passkeysWithKeys: number | null;
}): readonly RecoverMethod[] {
  const offerPasskey =
    args.passkeysSupported && (args.passkeysWithKeys === null || args.passkeysWithKeys > 0);
  return RESTORE_IDENTITY_METHODS.filter((m) => m.id !== "passkey" || offerPasskey);
}

export function recoverMethod(id: RecoverMethodId): RecoverMethod {
  const found = RECOVER_METHODS.find((m) => m.id === id);
  if (found === undefined) throw new Error(`unknown recovery method ${id}`);
  return found;
}

/** The restore prompt's row for `id` (its subtitles differ from the
 *  recovery screen's). */
export function restoreMethod(id: RecoverMethodId): RecoverMethod {
  const found = RESTORE_IDENTITY_METHODS.find((m) => m.id === id);
  if (found === undefined) throw new Error(`unknown restore method ${id}`);
  return found;
}
