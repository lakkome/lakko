// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * `StorageClient` — fetch-based client for `lakko-svc-storage`.
 *
 * Content-addressable blob proxy. The wire is intentionally minimal:
 *
 *   * `PUT  /blob/<sha256_hex>` — body = ciphertext, idempotent.
 *     Server verifies that `SHA-256(body) === <sha256_hex>` and
 *     stores at that key. 201 on first upload, 200 on re-upload
 *     of identical bytes, 400 on hash mismatch.
 *   * `GET  /blob/<sha256_hex>` — body = ciphertext, streamed.
 *     200 on hit, 404 when the blob is gone.
 *   * `HEAD /blob/<sha256_hex>` — same, with `Content-Length` so
 *     callers can size a download before pulling.
 *
 * Phase 1.19c1: thin HTTP shim around the storage service. The
 * `blob_id` is **always** the raw 32-byte SHA-256 of the *ciphertext*
 * — exactly what `lakko-attachments::encrypt_attachment` returns via
 * the wasm-bindgen `EncryptedAttachment.blobId` getter. Callers do
 * not compute the hash themselves; they hand the bytes from the
 * encryption layer straight through.
 *
 * Blob requests carry no account (ADR-0065): an upload carries an upload
 * pass and the blob's public delete key, a download or keep a read pass
 * (each pass bought with one anonymous token, `blob-access.ts`), and a
 * delete is signed with the blob's delete key. Content addressing plus the
 * server's hash check give integrity. nginx adds a per-address limit in
 * front of the blob routes; its refusal (a 429 with no JSON code) reads as
 * the generic failure.
 *
 * Errors mirror the auth + discovery client surface:
 *
 *   * [`StorageClientArgError`] — local input validation (wrong
 *     length blob_id, oversize body) caught before any round-trip.
 *   * [`StorageHttpError`] — server returned non-2xx; carries the
 *     status code + endpoint so the UI can branch on 404 / 400.
 *   * [`StorageNetworkError`] — DNS / TLS / transport failure.
 */
import { sessionFetch } from "../auth/session-fetch";
import {
  BLOB_DELETE_KEY_HEADER,
  BLOB_PASS_HEADER,
  StorageLimitError,
  base64Url,
  blobAccess,
  blobRefusalOf,
  storageErrorCode,
  type BlobPassKind,
} from "./blob-access";

export { StorageLimitError } from "./blob-access";

/** At most this many ids per `POST /blob/keep` (the server's bound). */
export const MAX_KEEP_IDS = 256;

/** One blob request's answer, as `withPass` judges it. */
type PassAnswer = { readonly ok: true } | { readonly ok: false; readonly status: number; readonly body: string };

function checkBlobId(blobId: Uint8Array): void {
  if (blobId.length !== BLOB_ID_LEN) {
    throw new StorageClientArgError(`blobId must be ${BLOB_ID_LEN} bytes (got ${blobId.length})`);
  }
}

/** Default base URL — Vite dev proxy (`/api/storage` →
 *  `127.0.0.1:8083`). Production nginx routes the same path. */
export const DEFAULT_BASE_URL = "/api/storage";

/** Raw SHA-256 blob id length. */
const BLOB_ID_LEN = 32;

/** Per-blob ceiling. Mirrors `MAX_BLOB_BYTES` in
 *  `services/storage/src/main.rs` and the
 *  `MAX_ATTACHMENT_BYTES` constant in `lakko-attachments` — surface
 *  here so a misbehaving caller fails fast without spending an
 *  unencrypted-body round-trip. */
const MAX_BLOB_BYTES = 50 * 1024 * 1024;

/** Programmer error — wrong-length blob id, empty body, oversize
 *  body. Distinct from HTTP / network errors so the UI can surface
 *  a developer-visible message rather than retrying. */
export class StorageClientArgError extends Error {
  override name = "StorageClientArgError";
}

/** Server returned a non-2xx status. Carries the original status +
 *  endpoint so the UI can branch on 400 (hash mismatch / malformed)
 *  vs 404 (not found) vs 413 (body too large) vs 5xx. */
