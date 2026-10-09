// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * The one conversation list, rendered in the order the shared rule answered
 * (`docs/ux/navigation.md` section 2).
 *
 * ## Why jsdom and not Playwright
 *
 * The question is the ORDER OF THE ROWS, which is a tree fact: the `<For>`'s
 * output read top to bottom. Nothing about it needs layout, a media engine or a
 * browser policy, and a real browser would cost a fixture that stands up the
 * pane, the router and six contexts for a list of strings.
 *
 * ## What this covers that `conversation-list.test.ts` cannot
 *
 * That module proves `sortConversations` maps the web's rich row onto the six
 * fields `order_conversations` reads, and the Rust crate proves the order
 * itself. Neither can see whether the SCREEN asks. `ChatList` builds its rows
 * in its own memo, hands them to the rule, and then filters them again for the
 * search field — so a comparator added in the component, a `.reverse()` in the
 * filter, or a `<For>` fed from the unsorted memo would leave every existing
 * test green and put the newest conversation somewhere in the middle. The whole
 * point of the shared rule is that three clients agree; a client that asks it
 * and ignores the answer disagrees just as loudly as one that never asked.
 *
 * So the expectation below is computed by calling the rule DIRECTLY on the same
 * six-field rows and comparing it to what the component rendered. Two independent
 * routes to one answer: if they disagree, the component is doing its own
 * ordering somewhere. The rule is the real Rust one — `initWasm` loads the built
 * core — so this is not a spy agreeing with itself.
 *
 * ## What is stood in for, and why none of it can decide the order
 *
 * Six contexts, all of them NAME RESOLVERS or FLAG STORES: who is in a
 * conversation, what it is called, whether it is pinned, whether a call is
 * ringing in it. Reaching the real ones means an MLS identity, an OPAQUE
 * session, IndexedDB and the device-group sync runtime. They are replaced the
 * way `TimelineBubble.test.tsx` replaces its two, by module id, and each one
 * answers a constant. The auth state is deliberately `anon`, which is what
 * keeps the persisted message log out of the frame: `reloadLog` returns before
 * it touches the device key, so every row's `lastActivityMs` is the
 * `createdAtMs` this file sets — the sort key, in the test's own hand.
 *
 * Not stood in for: `ChatList` itself, `conversation-list.ts`, `ui-rules.ts`,
 * the wasm core, and the router. The rows really do travel out of the component
 * and back.
 */

// @vitest-environment jsdom

import { beforeAll, beforeEach, describe, expect, it, vi } from "vitest";
import { MemoryRouter, Route } from "@solidjs/router";
import type { Group as WasmGroup } from "../../wasm/lakko_ffi_wasm";

import type { ActiveGroup } from "./ChatContext";

// ----- the six contexts, replaced by module id ------------------------------
//
// Hoisted above every import, so `ChatList`'s own imports resolve to these.
// `rows` is read through a getter rather than captured, so a test can set the
// conversations before the component is rendered.

let groups: ActiveGroup[] = [];
let flags: Record<string, { pinned?: boolean; muted?: boolean; archived?: boolean }> = {};

