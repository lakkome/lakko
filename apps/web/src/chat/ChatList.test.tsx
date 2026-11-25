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
 * `createdAtMs` this file sets — the sort key, in the test's own hand. With no
 * session the stores would never count as read, so the readiness rule
 * (`view-ready.ts`, tested on its own) answers what this file says.
 *
 * Not stood in for: `ChatList` itself, `conversation-list.ts`, `ui-rules.ts`,
 * the wasm core, and the router. The rows really do travel out of the component
 * and back.
 */

// @vitest-environment jsdom

import { afterEach, beforeAll, beforeEach, describe, expect, it, vi } from "vitest";
import { MemoryRouter, Route, createMemoryHistory } from "@solidjs/router";
import type { Group as WasmGroup } from "../../wasm/lakko_ffi_wasm";

import type { ActiveGroup } from "./ChatContext";

// ----- the six contexts, replaced by module id ------------------------------
//
// Hoisted above every import, so `ChatList`'s own imports resolve to these.
// `rows` is read through a getter rather than captured, so a test can set the
// conversations before the component is rendered.

let groups: ActiveGroup[] = [];
/** When set, the list reads its groups through this accessor, so a test can
 *  change them under a mounted list. */
let groupsAccessor: (() => ActiveGroup[]) | null = null;
/** `docs/ux/navigation.md` 2.4 — whether the stores that name and order the
 *  rows have been read; `false` keeps the list from drawing anything. */
let listReady = true;
let flags: Record<string, { pinned?: boolean; muted?: boolean; archived?: boolean }> = {};
/** The synced pin order (`docs/ux/navigation.md` 2.2.1); `null` for none. */
let pinOrder: { groupIds: Uint8Array[] } | null = null;
/** Every list `setOrder` was handed. */
let orderWrites: Uint8Array[][] = [];

