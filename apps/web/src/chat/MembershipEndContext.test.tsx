// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

// @vitest-environment jsdom

/**
 * ADR-0087 section 9 — the seam, on a real Solid root.
 *
 * `membership-end.ts` and `membership-end-runtime.ts` have the rules and the
 * order; this drives what binds them: that a commit applied under the group's
 * MLS lock reaches the trigger, that a device whose own leaf was retired is
 * left alone, that a heard register is judged BEFORE anything stops, and which
 * delete each path calls. The contexts around it are stood in for by module id;
 * the lock, the store, the runtime and the provider are the real ones.
 */
import { createSignal } from "solid-js";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import { renderUi } from "../test/ui";

// ----- what the provider reads ----------------------------------------------

const log: string[] = [];

interface FakeGroup {
  groupId: Uint8Array;
  isActive: boolean;
  epoch: bigint;
  roster: string[];
}

const ids = {
  group: new Uint8Array(32).fill(0xaa),
  own: new Uint8Array(32).fill(1),
  other: new Uint8Array(32).fill(2),
};

const world = vi.hoisted(() => ({
  ended: null as null | ((v: readonly { ref: { kind: number; id: Uint8Array }; epoch: bigint }[]) => void),
  groups: null as null | ((v: readonly unknown[]) => void),
  list: null as null | (() => readonly unknown[]),
  ready: null as null | ((v: boolean) => void),
  row: null as unknown,
  leaveRequested: false,
}));

function keyOf(id: Uint8Array): string {
  return btoa(String.fromCharCode(...id));
}

function fakeGroup(over: Partial<FakeGroup> = {}): unknown {
  const g: FakeGroup = {
    groupId: ids.group,
    isActive: true,
    epoch: 5n,
    roster: [keyOf(ids.own), keyOf(ids.other)],
    ...over,
  };
  return {
    groupId: g.groupId,
    isActive: g.isActive,
    epoch: g.epoch,
    groupKind: () => 2,
    members: () =>
      g.roster.map((k, i) => ({
        leafIndex: i,
        signaturePublicKey: Uint8Array.from(atob(k), (c) => c.charCodeAt(0)),
      })),
    deleteLocally: () => {
      log.push("deleteLocally");
      if (g.isActive) throw Object.assign(new Error("still"), { refusalReason: "groupStillActive", refusalGroupId: ids.group });
    },
    deleteAfterEndedElsewhere: (_id: unknown, epoch: bigint) => {
      log.push(`deleteAfterEndedElsewhere ${epoch}`);
    },
    free: () => undefined,
  };
}

