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
