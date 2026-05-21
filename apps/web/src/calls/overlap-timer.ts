// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * 1-second SFrame overlap-window timer (Phase 3c5 — web).
 *
 * The Rust core
 * (`crates/lakko-calls/src/group_call.rs::GroupCallSession`)
 * is wall-clock-free by design: on every `refresh_call_key`
 * it stashes the prior CallKey into `previous_call_key`,
 * and the caller must invoke `drop_previous_call_key`
 * after a ~1-second overlap window expires. This module is
 * the **platform-side timer** that owns that schedule on
 * the web client.
 *
 * Usage:
 *
 * ```ts
 * import { OverlapTimer } from "./overlap-timer";
 *
 * const timer = new OverlapTimer({ ttlMs: 1000 });
 * // Right after refresh_call_key():
 * timer.arm(() => callSession.dropPreviousCallKey());
 * // ...the drop fires ~1 s later.
 *
 * // To cancel before firing (e.g. session torn down):
 * timer.cancel();
 * ```
 *
 * Properties:
 *
 *   * **Single-shot**: arming twice without cancellation
 *     replaces the prior callback (the newer rotation
 *     subsumes the older). The first callback never fires.
 *   * **Idempotent cancel**: `cancel()` on a disarmed
 *     timer is a no-op.
 *   * **Safe across destroy**: GC of the timer instance
 *     leaves the underlying `setTimeout` running with a
 *     captured closure — caller must cancel explicitly
 *     before drop.
 *
 * Why a dedicated class instead of plain `setTimeout`:
 *
 *   * Single seam to mock in tests (when a test framework
 *     lands; currently apps/web has no jest, so this is
 *     prep work).
 *   * Easy place to add EWMA / coalesce logic later
 *     without rewriting every call site.
 *   * Symmetric to the iOS / Android `OverlapTimer`
 *     helpers (those sprints follow).
 *
 * Closes part of G-18 (known-gaps): "Overlap-window timer
 * lives in the platform layer". The Rust side is
 * wall-clock-free by design; this helper is the canonical
 * platform-side schedule. Wiring it into a live call flow
 * lands with the SFU integration in 3b.
 *
 * See:
 *   * `docs/security/calls.md` C-9 — overlap window
 *     invariant.
 *   * `docs/security/known-gaps.md` G-18 — what this helper
 *     snaps.
 *   * `apps/web/wasm/lakko_ffi_wasm.d.ts` for the
 *     `sframeOpenWithOverlap` free function the caller
 *     plumbs frames through during the window.
 */

/** Default overlap window — 1 second per ADR-0008 §
 *  "1-second overlap absorbs in-flight frames". */
export const DEFAULT_OVERLAP_TTL_MS = 1000;

/** Bounds: don't accept anything below 50 ms (would skip
 *  too many in-flight frames) or above 10 s (would extend
 *  the seed-boundary forward-secrecy window unreasonably).
 *  Clamping is the safer side of "fail loudly on a typo". */
export const MIN_OVERLAP_TTL_MS = 50;
export const MAX_OVERLAP_TTL_MS = 10_000;

export interface OverlapTimerOptions {
  /** Time-to-live in milliseconds. Clamped to
   *  `[MIN_OVERLAP_TTL_MS, MAX_OVERLAP_TTL_MS]`. Defaults
   *  to `DEFAULT_OVERLAP_TTL_MS` (1 s). */
  readonly ttlMs?: number;
}

/** State of an `OverlapTimer`. Useful for tests + the
 *  occasional "is the window still open?" check during
 *  diagnostics. */
export type OverlapTimerState = "disarmed" | "armed" | "fired";

/**
 * Platform-side timer that triggers a `drop_previous_call_key`
 * callback after the overlap window expires. Single-shot;
 * re-arming replaces the prior schedule.
 *
 * Caller responsibilities:
 *
 *   - Cancel before disposing the timer instance — the
 *     underlying `setTimeout` keeps a closure live until
 *     it fires, so a forgotten arm leaks the closure.
 *   - Arm right after each `refresh_call_key`; don't pile
 *     up multiple timers per rotation.
 *
 * The Rust core (`GroupCallSession::drop_previous_call_key`)
 * is itself idempotent — calling it twice without an
 * intervening `refresh_call_key` is a no-op — so a misfire
 * from a stale callback is bounded to a wasted FFI call.
 */
export class OverlapTimer {
  public readonly ttlMs: number;
  private handle: ReturnType<typeof setTimeout> | null = null;
  private currentState: OverlapTimerState = "disarmed";

  constructor(options: OverlapTimerOptions = {}) {
    const requested = options.ttlMs ?? DEFAULT_OVERLAP_TTL_MS;
    if (!Number.isFinite(requested)) {
      this.ttlMs = DEFAULT_OVERLAP_TTL_MS;
    } else {
      this.ttlMs = Math.min(
        Math.max(Math.round(requested), MIN_OVERLAP_TTL_MS),
        MAX_OVERLAP_TTL_MS,
      );
    }
  }

  /** Current state — useful for diagnostics + tests. */
  get state(): OverlapTimerState {
    return this.currentState;
  }

  /**
   * Schedule `callback` to fire after `ttlMs`. If the
   * timer was already armed, the prior schedule is
   * cancelled and only the new `callback` will fire. The
   * caller is expected to invoke this immediately after
   * the corresponding `refresh_call_key` call on the
   * Rust core.
   */
  arm(callback: () => void): void {
    if (this.handle !== null) {
      clearTimeout(this.handle);
    }
    this.currentState = "armed";
    this.handle = setTimeout(() => {
      // Mark fired BEFORE running the callback so
      // a re-entrant arm() inside the callback observes
      // the prior shot as completed.
      this.handle = null;
      this.currentState = "fired";
      callback();
    }, this.ttlMs);
  }

  /** Cancel the pending callback, if any. No-op when
   *  disarmed. */
  cancel(): void {
    if (this.handle !== null) {
      clearTimeout(this.handle);
      this.handle = null;
    }
    this.currentState = "disarmed";
  }

  /** `true` iff a callback is scheduled and hasn't fired. */
  get isArmed(): boolean {
    return this.handle !== null;
  }
}
