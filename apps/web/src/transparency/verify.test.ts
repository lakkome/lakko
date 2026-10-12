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

import init, { verifyWitnessQuorum } from "../../wasm/lakko_ffi_wasm";
import {
  verifyIdentityInLog,
  verifyRotationChain,
  type LogVerifyResult,
  type LogVerifyStatus,
  type RotationChainResult,
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
const WITNESS_KEY = new Uint8Array(32).fill(0x5a);
/** A second + third pinned witness key, for K-of-N quorum tests (ADR-0039). */
const WITNESS_KEY_2 = new Uint8Array(32).fill(0x5b);
const WITNESS_KEY_3 = new Uint8Array(32).fill(0x5c);
/** A witness co-sig blob fixture (105 bytes — the gate stub ignores the
 *  contents; the real verify is covered by the Rust `witness.rs` tests). */
const COSIG_FIXTURE = new Uint8Array(105).fill(0x6c);

/** Build a `SignedRoot` fixture at the given log size, optionally with a
 *  list of witness co-sigs (ADR-0039; default `[]` — no co-sig served). */
function rootAt(
  logSize: bigint,
  witnessCosigs: Uint8Array[] = [],
): SignedRoot {
  return {
    treeRoot: new Uint8Array(32).fill(0x11),
    logSize,
    timestampUnix: 1_700_000_000n,
    signature: new Uint8Array(64).fill(0x22),
    witnessCosigs,
  };
}

/** Build a `LatestEntry` fixture serving `pubkey` with an optional
 *  `is_reset` marker (default `false`), an optional `leafAuth`
 *  (G-30 #2; default `null` — no genesis self-sig to check), and an
 *  optional `genesisAttest` (ADR-0036; default `null` — no auth-attested
 *  genesis to check). */
function latestWith(
  pubkey: Uint8Array,
  isReset = false,
  leafAuth: Uint8Array | null = null,
  genesisAttest: Uint8Array | null = null,
): LatestEntry {
  return {
    idx: 3n,
    version: 1n,
    identityPubkey: pubkey,
    isReset,
    leafAuth,
    genesisAttest,
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
 *  inclusion stub unless overridden, returning the full result. */
function runFull(
  client: TransparencyClient | null,
  extra: Partial<VerifyDeps> = {},
): Promise<LogVerifyResult> {
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

/** Convenience — run the verifier and return only its `status`. */
async function run(
  client: TransparencyClient | null,
  extra: Partial<VerifyDeps> = {},
): Promise<LogVerifyStatus> {
  return (await runFull(client, extra)).status;
}

describe("verifyIdentityInLog (KT-log S3c status machine)", () => {
  it('returns "off" when the client is null (integration unconfigured)', async () => {
    expect(await run(null)).toBe("off");
  });

  it('returns "off" when the operator key is null', async () => {
    const client = mockClient({});
    expect(
      (
        await verifyIdentityInLog(ACCOUNT_ID, SERVED_PUBKEY, {
          client,
          operatorKey: null,
        })
      ).status,
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

describe("verifyIdentityInLog — KT-log S4 isReset capture", () => {
  it("confirmed carries isReset=true when the latest entry is a reset", async () => {
    const client = mockClient({
      fetchLatest: vi.fn(async () => latestWith(SERVED_PUBKEY, true)),
    });
    const result = await runFull(client);
    expect(result.status).toBe("confirmed");
    expect(result.isReset).toBe(true);
  });

  it("confirmed carries isReset=false when the latest entry is NOT a reset", async () => {
    const client = mockClient({
      fetchLatest: vi.fn(async () => latestWith(SERVED_PUBKEY, false)),
    });
    const result = await runFull(client);
    expect(result.status).toBe("confirmed");
    expect(result.isReset).toBe(false);
  });

  it("mismatch (key divergence) still carries the latest's isReset", async () => {
    // A latest entry exists (so isReset is meaningful), but its pubkey
    // differs from the served key → mismatch. The marker still rides out.
    const client = mockClient({
      fetchLatest: vi.fn(async () => latestWith(OTHER_PUBKEY, true)),
    });
    const result = await runFull(client);
    expect(result.status).toBe("mismatch");
    expect(result.isReset).toBe(true);
  });

  it("mismatch (proof-fail) carries the latest's isReset", async () => {
    const client = mockClient({
      fetchLatest: vi.fn(async () => latestWith(SERVED_PUBKEY, true)),
    });
    const result = await runFull(client, {
      verifyInclusion: () => {
        throw new Error("inclusion verify failed");
      },
    });
    expect(result.status).toBe("mismatch");
    expect(result.isReset).toBe(true);
  });

  it("absent → isReset is null (no latest entry to read)", async () => {
    const client = mockClient({ fetchLatest: vi.fn(async () => null) });
    const result = await runFull(client);
    expect(result.status).toBe("absent");
    expect(result.isReset).toBeNull();
  });

  it("rollback → isReset is null (short-circuits before the latest lookup)", async () => {
    checkAndAdvance(20n);
    const client = mockClient({ fetchRoot: vi.fn(async () => rootAt(10n)) });
    const result = await runFull(client);
    expect(result.status).toBe("rollback");
    expect(result.isReset).toBeNull();
  });

  it("unreachable → isReset is null", async () => {
    const client = mockClient({
      fetchLatest: vi.fn(async () => {
        throw new TransparencyNetworkError("/transparency/latest", "down");
      }),
    });
    const result = await runFull(client);
    expect(result.status).toBe("unreachable");
    expect(result.isReset).toBeNull();
  });

  it("off → isReset is null", async () => {
    const result = await verifyIdentityInLog(ACCOUNT_ID, SERVED_PUBKEY, {
      client: null,
      operatorKey: OPERATOR_KEY,
    });
    expect(result.status).toBe("off");
    expect(result.isReset).toBeNull();
  });
});

describe("verifyRotationChain (G-30 #2, ADR-0019)", () => {
  // The rotation context the chain verifier consumes. `pinnedOldPubkey`
  // and `supersessionProofB64` are arbitrary in these tests because the
  // `verifyChain` SEAM replaces the real wasm gate (a valid supersession
  // proof + Merkle proof can only be built by the Rust log builder).
  const OLD_PUBKEY = new Uint8Array(32).fill(0x99);
  const ROTATION_CTX = {
    supersessionProofB64: btoa("fake-supersession-proof-bytes"),
    pinnedOldPubkey: OLD_PUBKEY,
    pinnedLogVersion: 1,
  };

  /** Run the rotation-chain verifier with a fixed operator key + a
   *  default-PASS chain stub (returns the served pubkey) unless
   *  overridden. */
  function runChain(
    client: TransparencyClient | null,
    extra: Partial<VerifyDeps> = {},
  ): Promise<RotationChainResult> {
    return verifyRotationChain(ACCOUNT_ID, SERVED_PUBKEY, ROTATION_CTX, {
      client,
      operatorKey: OPERATOR_KEY,
      // Default chain stub: PASS — returns the authorised new pubkey.
      verifyChain: () => SERVED_PUBKEY,
      ...extra,
    });
  }

  it('returns "off" + version null when the client is null', async () => {
    const r = await runChain(null);
    expect(r.status).toBe("off");
    expect(r.version).toBeNull();
    expect(r.isReset).toBeNull();
  });

  it('returns "off" when the operator key is null', async () => {
    const client = mockClient({});
    const r = await verifyRotationChain(ACCOUNT_ID, SERVED_PUBKEY, ROTATION_CTX, {
      client,
      operatorKey: null,
      verifyChain: () => SERVED_PUBKEY,
    });
    expect(r.status).toBe("off");
    expect(r.version).toBeNull();
  });

  it('returns "confirmed" + the logged version on a passing chain', async () => {
    // latest serves the SERVED key at version 7 → confirmed carries 7.
    const client = mockClient({
      fetchLatest: vi.fn(async () => ({
        idx: 3n,
        version: 7n,
        identityPubkey: SERVED_PUBKEY,
        isReset: false,
        leafAuth: null,
        genesisAttest: null,
      })),
    });
    const r = await runChain(client);
    expect(r.status).toBe("confirmed");
    expect(r.version).toBe(7);
    expect(r.isReset).toBe(false);
  });

  it('carries the latest isReset through a confirmed chain', async () => {
    const client = mockClient({
      fetchLatest: vi.fn(async () => latestWith(SERVED_PUBKEY, true)),
    });
    const r = await runChain(client);
    expect(r.status).toBe("confirmed");
    expect(r.isReset).toBe(true);
  });

  it('returns "mismatch" (NO throw) when the chain gate throws', async () => {
    const client = mockClient({});
    const r = await runChain(client, {
      verifyChain: () => {
        throw new Error("chain verify failed");
      },
    });
    expect(r.status).toBe("mismatch");
    // mismatch never anchors a version (the chain did not prove it).
    expect(r.version).toBeNull();
    // A latest entry existed, so its marker still rides out.
    expect(r.isReset).toBe(false);
  });

  it('returns "absent" + version null when /latest is null (rotated key not yet logged)', async () => {
    const client = mockClient({ fetchLatest: vi.fn(async () => null) });
    const r = await runChain(client);
    expect(r.status).toBe("absent");
    expect(r.version).toBeNull();
    expect(r.isReset).toBeNull();
  });

  it('returns "absent" when the log head is NOT the served key (cannot chain-prove it)', async () => {
    // The log's latest commits to a DIFFERENT key — the served rotation
    // is not (yet) the log head, so the chain cannot prove it. We surface
    // `absent` (no chain confirmation) and do NOT block; the standalone
    // gate-1 result stands. The latest's isReset still rides out.
    const fetchProof = vi.fn(async () => PROOF_FIXTURE);
    const client = mockClient({
      fetchLatest: vi.fn(async () => latestWith(OTHER_PUBKEY, true)),
      fetchProof,
    });
    const r = await runChain(client);
    expect(r.status).toBe("absent");
    expect(r.version).toBeNull();
    expect(r.isReset).toBe(true);
    // We never even fetched a proof — the head divergence short-circuits.
    expect(fetchProof).not.toHaveBeenCalled();
  });

  it('returns "rollback" when the root log_size shrank below last-seen', async () => {
    checkAndAdvance(20n);
    const client = mockClient({ fetchRoot: vi.fn(async () => rootAt(10n)) });
    const r = await runChain(client);
    expect(r.status).toBe("rollback");
    expect(r.version).toBeNull();
  });

  it("checks rollback BEFORE the latest lookup (a shrink short-circuits)", async () => {
    checkAndAdvance(20n);
    const fetchLatest = vi.fn(async () => latestWith(SERVED_PUBKEY));
    const client = mockClient({
      fetchRoot: vi.fn(async () => rootAt(10n)),
      fetchLatest,
    });
    const r = await runChain(client);
    expect(r.status).toBe("rollback");
    expect(fetchLatest).not.toHaveBeenCalled();
  });

  it('returns "unreachable" on a network error fetching the root', async () => {
    const client = mockClient({
      fetchRoot: vi.fn(async () => {
        throw new TransparencyNetworkError("/transparency/root", "down");
      }),
    });
    const r = await runChain(client);
    expect(r.status).toBe("unreachable");
    expect(r.version).toBeNull();
  });

  it('returns "unreachable" on a network error fetching the proof', async () => {
    const client = mockClient({
      fetchProof: vi.fn(async () => {
        throw new TransparencyNetworkError("/transparency/proof", "down");
      }),
    });
    const r = await runChain(client);
    expect(r.status).toBe("unreachable");
    expect(r.version).toBeNull();
  });
});

describe("verifyIdentityInLog — G-30 #2 genesis self-sig", () => {
  const GENESIS_SIG = new Uint8Array(64).fill(0x47);
  const ROTATION_PROOF = new Uint8Array(145).fill(0x52);

  it('confirmed when a 64-byte genesis leaf_auth verifies', async () => {
    const client = mockClient({
      fetchLatest: vi.fn(async () =>
        latestWith(SERVED_PUBKEY, false, GENESIS_SIG),
      ),
    });
    const r = await runFull(client, {
      verifyGenesis: () => {
        /* pass */
      },
    });
    expect(r.status).toBe("confirmed");
  });

  it('mismatch when a 64-byte genesis leaf_auth FAILS to verify', async () => {
    const client = mockClient({
      fetchLatest: vi.fn(async () =>
        latestWith(SERVED_PUBKEY, false, GENESIS_SIG),
      ),
    });
    const r = await runFull(client, {
      verifyGenesis: () => {
        throw new Error("bad genesis self-sig");
      },
    });
    expect(r.status).toBe("mismatch");
  });

  it("confirmed (genesis check skipped) when leaf_auth is null", async () => {
    const client = mockClient({
      fetchLatest: vi.fn(async () => latestWith(SERVED_PUBKEY, false, null)),
    });
    const verifyGenesis = vi.fn(() => {
      throw new Error("genesis check must NOT run for a null leaf_auth");
    });
    const r = await runFull(client, { verifyGenesis });
    expect(r.status).toBe("confirmed");
    expect(verifyGenesis).not.toHaveBeenCalled();
  });

  it("confirmed (genesis check skipped) when leaf_auth is a 145-byte rotation proof", async () => {
    // A 145-byte supersession proof is the rotation case — verified by the
    // chain in verifyRotationChain, not the genesis gate here.
    const client = mockClient({
      fetchLatest: vi.fn(async () =>
        latestWith(SERVED_PUBKEY, false, ROTATION_PROOF),
      ),
    });
    const verifyGenesis = vi.fn(() => {
      throw new Error("genesis check must NOT run for a 145-byte proof");
    });
    const r = await runFull(client, { verifyGenesis });
    expect(r.status).toBe("confirmed");
    expect(verifyGenesis).not.toHaveBeenCalled();
  });
});

describe("verifyIdentityInLog — ADR-0036 auth-attested genesis", () => {
  const GENESIS_SIG = new Uint8Array(64).fill(0x47);
  const GENESIS_ATTEST = new Uint8Array(153).fill(0x36);
  const ATTEST_KEY = new Uint8Array(32).fill(0x5a);

  it("confirmed when a present genesis_attest verifies (additive to self-sig)", async () => {
    const client = mockClient({
      fetchLatest: vi.fn(async () =>
        latestWith(SERVED_PUBKEY, false, GENESIS_SIG, GENESIS_ATTEST),
      ),
    });
    const verifyGenesisAttest = vi.fn(() => {
      /* pass */
    });
    const r = await runFull(client, {
      // the self-sig gate must also pass so we reach the attest gate.
      verifyGenesis: () => {
        /* pass */
      },
      attestKey: ATTEST_KEY,
      verifyGenesisAttest,
    });
    expect(r.status).toBe("confirmed");
    expect(verifyGenesisAttest).toHaveBeenCalledTimes(1);
  });

  it("mismatch when a present genesis_attest FAILS to verify (fail-closed)", async () => {
    const client = mockClient({
      fetchLatest: vi.fn(async () =>
        latestWith(SERVED_PUBKEY, false, GENESIS_SIG, GENESIS_ATTEST),
      ),
    });
    const r = await runFull(client, {
      verifyGenesis: () => {
        /* pass */
      },
      attestKey: ATTEST_KEY,
      verifyGenesisAttest: () => {
        throw new Error("bad genesis attestation");
      },
    });
    expect(r.status).toBe("mismatch");
  });

  it("confirmed (attest check skipped) when genesis_attest is null", async () => {
    const client = mockClient({
      fetchLatest: vi.fn(async () =>
        latestWith(SERVED_PUBKEY, false, GENESIS_SIG, null),
      ),
    });
    const verifyGenesisAttest = vi.fn(() => {
      throw new Error("attest check must NOT run for a null genesis_attest");
    });
    const r = await runFull(client, {
      verifyGenesis: () => {
        /* pass */
      },
      attestKey: ATTEST_KEY,
      verifyGenesisAttest,
    });
    expect(r.status).toBe("confirmed");
    expect(verifyGenesisAttest).not.toHaveBeenCalled();
  });

  it("confirmed (attest check skipped) when the attest key is unavailable", async () => {
    // A present attestation the client cannot verify (no pin + JWKS
    // unreachable → attestKey null) is SKIPPED, not failed — the self-sig
    // + inclusion gates still stand (best-effort posture).
    const client = mockClient({
      fetchLatest: vi.fn(async () =>
        latestWith(SERVED_PUBKEY, false, GENESIS_SIG, GENESIS_ATTEST),
      ),
    });
    const verifyGenesisAttest = vi.fn(() => {
      throw new Error("attest check must NOT run when the key is null");
    });
    const r = await runFull(client, {
      verifyGenesis: () => {
        /* pass */
      },
      attestKey: null,
      verifyGenesisAttest,
    });
    expect(r.status).toBe("confirmed");
    expect(verifyGenesisAttest).not.toHaveBeenCalled();
  });
});

describe("witness quorum gate (ADR-0039, G-30 finding-1)", () => {
  const ROTATION_CTX = {
    supersessionProofB64: btoa("fake-supersession-proof-bytes"),
    pinnedOldPubkey: new Uint8Array(32).fill(0x99),
    pinnedLogVersion: 1,
  };
  /** A distinct 105-byte foreign/garbage co-sig blob (not from a pinned
   *  witness — never inflates or sinks the distinct-valid tally). */
  const GARBAGE_COSIG = new Uint8Array(105).fill(0x99);

  describe("verifyIdentityInLog — gate posture", () => {
    it("no pinned witness keys (empty set) → gate OFF, a root with no co-sig still confirms", async () => {
      // The transition posture: with no witness keys pinned, the operator-only
      // path is unchanged — a root without any co-sig is fine.
      const client = mockClient({
        fetchRoot: vi.fn(async () => rootAt(10n, [])),
      });
      const r = await runFull(client, { witnessKeys: [] });
      expect(r.status).toBe("confirmed");
    });

    it("1-of-1: pinned key + valid co-sig → confirmed; gate handed the cosig LIST + key SET + threshold", async () => {
      // Capture the gate args (the quorum gate is handed the served root's
      // (tree_root, log_size) + the WHOLE cosig list + the pinned key SET +
      // the threshold).
      let captured: {
        cosigs: Uint8Array[];
        witnessKeys: Uint8Array[];
        threshold: number;
        treeRoot: Uint8Array;
        logSize: bigint;
      } | null = null;
      let calls = 0;
      const client = mockClient({
        fetchRoot: vi.fn(async () => rootAt(10n, [COSIG_FIXTURE])),
      });
      const r = await runFull(client, {
        witnessKeys: [WITNESS_KEY],
        // threshold omitted → defaults to 1 via witnessThreshold()
        verifyWitness: (args) => {
          calls += 1;
          captured = args;
        },
      });
      expect(r.status).toBe("confirmed");
      expect(calls).toBe(1);
      expect(captured).not.toBeNull();
      expect(captured!.cosigs).toEqual([COSIG_FIXTURE]);
      expect(captured!.witnessKeys).toEqual([WITNESS_KEY]);
      expect(captured!.threshold).toBe(1);
      expect(captured!.logSize).toBe(10n);
    });

    it("K-of-N: ≥K distinct valid co-sigs (quorum satisfied) → confirmed", async () => {
      // 3 pinned witnesses, threshold 2, 3 co-sigs served. The seam simulates
      // a satisfied quorum (returns). Assert the threshold + full key set are
      // threaded through.
      let captured: { witnessKeys: Uint8Array[]; threshold: number } | null =
        null;
      const client = mockClient({
        fetchRoot: vi.fn(async () =>
          rootAt(10n, [COSIG_FIXTURE, COSIG_FIXTURE, COSIG_FIXTURE]),
        ),
      });
      const r = await runFull(client, {
        witnessKeys: [WITNESS_KEY, WITNESS_KEY_2, WITNESS_KEY_3],
        witnessThreshold: 2,
        verifyWitness: (args) => {
          captured = args;
        },
      });
      expect(r.status).toBe("confirmed");
      expect(captured!.witnessKeys).toHaveLength(3);
      expect(captured!.threshold).toBe(2);
    });

    it("K-of-N: fewer than K distinct valid (sub-threshold) → mismatch (fail-closed)", async () => {
      // The seam THROWS to simulate the wasm's sub-threshold refusal (K-1
      // valid < K). The gate must fail closed.
      const client = mockClient({
        fetchRoot: vi.fn(async () => rootAt(10n, [COSIG_FIXTURE])),
      });
      const r = await runFull(client, {
        witnessKeys: [WITNESS_KEY, WITNESS_KEY_2, WITNESS_KEY_3],
        witnessThreshold: 2,
        verifyWitness: () => {
          throw new Error("only 1 distinct valid witness < threshold 2");
        },
      });
      expect(r.status).toBe("mismatch");
      expect(r.isReset).toBeNull();
    });

    it("K-of-N: a foreign/garbage co-sig alongside ≥K distinct valid → confirmed (garbage never sinks the quorum)", async () => {
      // The served bundle carries a foreign/garbage co-sig BESIDE the valid
      // ones. A garbage co-sig contributes 0 to the distinct-pinned-key tally
      // (never inflates it) and, being a well-formed 105-byte blob, never
      // sinks an otherwise-valid quorum. The whole list (garbage included) is
      // handed to the gate.
      let captured: { cosigs: Uint8Array[] } | null = null;
      const client = mockClient({
        fetchRoot: vi.fn(async () =>
          rootAt(10n, [GARBAGE_COSIG, COSIG_FIXTURE, COSIG_FIXTURE]),
        ),
      });
      const r = await runFull(client, {
        witnessKeys: [WITNESS_KEY, WITNESS_KEY_2],
        witnessThreshold: 2,
        verifyWitness: (args) => {
          captured = args;
        },
      });
      expect(r.status).toBe("confirmed");
      expect(captured!.cosigs).toContain(GARBAGE_COSIG);
      expect(captured!.cosigs).toHaveLength(3);
    });

    it("pinned key + NO co-sig on the root → mismatch (fail-closed, REAL wasm)", async () => {
      // No seam → drives the real wasm verifyWitnessQuorum. An empty cosig
      // bundle is 0 distinct valid < threshold 1 → the wasm throws → refuse.
      // The latest lookup must never run — the quorum gate short-circuits.
      const fetchLatest = vi.fn(async () => latestWith(SERVED_PUBKEY));
      const client = mockClient({
        fetchRoot: vi.fn(async () => rootAt(10n, [])),
        fetchLatest,
      });
      const r = await runFull(client, { witnessKeys: [WITNESS_KEY] });
      expect(r.status).toBe("mismatch");
      expect(r.isReset).toBeNull();
      expect(fetchLatest).not.toHaveBeenCalled();
    });

    it("pinned key + a well-formed but non-witness co-sig → mismatch (fail-closed, REAL wasm)", async () => {
      // A single 105-byte garbage co-sig cannot verify under the pinned key →
      // 0 distinct valid < threshold 1 → the real wasm throws → refuse.
      const client = mockClient({
        fetchRoot: vi.fn(async () => rootAt(10n, [GARBAGE_COSIG])),
      });
      const r = await runFull(client, { witnessKeys: [WITNESS_KEY] });
      expect(r.status).toBe("mismatch");
      expect(r.isReset).toBeNull();
    });

    it("pinned key + co-sig that FAILS to verify → mismatch (seam fail-closed)", async () => {
      const client = mockClient({
        fetchRoot: vi.fn(async () => rootAt(10n, [COSIG_FIXTURE])),
      });
      const r = await runFull(client, {
        witnessKeys: [WITNESS_KEY],
        verifyWitness: () => {
          throw new Error("witness co-sig does not verify");
        },
      });
      expect(r.status).toBe("mismatch");
      expect(r.isReset).toBeNull();
    });

    it("quorum gate runs AFTER the rollback gate (a shrink is still rollback)", async () => {
      // A shrunk root is rollback regardless of the witness quorum.
      checkAndAdvance(20n);
      const verifyWitness = vi.fn(() => {
        /* would pass, but must not even be consulted */
      });
      const client = mockClient({
        fetchRoot: vi.fn(async () => rootAt(10n, [COSIG_FIXTURE])),
      });
      const r = await runFull(client, {
        witnessKeys: [WITNESS_KEY],
        verifyWitness,
      });
      expect(r.status).toBe("rollback");
      expect(verifyWitness).not.toHaveBeenCalled();
    });
  });

  describe("verifyRotationChain — gate posture", () => {
    it("quorum satisfied (seam passes) → proceeds (confirmed)", async () => {
      const client = mockClient({
        fetchRoot: vi.fn(async () =>
          rootAt(10n, [COSIG_FIXTURE, COSIG_FIXTURE]),
        ),
      });
      const r = await verifyRotationChain(ACCOUNT_ID, SERVED_PUBKEY, ROTATION_CTX, {
        client,
        operatorKey: OPERATOR_KEY,
        witnessKeys: [WITNESS_KEY, WITNESS_KEY_2],
        witnessThreshold: 2,
        verifyWitness: () => {
          /* satisfied */
        },
        verifyChain: () => SERVED_PUBKEY,
      });
      expect(r.status).toBe("confirmed");
    });

    it("pinned keys + NO co-sig → mismatch (fail-closed, REAL wasm)", async () => {
      const fetchLatest = vi.fn(async () => latestWith(SERVED_PUBKEY));
      const client = mockClient({
        fetchRoot: vi.fn(async () => rootAt(10n, [])),
        fetchLatest,
      });
      const r = await verifyRotationChain(ACCOUNT_ID, SERVED_PUBKEY, ROTATION_CTX, {
        client,
        operatorKey: OPERATOR_KEY,
        witnessKeys: [WITNESS_KEY, WITNESS_KEY_2],
        witnessThreshold: 2,
        verifyChain: () => SERVED_PUBKEY,
      });
      expect(r.status).toBe("mismatch");
      expect(r.version).toBeNull();
      expect(fetchLatest).not.toHaveBeenCalled();
    });

    it("sub-threshold quorum (seam throws) → mismatch (fail-closed)", async () => {
      const client = mockClient({
        fetchRoot: vi.fn(async () => rootAt(10n, [COSIG_FIXTURE])),
      });
      const r = await verifyRotationChain(ACCOUNT_ID, SERVED_PUBKEY, ROTATION_CTX, {
        client,
        operatorKey: OPERATOR_KEY,
        witnessKeys: [WITNESS_KEY, WITNESS_KEY_2],
        witnessThreshold: 2,
        verifyWitness: () => {
          throw new Error("only 1 distinct valid witness < threshold 2");
        },
        verifyChain: () => SERVED_PUBKEY,
      });
      expect(r.status).toBe("mismatch");
      expect(r.version).toBeNull();
    });

    it("no pinned witness keys (empty set) → gate OFF, unchanged rotation behaviour", async () => {
      const client = mockClient({
        fetchRoot: vi.fn(async () => rootAt(10n, [])),
      });
      const r = await verifyRotationChain(ACCOUNT_ID, SERVED_PUBKEY, ROTATION_CTX, {
        client,
        operatorKey: OPERATOR_KEY,
        witnessKeys: [],
        verifyChain: () => SERVED_PUBKEY,
      });
      expect(r.status).toBe("confirmed");
    });
  });

  // Direct REAL-wasm coverage of the verifyWitnessQuorum fail-closed contract
  // with fixed vectors. A valid quorum needs Ed25519 witness signatures that
  // only the (server-side) witness signer can mint — the wasm is verify-only
  // (ADR-0020) — so the pass path is covered above via the gate seam; here we
  // pin the REFUSAL contract end-to-end through the real symbol.
  describe("real wasm verifyWitnessQuorum — fail-closed contract (fixed vectors)", () => {
    const TREE_ROOT = new Uint8Array(32).fill(0x11);
    const LOG_SIZE = 10n;
    const ONE_KEY = new Uint8Array(32).fill(0x5a);
    const ONE_COSIG = new Uint8Array(105).fill(0x6c);

    it("throws when threshold is 0 (rejected fail-closed, invariant #4)", () => {
      expect(() =>
        verifyWitnessQuorum(ONE_COSIG, ONE_KEY, 0, TREE_ROOT, LOG_SIZE),
      ).toThrow();
    });

    it("throws when cosigsFlat is not a multiple of 105 bytes", () => {
      expect(() =>
        verifyWitnessQuorum(
          new Uint8Array(104),
          ONE_KEY,
          1,
          TREE_ROOT,
          LOG_SIZE,
        ),
      ).toThrow();
    });

    it("throws when pinnedKeysFlat is not a multiple of 32 bytes", () => {
      expect(() =>
        verifyWitnessQuorum(
          ONE_COSIG,
          new Uint8Array(31),
          1,
          TREE_ROOT,
          LOG_SIZE,
        ),
      ).toThrow();
    });

    it("throws when the expected tree root is not 32 bytes", () => {
      expect(() =>
        verifyWitnessQuorum(
          ONE_COSIG,
          ONE_KEY,
          1,
          new Uint8Array(31),
          LOG_SIZE,
        ),
      ).toThrow();
    });

    it("throws when 0 distinct valid co-sigs < threshold 1 (garbage never verifies)", () => {
      // A well-formed-length garbage co-sig cannot verify under the pinned key.
      expect(() =>
        verifyWitnessQuorum(ONE_COSIG, ONE_KEY, 1, TREE_ROOT, LOG_SIZE),
      ).toThrow();
    });

    it("throws on an empty cosig bundle (0 < threshold 1)", () => {
      expect(() =>
        verifyWitnessQuorum(
          new Uint8Array(0),
          ONE_KEY,
          1,
          TREE_ROOT,
          LOG_SIZE,
        ),
      ).toThrow();
    });
  });
});
