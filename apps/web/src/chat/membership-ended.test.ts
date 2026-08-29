// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * ADR-0045 UX §6.2 — the end of your own membership.
 *
 * `routes/Chat.tsx` cannot be tested at all — `vitest.config.ts` collects
 * `src/**\/*.test.ts` in a plain `node` environment with no jsdom and no Solid
 * JSX transform — so every rule §6.2 states lives in `membership-ended.ts` and
 * is pinned here, exactly as `pending-requests.ts` / `ownership-transfer.ts` /
 * `committer-beacon.ts` already do it.
 *
 * The discrimination evidence for each rule (each probe was RUN, the failure
 * observed, and the probe reverted) is recorded in `membership-ended.ts`'s own
 * header, beside the rule it discriminates.
 */

import { afterEach, beforeEach, describe, expect, it } from "vitest";

import {
  CHAT_LIST_ENDED_LINE,
  COMPOSER_WITHDRAWN,
  DELETE_CHAT_BODY,
  DELETE_CHAT_TITLE,
  ENDED_CONSEQUENCE,
  ENDED_LEFT,
  ENDED_REMOVED,
  ENDED_TEARDOWN_STEPS,
  FROZEN_ROSTER_HEADER,
  UNSENT_DRAFT_LABEL,
  chatListLine,
  claimTeardownSteps,
  clearLeaveRequested,
  clearLeaveRequestedForAccount,
  composerState,
  loadLeaveRequestedAt,
  membershipEnded,
  membershipState,
  pendingTeardownSteps,
  readIsActive,
  recordLeaveRequested,
  releaseTeardownStep,
  terminalRow,
  type EndedTeardownStep,
} from "./membership-ended";
import { pruneObservations, recordObservation, loadObservations } from "./pending-requests";
import { exitBlock } from "./ownership-transfer";
import { membershipRotationDecision } from "../push/group-routing";
import { withoutGroup, type MessageLogPayload } from "./message-log-store";

/** The `localStorage` shim convention this repo already uses
 *  (`contacts/local-store.test.ts`, `chat/pending-requests.test.ts`). */
class MemoryStorage {
  private readonly map = new Map<string, string>();
  get length(): number {
    return this.map.size;
  }
  key(i: number): string | null {
    return [...this.map.keys()][i] ?? null;
  }
  getItem(k: string): string | null {
    return this.map.get(k) ?? null;
  }
  setItem(k: string, v: string): void {
    this.map.set(k, v);
  }
  removeItem(k: string): void {
    this.map.delete(k);
  }
  clear(): void {
    this.map.clear();
  }
}

beforeEach(() => {
  (globalThis as unknown as { localStorage: unknown }).localStorage =
    new MemoryStorage();
});

afterEach(() => {
  delete (globalThis as unknown as { localStorage?: unknown }).localStorage;
});

const ACCOUNT = "acct-A";
const GROUP = "Z3JvdXAtMQ==";

// ---------------------------------------------------------------------------
// Detecting the state
// ---------------------------------------------------------------------------

describe("membershipState", () => {
  it("an active group is unaffected", () => {
    const state = membershipState({
      isActive: true,
      leaveRequestedAtMs: undefined,
    });
    expect(state).toEqual({ kind: "active" });
    expect(membershipEnded(state)).toBe(false);
    expect(terminalRow(state)).toBeNull();
    expect(chatListLine(state)).toBeNull();
    expect(composerState(state, "half a sentence")).toEqual({ kind: "composer" });
  });

  it("an active group with a leave request outstanding is still active", () => {
    // §6.2.11 — NO optimistic transition. Between publishing a leave request
    // and the commit merging you are still fully in the group: still
    // receiving, still decrypting. The row annotation is §5.2's
    // `Waiting to leave.` and nothing else changes.
    const state = membershipState({ isActive: true, leaveRequestedAtMs: 1_000 });
    expect(state).toEqual({ kind: "active" });
    expect(composerState(state, "")).toEqual({ kind: "composer" });
  });

  it("eviction is detected once the removing commit has been applied", () => {
    const state = membershipState({
      isActive: false,
      leaveRequestedAtMs: undefined,
    });
    expect(state).toEqual({ kind: "ended", left: false });
    expect(membershipEnded(state)).toBe(true);
  });

  it("a torn read is NOT evidence of an eviction", () => {
    // An unreadable handle says nothing about membership. Claiming
    // `You are no longer a member of this group.` on it would be a false
    // statement the user cannot check; leaving the chat as it is today is
    // merely the status quo. So `undefined` fails toward `active`.
    const state = membershipState({
      isActive: undefined,
      leaveRequestedAtMs: 4_000,
    });
    expect(state).toEqual({ kind: "active" });
  });
});