vi.mock("./ChatContext", () => ({
  useChats: () => ({
    groups: () => groups,
    // No identity: with none, no row can be a chat with "us" in it, which is
    // the 1x1 naming path and has nothing to do with the order.
    identity: () => null,
  }),
}));
vi.mock("../profile/ProfileContext", () => ({
  useProfiles: () => ({
    version: () => 0,
    resolve: (k: Uint8Array | string) => ({
      name: typeof k === "string" ? k : "Member",
      // `ResolvedName.username` is `string | null`; `undefined` is not a value
      // the real resolver returns.
      username: null,
    }),
    // The people search (src/search) reads these for local candidates and
    // for the Add contact action; nothing here holds an account id.
    accountIdBindings: () => [],
    accountIdHexOf: () => null,
    learnUsername: () => {},
    ownProfileEnvelope: async () => null,
  }),
}));
vi.mock("./GroupNameContext", () => ({
  // No shared name, so a group is titled by the label this file gives it.
  useGroupNames: () => ({ nameOf: () => null, version: () => 0 }),
}));
vi.mock("./GroupClosedContext", () => ({
  // ADR-0065 phase 2 — no group here was closed.
  useGroupClosed: () => ({ closedOf: () => undefined, version: () => 0 }),
}));
vi.mock("../devices/sync/DeviceGroupSyncContext", () => ({
  useDeviceGroupSync: () => ({
    flags: () => flags,
    readCursors: () => ({}),
    folders: () => [],
    pendingFolderSync: () => false,
    setFlags: () => Promise.resolve(),
    applyFolderEdits: () => Promise.resolve(),
  }),
}));
vi.mock("../auth/AuthContext", () => ({
  // `anon` keeps the persisted log out: `reloadLog` returns before the device
  // key, so the sort key is the `createdAtMs` below and nothing else.
  useAuth: () => ({ state: () => ({ kind: "anon" }), recallDone: () => false }),
}));
vi.mock("../calls/CallHostContext", () => ({
  useCallHost: () => ({
    quietIncomingCall: () => null,
    acceptIncomingCall: () => Promise.resolve(),
    declineIncomingCall: () => Promise.resolve(),
  }),
}));

import { groupIdHex } from "../devices/sync/state";
import { all, initWasm, installLocalStorage, one, renderUi, settle } from "../test/ui";
import { sortConversations, type ConversationRow } from "./conversation-list";
import type { UiRules } from "./ui-rules";

let ChatList: typeof import("./ChatList")["ChatList"];
let rules: UiRules;

beforeAll(async () => {
  // Before `./ChatList` is imported, and the order is load-bearing for the same
  // reason it is in `CallBar.test.tsx`: `ui-rules` primes the shared rules at
  // import with `loadWasm()`, whose `init()` fetches a URL jsdom cannot fetch.
  // Initialising the same module from bytes first makes that a resolved no-op.
  await initWasm();
  const wasm = await import("../../wasm/lakko_ffi_wasm");
  rules = wasm as unknown as UiRules;
  ({ ChatList } = await import("./ChatList"));
  await settle();
});

beforeEach(() => {
  // The accepted-contact list and the restore marker are both read out of
  // `localStorage`, and there is not a working one here — see `installLocalStorage`. Empty is the state this file wants:
  // nothing named, nothing pinned by a previous test.
  installLocalStorage();
  groups = [];
  flags = {};
});

/**
 * One conversation, as the store holds it.
 *
 * `group` stands in for the wasm `Group` with the three reads `ChatList` makes
 * of it — the roster, the id and whether the membership is live. It has no
 * `groupKind()`, which reads as a group (ADR-0062, `conversation-kind.ts`), so
 * the title is the label:
 * a 1x1 would be titled by the peer resolver instead, which is a different
 * question from this one.
 */
function conversation(label: string, id: number, createdAtMs: number): ActiveGroup {
  const groupId = new Uint8Array(32).fill(id);
  const member = (n: number): { leafIndex: number; signaturePublicKey: Uint8Array } => ({
    leafIndex: n,
    signaturePublicKey: new Uint8Array(32).fill(id * 10 + n),
  });
  return {
    groupIdB64: `g${id}`,
    label,
    createdAtMs,
    group: {
      groupId,
      isActive: true,
      members: () => [member(0), member(1), member(2)],
    } as unknown as WasmGroup,
  } as unknown as ActiveGroup;
}

function pin(c: ActiveGroup): void {
  flags[groupIdHex((c.group as unknown as { groupId: Uint8Array }).groupId)] = { pinned: true };
}

function archive(c: ActiveGroup): void {
  flags[groupIdHex((c.group as unknown as { groupId: Uint8Array }).groupId)] = { archived: true };
}

function mountList(): void {
  // The real router, so the rows are the production `<A>` links they ship as
  // rather than an anchor a mock drew.
  renderUi(() => (
    <MemoryRouter>
      <Route path="/" component={() => <ChatList activeGroupIdB64={null} />} />
    </MemoryRouter>
  ));
}

/** The titles of the rendered rows, top to bottom. */
function renderedOrder(): string[] {
  return all(".lk-row .lk-row-title").map((el) => el.textContent ?? "");
}

