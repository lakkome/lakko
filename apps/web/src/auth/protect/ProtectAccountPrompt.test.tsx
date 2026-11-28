// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * "Protect your account" (docs/ux/identity-recovery.md section 19.1a, as
 * amended 2026-11-26), as wiring: which rows are drawn, what each opens,
 * that `Continue` is the only way off the step and stays disabled until one
 * of the three is done, and where the two warnings appear. The surfaces the
 * rows open are stood in for; their own behaviour is tested where it lives.
 */
import { beforeEach, describe, expect, it, vi } from "vitest";

const auth = {
  state: vi.fn(() => ({ kind: "session" as const, account: { accountId: "acct", username: "alice" } })),
  identitySecrets: vi.fn(() => null),
  finishProtectStep: vi.fn(),
};
vi.mock("../AuthContext", () => ({ useAuth: () => auth }));
vi.mock("../../ui/AuthLayout", () => ({
  AuthLayout: (props: { children: unknown }) => props.children,
}));
vi.mock("./RecoveryEmailForm", () => ({
  RecoveryEmailForm: (props: { onDone: () => void }) => (
    <button data-testid="email-form" onClick={() => props.onDone()} />
  ),
}));
vi.mock("../RecoveryFactors", () => ({
  TotpSetup: (props: { onDone: () => void }) => (
    <button data-testid="totp-setup" onClick={() => props.onDone()} />
  ),
}));
// This browser can use passkeys (the row's absence where it cannot is the
// pure rule's test, `protect-step.test.ts`).
vi.mock("../webauthn-client", () => ({
  WebauthnClient: { isWebauthnAvailable: () => true },
}));
vi.mock("../SignupPhrasePrompt", () => ({
  PasskeyOffer: (props: { onAdded?: () => void; onDone: () => void; onCancelled?: () => void }) => (
    <>
      <button
        data-testid="passkey-offer"
        onClick={() => {
          props.onAdded?.();
          props.onDone();
        }}
      />
      <button data-testid="passkey-cancel" onClick={() => props.onCancelled?.()} />
    </>
  ),
}));
vi.mock("./SavePhrasePanel", () => ({
  SavePhrasePanel: () => <div data-testid="phrase-panel" />,
}));

import { all, installLocalStorage, one, present, renderUi } from "../../test/ui";
import { dropAccountMemory } from "../account-memory";
import { adoptOwnSealFacts } from "../seal/own-seals";
import { markProtectStepOwed, passwordOnlyDialogSeen } from "./account-protection";
import { notePasskeyCount } from "./factors";
import { plainStore } from "./local";
import { notePasskeyAdded } from "./password-only-state";
import { adoptProtectionStatus, dropProtectionStatus } from "./protection-state";
import ProtectAccountPrompt from "./ProtectAccountPrompt";

const status = (over: Partial<Parameters<typeof adoptProtectionStatus>[1]> = {}) => ({
  available: true,
  enabled: false,
  totp: false,
  ...over,
});
/** What the server says the account has. */
function known(over: Partial<Parameters<typeof adoptProtectionStatus>[1]> = {}, passkeys = 0): void {
  adoptProtectionStatus("acct", status(over));
  notePasskeyCount("acct", passkeys);
}
const rows = (): string[] => all(".lk-method-row-title").map((el) => el.textContent ?? "");
const button = (label: string): HTMLButtonElement | undefined =>
  all<HTMLButtonElement>("button").find((b) => b.textContent === label);
const rowButtons = (): HTMLButtonElement[] => all<HTMLButtonElement>("button.lk-method-row");
const notes = (): string[] => all(".lk-warning-note").map((el) => el.textContent ?? "");

beforeEach(() => {
  installLocalStorage();
  auth.finishProtectStep.mockClear();
  dropProtectionStatus();
  dropAccountMemory(["acct"]);
  adoptOwnSealFacts("acct", null);
});

