// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * KT-log S3c — `verifyIdentityInLog` status-machine tests.
 *
 * Drives the REAL wasm core for the account-key derivation step (the
 * `accountKey(...)` call inside the verifier), and injects a MOCKED
 * `TransparencyClient` + a fixed pinned operator key + a stubbed
 * inclusion gate (`verifyInclusion`) via the `VerifyDeps` seam. A valid
 * Merkle proof can only be built by the Rust log builder (not exposed
 * to wasm), so the inclusion gate is stubbed to throw/return — the
 * fields-plumbing through to the wasm verifier is covered by the Rust
 * `transparency.rs` tests + the S3b crate tests.
 *
 * Backed by an in-memory `localStorage` shim because the verifier runs
 * the no-rollback gate against the last-seen-root store.
 */
import {
  afterEach,
  beforeAll,
  beforeEach,
  describe,
  expect,
  it,
  vi,
} from "vitest";
import { readFileSync } from "node:fs";
import { fileURLToPath } from "node:url";

import init from "../../wasm/lakko_ffi_wasm";
import {
  verifyIdentityInLog,
  type LogVerifyStatus,
  type VerifyDeps,
} from "./verify";
import {
  TransparencyClient,
  TransparencyNetworkError,
  type SignedRoot,
  type LatestEntry,
  type InclusionProof,
} from "./client";
import { clearLastSeenRoot, checkAndAdvance } from "./last-seen-root";

/** In-memory `localStorage` shim (vitest `node` env has none). */
class MemoryStorage {
  private map = new Map<string, string>();
  getItem(k: string): string | null {
    return this.map.has(k) ? (this.map.get(k) as string) : null;
  }
  setItem(k: string, v: string): void {
    this.map.set(k, String(v));
  }
  removeItem(k: string): void {
    this.map.delete(k);
  }
  clear(): void {
    this.map.clear();
  }
  get length(): number {
    return this.map.size;
  }
  key(i: number): string | null {
    return Array.from(this.map.keys())[i] ?? null;
  }
}

beforeAll(async () => {
  const wasmPath = fileURLToPath(
    new URL("../../wasm/lakko_ffi_wasm_bg.wasm", import.meta.url),
  );
  await init({ module_or_path: readFileSync(wasmPath) });
});

beforeEach(() => {
  (globalThis as unknown as { localStorage: Storage }).localStorage =
    new MemoryStorage() as unknown as Storage;
});

afterEach(() => {
  delete (globalThis as unknown as { localStorage?: Storage }).localStorage;
});

const ACCOUNT_ID = new Uint8Array([
  1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 16,
]);
const SERVED_PUBKEY = new Uint8Array(32).fill(0xab);
const OTHER_PUBKEY = new Uint8Array(32).fill(0xcd);
const OPERATOR_KEY = new Uint8Array(32).fill(0x07);

/** Build a `SignedRoot` fixture at the given log size. */
function rootAt(logSize: bigint): SignedRoot {
  return {
    treeRoot: new Uint8Array(32).fill(0x11),
    logSize,
    timestampUnix: 1_700_000_000n,
    signature: new Uint8Array(64).fill(0x22),
  };
}

/** Build a `LatestEntry` fixture serving `pubkey`. */
function latestWith(pubkey: Uint8Array): LatestEntry {
  return {
    idx: 3n,
    version: 1n,
    identityPubkey: pubkey,
    isReset: false,
  };
}

const PROOF_FIXTURE: InclusionProof = {
  idx: 3n,
  leaf: new Uint8Array([0xde, 0xad]),
  auditPath: new Uint8Array(64).fill(0x33),
  logSize: 10n,
};

/**
 * A mock `TransparencyClient`. Each method is a vitest mock the test
 * configures. The cast to `TransparencyClient` lets it flow through the
 * `VerifyDeps.client` seam (only these three methods are called).
 */
function mockClient(impl: {
  fetchRoot?: () => Promise<SignedRoot>;
  fetchLatest?: (ak: Uint8Array) => Promise<LatestEntry | null>;
  fetchProof?: (idx: bigint) => Promise<InclusionProof>;
}): TransparencyClient {
  return {
    fetchRoot:
      impl.fetchRoot ?? vi.fn(async () => rootAt(10n)),
    fetchLatest:
      impl.fetchLatest ?? vi.fn(async () => latestWith(SERVED_PUBKEY)),
    fetchProof: impl.fetchProof ?? vi.fn(async () => PROOF_FIXTURE),
  } as unknown as TransparencyClient;
}