export class StorageHttpError extends Error {
  override name = "StorageHttpError";
  constructor(
    public readonly status: number,
    public readonly endpoint: string,
    public readonly body: string,
  ) {
    super(`storage ${endpoint} → ${status}: ${body}`);
  }
}

/** Network-level failure (DNS, TLS, transport). */
export class StorageNetworkError extends Error {
  override name = "StorageNetworkError";
  constructor(public readonly endpoint: string, cause: unknown) {
    super(`storage ${endpoint} network error: ${String(cause)}`);
  }
}

/** Hex-encode the 32-byte blob id to the lowercase form the server
 *  expects (`validate_hex` in `services/storage/src/blob.rs`
 *  rejects uppercase to keep a single canonical URL form). */
function blobIdHex(blobId: Uint8Array): string {
  let out = "";
  for (const b of blobId) {
    out += b.toString(16).padStart(2, "0");
  }
  return out;
}

/** ADR-0087 section 13: the upload header naming a blob's own idle life. */
export const BLOB_IDLE_DAYS_HEADER = "x-lakko-blob-idle-days";

/** ADR-0087 section 13: a channel file lives 45 days without a read (30 days
 *  of posts and 15 of margin, chosen in the ADR, not measured). */
export const CHANNEL_FILE_IDLE_DAYS = 45;

export class StorageClient {
  /** Bearer token attached to per-account backup requests. Set
   *  via [`setSessionToken`] post-construction. `null` is OK for
   *  the content-addressed blob routes (they're auth-free); a
   *  null value on a backup-route call surfaces as a 401 from
   *  the server. */
  private sessionToken: string | null = null;

  constructor(
    public readonly baseUrl: string = DEFAULT_BASE_URL,
    /** ADR-0087 section 13: the idle life an upload asks for, in days
     *  (`x-lakko-blob-idle-days`, 1..=255); absent = the server's default.
     *  A channel post's files ask for {@link CHANNEL_FILE_IDLE_DAYS}. */
    public readonly idleDays?: number,
  ) {}

  /** Bind / rebind the bearer token used for the
   *  `PUT/GET/DELETE /backup` routes (Phase 2a1c). Caller drives
   *  this from the `AuthContext` session-token signal — same
   *  pattern the discovery / ds clients use. */
  setSessionToken(token: string | null): void {
    this.sessionToken = token;
  }

  private authHeaders(): Record<string, string> {
    return this.sessionToken === null
      ? {}
      : { authorization: `Bearer ${this.sessionToken}` };
  }

  /**
   * `PUT /blob/<hex>` — upload ciphertext. Idempotent: re-PUT of
   * the same hash overwrites with byte-identical content (which is
   * a no-op by the meaning of content addressing).
   *
   * The server hashes the body and compares against the URL slug;
   * a mismatch surfaces as `StorageHttpError(400, …)`. This lets
   * the caller detect a buggy local encryption path before a
   * recipient ever sees a broken blob.
   *
   * @param blobId      32-byte SHA-256 of the ciphertext (the
   *                    `blob_id` returned by
   *                    `EncryptedAttachment.blobId` from
   *                    `lakko-attachments`).
   * @param ciphertext  The bytes to store. Must hash to `blobId`.
   *
   * @throws `StorageClientArgError` on wrong-length / oversize
   *         inputs, `StorageHttpError` on server rejection,
   *         `StorageNetworkError` on transport failure.
   */
  async putBlob(blobId: Uint8Array, ciphertext: Uint8Array): Promise<void> {
    await this.putBlobWithProgress(blobId, ciphertext, undefined);
  }