vi.mock("../auth/AuthContext", () => ({
  useAuth: () => ({ state: () => ({ kind: "session", account: { accountId: "acct" } }) }),
}));
vi.mock("../auth/key-store", () => ({ getDeviceKey: async () => ({ fake: "key" }) }));
vi.mock("./ChatContext", () => ({
  useChats: () => ({
    groups: () => (world.list as () => readonly unknown[])(),
    get: (id: string) =>
      (world.list as () => readonly { groupIdB64: string }[])().find((g) => g.groupIdB64 === id),
    remove: (id: string) => {
      log.push("chats.remove");
      (world.groups as (v: readonly unknown[]) => void)(
        (world.list as () => readonly { groupIdB64: string }[])().filter((g) => g.groupIdB64 !== id),
      );
    },
    identity: () => ({ accountPublicKey: new Uint8Array(32).fill(1), signaturePublicKey: new Uint8Array(32).fill(1) }),
    accountIdentity: () => ({ signaturePublicKey: new Uint8Array(32).fill(1) }),
    ownPredecessorIdentities: () => [],
    persistSnapshotDurable: async () => {
      log.push("persist");
    },
  }),
}));
vi.mock("../devices/sync/DeviceGroupSyncContext", () => ({
  useDeviceGroupSync: () => ({
    ready: readyAccessor(),
    endedConversations: endedAccessor(),
    writeConversationEnd: async (_ref: unknown, state: string, epoch: bigint) => {
      log.push(`register ${state} ${epoch}`);
      return true;
    },
    saveDraft: async (_groupId: Uint8Array, text: string) => {
      log.push(`draft ${text}`);
    },
    saved: () => [],
    folders: () => [],
    getFlags: () => undefined,
    getDraft: () => undefined,
    getReadCursor: () => undefined,
    order: () => null,
    applyFolderEdits: async () => undefined,
  }),
}));
vi.mock("./use-lineage", () => ({
  useLineage: () => ({
    resolve: () => undefined,
    conversationIdOf: (id: Uint8Array) => id,
    conversationHexOf: () => "aa",
  }),
}));
vi.mock("../profile/ProfileContext", () => ({
  useProfiles: () => ({
    resolve: () => ({}),
    groupShrank: async () => {
      log.push("rotate reach keys");
    },
  }),
}));
vi.mock("../profile/member-label", () => ({ memberNameOf: () => "Someone" }));
vi.mock("./GroupNameContext", () => ({
  useGroupNames: () => ({ nameOf: () => "Book club", forgetGroup: async () => undefined }),
}));
vi.mock("./GroupClosedContext", () => ({ useGroupClosed: () => ({ forget: async () => undefined }) }));
vi.mock("./PinContext", () => ({ usePins: () => ({ forgetConversation: async () => undefined }) }));
vi.mock("../discover/SharedChannelsContext", () => ({
  useSharedChannels: () => ({ forgetGroup: async () => undefined }),
}));
vi.mock("./ReadWatermarkContext", () => ({ useReadWatermarks: () => ({ forgetGroup: () => undefined }) }));
vi.mock("../calls/CallLogContext", () => ({ useCallLog: () => ({ forgetGroup: () => undefined }) }));
vi.mock("./membership-end-idb", () => ({
  IdbMembershipEndBackend: class {
    async load(): Promise<unknown> {
      return world.row;
    }
    async save(_a: string, row: unknown): Promise<void> {
      world.row = row;
    }
  },
}));
vi.mock("./membership-wipe", () => ({
  groupWipeSteps: () => [],
  runWipeSteps: async () => {
    log.push("wipe stores");
    return [];
  },
}));
vi.mock("./membership-ended", async (original) => ({
  ...(await original<typeof import("./membership-ended")>()),
  loadLeaveRequestedAt: () => (world.leaveRequested ? 1 : undefined),
}));
vi.mock("../push/group-routing-store", () => ({
  getGroupRoutingSecretForAccount: async () => new Uint8Array(32),
}));
vi.mock("../push/group-routing", () => ({
  retireOldPushGid: async () => {
    log.push("retire push");
  },
}));
vi.mock("../channels/unsubscribe", () => ({
  leaveChannelOnServer: async (id: string) => {
    log.push(`server leave ${id}`);
    return { kind: "left" };
  },
}));
vi.mock("../channels/member-cards", () => ({ forgetUploadedCard: () => undefined }));
vi.mock("../channels/channel-subscription-register", () => ({
  leaveChannelSubscriptionHere: async (_account: string, id: string) => {
    log.push(`subscription left ${id}`);
  },
}));
vi.mock("../state/channels-state", () => ({
  channelsActions: {
    clearSubscriberKeys: (id: string) => log.push(`clear keys ${id}`),
    removeChannel: (id: string) => log.push(`remove channel ${id}`),
  },
  channelsState: {
    channels: () => [],
    keysByChannel: () => new Map(),
    metaByChannel: () => new Map(),
    orphanedByKeyReset: () => new Set(),
    subscriberKeysByChannel: () =>
      new Map([["f0a0c3f1abdc44d9505e4f76fed3f969", { secret: new Uint8Array(32), publicKey: new Uint8Array(32) }]]),
  },
}));
vi.mock("../feed/feed-marks-store", () => ({ updateFeedMarks: async () => undefined }));
vi.mock("../devices/state-bundle", () => ({
  loadKnownGroupIds: () => [],
  saveKnownGroupIds: () => undefined,
}));

import { withGroupMlsLock } from "../mls/group-lock";
import { MembershipEndProvider, useMembershipEnd, type MembershipEndApi } from "./MembershipEndContext";
import { draftToRestore } from "./membership-end";
import { blockGroupInMessageLog, withoutBlockedSlots } from "./message-log-store";
import type { ActiveGroup } from "./ChatContext";

let readyAccessor: () => () => boolean;
let endedAccessor: () => () => readonly { ref: { kind: number; id: Uint8Array }; epoch: bigint }[];

const GROUP_ID = keyOf(ids.group);