describe("the rows", () => {
  it("offers the email (Recommended), an authenticator app and a passkey, then the phrase under Optional", () => {
    known();
    renderUi(() => <ProtectAccountPrompt />);
    expect(rows()).toEqual([
      "Add an email Recommended",
      "Use an authenticator app",
      "Create a passkey",
      "Also save a recovery phrase",
    ]);
    expect(one(".lk-protect-caption").textContent).toBe("Optional");
  });

  it("has no email row when mail is not set up on this server", () => {
    known({ available: false });
    renderUi(() => <ProtectAccountPrompt />);
    expect(rows()).toEqual(["Use an authenticator app", "Create a passkey", "Also save a recovery phrase"]);
  });

  it("is headed 'Protect your account' after sign-up and by the landing title otherwise", () => {
    known();
    markProtectStepOwed(plainStore(), "acct");
    renderUi(() => <ProtectAccountPrompt />);
    expect(one("h1").textContent).toBe("Protect your account");
  });

  it("at a landing (no sign-up marker) the heading asks for a second way in", () => {
    known();
    renderUi(() => <ProtectAccountPrompt />);
    expect(one("h1").textContent).toBe("Add a second way to get back in");
  });

  it("opens the surface of the row that was picked", () => {
    known();
    renderUi(() => <ProtectAccountPrompt />);
    rowButtons()[1]?.click();
    expect(present('[data-testid="totp-setup"]')).toBe(true);
    expect(one("h1").textContent).toBe("Use an authenticator app");
  });

  it("the phrase row opens the phrase panel", () => {
    known();
    renderUi(() => <ProtectAccountPrompt />);
    rowButtons()[3]?.click();
    expect(present('[data-testid="phrase-panel"]')).toBe(true);
  });
});

describe("Continue", () => {
  it("is the only way off the step: there is no Not now", () => {
    known();
    renderUi(() => <ProtectAccountPrompt />);
    expect(button("Not now")).toBeUndefined();
    expect(button("Continue")).toBeDefined();
  });

  it("is disabled, with the line saying why, until one of the three is done", () => {
    known();
    renderUi(() => <ProtectAccountPrompt />);
    const go = button("Continue") as HTMLButtonElement;
    expect(go.disabled).toBe(true);
    const help = document.getElementById(go.getAttribute("aria-describedby") ?? "");
    expect(help?.textContent).toBe("Choose one of the first three to continue.");
    go.click();
    expect(auth.finishProtectStep).not.toHaveBeenCalled();
  });

  it("a saved phrase alone does not enable it", () => {
    known();
    adoptOwnSealFacts("acct", { password: true, phrase: true, passkeyTagsHex: [], seq: "2" });
    renderUi(() => <ProtectAccountPrompt />);
    expect(rows()[3]).toBe("Also save a recovery phrase Done");
    expect((button("Continue") as HTMLButtonElement).disabled).toBe(true);
  });

  it("stays disabled while what the account has is not known", () => {
    renderUi(() => <ProtectAccountPrompt />);
    expect((button("Continue") as HTMLButtonElement).disabled).toBe(true);
  });

  it("a confirmed email returns to the step, marks its row Done and enables Continue", () => {
    known();
    renderUi(() => <ProtectAccountPrompt />);
    rowButtons()[0]?.click();
    // The form confirmed the address (the server now reports the email).
    adoptProtectionStatus("acct", status({ enabled: true }));
    one<HTMLButtonElement>('[data-testid="email-form"]').click();
    expect(auth.finishProtectStep).not.toHaveBeenCalled();
    expect(rows()[0]).toBe("Add an email Done");
    const go = button("Continue") as HTMLButtonElement;
    expect(go.disabled).toBe(false);
    expect(go.hasAttribute("aria-describedby")).toBe(false);
    go.click();
    expect(auth.finishProtectStep).toHaveBeenCalledTimes(1);
  });

  it("a row marked Done cannot be opened again from the step", () => {
    known({ enabled: true });
    renderUi(() => <ProtectAccountPrompt />);
    // Email is done: three buttons are left (authenticator, passkey, phrase).
    expect(rowButtons().length).toBe(3);
    expect(rowButtons()[0]?.textContent).toContain("Use an authenticator app");
  });

  it("a created passkey returns to the step with its row marked Done", () => {
    known();
    renderUi(() => <ProtectAccountPrompt />);
    rowButtons()[2]?.click();
    notePasskeyCount("acct", 1);
    one<HTMLButtonElement>('[data-testid="passkey-offer"]').click();
    expect(rows()[2]).toBe("Create a passkey Done");
    expect((button("Continue") as HTMLButtonElement).disabled).toBe(false);
    expect(auth.finishProtectStep).not.toHaveBeenCalled();
  });

  it("a closed passkey sheet returns to the step with one plain line and nothing done", () => {
    known();
    renderUi(() => <ProtectAccountPrompt />);
    rowButtons()[2]?.click();
    one<HTMLButtonElement>('[data-testid="passkey-cancel"]').click();
    expect(all("p").some((p) => p.textContent === "No passkey was added.")).toBe(true);
    expect((button("Continue") as HTMLButtonElement).disabled).toBe(true);
  });

  it("pressing it records that this device was told what keeps the chats", () => {
    known({ enabled: true });
    renderUi(() => <ProtectAccountPrompt />);
    expect(passwordOnlyDialogSeen(plainStore(), "acct")).toBe(false);
    button("Continue")?.click();
    expect(passwordOnlyDialogSeen(plainStore(), "acct")).toBe(true);
  });
});