describe("readIsActive", () => {
  it("reads the core's own predicate", () => {
    expect(readIsActive({ isActive: true })).toBe(true);
    expect(readIsActive({ isActive: false })).toBe(false);
  });

  it("a throwing getter reads as unknown, never as evicted", () => {
    const torn = {};
    Object.defineProperty(torn, "isActive", {
      get(): boolean {
        throw new Error("torn group handle");
      },
    });
    expect(readIsActive(torn as { isActive: boolean })).toBeUndefined();
  });
});

// ---------------------------------------------------------------------------
// §6.2.3 — left or removed
// ---------------------------------------------------------------------------

describe("left vs removed", () => {
  it("the durable leave record selects the LEFT copy", () => {
    const state = membershipState({ isActive: false, leaveRequestedAtMs: 99 });
    expect(state).toEqual({ kind: "ended", left: true });
    expect(terminalRow(state)?.headline).toBe(ENDED_LEFT);
    expect(ENDED_LEFT).toBe("You left this group.");
  });

  it("an absent record degrades to the REMOVED copy, which is still true", () => {
    // The fallback is the neutral string and the narrowing is what needs
    // positive evidence — the inverse of the usual reflex. "You are no longer
    // a member of this group" is true of someone who left as well as someone
    // who was removed; "you left this group" is true of only one. So a device
    // that lost the record (the ADR-0040 crash window, a reinstall, a restore)
    // degrades to a sentence that is still correct.
    const state = membershipState({
      isActive: false,
      leaveRequestedAtMs: undefined,
    });
    expect(terminalRow(state)?.headline).toBe(ENDED_REMOVED);
    expect(ENDED_REMOVED).toBe("You are no longer a member of this group.");
  });

  it("the standing consequence is the same either way", () => {
    const left = membershipState({ isActive: false, leaveRequestedAtMs: 1 });
    const removed = membershipState({
      isActive: false,
      leaveRequestedAtMs: undefined,
    });
    expect(terminalRow(left)?.consequence).toBe(ENDED_CONSEQUENCE);
    expect(terminalRow(removed)?.consequence).toBe(ENDED_CONSEQUENCE);
    // Written to be true whether or not a device was offline across its own
    // removal and lost the tail of its history (§6.2.6): it claims exactly
    // what is claimed and no more.
    expect(ENDED_CONSEQUENCE).toBe(
      "The messages you already received are still here. No new ones will arrive.",
    );
  });

  it("names nobody in either direction", () => {
    for (const at of [undefined, 12_345]) {
      const row = terminalRow(
        membershipState({ isActive: false, leaveRequestedAtMs: at }),
      );
      expect(row).not.toBeNull();
      expect(`${row?.headline} ${row?.consequence}`).not.toMatch(
        /removed by|by an admin|by the owner/i,
      );
    }
  });
});

// ---------------------------------------------------------------------------
// §6.2.3 / §9 — the durable record, and the prune that must not touch it
// ---------------------------------------------------------------------------

