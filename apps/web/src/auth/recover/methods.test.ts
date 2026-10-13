// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import { describe, expect, it } from "vitest";

import {
  RECOVER_METHODS,
  RECOVER_METHOD_GROUPS,
  RESTORE_IDENTITY_METHODS,
  accessOnlyNoteKey,
  isSignInOnly,
  recoverMethodGroups,
  recoverMethod,
  restoreIdentityMethods,
  restoreMethod,
} from "./methods";
import { t } from "../../l10n";

const BANNED = /OPAQUE|PAKE|OPRF|MLS|KeyPackage|epoch|pseudonym|sprint|\b1\.\d+[a-z]?\b/i;

describe("the recovery method rows", () => {
  it("list the email code first, then another device, passkey and phrase, then the sign-in-only routes", () => {
    expect(RECOVER_METHODS.map((m) => m.id)).toEqual([
      "email",
      "device",
      "passkey",
      "phrase",
      "words",
      "totp",
    ]);
  });

  it("are grouped: the email code above both, what brings back the chats, then what only signs in", () => {
    expect(RECOVER_METHOD_GROUPS.map((g) => [g.id, g.methods.map((m) => m.id)])).toEqual([
      ["email", ["email"]],
      ["keys", ["device", "passkey", "phrase"]],
      ["signin", ["words", "totp"]],
    ]);
    // The email row is in neither group and has no heading: what it brings
    // depends on the account, and nothing may say which (UX 19.3).
    expect(RECOVER_METHOD_GROUPS.map((g) => g.label)).toEqual([
      "",
      "Brings back your chats",
      "Logs you in on this device",
    ]);
  });

  it("list three words only on a device that can check them itself", () => {
    const ids = (threeWords: boolean): string[] =>
      recoverMethodGroups({ threeWords }).flatMap((g) => g.methods.map((m) => m.id));
    expect(ids(true)).toEqual(["email", "device", "passkey", "phrase", "words", "totp"]);
    // Absent, not greyed: the row is simply not there.
    expect(ids(false)).toEqual(["email", "device", "passkey", "phrase", "totp"]);
    // The email row and the first group never depend on the device.
    for (const i of [0, 1]) {
      expect(recoverMethodGroups({ threeWords: false })[i]).toBe(
        recoverMethodGroups({ threeWords: true })[i],
      );
    }
  });

  it("promise three words in the note only where the row is", () => {
    expect(accessOnlyNoteKey({ threeWords: true })).toBe("recovery.access_only_note_words");
    expect(accessOnlyNoteKey({ threeWords: false })).toBe("recovery.access_only_note");
    expect(t(accessOnlyNoteKey({ threeWords: true }))).toMatch(/^Three words or an authenticator code/);
    expect(t(accessOnlyNoteKey({ threeWords: false }))).not.toMatch(/three words/i);
  });

  it("mark exactly three words and the authenticator as sign-in only", () => {
    expect(RECOVER_METHODS.filter((m) => isSignInOnly(m.id)).map((m) => m.id)).toEqual([
      "words",
      "totp",
    ]);
    const signin = RECOVER_METHOD_GROUPS.find((g) => g.id === "signin");
    expect(signin?.methods.every((m) => isSignInOnly(m.id))).toBe(true);
  });

  it("put the email code first, always, whatever the device or the account", () => {
    expect(RECOVER_METHODS[0]?.id).toBe("email");
    expect(RECOVER_METHODS[0]?.title).toBe("Email code");
    expect(RECOVER_METHODS[0]?.sub).toBe("A code sent to your recovery email.");
    // It is not one of the routes that only sign in.
    expect(isSignInOnly("email")).toBe(false);
  });

  it("say 'if you have one' on the first row of the keys group", () => {
    const keys = RECOVER_METHOD_GROUPS.find((g) => g.id === "keys");
    expect(keys?.methods[0]?.title).toBe("Another device");
    expect(keys?.methods[0]?.sub).toMatch(/^If you have one/);
  });

  it("carry no protocol words or sprint numbers", () => {
    for (const m of [...RECOVER_METHODS, ...RESTORE_IDENTITY_METHODS]) {
      expect(m.title).not.toMatch(BANNED);
      expect(m.sub).not.toMatch(BANNED);
    }
  });

  it("resolve by id", () => {
    expect(recoverMethod("totp").title).toBe("Authenticator code");
    expect(recoverMethod("words").title).toBe("Three words from your phrase");
    expect(() => restoreMethod("words")).toThrow();
    expect(restoreMethod("phrase").sub).toBe("Type your 12 words.");
    expect(() => restoreMethod("totp")).toThrow();
  });
});

describe("the restore prompt's rows", () => {
  it("are another device, passkey and phrase, in that order", () => {
    expect(RESTORE_IDENTITY_METHODS.map((m) => m.id)).toEqual(["device", "passkey", "phrase"]);
  });

  it("never offer a route that only signs in", () => {
    for (const passkeysSupported of [true, false]) {
      for (const passkeysWithKeys of [null, 0, 1, 3]) {
        const rows = restoreIdentityMethods({ passkeysSupported, passkeysWithKeys });
        expect(rows.some((m) => isSignInOnly(m.id))).toBe(false);
        expect(rows[0]?.id).toBe("device");
        expect(rows[rows.length - 1]?.id).toBe("phrase");
      }
    }
  });

  it("keep the passkey row while it is not known whether a passkey carries keys", () => {
    expect(
      restoreIdentityMethods({ passkeysSupported: true, passkeysWithKeys: null }).map((m) => m.id),
    ).toEqual(["device", "passkey", "phrase"]);
  });

  it("drop the passkey row when no passkey of the account carries keys", () => {
    expect(
      restoreIdentityMethods({ passkeysSupported: true, passkeysWithKeys: 0 }).map((m) => m.id),
    ).toEqual(["device", "phrase"]);
  });

  it("drop the passkey row where passkeys are not supported", () => {
    expect(
      restoreIdentityMethods({ passkeysSupported: false, passkeysWithKeys: 2 }).map((m) => m.id),
    ).toEqual(["device", "phrase"]);
  });
});

// ADR-0075 (alignment J): Restore your keys reads how the account is protected.
describe("the restore prompt and the recovery email", () => {
  const ids = (protection: { enabled: boolean; vault: boolean; phraseSaved: boolean } | null): string[] =>
    restoreIdentityMethods({ passkeysSupported: true, passkeysWithKeys: 1, protection }).map((m) => m.id);

  it("changes nothing while the account's protection is not known", () => {
    expect(ids(null)).toEqual(["device", "passkey", "phrase"]);
    expect(
      restoreIdentityMethods({ passkeysSupported: true, passkeysWithKeys: 1 }).map((m) => m.id),
    ).toEqual(["device", "passkey", "phrase"]);
  });

  it("hides the 12-word phrase row when no phrase is saved", () => {
    expect(ids({ enabled: false, vault: false, phraseSaved: false })).toEqual(["device", "passkey"]);
  });

  it("adds Email code, first, when the account has an email and its vault", () => {
    expect(ids({ enabled: true, vault: true, phraseSaved: false })).toEqual(["email", "device", "passkey"]);
    expect(restoreMethod("email").title).toBe("Email code");
  });

  it("does not offer the email without its vault: a mailed code would restore nothing", () => {
    expect(ids({ enabled: true, vault: false, phraseSaved: false })).toEqual(["device", "passkey"]);
    // A phrase-protected account with an email: the phrase, never the code.
    expect(ids({ enabled: true, vault: false, phraseSaved: true })).toEqual(["device", "passkey", "phrase"]);
  });
});
