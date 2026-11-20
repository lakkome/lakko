// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * `docs/ux/attachments.md` section 2 — the paperclip menu, as the core's
 * `attachmentMenu` rule decides it (one rule for the four clients), turned
 * into what this client draws: label key, icon, reason line. The menu is one
 * list with no separators (UX 2.2, amended 2026-11-18).
 */
import type { AttachDisabled, AttachItem, AttachMenuEntry } from "./types";
import type { IconName } from "../ui/icons";

/** The catalog key of each row's label. */
export const ITEM_LABEL_KEY: Readonly<Record<AttachItem, string>> = {
  "photo-or-video": "attach.menu.photo_video",
  camera: "attach.menu.camera",
  file: "attach.menu.file",
  audio: "attach.menu.audio",
  poll: "attach.menu.poll",
  checklist: "attach.menu.checklist",
  location: "attach.menu.location",
  contact: "attach.menu.contact",
  article: "attach.menu.article",
};

/** The icon of each row (UX 2.2's meanings). */
export const ITEM_ICON: Readonly<Record<AttachItem, IconName>> = {
  "photo-or-video": "photo-stack",
  camera: "camera",
  file: "doc",
  audio: "music-note",
  poll: "chart-bar",
  checklist: "checklist",
  location: "location",
  contact: "person",
  article: "doc-text",
};

/** The catalog key of a disabled row's reason (UX 2.4, 5). */
export const DISABLED_KEY: Readonly<Record<AttachDisabled, string>> = {
  "no-camera": "attach.menu.no_camera",
  "camera-in-call": "attach.menu.camera_in_call",
  "no-location": "attach.menu.no_location",
};

const ITEMS = new Set<string>(Object.keys(ITEM_LABEL_KEY));
const DISABLED = new Set<string>(Object.keys(DISABLED_KEY));

/** The core's rows as plain entries; an unknown item (a newer core) is left
 *  out rather than drawn without a label. */
export function menuEntriesOf(
  rows: readonly { readonly item: string; readonly section: number; readonly disabled: string | undefined }[],
): AttachMenuEntry[] {
  const out: AttachMenuEntry[] = [];
  for (const r of rows) {
    if (!ITEMS.has(r.item)) continue;
    const disabled = r.disabled !== undefined && DISABLED.has(r.disabled) ? (r.disabled as AttachDisabled) : undefined;
    out.push({ item: r.item as AttachItem, section: r.section, disabled });
  }
  return out;
}

/** The next enabled row from `from` in direction `step` (arrow keys), wrapping. */
export function nextEnabled(entries: readonly AttachMenuEntry[], from: number, step: 1 | -1): number {
  const n = entries.length;
  if (n === 0) return -1;
  for (let k = 1; k <= n; k++) {
    const i = (((from + step * k) % n) + n) % n;
    if (entries[i]?.disabled === undefined) return i;
  }
  return -1;
}

/** The role string the core takes for this person in this conversation. */
export function menuRole(args: { readonly owner: boolean; readonly admin: boolean }): "owner" | "admin" | "member" {
  if (args.owner) return "owner";
  if (args.admin) return "admin";
  return "member";
}

/** Whether this device has a camera (any video input). Never prompts. */
export async function deviceHasCamera(): Promise<boolean> {
  try {
    const devices = await navigator.mediaDevices?.enumerateDevices?.();
    return (devices ?? []).some((d) => d.kind === "videoinput");
  } catch {
    return false;
  }
}

/** Whether this platform has a location service at all. */
export function deviceHasLocation(): boolean {
  return typeof navigator !== "undefined" && "geolocation" in navigator;
}
