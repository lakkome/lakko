// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * The subscriber view across a change of channel.
 *
 * The router keeps one `ChannelSubscribe` while only `:id` changes. The view
 * starts a bootstrap that spans several awaits (hydration, the key lookup, a
 * fresh key, the registration). Before the view was keyed on the id that
 * bootstrap re-read the route param after each await, so opening channel A
 * and switching to channel B while A's key was being minted stored A's fresh
 * key under B: B's valid key was overwritten and every seed sealed to it
 * orphaned. This drives exactly that switch.
 */
import { createSignal } from "solid-js";
import { beforeEach, describe, expect, it, vi } from "vitest";

const h = vi.hoisted(() => ({
  setId: null as null | ((id: string) => void),
  id: null as null | (() => string),
  keys: new Map<string, { secret: Uint8Array; public: Uint8Array }>(),
  stored: [] as Array<{ channel: string; pub: number }>,
  subscribed: [] as Array<{ channel: string; pubHex: string }>,
  epochAsked: [] as string[],
  fresh: [] as Array<(key: { secret: Uint8Array; public: Uint8Array }) => void>,
}));

vi.mock("@solidjs/router", () => ({
  useParams: () => ({
    get id(): string {
      return h.id === null ? "" : h.id();
    },
  }),
  useLocation: () => ({ pathname: "/channels/x/subscribe", state: undefined }),
  useNavigate: () => () => undefined,
  Navigate: () => null,
}));
vi.mock("../channels/api", () => ({
  apiError: { is: () => false },
  channelErrorText: () => "",
  createChannel: vi.fn(),
  publishPost: vi.fn(),
  fetchPosts: async () => ({ posts: [] }),
  fetchEnvelopes: async () => [],
  fetchEpoch: async (id: string) => {
    h.epochAsked.push(id);
    return { epoch: 1 };
  },
  subscribe: async (channel: string, pubHex: string) => {
    h.subscribed.push({ channel, pubHex });
  },
}));
vi.mock("../channels/composer", () => ({
  bytesToHex: (b: Uint8Array) => [...b].map((x) => x.toString(16).padStart(2, "0")).join(""),
  composePost: vi.fn(),
  decodeAndDecryptPost: vi.fn(),
  freshPublisherKeys: vi.fn(),
  // Held until the test releases it: the await the switch lands in.
  freshSubscriberKeys: () =>
    new Promise<{ secret: Uint8Array; public: Uint8Array }>((resolve) => {
      h.fresh.push(resolve);
    }),
  isCurrentSubscriberKey: async () => true,
  openSubscriberSeedEnvelope: vi.fn(),
  utf8Encode: (s: string) => new TextEncoder().encode(s),
}));
vi.mock("../state/channels-state", () => {
  const empty = new Map();
  return {
    awaitChannelsHydration: async () => undefined,
    channelsState: {
      channels: () => [],
      keysByChannel: () => empty,
      subscriberKeysByChannel: () => h.keys,
      subscribedSeedsByChannel: () => empty,
      epochByChannel: () => empty,
      metaByChannel: () => empty,
      postsByChannel: () => empty,
      hydrated: () => true,
    },
    channelsActions: {
      setSubscriberKeys: (channel: string, keys: { secret: Uint8Array; public: Uint8Array }) => {
        h.stored.push({ channel, pub: keys.public[0] ?? -1 });
        h.keys.set(channel, keys);
      },
      addChannel: () => undefined,
      setEpoch: () => undefined,
      setPosts: () => undefined,
      setSubscribedSeedForEpoch: () => undefined,
      currentSeed: () => undefined,
    },
  };
});
vi.mock("../l10n", () => ({ t: (key: string) => key }));
vi.mock("../l10n/live-text", async () => {
  const solid = await import("solid-js");
  return { createLiveText: () => solid.createSignal<(() => string) | null>(null) };
});
vi.mock("../channels/own-member-card", () => ({ useOwnMemberCard: () => undefined }));
vi.mock("../channels/channel-row", () => ({ channelTitle: () => "", displayHandle: () => "" }));
vi.mock("../channels/PostCard", () => ({
  PostCard: () => null,
  isHiddenPostDecode: () => false,
  toPostDecode: vi.fn(),
}));
vi.mock("../channels/ManagePanel", () => ({ ManagePanel: () => null }));
vi.mock("../channels/MembersPanel", () => ({ MembersPanel: () => null }));
vi.mock("../channels/PublishComposer", () => ({ PublishComposer: () => null }));
vi.mock("../settings/MembersCanFindMeSwitch", () => ({ MembersCanFindMeSwitch: () => null }));
vi.mock("../ui/ModalLayer", () => ({ ModalLayer: () => null }));
vi.mock("../ui/Avatar", () => ({ Avatar: () => null }));
vi.mock("../ui/icons", () => ({ Icon: () => null }));
vi.mock("../ui/confirm-dialog", () => ({ requestConfirm: vi.fn() }));
vi.mock("../auth/AuthContext", () => ({
  useAuth: () => ({ identitySecrets: () => null, state: () => ({ kind: "anon" }) }),
}));
vi.mock("../chat/ChatContext", () => ({ useChats: () => ({ accountIdentity: () => null }) }));
vi.mock("../feed/ChannelPostSync", () => ({ requestChannelPostSync: () => undefined }));
vi.mock("../feed/feed-marks-store", () => ({ markChannelRead: async () => undefined }));
vi.mock("../wasm", () => ({ loadWasm: async () => ({}) }));
vi.mock("../ds/client", () => ({ DsClient: class {} }));
vi.mock("../channels/copublisher", () => ({
  pollForCoPublishWelcome: vi.fn(),
  requestToCoPublish: vi.fn(),
}));
vi.mock("../channels/copublisher-join-failure", () => ({ coPublisherJoinFailure: vi.fn() }));
vi.mock("./join-group-failure", () => ({ JOIN_TRY_AGAIN: () => "" }));
vi.mock("../channels/channel-discovery", () => ({
  DiscoveryHttpError: class extends Error {},
  registerChannelHandle: vi.fn(),
}));
vi.mock("../channels/legacy-v1", () => ({ LegacyChannelError: class extends Error {} }));
vi.mock("../channels/ui-cache", () => ({ knownSubscriberCount: () => undefined }));
vi.mock("../chat/conversation-info", () => ({
  channelInfoSections: () => [],
  infoHeading: () => "",
  infoOpenLabel: () => "",
}));
vi.mock("../chat/ui-rules", () => ({ uiRules: () => null }));
vi.mock("../channels/channel-meta", () => ({
  foldChannelMeta: async () => undefined,
  publishChannelMeta: vi.fn(),
}));
vi.mock("../channels/member-cards", () => ({ forgetUploadedCard: vi.fn() }));
vi.mock("../channels/unsubscribe", () => ({ leaveChannelOnServer: vi.fn() }));
vi.mock("../tokens/token-copy", () => ({ anonTokenMessage: () => null }));

