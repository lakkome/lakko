// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * Member labels without a key fingerprint (docs/ux/client-redesign.md
 * section 6). The resolver (`resolver.ts`) falls back to `a1b2 c3d4` when a
 * person has no rename, no profile name and no verified username; that is
 * the right last resort for a contact DETAIL, where the safety number sits
 * next to it, and the wrong thing in a roster, a join request or a sender
 * line, where a hex group reads as an identifier the user is meant to
 * recognise. Those surfaces show a plain word instead.
 *
 * Pure: no wasm, no storage, no Solid.
 */
import type { ResolvedName } from "./resolver";

/** The generic label for a person the device cannot name. */
export const UNNAMED_MEMBER = "Member";

/** One-string surfaces (sender lines, typing, reactions, notices). */
export function memberNameOf(resolved: ResolvedName): string {
  return resolved.source === "fingerprint" ? UNNAMED_MEMBER : resolved.name;
}

/** Roster rows: `Name · @username` when there is one, never a fingerprint. */
export function memberListLabelOf(resolved: ResolvedName): string {
  return resolved.source === "fingerprint" ? UNNAMED_MEMBER : resolved.listLabel;
}
