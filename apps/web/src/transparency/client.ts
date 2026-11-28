// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * `TransparencyClient` — fetch-based client for `lakko-svc-transparency`
 * (KT-log S3c).
 *
 * Three anonymous GET endpoints back the inclusion check:
 *
 *   * `GET /transparency/root` → the operator-signed Merkle root
 *     (`{ tree_root_b64, log_size, timestamp_unix, signature_b64 }`).
 *   * `GET /transparency/latest/:account_key_b64url` → the latest
 *     entry for an account-key (`{ idx, version, identity_pubkey_b64,
 *     is_reset }`) or 404 when the account is not yet logged.
 *   * `GET /transparency/proof/:idx` → an inclusion proof
 *     (`{ idx, leaf_b64, audit_path: [b64...], log_size }`).
 *
 * The client decodes the base64 fields to raw bytes so the verify
 * layer hands them straight into the wasm `verifyLogInclusion`. It is
 * transport-only: it never verifies signatures (that is `verify.ts`'s
 * job, against the build-time pinned operator key) and never consults
 * the `/transparency/operator-key` endpoint for trust (the operator
 * key is pinned out-of-band).
 *
 * `forOptionalConfig()` returns `null` when the transparency
 * integration is OFF (URL or operator-key pin unset) so the caller can
 * skip the whole check; see `config.ts` for the env-var contract.
 */

import {
  base64Decode,
  base64UrlEncodeNoPad,
} from "../auth/account-id";
import { transparencyBaseUrl, pinnedOperatorKey } from "./config";

/** Decoded `GET /transparency/root` response — bytes, not base64. */
export interface SignedRoot {
  /** 32-byte Merkle root over the first `logSize` entries. */
  readonly treeRoot: Uint8Array;
  /** Number of entries the root commits to. */
  readonly logSize: bigint;
  /** Operator-chosen timestamp (seconds since UNIX epoch). */
  readonly timestampUnix: bigint;
  /** 64-byte Ed25519 signature over the canonical root pre-image. */
  readonly signature: Uint8Array;
  /** ADR-0039 (G-30 finding-1): the witness co-signature wire blobs over
   *  `(treeRoot, logSize)` — a LIST, one per co-signing witness (empty `[]`
   *  when the server serves none). A client that pins a witness key SET
   *  requires a K-of-N quorum of valid ones (the split-view defence). Replaces
   *  the pre-ADR-0039 singular `witnessCosig`. */
  readonly witnessCosigs: Uint8Array[];
}

/** Decoded `GET /transparency/latest/:account_key` response. */
export interface LatestEntry {
  /** 0-based leaf index of the latest entry (== the `/proof` idx). */
  readonly idx: bigint;
  /** Server-assigned monotonic per-account-key version (>= 1). */
  readonly version: bigint;
  /** The account's current identity public key (raw bytes). */
  readonly identityPubkey: Uint8Array;
  /** ADR-0017 fresh-start marker — `true` if the latest entry was a
   *  no-supersession-proof owner reset. */
  readonly isReset: boolean;
  /** G-30 #2 (ADR-0019): the opaque client leaf authorisation stored with
   *  this entry (a 64-byte genesis self-sig or a 145-byte supersession
   *  proof), or `null` for a pre-Sprint-2 / unauthenticated row. A client
   *  verifies a genesis self-sig against the served key on a first
   *  observation. */
  readonly leafAuth: Uint8Array | null;
  /** ADR-0036 (G-30 finding 2): the opaque auth-attested genesis
   *  attestation stored with this entry, or `null` for a rotation / a
   *  pre-ADR-0036 row. A client verifies it against the pinned auth attest
   *  key on a first observation (beside the self-sig) to confirm the
   *  account authorised the genesis key — the anti-squatting gate. */
  readonly genesisAttest: Uint8Array | null;
}

/** Decoded `GET /transparency/proof/:idx` response — bytes, not base64.
 *  The `auditPath` is FLATTENED (each 32-byte sibling hash concatenated
 *  leaf→root) so it feeds straight into `verifyLogInclusion`. */
export interface InclusionProof {
  /** 0-based leaf index of the proven entry. */
  readonly idx: bigint;
  /** The entry's canonical leaf bytes (informational; the verify path
   *  binds the decomposed `(account_key, pubkey, version)` instead). */
  readonly leaf: Uint8Array;
  /** Flattened audit path: sibling node-hashes concatenated leaf→root,
   *  length always a multiple of 32. */
  readonly auditPath: Uint8Array;
  /** Tree size the proof was generated against. */
  readonly logSize: bigint;
}

