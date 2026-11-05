// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * Where `Invite contacts` on the call screen sends an invitation
 * (`docs/ux/call-links.md` section 3; `docs/ux/who-can-find-me.md` section
 * 6.4): the same order as Message on a contact's card.
 *
 * - A one-to-one with the contact on this device: the call link goes into it.
 * - Else `chooseStartRoute`: a grant that still reaches them founds the
 *   one-to-one and the link goes into it; a request already standing in Sent
 *   sends nothing; a shared group gets the contact request (no call link until
 *   they accept); nothing at all sends nothing.
 *
 * Pure orchestration: every effect is a dependency, so a test runs it.
 */
import { chooseStartRoute, type SharedGroup } from "../contacts/message-action";

/** What the invitee's row says afterwards. */
export type InviteOutcome = "invited" | "request-sent" | "cannot-message";

export interface InviteRouteDeps<E> {
  /** The one-to-one with the contact on this device, if any. */
  readonly existing: () => E | undefined;
  /** Whether the contact's account id is known (founding needs it). */
  readonly accountKnown: boolean;
  readonly grantReaches: () => Promise<boolean>;
  readonly requestWaiting: () => boolean;
  readonly sharedGroups: () => Promise<readonly SharedGroup[]>;
  /** Found the one-to-one and return it. */
  readonly found: () => Promise<E>;
  /** Send the call link into the one-to-one. */
  readonly sendLink: (entry: E) => Promise<void>;
  /** Send the contact request inside the group and record it in Sent. */
  readonly sendInGroup: (groupIdB64: string) => Promise<void>;
}

/** Throws when something that was tried failed; the picker shows it and the
 *  row stays pressable. */
export async function inviteContactToCall<E>(deps: InviteRouteDeps<E>): Promise<InviteOutcome> {
  const held = deps.existing();
  if (held !== undefined) {
    await deps.sendLink(held);
    return "invited";
  }
  if (!deps.accountKnown) throw new Error("contact has no account id");
  const route = chooseStartRoute({
    grantReaches: await deps.grantReaches(),
    requestWaiting: deps.requestWaiting(),
    sharedGroups: await deps.sharedGroups(),
  });
  switch (route.kind) {
    case "found":
      await deps.sendLink(await deps.found());
      return "invited";
    case "waiting":
      return "request-sent";
    case "group":
      await deps.sendInGroup(route.groupIdB64);
      return "request-sent";
    case "cannot":
      return "cannot-message";
  }
}