  /**
   * ADR-0047 — [`putBlob`] with upload progress, for the timeline's
   * determinate uploading ring. `fetch` exposes no upload progress, so this
   * uses `XMLHttpRequest` where the environment has it and `fetch` (reporting
   * only completion) where it does not. Same URL, same body, same success
   * statuses, same error types.
   *
   * ADR-0065: the request carries an upload pass and the blob's public
   * delete key. A refused pass is taken again once; a budget refused twice
   * is {@link StorageLimitError} `upload` (UX 4.2).
   */
  async putBlobWithProgress(
    blobId: Uint8Array,
    ciphertext: Uint8Array,
    onProgress?: (fraction: number) => void,
  ): Promise<void> {
    checkBlobId(blobId);
    if (ciphertext.length === 0) {
      throw new StorageClientArgError("ciphertext must be non-empty");
    }
    if (ciphertext.length > MAX_BLOB_BYTES) {
      throw new StorageClientArgError(
        `ciphertext exceeds the ${MAX_BLOB_BYTES}-byte ceiling ` +
          `(got ${ciphertext.length} bytes)`,
      );
    }
    const endpoint = `/blob/${blobIdHex(blobId)}`;
    const url = `${this.baseUrl}${endpoint}`;
    const access = blobAccess();
    const deleteKey = access === null ? null : base64Url(await access.deleteKey(blobId));
    await this.withPass("upload", endpoint, async (pass) => {
      const headers: Record<string, string> = { "content-type": "application/octet-stream" };
      if (pass !== null) headers[BLOB_PASS_HEADER] = pass;
      if (deleteKey !== null) headers[BLOB_DELETE_KEY_HEADER] = deleteKey;
      if (this.idleDays !== undefined) headers[BLOB_IDLE_DAYS_HEADER] = String(this.idleDays);
      if (onProgress === undefined || typeof XMLHttpRequest === "undefined") {
        let response: Response;
        try {
          // `.slice()` reifies the backing buffer as a fresh `ArrayBuffer`
          // so it satisfies TS's `BodyInit` type.
          response = await fetch(url, { method: "PUT", headers, body: ciphertext.slice() });
        } catch (err) {
          throw new StorageNetworkError(endpoint, err);
        }
        if (response.status === 200 || response.status === 201) {
          await response.body?.cancel();
          onProgress?.(1);
          return { ok: true };
        }
        return { ok: false, status: response.status, body: await safeReadBody(response) };
      }
      return new Promise<PassAnswer>((resolve, reject) => {
        const xhr = new XMLHttpRequest();
        xhr.open("PUT", url);
        for (const [k, v] of Object.entries(headers)) xhr.setRequestHeader(k, v);
        xhr.upload.onprogress = (e) => {
          if (e.lengthComputable && e.total > 0) {
            onProgress(Math.min(1, e.loaded / e.total));
          }
        };
        xhr.onload = () => {
          if (xhr.status === 200 || xhr.status === 201) {
            onProgress(1);
            resolve({ ok: true });
          } else {
            resolve({ ok: false, status: xhr.status, body: String(xhr.responseText ?? "") });
          }
        };
        xhr.onerror = () => reject(new StorageNetworkError(endpoint, "xhr error"));
        xhr.onabort = () => reject(new StorageNetworkError(endpoint, "xhr aborted"));
        // The bytes, never a Blob: WebKit hands a Blob to a custom scheme as a
        // stream the desktop host reads empty (ADR-0085 section 14).
        xhr.send(ciphertext.slice());
      });
    });
  }

  /**
   * `GET /blob/<hex>` — fetch ciphertext. Returns the raw bytes on
   * 200, `null` on 404 (the blob is gone, UX 4.1). Throws
   * {@link StorageLimitError} `read` when opening files is limited (UX 4.2)
   * and `StorageHttpError` on any other refusal.
   *
   * @param blobId  32-byte SHA-256 of the ciphertext.
   */
  async getBlob(blobId: Uint8Array): Promise<Uint8Array | null> {
    checkBlobId(blobId);
    const endpoint = `/blob/${blobIdHex(blobId)}`;
    const url = `${this.baseUrl}${endpoint}`;
    let bytes: Uint8Array | null = null;
    const found = await this.withPass("read", endpoint, async (pass) => {
      let response: Response;
      try {
        response = await fetch(url, { method: "GET", headers: pass === null ? {} : { [BLOB_PASS_HEADER]: pass } });
      } catch (err) {
        throw new StorageNetworkError(endpoint, err);
      }
      if (!response.ok) return { ok: false, status: response.status, body: await safeReadBody(response) };
      bytes = new Uint8Array(await response.arrayBuffer());
      return { ok: true };
    });
    return found ? bytes : null;
  }

