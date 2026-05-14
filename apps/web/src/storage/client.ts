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
 *     200 on hit, 404 on miss.
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
 * No auth on this service in Phase 1 — it's a "public CDN for E2EE
 * blobs", content addressing + server-side hash verification gives
 * the integrity property without per-request auth. Per-IP rate
 * limit lives in nginx. Owner tracking + quota land in Phase 2.
 *
 * Errors mirror the auth + discovery client surface:
 *
 *   * [`StorageClientArgError`] — local input validation (wrong
 *     length blob_id, oversize body) caught before any round-trip.
 *   * [`StorageHttpError`] — server returned non-2xx; carries the
 *     status code + endpoint so the UI can branch on 404 / 400.
 *   * [`StorageNetworkError`] — DNS / TLS / transport failure.
 */

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

export class StorageClient {
  constructor(public readonly baseUrl: string = DEFAULT_BASE_URL) {}

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
    if (blobId.length !== BLOB_ID_LEN) {
      throw new StorageClientArgError(
        `blobId must be ${BLOB_ID_LEN} bytes (got ${blobId.length})`,
      );
    }
    if (ciphertext.length === 0) {
      throw new StorageClientArgError("ciphertext must be non-empty");
    }
    if (ciphertext.length > MAX_BLOB_BYTES) {
      throw new StorageClientArgError(
        `ciphertext exceeds the ${MAX_BLOB_BYTES}-byte ceiling ` +
          `(got ${ciphertext.length} bytes)`,
      );
    }

    const hex = blobIdHex(blobId);
    const endpoint = `/blob/${hex}`;
    const url = `${this.baseUrl}${endpoint}`;
    let response: Response;
    try {
      response = await fetch(url, {
        method: "PUT",
        headers: { "content-type": "application/octet-stream" },
        // `fetch` accepts ArrayBufferView; pass the bytes through
        // as-is so the body is the literal ciphertext, no JSON
        // wrapping, no base64 inflation. `.slice()` reifies the
        // backing buffer as a fresh `ArrayBuffer` (vs the generic
        // `ArrayBufferLike`) so it satisfies TS's `BodyInit`
        // type — same trick as `ds/client.ts` for the publish
        // path.
        body: ciphertext.slice(),
      });
    } catch (err) {
      throw new StorageNetworkError(endpoint, err);
    }
    // 200 OK = idempotent re-upload, 201 Created = first upload.
    // Both are success.
    if (response.status === 200 || response.status === 201) {
      // Drain the (empty) body so the connection can be reused.
      await response.body?.cancel();
      return;
    }
    const text = await safeReadBody(response);
    throw new StorageHttpError(response.status, endpoint, text);
  }

  /**
   * `GET /blob/<hex>` — fetch ciphertext. Returns the raw bytes on
   * 200, `null` on 404 (the typed not-found path so the UI can
   * surface "attachment missing" without an exception). Throws on
   * any other non-2xx.
   *
   * @param blobId  32-byte SHA-256 of the ciphertext.
   */
  async getBlob(blobId: Uint8Array): Promise<Uint8Array | null> {
    if (blobId.length !== BLOB_ID_LEN) {
      throw new StorageClientArgError(
        `blobId must be ${BLOB_ID_LEN} bytes (got ${blobId.length})`,
      );
    }
    const hex = blobIdHex(blobId);
    const endpoint = `/blob/${hex}`;
    const url = `${this.baseUrl}${endpoint}`;
    let response: Response;
    try {
      response = await fetch(url, { method: "GET" });
    } catch (err) {
      throw new StorageNetworkError(endpoint, err);
    }
    if (response.status === 404) return null;
    if (!response.ok) {
      const text = await safeReadBody(response);
      throw new StorageHttpError(response.status, endpoint, text);
    }
    const buf = await response.arrayBuffer();
    return new Uint8Array(buf);
  }

  /**
   * `HEAD /blob/<hex>` — return the blob's `Content-Length` if it
   * exists, `null` if not. Used by the UI to size a download
   * indicator before issuing the GET.
   *
   * Throws on any non-200 / non-404 — a 5xx here means the storage
   * service is sick, not that the blob is missing.
   */
  async headBlob(blobId: Uint8Array): Promise<number | null> {
    if (blobId.length !== BLOB_ID_LEN) {
      throw new StorageClientArgError(
        `blobId must be ${BLOB_ID_LEN} bytes (got ${blobId.length})`,
      );
    }
    const hex = blobIdHex(blobId);
    const endpoint = `/blob/${hex}`;
    const url = `${this.baseUrl}${endpoint}`;
    let response: Response;
    try {
      response = await fetch(url, { method: "HEAD" });
    } catch (err) {
      throw new StorageNetworkError(endpoint, err);
    }
    if (response.status === 404) return null;
    if (!response.ok) {
      const text = await safeReadBody(response);
      throw new StorageHttpError(response.status, endpoint, text);
    }
    const cl = response.headers.get("content-length");
    if (cl === null) return null;
    const parsed = Number.parseInt(cl, 10);
    return Number.isFinite(parsed) && parsed >= 0 ? parsed : null;
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