describe("the leaveRequestedAt record", () => {
  it("round-trips per (account, group)", () => {
    recordLeaveRequested(ACCOUNT, GROUP, 1_700_000);
    expect(loadLeaveRequestedAt(ACCOUNT, GROUP)).toBe(1_700_000);
    expect(loadLeaveRequestedAt(ACCOUNT, "b3RoZXI=")).toBeUndefined();
    expect(loadLeaveRequestedAt("acct-B", GROUP)).toBeUndefined();
  });

  it("SURVIVES the pending-proposal prune that destroys the observation", () => {
    // This is the whole reason it is a separate field. The observation for a
    // leave lives under `remove:{ownLeaf}` and every fold prunes an
    // observation once the core no longer holds the request — and the sweep
    // that removes you is exactly what stops the core holding it. So the poll
    // pass that carries the eviction also destroys the only evidence that you
    // asked for it, and the order of those two steps is an implementation
    // detail no test pins.
    recordObservation(ACCOUNT, GROUP, "remove:3", 1_000, true);
    recordLeaveRequested(ACCOUNT, GROUP, 1_000);

    // The eviction empties the core's pending set, so the fold prunes
    // everything.
    pruneObservations(ACCOUNT, GROUP, []);

    expect(loadObservations(ACCOUNT, GROUP).size).toBe(0);
    expect(loadLeaveRequestedAt(ACCOUNT, GROUP)).toBe(1_000);
    expect(
      membershipState({
        isActive: false,
        leaveRequestedAtMs: loadLeaveRequestedAt(ACCOUNT, GROUP),
      }),
    ).toEqual({ kind: "ended", left: true });
  });

  it("the FIRST record wins — a re-request never re-stamps it", () => {
    recordLeaveRequested(ACCOUNT, GROUP, 1_000);
    recordLeaveRequested(ACCOUNT, GROUP, 9_000);
    expect(loadLeaveRequestedAt(ACCOUNT, GROUP)).toBe(1_000);
  });

  it("is discarded with the group record, and with the account", () => {
    recordLeaveRequested(ACCOUNT, GROUP, 1_000);
    recordLeaveRequested(ACCOUNT, "b3RoZXI=", 2_000);
    clearLeaveRequested(ACCOUNT, GROUP);
    expect(loadLeaveRequestedAt(ACCOUNT, GROUP)).toBeUndefined();
    expect(loadLeaveRequestedAt(ACCOUNT, "b3RoZXI=")).toBe(2_000);

    recordLeaveRequested("acct-B", GROUP, 3_000);
    clearLeaveRequestedForAccount(ACCOUNT);
    expect(loadLeaveRequestedAt(ACCOUNT, "b3RoZXI=")).toBeUndefined();
    expect(loadLeaveRequestedAt("acct-B", GROUP)).toBe(3_000);
  });

  it("a corrupt row reads as absent, which degrades to the removed copy", () => {
    globalThis.localStorage.setItem(
      `lakko.leave-requested.v1.${ACCOUNT}.${GROUP}`,
      "not-a-number",
    );
    expect(loadLeaveRequestedAt(ACCOUNT, GROUP)).toBeUndefined();
  });
});

// ---------------------------------------------------------------------------
// §6.2.5 — the composer and the draft it was holding
// ---------------------------------------------------------------------------

describe("the composer", () => {
  const ended = membershipState({
    isActive: false,
    leaveRequestedAtMs: undefined,
  });

  it("is REPLACED, not disabled", () => {
    // §5.3 argues a pending row's `Remove` must stay visible-but-disabled,
    // because hiding it would read as the permission having vanished when it
    // is merely suspended. The permission here has vanished permanently, so
    // the rule inverts: disable while the state is transient, REMOVE when it
    // is terminal. A disabled Send on a group you will never be in again is a
    // control promising a future it does not have.
    const state = composerState(ended, "");
    expect(state.kind).toBe("withdrawn");
    if (state.kind !== "withdrawn") throw new Error("unreachable");
    expect(state.notice).toBe(COMPOSER_WITHDRAWN);
    expect(COMPOSER_WITHDRAWN).toBe(
      "You can no longer send messages to this group.",
    );
  });

  it("preserves an unsent draft verbatim, labelled", () => {
    // The commit lands during a poll, which can be mid-sentence. Silently
    // discarding what the user typed is a small data loss with no upside.
    const state = composerState(ended, "  see you tomorrow  ");
    if (state.kind !== "withdrawn") throw new Error("unreachable");
    expect(state.draft).toBe("  see you tomorrow  ");
    expect(state.draftLabel).toBe(UNSENT_DRAFT_LABEL);
    expect(UNSENT_DRAFT_LABEL).toBe("Unsent draft");
  });

  it("renders no draft block when the draft was empty", () => {
    for (const draft of ["", "   ", "\n\t"]) {
      const state = composerState(ended, draft);
      if (state.kind !== "withdrawn") throw new Error("unreachable");
      expect(state.draft).toBeNull();
      expect(state.draftLabel).toBeNull();
    }
  });
});

// ---------------------------------------------------------------------------
// §6.2.4 / §6.2.7 — the chat row and the frozen roster
// ---------------------------------------------------------------------------

describe("the chat list row and the frozen roster", () => {
  it("gains one muted line and nothing else", () => {
    const ended = membershipState({ isActive: false, leaveRequestedAtMs: 5 });
    expect(chatListLine(ended)).toBe(CHAT_LIST_ENDED_LINE);
    expect(CHAT_LIST_ENDED_LINE).toBe("You are no longer a member.");
  });

  it("says the same thing whether you left or were removed", () => {
    // The row line is deliberately neutral between the two cases: the list is
    // a glance surface and the distinction belongs in the transcript.
    expect(
      chatListLine(membershipState({ isActive: false, leaveRequestedAtMs: 5 })),
    ).toBe(
      chatListLine(
        membershipState({ isActive: false, leaveRequestedAtMs: undefined }),
      ),
    );
  });

  it("the frozen-roster header is a statement about the LIST, not a row", () => {
    // The role table is untouched by a `Remove` — a removal rewrites no
    // `GroupContext` — so a removed admin's frozen roster still names them an
    // admin. Nothing may present that as a live role, which is why the header
    // is about the list.
    expect(FROZEN_ROSTER_HEADER).toBe(
      "This list stopped updating when your membership ended.",
    );
  });
});

