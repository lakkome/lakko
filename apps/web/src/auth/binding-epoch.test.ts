// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import { readFileSync } from "node:fs";
import { fileURLToPath } from "node:url";

import { describe, expect, it } from "vitest";

import { BindingEpoch, SessionBinding, StaleBindingError, isAbandoned } from "./binding-epoch";

describe("BindingEpoch", () => {
  it("answers true until the binding ends", () => {
    const binding = new BindingEpoch();
    const current = binding.capture();
    expect(current()).toBe(true);
    binding.advance();
    expect(current()).toBe(false);
  });

  it("does not revive a stale capture when the same account binds again", () => {
    const binding = new BindingEpoch();
    const first = binding.capture();
    binding.advance();
    const second = binding.capture();
    expect(first()).toBe(false);
    expect(second()).toBe(true);
  });

  it("keeps work for account A out of account B across an await", async () => {
    const binding = new BindingEpoch();
    const state: { owner: string | null; rows: string[] } = { owner: "A", rows: [] };
    let release: (rows: string[]) => void = () => undefined;
    const fetched = new Promise<string[]>((resolve) => {
      release = resolve;
    });
    async function load(): Promise<void> {
      const current = binding.capture();
      const rows = await fetched;
      if (!current()) return;
      state.rows = rows;
    }
    const pending = load();
    binding.advance();
    state.owner = "B";
    release(["draft of A"]);
    await pending;
    expect(state).toEqual({ owner: "B", rows: [] });
  });
});

describe("SessionBinding", () => {
  it("ends a flow's binding on a sign-out", () => {
    const binding = new SessionBinding();
    const current = binding.capture();
    binding.signedOut();
    expect(current()).toBe(false);
  });

  it("ends a flow's binding when another account lands", () => {
    const binding = new SessionBinding();
    const current = binding.capture();
    binding.landed();
    expect(current()).toBe(false);
  });

  it("keeps a committed registration bound across another landing, not across a sign-out", () => {
    const binding = new SessionBinding();
    const notSignedOut = binding.captureSignOut();
    binding.landed();
    expect(notSignedOut()).toBe(true);
    binding.signedOut();
    expect(notSignedOut()).toBe(false);
  });

  it("binds a flow that starts after a landing", () => {
    const binding = new SessionBinding();
    binding.landed();
    const current = binding.capture();
    expect(current()).toBe(true);
  });

  it("drops a sign-in whose response arrives after another session landed", async () => {
    // The shape of `login`: capture before the first await, ask after it,
    // write nothing once the answer is no.
    const binding = new SessionBinding();
    const device: { session: string | null; cachedAccount: string | null } = {
      session: null,
      cachedAccount: null,
    };
    let respond: (account: string) => void = () => undefined;
    const roundTrip = new Promise<string>((resolve) => {
      respond = resolve;
    });
    async function signIn(): Promise<void> {
      const current = binding.capture();
      const account = await roundTrip;
      if (!current()) throw new StaleBindingError();
      device.cachedAccount = account;
      device.session = account;
      binding.landed();
    }
    const pending = signIn();
    // A guest account lands while the round trip is out.
    device.session = "guest";
    device.cachedAccount = "guest";
    binding.landed();
    respond("A");
    const outcome = await pending.then(
      () => null,
      (err: unknown) => err,
    );
    expect(isAbandoned(outcome)).toBe(true);
    expect(device).toEqual({ session: "guest", cachedAccount: "guest" });
  });

  it("lands a sign-in nothing overtook", async () => {
    const binding = new SessionBinding();
    const current = binding.capture();
    await Promise.resolve();
    expect(current()).toBe(true);
  });
});

describe("isAbandoned", () => {
  it("is true for a stale binding only", () => {
    expect(isAbandoned(new StaleBindingError())).toBe(true);
    expect(isAbandoned(new Error("network"))).toBe(false);
    expect(isAbandoned(undefined)).toBe(false);
  });
});

