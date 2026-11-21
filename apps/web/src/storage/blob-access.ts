// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * ADR-0065 sections 5 and 6 — what a blob request carries, and what a
 * refusal means.
 *
 *   * A blob pass: one `blob-upload` or `blob-read` anonymous token buys an
 *     hour-long pass with a budget (`POST /blob/pass/{upload,read}`). One of
 *     each is cached per device in memory and taken again before it expires,
 *     or when the server refuses it (a pass lives in the server's memory, so
 *     a restart can end it early).
 *   * A delete key: every upload carries the public half of a key derived
 *     from the account seed and the blob id, so only this account can later
 *     delete the blob (`DELETE /blob/{hex}` signed with it).
 *
 * The cache and the refusal mapping are pure and tested; the account-bound
 * ports (`VaultKeys` from the account identity, the token spend) are
 * installed by `BlobAccessRuntime` and read through {@link blobAccess}.
 */

/** The two pass kinds. */
export type BlobPassKind = "upload" | "read";

/** The header a pass travels in. */
export const BLOB_PASS_HEADER = "x-lakko-blob-pass";
/** The header an upload's delete key travels in (base64url, 32 bytes). */
export const BLOB_DELETE_KEY_HEADER = "x-lakko-blob-delete-key";

/** A pass is taken again this long before the server's expiry, so a request
 *  never leaves with one about to lapse. Chosen. */
export const PASS_RENEW_MARGIN_MS = 120_000;

/** A pass as the server issued it. */
export interface BlobPass {
  readonly pass: string;
  readonly expiresAtMs: number;
}

/**
 * The daily or hourly allowance for files is used up (UX 4.2): an upload
 * refused for its budget, a download refused for its budget, or no token to
 * buy a new pass with.
 */
export class StorageLimitError extends Error {
  override name = "StorageLimitError";
  constructor(public readonly kind: BlobPassKind) {
    super(`storage: ${kind} limit reached`);
  }
}

/**
 * docs/ux/storage-full.md — the server refused an upload because its disk is
 * too full (`507 {"error":"storage-full"}`). Not retried: trying again is the
 * person's choice (or, for background uploads, the next scheduled attempt).
 */
export class StorageFullError extends Error {
  override name = "StorageFullError";
  constructor() {
    super("storage: the server has no room for new files");
  }
}

/** Whether an error, or one of its causes, is {@link StorageFullError}
 *  (matched by name, since the error crosses module lines). */
export function isStorageFull(err: unknown): boolean {
  let current: unknown = err;
  for (let depth = 0; depth < 8 && typeof current === "object" && current !== null; depth += 1) {
    const r = current as { name?: unknown; cause?: unknown };
    if (r.name === "StorageFullError") return true;
    current = r.cause;
  }
  return false;
}

/** What a refusal of a blob request means for the caller. */
export type BlobRefusal =
  /** The pass is no good (missing, unknown after a restart, expired): take a
   *  new one once and try again. */
  | "renew-pass"
  /** The pass's budget is spent: take a new one once; if that is refused
   *  too, the limit is reached. */
  | "budget"
  /** 404: the blob is gone (UX 4.1, no Retry). */
  | "gone"
  /** 507 `storage-full`: the server has no room for new files
   *  (docs/ux/storage-full.md). Not retried. */
  | "full"
  /** Anything else: the generic failure, with Try again. */
  | "failed";

/** The `{"error": code}` of a storage refusal, or `null` (an nginx page, an
 *  empty body). */
export function storageErrorCode(body: string): string | null {
  try {
    const code = (JSON.parse(body) as { error?: unknown }).error;
    return typeof code === "string" ? code : null;
  } catch {
    return null;
  }
}

