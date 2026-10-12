// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * "Email code" on Get back in (docs/ux/identity-recovery.md section 19.3),
 * as wiring: which request each step sends, and what the person is shown
 * for each answer. The step machine itself is `protect/email-flow.test.ts`;
 * this drives the real component over a scripted client, the real wasm core
 * (the username and address rules, the login handle) and a fake vault core.
 * No request leaves the process.
 */
import { afterEach, beforeAll, beforeEach, describe, expect, it, vi } from "vitest";

const auth = {
  state: vi.fn(() => ({ kind: "anon" as const })),
  recover: vi.fn(async (_phrase: string, _password: string, _username?: string) => undefined),
  landEmailPasswordRecovery: vi.fn(async () => undefined),
  landingDeviceIdFor: vi.fn(() => undefined),
};
vi.mock("../AuthContext", () => ({ useAuth: () => auth }));

import { createSignal } from "solid-js";

import { all, initWasm, installLocalStorage, one, present, renderUi, settle } from "../../test/ui";
import { AuthHttpError } from "../client";
import { setEmailVaultRuntimeForTests } from "../protect/email-vault";
import type { EmailVaultCore } from "../protect/email-vault-job";
import type { RecoveryEmailClient } from "../protect/recovery-email-client";
import { loadWasm } from "../../wasm";
import { EmailRecover } from "./EmailRecover";

const PHRASE = "legal winner thank year wave sausage worth useful legal winner thank yellow";

type Script = Partial<{
  [K in "recoverStart" | "recoverVerify" | "recoverTotp" | "recoverOpen" | "recoverPassword"]: (
    ...args: never[]
  ) => Promise<unknown>;
}>;

function scripted(script: Script): { client: RecoveryEmailClient; calls: string[] } {
  const calls: string[] = [];
  const route =
    (name: keyof Script, fallback: () => unknown) =>
    async (...args: unknown[]): Promise<unknown> => {
      calls.push(name);
      const own = script[name] as ((...a: unknown[]) => Promise<unknown>) | undefined;
      return own === undefined ? fallback() : own(...args);
    };
  const client = {
    recoverStart: route("recoverStart", () => ({ challengeId: "cid", resendAfterSecs: 45, expiresInSecs: 600 })),
    recoverVerify: route("recoverVerify", () => ({ ticket: "tkt", totpRequired: false })),
    recoverTotp: route("recoverTotp", () => undefined),
    recoverOpen: route("recoverOpen", () => ({ mode: "password" })),
    recoverPassword: route("recoverPassword", () => ({
      accountId: new Uint8Array(16).fill(1),
      session: {
        sessionToken: "v3.tok",
        sessionExpiresAtMs: 1,
        refreshToken: "r2.tok",
        refreshExpiresAtMs: 2,
        keyGeneration: 1n,
      },
    })),
  } as unknown as RecoveryEmailClient;
  return { client, calls };
}

function vaultCore(opens: boolean): EmailVaultCore {
  return {
    RecoveryMnemonic: { fromPhrase: (phrase) => ({ phrase, free: () => undefined }) },
    emailVaultSeal: () => new Uint8Array(44),
    emailVaultOpen: () => {
      if (!opens) throw new Error("not-opened: the vault did not open");
      return { phrase: PHRASE, free: () => undefined };
    },
  };
}

function type(el: HTMLInputElement, value: string): void {
  el.value = value;
  el.dispatchEvent(new InputEvent("input", { bubbles: true }));
}
async function submit(): Promise<void> {
  one<HTMLFormElement>("form").dispatchEvent(new SubmitEvent("submit", { bubbles: true, cancelable: true }));
  for (let i = 0; i < 6; i += 1) await settle();
}

interface Mounted {
  readonly calls: string[];
  readonly back: ReturnType<typeof vi.fn>;
  readonly done: ReturnType<typeof vi.fn>;
}