vi.mock("./ChatContext", () => ({
  useChats: () => ({
    groups: () => (groupsAccessor === null ? groups : groupsAccessor()),
    // No identity: with none, no row can be a chat with "us" in it, which is
    // the 1x1 naming path and has nothing to do with the order.
    identity: () => null,
  }),
}));
vi.mock("../profile/ProfileContext", () => ({
  useProfiles: () => ({
    version: () => 0,
    loadedFor: () => null,
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
  useGroupNames: () => ({
    nameOf: () => null,
    handleOf: () => undefined,
    version: () => 0,
    loadedFor: () => null,
  }),
}));
vi.mock("../devices/sync/DeviceGroupSyncContext", () => ({
  useDeviceGroupSync: () => ({
    flags: () => flags,
    // ADR-0073 R.5 — no conversation here continued in a successor group, so
    // every group is its own conversation and its row is keyed on its id.
    lineage: () => [],
    readCursors: () => ({}),
    // `docs/ux/navigation.md` 2.2.1 - the pin order set by hand, and what a
    // reorder writes.
    order: () => pinOrder,
    setOrder: (ids: readonly Uint8Array[]) => {
      orderWrites.push([...ids]);
      return Promise.resolve();
    },
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
vi.mock("./view-ready", async (importOriginal) => ({
  ...(await importOriginal<typeof import("./view-ready")>()),
  chatListReady: () => listReady,
}));
vi.mock("../calls/CallHostContext", () => ({
  useCallHost: () => ({
    quietIncomingCall: () => null,
    acceptIncomingCall: () => Promise.resolve(),
    declineIncomingCall: () => Promise.resolve(),
  }),
}));

import { createSignal } from "solid-js";
import { base64UrlEncodeNoPad } from "../auth/account-id";
import { channelsState } from "../state/channels-state";
import { channelRoutePattern, chatRoutePattern, groupChatRoutePattern, paths } from "../routes/paths";
import { groupIdHex } from "../devices/sync/state";
import { all, initWasm, installLocalStorage, installStylesheet, one, renderUi, settle } from "../test/ui";
import { sortConversations, type ConversationRow } from "./conversation-list";
import type { UiRules } from "./ui-rules";

let ChatList: typeof import("./ChatList")["ChatList"];
let ChatsLayout: typeof import("../routes/ChatsLayout")["default"];
let rules: UiRules;

beforeAll(async () => {
  // Before `./ChatList` is imported, and the order is load-bearing for the same
  // reason it is in `CallWindow.test.tsx`: `ui-rules` primes the shared rules at
  // import with `loadWasm()`, whose `init()` fetches a URL jsdom cannot fetch.
  // Initialising the same module from bytes first makes that a resolved no-op.
  await initWasm();
  const wasm = await import("../../wasm/lakko_ffi_wasm");
  rules = wasm as unknown as UiRules;
  ({ ChatList } = await import("./ChatList"));
  ({ default: ChatsLayout } = await import("../routes/ChatsLayout"));
  await settle();
});

beforeEach(() => {
  // The accepted-contact list and the restore marker are both read out of
  // `localStorage`, and there is not a working one here — see `installLocalStorage`. Empty is the state this file wants:
  // nothing named, nothing pinned by a previous test.
  installLocalStorage();
  groups = [];
  flags = {};
  pinOrder = null;
  orderWrites = [];
  listReady = true;
  groupsAccessor = null;
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

    // The archived one is not in the chats list: it is behind the one
    // "Archived" bar at the bottom of the panel (navigation.md 2.2 step 3).
    const chatsShown = renderedOrder();
    expect(chatsShown).toEqual(["Stale", "Fresh", "Chatty"]);
    const bar = one<HTMLButtonElement>('[data-testid="archived-bar"]');
    expect(bar.textContent).toContain("1");
    // Its chevron points the other way in a right-to-left layout.
    expect(bar.querySelectorAll("svg.lk-icon-flip-rtl")).toHaveLength(1);

    // The archive opens in the same panel, with a way back.
    bar.click();
    expect(all('[data-testid="archive-header"]')).toHaveLength(1);
    expect(all('[data-testid="archived-bar"]')).toHaveLength(0);
    const archiveShown = renderedOrder();
    expect(archiveShown).toEqual(["Parked"]);
    // Everything here is archived, so nothing is drawn dimmed or marked.
    expect(all(".lk-row.archived")).toHaveLength(0);

    // The two lists, read in turn, are the rule's order.
    expect([...chatsShown, ...archiveShown]).toEqual(ruleOrder());
    // And it is not the order a time-only comparator would give, which is what
    // makes the agreement above evidence rather than a coincidence.
    expect([...chatsShown, ...archiveShown]).not.toEqual(["Fresh", "Parked", "Chatty", "Stale"]);

    one<HTMLButtonElement>('[data-testid="archive-header"] button').click();
    expect(all('[data-testid="archive-header"]')).toHaveLength(0);
    expect(renderedOrder()).toEqual(["Stale", "Fresh", "Chatty"]);
  });

  it("lists archived matches dimmed under the divider in a search", () => {
    const live = conversation("Live team", 1, 2_000);
    const parked = conversation("Parked team", 2, 1_000);
    archive(parked);
    groups = [live, parked];

    mountList();
    const search = one<HTMLInputElement>('input[type="search"]');
    search.value = "team";
    search.dispatchEvent(new Event("input", { bubbles: true }));

    expect(all('[data-testid="archived-bar"]')).toHaveLength(0);
    expect(all('[data-testid="archived-divider"]')).toHaveLength(1);
    expect(renderedOrder()).toEqual(["Live team", "Parked team"]);
    const dimmed = all(".lk-row.archived .lk-row-title").map((el) => el.textContent);
    expect(dimmed).toEqual(["Parked team"]);
  });

  it("goes back from the archive on Escape", () => {
    const live = conversation("Live", 1, 2_000);
    const parked = conversation("Parked", 2, 1_000);
    archive(parked);
    groups = [live, parked];

    mountList();
    one<HTMLButtonElement>('[data-testid="archived-bar"]').click();
    expect(renderedOrder()).toEqual(["Parked"]);

    // Escape goes back as the back control does.
    document.dispatchEvent(new KeyboardEvent("keydown", { key: "Escape", bubbles: true }));
    expect(all('[data-testid="archive-header"]')).toHaveLength(0);
    expect(renderedOrder()).toEqual(["Live"]);
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

describe("before the stores are read (navigation.md 2.4)", () => {
  it("draws no row and no empty state", () => {
    groups = [conversation("Oldest", 1, 1_000), conversation("Newest", 2, 3_000)];
    listReady = false;

    mountList();

    expect(renderedOrder()).toEqual([]);
    expect(all(".lk-empty")).toHaveLength(0);
  });

  it("with nothing to list, the empty state waits for the read too", () => {
    groups = [];
    listReady = false;
    mountList();
    expect(all(".lk-empty")).toHaveLength(0);
  });

  it("once read, an empty list shows its empty state (the selector above is real)", () => {
    groups = [];
    listReady = true;
    mountList();
    expect(all(".lk-empty")).toHaveLength(1);
  });
});

describe("the rows stay the same elements", () => {
  it("keeps every row's element when the selection changes", () => {
    const a = conversation("Alpha", 1, 3_000);
    const b = conversation("Beta", 2, 2_000);
    const c = conversation("Gamma", 3, 1_000);
    groups = [a, b, c];
    const [active, setActive] = createSignal<string | null>(null);
    renderUi(() => (
      <MemoryRouter>
        <Route path="/" component={() => <ChatList activeGroupIdB64={active()} />} />
      </MemoryRouter>
    ));
    const before = all<HTMLElement>(".lk-pane-body > .lk-row");
    expect(before).toHaveLength(3);

    // The layout hands in the id as the URL carries it: the conversation id,
    // URL-safe base64 without padding.
    setActive(base64UrlEncodeNoPad(new Uint8Array(32).fill(2)));
    const after = all<HTMLElement>(".lk-pane-body > .lk-row");
    expect(after).toHaveLength(3);
    after.forEach((el, i) => expect(el).toBe(before[i]));
    expect(after.map((el) => el.classList.contains("selected"))).toEqual([false, true, false]);
  });

  it("keeps the other rows' elements when the list changes elsewhere", () => {
    const a = conversation("Alpha", 1, 3_000);
    const b = conversation("Beta", 2, 2_000);
    const [list, setList] = createSignal<ActiveGroup[]>([a, b]);
    groupsAccessor = list;
    mountList();
    const before = all<HTMLElement>(".lk-pane-body > .lk-row");
    expect(renderedOrder()).toEqual(["Alpha", "Beta"]);

    // Every row object is built again on this change; the elements are not.
    setList([a, b, conversation("Older", 3, 1_000)]);
    expect(renderedOrder()).toEqual(["Alpha", "Beta", "Older"]);
    const after = all<HTMLElement>(".lk-pane-body > .lk-row");
    expect(after[0]).toBe(before[0]);
    expect(after[1]).toBe(before[1]);
  });
});

describe("hovering a row changes no layout", () => {
  it("lays the actions over the row in every row, out of its layout", () => {
    installStylesheet();
    groups = [conversation("Alpha", 1, 3_000), conversation("Beta", 2, 2_000)];
    mountList();
    const rows = all<HTMLElement>(".lk-pane-body > .lk-row");
    expect(rows).toHaveLength(2);
    for (const row of rows) {
      // Present before any hover, so showing it inserts nothing.
      const actions = one<HTMLElement>(".lk-row-actions", row);
      expect(getComputedStyle(actions).position).toBe("absolute");
      expect(getComputedStyle(row).position).toBe("relative");
    }
  });
});

describe("the open conversation's row is selected", () => {
  // Through the real layout and the real route patterns, at the URL each
  // kind opens at: the layout reads the id out of the URL, so a row that
  // compares another form of it (standard base64, padded) is never selected.
  const CHANNEL_HEX = "0123456789abcdef0123456789abcdef";
  afterEach(() => vi.restoreAllMocks());

  function mountAt(url: string): void {
    renderUi(() => (
      <MemoryRouter
        root={(props) => <>{props.children}</>}
        history={(() => {
          const h = createMemoryHistory();
          h.set({ value: url, replace: true, scroll: false });
          return h;
        })()}
      >
        <Route path="/" component={ChatsLayout}>
          <Route path="/" component={() => null} />
          <Route path={chatRoutePattern} component={() => null} />
          <Route path={channelRoutePattern} component={() => null} />
          <Route path={paths.groups} component={() => null} />
          <Route path={`${paths.groups}${groupChatRoutePattern}`} component={() => null} />
        </Route>
      </MemoryRouter>
    ));
  }

  function selectedTitles(): string[] {
    return all(".lk-pane-body > .lk-row.selected .lk-row-title").map((el) => el.textContent ?? "");
  }

  function withChannel(): void {
    vi.spyOn(channelsState, "channels").mockReturnValue([
      { id: CHANNEL_HEX, localName: "News", localDescription: "", addedAtMs: 500 },
    ]);
  }

  it("at /groups/<id>", () => {
    withChannel();
    const group = conversation("Team", 1, 3_000);
    const chat = conversation("Ann", 2, 2_000);
    groups = [group, chat];
    const slug = (id: number): string => base64UrlEncodeNoPad(new Uint8Array(32).fill(id));

    mountAt(paths.groupChat(slug(1)));
    expect(selectedTitles()).toEqual(["Team"]);
  });

  it("at /chat/<id>", () => {
    withChannel();
    groups = [conversation("Team", 1, 3_000), conversation("Ann", 2, 2_000)];
    mountAt(paths.chat(base64UrlEncodeNoPad(new Uint8Array(32).fill(2))));
    expect(selectedTitles()).toEqual(["Ann"]);
  });

  it("at /channel/<id>", () => {
    withChannel();
    groups = [conversation("Team", 1, 3_000), conversation("Ann", 2, 2_000)];
    mountAt(paths.channel(CHANNEL_HEX));
    expect(all(".lk-pane-body > .lk-row").length).toBe(3);
    expect(selectedTitles()).toEqual(["News"]);
  });

  it("nowhere at /", () => {
    groups = [conversation("Team", 1, 3_000)];
    mountAt("/");
    expect(all(".lk-pane-body > .lk-row")).toHaveLength(1);
    expect(selectedTitles()).toEqual([]);
  });
});

describe("the order of the pins (navigation.md 2.2.1)", () => {
  const idOf = (n: number): Uint8Array => new Uint8Array(32).fill(n);

  function menuItems(): string[] {
    return all('.lk-menu [role="menuitem"]').map((el) => el.textContent?.trim() ?? "");
  }

  function openMenuOn(title: string): void {
    const rowEl = all(".lk-row").find((el) => el.querySelector(".lk-row-title")?.textContent === title);
    if (rowEl === undefined) throw new Error(`no row ${title}`);
    rowEl.dispatchEvent(new MouseEvent("contextmenu", { bubbles: true, cancelable: true, clientX: 5, clientY: 5 }));
  }

  it("draws the pins in the stored order, whatever their times say", () => {
    const a = conversation("A", 1, 1_000);
    const b = conversation("B", 2, 9_000);
    const c = conversation("C", 3, 5_000);
    groups = [a, b, c];
    pin(a);
    pin(b);
    pin(c);
    pinOrder = { groupIds: [idOf(3), idOf(1), idOf(2)] };
    mountList();
    expect(renderedOrder()).toEqual(["C", "A", "B"]);
  });

  it("offers Move up and Move down right after Unpin, only where they apply", () => {
    const a = conversation("A", 1, 1_000);
    const b = conversation("B", 2, 2_000);
    const c = conversation("C", 3, 3_000);
    groups = [a, b, c];
    pin(a);
    pin(b);
    pin(c);
    pinOrder = { groupIds: [idOf(1), idOf(2), idOf(3)] };
    mountList();

    openMenuOn("B");
    const items = menuItems();
    const unpin = items.indexOf("Unpin");
    expect(unpin).toBeGreaterThanOrEqual(0);
    expect(items.slice(unpin + 1, unpin + 3)).toEqual(["Move up", "Move down"]);

    document.dispatchEvent(new KeyboardEvent("keydown", { key: "Escape", bubbles: true }));
    openMenuOn("A");
    expect(menuItems()).not.toContain("Move up");
    expect(menuItems()).toContain("Move down");
  });

  it("Move up writes the new order and the list shows it at once", () => {
    const a = conversation("A", 1, 1_000);
    const b = conversation("B", 2, 2_000);
    groups = [a, b];
    pin(a);
    pin(b);
    pinOrder = { groupIds: [idOf(1), idOf(2)] };
    mountList();

    openMenuOn("B");
    const moveUp = all<HTMLButtonElement>('.lk-menu [role="menuitem"]').find(
      (el) => el.textContent?.trim() === "Move up",
    );
    moveUp?.click();
    expect(orderWrites).toEqual([[idOf(2), idOf(1)]]);
    expect(renderedOrder()).toEqual(["B", "A"]);
  });

  it("one pin has nothing to reorder", () => {
    const a = conversation("A", 1, 1_000);
    groups = [a, conversation("B", 2, 2_000)];
    pin(a);
    mountList();
    openMenuOn("A");
    expect(menuItems()).not.toContain("Move up");
    expect(menuItems()).not.toContain("Move down");
  });
});