/** Map one refusal to what the caller does. */
export function blobRefusalOf(status: number, code: string | null): BlobRefusal {
  if (status === 404) return "gone";
  if (
    status === 401 &&
    (code === "blob-pass-required" || code === "blob-pass-invalid" || code === "blob-pass-expired")
  ) {
    return "renew-pass";
  }
  if (status === 429 && (code === "blob-upload-budget-exhausted" || code === "blob-read-budget-exhausted")) {
    return "budget";
  }
  if (status === 507 && code === "storage-full") return "full";
  return "failed";
}

/**
 * One cached pass per kind. `fetchPass` buys a new one (and throws
 * {@link StorageLimitError} when no token can be had); concurrent requests
 * share one purchase.
 */
export class BlobPassCache {
  private readonly held = new Map<BlobPassKind, BlobPass>();
  private readonly inFlight = new Map<BlobPassKind, Promise<BlobPass>>();

  constructor(
    private readonly fetchPass: (kind: BlobPassKind) => Promise<BlobPass>,
    private readonly now: () => number = () => Date.now(),
  ) {}

  /** A pass of `kind` good for at least {@link PASS_RENEW_MARGIN_MS}. */
  async get(kind: BlobPassKind): Promise<string> {
    const held = this.held.get(kind);
    if (held !== undefined && held.expiresAtMs - this.now() > PASS_RENEW_MARGIN_MS) return held.pass;
    let pending = this.inFlight.get(kind);
    if (pending === undefined) {
      pending = this.fetchPass(kind).finally(() => this.inFlight.delete(kind));
      this.inFlight.set(kind, pending);
    }
    const fresh = await pending;
    this.held.set(kind, fresh);
    return fresh.pass;
  }

  /** The server refused `pass`: drop it (only if it is still the one held,
   *  so a refusal of an older pass does not throw away a newer one). */
  invalidate(kind: BlobPassKind, pass: string): void {
    if (this.held.get(kind)?.pass === pass) this.held.delete(kind);
  }

  /** Forget both (sign-out). */
  clear(): void {
    this.held.clear();
  }
}

/** Parse `POST /blob/pass/{kind}`'s 200 body. Throws on anything else. */
export function parseBlobPass(json: unknown, kind: BlobPassKind): BlobPass {
  const o = json as { pass?: unknown; kind?: unknown; expires_at_unix_secs?: unknown };
  if (typeof o?.pass !== "string" || o.pass.length === 0 || o.kind !== kind) {
    throw new Error("blob pass: malformed answer");
  }
  const secs = o.expires_at_unix_secs;
  if (typeof secs !== "number" || !Number.isFinite(secs)) throw new Error("blob pass: no expiry");
  return { pass: o.pass, expiresAtMs: secs * 1000 };
}

/** The account-bound ports a blob request needs. */
export interface BlobAccess {
  readonly passes: BlobPassCache;
  /** The account these ports belong to (its owed deletes are kept under
   *  it); absent in tests. */
  readonly accountId?: string;
  /** The 32-byte public delete key an upload of `blobId` carries. */
  deleteKey(blobId: Uint8Array): Promise<Uint8Array>;
  /** The `x-lakko-storage-proof` header (name, value) for
   *  `DELETE /blob/<hex>`. */
  signDelete(blobId: Uint8Array, path: string): Promise<readonly [string, string]>;
}

let installed: BlobAccess | null = null;

/** Installed by `BlobAccessRuntime` for the signed-in account; `null` on
 *  sign-out. */
export function installBlobAccess(access: BlobAccess | null): void {
  installed?.passes.clear();
  installed = access;
}

/** The ports for the signed-in account, or `null` when none is installed
 *  (signed out, or a test). */
export function blobAccess(): BlobAccess | null {
  return installed;
}

/** Base64url without padding, the delete-key header form. */
export function base64Url(bytes: Uint8Array): string {
  let bin = "";
  for (const b of bytes) bin += String.fromCharCode(b);
  return btoa(bin).replace(/\+/g, "-").replace(/\//g, "_").replace(/=+$/, "");
}
