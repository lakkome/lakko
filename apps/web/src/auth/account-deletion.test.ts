// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import { describe, expect, it } from "vitest";
import {
  DELETION_CLEANUP_RETRIES,
  retryableHttpFailure,
  runAccountDeletion,
  runCleanupWithRetry,
  type DeletionCleanup,
} from "./account-deletion";

class HttpError extends Error {
  constructor(readonly status: number) {
    super(`http ${status}`);
    this.name = "HttpError";
  }
}

function recorder(): {
  events: string[];
  sleeps: number[];
  logs: string[];
  deps: { sleep: (ms: number) => Promise<void>; log: (m: string, l: string) => void };
} {
  const events: string[] = [];
  const sleeps: number[] = [];
  const logs: string[] = [];
  return {
    events,
    sleeps,
    logs,
    deps: {
      sleep: (ms) => {
        sleeps.push(ms);
        return Promise.resolve();
      },
      log: (_m, label) => {
        logs.push(label);
      },
    },
  };
}

describe("runAccountDeletion", () => {
  it("runs the first cleanups to the end before any other cleanup starts (ADR-0098, walk 10)", async () => {
    const r = recorder();
    let releaseStatement: () => void = () => {};
    const statementGate = new Promise<void>((resolve) => {
      releaseStatement = resolve;
    });
    const run = runAccountDeletion({
      ...r.deps,
      deleteAtAuth: async () => void r.events.push("auth"),
      firstCleanups: async () => [
        {
          label: "conversations",
          run: async () => {
            r.events.push("statement start");
            await statementGate;
            r.events.push("statement end");
          },
        },
      ],
      cleanups: async () => [
        { label: "discovery", run: async () => void r.events.push("discovery") },
        { label: "push", run: async () => void r.events.push("push") },
      ],
      wipe: async () => void r.events.push("wipe"),
    });
    await new Promise((resolve) => setTimeout(resolve, 0));
    expect(r.events).toEqual(["auth", "statement start"]);
    releaseStatement();
    await run;
    expect(r.events.slice(0, 3)).toEqual(["auth", "statement start", "statement end"]);
    expect(new Set(r.events.slice(3, 5))).toEqual(new Set(["discovery", "push"]));
    expect(r.events.at(-1)).toBe("wipe");
  });

  it("deletes at auth, then runs the cleanups, then wipes", async () => {
    const r = recorder();
    await runAccountDeletion({
      ...r.deps,
      deleteAtAuth: async () => {
        r.events.push("auth");
      },
      cleanups: async () => {
        r.events.push("list");
        return [
          { label: "push", run: async () => void r.events.push("push") },
          { label: "backup", run: async () => void r.events.push("backup") },
          { label: "discovery", run: async () => void r.events.push("discovery") },
        ];
      },
      wipe: async () => {
        r.events.push("wipe");
      },
    });
    expect(r.events[0]).toBe("auth");
    expect(r.events[1]).toBe("list");
    expect(new Set(r.events.slice(2, 5))).toEqual(new Set(["push", "backup", "discovery"]));
    expect(r.events[5]).toBe("wipe");
    expect(r.events).toHaveLength(6);
  });

  it("buys the tokens before the auth delete", async () => {
    const r = recorder();
    await runAccountDeletion({
      ...r.deps,
      beforeAuth: async () => void r.events.push("tokens"),
      deleteAtAuth: async () => void r.events.push("auth"),
      cleanups: async () => [{ label: "push", run: async () => void r.events.push("push") }],
      wipe: async () => void r.events.push("wipe"),
    });
    expect(r.events).toEqual(["tokens", "auth", "push", "wipe"]);
  });

  it("goes on with the deletion when buying the tokens fails", async () => {
    const r = recorder();
    await runAccountDeletion({
      ...r.deps,
      beforeAuth: async () => {
        throw new TypeError("network");
      },
      deleteAtAuth: async () => void r.events.push("auth"),
      cleanups: async () => [],
      wipe: async () => void r.events.push("wipe"),
    });
    expect(r.events).toEqual(["auth", "wipe"]);
    expect(r.logs).toEqual(["tokens"]);
  });

  it("runs no cleanup and no wipe when auth refuses", async () => {
    const r = recorder();
    const refusal = new HttpError(409);
    await expect(
      runAccountDeletion({
        ...r.deps,
        deleteAtAuth: async () => {
          throw refusal;
        },
        cleanups: async () => {
          r.events.push("list");
          return [{ label: "push", run: async () => void r.events.push("push") }];
        },
        wipe: async () => {
          r.events.push("wipe");
        },
      }),
    ).rejects.toBe(refusal);
    expect(r.events).toEqual([]);
  });

  it("wipes although every cleanup fails", async () => {
    const r = recorder();
    let attempts = 0;
    await runAccountDeletion({
      ...r.deps,
      deleteAtAuth: async () => undefined,
      cleanups: async () => [
        {
          label: "backup",
          run: async () => {
            attempts += 1;
            throw new TypeError("network");
          },
        },
        {
          label: "push",
          run: async () => {
            throw new HttpError(400);
          },
          retryable: retryableHttpFailure,
        },
      ],
      wipe: async () => {
        r.events.push("wipe");
      },
    });
    expect(r.events).toEqual(["wipe"]);
    expect(attempts).toBe(1 + DELETION_CLEANUP_RETRIES);
    expect(r.logs.sort()).toEqual(["backup", "push"]);
  });

  it("wipes when the cleanup list cannot be built", async () => {
    const r = recorder();
    await runAccountDeletion({
      ...r.deps,
      deleteAtAuth: async () => undefined,
      cleanups: async () => {
        throw new Error("idb");
      },
      wipe: async () => {
        r.events.push("wipe");
      },
    });
    expect(r.events).toEqual(["wipe"]);
  });

  it("does not wait past the budget for a cleanup that hangs", async () => {
    const r = recorder();
    const started = Date.now();
    await runAccountDeletion({
      ...r.deps,
      budgetMs: 50,
      deleteAtAuth: async () => undefined,
      cleanups: async () => [{ label: "hang", run: () => new Promise<void>(() => undefined) }],
      wipe: async () => {
        r.events.push("wipe");
      },
    });
    expect(r.events).toEqual(["wipe"]);
    expect(Date.now() - started).toBeLessThan(2_000);
  });
});

