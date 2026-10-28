// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * ADR-0048 §3 — username verification.
 *
 * A received profile's `username` is the sender's CLAIM. It is shown only
 * after this module has resolved it and found it bound to the sender's
 * authenticated key. Since ADR-0060 section 5 the resolution is the
 * discovery-signed username certificate the sender's own profile carries,
 * checked against the lookup key of `OPRF(username)` and the account id the profile claims
 * (`verifyUsernameBinding`, the key pinned or fetched once) — no lookup, so a
 * person hidden from lookups keeps a verifiable username, and no endpoint
 * confirms a guessed username for a known account. The resolver lives in
 * `ProfileContext.tsx`; this module owns the states and the cache.
 *
 * ## States and what is cached
 *
 * | Outcome                                   | State      | Cached | Retried |
 * |-------------------------------------------|------------|--------|---------|
 * | resolved key == sender key                | `verified` | yes    | no      |
 * | resolved key != sender key                | `mismatch` | yes    | after `MISMATCH_RECHECK_MS` |
 * | username not registered / no bundle       | `mismatch` | yes    | after `MISMATCH_RECHECK_MS` |
 * | claim fails the username rule             | `mismatch` | yes    | no lookup is ever sent |
 * | lookup threw (network, a 429, tokens)     | `error`    | yes    | after `errorRetryDelayMs` (in memory) |
 *
 * The cache key is `(signature key, username)`, so a CHANGED claim is a
 * different key and verifies afresh, and a verified old claim says nothing
 * about a new one. A mismatch is re-checked only after a long interval: a
 * username is immutable once registered, so the only thing that can flip a
 * mismatch is a registration that had not happened yet when we looked.
 *
 * ## Single-flight
 *
 * Every lookup costs two proofs of work and tells discovery that someone
 * resolved that username (ADR-0048 Consequences). Concurrent requests for the
 * same `(key, username)` share one lookup, and lookups run one at a time.
 *
 * ## Never from the service worker
 *
 * Only the foreground constructs this class. The service worker reads the
 * persisted results; it never sends a lookup (UX spec §5).
 */

import type { VerificationState } from "./resolver";
import type { StoredVerification } from "./profile-store";

/** Re-check a cached mismatch after this long. */
export const MISMATCH_RECHECK_MS = 24 * 60 * 60 * 1000;
/** The longest wait before a failed lookup is tried again. */
export const ERROR_RETRY_MS = 5 * 60 * 1000;

/** The waits before a failed lookup is tried again: seconds at first, so a
 *  rate limit or a dropped connection clears quickly, growing to
 *  {@link ERROR_RETRY_MS}. Chosen, not measured. */
const ERROR_RETRY_STEPS_MS = [15_000, 30_000, 60_000, 120_000, ERROR_RETRY_MS] as const;

/** The wait before retry number `failures` (1 for the first failure), never
 *  shorter than what a 429 asked for (`retryAfterMs`), never longer than
 *  {@link ERROR_RETRY_MS} unless the server asked for more. */
export function errorRetryDelayMs(failures: number, retryAfterMs: number | null): number {
  const index = Math.min(Math.max(failures, 1), ERROR_RETRY_STEPS_MS.length) - 1;
  const step = ERROR_RETRY_STEPS_MS[index] ?? ERROR_RETRY_MS;
  return retryAfterMs !== null && retryAfterMs > step ? retryAfterMs : step;
}

/** The `retryAfterSecs` a refused request carried, anywhere in the error's
 *  chain, as milliseconds; `null` without one. */
function retryAfterMsOf(err: unknown): number | null {
  let current: unknown = err;
  for (let depth = 0; depth < 8 && typeof current === "object" && current !== null; depth += 1) {
    const secs = (current as { retryAfterSecs?: unknown }).retryAfterSecs;
    if (typeof secs === "number" && Number.isFinite(secs) && secs > 0) return secs * 1000;
    const record = current as { cause?: unknown; variant?: { cause?: unknown } };
    current = record.cause ?? record.variant?.cause;
  }
  return null;
}

/** What discovery says a username resolves to. */
export type UsernameResolution =
  /** The signature public key the username's account publishes. */
  | { readonly kind: "key"; readonly signatureKey: Uint8Array }
  /** The username is not registered, or its account has no identity bundle. */
  | { readonly kind: "unregistered" };

export interface UsernameVerifierDeps {
  /** Resolve a canonical username claimed by `keyB64`. ADR-0060: by the
   *  discovery-signed username certificate the person's profile carries (one
   *  OPRF evaluation, no lookup). Throws on a failure that says nothing about
   *  the answer (network, tokens). */
  resolve(username: string, keyB64: string): Promise<UsernameResolution>;
  /** The core's `validateUsername`: returns the normalised form or throws. */
  validate(username: string): string;
  /** Persisted result for a claim, or `undefined`. */
  cached(keyB64: string, username: string): StoredVerification | undefined;
  /** Persist a result. */
  record(keyB64: string, username: string, v: StoredVerification): void;
  now(): number;
  /** Decode a base64 key to bytes. */
  decodeKey(keyB64: string): Uint8Array;
  /** Run `retry` after `delayMs` (a failed lookup trying again). Absent in
   *  tests that drive the clock themselves. */
  scheduleRetry?(retry: () => Promise<VerificationState>, delayMs: number): void;
}

