// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * The words on the `/g/` group invite card (docs/ux/group-invite-page.md
 * section 3), derived from the sealed preview a link carries — or from its
 * absence. Pure, so `vitest` can hold the copy to the spec; `JoinGroup.tsx`
 * only places the lines.
 *
 * A missing preview is a state, not an error: a link minted without one, a
 * fragment a chat app stripped, a key that does not open. All three read the
 * same, and nothing here invents a name.
 */

/** What `GroupInviteCapability.openPreview` yields, as plain data. */
export interface GroupInvitePreviewData {
  readonly groupName?: string | undefined;
  readonly inviterName?: string | undefined;
  readonly memberCount: number;
}

/** The lines of the card's headline, top to bottom. */
export interface InviteHeadline {
  /** The sentence with the inviter as its subject. When the group has no
   *  name the sentence ends in `a group` and `title` is `null`. */
  readonly lead: string;
  /** The group's name on its own line, or `null` (then the lead already says
   *  `a group`). */
  readonly title: string | null;
  /** `1 member` / `N members` — the roster at mint time — or `null` without
   *  a preview. */
  readonly members: string | null;
}

/** The prefix of the muted foot line that carries the issuer fingerprint. */
export const INVITED_BY_PREFIX = "Invited by";

const UNNAMED_GROUP = "a group";
const NO_INVITER = "You are invited to";

function trimmed(value: string | undefined): string | null {
  const t = value?.trim() ?? "";
  return t.length > 0 ? t : null;
}

/** `1 member` / `N members`. */
export function memberCountLine(count: number): string {
  const n = Number.isFinite(count) && count > 0 ? Math.floor(count) : 0;
  return n === 1 ? "1 member" : `${n} members`;
}

/** The headline for a preview, or for no preview at all (`null`). */
export function inviteHeadline(preview: GroupInvitePreviewData | null): InviteHeadline {
  if (preview === null) {
    return { lead: `${NO_INVITER} ${UNNAMED_GROUP}`, title: null, members: null };
  }
  const inviter = trimmed(preview.inviterName);
  const group = trimmed(preview.groupName);
  const subject = inviter === null ? NO_INVITER : `${inviter} invites you to`;
  return {
    lead: group === null ? `${subject} ${UNNAMED_GROUP}` : subject,
    title: group,
    members: memberCountLine(preview.memberCount),
  };
}

/** The muted foot line: `Invited by <fingerprint>`. */
export function invitedByLine(fingerprint: string): string {
  return `${INVITED_BY_PREFIX} ${fingerprint}`;
}
