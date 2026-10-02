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
  it("list another device first, then passkey and phrase, then the sign-in-only routes", () => {
    expect(RECOVER_METHODS.map((m) => m.id)).toEqual([
      "device",
      "passkey",
      "phrase",
      "words",
      "totp",
    ]);
  });

  it("are grouped: what brings back the chats, then what only signs in", () => {
    expect(RECOVER_METHOD_GROUPS.map((g) => [g.id, g.methods.map((m) => m.id)])).toEqual([
      ["keys", ["device", "passkey", "phrase"]],
      ["signin", ["words", "totp"]],
    ]);
    expect(RECOVER_METHOD_GROUPS.map((g) => g.label)).toEqual([
      "Brings back your chats",
      "Logs you in on this device",
    ]);
  });

  it("list three words only on a device that can check them itself", () => {
    const ids = (threeWords: boolean): string[] =>
      recoverMethodGroups({ threeWords }).flatMap((g) => g.methods.map((m) => m.id));
    expect(ids(true)).toEqual(["device", "passkey", "phrase", "words", "totp"]);
    // Absent, not greyed: the row is simply not there.
    expect(ids(false)).toEqual(["device", "passkey", "phrase", "totp"]);
    // The first group never depends on the device.
    expect(recoverMethodGroups({ threeWords: false })[0]).toBe(
      recoverMethodGroups({ threeWords: true })[0],
    );
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

  it("say 'if you have one' on the first row", () => {
    expect(RECOVER_METHODS[0]?.title).toBe("Another device");
    expect(RECOVER_METHODS[0]?.sub).toMatch(/^If you have one/);
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
