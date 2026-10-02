// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * The account-lifecycle confirmations — remove a passkey, forget this device,
 * delete the account.
 *
 * These are the three `window.confirm` sites the ADR-0045 §14 brief did not
 * name, and they are the most destructive in the app: two of them end the
 * user's access to their own account. None of their copy is owned by a spec
 * section, so the strings are carried over from what the app already said and
 * split into the title / body / action shape §6 establishes. Where a sentence
 * is new it is written in §6's voice and marked as such in the module.
 */

import { describe, expect, it } from "vitest";

import {
  deleteAccountConfirmation,
  forgetDeviceConfirmation,
  removePasskeyConfirmation,
  stopRestoringKeysConfirmation,
} from "./destructive-confirm";

describe("the remove-passkey confirmation", () => {
  it("names the passkey in the action button, not in the question", () => {
    // The old string stuffed the nickname into the question
    // (`Remove passkey "laptop"?`) because `window.confirm` renders one
    // string and a browser OK/Cancel pair — there was nowhere else to put
    // it. With a real action button the nickname belongs on the button, which
    // is what a screen reader announces at the point of no return, and the
    // question can take §6's generic `Remove this …?` shape.
    const c = removePasskeyConfirmation({ nickname: "laptop" });
    expect(c.title).toBe("Remove this passkey?");
    expect(c.confirmLabel).toBe("Remove laptop");
    expect(c.cancelLabel).toBe("Cancel");
    expect(c.tone).toBe("danger");
  });

  it("states the consequence and the irreversibility", () => {
    expect(removePasskeyConfirmation({ nickname: "laptop" }).body).toBe(
      "The device that owns the matching private key will no longer be able " +
        "to sign in with it. This cannot be undone.",
    );
  });

  it("falls back to the bare verb for an unnamed passkey", () => {
    for (const blank of [null, undefined, "", "  "]) {
      expect(removePasskeyConfirmation({ nickname: blank }).confirmLabel).toBe(
        "Remove",
      );
    }
  });
});

describe("the forget-device confirmation", () => {
  it("names the thing it acts on", () => {
    // A singleton — there is one device being forgotten and the user is on
    // it — so the noun names it unambiguously and an identifier would add
    // nothing. `Forget device` is the target; `Forget` alone is not.
    const c = forgetDeviceConfirmation();
    expect(c.title).toBe("Forget this device?");
    expect(c.confirmLabel).toBe("Forget device");
    expect(c.cancelLabel).toBe("Cancel");
    expect(c.tone).toBe("danger");
  });

  it("names the ways back, which decide whether to press the button", () => {
    // The one fact that decides whether the user should press the button,
    // pinned as a literal rather than merely referenced. ADR-0064: the phrase
    // is no longer the only way back; another device and a passkey that
    // restores keys come first.
    expect(forgetDeviceConfirmation().body).toBe(
      "You'll lose the cached account on this browser. The way back is another " +
        "device, a passkey that restores keys, or the 12-word recovery phrase " +
        "you wrote down at signup.",
    );
  });
});

describe("the delete-account confirmation", () => {
  it("names the thing it acts on", () => {
    const c = deleteAccountConfirmation();
    expect(c.title).toBe("Delete your account?");
    expect(c.confirmLabel).toBe("Delete account");
    expect(c.cancelLabel).toBe("Cancel");
    expect(c.tone).toBe("danger");
  });

  it("enumerates what is erased rather than gesturing at it", () => {
    // Same rule §6.2.4 states for the chat delete: the body is exact because
    // the user cannot check afterwards. This one spans both tiers — the
    // server-side identity AND the local device — and says so.
    const body = deleteAccountConfirmation().body ?? "";
    expect(body).toContain("recovery factors");
    expect(body).toContain("signs you out everywhere");
    expect(body).toContain("this device");
    expect(body).toContain("cannot be undone");
  });
});

describe("all three", () => {
  it("are destructive and offer a cancel", () => {
    // There is no code path on which any of these happens without a
    // confirmation and without an exit — the same "never absent" property
    // `remove-member-confirm.ts` states for its own flow.
    for (const c of [
      removePasskeyConfirmation({ nickname: "laptop" }),
      forgetDeviceConfirmation(),
      deleteAccountConfirmation(),
    ]) {
      expect(c.tone).toBe("danger");
      expect(c.cancelLabel).toBe("Cancel");
      expect(c.title.length).toBeGreaterThan(0);
      expect(c.body).not.toBeNull();
    }
  });

  it("uses no protocol vocabulary", () => {
    for (const c of [
      removePasskeyConfirmation({ nickname: "laptop" }),
      forgetDeviceConfirmation(),
      deleteAccountConfirmation(),
    ]) {
      const copy =
        `${c.title} ${c.body ?? ""} ${c.confirmLabel} ${c.cancelLabel}`.toLowerCase();
      for (const word of ["leaf", "epoch", "opaque", "pseudonym"]) {
        expect(copy).not.toContain(word);
      }
    }
  });
});

describe("the stop-restoring-keys confirmation", () => {
  it("states what stops and what stays, and is not styled as destructive", () => {
    const c = stopRestoringKeysConfirmation();
    expect(c.title).toBe("Stop restoring keys with this passkey?");
    expect(c.body).toBe(
      "This passkey will only sign you in. Keys already restored on other devices stay there.",
    );
    expect(c.confirmLabel).toBe("Stop restoring keys");
    expect(c.cancelLabel).toBe("Cancel");
    expect(c.tone).toBe("neutral");
  });
});