  /**
   * `HEAD /blob/<hex>` — return the blob's `Content-Length` if it
   * exists, `null` if not. Counts as one download of the read pass.
   */
  async headBlob(blobId: Uint8Array): Promise<number | null> {
    checkBlobId(blobId);
    const endpoint = `/blob/${blobIdHex(blobId)}`;
    const url = `${this.baseUrl}${endpoint}`;
    let length: number | null = null;
    const found = await this.withPass("read", endpoint, async (pass) => {
      let response: Response;
      try {
        response = await fetch(url, { method: "HEAD", headers: pass === null ? {} : { [BLOB_PASS_HEADER]: pass } });
      } catch (err) {
        throw new StorageNetworkError(endpoint, err);
      }
      if (!response.ok) return { ok: false, status: response.status, body: await safeReadBody(response) };
      const cl = response.headers.get("content-length");
      const parsed = cl === null ? Number.NaN : Number.parseInt(cl, 10);
      length = Number.isFinite(parsed) && parsed >= 0 ? parsed : null;
      return { ok: true };
    });
    return found ? length : null;
  }

  /**
   * ADR-0065 section 3 — `POST /blob/keep`: mark up to 256 blobs as used
   * (pinned and saved messages, the own profile photo). The server answers
   * 204 whatever it holds.
   */
  async keepBlobs(blobIdsHex: readonly string[]): Promise<void> {
    if (blobIdsHex.length === 0) return;
    if (blobIdsHex.length > MAX_KEEP_IDS) {
      throw new StorageClientArgError(`at most ${MAX_KEEP_IDS} ids per keep`);
    }
    const endpoint = "/blob/keep";
    const url = `${this.baseUrl}${endpoint}`;
    const body = JSON.stringify({ ids: blobIdsHex });
    await this.withPass("read", endpoint, async (pass) => {
      let response: Response;
      try {
        response = await fetch(url, {
          method: "POST",
          headers: { "content-type": "application/json", ...(pass === null ? {} : { [BLOB_PASS_HEADER]: pass }) },
          body,
        });
      } catch (err) {
        throw new StorageNetworkError(endpoint, err);
      }
      if (response.ok) {
        await response.body?.cancel();
        return { ok: true };
      }
      return { ok: false, status: response.status, body: await safeReadBody(response) };
    });
  }

  /**
   * ADR-0065 section 6 — `DELETE /blob/<hex>`, signed with the blob's
   * delete key (no pass). Resolves on 204; throws `StorageHttpError` on any
   * other answer (the server answers every refusal 403
   * `blob-delete-refused`, which the owed list drops) and
   * `StorageNetworkError` offline.
   * Needs the installed account ports (`blobAccess`).
   */
  async deleteBlob(blobId: Uint8Array): Promise<void> {
    checkBlobId(blobId);
    const access = blobAccess();
    if (access === null) throw new StorageClientArgError("no account to delete a blob for");
    const endpoint = `/blob/${blobIdHex(blobId)}`;
    const [name, value] = await access.signDelete(blobId, endpoint);
    let response: Response;
    try {
      response = await fetch(`${this.baseUrl}${endpoint}`, { method: "DELETE", headers: { [name]: value } });
    } catch (err) {
      throw new StorageNetworkError(`${endpoint} (DELETE)`, err);
    }
    if (response.ok) {
      await response.body?.cancel();
      return;
    }
    throw new StorageHttpError(response.status, `${endpoint} (DELETE)`, await safeReadBody(response));
  }

