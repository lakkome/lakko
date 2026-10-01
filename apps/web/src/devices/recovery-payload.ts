// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * Recovery-payload wire module — ADR-0018 Part 2a, factor (a)
 * ("another-device" recovery), Path B (MLS exporter).
 *
 * After the add-device pairing completes, the scanner (existing,
 * authenticated) device transfers BOTH halves of recovery to the
 * fresh device over the rendezvous mailbox:
 *
 *   * **ACCESS** — a single-use 32-byte device-authorization
 *     `grant_token` minted by `POST /device/authorize/start`
 *     (bearer). The fresh device redeems it anonymously at
 *     `POST /device/authorize/redeem` for a session-token triple.
 *   * **IDENTITY** — the 64-byte AccountIdentity (BIP39 phrase)
 *     seed (ADR-0017), so the fresh device never types the phrase.
 *
 * ## Crypto (Path B — MLS exporter)
 *
 * Both devices sit at the same DeviceGroup epoch once the Welcome
 * is processed (the scanner committed the add; the joiner ratcheted
 * from the Welcome), so both derive the SAME 32-byte key via
 *
 * ```text
 *   key = deviceGroup.exportSecret(
 *           identity,
 *           "lakko/device-recovery/v1",   // RECOVERY_EXPORTER_LABEL
 *           rendezvous_pseudonym,          // 32-byte, THIS pairing session
 *           32,
 *         )
 * ```
 *
 * The scanner seals `grant_token(32) ‖ phrase_seed(64)` (96 bytes
 * fixed) with `sealWithKey(key, payload)` — ChaCha20-Poly1305,
 * wire `nonce(12) ‖ ciphertext+tag` — and the joiner opens it with
 * `openWithKey`. The context binding to the rendezvous pseudonym
 * scopes the key to THIS pairing session.
 *
 * ## Mailbox row format (DS opacity)
 *
 * The sealed blob rides the SAME rendezvous mailbox the Welcome
 * (row 1) and the optional history bundle (next row) use — no new
 * server surface. To keep the three row types distinguishable the
 * recovery row carries a plaintext ASCII magic prefix:
 *
 * ```text
 *   row = "lakko/recovery-payload/v1" ‖ sealed_blob
 * ```
 *
 * The magic marks only the row TYPE; everything sensitive is
 * inside the AEAD blob. The DS stores opaque bytes at a pseudonym
 * either way (invariant #2) — the phrase seed crosses the wire
 * only sealed under the exporter key, which the server cannot
 * derive (it has no DeviceGroup state). The unit tests pin that
 * the published row never contains the seed as a subsequence.
 */
import { DsClient, DsHttpError, DsNetworkError } from "../ds/client";

/** MLS exporter label for the per-pairing recovery key.
 *  Pinned ASCII; the `/v1` suffix is part of the wire contract.
 *  Keep in lockstep with the iOS / Android clients. */
export const RECOVERY_EXPORTER_LABEL = "lakko/device-recovery/v1";

/** Exporter output length — the ChaCha20-Poly1305 key size. */
export const RECOVERY_EXPORTER_KEY_LENGTH = 32;

/** Raw device-authorization grant token length
 *  (`services/auth/src/device_authorize.rs::GRANT_TOKEN_LEN`). */
export const RECOVERY_GRANT_TOKEN_LENGTH = 32;

/** AccountIdentity (BIP39 phrase) seed length (ADR-0017). */
export const RECOVERY_PHRASE_SEED_LENGTH = 64;

/** Fixed plaintext payload length: `grant(32) ‖ seed(64)`. */
export const RECOVERY_PAYLOAD_LENGTH =
  RECOVERY_GRANT_TOKEN_LENGTH + RECOVERY_PHRASE_SEED_LENGTH;

/** Plaintext ASCII magic prefix that tags a rendezvous-mailbox row
 *  as a recovery payload (vs the Welcome / history-bundle rows). */
const RECOVERY_ROW_MAGIC = new TextEncoder().encode(
  "lakko/recovery-payload/v1",
);

/** Server-side long-poll budget per request, in seconds — same
 *  rationale as `welcome-courier.ts`. */
const PER_REQUEST_LONG_POLL_SECS = 15;

/** Default soft timeout for the joiner-side recovery poll. The
 *  scanner publishes the row right after the Welcome + history
 *  bundle (one extra `/device/authorize/start` round-trip),
 *  so a short window suffices; absence (older scanner build, or
 *  the best-effort scanner half failed) is tolerated — the join
 *  completes without a recovery payload. */
const RECOVERY_POLL_TIMEOUT_MS = 15_000;

// ---- payload build / parse ---------------------------------------

/**
 * Scanner side: concatenate `grant_token(32) ‖ phrase_seed(64)`
 * into the fixed 96-byte plaintext payload. Strict length
 * validation — a wrong-size grant or seed is a caller bug that
 * must surface before anything is sealed or published.
 *
 * The returned buffer is a fresh copy; the caller should zeroize
 * it (and its inputs) once the sealed blob is built.
 */
export function buildRecoveryPayload(
  grantToken: Uint8Array,
  phraseSeed: Uint8Array,
): Uint8Array {
  if (grantToken.length !== RECOVERY_GRANT_TOKEN_LENGTH) {
    throw new Error(
      `grant token must be ${RECOVERY_GRANT_TOKEN_LENGTH} bytes ` +
        `(got ${grantToken.length})`,
    );
  }
  if (phraseSeed.length !== RECOVERY_PHRASE_SEED_LENGTH) {
    throw new Error(
      `phrase seed must be ${RECOVERY_PHRASE_SEED_LENGTH} bytes ` +
        `(got ${phraseSeed.length})`,
    );
  }
  const out = new Uint8Array(RECOVERY_PAYLOAD_LENGTH);
  out.set(grantToken, 0);
  out.set(phraseSeed, RECOVERY_GRANT_TOKEN_LENGTH);
  return out;
}

/** Parsed recovery payload — fresh copies of both halves. */
export interface ParsedRecoveryPayload {
  /** Raw 32-byte single-use device-authorization grant token. */
  readonly grantToken: Uint8Array;
  /** Raw 64-byte AccountIdentity (BIP39 phrase) seed. */
  readonly phraseSeed: Uint8Array;
}

/**
 * Joiner side: split the opened 96-byte plaintext back into
 * `(grant_token, phrase_seed)`. Strict length validation — any
 * other length means a wire/protocol mismatch and is refused.
 *
 * Both returned buffers are fresh copies; the caller should
 * zeroize the input `payload` after this returns.
 */
export function parseRecoveryPayload(
  payload: Uint8Array,
): ParsedRecoveryPayload {
  if (payload.length !== RECOVERY_PAYLOAD_LENGTH) {
    throw new Error(
      `recovery payload must be ${RECOVERY_PAYLOAD_LENGTH} bytes ` +
        `(got ${payload.length})`,
    );
  }
  return {
    grantToken: payload.slice(0, RECOVERY_GRANT_TOKEN_LENGTH),
    phraseSeed: payload.slice(RECOVERY_GRANT_TOKEN_LENGTH),
  };
}

// ---- mailbox row wrap / unwrap -----------------------------------

/** Wrap a sealed blob into the tagged mailbox row:
 *  `magic ‖ sealed`. */
export function wrapRecoveryRow(sealedBlob: Uint8Array): Uint8Array {
  const out = new Uint8Array(RECOVERY_ROW_MAGIC.length + sealedBlob.length);
  out.set(RECOVERY_ROW_MAGIC, 0);
  out.set(sealedBlob, RECOVERY_ROW_MAGIC.length);
  return out;
}

/** `true` when the mailbox row carries the recovery-payload magic
 *  prefix. Used by the joiner-side dispatch (and by the
 *  history-bundle poll to SKIP recovery rows). */
export function isRecoveryPayloadRow(row: Uint8Array): boolean {
  if (row.length <= RECOVERY_ROW_MAGIC.length) return false;
  for (let i = 0; i < RECOVERY_ROW_MAGIC.length; i += 1) {
    if (row[i] !== RECOVERY_ROW_MAGIC[i]) return false;
  }
  return true;
}

/** Strip the magic prefix and return the sealed blob, or `null`
 *  when the row is not a recovery-payload row. */
export function unwrapRecoveryRow(row: Uint8Array): Uint8Array | null {
  if (!isRecoveryPayloadRow(row)) return null;
  return row.slice(RECOVERY_ROW_MAGIC.length);
}

// ---- publish / poll ----------------------------------------------

/**
 * Scanner side: PUT the sealed recovery payload to the rendezvous
 * mailbox AFTER the Welcome (and the optional history bundle).
 * Same mailbox; the FIFO journal assigns it the next id. The row
 * is wrapped with the plaintext magic so the joiner-side polls can
 * dispatch by row type regardless of FIFO position.
 */
export async function publishRecoveryPayload(
  rendezvousPseudonym: Uint8Array,
  sealedBlob: Uint8Array,
  options: { ds?: DsClient } = {},
): Promise<void> {
  const ds = options.ds ?? new DsClient();
  await ds.publishRendezvous(rendezvousPseudonym, wrapRecoveryRow(sealedBlob));
}

/** Result of [`pollForRecoveryPayload`]. */
export type RecoveryPayloadOutcome =
  | { readonly kind: "received"; readonly sealedBlob: Uint8Array }
  | { readonly kind: "timeout" }
  | { readonly kind: "cancelled" };

/**
 * Joining side: poll the rendezvous mailbox past the Welcome
 * cursor for the recovery-payload row. Non-recovery rows (the
 * history bundle) are skipped by advancing the cursor; the first
 * row carrying the magic prefix wins.
 *
 * Absence is an expected outcome (`timeout`): an older scanner
 * build, or a scanner whose best-effort recovery half failed,
 * publishes no row — the join then completes exactly as today and
 * the fresh device falls back to phrase entry.
 *
 * `afterCursor` is typically the Welcome's DS journal id; both
 * this poll and `pollForHistoryBundle` start there and filter
 * disjoint row types, so they can run concurrently.
 */
export async function pollForRecoveryPayload(
  rendezvousPseudonym: Uint8Array,
  afterCursor: number,
  options: {
    ds?: DsClient;
    signal?: AbortSignal;
    timeoutMs?: number;
  } = {},
): Promise<RecoveryPayloadOutcome> {
  const ds = options.ds ?? new DsClient();
  const signal = options.signal;
  const timeoutMs = options.timeoutMs ?? RECOVERY_POLL_TIMEOUT_MS;
  const deadline = performance.now() + timeoutMs;
  let cursor = afterCursor;

  while (true) {
    if (signal?.aborted) return { kind: "cancelled" };
    const remainingMs = deadline - performance.now();
    if (remainingMs <= 0) return { kind: "timeout" };
    const waitSecs = Math.min(
      PER_REQUEST_LONG_POLL_SECS,
      Math.max(1, Math.floor(remainingMs / 1000)),
    );
    let result;
    try {
      result = await ds.fetchRendezvous(rendezvousPseudonym, {
        after: cursor,
        limit: 1,
        waitSecs,
      });
    } catch (err) {
      // Same transient-error posture as the Welcome / history polls.
      if (err instanceof DsHttpError && err.status === 404) continue;
      if (err instanceof DsNetworkError) continue;
      throw err;
    }
    if (result.messages.length > 0) {
      const first = result.messages[0];
      if (first === undefined) continue;
      const sealed = unwrapRecoveryRow(first.payload);
      if (sealed !== null) {
        return { kind: "received", sealedBlob: sealed };
      }
      // Not a recovery row (e.g. the history bundle) — skip it.
      cursor = first.id;
      continue;
    }
    cursor = result.messages.reduce((acc, m) => Math.max(acc, m.id), cursor);
  }
}
