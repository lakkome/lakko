// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * "Confirm it is you" on the reset screen (docs/ux/identity-recovery.md
 * 15.1b and 15.1c), as wiring: which rows are drawn, that the password is
 * checked before a factor, that the confirmed line and the sealed / unsealed
 * line appear, and what each failure shows. The rules themselves are tested
 * in `seal/reset-factor.test.ts` and `seal/seal.test.ts`.
 */
import { beforeEach, describe, expect, it, vi } from "vitest";

vi.mock("./webauthn-client", () => ({
  WebauthnClient: Object.assign(
    class {
      setSessionToken(): void {}
      async listCredentials(): Promise<{ credentials: unknown[] }> {
        return { credentials: [] };
      }
    },
    { isWebauthnAvailable: () => true },
  ),
  WebauthnHttpError: class extends Error {
    status = 0;
    body = "";
  },
  WebauthnNetworkError: class extends Error {},
}));

import { all, installLocalStorage, one, present, renderUi, settle } from "../test/ui";
import type { AuthApi } from "./AuthContext";
import { AuthHttpError } from "./client";
import { FreshAuthRequiredError, ProofCancelledError, ProofWrongPasswordError } from "./fresh-auth";
import type { AccountFactors } from "./protect/factors";
import { createResetFactors, ResetFactorSection, rowFailureOf } from "./ResetFactorSection";
import { ResetFactorRefusedError, type ResetFactorKind } from "./seal/reset-factor";
import type { ResetOutlook } from "./seal/seal-announcement";

interface World {
  factors: AccountFactors | null;
  passwordLive: boolean;
  confirmed: ResetFactorKind | null;
  outlook: ResetOutlook | null;
  calls: string[];
  totp: (code: string) => Promise<void>;
  passkey: () => Promise<void>;
  emailFinish: (code: string) => Promise<void>;
  prove: (password: string) => Promise<void>;
}

function world(over: Partial<World> = {}): { w: World; auth: AuthApi } {
  const w: World = {
    factors: { email: true, totp: true, passkeys: 1, mailAvailable: true },
    passwordLive: true,
    confirmed: null,
    outlook: { sealed: true, passkeyCouldSeal: false },
    calls: [],
    totp: async () => {
      w.confirmed = "totp";
    },
    passkey: async () => {
      w.confirmed = "passkey";
    },
    emailFinish: async () => {
      w.confirmed = "email";
    },
    prove: async () => {
      w.passwordLive = true;
    },
    ...over,
  };
  const auth = {
    pendingSessionToken: () => "tok",
    freshProofLive: () => w.passwordLive,
    freshProof: () => (w.passwordLive ? "k1.grant" : null),
    dropFreshProof: () => {
      w.passwordLive = false;
    },
    proveWithPassword: async (password: string) => {
      w.calls.push(`prove:${password}`);
      await w.prove(password);
    },
    proveWithPasskey: async () => undefined,
    resetFactor: {
      passwordProofLive: () => w.passwordLive,
      confirmed: () => w.confirmed,
      factors: async () => w.factors,
      confirmTotp: async (code: string) => {
        w.calls.push(`totp:${code}`);
        await w.totp(code);
      },
      emailStart: async (address: string) => {
        w.calls.push(`email-start:${address}`);
        return { challengeId: "c1", resendAfterSecs: 45, expiresInSecs: 600 };
      },
      emailFinish: async (challengeId: string, code: string) => {
        w.calls.push(`email-finish:${challengeId}:${code}`);
        await w.emailFinish(code);
      },
      confirmPasskey: async () => {
        w.calls.push("passkey");
        await w.passkey();
      },
      outlook: async () => w.outlook,
      clear: () => undefined,
    },
  } as unknown as AuthApi;
  return { w, auth };
}

function mount(auth: AuthApi): void {
  renderUi(() => {
    const factors = createResetFactors({
      auth,
      busy: () => false,
      usernameKnown: () => true,
      username: () => "alice",
      accountIdB64: () => "acct",
    });
    return (
      <>
        <ResetFactorSection factors={factors} />
        <output data-testid="section">{factors.section().kind}</output>
        <output data-testid="outlook">{factors.outlook() === null ? "unknown" : factors.outlook()?.sealed ? "sealed" : "unsealed"}</output>
      </>
    );
  });
}

const rows = (): string[] => all(".lk-method-row-title").map((el) => el.textContent ?? "");
const rowButton = (label: string): HTMLButtonElement | undefined =>
  all<HTMLButtonElement>("button.lk-method-row").find((b) => b.textContent?.includes(label));
const button = (label: string): HTMLButtonElement | undefined =>
  all<HTMLButtonElement>("button").find((b) => b.textContent === label);
const text = (): string => document.body.textContent ?? "";