function bytesEqual(a: Uint8Array, b: Uint8Array): boolean {
  if (a.length !== b.length) return false;
  let diff = 0;
  for (let i = 0; i < a.length; i++) diff |= (a[i] ?? 0) ^ (b[i] ?? 0);
  return diff === 0;
}

/**
 * The decision for one completed lookup, on its own so it can be tested
 * without a discovery client.
 */
export function verdictFor(senderKey: Uint8Array, resolution: UsernameResolution): "verified" | "mismatch" {
  if (resolution.kind === "unregistered") return "mismatch";
  return bytesEqual(senderKey, resolution.signatureKey) ? "verified" : "mismatch";
}

/** Whether a cached result is still authoritative at `now`. */
export function cachedIsFresh(v: StoredVerification, now: number): boolean {
  switch (v.state) {
    case "verified":
      return true;
    case "mismatch":
      return now - v.checkedAtMs < MISMATCH_RECHECK_MS;
    case "error":
      // When a failed lookup is retried is held in memory by the verifier;
      // a persisted error is stale on its own.
      return false;
  }
}

export class UsernameVerifier {
  private readonly inflight = new Map<string, Promise<VerificationState>>();
  private queue: Promise<unknown> = Promise.resolve();
  /** Failed lookups in a row per claim, and when the next try is due. */
  private readonly failures = new Map<string, { count: number; retryAtMs: number }>();
  /** How many lookups this verifier has sent — for tests and diagnostics. */
  lookups = 0;

  constructor(private readonly deps: UsernameVerifierDeps) {}

  /**
   * The state to render right now, without starting anything: the cached
   * result, `pending` while a lookup runs, or `undefined` when nothing is
   * known yet.
   */
  state(keyB64: string, username: string): VerificationState | undefined {
    if (this.inflight.has(`${keyB64}|${username}`)) {
      const cached = this.deps.cached(keyB64, username);
      // A re-check of a still-verified claim keeps showing it.
      return cached?.state === "verified" ? "verified" : "pending";
    }
    return this.deps.cached(keyB64, username)?.state;
  }

  /**
   * Make sure the claim `(keyB64, username)` is verified or has a fresh
   * verdict. Returns the resulting state. Never throws.
   */
  ensure(keyB64: string, username: string): Promise<VerificationState> {
    const flightKey = `${keyB64}|${username}`;
    const cached = this.deps.cached(keyB64, username);
    if (cached !== undefined && cachedIsFresh(cached, this.deps.now())) {
      return Promise.resolve(cached.state);
    }
    const failed = this.failures.get(flightKey);
    if (cached?.state === "error" && failed !== undefined && this.deps.now() < failed.retryAtMs) {
      return Promise.resolve("error");
    }
    const existing = this.inflight.get(flightKey);
    if (existing !== undefined) return existing;

    const run = async (): Promise<VerificationState> => {
      // The rule first: a claim the core would never display is never looked
      // up either (the blinder would refuse it anyway).
      let canonical: string;
      try {
        canonical = this.deps.validate(username);
      } catch {
        this.deps.record(keyB64, username, { state: "mismatch", checkedAtMs: this.deps.now() });
        return "mismatch";
      }
      if (canonical !== username) {
        // A stored claim is canonical by construction; one that is not is not
        // the name the sender's account registered.
        this.deps.record(keyB64, username, { state: "mismatch", checkedAtMs: this.deps.now() });
        return "mismatch";
      }
      let senderKey: Uint8Array;
      try {
        senderKey = this.deps.decodeKey(keyB64);
      } catch {
        this.deps.record(keyB64, username, { state: "mismatch", checkedAtMs: this.deps.now() });
        return "mismatch";
      }
      let resolution: UsernameResolution;
      try {
        this.lookups += 1;
        resolution = await this.deps.resolve(canonical, keyB64);
      } catch (err) {
        this.deps.record(keyB64, username, { state: "error", checkedAtMs: this.deps.now() });
        const count = (this.failures.get(flightKey)?.count ?? 0) + 1;
        const delay = errorRetryDelayMs(count, retryAfterMsOf(err));
        this.failures.set(flightKey, { count, retryAtMs: this.deps.now() + delay });
        this.deps.scheduleRetry?.(() => this.ensure(keyB64, username), delay);
        return "error";
      }
      this.failures.delete(flightKey);
      const verdict = verdictFor(senderKey, resolution);
      this.deps.record(keyB64, username, { state: verdict, checkedAtMs: this.deps.now() });
      return verdict;
    };

    // One lookup at a time: each costs proofs of work.
    const promise = this.queue.then(run, run).finally(() => {
      this.inflight.delete(flightKey);
    });
    this.queue = promise.catch(() => undefined);
    this.inflight.set(flightKey, promise);
    return promise;
  }

  /**
   * Record a result this device learned from a lookup it ran for another
   * reason — adding a contact by username resolves exactly the binding a
   * verification would. Saves a second lookup; shows nothing by itself (a
   * username is only ever shown when the person's own profile claims it).
   */
  learn(keyB64: string, username: string): void {
    this.deps.record(keyB64, username, { state: "verified", checkedAtMs: this.deps.now() });
  }
}