  /**
   * Run one blob request with a pass of `kind`: a refused pass is taken
   * again once, a budget refused twice is the limit. Resolves `true` on
   * success and `false` on 404 (gone); throws on everything else. With no
   * account ports installed the request goes without a pass.
   */
  private async withPass(
    kind: BlobPassKind,
    endpoint: string,
    send: (pass: string | null) => Promise<PassAnswer>,
  ): Promise<boolean> {
    const access = blobAccess();
    for (let attempt = 0; ; attempt += 1) {
      const pass = access === null ? null : await access.passes.get(kind);
      const answer = await send(pass);
      if (answer.ok) return true;
      const refusal = blobRefusalOf(answer.status, storageErrorCode(answer.body));
      if (refusal === "gone") return false;
      if ((refusal === "renew-pass" || refusal === "budget") && access !== null && pass !== null) {
        access.passes.invalidate(kind, pass);
        if (attempt === 0) continue;
        if (refusal === "budget") throw new StorageLimitError(kind);
      }
      throw new StorageHttpError(answer.status, endpoint, answer.body);
    }
  }

  // ---------------------------------------------------------------
  // Per-account backup blob (Phase 2a1c)
  // ---------------------------------------------------------------

  /**
   * `PUT /backup` — overwrite this session's backup blob. Body
   * is the wire bytes from `lakko-backup`'s envelope `seal()`
   * (the wasm-bindgen `sealBackup` exposes the same primitive
   * to JS). The server identifies the owning account via the
   * session token — there is no account_id in the URL or body,
   * so a stolen token cannot scribble over a different account.
   *
   * 201 on first upload, 200 on overwrite. Both surface as a
   * resolved `void`; the caller doesn't need to distinguish.
   *
   * @throws `StorageHttpError` on 4xx / 5xx,
   *         `StorageNetworkError` on transport failure.
   */
  async putBackup(envelopeBytes: Uint8Array): Promise<void> {
    const endpoint = "/backup";
    const url = `${this.baseUrl}${endpoint}`;
    let response: Response;
    try {
      response = await sessionFetch(url, {
        method: "PUT",
        headers: {
          "content-type": "application/octet-stream",
          ...this.authHeaders(),
        },
        body: envelopeBytes.slice(),
      });
    } catch (err) {
      throw new StorageNetworkError(`${endpoint} (PUT)`, err);
    }
    if (response.ok) {
      await response.body?.cancel();
      return;
    }
    const text = await safeReadBody(response);
    throw new StorageHttpError(response.status, `${endpoint} (PUT)`, text);
  }

  /**
   * `GET /backup` — fetch this session's backup blob, or `null`
   * if none has ever been uploaded.
   *
   * @throws `StorageHttpError` on non-200 / non-404,
   *         `StorageNetworkError` on transport failure.
   */
  async getBackup(): Promise<Uint8Array | null> {
    const endpoint = "/backup";
    const url = `${this.baseUrl}${endpoint}`;
    let response: Response;
    try {
      response = await sessionFetch(url, {
        method: "GET",
        headers: this.authHeaders(),
      });
    } catch (err) {
      throw new StorageNetworkError(`${endpoint} (GET)`, err);
    }
    if (response.status === 404) return null;
    if (!response.ok) {
      const text = await safeReadBody(response);
      throw new StorageHttpError(response.status, `${endpoint} (GET)`, text);
    }
    const buf = await response.arrayBuffer();
    return new Uint8Array(buf);
  }

  /**
   * `DELETE /backup` — remove this session's backup blob.
   * Idempotent — 204 on success regardless of whether a blob
   * existed.
   *
   * @throws `StorageHttpError` on 4xx / 5xx (NOT 404 — that's
   *         folded into success by the server),
   *         `StorageNetworkError` on transport failure.
   */
  async deleteBackup(): Promise<void> {
    const endpoint = "/backup";
    const url = `${this.baseUrl}${endpoint}`;
    let response: Response;
    try {
      response = await sessionFetch(url, {
        method: "DELETE",
        headers: this.authHeaders(),
      });
    } catch (err) {
      throw new StorageNetworkError(`${endpoint} (DELETE)`, err);
    }
    if (response.ok) {
      await response.body?.cancel();
      return;
    }
    const text = await safeReadBody(response);
    throw new StorageHttpError(response.status, `${endpoint} (DELETE)`, text);
  }
}

async function safeReadBody(response: Response): Promise<string> {
  try {
    const text = await response.text();
    return text.length > 512 ? `${text.slice(0, 512)}…` : text;
  } catch {
    return "<unreadable body>";
  }
}
