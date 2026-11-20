// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * docs/ux/identity-recovery.md section 8b - a session that landed with no
 * username learns it:
 *
 *   - from the backup first: a name auth confirms (`same`) is adopted with
 *     no prompt;
 *   - else the prompt asks; `same` adopts, `other` / `429` / no answer keep
 *     the field with their line, `none` closes, a name the rule refuses is
 *     its own copy and makes no request;
 *   - "I don't remember" closes after a note, and only the next start of the
 *     app asks again.
 */
import { describe, expect, it } from "vitest";

import { AuthHttpError, AuthNetworkError } from "./client";
import { LoginHandleUsernameError, type NameTarget } from "./login-handle";
import type { HandleState } from "./login-handle-sync";
import {
  backupUsernameCandidates,
  checkUsername,
  createUsernamePromptController,
  resolveFromBackup,
  shouldLookForUsername,
  usernamePromptErrorKey,
  type UsernameCheckDeps,
  type UsernamePromptPorts,
} from "./username-prompt";

const ACCOUNT = "QUFBQUFBQUFBQUFBQUFBQQ==";
const TOKEN = "v2.token";

/** The account holds `held` (`null`: no handle at all). Names are canonical
 *  when lower case; anything with a space is refused by the rule. */
function server(held: string | null, opts: { fail?: "429" | "network" | "500" } = {}) {
  const asked: string[] = [];
  const deps: UsernameCheckDeps = {
    nameTarget: async (typed): Promise<NameTarget> => {
      if (typed.includes(" ") || typed.trim().length === 0) throw new LoginHandleUsernameError("invalidCharacters");
      const username = typed.trim().replace(/^@/, "").toLowerCase();
      return { username, handle: { bytes: new Uint8Array(32), wire: `h:${username}` } };
    },
    check: async (token, wire): Promise<HandleState> => {
      expect(token).toBe(TOKEN);
      asked.push(wire);
      if (opts.fail === "429") throw new AuthHttpError(429, "/auth/login-handle/check", "", 60);
      if (opts.fail === "500") throw new AuthHttpError(500, "/auth/login-handle/check", "");
      if (opts.fail === "network") throw new AuthNetworkError("/auth/login-handle/check", new Error("offline"));
      if (held === null) return "none";
      return wire === `h:${held}` ? "same" : "other";
    },
  };
  return { deps, asked };
}

function controllerFor(
  held: string | null,
  backup: readonly string[],
  opts: { fail?: "429" | "network" | "500" } = {},
) {
  const s = server(held, opts);
  const adopted: string[] = [];
  const ports: UsernamePromptPorts = {
    ...s.deps,
    sessionToken: () => TOKEN,
    backupCandidates: async () => backup,
    adopt: async (username) => {
      adopted.push(username);
    },
    warn: () => undefined,
  };
  return { controller: createUsernamePromptController(ports), adopted, asked: s.asked, ports };
}

describe("checking one name", () => {
  it("same: the canonical name; other; none", async () => {
    expect(await checkUsername(server("anna").deps, TOKEN, "@Anna")).toEqual({ kind: "same", username: "anna" });
    expect(await checkUsername(server("anna").deps, TOKEN, "boris")).toEqual({ kind: "other" });
    expect(await checkUsername(server(null).deps, TOKEN, "boris")).toEqual({ kind: "none" });
  });

  it("429 is too many tries; a network failure or any other status is a network error", async () => {
    expect(await checkUsername(server("anna", { fail: "429" }).deps, TOKEN, "anna")).toEqual({ kind: "too-many" });
    expect(await checkUsername(server("anna", { fail: "network" }).deps, TOKEN, "anna")).toEqual({ kind: "network" });
    expect(await checkUsername(server("anna", { fail: "500" }).deps, TOKEN, "anna")).toEqual({ kind: "network" });
  });

  it("a name the rule refuses: its own copy, and no request", async () => {
    const s = server("anna");
    const result = await checkUsername(s.deps, TOKEN, "an na");
    expect(result.kind).toBe("invalid");
    expect(s.asked).toEqual([]);
  });

  it("each check's line", () => {
    expect(usernamePromptErrorKey("other")).toBe("auth.username_prompt.error.not_this");
    expect(usernamePromptErrorKey("too-many")).toBe("auth.username_prompt.error.too_many");
    expect(usernamePromptErrorKey("network")).toBe("auth.username_prompt.error.network");
    expect(usernamePromptErrorKey("same")).toBeNull();
    expect(usernamePromptErrorKey("none")).toBeNull();
  });
});

