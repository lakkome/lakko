// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * ADR-0074 "Joining after a rotation" (contract G-56 sections 2 and 4): the
 * landing's gate, the parked device's look for the earlier keys, and the
 * joining device's persist order, with the core's real binding verdict.
 */
import { readFileSync } from "node:fs";
import { fileURLToPath } from "node:url";
import { beforeAll, describe, expect, it, vi } from "vitest";

import init, * as wasm from "../../wasm/lakko_ffi_wasm";
import {
  OWN_LINK_ABSENT,
  OWN_LINK_UNKNOWN,
  bindingVerdict,
  type OwnLink,
} from "../devices/own-conversation-key";
import {
  ParkedLandingGuard,
  persistRecoveredKeysInOrder,
  runEarlierKeysGate,
  tryEarlierKeysOnce,
  type BindingFacts,
  type EarlierKeysGateDeps,
} from "./earlier-keys";

beforeAll(async () => {
  const path = fileURLToPath(new URL("../../wasm/lakko_ffi_wasm_bg.wasm", import.meta.url));
  await init({ module_or_path: readFileSync(path) });
});

const key = (n: number): Uint8Array => new Uint8Array(32).fill(n);
const CUR = key(0xc0);
const K0 = key(0x10);
const LINK_K0: OwnLink = { kind: "key", key: K0 };

/** A fresh device with no binding, no conversation, and `held` earlier keys. */
function facts(held: readonly Uint8Array[], bound: Uint8Array | null = null): BindingFacts {
  return { boundKey: bound, currentKey: CUR, predecessorKeys: held, holdsConversations: false };
}

function deps(over: Partial<EarlierKeysGateDeps> & { held?: () => readonly Uint8Array[]; bound?: Uint8Array | null }): EarlierKeysGateDeps & {
  fetches: number;
  written: OwnLink[];
} {
  const out = {
    fetches: 0,
    written: [] as OwnLink[],
    readFacts: async () => facts(over.held?.() ?? [], over.bound ?? null),
    readRecord: async () => null,
    fetchLink: async (): Promise<OwnLink> => {
      out.fetches += 1;
      return OWN_LINK_ABSENT;
    },
    writeRecord: async (link: OwnLink) => {
      out.written.push(link);
    },
    verdict: (f: BindingFacts, link: OwnLink) => bindingVerdict(wasm, { ...f, link }),
    ...over,
  };
  return out;
}

describe("the earlier-keys gate (runs before anything binds)", () => {
  it("a landing with link Key(k0) and no k0 waits: nothing to mint, nothing to publish", async () => {
    const d = deps({ fetchLink: async () => LINK_K0 });
    const mint = vi.fn();
    const publishKeyPackage = vi.fn();
    const answer = await runEarlierKeysGate(d);
    // The landing's order: the gate, then (only on `land`) the binding and
    // the KeyPackage.
    if (answer.kind === "land") {
      mint();
      publishKeyPackage();
    }
    expect(answer.kind).toBe("wait");
    expect(mint).not.toHaveBeenCalled();
    expect(publishKeyPackage).not.toHaveBeenCalled();
    // The answer is recorded for the lineage guard.
    expect(d.written).toEqual([LINK_K0]);
  });

  it("lands under the conversation key once k0 is held", async () => {
    const d = deps({ held: () => [K0], fetchLink: async () => LINK_K0 });
    expect(await runEarlierKeysGate(d)).toEqual({ kind: "land", link: LINK_K0 });
  });

  it("a mint needs an answer: an unreadable bundle waits", async () => {
    const d = deps({ fetchLink: async () => OWN_LINK_UNKNOWN });
    expect((await runEarlierKeysGate(d)).kind).toBe("wait");
    expect(d.written).toEqual([OWN_LINK_UNKNOWN]);
  });

  it("a warm landing whose binding is kept reads no bundle", async () => {
    const d = deps({ bound: CUR });
    expect(await runEarlierKeysGate(d)).toEqual({ kind: "land", link: OWN_LINK_ABSENT });
    expect(d.fetches).toBe(0);
  });

  it("uses the route's link and fetches nothing more", async () => {
    const d = deps({});
    expect((await runEarlierKeysGate(d, { link: LINK_K0 })).kind).toBe("wait");
    expect(d.fetches).toBe(0);
  });

  it("a kept device that the recorded link makes wait reads the bundle again, and waits", async () => {
    // Bound under the current key, no conversations, the record names k0:
    // the vector `cur / [] / Key(k0) / no -> wait`.
    const fetchLink = vi.fn(async () => LINK_K0);
    const d = deps({ bound: CUR, readRecord: async () => LINK_K0, fetchLink });
    expect((await runEarlierKeysGate(d)).kind).toBe("wait");
    expect(fetchLink).toHaveBeenCalledTimes(1);
  });

  it("the link gone (the account was reset, or the link stripped) lands", async () => {
    const d = deps({ readRecord: async () => LINK_K0, fetchLink: async () => OWN_LINK_ABSENT });
    expect((await runEarlierKeysGate(d)).kind).toBe("land");
  });

  it("signup, guest signup and the landing after a reset or recall are exempt", async () => {
    const d = deps({ fetchLink: async () => LINK_K0 });
    expect(await runEarlierKeysGate(d, { exempt: true })).toEqual({ kind: "land", link: OWN_LINK_ABSENT });
    expect(d.fetches).toBe(0);
  });
});