function entry(group: unknown): ActiveGroup {
  return { groupIdB64: GROUP_ID, label: "Book club", group, createdAtMs: 1, joinGeneration: 0 } as ActiveGroup;
}

let api: MembershipEndApi;

function mount(initial: readonly unknown[], options: { ready?: boolean } = {}): void {
  const [list, setList] = createSignal<readonly unknown[]>(initial);
  const [ready, setReady] = createSignal(options.ready ?? true);
  const [ended, setEnded] = createSignal<
    readonly { ref: { kind: number; id: Uint8Array }; epoch: bigint }[]
  >([]);
  world.list = list;
  world.groups = setList;
  world.ready = setReady;
  world.ended = setEnded;
  readyAccessor = () => ready;
  endedAccessor = () => ended;
  renderUi(() => (
    <MembershipEndProvider>
      {(() => {
        api = useMembershipEnd();
        return null;
      })()}
    </MembershipEndProvider>
  ));
}

const quiet = async (): Promise<void> => {
  for (let i = 0; i < 12; i += 1) await new Promise((r) => setTimeout(r, 0));
};

beforeEach(() => {
  log.length = 0;
  world.row = null;
  world.leaveRequested = false;
  vi.spyOn(console, "warn").mockImplementation(() => undefined);
});

afterEach(() => {
  vi.restoreAllMocks();
});

describe("Unsubscribe from a channel", () => {
  // Seen live on Android 2026-11-24: a channel opened from Chats search stayed
  // in the list after Unsubscribe. On web the info's Unsubscribe runs this and
  // then leaves the route for Chats (`routes/ChannelConversation.tsx`), the
  // same route whichever way the channel was opened.
  it("takes the channel out of the list and its posts off this device, and tells the service", async () => {
    mount([]);
    // The provider loads its store first.
    await quiet();
    const id = "f0a0c3f1abdc44d9505e4f76fed3f969";
    await api.endChannel(id, "Night walk");
    await quiet();
    expect(log).toContain(`remove channel ${id}`);
    expect(log).toContain(`clear keys ${id}`);
    expect(log).toContain(`subscription left ${id}`);
    expect(log).toContain(`server leave ${id}`);
    // The register is written before the channel goes, so no reconcile adds it back.
    expect(log.indexOf(`subscription left ${id}`)).toBeLessThan(log.indexOf(`remove channel ${id}`));
  });
});

describe("after a commit", () => {
  it("a person out of the group loses it, in order, the registry entry last", async () => {
    const group = { current: fakeGroup() };
    mount([entry(group.current)]);
    await quiet();
    log.length = 0;
    // The commit that removes every leaf of this account lands.
    world.groups?.([entry(fakeGroup({ isActive: false, epoch: 6n, roster: [keyOf(ids.other)] }))]);
    await withGroupMlsLock(GROUP_ID, async () => undefined);
    await quiet();
    expect(log).toEqual([
      "register ended 6",
      "retire push",
      "rotate reach keys",
      "wipe stores",
      "deleteLocally",
      "persist",
      "chats.remove",
      "persist",
    ]);
    expect(api.notices()).toMatchObject([{ title: "Book club" }]);
  });

  it("a device whose own leaf was retired while the account stays keeps everything", async () => {
    mount([entry(fakeGroup({ isActive: false, roster: [keyOf(ids.own), keyOf(ids.other)] }))]);
    await withGroupMlsLock(GROUP_ID, async () => undefined);
    await quiet();
    expect(log).toEqual([]);
    expect(api.isEnding(GROUP_ID)).toBe(false);
  });

  it("an active group is left alone", async () => {
    mount([entry(fakeGroup())]);
    await withGroupMlsLock(GROUP_ID, async () => undefined);
    await quiet();
    expect(log).toEqual([]);
  });

  it("a person who asked to leave gets no notice", async () => {
    world.leaveRequested = true;
    mount([entry(fakeGroup({ isActive: false, roster: [keyOf(ids.other)] }))]);
    await quiet();
    expect(log).toContain("chats.remove");
    expect(api.notices()).toEqual([]);
  });

  it("nor when another device's register reached it first", async () => {
    world.leaveRequested = true;
    mount([entry(fakeGroup({ epoch: 5n }))]);
    await quiet();
    world.ended?.([{ ref: { kind: 2, id: ids.group }, epoch: 7n }]);
    await quiet();
    expect(log).toContain("chats.remove");
    expect(api.notices()).toEqual([]);
  });

  it("takes the view's unsent text into the notice", async () => {
    mount([entry(fakeGroup())]);
    await quiet();
    const stop = api.registerOpenDraft(GROUP_ID, () => "half a sentence");
    world.groups?.([entry(fakeGroup({ isActive: false, roster: [keyOf(ids.other)] }))]);
    await quiet();
    expect(api.notices()[0]?.draft).toBe("half a sentence");
    stop();
  });

  it("ends a call in that group", async () => {
    mount([entry(fakeGroup())]);
    const stopped: string[] = [];
    api.registerCallStop((g) => stopped.push(g));
    world.groups?.([entry(fakeGroup({ isActive: false, roster: [keyOf(ids.other)] }))]);
    await quiet();
    expect(stopped).toEqual([GROUP_ID]);
  });
});