describe("runCleanupWithRetry", () => {
  it("retries a transient failure and stops on success", async () => {
    const r = recorder();
    let attempts = 0;
    const cleanup: DeletionCleanup = {
      label: "backup",
      run: async () => {
        attempts += 1;
        if (attempts < 2) throw new HttpError(503);
      },
      retryable: retryableHttpFailure,
    };
    const ok = await runCleanupWithRetry(cleanup, Date.now() + 10_000, r.deps);
    expect(ok).toBe(true);
    expect(attempts).toBe(2);
    expect(r.sleeps).toHaveLength(1);
    expect(r.logs).toEqual([]);
  });

  it("does not retry a refusal a retry cannot change", async () => {
    const r = recorder();
    let attempts = 0;
    const ok = await runCleanupWithRetry(
      {
        label: "backup",
        run: async () => {
          attempts += 1;
          throw new HttpError(401);
        },
        retryable: retryableHttpFailure,
      },
      Date.now() + 10_000,
      r.deps,
    );
    expect(ok).toBe(false);
    expect(attempts).toBe(1);
    expect(r.sleeps).toEqual([]);
  });

  it("starts no retry past the deadline", async () => {
    const r = recorder();
    let attempts = 0;
    const ok = await runCleanupWithRetry(
      {
        label: "push",
        run: async () => {
          attempts += 1;
          throw new TypeError("network");
        },
      },
      Date.now(),
      r.deps,
    );
    expect(ok).toBe(false);
    expect(attempts).toBe(1);
  });
});

