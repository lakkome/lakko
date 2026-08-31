// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * ADR-0045 UX §6 — the `Remove this member?` confirmation.
 *
 * `routes/Chat.tsx` cannot be tested at all (`vitest.config.ts` runs a plain
 * `node` environment with no jsdom and no Solid JSX transform), so the rule
 * lives in `remove-member-confirm.ts` and is pinned here — the same split
 * `membership-ended.ts` / `ownership-transfer.ts` / `pending-requests.ts` use.
 *
 * What is pinned is the DECISION — is a confirmation required for this
 * action, and what does it say — never the widget. `window.confirm` versus an
 * in-page dialog is §14's business and is deliberately not asserted here.
 *
 * The discrimination evidence for each rule (each probe was RUN, the failure
 * observed, and the probe reverted) is recorded in the module's own header.
 */

import { describe, expect, it } from "vitest";

import {
  REMOVE_MEMBER_BODY,
  REMOVE_MEMBER_CANCEL_LABEL,
  REMOVE_MEMBER_CONFIRM_LABEL,
  REMOVE_MEMBER_TITLE,
  removeMemberConfirmation,
} from "./remove-member-confirm";

describe("the remove-member confirmation", () => {
  it("is required on BOTH paths, because neither can be withdrawn", () => {
    // §3: once published, the request is in every member's proposal store and
    // dies only when some commit lands — the core has no withdraw and MLS
    // offers no mechanism for one. On the direct path there is no withdraw
    // either: the commit is published immediately. So the confirmation is the
    // last exit in both cases, and "confirm only on the proposal path" would
    // be exactly backwards — the direct path removes somebody FASTER.
    for (const usesProposalPath of [true, false]) {
      const c = removeMemberConfirmation({ usesProposalPath });
      expect(c.title).toBe(REMOVE_MEMBER_TITLE);
      expect(c.confirmLabel).toBe(REMOVE_MEMBER_CONFIRM_LABEL);
      expect(c.cancelLabel).toBe(REMOVE_MEMBER_CANCEL_LABEL);
    }
  });

  it("states §6's copy verbatim on the proposal path", () => {
    // "That body line is not a nicety. It is the one fact a user removing
    // someone for a safety reason must have, and it is the fact the old
    // immediate-looking UI concealed."
    const c = removeMemberConfirmation({ usesProposalPath: true });
    expect(c.body).toBe(REMOVE_MEMBER_BODY);
    expect(c.title).toBe("Remove this member?");
    expect(REMOVE_MEMBER_BODY).toBe(
      "They will keep receiving messages until an owner or an admin applies the change.",
    );
    expect(c.confirmLabel).toBe("Remove");
    expect(c.cancelLabel).toBe("Cancel");
  });

  it("says NOTHING on the direct path, because §6's body would be false there", () => {
    // A group with no role table still removes by direct commit, and a direct
    // commit takes effect the moment it lands. "They will keep receiving
    // messages until an owner or an admin applies the change" is then simply
    // untrue, and a security tool that reassures a user with a false sentence
    // about who can still read their messages is worse than one that says
    // less. So the body is dropped rather than reworded: the title alone is
    // true of both paths, and there is no second sentence §6 authorises.
    expect(removeMemberConfirmation({ usesProposalPath: false }).body).toBeNull();
  });

  it("uses no protocol vocabulary", () => {
    // §1: user-facing copy says `request` and `change`; it never says
    // proposal, commit, epoch, sweep, committer — or `leaf`, which is what
    // the web client's own pre-ADR-0045 direct-path string leaked
    // (`Remove this member (leaf #3)?`). A leaf index is meaningless to a
    // user and is the internal identifier ADR-0022 keys nothing on.
    const forbidden = ["leaf", "proposal", "commit", "epoch", "sweep", "committer"];
    for (const usesProposalPath of [true, false]) {
      const c = removeMemberConfirmation({ usesProposalPath });
      const copy = `${c.title} ${c.body ?? ""} ${c.confirmLabel} ${c.cancelLabel}`;
      for (const word of forbidden) {
        expect(copy.toLowerCase()).not.toContain(word);
      }
    }
  });
});