describe("the launch sweep", () => {
  it("catches a commit applied just before a crash", async () => {
    mount([entry(fakeGroup({ isActive: false, epoch: 6n, roster: [keyOf(ids.other)] }))]);
    await quiet();
    expect(log).toContain("register ended 6");
    expect(log).toContain("chats.remove");
  });

  it("waits for the synced state", async () => {
    mount([entry(fakeGroup({ isActive: false, roster: [keyOf(ids.other)] }))], { ready: false });
    await quiet();
    expect(log).toEqual([]);
    world.ready?.(true);
    await quiet();
    expect(log).toContain("chats.remove");
  });
});

describe("a heard register", () => {
  const register = (epoch: bigint) => [{ ref: { kind: 2, id: ids.group }, epoch }];

  it("ends a group this device had not caught up with, on the register's word", async () => {
    mount([entry(fakeGroup({ epoch: 5n }))]);
    await quiet();
    log.length = 0;
    world.ended?.(register(7n));
    await quiet();
    expect(log).toEqual([
      "retire push",
      "wipe stores",
      "deleteAfterEndedElsewhere 7",
      "persist",
      "chats.remove",
      "persist",
    ]);
    expect(api.notices()).toHaveLength(1);
  });

  it("a group active above the register's epoch came back: nothing stops, joined is sent", async () => {
    mount([entry(fakeGroup({ epoch: 9n }))]);
    await quiet();
    log.length = 0;
    world.ended?.(register(7n));
    await quiet();
    expect(log).toEqual(["register joined 9"]);
    expect(api.isEnding(GROUP_ID)).toBe(false);
    expect(api.notices()).toEqual([]);
  });

  it("a group not held is nothing to act on", async () => {
    mount([]);
    await quiet();
    world.ended?.(register(7n));
    await quiet();
    expect(log).toEqual([]);
  });

  it("read again after the wipe it is idempotent", async () => {
    mount([entry(fakeGroup({ epoch: 5n }))]);
    await quiet();
    world.ended?.(register(7n));
    await quiet();
    log.length = 0;
    world.ended?.(register(7n));
    world.groups?.([]);
    await quiet();
    expect(log).toEqual([]);
  });
});