function mount(script: Script = {}): Mounted {
  const { client, calls } = scripted(script);
  const back = vi.fn();
  const done = vi.fn();
  const [username, setUsername] = createSignal("");
  renderUi(() => (
    <EmailRecover username={username} setUsername={setUsername} onBack={back} onDone={done} client={client} />
  ));
  return { calls, back, done };
}

/** Step 1, filled in and sent. */
async function toCodeStep(username: string, address: string): Promise<void> {
  type(one<HTMLInputElement>('input[autocomplete="username"]'), username);
  type(one<HTMLInputElement>('input[type="email"]'), address);
  for (let i = 0; i < 4; i += 1) await settle();
  await submit();
}
async function enterCode(code: string): Promise<void> {
  type(one<HTMLInputElement>('input[autocomplete="one-time-code"]'), code);
  await submit();
}
async function choosePassword(password: string): Promise<void> {
  const fields = all<HTMLInputElement>('input[autocomplete="new-password"]');
  expect(fields).toHaveLength(2);
  for (const f of fields) type(f, password);
  await submit();
}
const text = (): string => document.body.textContent ?? "";

beforeAll(async () => {
  await initWasm();
});
beforeEach(() => {
  installLocalStorage();
  for (const fn of Object.values(auth)) fn.mockClear();
  auth.state.mockImplementation(() => ({ kind: "anon" as const }));
  auth.recover.mockImplementation(async () => undefined);
  setEmailVaultRuntimeForTests({ workerFactory: null, core: async () => vaultCore(true) });
});
afterEach(() => {
  setEmailVaultRuntimeForTests({ workerFactory: null, core: () => loadWasm() });
});

describe("step 2 is the same whatever was typed", () => {
  it("draws the same screen for two different usernames and addresses", async () => {
    mount();
    await toCodeStep("alice", "alice@example.com");
    const first = one("form").innerHTML;
    expect(text()).toContain("Check your email");
    expect(text()).toContain("If that username and address belong together, a code is on its way.");
    // The address is not echoed back, and nothing says whether it matched.
    expect(text()).not.toContain("alice@example.com");
    document.body.replaceChildren();
    mount({ recoverStart: async () => ({ challengeId: "decoy", resendAfterSecs: 45, expiresInSecs: 600 }) });
    await toCodeStep("nobody_here", "x@y.zz");
    expect(one("form").innerHTML).toBe(first);
  });

  it("a wrong code and a code that was never sent get the same line", async () => {
    mount({
      recoverVerify: async () => {
        throw new AuthHttpError(401, "/recovery-email/recover/verify", '{"error":"code-refused"}');
      },
    });
    await toCodeStep("alice", "alice@example.com");
    await enterCode("000000");
    expect(one('[role="alert"]').textContent).toBe("That code is not right.");
    // Still on the code step, with the field cleared.
    expect(one<HTMLInputElement>('input[autocomplete="one-time-code"]').value).toBe("");
  });
});

describe("the authenticator step", () => {
  it("is asked only when the account has one, after the mailed code, and cannot be skipped", async () => {
    const m = mount({ recoverVerify: async () => ({ ticket: "tkt", totpRequired: true }) });
    await toCodeStep("alice", "alice@example.com");
    await enterCode("123456");
    expect(text()).toContain("Type the 6-digit code from your authenticator app.");
    expect(present('input[autocomplete="new-password"]')).toBe(false);
    expect(m.calls).toEqual(["recoverStart", "recoverVerify"]);
    await enterCode("654321");
    expect(m.calls).toEqual(["recoverStart", "recoverVerify", "recoverTotp"]);
    expect(all('input[autocomplete="new-password"]')).toHaveLength(2);
  });

  it("is absent for an account without one", async () => {
    const m = mount();
    await toCodeStep("alice", "alice@example.com");
    await enterCode("123456");
    expect(m.calls).toEqual(["recoverStart", "recoverVerify"]);
    expect(all('input[autocomplete="new-password"]')).toHaveLength(2);
    // Nothing was opened yet: which kind of account this is, is not known.
    expect(m.calls.includes("recoverOpen")).toBe(false);
  });
});