describe("the parked device looks for the earlier keys", () => {
  it("lands after the backup supplies k0, and not before", async () => {
    let held: Uint8Array[] = [];
    let backupHasK0 = false;
    const land = vi.fn(async (_link: OwnLink) => undefined);
    const gateDeps = deps({ held: () => held, fetchLink: async () => LINK_K0 });
    const retry = {
      stillParked: () => true,
      fetchBackup: async () => {
        if (backupHasK0) held = [K0];
      },
      fetchLink: async () => LINK_K0,
      gate: (link: OwnLink) => runEarlierKeysGate(gateDeps, { link }),
      land,
    };
    expect(await tryEarlierKeysOnce(retry)).toBe("waiting");
    expect(land).not.toHaveBeenCalled();
    backupHasK0 = true;
    expect(await tryEarlierKeysOnce(retry)).toBe("landed");
    expect(land).toHaveBeenCalledWith(LINK_K0);
  });

  it("stops when the device is no longer parked", async () => {
    const land = vi.fn();
    const outcome = await tryEarlierKeysOnce({
      stillParked: () => false,
      fetchBackup: vi.fn(),
      fetchLink: vi.fn(),
      gate: vi.fn(),
      land,
    });
    expect(outcome).toBe("stopped");
    expect(land).not.toHaveBeenCalled();
  });
});

describe("the joining device persists the earlier keys first", () => {
  it("stores the predecessors before the seed, the phrase and the rest", async () => {
    const order: string[] = [];
    await persistRecoveredKeysInOrder({
      predecessors: async () => void order.push("predecessors"),
      seed: async () => void order.push("seed"),
      phrase: async () => void order.push("phrase"),
      rest: async () => void order.push("rest"),
    });
    expect(order).toEqual(["predecessors", "seed", "phrase", "rest"]);
  });
});

describe("one landing of a parked account at a time", () => {
  /** A landing that records when it runs and can be held open. */
  function landing(log: string[], name: string): { run: () => Promise<string>; finish: () => void } {
    let finish: () => void = () => undefined;
    const held = new Promise<void>((resolve) => {
      finish = resolve;
    });
    return {
      run: async () => {
        log.push(`${name}:start`);
        await held;
        log.push(`${name}:end`);
        return name;
      },
      finish: () => finish(),
    };
  }

  it("two concurrent looks run once", async () => {
    const guard = new ParkedLandingGuard();
    const log: string[] = [];
    const a = landing(log, "look-a");
    const b = landing(log, "look-b");
    const first = guard.tryRun(a.run);
    const second = guard.tryRun(b.run);
    expect(first).not.toBeNull();
    expect(second).toBeNull();
    a.finish();
    await first;
    expect(log).toEqual(["look-a:start", "look-a:end"]);
  });

  it("a look skips while a pairing completion is in flight", async () => {
    const guard = new ParkedLandingGuard();
    const log: string[] = [];
    const pairing = landing(log, "pairing");
    const running = guard.runExclusive(pairing.run);
    expect(guard.tryRun(landing(log, "look").run)).toBeNull();
    pairing.finish();
    await running;
    expect(log).toEqual(["pairing:start", "pairing:end"]);
  });

  it("a pairing completion waits out a look in progress, then runs and re-checks", async () => {
    const guard = new ParkedLandingGuard();
    const log: string[] = [];
    let parked = true;
    const look = landing(log, "look");
    const looking = guard.tryRun(async () => {
      const r = await look.run();
      parked = false; // the look landed the session
      return r;
    });
    const pairing = guard.runExclusive(async () => {
      log.push(parked ? "pairing:lands" : "pairing:stale");
      return "pairing";
    });
    await Promise.resolve();
    expect(log).toEqual(["look:start"]);
    look.finish();
    await Promise.all([looking, pairing]);
    expect(log).toEqual(["look:start", "look:end", "pairing:stale"]);
    expect(guard.busy).toBe(false);
  });

  it("idle resolves once the holder lets go, and a thrown landing releases the guard", async () => {
    const guard = new ParkedLandingGuard();
    await expect(guard.runExclusive(async () => {
      throw new Error("refused");
    })).rejects.toThrow("refused");
    expect(guard.busy).toBe(false);
    const log: string[] = [];
    const held = landing(log, "held");
    const running = guard.runExclusive(held.run);
    const after = guard.idle().then(() => log.push("idle"));
    held.finish();
    await Promise.all([running, after]);
    expect(log).toEqual(["held:start", "held:end", "idle"]);
  });
});
