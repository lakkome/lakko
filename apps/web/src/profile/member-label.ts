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
import { t } from "../l10n";
import type { ResolvedName } from "./resolver";

/** The generic label for a person the device cannot name. */
export const UNNAMED_MEMBER = (): string => t("chat.members.unnamed");

/** The people already reported by {@link reportUnnamedPerson} this session,
 *  by the resolver's fingerprint label (kept here, never logged). */
const reportedUnnamed = new Set<string>();

/**
 * A person always has a name on this device (2026-11-04): a surface that
 * reaches the placeholder word is a defect. Logged once per person per
 * session, as one plain line that names the surface and nobody, so the next
 * route that misses shows up in the log. `marker` tells people apart and is
 * never printed; `""` stands for a sender nobody could attribute.
 */
export function reportUnnamedPerson(marker: string, surface: string): void {
  if (reportedUnnamed.has(marker)) return;
  reportedUnnamed.add(marker);
  // eslint-disable-next-line no-console
  console.error(`[names] a person has no name on this device; ${surface} shows the placeholder`);
}

/** One-string surfaces (sender lines, typing, reactions, notices): the real
 *  name, else the placeholder word, reported ({@link reportUnnamedPerson}). */
export function memberNameOf(resolved: ResolvedName, surface: string): string {
  if (resolved.source !== "fingerprint") return resolved.name;
  reportUnnamedPerson(resolved.name, surface);
  return UNNAMED_MEMBER();
}

/**
 * The person's REAL name as the resolver gives it, or `null` when it could
 * only show a key fingerprint. For everything that outlives the moment it is
 * drawn or leaves the device: a reply or forward snapshot, a saved message,
 * a notification label. Those never carry a placeholder word or a
 * fingerprint (2026-11-04: a sender always has a name on this device, and a
 * missing one is a defect, not a word to bake in).
 */
export function realNameOf(resolved: ResolvedName): string | null {
  return resolved.source === "fingerprint" ? null : resolved.name;
}

/**
 * The name line of a message bubble. A sender always has a name on this
 * device (the profile in the same batch is filed first, an owed profile goes
 * out before the sender's next message): reaching the placeholder here is a
 * defect, so it is logged once per sender per session, as one plain line
 * naming nobody, and the placeholder word is kept rather than a name made
 * up. `keyB64` undefined is a sender the receive path could not attribute.
 */
export function bubbleSenderLabelOf(resolved: ResolvedName | null, keyB64: string | undefined): string {
  if (resolved === null || keyB64 === undefined) {
    reportUnnamedPerson("", "a message bubble (no attributable sender)");
    return UNNAMED_MEMBER();
  }
  return memberNameOf(resolved, "a message bubble");
}

/** Roster rows: `Name · @username` when there is one, never a fingerprint;
 *  the placeholder word is reported as in {@link memberNameOf}. */
export function memberListLabelOf(resolved: ResolvedName, surface = "a list row"): string {
  if (resolved.source !== "fingerprint") return resolved.listLabel;
  reportUnnamedPerson(resolved.name, surface);
  return UNNAMED_MEMBER();
}

/** One roster entry, as the label mirror reads it. */
export interface LabelledMember {
  readonly signaturePublicKey: Uint8Array;
  /** ADR-0050 — the account the leaf is bound to, when known. */
  readonly accountKey?: Uint8Array | undefined;
}

/**
 * The names the app holds for each member of the given rosters and for each
 * further person key (`otherKeys`: contacts, a contact's retired keys), under
 * every key a background reader can be handed for that person: the account
 * key a leaf is bound to (the person key) and the leaf's own signature key.
 * `nameOf` is the app's own rule (the resolver); it answers `null` for a
 * person whose name the app does not know, and such a person is LEFT OUT: a
 * notification never carries a placeholder word. A key it throws on is left
 * out too. Pure.
 */
export function conversationLabelsOf(
  rosters: Iterable<readonly LabelledMember[]>,
  nameOf: (personKeyB64: string) => string | null,
  encode: (bytes: Uint8Array) => string,
  otherKeys: Iterable<string> = [],
): Map<string, string> {
  const out = new Map<string, string>();
  const named = (key: string): string | null => {
    try {
      const name = nameOf(key)?.trim() ?? "";
      return name.length > 0 ? name : null;
    } catch {
      return null;
    }
  };
  for (const roster of rosters) {
    for (const m of roster) {
      const person = encode(m.accountKey ?? m.signaturePublicKey);
      const name = named(person);
      if (name === null) continue;
      out.set(person, name);
      out.set(encode(m.signaturePublicKey), name);
    }
  }
  for (const key of otherKeys) {
    if (out.has(key)) continue;
    const name = named(key);
    if (name !== null) out.set(key, name);
  }
  return out;
}

/** What the viewer's own avatar is lettered from when it has no photo: their
 *  display name, the username only when there is none. */
export function ownAvatarLabel(displayName: string | null, username: string): string {
  const name = displayName?.trim() ?? "";
  if (name.length > 0) return name;
  return username.length > 0 ? username : "?";
}