describe("the backup's names", () => {
  it("are tried in order, each once, empty ones dropped", () => {
    expect(backupUsernameCandidates(["anna", "", null, undefined, " anna ", "boris"])).toEqual(["anna", "boris"]);
  });

  it("a name the account holds is adopted; a stale one is passed over; none left asks", async () => {
    expect(await resolveFromBackup(server("anna").deps, TOKEN, ["old", "anna"])).toEqual({
      kind: "adopt",
      username: "anna",
    });
    expect(await resolveFromBackup(server("anna").deps, TOKEN, ["old"])).toEqual({ kind: "ask" });
    expect(await resolveFromBackup(server("anna").deps, TOKEN, [])).toEqual({ kind: "ask" });
    expect(await resolveFromBackup(server(null).deps, TOKEN, ["old"])).toEqual({ kind: "no-username" });
    expect(await resolveFromBackup(server("anna", { fail: "network" }).deps, TOKEN, ["anna"])).toEqual({
      kind: "ask",
    });
  });
});

describe("when the landing looks", () => {
  const base = { sessionLanded: true, username: "", guest: false, backupSettled: true };
  it("a landed session with no name, not a guest, once the backup step finished", () => {
    expect(shouldLookForUsername(base)).toBe(true);
    expect(shouldLookForUsername({ ...base, username: "anna" })).toBe(false);
    expect(shouldLookForUsername({ ...base, guest: true })).toBe(false);
    expect(shouldLookForUsername({ ...base, backupSettled: false })).toBe(false);
    expect(shouldLookForUsername({ ...base, sessionLanded: false })).toBe(false);
  });
});

describe("the prompt", () => {
  it("the backup gives the name: verified and adopted, no prompt", async () => {
    const c = controllerFor("anna", ["anna"]);
    await c.controller.start(ACCOUNT);
    expect(c.adopted).toEqual(["anna"]);
    expect(c.controller.view().phase).toBe("hidden");
  });

  it("no name from the backup: the prompt asks; same adopts and closes", async () => {
    const c = controllerFor("anna", []);
    await c.controller.start(ACCOUNT);
    expect(c.controller.view()).toEqual({ phase: "asking", busy: false, error: null, errorIsText: false });
    await c.controller.submit("Anna");
    expect(c.adopted).toEqual(["anna"]);
    expect(c.controller.view().phase).toBe("hidden");
  });

  it("a stale backup name is not adopted: the prompt asks", async () => {
    const c = controllerFor("anna", ["old"]);
    await c.controller.start(ACCOUNT);
    expect(c.adopted).toEqual([]);
    expect(c.controller.view().phase).toBe("asking");
  });

  it("other, 429 and no answer keep the field with their line", async () => {
    for (const [fail, key] of [
      [undefined, "auth.username_prompt.error.not_this"],
      ["429", "auth.username_prompt.error.too_many"],
      ["network", "auth.username_prompt.error.network"],
    ] as const) {
      const c = controllerFor("anna", [], fail === undefined ? {} : { fail });
      await c.controller.start(ACCOUNT);
      await c.controller.submit(fail === undefined ? "boris" : "anna");
      expect(c.controller.view()).toEqual({ phase: "asking", busy: false, error: key, errorIsText: false });
      expect(c.adopted).toEqual([]);
    }
  });

  it("none: the account has no username; the prompt closes and nothing is adopted", async () => {
    const c = controllerFor(null, []);
    await c.controller.start(ACCOUNT);
    expect(c.controller.view().phase).toBe("asking");
    await c.controller.submit("anna");
    expect(c.controller.view().phase).toBe("hidden");
    expect(c.adopted).toEqual([]);
  });

  it("a name the rule refuses: its own copy as text, no request", async () => {
    const c = controllerFor("anna", []);
    await c.controller.start(ACCOUNT);
    const before = c.asked.length;
    await c.controller.submit("an na");
    const v = c.controller.view();
    expect(v.phase === "asking" && v.errorIsText && v.error !== null).toBe(true);
    expect(c.asked.length).toBe(before);
  });

  it("I don't remember: the note, then closed; not asked again in this start, asked again at the next", async () => {
    const c = controllerFor("anna", []);
    await c.controller.start(ACCOUNT);
    c.controller.forgot();
    expect(c.controller.view().phase).toBe("forgot");
    c.controller.close();
    expect(c.controller.view().phase).toBe("hidden");
    await c.controller.start(ACCOUNT);
    expect(c.controller.view().phase).toBe("hidden");

    // The next start of the app: a new controller, the name still unknown.
    const next = createUsernamePromptController(c.ports);
    await next.start(ACCOUNT);
    expect(next.view().phase).toBe("asking");
  });

  it("a sign-out or Forget device lets the next landing look again", async () => {
    const c = controllerFor("anna", []);
    await c.controller.start(ACCOUNT);
    c.controller.close();
    c.controller.reset();
    await c.controller.start(ACCOUNT);
    expect(c.controller.view().phase).toBe("asking");
  });

  it("a second Continue while one check runs is ignored", async () => {
    const c = controllerFor("anna", []);
    await c.controller.start(ACCOUNT);
    const first = c.controller.submit("anna");
    const second = c.controller.submit("anna");
    await Promise.all([first, second]);
    expect(c.adopted).toEqual(["anna"]);
  });
});
