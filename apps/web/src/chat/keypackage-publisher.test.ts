// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * KeyPackage republish ordering and single-flight (`keypackage-publisher.ts`).
 *
 * The ordering property is the security-relevant one: a KeyPackage may be
 * uploaded only after the snapshot holding its private key is on disk, and a
 * persist failure must mean no upload at all.
 */
import { describe, expect, it } from "vitest";

import {
  createKeyPackagePublisher,
  type KeyPackagePublisherDeps,
} from "./keypackage-publisher";

function deferred(): { promise: Promise<void>; resolve: () => void } {
  let resolve!: () => void;
  const promise = new Promise<void>((r) => {
    resolve = r;
  });
  return { promise, resolve };
}

async function flush(): Promise<void> {
  for (let i = 0; i < 10; i += 1) await Promise.resolve();
}

describe("createKeyPackagePublisher", () => {
  it("creates, then persists, then uploads the same KeyPackage", async () => {
    const log: string[] = [];
    let serial = 0;
    const deps: KeyPackagePublisherDeps<{ account: string }> = {
      resolveContext: () => {
        log.push("context");
        return { account: "a" };
      },
      createKeyPackage: () => {
        serial += 1;
        log.push(`create ${serial}`);
        return new Uint8Array([serial]);
      },
      persistDurable: async () => {
        log.push("persist");
      },
      upload: async (_ctx, kp) => {
        log.push(`upload ${kp[0]}`);
      },
    };
    await createKeyPackagePublisher(deps).publish();
    expect(log).toEqual(["context", "create 1", "persist", "upload 1"]);
  });

  it("does not upload when the persist fails, and reports the failure", async () => {
    let uploads = 0;
    const publisher = createKeyPackagePublisher({
      resolveContext: () => ({}),
      createKeyPackage: () => new Uint8Array([1]),
      persistDurable: async () => {
        throw new Error("IDB write failed");
      },
      upload: async () => {
        uploads += 1;
      },
    });
    await expect(publisher.publish()).rejects.toThrow("IDB write failed");
    expect(uploads).toBe(0);
  });

  it("does nothing when there is no session to publish for", async () => {
    let creates = 0;
    const publisher = createKeyPackagePublisher({
      resolveContext: () => {
        throw new Error("no session");
      },
      createKeyPackage: () => {
        creates += 1;
        return new Uint8Array([1]);
      },
      persistDurable: async () => undefined,
      upload: async () => undefined,
    });
    await expect(publisher.publish()).rejects.toThrow("no session");
    expect(creates).toBe(0);
  });

  it("collapses a burst of requests during a publish into one trailing publish", async () => {
    const uploadGates: Array<ReturnType<typeof deferred>> = [];
    let creates = 0;
    let uploadsStarted = 0;
    const publisher = createKeyPackagePublisher({
      resolveContext: () => ({}),
      createKeyPackage: () => {
        creates += 1;
        return new Uint8Array([creates]);
      },
      persistDurable: async () => undefined,
      upload: async () => {
        uploadsStarted += 1;
        const gate = deferred();
        uploadGates.push(gate);
        await gate.promise;
      },
    });

    const first = publisher.publish();
    await flush();
    expect(uploadsStarted).toBe(1);

    // Five Welcomes land while the first upload is parked.
    const burst = [1, 2, 3, 4, 5].map(() => publisher.publish());
    await flush();
    expect(creates).toBe(1);

    uploadGates[0]?.resolve();
    await first;
    await flush();
    // Exactly one trailing publish started after the first finished.
    expect(creates).toBe(2);
    expect(uploadsStarted).toBe(2);

    uploadGates[1]?.resolve();
    await Promise.all(burst);
    expect(creates).toBe(2);
  });

  it("still runs the trailing publish when the running one failed", async () => {
    let attempt = 0;
    let uploads = 0;
    const gate = deferred();
    const publisher = createKeyPackagePublisher({
      resolveContext: () => ({}),
      createKeyPackage: () => new Uint8Array([1]),
      persistDurable: async () => {
        attempt += 1;
        if (attempt === 1) {
          await gate.promise;
          throw new Error("transient");
        }
      },
      upload: async () => {
        uploads += 1;
      },
    });
    const first = publisher.publish();
    const second = publisher.publish();
    gate.resolve();
    await expect(first).rejects.toThrow("transient");
    await expect(second).resolves.toBeUndefined();
    expect(uploads).toBe(1);
  });
});
