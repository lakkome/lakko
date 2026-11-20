// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * Settings > Profile (docs/ux/profile-screen.md) — every rule the screen
 * follows, as plain functions so they are tested without a DOM:
 *
 *   * the `saved` / `pending` model of section 8, and when Save is enabled
 *     (section 5);
 *   * the in-place field (sections 3 and 4): open, type, close, Escape;
 *   * the save run, in order photo -> username -> profile, with what stays
 *     pending when a step fails (section 5);
 *   * the leave guard (section 6).
 *
 * `M` is the screen's local photo (the cropped media not yet uploaded); the
 * model never looks inside it.
 */

import type { AvatarData } from "./avatar-model";
import { isStorageFull } from "../storage/blob-access";

// ---------------------------------------------------------------------------
// Saved and pending
// ---------------------------------------------------------------------------

/** What is saved now. */
export interface SavedFields {
  readonly name: string | null;
  readonly username: string | null;
  readonly avatar: AvatarData | null;
}

/** A pending photo change. `uploaded` is a new photo whose upload already
 *  went through in an earlier save that failed at a later step: it is not
 *  uploaded again. */
export type PendingPhoto<M> =
  | { readonly kind: "remove" }
  | { readonly kind: "set"; readonly media: M }
  | { readonly kind: "uploaded"; readonly media: M; readonly avatar: AvatarData };

/** The changes not saved yet; an absent field is unchanged. */
export interface PendingFields<M> {
  readonly name?: string | null;
  readonly username?: string;
  readonly photo?: PendingPhoto<M>;
}

/**
 * Drop every pending value that equals the saved one: changing a field back
 * to its saved value is no change (section 5).
 */
export function normalizePending<M>(saved: SavedFields, pending: PendingFields<M>): PendingFields<M> {
  const out: { name?: string | null; username?: string; photo?: PendingPhoto<M> } = {};
  if (pending.name !== undefined && pending.name !== saved.name) out.name = pending.name;
  if (pending.username !== undefined && pending.username !== saved.username) out.username = pending.username;
  if (pending.photo !== undefined && !(pending.photo.kind === "remove" && saved.avatar === null)) {
    out.photo = pending.photo;
  }
  return out;
}

/** Whether anything differs from what is saved. */
export function isDirty<M>(saved: SavedFields, pending: PendingFields<M>): boolean {
  const p = normalizePending(saved, pending);
  return p.name !== undefined || p.username !== undefined || p.photo !== undefined;
}

/** Section 5 — Save is enabled when something differs, every open field is
 *  valid, and no save is running. */
export function saveEnabled<M>(input: {
  readonly saved: SavedFields;
  readonly pending: PendingFields<M>;
  readonly openFieldsValid: boolean;
  readonly busy: boolean;
}): boolean {
  return !input.busy && input.openFieldsValid && isDirty(input.saved, input.pending);
}

/** Whether a photo or video is shown (saved or pending) — the `Remove photo`
 *  menu item (section 2.1). */
export function photoShown<M>(saved: SavedFields, pending: PendingFields<M>): boolean {
  const p = pending.photo;
  if (p === undefined) return saved.avatar !== null;
  return p.kind !== "remove";
}

// ---------------------------------------------------------------------------
// The in-place field
// ---------------------------------------------------------------------------

export type FieldState =
  | { readonly open: false }
  | { readonly open: true; readonly text: string; readonly before: string };

export const CLOSED_FIELD: FieldState = { open: false };

/** Tap the pencil: the field opens holding `current`. */
export function openField(current: string): FieldState {
  return { open: true, text: current, before: current };
}

/** The person typed. */
export function typeIntoField(state: FieldState, text: string): FieldState {
  return state.open ? { ...state, text } : state;
}

/**
 * Focus left the field (blur, Return, Enter). A field whose text is invalid
 * stays open; a valid one closes and hands its text to the caller.
 */
export function leaveField(
  state: FieldState,
  valid: boolean,
): { readonly state: FieldState; readonly committed: string | null } {
  if (!state.open) return { state, committed: null };
  if (!valid) return { state, committed: null };
  return { state: CLOSED_FIELD, committed: state.text };
}

/** Escape: the field closes and the value returns to what it was before
 *  this edit. */
export function revertField(state: FieldState): { readonly state: FieldState; readonly restored: string | null } {
  if (!state.open) return { state, restored: null };
  return { state: CLOSED_FIELD, restored: state.before };
}