// ---------------------------------------------------------------------------
// §6.2.8 — what must stop, and what must be torn down
// ---------------------------------------------------------------------------

describe("teardown", () => {
  const ended = membershipState({
    isActive: false,
    leaveRequestedAtMs: undefined,
  });
  const active = membershipState({
    isActive: true,
    leaveRequestedAtMs: undefined,
  });

  it("nothing is torn down while the membership holds", () => {
    expect(pendingTeardownSteps(active, new Set())).toEqual([]);
  });

  it("every step is owed exactly once, and the second pass is a no-op", () => {
    const done = new Set<EndedTeardownStep>();
    const first = pendingTeardownSteps(ended, done);
    expect([...first].sort()).toEqual([...ENDED_TEARDOWN_STEPS].sort());
    for (const step of first) done.add(step);
    expect(pendingTeardownSteps(ended, done)).toEqual([]);
    // Idempotent under repetition, which is what makes it safe to drive from a
    // reactive effect that can re-run for unrelated reasons.
    expect(pendingTeardownSteps(ended, done)).toEqual([]);
  });

  it("a partially-completed teardown resumes rather than restarting", () => {
    const done = new Set<EndedTeardownStep>(["stop-beacon-ticker"]);
    expect(pendingTeardownSteps(ended, done)).toEqual([
      "retire-push-subscription",
    ]);
  });

  it("claims what it hands out, so a concurrent re-entry gets nothing", () => {
    // The defect this exists for: one of the two steps is async, so marking it
    // done after the await leaves a window in which the driving effect re-runs
    // — and it re-runs on every roster change, for reasons unrelated to the
    // eviction — reads the step as still owed, and fires a second concurrent
    // unsubscribe. "Exactly once" has to be decided BEFORE the first await.
    const done = new Set<EndedTeardownStep>();
    const first = claimTeardownSteps(ended, done);
    expect(first.length).toBe(ENDED_TEARDOWN_STEPS.length);
    // The re-entry, synchronously, before anything the first pass started has
    // had a chance to resolve.
    expect(claimTeardownSteps(ended, done)).toEqual([]);
  });

  it("a released step is retried, and only that one", () => {
    const done = new Set<EndedTeardownStep>();
    claimTeardownSteps(ended, done);
    releaseTeardownStep(done, "retire-push-subscription");
    expect(claimTeardownSteps(ended, done)).toEqual([
      "retire-push-subscription",
    ]);
    expect(claimTeardownSteps(ended, done)).toEqual([]);
  });

  it("claims nothing while the membership holds", () => {
    const done = new Set<EndedTeardownStep>();
    expect(claimTeardownSteps(active, done)).toEqual([]);
    expect(done.size).toBe(0);
  });

  it("names the push subscription, which is the half ADR-0043 called unclosable", () => {
    // ADR-0043 recorded the ex-member's push-gw row as never deleted, because
    // "nobody else can delete it and no client has 'you were removed'
    // handling". This IS that handling: the evicted device deletes its own
    // row.
    expect(ENDED_TEARDOWN_STEPS).toContain("retire-push-subscription");
    expect(ENDED_TEARDOWN_STEPS).toContain("stop-beacon-ticker");
  });
});

// ---------------------------------------------------------------------------
// §6.2.8 item 3 — the ADR-0043 shrink rotation must be suppressed
// ---------------------------------------------------------------------------