function type(input: HTMLInputElement, value: string): void {
  input.value = value;
  input.dispatchEvent(new Event("input", { bubbles: true }));
}

beforeEach(() => {
  installLocalStorage();
});

describe("the rows", () => {
  it("are the factors the account has, authenticator, email, passkey", async () => {
    const { auth } = world();
    mount(auth);
    await settle();
    expect(rows()).toEqual(["Authenticator app", "Code by email", "Passkey"]);
    expect(text()).toContain("Confirm with one of these:");
    expect(present('input[autocomplete="current-password"]')).toBe(false);
  });

  it("a row the account does not have is absent, never disabled", async () => {
    const { auth } = world({ factors: { email: false, totp: true, passkeys: 0, mailAvailable: true } });
    mount(auth);
    await settle();
    expect(rows()).toEqual(["Authenticator app"]);
  });

  it("without a live password proof the password field is drawn above the rows", async () => {
    const { auth } = world({ passwordLive: false });
    mount(auth);
    await settle();
    expect(present('input[autocomplete="current-password"]')).toBe(true);
    expect(text()).toContain("Then confirm with one of these:");
  });

  it("an account with none of the three, and 'I cannot use any of these', leave the rows", async () => {
    const none = world({ factors: { email: false, totp: false, passkeys: 0, mailAvailable: true } });
    mount(none.auth);
    await settle();
    expect(one('[data-testid="section"]').textContent).toBe("no-factor");
    document.body.innerHTML = "";

    const { auth } = world();
    mount(auth);
    await settle();
    button("I cannot use any of these")?.click();
    await settle();
    expect(one('[data-testid="section"]').textContent).toBe("no-factor");
  });
});

describe("the authenticator code", () => {
  it("is checked on the sixth digit and collapses the section to the confirmed line with the sealed line", async () => {
    const { w, auth } = world();
    mount(auth);
    await settle();
    rowButton("Authenticator app")?.click();
    await settle();
    const input = one<HTMLInputElement>("input.lk-code-input");
    type(input, "12345");
    await settle();
    expect(w.calls).toEqual([]);
    type(input, "123456");
    await settle();
    expect(w.calls).toEqual(["totp:123456"]);
    expect(one('[data-testid="section"]').textContent).toBe("confirmed");
    expect(text()).toContain("Confirmed with your authenticator app.");
    expect(text()).toContain("will come back from your contacts");
    expect(one('[data-testid="outlook"]').textContent).toBe("sealed");
  });

  it("a wrong code says so, clears the digits and confirms nothing", async () => {
    const { auth } = world({
      totp: async () => {
        throw new ResetFactorRefusedError();
      },
    });
    mount(auth);
    await settle();
    rowButton("Authenticator app")?.click();
    await settle();
    const input = one<HTMLInputElement>("input.lk-code-input");
    type(input, "000000");
    await settle();
    expect(text()).toContain("That code did not work.");
    expect(one<HTMLInputElement>("input.lk-code-input").value).toBe("");
    expect(one('[data-testid="section"]').textContent).toBe("choose");
  });

  it("too many codes shows the countdown and disables the field; the other rows stay", async () => {
    const { auth } = world({
      totp: async () => {
        throw new AuthHttpError(429, "/account/keys/reset/factor/totp", '{"error":"too-many-attempts"}', 120);
      },
    });
    mount(auth);
    await settle();
    rowButton("Authenticator app")?.click();
    await settle();
    type(one<HTMLInputElement>("input.lk-code-input"), "000000");
    await settle();
    expect(text()).toContain("Too many attempts. Try again in");
    expect(one<HTMLInputElement>("input.lk-code-input").disabled).toBe(true);
    expect(rowButton("Code by email")?.disabled).toBe(false);
  });
});

describe("the password is checked first", () => {
  it("signs in with the typed password, then checks the factor", async () => {
    const { w, auth } = world({ passwordLive: false });
    mount(auth);
    await settle();
    rowButton("Authenticator app")?.click();
    await settle();
    // No password typed: the code field waits for it.
    expect(one<HTMLInputElement>("input.lk-code-input").disabled).toBe(true);
    type(one<HTMLInputElement>('input[autocomplete="current-password"]'), "hunter2");
    await settle();
    type(one<HTMLInputElement>("input.lk-code-input"), "123456");
    await settle();
    expect(w.calls).toEqual(["prove:hunter2", "totp:123456"]);
    expect(one('[data-testid="section"]').textContent).toBe("confirmed");
  });

  it("a wrong password stops there: the factor is not tried and no code is used up", async () => {
    const { w, auth } = world({
      passwordLive: false,
      prove: async () => {
        throw new ProofWrongPasswordError();
      },
    });
    mount(auth);
    await settle();
    rowButton("Authenticator app")?.click();
    await settle();
    type(one<HTMLInputElement>('input[autocomplete="current-password"]'), "wrong");
    await settle();
    type(one<HTMLInputElement>("input.lk-code-input"), "123456");
    await settle();
    expect(w.calls).toEqual(["prove:wrong"]);
    expect(text()).toContain("Wrong password. Check it and try again.");
    expect(one('[data-testid="section"]').textContent).toBe("choose");
  });
});