// `AuthContext.tsx` needs a Solid runtime this suite does not have, so that
// its flows follow the rules above is pinned on its source.
describe("AuthContext wiring", () => {
  const src = readFileSync(fileURLToPath(new URL("./AuthContext.tsx", import.meta.url)), "utf8");

  /** The source of one `AuthApi` method, up to the next one. */
  function method(name: string): string {
    const start = src.indexOf(`\n    async ${name}(`);
    expect(start).toBeGreaterThan(-1);
    const rest = src.slice(start + 1);
    const next = rest.slice(1).search(/\n {4}(async )?[A-Za-z]+\([^)]*\) \{\n/);
    return next === -1 ? rest : rest.slice(0, next + 1);
  }

  it("every landing ends the binding earlier flows started under", () => {
    expect(src).toMatch(
      /const setState = \(next: AuthState\): void => \{\s*if \(next\.kind !== "anon" && next\.kind !== "loading"\) \{[^}]*binding\.landed\(\);/,
    );
    // Every state change goes through that wrapper.
    expect(src.match(/setStateSignal\(/g)?.length).toBe(1);
    // `logout`, `forgetDevice`, the sign-out that follows a key reset or a
    // device removal elsewhere (`wipeAfterSignOut`), and the end of a
    // suspended account that deleted itself (ADR-0099,
    // `finishSuspendedDeletion`).
    expect(src.match(/binding\.signedOut\(\);/g)?.length).toBe(4);
  });

  it.each([
    "login",
    "loginWithPasskey",
    "recover",
    "recoverWithTotp",
    "recoverWithThreeWords",
    "landEmailPasswordRecovery",
    "restoreKeysWithPasskey",
    "completeFromDeviceRestore",
    "completeFromPairing",
    "provideRecoveryPhrase",
    "mintFreshIdentity",
  ])("%s captures the binding before its first await", (name) => {
    const body = method(name);
    // `provideRecoveryPhrase` continues the mark of a route whose park this
    // page completes in-process (`withParkRouteWipes`).
    const captured = Math.max(
      body.indexOf("const flow = startFlow();"),
      body.indexOf("const flow = startFlow(parkRouteWipes ?? undefined);"),
    );
    expect(captured).toBeGreaterThan(-1);
    expect(captured).toBeLessThan(body.indexOf("await "));
  });

  it.each(["login", "loginWithPasskey", "recover", "completeFromDeviceRestore"])(
    "%s asks the binding before it writes the cached-account marker",
    (name) => {
      const body = method(name);
      for (const marker of body.matchAll(/savePersisted\(/g)) {
        const before = body.slice(0, marker.index);
        const lastAwait = before.lastIndexOf("await ");
        const lastCheck = Math.max(
          before.lastIndexOf("flow.require();"),
          before.lastIndexOf("requireCurrent();"),
        );
        expect(lastCheck).toBeGreaterThan(lastAwait);
      }
    },
  );

  it("a sign-up is not stopped once its registration has finished", () => {
    for (const name of ["signup", "signupGuest"]) {
      const body = method(name);
      expect(body).not.toContain("startFlow()");
      expect(body).not.toContain("flow.require()");
    }
    // The guest landing answers to a sign-out only.
    expect(method("signupGuest")).toMatch(/guest: true,\s*registered: true,/);
    expect(src).toMatch(
      /const bound = args\.registered === true \? notSignedOut : binding\.capture\(\);/,
    );
    // The sign-ups mint a new account, so no wipe mark stops them.
    expect(method("signup")).not.toContain("wipes: flow.wipes");
    expect(method("signupGuest")).not.toContain("wipes: flow.wipes");
  });

  it("a sign-in route opens its account's stores only if the account was not wiped since it began (ADR-0077)", () => {
    // `flow.openStores` refuses a wipe since the flow's mark (a wipe before
    // it does not count: the route lands the account); `flow.current()`
    // asks the same once the flow named its account.
    expect(src).toMatch(/const wipes = inherited \?\? takeWipeMark\(\);\s*let account: string \| null = null;/);
    // An in-process park completion continues the route's mark, set only
    // around its synchronous start.
    expect(src).toContain("const flow = startFlow(parkRouteWipes ?? undefined);");
    expect(src).toMatch(/parkRouteWipes = wipes;\s*try \{\s*return run\(\);\s*\} finally \{\s*parkRouteWipes = null;/);
    expect(src).toContain("!openAccountStoresSince(wipes, accountStoreIds(accountIdB64))");
    for (const name of ["login", "loginWithPasskey", "recover", "landEmailPasswordRecovery"]) {
      const body = method(name);
      expect(body).toContain("flow.openStores(accountIdB64);");
      expect(body).not.toContain("openStoresFor(");
    }
    const access = src.slice(src.indexOf("\n  async function landAfterAccessRecovery("));
    expect(access.slice(0, access.indexOf("installFlowSession("))).toContain("flow.openStores(accountIdB64);");
    expect(method("completeFromDeviceRestore")).toContain("flow.openStores(accountIdB64);");
    // The parked completions name the parked account.
    for (const name of ["provideRecoveryPhrase", "completeFromPairing", "restoreKeysWithPasskey", "mintFreshIdentity"]) {
      expect(method(name)).toContain("flow.forAccount(pending.accountIdB64);");
    }
    // Their landings carry the mark.
    for (const name of ["login", "provideRecoveryPhrase", "completeFromPairing", "completeFromDeviceRestore"]) {
      expect(method(name)).toContain("wipes: flow.wipes,");
    }
    // The only plain openings left: the landing itself, a landing without a
    // route (page-load restore, the sign-ups), the two post-reset paths,
    // and the suspended landing called without a route (ADR-0099: the
    // routes that have one pass it and open through `flow.openStores`).
    expect(src.match(/openStoresFor\(/g)?.length).toBe(8);
  });

  it("a key reset's landing writes the keys only if the account was not wiped since the route began", () => {
    const start = src.indexOf("\n  async function adoptKeyResetOutcome(");
    const body = src.slice(start, src.indexOf("\n  /**\n", start + 1));
    expect(body).toContain("putDeviceKey(ctx.accountIdB64, deviceKey, ctx.wipes)");
    expect(body).toContain("if (keyWritesAllowed()) await putAccountIdentitySeed(");
    expect(body).toContain('if (keyWritesAllowed()) await persistSessionPair(ctx.accountIdB64, ctx.reset.session, "reset");');
  });

  it("a key reset stores the new phrase before the publish and owes its screen", () => {
    // `mintFreshIdentity` (and a recall) hand the server's answer to
    // `adoptKeyResetOutcome`: the reset cannot be taken back, so the phrase
    // is stored and its screen owed before anything is published, and
    // another account's screen is never replaced by it.
    expect(method("mintFreshIdentity")).toContain("await adoptKeyResetOutcome({");
    const start = src.indexOf("\n  async function adoptKeyResetOutcome(");
    expect(start).toBeGreaterThan(-1);
    const body = src.slice(start, src.indexOf("\n  /**\n", start + 1));
    const cached = body.indexOf(
      'await storeDevicePhrase(ctx.accountIdB64, ctx.phrase, deviceKey, "fresh-identity", ctx.wipes);',
    );
    const owed = body.indexOf("owePhraseScreen(ctx.accountIdB64);");
    const decided = body.indexOf("if (overtaken()) {");
    const shown = body.indexOf("setPendingFreshPhrase({");
    const published = body.indexOf("await retryKeyResetPublish(");
    expect(body).toContain("freshPhraseDisposition({ mintedFor: ctx.accountIdB64, boundAccount: boundAccountId() })");
    expect(cached).toBeGreaterThan(-1);
    expect(cached).toBeLessThan(owed);
    expect(owed).toBeLessThan(decided);
    expect(decided).toBeLessThan(shown);
    expect(shown).toBeLessThan(published);
    // Confirming the phrase is what clears the marker.
    expect(method("confirmFreshPhraseSaved")).toContain(
      "clearOwedPhraseScreen(pending.accountIdB64);",
    );
  });
});