describe("the shrink rotation", () => {
  const OURS = "aa".repeat(32);
  const THEIRS = "bb".repeat(32);
  const OTHER = "cc".repeat(32);

  it("an evicted admin does NOT rotate, even though the role table still names it admin", () => {
    // `membershipRotationDecision` sees a member depart (us) and reads
    // `ownRole` from the UNTOUCHED role table — a `Remove` rewrites no
    // `GroupContext` — so before this an evicted admin returned "rotate" and
    // then threw on the broadcast, because `createApplicationMessage` is
    // refused for an inactive group.
    expect(
      membershipRotationDecision({
        previousMemberKeys: [OURS, THEIRS, OTHER],
        currentMemberKeys: [THEIRS, OTHER],
        ownRole: "admin",
        selfEvicted: true,
      }),
    ).toBe("skip-self-evicted");
  });

  it("the eviction beats every other verdict, including the authority check", () => {
    // Evaluated FIRST: a device that is out of the group has no business
    // rotating whatever its stale role table says, and the answer must not
    // depend on which of two skip reasons happened to be checked first.
    expect(
      membershipRotationDecision({
        previousMemberKeys: [OURS, THEIRS],
        currentMemberKeys: [OURS, THEIRS],
        ownRole: "member",
        selfEvicted: true,
      }),
    ).toBe("skip-self-evicted");
  });

  it("a SURVIVOR of someone else's removal still rotates", () => {
    // The whole point of ADR-0043: the remaining admin re-keys so the evicted
    // member's `push_gid` stops being notified. Suppressing that would undo
    // the ADR.
    expect(
      membershipRotationDecision({
        previousMemberKeys: [OURS, THEIRS, OTHER],
        currentMemberKeys: [OURS, OTHER],
        ownRole: "admin",
        selfEvicted: false,
      }),
    ).toBe("rotate");
  });

  it("an omitted flag behaves exactly as before", () => {
    // Every existing caller and every existing test passes no flag at all.
    expect(
      membershipRotationDecision({
        previousMemberKeys: [OURS, THEIRS],
        currentMemberKeys: [OURS],
        ownRole: "owner",
      }),
    ).toBe("rotate");
  });
});

// ---------------------------------------------------------------------------
// §6.2.7 / §5 item 6 — the exit block's third state
// ---------------------------------------------------------------------------

describe("the exit block once membership has ended", () => {
  const base = {
    usesProposalPath: true,
    isOwner: false,
    ownLeafKnown: true,
    candidateCount: 3,
    waitingRoleChange: null,
    ownRemovalPending: false,
  } as const;

  it("holds `Delete this chat` and neither of the other two controls", () => {
    // One names a group you are not in; the other is owner-only and you are
    // not. By §6.1.1's own argument the exit block is the right home for the
    // one action still honest: its meaning is *the exit*, and once you are out
    // the only exit-shaped action left is closing the door behind you.
    expect(exitBlock({ ...base, membershipEnded: true })).toEqual({
      kind: "ended",
    });
  });

  it("beats every other arm, including a group with no role table", () => {
    // A legacy group with no role table still evicts you by direct commit, so
    // the ended arm must be reached before the `hidden` gate.
    expect(
      exitBlock({ ...base, usesProposalPath: false, membershipEnded: true }),
    ).toEqual({ kind: "ended" });
    expect(
      exitBlock({ ...base, isOwner: true, membershipEnded: true }),
    ).toEqual({ kind: "ended" });
  });

  it("leaves the live arms untouched", () => {
    expect(exitBlock({ ...base, membershipEnded: false })).toEqual({
      kind: "member",
      leave: { enabled: true, disabledReason: null },
    });
  });

  it("the delete confirmation states exactly what is destroyed", () => {
    // Precise rather than sweeping, and the precision is load-bearing: the
    // messages really are gone, and the inert MLS handle is not.
    expect(DELETE_CHAT_TITLE).toBe("Delete this chat?");
    expect(DELETE_CHAT_BODY).toBe(
      "This removes the conversation and its messages from this device. " +
        "There is no copy anywhere else and this cannot be undone.",
    );
  });
});

// ---------------------------------------------------------------------------
// §6.2.4 — the per-group history delete
// ---------------------------------------------------------------------------

describe("withoutGroup", () => {
  const payload: MessageLogPayload = {
    version: 1,
    groups: {
      aaaa: { cursor: "7", messages: [] },
      bbbb: { cursor: "9", messages: [] },
    },
  };

  it("drops exactly one group and keeps the rest", () => {
    const next = withoutGroup(payload, "aaaa");
    expect(next).not.toBeNull();
    expect(Object.keys(next?.groups ?? {})).toEqual(["bbbb"]);
    expect(next?.version).toBe(1);
  });

  it("reports an absent group so the caller can skip the write", () => {
    expect(withoutGroup(payload, "cccc")).toBeNull();
  });

  it("does not mutate the payload it was given", () => {
    withoutGroup(payload, "aaaa");
    expect(Object.keys(payload.groups).sort()).toEqual(["aaaa", "bbbb"]);
  });
});
