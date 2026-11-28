// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * The ways to get back into an account, as the recovery screen lists them
 * (docs/ux/identity-recovery.md section 2, ADR-0072 decision 7), and the
 * subset a signed-in device without the account's keys can use
 * (`RestoreIdentityPrompt`, section 8). One definition so the two screens
 * name things the same way.
 *
 * Two kinds of route, and the order says which is which:
 *
 *   - routes that bring back the keys (and so the chats): another device
 *     first ("if you have one"), then a passkey, then the 12-word phrase;
 *   - routes that only sign the person in: three words from the phrase
 *     (where offered), an authenticator code, and an email code (ADR-0075,
 *     UX 19.3: a code sent to the recovery email gives a new password and
 *     signs the person in; it never brings the keys).
 *
 * Until 2026-11-28 the email row stood above both groups with no heading,
 * which read as if it belonged under "Brings back your chats"; it is now
 * the last row of the sign-in group.
 *
 * Nothing in the list depends on server state: before a session the server
 * must not say whether an account has other devices, a passkey, an
 * authenticator or a recovery email, so no row is hidden or badged from it.
 *
 * One row depends on a LOCAL fact. "Three words from your phrase"
 * (maintainer decision 2026-10-02, `three-words.ts`) is checked on the
 * device against the recovery phrase the device holds, so it is listed only
 * on a device that holds the phrase and the keys for its cached account,
 * and only once that account's phrase was saved (ADR-0075 section 11: an
 * account whose phrase was never shown has no words to type; the per-device
 * flag is fed from the account's published protection). It is absent
 * everywhere else. The note under the list follows: it
 * mentions three words only where the row is.
 *
 * The restore prompt never offers a sign-in-only route: it cannot restore
 * keys.
 */
import { t } from "../../l10n";
import type { IconName } from "../../ui/icons";

export type RecoverMethodId =
  | "email"
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

const DEVICE = method("device", "smartphone-waves", "recovery.method.device.title", "recovery.method.device.sub");

const WORDS = method("words", "words", "recovery.method.words.title", "recovery.method.words.sub");
const TOTP = method("totp", "lock-rotation", "recovery.method.totp.title", "recovery.method.totp.sub");

// ADR-0075 (UX 19.3): a code sent to the recovery email gives a new password
// and signs the person in; it never brings the keys (the email vault was
// removed on 2026-11-26). Always shown: no row depends on server state.
const EMAIL = method("email", "envelope", "recovery.email.row.title", "recovery.email.row.body");

const KEYS_GROUP: RecoverMethodGroup = {
  id: "keys",
  get label(): string {
    return t("recovery.group.keys");
  },
  methods: [
    DEVICE,
    method("passkey", "passkey", "recovery.method.passkey.title", "recovery.method.passkey.sub"),
    method("phrase", "key", "recovery.method.phrase.title", "recovery.method.phrase.sub"),
  ],
};

function signinGroup(threeWords: boolean): RecoverMethodGroup {
  return {
    id: "signin",
    get label(): string {
      return t("recovery.group.signin");
    },
    methods: threeWords ? [WORDS, TOTP, EMAIL] : [TOTP, EMAIL],
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
  return id === "words" || id === "totp" || id === "email";
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
 * UX `identity-recovery.md` 8a (ADR-0074, "Joining after a rotation"): the
 * restore prompt of a device that holds the account's current keys but not
 * the earlier keys its chats use. Another device alone: a passkey and the
 * phrase bring only the keys this device already has.
 */
export function earlierKeysRestoreMethods(): readonly RecoverMethod[] {
  return [EARLIER_KEYS_DEVICE];
}

const EARLIER_KEYS_DEVICE = method(
  "device",
  "smartphone-waves",
  "recovery.method.device.title",
  "restore_identity.earlier_keys.another_device.sub",
);

/** The email row. The restore prompt no longer lists it (a mailed code
 *  restores nothing); kept so `restoreMethod("email")` still resolves. */
const RESTORE_EMAIL = EMAIL;

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
 *
 * Amended 2026-11-26 (ADR-0075, second amendment):
 *
 *   - `Email code` is never a row here: a mailed code gives a new password
 *     and brings no keys, so it restores nothing.
 *   - The 12-word phrase row is dropped only when the account's published
 *     protection is known to list no saved phrase (`phraseSaved === false`):
 *     the owner of such an account was never shown words. While that is not
 *     known the row stays.
 *
 * `protection` (the answer of `GET /recovery-email`) is ignored since the
 * vault went; it is still accepted so callers need not change at once.
 */
export function restoreIdentityMethods(args: {
  readonly passkeysSupported: boolean;
  readonly passkeysWithKeys: number | null;
  readonly protection?: unknown;
  /** What the account's published protection says about a saved phrase;
   *  `null` or absent while it is not known. */
  readonly phraseSaved?: boolean | null;
}): readonly RecoverMethod[] {
  const offerPasskey =
    args.passkeysSupported && (args.passkeysWithKeys === null || args.passkeysWithKeys > 0);
  const offerPhrase = args.phraseSaved !== false;
  return RESTORE_IDENTITY_METHODS.filter(
    (m) => (m.id !== "passkey" || offerPasskey) && (m.id !== "phrase" || offerPhrase),
  );
}

export function recoverMethod(id: RecoverMethodId): RecoverMethod {
  const found = RECOVER_METHODS.find((m) => m.id === id);
  if (found === undefined) throw new Error(`unknown recovery method ${id}`);
  return found;
}

/** The restore prompt's row for `id` (its subtitles differ from the
 *  recovery screen's). */
export function restoreMethod(id: RecoverMethodId): RecoverMethod {
  const found = [RESTORE_EMAIL, ...RESTORE_IDENTITY_METHODS].find((m) => m.id === id);
  if (found === undefined) throw new Error(`unknown restore method ${id}`);
  return found;
}
