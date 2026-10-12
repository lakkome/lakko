// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * The suite-wide network guard: NO unit test may reach the network.
 *
 * A unit test that builds a client with its real transport publishes to
 * whatever stack the build points at (it happened on another client: MLS
 * ciphertext rows landed on the test stack's Delivery Service from a
 * fixture). This file is a vitest `setupFiles` entry of BOTH projects
 * (`vitest.config.ts`), so it runs before every test file and replaces the
 * ways a browser client can open a connection with stubs that refuse:
 *
 *   `fetch`, `WebSocket`, `XMLHttpRequest`, `EventSource`,
 *   `navigator.sendBeacon`.
 *
 * A request that reaches a stub
 *
 *   1. THROWS (rejects, for `fetch`) there and then, with the URL, so the
 *      code under test sees a failure and not a response; and
 *   2. is RECORDED, and the test it happened in FAILS in the `afterEach`
 *      below even when the code under test caught the error (a request
 *      that lands after its test finished fails the next test of the file,
 *      or the file itself in the `afterAll`). Production
 *      code swallows transport errors in many places (a best-effort
 *      publish, a background refresh); a guard that only threw would be
 *      invisible exactly there.
 *
 * A test that needs a transport installs its own: `vi.stubGlobal("fetch",
 * mock)`, or a plain assignment to `globalThis.fetch`, or (the usual shape
 * in this codebase) a fake client object handed to the constructor. Either
 * replaces the stub for as long as the test keeps it; `vi.unstubAllGlobals()`
 * puts the guard back, not the real thing.
 *
 * Not covered: Node's own `http` / `net` modules. Nothing under `src/`
 * imports them, and blocking sockets at that level would also cut the
 * channel vitest's workers talk to the runner over.
 */
import { afterAll, afterEach, beforeEach, expect } from "vitest";

/** Requests refused since the current test started. */
const refused: string[] = [];

function refuse(what: string, target: unknown): Error {
  let url: string;
  try {
    url =
      typeof target === "string"
        ? target
        : target instanceof URL
          ? target.href
          : typeof target === "object" && target !== null && "url" in target
            ? String((target as { url: unknown }).url)
            : String(target);
  } catch {
    url = "<unprintable>";
  }
  const line = `${what} ${url}`;
  refused.push(line);
  return new Error(
    `[no-network] a unit test reached the network: ${line}. ` +
      "Stub the transport in the test (vi.stubGlobal, or a fake client).",
  );
}

/** A constructor that refuses: `new WebSocket(url)` and friends. */
function refusingClass(what: string): unknown {
  return class {
    constructor(target?: unknown) {
      throw refuse(what, target);
    }
  };
}

/** `XMLHttpRequest`: constructible (libraries feature-test it), refuses to
 *  open or send. */
class RefusingXhr {
  open(_method: string, url: unknown): void {
    throw refuse("XMLHttpRequest", url);
  }
  send(): void {
    throw refuse("XMLHttpRequest", "<send>");
  }
  setRequestHeader(): void {}
  abort(): void {}
  addEventListener(): void {}
  removeEventListener(): void {}
}

const guardedFetch = (input: unknown): Promise<never> => Promise.reject(refuse("fetch", input));

type Globals = Record<string, unknown>;

/** Put the guard on every transport global. Idempotent. */
export function installNetworkGuard(): void {
  const g = globalThis as unknown as Globals;
  g["fetch"] = guardedFetch;
  g["WebSocket"] = refusingClass("WebSocket");
  g["XMLHttpRequest"] = RefusingXhr;
  g["EventSource"] = refusingClass("EventSource");
  const nav = g["navigator"] as { sendBeacon?: unknown } | undefined;
  if (nav !== undefined && nav !== null) {
    try {
      Object.defineProperty(nav, "sendBeacon", {
        configurable: true,
        writable: true,
        value: (url: unknown): boolean => {
          throw refuse("sendBeacon", url);
        },
      });
    } catch {
      /* a frozen navigator has no beacon to guard */
    }
  }
}

/** Whether `fetch` is still the guard (a test did not install its own). */
export function fetchIsGuarded(): boolean {
  return (globalThis as unknown as Globals)["fetch"] === guardedFetch;
}

/** The requests refused in the current test, for the guard's own tests. */
export function takeRefusedRequests(): string[] {
  return refused.splice(0, refused.length);
}

installNetworkGuard();

beforeEach(() => {
  refused.length = 0;
});

afterEach(() => {
  const seen = refused.splice(0, refused.length);
  expect(seen, "a unit test reached the network (see src/test/no-network.ts)").toEqual([]);
});

// A request fired by a test and left running (a fire-and-forget publish)
// can land after that test's own check: whatever arrived after the last
// test of the file fails the file here.
afterAll(() => {
  const late = refused.splice(0, refused.length);
  expect(late, "a unit test reached the network after it finished (see src/test/no-network.ts)").toEqual([]);
});