describe("the email code", () => {
  it("sends, then confirms on the sixth digit; the answer line does not say whether the address was right", async () => {
    const { w, auth } = world();
    mount(auth);
    await settle();
    rowButton("Code by email")?.click();
    await settle();
    type(one<HTMLInputElement>('input[type="email"]'), "a@example.com");
    await settle();
    button("Send code")?.click();
    await settle();
    expect(text()).toContain("If this is the recovery email of @alice, a code is on its way.");
    type(one<HTMLInputElement>("input.lk-code-input"), "654321");
    await settle();
    expect(w.calls).toEqual(["email-start:a@example.com", "email-finish:c1:654321"]);
    expect(text()).toContain("Confirmed with a code by email.");
  });

  it("five wrong codes return the row to Send code", async () => {
    const { auth } = world({
      emailFinish: async () => {
        throw new ResetFactorRefusedError();
      },
    });
    mount(auth);
    await settle();
    rowButton("Code by email")?.click();
    await settle();
    type(one<HTMLInputElement>('input[type="email"]'), "a@example.com");
    await settle();
    button("Send code")?.click();
    await settle();
    for (let i = 0; i < 4; i += 1) {
      type(one<HTMLInputElement>("input.lk-code-input"), "000000");
      await settle();
      expect(text()).toContain("That code is not right.");
    }
    type(one<HTMLInputElement>("input.lk-code-input"), "000000");
    await settle();
    expect(text()).toContain("Too many attempts. Send a new code.");
    expect(button("Send code")).toBeDefined();
  });
});

describe("a passkey, and what the reset keeps", () => {
  it("a cancelled sheet says nothing was reset, with no error styling", async () => {
    const { auth } = world({
      passkey: async () => {
        throw new ProofCancelledError();
      },
    });
    mount(auth);
    await settle();
    rowButton("Passkey")?.click();
    await settle();
    button("Use passkey")?.click();
    await settle();
    expect(text()).toContain("Cancelled. Nothing was reset.");
    expect(present(".form-error")).toBe(false);
  });

  it("an unsealed reset is said in the warning style, and offers the passkey that keeps the chats", async () => {
    const { w, auth } = world({ outlook: { sealed: false, passkeyCouldSeal: true } });
    mount(auth);
    await settle();
    rowButton("Authenticator app")?.click();
    await settle();
    type(one<HTMLInputElement>("input.lk-code-input"), "123456");
    await settle();
    expect(one(".lk-warning-note").textContent).toContain("This reset starts your account over.");
    expect(text()).toContain("Use a passkey that keeps your chats to bring them back.");
    // The passkey seals it: the line becomes the sealed one.
    w.outlook = { sealed: true, passkeyCouldSeal: false };
    button("Use passkey")?.click();
    await settle();
    expect(w.calls).toContain("passkey");
    expect(present(".lk-warning-note")).toBe(false);
    expect(text()).toContain("Confirmed with a passkey.");
    expect(text()).toContain("will come back from your contacts");
  });

  it("while the published protection cannot be read, nothing is claimed and Try again is offered", async () => {
    const { w, auth } = world({ outlook: null });
    mount(auth);
    await settle();
    rowButton("Authenticator app")?.click();
    await settle();
    type(one<HTMLInputElement>("input.lk-code-input"), "123456");
    await settle();
    expect(one('[data-testid="outlook"]').textContent).toBe("unknown");
    expect(text()).not.toContain("will come back");
    expect(text()).not.toContain("starts your account over");
    w.outlook = { sealed: false, passkeyCouldSeal: false };
    button("Try again")?.click();
    await settle();
    expect(one('[data-testid="outlook"]').textContent).toBe("unsealed");
  });
});

describe("what a thrown error is", () => {
  it("maps the auth layer's errors to the row's lines", () => {
    expect(rowFailureOf(new FreshAuthRequiredError()).kind).toBe("password-needed");
    expect(rowFailureOf(new ResetFactorRefusedError()).kind).toBe("refused");
    expect(rowFailureOf(new ProofCancelledError()).kind).toBe("cancelled");
    expect(rowFailureOf(new AuthHttpError(503, "/x", "")).kind).toBe("send-failed");
    expect(rowFailureOf(new Error("x")).kind).toBe("generic");
  });
});
