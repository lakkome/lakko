// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * DS-mailbox round-trip for the add-device Welcome delivery
 * (Phase 2a3b2 web infrastructure).
 *
 * The pairing flow uses the DS mailbox at the rendezvous
 * pseudonym (chosen by the joining device, embedded in the
 * pairing QR) as the one-shot Welcome delivery slot:
 *
 *   * Scanner side (already in the DeviceGroup): produces a
 *     Welcome via `DeviceGroup.addDeviceFromWire`, PUTs it via
 *     [`publishWelcome`].
 *   * Joining side (fresh device): polls the rendezvous slot
 *     via [`pollForWelcome`] until the Welcome shows up, then
 *     hands the bytes to `DeviceGroup.joinFromWelcome`.
 *
 * No new server endpoints — uses the existing anonymous
 * `POST/GET /ds/mailbox/{pseudonym_hex}/messages` route. The
 * server treats the rendezvous pseudonym like any other
 * 32-byte mailbox slot; only the client knows the slot is
 * single-use and add-device-flavoured.
 */
import { DsClient, DsHttpError, DsNetworkError } from "../ds/client";

/** Soft cap on how long [`pollForWelcome`] waits before
 *  returning a `null` outcome to give the UI a chance to
 *  surface a "still waiting…" indicator and the user a way
 *  to cancel. */
const DEFAULT_POLL_TIMEOUT_MS = 60_000;

/** Server-side long-poll budget per request, in seconds. The
 *  DS service clamps to its own MAX_WAIT_SECS; we ask for
 *  comfortably below that so a misclock between client + server
 *  doesn't waste a whole poll. */
const PER_REQUEST_LONG_POLL_SECS = 15;

/**
 * Scanner side: PUT a Welcome to the rendezvous-mailbox slot
 * the joining device chose. The body is the raw MLS-encoded
 * Welcome bytes that `DeviceGroup.addDeviceFromWire` returned.
 *
 * Idempotent vis-à-vis the FIFO journal — re-publishing the
 * same Welcome appends a second copy; the joining device's
 * `pollForWelcome` filters with `after = 0` so it takes the
 * first one and exits.
 */
export async function publishWelcome(
  rendezvousPseudonym: Uint8Array,
  welcomeBytes: Uint8Array,
  options: { ds?: DsClient } = {},
): Promise<void> {
  const ds = options.ds ?? new DsClient();
  await ds.publishMailbox(rendezvousPseudonym, welcomeBytes);
}

/** Result of [`pollForWelcome`]. */
export type WelcomeOutcome =
  | {
      readonly kind: "received";
      readonly welcome: Uint8Array;
      /** DS journal id the Welcome landed on. Passed to
       *  [`pollForHistoryBundle`] as the `welcomeCursor` so
       *  the history poll starts at the next FIFO slot. */
      readonly welcomeCursor: number;
    }
  | { readonly kind: "timeout" }
  | { readonly kind: "cancelled" };

/**
 * Joining side: poll the rendezvous mailbox until the Welcome
 * arrives, the caller's `AbortSignal` fires, or the soft
 * timeout elapses.
 *
 * The DS endpoint supports a server-side long-poll budget per
 * request; we issue back-to-back 15-second long polls in a
 * loop so a single network round-trip doesn't burn the whole
 * UX budget when the scanner side is slow.
 *
 * Returns:
 *   * `{kind:"received", welcome}` — the Welcome arrived;
 *     hand the bytes to `DeviceGroup.joinFromWelcome`.
 *   * `{kind:"timeout"}` — `timeoutMs` elapsed with no
 *     Welcome. UI can surface "no response yet" with a retry
 *     affordance.
 *   * `{kind:"cancelled"}` — `signal` fired (user closed the
 *     pairing modal).
 *
 * Throws on transport / server failures (caller surfaces as
 * a one-line toast).
 */