import { renderUi } from "../test/ui";
import { ChannelSubscribe } from "./Channels";

function key(tag: number): { secret: Uint8Array; public: Uint8Array } {
  return { secret: new Uint8Array([tag]), public: new Uint8Array([tag]) };
}

beforeEach(() => {
  h.keys.clear();
  h.stored.length = 0;
  h.subscribed.length = 0;
  h.epochAsked.length = 0;
  h.fresh.length = 0;
  const [id, setId] = createSignal("aa");
  h.id = id;
  h.setId = setId;
});

describe("ChannelSubscribe, switching channel while the first is still registering", () => {
  it("stores channel A's fresh key under A and leaves channel B's key alone", async () => {
    // B is already followed with a valid key; A is new.
    const keyB = key(0xb0);
    h.keys.set("bb", keyB);

    renderUi(() => <ChannelSubscribe />);
    // A's bootstrap is now waiting for its fresh key.
    await vi.waitFor(() => expect(h.fresh).toHaveLength(1));

    // The person picks channel B in the list pane: same route component.
    h.setId?.("bb");
    // B's own bootstrap runs to its registration with B's existing key.
    await vi.waitFor(() =>
      expect(h.subscribed).toContainEqual({ channel: "bb", pubHex: "b0" }),
    );

    // A's key arrives late.
    h.fresh[0]?.(key(0xa0));
    await vi.waitFor(() =>
      expect(h.subscribed).toContainEqual({ channel: "aa", pubHex: "a0" }),
    );

    // A's key under A; nothing was ever stored under B.
    expect(h.stored).toEqual([{ channel: "aa", pub: 0xa0 }]);
    expect(h.keys.get("bb")).toBe(keyB);
    // And B was never registered with A's key.
    expect(h.subscribed).not.toContainEqual({ channel: "bb", pubHex: "a0" });
    // Each channel was caught up from its own state.
    expect(h.epochAsked).toContain("aa");
    expect(h.epochAsked).toContain("bb");
  });
});
