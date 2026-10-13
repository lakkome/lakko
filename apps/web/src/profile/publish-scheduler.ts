// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * ADR-0048 §2 — WHEN a member publishes its profile into a group.
 *
 * A profile rides an MLS application message, and a newcomer cannot read any
 * epoch before its join. So a member publishes:
 *
 *   1. **on joining a group** (a Welcome, founding one, accepting a request) —
 *      immediately;
 *   2. **on changing its profile** — immediately, into every group;
 *   3. **when a group gains a member** — so the newcomer learns existing
 *      names. THROTTLED and JITTERED per group: at most one such republish per
 *      group per {@link GROWTH_MIN_INTERVAL_MS}, delayed by a random
 *      0..{@link GROWTH_JITTER_MAX_MS}, so one add does not make every member
 *      publish in the same instant.
 *
 * ## Coalescing
 *
 * A growth republish already scheduled for a group absorbs any further growth
 * of that group until it fires. Any publish that DOES go out for a group —
 * join, change or growth — cancels a pending growth republish for it, because
 * it was sealed at the current epoch and the newcomer can read it.
 *
 * ## Failures
 *
 * A failed publish is retried through the throttled growth path, up to
 * {@link MAX_RETRIES} times per trigger, so a transient DS error does not leave
 * a newcomer nameless and a permanently unpublishable group does not spin.
 * The publish function itself decides what "nothing to do" means (a group the
 * device has left, no profile to publish) and returns normally for it.
 *
 * Pure: time, timers and randomness are injected.
 */

/** Minimum spacing of growth republishes for one group. */
export const GROWTH_MIN_INTERVAL_MS = 30_000;
/** Upper bound of the random delay added to a growth republish. */
export const GROWTH_JITTER_MAX_MS = 10_000;
/** Retries of one failed publish. */
export const MAX_RETRIES = 3;

export type PublishReason = "join" | "change" | "growth" | "retry";

export interface PublishSchedulerDeps<Timer> {
  now(): number;
  setTimer(fn: () => void, delayMs: number): Timer;
  clearTimer(timer: Timer): void;
  /** Uniform in [0, 1). */
  random(): number;
  /** Publish the current profile into one group. Rejects on failure. */
  publish(groupIdB64: string, reason: PublishReason): Promise<void>;
  onError?(groupIdB64: string, err: unknown): void;
}

interface Pending<Timer> {
  readonly timer: Timer;
  readonly retries: number;
}

export class ProfilePublishScheduler<Timer = unknown> {
  private readonly lastPublishAt = new Map<string, number>();
  private readonly pending = new Map<string, Pending<Timer>>();
  private stopped = false;

  constructor(private readonly deps: PublishSchedulerDeps<Timer>) {}

  /** Trigger 1 — this device just joined `groupIdB64`. */
  joined(groupIdB64: string): Promise<void> {
    return this.publishNow(groupIdB64, "join", 0).then(() => undefined);
  }

  /** Trigger 2 — the profile changed; publish into every listed group. */
  profileChanged(groupIdsB64: readonly string[]): Promise<void> {
    return Promise.all(groupIdsB64.map((g) => this.publishNow(g, "change", 0))).then(() => undefined);
  }

  /**
   * Publish into every group now and answer the ones whose publish FAILED
   * (ADR-0074 section 3.5a rule 3: the re-send after a key rotation is a
   * step that can be owed, so its caller must learn the outcome). A failed
   * one is retried by the scheduler exactly as any other.
   */
  async publishAll(groupIdsB64: readonly string[]): Promise<string[]> {
    const results = await Promise.all(
      groupIdsB64.map(async (g) => ({ g, ok: await this.publishNow(g, "change", 0) })),
    );
    return results.filter((r) => !r.ok).map((r) => r.g);
  }

  /** Trigger 3 — `groupIdB64` gained at least one member. */
  groupGrew(groupIdB64: string): void {
    this.schedule(groupIdB64, "growth", 0);
  }

  /** Whether a growth republish is waiting for `groupIdB64`. */
  hasPending(groupIdB64: string): boolean {
    return this.pending.has(groupIdB64);
  }

  /** Cancel everything (sign-out). The scheduler stays usable after `start`. */
  stop(): void {
    this.stopped = true;
    for (const p of this.pending.values()) this.deps.clearTimer(p.timer);
    this.pending.clear();
    this.lastPublishAt.clear();
  }

  /** Re-arm after `stop`. */
  start(): void {
    this.stopped = false;
  }

  /** The delay a growth republish of `groupIdB64` would wait now. */
  growthDelay(groupIdB64: string): number {
    const last = this.lastPublishAt.get(groupIdB64);
    const throttle = last === undefined ? 0 : Math.max(0, last + GROWTH_MIN_INTERVAL_MS - this.deps.now());
    const jitter = Math.floor(this.deps.random() * (GROWTH_JITTER_MAX_MS + 1));
    return throttle + Math.min(jitter, GROWTH_JITTER_MAX_MS);
  }

  private schedule(groupIdB64: string, reason: "growth" | "retry", retries: number): void {
    if (this.stopped) return;
    if (this.pending.has(groupIdB64)) return;
    const delay = this.growthDelay(groupIdB64);
    const timer = this.deps.setTimer(() => {
      const entry = this.pending.get(groupIdB64);
      if (entry === undefined || entry.timer !== timer) return;
      this.pending.delete(groupIdB64);
      void this.publishNow(groupIdB64, reason, retries);
    }, delay);
    this.pending.set(groupIdB64, { timer, retries });
  }

  private async publishNow(groupIdB64: string, reason: PublishReason, retries: number): Promise<boolean> {
    if (this.stopped) return false;
    // This publish covers anything a pending growth republish was waiting to
    // deliver.
    const waiting = this.pending.get(groupIdB64);
    if (waiting !== undefined) {
      this.deps.clearTimer(waiting.timer);
      this.pending.delete(groupIdB64);
    }
    this.lastPublishAt.set(groupIdB64, this.deps.now());
    try {
      await this.deps.publish(groupIdB64, reason);
      return true;
    } catch (err) {
      this.deps.onError?.(groupIdB64, err);
      if (retries < MAX_RETRIES) this.schedule(groupIdB64, "retry", retries + 1);
      return false;
    }
  }
}
