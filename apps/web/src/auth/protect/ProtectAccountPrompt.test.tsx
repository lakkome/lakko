// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * "Protect your account", the step after sign-up
 * (docs/ux/identity-recovery.md section 19.1a), as wiring: which rows are
 * drawn, what each opens, and what "Not now" does. The two surfaces it opens
 * are stood in for; their own behaviour is tested where it lives.
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
// This browser can use passkeys (the row's absence where it cannot is the
// pure rule's test, `protect-step.test.ts`).
vi.mock("../webauthn-client", () => ({
  WebauthnClient: { isWebauthnAvailable: () => true },
}));
vi.mock("../use-webauthn-client", () => ({
  useWebauthnClient: () => ({ client: () => null, username: () => "alice" }),
}));
vi.mock("../SignupPhrasePrompt", () => ({
  PasskeyOffer: (props: { onAdded?: () => void; onDone: () => void }) => (
    <button
      data-testid="passkey-offer"
      onClick={() => {
        props.onAdded?.();
        props.onDone();
      }}
    />
  ),
}));
vi.mock("./SavePhrasePanel", () => ({
  SavePhrasePanel: () => <div data-testid="phrase-panel" />,
}));

import { all, installLocalStorage, one, present, renderUi, settle } from "../../test/ui";
import { pendingConfirm, resolvePendingConfirm, setConfirmHostMounted } from "../../ui/confirm-dialog";
import { adoptProtectionStatus, dropProtectionStatus } from "./protection-state";
import ProtectAccountPrompt from "./ProtectAccountPrompt";

const status = (over: Partial<Parameters<typeof adoptProtectionStatus>[1]> = {}) => ({
  available: true,
  enabled: false,
  vault: false,
  phraseSaved: false,
  totp: false,
  ...over,
});
const rows = (): string[] => all(".lk-method-row-title").map((el) => el.textContent ?? "");
const notNow = (): HTMLButtonElement =>
  all<HTMLButtonElement>("button").find((b) => b.textContent === "Not now") as HTMLButtonElement;

beforeEach(() => {
  installLocalStorage();
  auth.finishProtectStep.mockClear();
  dropProtectionStatus();
  setConfirmHostMounted(true);
});

const exitButton = (label: string): HTMLButtonElement | undefined =>
  all<HTMLButtonElement>("button").find((b) => b.textContent === label);

describe("the rows", () => {
  it("offers the email, marked Recommended, a passkey, then the phrase", () => {
    adoptProtectionStatus("acct", status());
    renderUi(() => <ProtectAccountPrompt />);
    expect(rows()).toEqual(["Add an email Recommended", "Create a passkey", "Save a recovery phrase"]);
    expect(one("h1").textContent).toBe("Protect your account");
  });

  it("has no email row when mail is not set up on this server", () => {
    adoptProtectionStatus("acct", status({ available: false }));
    renderUi(() => <ProtectAccountPrompt />);
    expect(rows()).toEqual(["Create a passkey", "Save a recovery phrase"]);
  });

  it("opens the surface of the row that was picked", () => {
    adoptProtectionStatus("acct", status());
    renderUi(() => <ProtectAccountPrompt />);
    all<HTMLButtonElement>(".lk-method-row")[2]?.click();
    expect(present('[data-testid="phrase-panel"]')).toBe(true);
    expect(one("h1").textContent).toBe("Save a recovery phrase");
  });

  it("a confirmed email returns to the step, which then offers Continue, not Not now", () => {
    adoptProtectionStatus("acct", status());
    renderUi(() => <ProtectAccountPrompt />);
    all<HTMLButtonElement>(".lk-method-row")[0]?.click();
    // The form confirmed the address (the server now reports the email).
    adoptProtectionStatus("acct", status({ enabled: true, vault: true }));
    one<HTMLButtonElement>('[data-testid="email-form"]').click();
    expect(auth.finishProtectStep).not.toHaveBeenCalled();
    expect(rows()[0]).toBe("Add an email Done");
    expect(exitButton("Not now")).toBeUndefined();
    const go = exitButton("Continue");
    expect(go?.classList.contains("primary")).toBe(true);
    go?.click();
    expect(auth.finishProtectStep).toHaveBeenCalledTimes(1);
  });

  it("a created passkey returns to the step with its row marked Done", () => {
    adoptProtectionStatus("acct", status());
    renderUi(() => <ProtectAccountPrompt />);
    all<HTMLButtonElement>(".lk-method-row")[1]?.click();
    one<HTMLButtonElement>('[data-testid="passkey-offer"]').click();
    expect(rows()).toEqual(["Add an email Recommended", "Create a passkey Done", "Save a recovery phrase"]);
    expect(exitButton("Continue")).toBeDefined();
    expect(auth.finishProtectStep).not.toHaveBeenCalled();
  });
});

describe("Not now", () => {
  it("asks in the app's own dialog, and Continue without ends the step", async () => {
    adoptProtectionStatus("acct", status());
    renderUi(() => <ProtectAccountPrompt />);
    notNow().click();
    await settle();
    expect(pendingConfirm()?.request.title).toBe("Continue without protection?");
    // Nothing ended yet.
    expect(auth.finishProtectStep).not.toHaveBeenCalled();
    resolvePendingConfirm(false);
    await settle();
    expect(auth.finishProtectStep).toHaveBeenCalledTimes(1);
  });

  it("the dialog's main action, Add an email, opens the email surface and keeps the step", async () => {
    adoptProtectionStatus("acct", status());
    renderUi(() => <ProtectAccountPrompt />);
    notNow().click();
    await settle();
    resolvePendingConfirm(true);
    await settle();
    expect(present('[data-testid="email-form"]')).toBe(true);
    expect(auth.finishProtectStep).not.toHaveBeenCalled();
  });
});

describe("an account that is already protected", () => {
  it("by a saved phrase: the step ends by itself", () => {
    adoptProtectionStatus("acct", status({ phraseSaved: true }));
    renderUi(() => <ProtectAccountPrompt />);
    expect(auth.finishProtectStep).toHaveBeenCalled();
  });

  it("by an email: the row is marked Done and Continue ends it", () => {
    adoptProtectionStatus("acct", status({ enabled: true, vault: true }));
    renderUi(() => <ProtectAccountPrompt />);
    expect(auth.finishProtectStep).not.toHaveBeenCalled();
    expect(rows()[0]).toBe("Add an email Done");
    expect(exitButton("Continue")).toBeDefined();
  });

  it("an email whose vault is gone is not done", () => {
    adoptProtectionStatus("acct", status({ enabled: true, vault: false }));
    renderUi(() => <ProtectAccountPrompt />);
    expect(rows()[0]).toBe("Add an email Recommended");
    expect(exitButton("Not now")).toBeDefined();
  });
});