describe("the authenticator as the only way", () => {
  it("shows the warning and still lets the person continue", () => {
    known({ totp: true });
    renderUi(() => <ProtectAccountPrompt />);
    expect(notes()).toEqual([
      "If you lose your authenticator app, you will not be able to get this account back. Add an email or a passkey as well.",
    ]);
    expect((button("Continue") as HTMLButtonElement).disabled).toBe(false);
  });

  it("has no warning once an email is there too", () => {
    known({ totp: true, enabled: true });
    renderUi(() => <ProtectAccountPrompt />);
    expect(notes()).toEqual([]);
  });
});

describe("'Your chats depend on your password' on the step", () => {
  const passwordOnly = { password: true, phrase: false, passkeyTagsHex: [], seq: "1" } as const;

  it("is not shown before one of the three is done", () => {
    known();
    adoptOwnSealFacts("acct", passwordOnly);
    renderUi(() => <ProtectAccountPrompt />);
    expect(notes()).toEqual([]);
  });

  it("is shown once a factor is done and nothing but the password keeps the chats", () => {
    known({ enabled: true });
    adoptOwnSealFacts("acct", passwordOnly);
    renderUi(() => <ProtectAccountPrompt />);
    expect(notes().length).toBe(1);
    expect(notes()[0]).toContain("Your chats depend on your password");
    button("Save a recovery phrase")?.click();
    expect(present('[data-testid="phrase-panel"]')).toBe(true);
  });

  it("is not shown while what keeps the chats is not known", () => {
    known({ enabled: true });
    renderUi(() => <ProtectAccountPrompt />);
    expect(notes()).toEqual([]);
  });

  it("is not shown when a passkey keeps the chats", () => {
    known({}, 1);
    adoptOwnSealFacts("acct", { password: true, phrase: false, passkeyTagsHex: ["ab"], seq: "2" });
    renderUi(() => <ProtectAccountPrompt />);
    expect(notes()).toEqual([]);
  });

  it("says so when the passkey just added does not keep the chats", () => {
    known({}, 1);
    adoptOwnSealFacts("acct", passwordOnly);
    notePasskeyAdded("acct", false);
    renderUi(() => <ProtectAccountPrompt />);
    expect(notes()[0]).toContain("This passkey signs you in but does not keep your chats.");
  });
});
