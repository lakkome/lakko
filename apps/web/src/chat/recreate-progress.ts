// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * ADR-0065 phase 2 (UX 16.5) — what the Recreate group sheet shows AFTER
 * the new group was founded and the old one closed, while the remaining
 * members are being invited.
 *
 * The sheet owns the whole operation. Founding the group, closing the old
 * one and the first invitation happen in the sheet over the OLD group; the
 * remaining invitations run through the new chat view's own sequential add
 * path (`pending-group-setup.ts`: durable commit record, proposal + sweep,
 * push-routing redistribution), which exists only once that view is
 * mounted. So the sheet opens the new group and carries on over it:
 *
 *   idle ──handed-off──▶ inviting ──adds-finished(none failed)──▶ idle
 *                           │
 *                           └──adds-finished(some failed)──▶ some-failed ──dismissed──▶ idle
 *
 * `inviting` is `Creating...` and cannot be dismissed. `some-failed` keeps
 * the sheet with the names and a `Done` button; the new group exists and is
 * open behind it. On full success the sheet closes on its own.
 *
 * Pure: no Solid, no wasm, no storage. In memory only, like the hand-off
 * it reports on: a reload mid-way leaves the group with the members that
 * landed, which the members panel can extend.
 */

export type RecreateProgress =
  | { readonly kind: "idle" }
  | { readonly kind: "inviting"; readonly groupIdB64: string }
  | {
      readonly kind: "some-failed";
      readonly groupIdB64: string;
      /** Display labels of the people whose invitation failed, in pick order. */
      readonly names: readonly string[];
    };

export type RecreateProgressEvent =
  /** The sheet founded `groupIdB64`, closed the old group and parked the
   *  remaining members for the new chat view. */
  | { readonly kind: "handed-off"; readonly groupIdB64: string }
  /** The new chat view ran every parked add. */
  | { readonly kind: "adds-finished"; readonly groupIdB64: string; readonly failed: readonly string[] }
  /** `Done`, or a dismissal gesture. */
  | { readonly kind: "dismissed" };

export const RECREATE_IDLE: RecreateProgress = { kind: "idle" };

export function recreateProgressReduce(
  state: RecreateProgress,
  event: RecreateProgressEvent,
): RecreateProgress {
  switch (event.kind) {
    case "handed-off":
      return { kind: "inviting", groupIdB64: event.groupIdB64 };
    case "adds-finished":
      // A report for a group this sheet is not waiting on changes nothing.
      if (state.kind !== "inviting" || state.groupIdB64 !== event.groupIdB64) return state;
      return event.failed.length === 0
        ? RECREATE_IDLE
        : { kind: "some-failed", groupIdB64: event.groupIdB64, names: [...event.failed] };
    case "dismissed":
      // The sheet cannot be dismissed while working.
      return state.kind === "inviting" ? state : RECREATE_IDLE;
  }
}

/** What the layer over the chat of `groupIdB64` shows. */
export interface RecreateProgressView {
  readonly open: boolean;
  /** `Creating...`, not dismissible. */
  readonly working: boolean;
  /** The names for the "could not add" line, or `null`. */
  readonly failedNames: readonly string[] | null;
}

export function recreateProgressView(
  state: RecreateProgress,
  groupIdB64: string,
): RecreateProgressView {
  if (state.kind === "idle" || state.groupIdB64 !== groupIdB64) {
    return { open: false, working: false, failedNames: null };
  }
  return state.kind === "inviting"
    ? { open: true, working: true, failedNames: null }
    : { open: true, working: false, failedNames: state.names };
}
