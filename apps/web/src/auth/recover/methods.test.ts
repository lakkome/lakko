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
  it("list another device, passkey and phrase, then the sign-in-only routes ending with the email code", () => {
    expect(RECOVER_METHODS.map((m) => m.id)).toEqual([
      "device",
      "passkey",
      "phrase",
      "words",
      "totp",
      "email",
    ]);
  });

  it("are grouped: what brings back the chats, then what only signs in, the email code included", () => {
    expect(RECOVER_METHOD_GROUPS.map((g) => [g.id, g.methods.map((m) => m.id)])).toEqual([
      ["keys", ["device", "passkey", "phrase"]],
      ["signin", ["words", "totp", "email"]],
    ]);
    // Every group has a heading, so no row sits headless above "Brings back
    // your chats" (the 2026-11-28 walk read the email code as bringing chats).
    expect(RECOVER_METHOD_GROUPS.map((g) => g.label)).toEqual([
      "Brings back your chats",
      "Logs you in on this device",
    ]);
    const keys = RECOVER_METHOD_GROUPS.find((g) => g.id === "keys");
    expect(keys?.methods.some((m) => m.id === "email")).toBe(false);
  });

  it("list three words only on a device that can check them itself", () => {
    const ids = (threeWords: boolean): string[] =>
      recoverMethodGroups({ threeWords }).flatMap((g) => g.methods.map((m) => m.id));
    expect(ids(true)).toEqual(["device", "passkey", "phrase", "words", "totp", "email"]);
    // Absent, not greyed: the row is simply not there.
    expect(ids(false)).toEqual(["device", "passkey", "phrase", "totp", "email"]);
    // The keys group never depends on the device.
    expect(recoverMethodGroups({ threeWords: false })[0]).toBe(
      recoverMethodGroups({ threeWords: true })[0],
    );
  });

  it("say in the note what the sign-in group does, never naming a way that may be absent", () => {
    expect(accessOnlyNoteKey({ threeWords: true })).toBe("recovery.access_only_note_words");
    expect(accessOnlyNoteKey({ threeWords: false })).toBe("recovery.access_only_note");
    // 2026-11-28: the note speaks of the whole sign-in group (the email code
    // is in it now), so it names no single way and never promises three words.
    for (const threeWords of [true, false]) {
      const note = t(accessOnlyNoteKey({ threeWords }));
      expect(note).toMatch(/^These log you in\./);
      expect(note).not.toMatch(/three words|authenticator/i);
    }
  });

  it("mark exactly three words, the authenticator and the email code as sign-in only", () => {
    expect(RECOVER_METHODS.filter((m) => isSignInOnly(m.id)).map((m) => m.id)).toEqual([
      "words",
      "totp",
      "email",
    ]);
    const signin = RECOVER_METHOD_GROUPS.find((g) => g.id === "signin");
    expect(signin?.methods.every((m) => isSignInOnly(m.id))).toBe(true);
  });

  it("put the email code last in the sign-in group, whatever the device", () => {
    for (const threeWords of [true, false]) {
      const signin = recoverMethodGroups({ threeWords }).find((g) => g.id === "signin");
      const last = signin?.methods[signin.methods.length - 1];
      expect(last?.id).toBe("email");
      expect(last?.title).toBe("Email code");
      expect(last?.sub).toBe("A code sent to your recovery email.");
    }
    expect(isSignInOnly("email")).toBe(true);
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
describe("the restore prompt after the email vault was removed (2026-11-26)", () => {
  const ids = (phraseSaved: boolean | null | undefined): string[] =>
    restoreIdentityMethods({
      passkeysSupported: true,
      passkeysWithKeys: 1,
      ...(phraseSaved === undefined ? {} : { phraseSaved }),
    }).map((m) => m.id);

  it("changes nothing while it is not known whether a phrase is saved", () => {
    expect(ids(null)).toEqual(["device", "passkey", "phrase"]);
    expect(ids(undefined)).toEqual(["device", "passkey", "phrase"]);
  });

  it("hides the 12-word phrase row only when the protection is known to list no phrase", () => {
    expect(ids(false)).toEqual(["device", "passkey"]);
    expect(ids(true)).toEqual(["device", "passkey", "phrase"]);
  });

  it("never offers Email code: a mailed code gives a new password and restores nothing", () => {
    for (const phraseSaved of [null, false, true]) expect(ids(phraseSaved)).not.toContain("email");
    // Whatever `GET /recovery-email` said is ignored here.
    expect(
      restoreIdentityMethods({
        passkeysSupported: true,
        passkeysWithKeys: 1,
        protection: { available: true, enabled: true, totp: false },
      }).map((m) => m.id),
    ).toEqual(["device", "passkey", "phrase"]);
    // The row still resolves for the recovery screen's own list.
    expect(restoreMethod("email").title).toBe("Email code");
  });
});
