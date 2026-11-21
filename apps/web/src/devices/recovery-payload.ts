// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * Recovery-payload transport — ADR-0018 Part 2a, factor (a)
 * ("another-device" recovery), Path B (MLS exporter); payload v3 since
 * 2026-11-15 (ADR-0074, "Joining after a rotation").
 *
 * After the add-device pairing completes, the scanner (existing,
 * authenticated) device transfers to the fresh device, over the rendezvous
 * mailbox:
 *
 *   * **ACCESS** — a single-use 32-byte device-authorization grant minted by
 *     `POST /device/authorize/start`, redeemed anonymously at
 *     `POST /device/authorize/redeem`;
 *   * **IDENTITY** — the 16-byte entropy of the 12-word recovery phrase;
 *   * **the account's earlier keys** — the own predecessor seeds (0..=8,
 *     oldest first, with their adoption times), so a device paired after a
 *     key rotation holds the key every conversation of the account is bound
 *     to before it binds anything. Without them the device would bind under
 *     the current key and never reach the account's chats (G-56).
 *
 * The codec is NOT here: it is written once in Rust
 * (`lakko-mnemonic::recovery_payload`) and called through the wasm surface
 * (`composeRecoveryPayload` / `decodeRecoveryPayload`, the row framing
 * `wrapRecoveryRow` / `unwrapRecoveryRow`, the exporter label
 * `recoveryExporterLabel()`, now `lakko/device-recovery/v3`). Plaintext v3 =
 * `grant(32) ‖ phrase_entropy(16) ‖ count(1) ‖ count × (seed(64) ‖
 * adopted_at_ms(8 BE))`, row = `"lakko/recovery-payload/v3" ‖ sealed`.
 * v2 is refused in both directions (flag day, no dual accept).
 *
 * ## Crypto (Path B — MLS exporter)
 *
 * Both devices sit at the same DeviceGroup epoch once the Welcome is
 * processed, so both derive the same 32-byte key
 * `deviceGroup.exportSecret(identity, recoveryExporterLabel(), rendezvous,
 * recoveryExporterKeyLen())` and seal / open with `sealWithKey` /
 * `openWithKey` (ChaCha20-Poly1305). The DS stores the row opaque
 * (invariant #2): the magic marks only the row TYPE.
 *
 * This module keeps the transport (publish, poll) and the one thing other
 * polls need synchronously: whether a mailbox row is a recovery row at all,
 * of any version, so the Welcome / history / state polls skip it.
 */
import { DsClient, DsHttpError, DsNetworkError } from "../ds/client";

/** The version-independent part of the recovery row magic. A row starting
 *  with it is a recovery row; whether its version is the one this build
 *  reads is the core's `unwrapRecoveryRow` to say. */
const RECOVERY_ROW_PREFIX = new TextEncoder().encode("lakko/recovery-payload/");

/** Server-side long-poll budget per request, in seconds — same
 *  rationale as `welcome-courier.ts`. */
const PER_REQUEST_LONG_POLL_SECS = 15;

/** Default soft timeout for the joiner-side recovery poll. The scanner
 *  publishes the row right after the Welcome + history bundle (one extra
 *  `/device/authorize/start` round-trip), so a short window suffices. */
const RECOVERY_POLL_TIMEOUT_MS = 15_000;

/** One earlier account key carried by the row (an own predecessor). */
export interface RecoveredPredecessor {
  /** The 64-byte account phrase seed. SECRET: zero it once stored. */
  readonly seed: Uint8Array;
  /** When the sending device adopted the key that replaced it, unix ms. */
  readonly adoptedAtMs: number;
}

/** The decoded v3 payload. Every byte buffer is the caller's to zero. */
export interface OpenedRecoveryPayload {
  readonly grant: Uint8Array;
  readonly phraseEntropy: Uint8Array;
  /** Oldest first; the first is the conversation key's seed. */
  readonly predecessors: readonly RecoveredPredecessor[];
}

/** The wasm calls of the codec (`lakko-mnemonic::recovery_payload`). */
export interface RecoveryPayloadCore {
  composeRecoveryPayload(
    grant: Uint8Array,
    phraseEntropy: Uint8Array,
    predecessorSeeds: Uint8Array,
    predecessorAdoptedAtMs: BigUint64Array,
  ): Uint8Array;
  decodeRecoveryPayload(plaintext: Uint8Array): {
    readonly grant: Uint8Array;
    readonly phraseEntropy: Uint8Array;
    readonly predecessorCount: number;
    readonly predecessorSeeds: Uint8Array;
    readonly predecessorAdoptedAtMs: BigUint64Array;
    free(): void;
  };
}

/** Seed length of an own predecessor (the 64-byte BIP39 seed). */
const PREDECESSOR_SEED_LENGTH = 64;

/**
 * Scanner side: the v3 plaintext through the core. Throws the core's
 * `"<code>: <reason>"` refusal (a wrong length, more than 8 predecessors, a
 * duplicate, a seed of the current key); the caller publishes no row then.
 */
export function composeRecoveryPlaintext(
  core: RecoveryPayloadCore,
  grant: Uint8Array,
  phraseEntropy: Uint8Array,
  predecessors: readonly RecoveredPredecessor[],
): Uint8Array {
  const seeds = new Uint8Array(predecessors.length * PREDECESSOR_SEED_LENGTH);
  const adopted = new BigUint64Array(predecessors.length);
  predecessors.forEach((p, i) => {
    seeds.set(p.seed, i * PREDECESSOR_SEED_LENGTH);
    adopted[i] = BigInt(Math.max(0, Math.floor(p.adoptedAtMs)));
  });
  try {
    return core.composeRecoveryPayload(grant, phraseEntropy, seeds, adopted);
  } finally {
    seeds.fill(0);
  }
}

/**
 * Joiner side: decode the opened plaintext through the core. Throws on any
 * refusal (a v2 48-byte payload is `bad-length`). The returned buffers are
 * fresh copies; the caller zeroes them.
 */
export function decodeRecoveryPlaintext(
  core: RecoveryPayloadCore,
  plaintext: Uint8Array,
): OpenedRecoveryPayload {
  const decoded = core.decodeRecoveryPayload(plaintext);
  try {
    const flat = decoded.predecessorSeeds;
    const times = decoded.predecessorAdoptedAtMs;
    const predecessors: RecoveredPredecessor[] = [];
    for (let i = 0; i < decoded.predecessorCount; i += 1) {
      predecessors.push({
        seed: flat.slice(i * PREDECESSOR_SEED_LENGTH, (i + 1) * PREDECESSOR_SEED_LENGTH),
        adoptedAtMs: Number(times[i] ?? 0n),
      });
    }
    flat.fill(0);
    return { grant: decoded.grant, phraseEntropy: decoded.phraseEntropy, predecessors };
  } finally {
    decoded.free();
  }
}

/** Zero every secret buffer of an opened payload. */
export function zeroOpenedRecoveryPayload(opened: OpenedRecoveryPayload): void {
  opened.grant.fill(0);
  opened.phraseEntropy.fill(0);
  for (const p of opened.predecessors) p.seed.fill(0);
}

/** `true` when the mailbox row is a recovery row of ANY version. Used by
 *  the Welcome, history and state polls to SKIP it. */
export function isRecoveryPayloadRow(row: Uint8Array): boolean {
  if (row.length <= RECOVERY_ROW_PREFIX.length) return false;
  for (let i = 0; i < RECOVERY_ROW_PREFIX.length; i += 1) {
    if (row[i] !== RECOVERY_ROW_PREFIX[i]) return false;
  }
  return true;
}

// ---- publish / poll ----------------------------------------------

/**
 * Scanner side: PUT the already wrapped row (`wrapRecoveryRow(sealed)`) to
 * the rendezvous mailbox AFTER the Welcome (and the optional history
 * bundle).
 */
export async function publishRecoveryRow(
  rendezvousPseudonym: Uint8Array,
  row: Uint8Array,
  options: { ds?: DsClient } = {},
): Promise<void> {
  const ds = options.ds ?? new DsClient();
  await ds.publishRendezvous(rendezvousPseudonym, row);
}

/** Result of [`pollForRecoveryPayload`]. */
export type RecoveryPayloadOutcome =
  | { readonly kind: "received"; readonly row: Uint8Array }
  | { readonly kind: "timeout" }
  | { readonly kind: "cancelled" };

/**
 * Joining side: poll the rendezvous mailbox past the Welcome cursor for the
 * recovery row. Other rows are skipped by advancing the cursor; the first
 * recovery row (of any version) wins and is returned whole: the caller
 * unwraps it with the core (`unwrapRecoveryRow` refuses another version).
 *
 * Absence is `timeout`: the confirming device holds no phrase, or could not
 * read its own earlier keys, and published no row.
 */
export async function pollForRecoveryPayload(
  rendezvousPseudonym: Uint8Array,
  afterCursor: number,
  options: {
    ds?: DsClient;
    signal?: AbortSignal;
    timeoutMs?: number;
    /** Whether a tagged row is the payload (it opens); one that does not is
     *  passed over and the poll reads on. Absent: the first tagged row. */
    accept?: (row: Uint8Array) => boolean;
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
      if (isRecoveryPayloadRow(first.payload) && (options.accept === undefined || options.accept(first.payload))) {
        return { kind: "received", row: first.payload };
      }
      // Not a recovery row (e.g. the history bundle), or one that does not
      // open (anyone who learned the rendezvous id can write one) — skip it.
      cursor = first.id;
      continue;
    }
    cursor = result.messages.reduce((acc, m) => Math.max(acc, m.id), cursor);
  }
}
