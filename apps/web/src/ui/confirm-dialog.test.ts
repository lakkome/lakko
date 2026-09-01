// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * ADR-0045 UX §14 — the in-page confirmation that replaces `window.confirm`.
 *
 * `ui/ConfirmDialog.tsx` cannot be tested at all (`vitest.config.ts` runs a
 * plain `node` environment with no jsdom and no Solid JSX transform), so every
 * rule that DECIDES anything lives in `confirm-dialog.ts` and is pinned here —
 * the same split `remove-member-confirm.ts` / `membership-ended.ts` already
 * use. The component is left holding only markup.
 *
 * What is pinned here is the shared machinery: how an action names its target,
 * and what answer a request resolves to when nobody can be asked. The per-flow
 * copy is pinned next to each flow.
 */

import { beforeEach, describe, expect, it } from "vitest";

import {
  namedAction,
  pendingConfirm,
  requestConfirm,
  resolvePendingConfirm,
  setConfirmHostMounted,
} from "./confirm-dialog";
import {
  REMOVE_MEMBER_CONFIRM_LABEL,
  removeMemberConfirmation,
} from "../chat/remove-member-confirm";

beforeEach(() => {
  // Every test states its own host state; nothing carries over.
  setConfirmHostMounted(false);
});

describe("namedAction", () => {
  it("names the person or thing the action acts on", () => {
    // ADR-0045 UX §6.1 established this for the transfer step: the action
    // button's accessible name is its visible label, so naming the successor
    // in the label is what makes a screen reader announce WHO is affected.
    // `window.confirm` could not do this at all — it renders one string and a
    // browser-supplied OK/Cancel pair — which is a reason the widget had to
    // change and not merely a reason to restyle it.
    expect(namedAction("Remove", "Alice")).toBe("Remove Alice");
    expect(namedAction("Delete", "Ops team")).toBe("Delete Ops team");
    expect(namedAction("Leave", "Ops team")).toBe("Leave Ops team");
  });

  it("falls back to the bare verb rather than naming nothing", () => {
    // A roster label can be absent (a contact with no name degrades to a
    // fingerprint, and that can itself be empty in a torn read). `Remove `
    // with a dangling space, or `Remove undefined`, is worse than `Remove` —
    // so the fallback is the spec's own bare label, which is always true.
    for (const blank of [null, undefined, "", "   ", "\t\n"]) {
      expect(namedAction("Remove", blank)).toBe("Remove");
    }
  });

  it("trims the name rather than embedding stray whitespace", () => {
    expect(namedAction("Remove", "  Alice  ")).toBe("Remove Alice");
  });

  it("names the member on the settled remove-member confirmation", () => {
    // §6 owns the remove strings and its decision is settled — the verb stays
    // `Remove`. What §6.1's rule adds is the target, composed here rather than
    // in the component, so the naming is a tested decision and not markup.
    const c = removeMemberConfirmation({ usesProposalPath: true });
    expect(namedAction(c.confirmLabel, "Alice")).toBe("Remove Alice");
    expect(REMOVE_MEMBER_CONFIRM_LABEL).toBe("Remove");
  });
});

describe("requestConfirm when nobody can be asked", () => {
  it("resolves false when no dialog host is mounted", async () => {
    // This is the property that lets `backup/client.ts` keep its ADR-0037
    // fail-closed default: a confirmation that cannot be presented must read
    // as REFUSED, never as granted. It is also what makes the whole migration
    // safe — a call site whose host failed to mount does nothing, exactly as
    // a suppressed `window.confirm` did, rather than proceeding.
    await expect(
      requestConfirm({
        title: "Delete this chat?",
        body: null,
        confirmLabel: "Delete",
        cancelLabel: "Cancel",
        tone: "danger",
      }),
    ).resolves.toBe(false);
  });

  it("leaves nothing pending when it refuses", async () => {
    await requestConfirm({
      title: "T",
      body: null,
      confirmLabel: "Y",
      cancelLabel: "N",
      tone: "danger",
    });
    expect(pendingConfirm()).toBeNull();
  });
});

describe("requestConfirm with a host mounted", () => {
  it("stays pending until it is answered, and publishes the request", async () => {
    setConfirmHostMounted(true);
    const answer = requestConfirm({
      title: "Leave this group?",
      body: "You will need a new invite to rejoin.",
      confirmLabel: "Leave Ops team",
      cancelLabel: "Cancel",
      tone: "danger",
    });
    // The request is readable by the host — this is what it renders.
    expect(pendingConfirm()?.request.title).toBe("Leave this group?");
    expect(pendingConfirm()?.request.confirmLabel).toBe("Leave Ops team");
    resolvePendingConfirm(true);
    await expect(answer).resolves.toBe(true);
    expect(pendingConfirm()).toBeNull();
  });

  it("resolves false on cancel, and the caller performs nothing", async () => {
    setConfirmHostMounted(true);
    let performed = false;
    const answer = requestConfirm({
      title: "T",
      body: null,
      confirmLabel: "Y",
      cancelLabel: "N",
      tone: "danger",
    }).then((ok) => {
      // The call sites are all `if (!(await requestConfirm(...))) return;`.
      // This models that shape: a cancel must leave the action UNSTARTED,
      // not half-done.
      if (ok) performed = true;
      return ok;
    });
    resolvePendingConfirm(false);
    await expect(answer).resolves.toBe(false);
    expect(performed).toBe(false);
  });

  it("refuses a second request while one is open rather than stacking", async () => {
    // Two destructive dialogs at once is not a state this design has copy
    // for, and the safe answer to "which one did the user just agree to" is
    // to never be in it. The SECOND is refused; the first is untouched and
    // still answerable, so the user's open question is not cancelled out
    // from under them by a stray second caller.
    setConfirmHostMounted(true);
    const first = requestConfirm({
      title: "First",
      body: null,
      confirmLabel: "Y",
      cancelLabel: "N",
      tone: "danger",
    });
    await expect(
      requestConfirm({
        title: "Second",
        body: null,
        confirmLabel: "Y",
        cancelLabel: "N",
        tone: "danger",
      }),
    ).resolves.toBe(false);
    expect(pendingConfirm()?.request.title).toBe("First");
    resolvePendingConfirm(true);
    await expect(first).resolves.toBe(true);
  });

  it("resolves false when the host unmounts with a question open", async () => {
    // A route change unmounts the host. The dialog is then gone from the
    // screen, so the question was never answered — and an unanswered
    // destructive question is a refusal.
    setConfirmHostMounted(true);
    const answer = requestConfirm({
      title: "T",
      body: null,
      confirmLabel: "Y",
      cancelLabel: "N",
      tone: "danger",
    });
    setConfirmHostMounted(false);
    await expect(answer).resolves.toBe(false);
    expect(pendingConfirm()).toBeNull();
  });

  it("ignores a resolve when nothing is pending", () => {
    // Defensive: a late click on a torn-down dialog must not throw and must
    // not answer a question that has not been asked yet.
    setConfirmHostMounted(true);
    expect(() => resolvePendingConfirm(true)).not.toThrow();
    expect(pendingConfirm()).toBeNull();
  });
});