describe("after the new password", () => {
  it("password mode: registers the password under the ticket and lands the session", async () => {
    const m = mount();
    await toCodeStep("Alice", "alice@example.com");
    await enterCode("123456");
    await choosePassword("correct horse battery");
    expect(m.calls).toEqual(["recoverStart", "recoverVerify", "recoverOpen", "recoverPassword"]);
    expect(auth.landEmailPasswordRecovery).toHaveBeenCalledTimes(1);
    // The canonical username, and no phrase restore.
    expect(auth.landEmailPasswordRecovery.mock.calls[0]?.[0 as never]).toBe("alice");
    expect(auth.recover).not.toHaveBeenCalled();
  });

  it("vault mode: opens the vault and runs the existing phrase restore with the new password", async () => {
    const m = mount({
      recoverOpen: async () => ({ mode: "vault", vaultSalt: "AAAAAAAAAAAAAAAAAAAAAA==", sealedEntropy: "AAAA" }),
    });
    await toCodeStep("alice", "alice@example.com");
    await enterCode("123456");
    await choosePassword("correct horse battery");
    expect(auth.recover).toHaveBeenCalledWith(PHRASE, "correct horse battery", "alice");
    expect(m.calls.includes("recoverPassword")).toBe(false);
    expect(auth.landEmailPasswordRecovery).not.toHaveBeenCalled();
    expect(m.done).toHaveBeenCalledTimes(1);
  });

  it("a vault that does not open: the account was not brought back, nothing else is sent", async () => {
    setEmailVaultRuntimeForTests({ workerFactory: null, core: async () => vaultCore(false) });
    const m = mount({
      recoverOpen: async () => ({ mode: "vault", vaultSalt: "AAAAAAAAAAAAAAAAAAAAAA==", sealedEntropy: "AAAA" }),
    });
    await toCodeStep("alice", "alice@example.com");
    await enterCode("123456");
    await choosePassword("correct horse battery");
    expect(one('[role="alert"]').textContent).toBe(
      "We could not bring back your account. Try again, or choose another way.",
    );
    expect(auth.recover).not.toHaveBeenCalled();
    expect(m.calls.includes("recoverPassword")).toBe(false);
    expect(m.done).not.toHaveBeenCalled();
    // The one control leads back to Get back in.
    one<HTMLButtonElement>("button").click();
    expect(m.back).toHaveBeenCalledTimes(1);
  });

  it("a restore that did not finish is retried without asking the server for the vault again", async () => {
    auth.recover.mockImplementationOnce(async () => {
      throw new Error("offline");
    });
    const m = mount({
      recoverOpen: async () => ({ mode: "vault", vaultSalt: "AAAAAAAAAAAAAAAAAAAAAA==", sealedEntropy: "AAAA" }),
    });
    await toCodeStep("alice", "alice@example.com");
    await enterCode("123456");
    await choosePassword("correct horse battery");
    expect(m.done).not.toHaveBeenCalled();
    await submit();
    expect(m.calls.filter((c) => c === "recoverOpen")).toHaveLength(1);
    expect(auth.recover).toHaveBeenCalledTimes(2);
    expect(m.done).toHaveBeenCalledTimes(1);
  });
});

describe("mail that is not set up", () => {
  it("says so and leads back to the list", async () => {
    const m = mount({
      recoverStart: async () => {
        throw new AuthHttpError(503, "/recovery-email/recover/start", '{"error":"recovery-email-disabled"}');
      },
    });
    await toCodeStep("alice", "alice@example.com");
    expect(text()).toContain("Email codes are not available right now.");
    one<HTMLButtonElement>("button").click();
    expect(m.back).toHaveBeenCalledTimes(1);
  });
});