export async function pollForWelcome(
  rendezvousPseudonym: Uint8Array,
  options: {
    ds?: DsClient;
    signal?: AbortSignal;
    timeoutMs?: number;
  } = {},
): Promise<WelcomeOutcome> {
  const ds = options.ds ?? new DsClient();
  const signal = options.signal;
  const timeoutMs = options.timeoutMs ?? DEFAULT_POLL_TIMEOUT_MS;
  const deadline = performance.now() + timeoutMs;

  // The rendezvous slot is fresh per pairing; messages with
  // id > 0 are the Welcome (the first publish lands as id=1).
  // `after=0` is the canonical "give me everything from the
  // beginning of this slot" cursor for the DS mailbox shape.
  let cursor = 0;

  while (true) {
    if (signal?.aborted) return { kind: "cancelled" };
    const remainingMs = deadline - performance.now();
    if (remainingMs <= 0) return { kind: "timeout" };

    // Cap the per-request long-poll budget at the remaining
    // window so the last poll doesn't overrun.
    const waitSecs = Math.min(
      PER_REQUEST_LONG_POLL_SECS,
      Math.max(1, Math.floor(remainingMs / 1000)),
    );
    let result;
    try {
      result = await ds.fetchMailbox(rendezvousPseudonym, {
        after: cursor,
        limit: 1,
        waitSecs,
      });
    } catch (err) {
      // 404 on a fresh mailbox is the DS service's "no rows
      // yet" reply — treat as "keep polling". Anything else
      // bubbles up so the UI shows a transport error.
      if (err instanceof DsHttpError && err.status === 404) {
        continue;
      }
      if (err instanceof DsNetworkError) {
        // Transient network blip — try again until the
        // timeout elapses. (Burns a poll iteration; if the
        // connection stays broken `fetchMailbox` keeps
        // throwing and the deadline wins.)
        continue;
      }
      throw err;
    }
    if (result.messages.length > 0) {
      const first = result.messages[0];
      // Defensive — `noUncheckedIndexedAccess` flags this
      // path; the length check above guarantees the value.
      if (first === undefined) continue;
      return {
        kind: "received",
        welcome: first.payload,
        welcomeCursor: first.id,
      };
    }
    // Advance the cursor by the highest id we've seen so the
    // next poll doesn't re-fetch the same empty range. In
    // the happy path the loop returns on the first non-empty
    // page; this branch only matters if the server emits
    // non-Welcome bytes into the slot (which shouldn't
    // happen — the slot is single-use by design — but the
    // cursor advance is the canonical FIFO-journal posture).
    cursor = result.messages.reduce((acc, m) => Math.max(acc, m.id), cursor);
  }
}

// ---------------------------------------------------------------
// Phase 2a3d8-ui — history-bundle courier piggybacks the
// rendezvous mailbox. After the Welcome lands on slot id=1, the
// scanner side publishes the sealed history bundle as the next
// FIFO row; the joining device polls past the Welcome cursor for
// a small soft window.
// ---------------------------------------------------------------

/** Default soft timeout for the joiner-side history poll. Short
 *  — the bundle is supposed to arrive within seconds of the
 *  Welcome; a longer wait creates a stalled UX without any
 *  meaningful payoff. */
const HISTORY_POLL_TIMEOUT_MS = 10_000;

/**
 * Scanner side: PUT the sealed history bundle to the rendezvous
 * slot AFTER the Welcome has been published. Same mailbox; the
 * FIFO journal assigns it the next id (id=2 if Welcome was the
 * first publish).
 */
export async function publishHistoryBundle(
  rendezvousPseudonym: Uint8Array,
  sealedBytes: Uint8Array,
  options: { ds?: DsClient } = {},
): Promise<void> {
  const ds = options.ds ?? new DsClient();
  await ds.publishMailbox(rendezvousPseudonym, sealedBytes);
}

/** Result of [`pollForHistoryBundle`]. */
export type HistoryBundleOutcome =
  | { readonly kind: "received"; readonly sealedBytes: Uint8Array }
  | { readonly kind: "timeout" }
  | { readonly kind: "cancelled" };

/**
 * Joining side: poll the rendezvous mailbox past the Welcome
 * cursor for the sealed history bundle. Soft timeout 10 s —
 * the scanner side either publishes within that window or never
 * (e.g. it had no prior history; nothing to send).
 *
 * `welcomeCursor` is the id of the Welcome row the joining
 * device just consumed (typically `1` for a fresh pairing).
 * The poll starts at `after = welcomeCursor` so the next FIFO
 * row is the bundle.
 */
export async function pollForHistoryBundle(
  rendezvousPseudonym: Uint8Array,
  welcomeCursor: number,
  options: {
    ds?: DsClient;
    signal?: AbortSignal;
    timeoutMs?: number;
  } = {},
): Promise<HistoryBundleOutcome> {
  const ds = options.ds ?? new DsClient();
  const signal = options.signal;
  const timeoutMs = options.timeoutMs ?? HISTORY_POLL_TIMEOUT_MS;
  const deadline = performance.now() + timeoutMs;
  let cursor = welcomeCursor;

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
      result = await ds.fetchMailbox(rendezvousPseudonym, {
        after: cursor,
        limit: 1,
        waitSecs,
      });
    } catch (err) {
      if (err instanceof DsHttpError && err.status === 404) continue;
      if (err instanceof DsNetworkError) continue;
      throw err;
    }
    if (result.messages.length > 0) {
      const first = result.messages[0];
      if (first === undefined) continue;
      return { kind: "received", sealedBytes: first.payload };
    }
    cursor = result.messages.reduce((acc, m) => Math.max(acc, m.id), cursor);
  }
}