/**
 * What the shared rule answers for the same conversations, reached directly.
 *
 * The second, independent route to the expected order. It goes through
 * `sortConversations` because that mapping is the web's own and is covered
 * elsewhere; what is being compared is whether the COMPONENT's output is this
 * one.
 */
function ruleOrder(): string[] {
  const rows: ConversationRow[] = groups.map((g) => {
    const hex = groupIdHex((g.group as unknown as { groupId: Uint8Array }).groupId);
    const f = flags[hex];
    return {
      id: g.groupIdB64,
      kind: "group",
      title: g.label,
      preview: "",
      lastActivityMs: g.createdAtMs,
      unread: 0,
      pinned: f?.pinned === true,
      muted: f?.muted === true,
      archived: f?.archived === true,
    };
  });
  return sortConversations(rows, rules).map((r) => r.title);
}

describe("the order of the one list", () => {
  it("renders newest first, and it is the rule's answer", () => {
    const oldest = conversation("Oldest", 1, 1_000);
    const newest = conversation("Newest", 2, 3_000);
    const middle = conversation("Middle", 3, 2_000);
    // Handed over in an order that is neither the answer nor its reverse, so a
    // component that simply rendered its input could not pass by accident.
    groups = [oldest, newest, middle];

    mountList();

    expect(renderedOrder()).toEqual(["Newest", "Middle", "Oldest"]);
    expect(renderedOrder()).toEqual(ruleOrder());
  });

  it("puts pinned first and archived last, whatever their times say", () => {
    // The case a comparator written in a component gets wrong: sorted by time
    // alone this is Fresh, Chatty, Stale, Parked — every position different.
    const stale = conversation("Stale", 1, 1_000);
    const chatty = conversation("Chatty", 2, 5_000);
    const fresh = conversation("Fresh", 3, 9_000);
    const parked = conversation("Parked", 4, 7_000);
    pin(stale);
    archive(parked);
    groups = [chatty, parked, fresh, stale];

    mountList();

    expect(renderedOrder()).toEqual(["Stale", "Fresh", "Chatty", "Parked"]);
    expect(renderedOrder()).toEqual(ruleOrder());
    // And it is not the order a time-only comparator would give, which is what
    // makes the agreement above evidence rather than a coincidence.
    expect(renderedOrder()).not.toEqual(["Fresh", "Parked", "Chatty", "Stale"]);
  });

  it("keeps the rule's order when the search field narrows the list", () => {
    // `visibleRows` filters the ordered memo. A filter that rebuilt the list
    // from the store would silently drop the ordering for anyone who typed.
    const stale = conversation("Stale team", 1, 1_000);
    const chatty = conversation("Chatty team", 2, 5_000);
    const fresh = conversation("Fresh team", 3, 9_000);
    pin(stale);
    groups = [chatty, fresh, stale];

    mountList();
    expect(renderedOrder()).toEqual(["Stale team", "Fresh team", "Chatty team"]);

    const search = one<HTMLInputElement>('input[type="search"]');
    search.value = "team";
    search.dispatchEvent(new Event("input", { bubbles: true }));

    expect(renderedOrder()).toEqual(["Stale team", "Fresh team", "Chatty team"]);

    search.value = "ty team";
    search.dispatchEvent(new Event("input", { bubbles: true }));

    expect(renderedOrder()).toEqual(["Chatty team"]);
  });
});

/**
 * ## Not covered here
 *
 * **Channels in the merged list.** `channelRows()` maps the channels store into
 * the same six fields and the rule orders them beside the groups, but filling
 * that store means its persistence sink (an account id, a device key and
 * IndexedDB). The mapping itself is `conversation-list.ts`'s and is covered
 * there; what is uncovered is that a channel and a group interleave by time on
 * screen.
 *
 * **The posture while the rules are still loading.** `ChatList` renders no rows
 * until `uiRules()` resolves, deliberately, so that it cannot fall back to a
 * second drifting copy of the order. Reaching that state needs the rules signal
 * held `null`, which this file cannot do while it also needs the real rule for
 * everything else.
 */