/** Run the verifier with a fixed operator key + a default-pass
 *  inclusion stub unless overridden. */
function run(
  client: TransparencyClient | null,
  extra: Partial<VerifyDeps> = {},
): Promise<LogVerifyStatus> {
  return verifyIdentityInLog(ACCOUNT_ID, SERVED_PUBKEY, {
    client,
    operatorKey: OPERATOR_KEY,
    // Default inclusion stub: PASS (no throw).
    verifyInclusion: () => {
      /* pass */
    },
    ...extra,
  });
}

describe("verifyIdentityInLog (KT-log S3c status machine)", () => {
  it('returns "off" when the client is null (integration unconfigured)', async () => {
    expect(await run(null)).toBe("off");
  });

  it('returns "off" when the operator key is null', async () => {
    const client = mockClient({});
    expect(
      await verifyIdentityInLog(ACCOUNT_ID, SERVED_PUBKEY, {
        client,
        operatorKey: null,
      }),
    ).toBe("off");
  });

  it('returns "confirmed" on a fully-valid root + latest + proof', async () => {
    const client = mockClient({});
    expect(await run(client)).toBe("confirmed");
  });

  it('returns "rollback" when the root log_size shrank below last-seen', async () => {
    // Seed the high-water mark above the root we will serve.
    checkAndAdvance(20n);
    const client = mockClient({ fetchRoot: vi.fn(async () => rootAt(10n)) });
    expect(await run(client)).toBe("rollback");
  });

  it('returns "absent" when /latest 404s (null)', async () => {
    const client = mockClient({ fetchLatest: vi.fn(async () => null) });
    expect(await run(client)).toBe("absent");
  });

  it('returns "mismatch" when the log key differs from the served key', async () => {
    const client = mockClient({
      fetchLatest: vi.fn(async () => latestWith(OTHER_PUBKEY)),
    });
    expect(await run(client)).toBe("mismatch");
  });

  it('returns "mismatch" when the inclusion proof fails to verify', async () => {
    const client = mockClient({});
    expect(
      await run(client, {
        verifyInclusion: () => {
          throw new Error("inclusion verify failed");
        },
      }),
    ).toBe("mismatch");
  });

  it('returns "unreachable" on a network error fetching the root', async () => {
    const client = mockClient({
      fetchRoot: vi.fn(async () => {
        throw new TransparencyNetworkError("/transparency/root", "down");
      }),
    });
    expect(await run(client)).toBe("unreachable");
  });

  it('returns "unreachable" on a network error fetching latest', async () => {
    const client = mockClient({
      fetchLatest: vi.fn(async () => {
        throw new TransparencyNetworkError("/transparency/latest", "down");
      }),
    });
    expect(await run(client)).toBe("unreachable");
  });

  it('returns "unreachable" on a network error fetching the proof', async () => {
    const client = mockClient({
      fetchProof: vi.fn(async () => {
        throw new TransparencyNetworkError("/transparency/proof", "down");
      }),
    });
    expect(await run(client)).toBe("unreachable");
  });

  it("checks rollback BEFORE the latest lookup (a shrink short-circuits)", async () => {
    checkAndAdvance(20n);
    const fetchLatest = vi.fn(async () => latestWith(SERVED_PUBKEY));
    const client = mockClient({
      fetchRoot: vi.fn(async () => rootAt(10n)),
      fetchLatest,
    });
    expect(await run(client)).toBe("rollback");
    // The latest lookup must NOT have run — rollback short-circuits.
    expect(fetchLatest).not.toHaveBeenCalled();
  });

  it("advances the last-seen mark on a non-shrinking confirmed check", async () => {
    clearLastSeenRoot();
    const client = mockClient({ fetchRoot: vi.fn(async () => rootAt(42n)) });
    expect(await run(client)).toBe("confirmed");
    // A subsequent smaller root is now a rollback.
    const client2 = mockClient({ fetchRoot: vi.fn(async () => rootAt(41n)) });
    expect(await run(client2)).toBe("rollback");
  });
});
