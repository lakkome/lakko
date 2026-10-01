// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * ADR-0057 - the wasm half of the journal authority: the signer factory the
 * signed-in session installs into the DS access registry, and the registrar
 * derivations the owner needs. Kept apart from `ownership.ts` so that module
 * stays free of wasm and testable with fakes.
 */
import type { Identity as WasmIdentity } from "../../wasm/lakko_ffi_wasm";
import type { Wasm } from "../wasm";
import type { JournalAuthorityFactory } from "./access";
import type { JournalAuthority, RegistrarDerivation } from "./ownership";
import type { RegistrarSeedStore } from "./registrar-seeds";

function b64(bytes: Uint8Array): string {
  let s = "";
  for (let i = 0; i < bytes.length; i++) s += String.fromCharCode(bytes[i] ?? 0);
  return btoa(s);
}

/** The part of the wasm namespace the factory calls. */
export type AuthorityWasm = Pick<
  Wasm,
  "dsJournalRootSigner" | "dsJournalRootSeed" | "dsStatementSigner" | "dsJournalRegistrarSeed"
>;

/**
 * The factory: the root from the ACCOUNT identity (`dsJournalRootSigner`,
 * with the root seed from `dsJournalRootSeed(accountIdentity.ed25519Seed,
 * journalId)`), the registrar from the seed the owner handed this account.
 */
export function wasmJournalAuthorityFactory(
  wasm: AuthorityWasm,
  accountIdentity: () => WasmIdentity | null,
  seeds: RegistrarSeedStore,
  /** The "root held" marker of the signed-in account (`root-held.ts`);
   *  omitted = always held. */
  rootHeldMarker?: {
    held(groupIdB64: string): boolean;
    mark(groupIdB64: string): void;
  },
): JournalAuthorityFactory {
  return {
    ...(rootHeldMarker === undefined
      ? {}
      : {
          rootHeld: (groupId: Uint8Array): boolean => rootHeldMarker.held(b64(groupId)),
          markRootHeld: (groupId: Uint8Array): void => rootHeldMarker.mark(b64(groupId)),
        }),
    root(groupId): JournalAuthority {
      const account = accountIdentity();
      if (account === null) return { kind: "none" };
      const rootSeed = wasm.dsJournalRootSeed(account.ed25519Seed, groupId);
      const signer = wasm.dsJournalRootSigner(account, groupId);
      return { kind: "root", signer, rootSeed };
    },
    registrar(groupId): JournalAuthority {
      const stored = seeds.get(b64(groupId));
      if (stored === undefined) return { kind: "none" };
      return { kind: "registrar", signer: wasm.dsStatementSigner(stored.seed), rank: stored.rank };
    },
  };
}

/** The registrar derivations over wasm. */
export function wasmRegistrarDerivation(wasm: AuthorityWasm): RegistrarDerivation {
  return {
    registrarSeed: (rootSeed, memberAccountKey) =>
      wasm.dsJournalRegistrarSeed(rootSeed, memberAccountKey),
    publicKeyOfSeed: (seed) => {
      const signer = wasm.dsStatementSigner(seed);
      try {
        return signer.publicKey();
      } finally {
        signer.free();
      }
    },
  };
}