describe("after a join", () => {
  it("publishes joined at the Welcome's epoch and takes rows again", async () => {
    mount([]);
    await quiet();
    blockGroupInMessageLog("acct", "aa");
    const joined = entry(fakeGroup({ epoch: 11n }));
    await api.noteJoined(joined);
    expect(log).toContain("register joined 11");
  });

  it("a Welcome that lands during an end closes the record without a delete", async () => {
    mount([entry(fakeGroup())]);
    await quiet();
    world.groups?.([entry(fakeGroup({ isActive: false, epoch: 6n, roster: [keyOf(ids.other)] }))]);
    // The group is back before the wipe's check runs.
    world.groups?.([entry(fakeGroup({ epoch: 8n }))]);
    await quiet();
    expect(log).not.toContain("chats.remove");
  });

  it("a Welcome never lands between the delete and the registry entry going", async () => {
    mount([entry(fakeGroup())]);
    await quiet();
    log.length = 0;
    world.groups?.([entry(fakeGroup({ isActive: false, epoch: 6n, roster: [keyOf(ids.other)] }))]);
    const join = api.joinAlone(async () => {
      log.push("join starts");
      await quiet();
      log.push("join ends");
    });
    await join;
    await api.settle();
    await quiet();
    const started = log.indexOf("join starts");
    const ended = log.indexOf("join ends");
    const between = log.slice(started + 1, ended);
    expect(between).not.toContain("wipe stores");
    expect(between).not.toContain("deleteLocally");
    expect(between).not.toContain("chats.remove");
    expect(log).toContain("chats.remove");
  });

  it("settling waits for an end a commit has only just set off", async () => {
    mount([entry(fakeGroup())]);
    await quiet();
    world.groups?.([entry(fakeGroup({ isActive: false, epoch: 6n, roster: [keyOf(ids.other)] }))]);
    await api.settle();
    expect(log).toContain("chats.remove");
  });

  it("a join over a group this device was out of leaves nothing of it behind", async () => {
    mount([]);
    await quiet();
    log.length = 0;
    await api.startOver(entry(fakeGroup({ isActive: false, epoch: 6n, roster: [keyOf(ids.other)] })));
    expect(log).toContain("wipe stores");
  });

  it("a join over a group this device is still in wipes nothing", async () => {
    mount([]);
    await quiet();
    log.length = 0;
    await api.startOver(entry(fakeGroup()));
    await api.startOver(undefined);
    expect(log).not.toContain("wipe stores");
  });

  it("closes the notice: the sentence would be false", async () => {
    mount([entry(fakeGroup())]);
    await quiet();
    world.groups?.([entry(fakeGroup({ isActive: false, epoch: 6n, roster: [keyOf(ids.other)] }))]);
    await api.settle();
    expect(api.notices().length).toBe(1);
    await api.noteJoined(entry(fakeGroup({ epoch: 8n })));
    expect(api.notices().length).toBe(0);
  });

  it("added back while the dialog is up: the notice goes and its text returns to the composer", async () => {
    mount([entry(fakeGroup())]);
    await quiet();
    const stop = api.registerOpenDraft(GROUP_ID, () => "half a sentence");
    world.groups?.([entry(fakeGroup({ isActive: false, epoch: 6n, roster: [keyOf(ids.other)] }))]);
    await api.settle();
    expect(api.noticeForRoute(GROUP_ID)?.draft).toBe("half a sentence");
    stop();
    await api.noteJoined(entry(fakeGroup({ epoch: 8n })));
    await quiet();
    expect(api.noticeForRoute(GROUP_ID)).toBeUndefined();
    expect(api.isEnding(GROUP_ID)).toBe(false);
    expect(log).toContain("draft half a sentence");
  });

  it("puts back only a draft that holds text", () => {
    expect(draftToRestore({ draft: "kept" })).toBe("kept");
    expect(draftToRestore({ draft: "  " })).toBeNull();
    expect(draftToRestore({ draft: null })).toBeNull();
    expect(draftToRestore(undefined)).toBeNull();
  });
});

describe("a crash", () => {
  it("is resumed from the stored record, at its stage", async () => {
    world.row = {
      records: [
        {
          key: `2:${"aa".repeat(32)}`,
          refKind: 2,
          idHex: "aa".repeat(32),
          segmentsB64: [GROUP_ID],
          routeIdsB64: [GROUP_ID],
          title: "Book club",
          left: false,
          origin: "applied",
          endedEpoch: "6",
          stage: "stopped",
          syncedCleared: false,
          atMs: 1,
        },
      ],
      notices: [],
      welcomes: [],
    };
    mount([entry(fakeGroup({ isActive: false, roster: [keyOf(ids.other)] }))]);
    await quiet();
    expect(log).toEqual(["wipe stores", "deleteLocally", "persist", "chats.remove", "persist"]);
    expect(api.isEnding(GROUP_ID)).toBe(false);
  });

  it("keeps the wiped group's rows out of the message log in this page", async () => {
    world.row = null;
    mount([entry(fakeGroup({ isActive: false, roster: [keyOf(ids.other)] }))]);
    await quiet();
    const hex = "aa".repeat(32);
    const written = withoutBlockedSlots("acct", {
      version: 5,
      groups: { [hex]: { messages: [], cursor: 0 } as never, bb: { messages: [], cursor: 0 } as never },
    });
    expect(Object.keys(written.groups)).toEqual(["bb"]);
  });
});