describe("retryableHttpFailure", () => {
  it("retries network failures, 5xx, 408 and 429, not other 4xx", () => {
    expect(retryableHttpFailure(new TypeError("x"))).toBe(true);
    expect(retryableHttpFailure(new HttpError(502))).toBe(true);
    expect(retryableHttpFailure(new HttpError(429))).toBe(true);
    expect(retryableHttpFailure(new HttpError(408))).toBe(true);
    expect(retryableHttpFailure(new HttpError(401))).toBe(false);
    expect(retryableHttpFailure(new HttpError(404))).toBe(false);
  });
});

describe("ADR-0098: the statements still go out once the session is refused", () => {
  // Walk 8: after DELETE /account a cleanup's bearer request met 401, the
  // refresh answered refresh-invalid, the device signed out, and the
  // account-deleted statements found no session left to post from.
  it("a refused session after the auth delete signs nothing out, and every conversation gets its statement", async () => {
    const { SessionTokenProvider } = await import("./session-token-provider");
    const NOW = 1_800_000_000_000;
    const provider = new SessionTokenProvider();
    const refreshes: string[] = [];
    provider.configure({
      refresh: async (rt) => {
        refreshes.push(rt);
        return { kind: "refused", status: 401, errorCode: "refresh-invalid" };
      },
      persist: async () => {},
      now: () => NOW,
      fetchImpl: async () => new Response("", { status: 401 }),
    });
    provider.install("QUJDRA==", {
      sessionToken: "v2.acct.1.1.tag",
      sessionExpiresAtMs: NOW + 900_000,
      refreshToken: "r1.acct.1.1.tag",
      refreshExpiresAtMs: NOW + 1_000_000_000,
      keyGeneration: 1n,
    });
    let signedOut = false;
    provider.subscribe((e) => {
      if (e.kind === "refresh-invalid" || e.kind === "superseded" || e.kind === "device-revoked") signedOut = true;
    });
    const posted: string[] = [];
    const conversations = ["one-to-one", "group-a", "group-b"];
    const r = recorder();
    await runAccountDeletion({
      ...r.deps,
      deleteAtAuth: async () => {
        r.events.push("auth");
      },
      afterAuth: () => provider.markAccountDeleted(),
      cleanups: async () => [
        {
          label: "push",
          // A bearer request to auth after the delete: 401, then a refresh.
          run: async () => {
            await provider.fetch("https://example.test/api/auth/auth/tokens", {
              method: "POST",
              headers: { authorization: "Bearer v2.acct.1.1.tag" },
            });
            await provider.refreshOnce();
          },
        },
        ...conversations.map(
          (c): DeletionCleanup => ({
            label: "account-deleted-statement",
            run: async () => {
              if (signedOut) throw new Error("no session left to post from");
              posted.push(c);
            },
          }),
        ),
      ],
      wipe: async () => {
        r.events.push("wipe");
      },
    });
    expect(signedOut).toBe(false);
    expect(refreshes).toEqual([]);
    expect(posted.sort()).toEqual([...conversations].sort());
    expect(r.events).toEqual(["auth", "wipe"]);
  });

  it("without the mark, the same refusal signs the device out (the defect)", async () => {
    const { SessionTokenProvider } = await import("./session-token-provider");
    const NOW = 1_800_000_000_000;
    const provider = new SessionTokenProvider();
    provider.configure({
      refresh: async () => ({ kind: "refused", status: 401, errorCode: "refresh-invalid" }),
      persist: async () => {},
      now: () => NOW,
    });
    provider.install("QUJDRA==", {
      sessionToken: "v2.acct.1.1.tag",
      sessionExpiresAtMs: NOW + 900_000,
      refreshToken: "r1.acct.1.1.tag",
      refreshExpiresAtMs: NOW + 1_000_000_000,
      keyGeneration: 1n,
    });
    const events: string[] = [];
    provider.subscribe((e) => events.push(e.kind));
    await provider.refreshOnce();
    expect(events).toEqual(["refresh-invalid"]);
  });
});