/** Decoded `GET /transparency/announcement/:account_key/:generation`
 *  (ADR-0101 section 10): the newest seal announcement leaf the log holds
 *  for the account at that generation. */
export interface AnnouncementLeafEntry {
  /** 0-based leaf index; its audit path is `fetchProof(idx)`. */
  readonly idx: bigint;
  readonly keyGeneration: bigint;
  readonly seq: bigint;
  /** The announcement bytes the leaf commits by hash. */
  readonly announcement: Uint8Array;
}

/** Network-level failure (DNS, TLS, no route, …). Distinct from a
 *  parse failure so `verify.ts` can map both to `"unreachable"`. */
export class TransparencyNetworkError extends Error {
  override name = "TransparencyNetworkError";
  constructor(public readonly endpoint: string, cause: unknown) {
    super(`transparency ${endpoint} network error: ${String(cause)}`);
  }
}

/** Server returned a non-2xx (and not the well-known 404) status, or a
 *  body that did not decode to the expected shape. */
export class TransparencyHttpError extends Error {
  override name = "TransparencyHttpError";
  constructor(
    public readonly status: number,
    public readonly endpoint: string,
    public readonly detail: string,
  ) {
    super(`transparency ${endpoint} → ${status}: ${detail}`);
  }
}

/** Wire shape of `GET /transparency/root` (matches the Rust `RootRes`). */
interface RootResWire {
  tree_root_b64: string;
  log_size: number;
  timestamp_unix: number;
  signature_b64: string;
  /** ADR-0039 (S3): base64 witness co-signatures, a JSON array — OMITTED when
   *  the server has no cosigs. Replaces the pre-ADR-0039 singular
   *  `witness_cosig_b64`. */
  witness_cosigs_b64?: string[];
}

/** Wire shape of `GET /transparency/announcement/:key/:generation` (Rust
 *  `AnnouncementRes`). */
interface AnnouncementResWire {
  idx: number;
  key_generation: number;
  seq: number;
  announcement_b64: string;
}

/** Wire shape of `GET /transparency/latest/:key` (Rust `LatestRes`). */
interface LatestResWire {
  idx: number;
  version: number;
  identity_pubkey_b64: string;
  is_reset: boolean;
  /** G-30 #2 (ADR-0019): base64 of the entry's leaf authorisation, or
   *  absent for a pre-Sprint-2 / unauthenticated row. */
  leaf_auth_b64?: string;
  /** ADR-0036 (G-30 finding 2): base64 of the entry's auth-attested
   *  genesis attestation, or absent for a rotation / a pre-ADR-0036 row. */
  genesis_attest_b64?: string;
}

/** Wire shape of `GET /transparency/proof/:idx` (Rust `ProofRes`). */
interface ProofResWire {
  idx: number;
  leaf_b64: string;
  audit_path: string[];
  log_size: number;
}

export class TransparencyClient {
  constructor(public readonly baseUrl: string) {}

  /**
   * Build a client from the build-time config, or return `null` when
   * the transparency integration is OFF (URL or operator-key pin
   * unset). The caller skips the whole log check on `null`.
   */
  static forOptionalConfig(): TransparencyClient | null {
    const base = transparencyBaseUrl();
    const operator = pinnedOperatorKey();
    if (base === null || operator === null) return null;
    return new TransparencyClient(base);
  }

  /** `GET /transparency/root`. */
  async fetchRoot(): Promise<SignedRoot> {
    const wire = await this.getJson<RootResWire>("/transparency/root");
    return {
      treeRoot: base64Decode(wire.tree_root_b64),
      logSize: BigInt(wire.log_size),
      timestampUnix: BigInt(wire.timestamp_unix),
      signature: base64Decode(wire.signature_b64),
      witnessCosigs: Array.isArray(wire.witness_cosigs_b64)
        ? wire.witness_cosigs_b64
            .filter((s) => typeof s === "string" && s.length > 0)
            .map(base64Decode)
        : [],
    };
  }

