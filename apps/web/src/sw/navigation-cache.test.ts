// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * How `public/sw.js` caches navigations: under the one shell key, never under
 * the URL navigated to. A per-URL key kept `/chat/<group id>` and invite links
 * in Cache Storage after Forget device.
 *
 * `sw.js` is copied verbatim into `dist/` and may run as a classic script, so
 * it cannot be imported. The function is cut out of the source and evaluated
 * against a fake `caches` / `fetch`: what is tested is the code that ships.
 */
import { readFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";

import { describe, expect, it } from "vitest";

const SW = readFileSync(
  join(dirname(fileURLToPath(import.meta.url)), "../../public/sw.js"),
  "utf8",
);

function sliceFunction(name: string): string {
  const start = SW.indexOf(`async function ${name}(`);
  if (start < 0) throw new Error(`${name} not found in sw.js`);
  // The function ends at the first line that is exactly "}".
  const end = SW.indexOf("\n}\n", start);
  return SW.slice(start, end + 2);
}

function shellKey(): string {
  const m = SW.match(/const SHELL_CACHE_KEY = "([^"]+)";/);
  if (m === null || m[1] === undefined) throw new Error("SHELL_CACHE_KEY not found in sw.js");
  return m[1];
}

type Handler = (req: Request) => Promise<Response>;

function load(fetchImpl: (req: Request) => Promise<Response>): {
  handler: Handler;
  store: Map<string, Response>;
} {
  const store = new Map<string, Response>();
  const keyOf = (k: Request | string): string => (typeof k === "string" ? k : new URL(k.url).pathname);
  const cache = {
    put: async (k: Request | string, v: Response): Promise<void> => {
      store.set(keyOf(k), v);
    },
    match: async (k: Request | string): Promise<Response | undefined> => store.get(keyOf(k)),
  };
  const caches = { open: async (): Promise<typeof cache> => cache };
  const make = new Function(
    "caches",
    "fetch",
    "CACHE_NAME",
    "SHELL_CACHE_KEY",
    `${sliceFunction("networkFirstWithCacheFallback")}\nreturn networkFirstWithCacheFallback;`,
  ) as (c: unknown, f: unknown, n: string, k: string) => Handler;
  return { handler: make(caches, fetchImpl, "lakko-app-shell-test", shellKey()), store };
}

function html(body: string): Response {
  return new Response(body, { status: 200, headers: { "content-type": "text/html; charset=utf-8" } });
}

const settle = (): Promise<void> => new Promise((r) => setTimeout(r, 0));

describe("sw.js navigation caching", () => {
  it("caches the shell under one key, never under the URL navigated to", async () => {
    const { handler, store } = load(async () => html("<shell>"));
    await handler(new Request("https://lakko.test/chat/AAAA-group-id"));
    await handler(new Request("https://lakko.test/g/invite-slug"));
    await settle();
    expect([...store.keys()]).toEqual([shellKey()]);
    expect(shellKey()).toBe("/index.html");
  });

  it("offline, any routed path is served the cached shell", async () => {
    const online = load(async () => html("<shell v2>"));
    await online.handler(new Request("https://lakko.test/chat/x"));
    await settle();
    const offline = load(async () => {
      throw new TypeError("offline");
    });
    for (const [k, v] of online.store) offline.store.set(k, v);
    const res = await offline.handler(new Request("https://lakko.test/somewhere/else"));
    expect(await res.text()).toBe("<shell v2>");
  });

  it("a navigation to a non-HTML file does not replace the shell", async () => {
    const { handler, store } = load(async (req) =>
      new URL(req.url).pathname === "/icon-512.png"
        ? new Response("png", { status: 200, headers: { "content-type": "image/png" } })
        : html("<shell>"),
    );
    await handler(new Request("https://lakko.test/"));
    await handler(new Request("https://lakko.test/icon-512.png"));
    await settle();
    expect(await store.get(shellKey())?.text()).toBe("<shell>");
  });

  it("an error response is not cached, and offline with no shell the failure surfaces", async () => {
    const { handler, store } = load(async () => new Response("nope", { status: 502, headers: { "content-type": "text/html" } }));
    await handler(new Request("https://lakko.test/chat/x"));
    await settle();
    expect(store.size).toBe(0);
    const offline = load(async () => {
      throw new TypeError("offline");
    });
    await expect(offline.handler(new Request("https://lakko.test/chat/x"))).rejects.toThrow("offline");
  });

  it("the precache seeds the shell key", () => {
    const build = readFileSync(
      join(dirname(fileURLToPath(import.meta.url)), "../../scripts/build-sw.mjs"),
      "utf8",
    );
    expect(build).toContain(`"${shellKey()}"`);
  });
});