// ---------------------------------------------------------------------------
// The save
// ---------------------------------------------------------------------------

/** How a username change ended, as the save needs it. */
export type RenameResult =
  | { readonly kind: "done"; readonly username: string }
  /** The person cancelled the proof block. */
  | { readonly kind: "cancelled" }
  | { readonly kind: "taken" }
  | { readonly kind: "error"; readonly text: string | null };

export interface SavePorts<M> {
  /** Encrypt and upload the photo. Throws on failure. */
  uploadPhoto(media: M): Promise<AvatarData>;
  /** Run the existing rename operation (ADR-0076). Never throws. */
  rename(newUsername: string): Promise<RenameResult>;
  /** Save and publish the profile (name and photo). Throws on failure. */
  publish(fields: { readonly name: string | null; readonly avatar: AvatarData | null }): Promise<void>;
}

export type SaveError =
  | { readonly kind: "upload" }
  /** docs/ux/storage-full.md — the server has no room for new files. */
  | { readonly kind: "full" }
  | { readonly kind: "taken"; readonly username: string }
  | { readonly kind: "cancelled" }
  | { readonly kind: "generic"; readonly text: string | null };

export type SaveOutcome<M> =
  | { readonly kind: "saved" }
  /** `pending` is what is still not saved (what succeeded is out of it). */
  | { readonly kind: "failed"; readonly error: SaveError; readonly pending: PendingFields<M> };

/**
 * Section 5 — run the save, in order: the photo, the username, the profile.
 * A failed step keeps itself and everything after it pending.
 */
export async function runProfileSave<M>(
  saved: SavedFields,
  input: PendingFields<M>,
  ports: SavePorts<M>,
): Promise<SaveOutcome<M>> {
  let pending = normalizePending(saved, input);
  const fail = (error: SaveError): SaveOutcome<M> => ({ kind: "failed", error, pending });

  // 1. The photo.
  let avatar: AvatarData | null = saved.avatar;
  const photo = pending.photo;
  if (photo !== undefined) {
    if (photo.kind === "remove") {
      avatar = null;
    } else if (photo.kind === "uploaded") {
      avatar = photo.avatar;
    } else {
      try {
        avatar = await ports.uploadPhoto(photo.media);
      } catch (err) {
        return fail(isStorageFull(err) ? { kind: "full" } : { kind: "upload" });
      }
      pending = { ...pending, photo: { kind: "uploaded", media: photo.media, avatar } };
    }
  }

  // 2. The username.
  if (pending.username !== undefined) {
    const wanted = pending.username;
    const r = await ports.rename(wanted);
    switch (r.kind) {
      case "done": {
        const { username: _done, ...rest } = pending;
        void _done;
        pending = rest;
        break;
      }
      case "cancelled":
        return fail({ kind: "cancelled" });
      case "taken":
        return fail({ kind: "taken", username: wanted });
      case "error":
        return fail({ kind: "generic", text: r.text });
    }
  }

  // 3. The profile (name and photo), only when one of them changed.
  if (pending.name !== undefined || pending.photo !== undefined) {
    const name = pending.name !== undefined ? pending.name : saved.name;
    try {
      await ports.publish({ name, avatar });
    } catch {
      return fail({ kind: "generic", text: null });
    }
  }
  return { kind: "saved" };
}

// ---------------------------------------------------------------------------
// Leaving
// ---------------------------------------------------------------------------

/** Section 6 — navigating away: go, or ask first. */
export function leaveDecision(dirty: boolean, saving: boolean): "leave" | "ask" | "stay" {
  if (saving) return "stay";
  return dirty ? "ask" : "leave";
}

/** The dialog's answer, and what it does. */
export type LeaveAnswer = "save" | "discard" | "cancel";

export function leaveAnswerEffect(answer: LeaveAnswer): {
  readonly runSave: boolean;
  readonly dropPending: boolean;
  readonly continueNavigation: "now" | "after-save" | "never";
} {
  switch (answer) {
    case "save":
      return { runSave: true, dropPending: false, continueNavigation: "after-save" };
    case "discard":
      return { runSave: false, dropPending: true, continueNavigation: "now" };
    case "cancel":
      return { runSave: false, dropPending: false, continueNavigation: "never" };
  }
}