  /**
   * `GET /transparency/latest/:account_key`. The 32-byte account-key
   * is encoded URL-safe-no-pad for the path segment (the server
   * accepts either alphabet). Returns `null` on 404 — the account is
   * not yet in the log.
   */
  async fetchLatest(accountKey: Uint8Array): Promise<LatestEntry | null> {
    const slug = base64UrlEncodeNoPad(accountKey);
    const endpoint = `/transparency/latest/${slug}`;
    const wire = await this.getJsonOr404<LatestResWire>(endpoint);
    if (wire === null) return null;
    return {
      idx: BigInt(wire.idx),
      version: BigInt(wire.version),
      identityPubkey: base64Decode(wire.identity_pubkey_b64),
      isReset: wire.is_reset,
      leafAuth:
        wire.leaf_auth_b64 !== undefined && wire.leaf_auth_b64.length > 0
          ? base64Decode(wire.leaf_auth_b64)
          : null,
      genesisAttest:
        wire.genesis_attest_b64 !== undefined &&
        wire.genesis_attest_b64.length > 0
          ? base64Decode(wire.genesis_attest_b64)
          : null,
    };
  }

  /**
   * `GET /transparency/announcement/:account_key/:generation` (ADR-0101
   * section 10): the newest seal announcement leaf of that account and
   * generation, or `null` on 404 (the account logged none at that
   * generation). The account key rides URL-safe-no-pad like `fetchLatest`.
   */
  async fetchAnnouncement(
    accountKey: Uint8Array,
    generation: bigint,
  ): Promise<AnnouncementLeafEntry | null> {
    const slug = base64UrlEncodeNoPad(accountKey);
    const endpoint = `/transparency/announcement/${slug}/${generation.toString()}`;
    const wire = await this.getJsonOr404<AnnouncementResWire>(endpoint);
    if (wire === null) return null;
    if (
      typeof wire.idx !== "number" ||
      typeof wire.key_generation !== "number" ||
      typeof wire.seq !== "number" ||
      typeof wire.announcement_b64 !== "string"
    ) {
      throw new TransparencyHttpError(200, endpoint, "not an announcement body");
    }
    return {
      idx: BigInt(wire.idx),
      keyGeneration: BigInt(wire.key_generation),
      seq: BigInt(wire.seq),
      announcement: base64Decode(wire.announcement_b64),
    };
  }

  /** `GET /transparency/proof/:idx`. */
  async fetchProof(idx: bigint): Promise<InclusionProof> {
    const endpoint = `/transparency/proof/${idx.toString()}`;
    const wire = await this.getJson<ProofResWire>(endpoint);
    // Flatten the audit path: each entry is a base64 32-byte sibling
    // hash; concatenate them so the wasm verifier reads a single
    // `Uint8Array` whose length is a multiple of 32.
    const hashes = wire.audit_path.map(base64Decode);
    const flat = new Uint8Array(hashes.length * 32);
    let off = 0;
    for (const h of hashes) {
      flat.set(h, off);
      off += h.length;
    }
    return {
      idx: BigInt(wire.idx),
      leaf: base64Decode(wire.leaf_b64),
      auditPath: flat,
      logSize: BigInt(wire.log_size),
    };
  }

  // -----------------------------------------------------------------
  // Internals
  // -----------------------------------------------------------------

  private async getJson<T>(endpoint: string): Promise<T> {
    const result = await this.getJsonOr404<T>(endpoint);
    if (result === null) {
      throw new TransparencyHttpError(404, endpoint, "unexpected 404");
    }
    return result;
  }

  /** GET + JSON, folding the well-known 404 into `null`. Any other
   *  non-2xx (or a JSON-parse failure) throws `TransparencyHttpError`;
   *  a transport failure throws `TransparencyNetworkError`. */
  private async getJsonOr404<T>(endpoint: string): Promise<T | null> {
    const url = `${this.baseUrl}${endpoint}`;
    let response: Response;
    try {
      response = await fetch(url, { method: "GET" });
    } catch (err) {
      throw new TransparencyNetworkError(endpoint, err);
    }
    if (response.status === 404) return null;
    if (!response.ok) {
      throw new TransparencyHttpError(
        response.status,
        endpoint,
        await safeReadBody(response),
      );
    }
    try {
      return (await response.json()) as T;
    } catch (err) {
      throw new TransparencyHttpError(
        response.status,
        endpoint,
        `body is not valid JSON: ${String(err)}`,
      );
    }
  }
}

async function safeReadBody(response: Response): Promise<string> {
  try {
    const text = await response.text();
    return text.length > 256 ? `${text.slice(0, 256)}…` : text;
  } catch {
    return "<unreadable body>";
  }
}
