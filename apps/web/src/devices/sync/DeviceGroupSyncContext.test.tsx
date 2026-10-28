// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * The sync provider across a change of account.
 *
 * The provider is mounted once for the page and is told when the session
 * ends. Its bootstrap for an account runs across several awaits; before it
 * re-checked its binding after each of them, a bootstrap for account A that
 * was still waiting for its DeviceGroup when A signed out went on to install
 * A's identity, group and synced state, mark the runtime ready and start a
 * poll loop - for nobody, or, once B had signed in, over B's session, where
 * B saw A's drafts and a publish would have written A's identity snapshot
 * under B's account id.
 */
import { createSignal } from "solid-js";
import { beforeEach, describe, expect, it, vi } from "vitest";

interface FakeHandle {
  readonly owner: string;
  free: ReturnType<typeof vi.fn>;
}

const h = vi.hoisted(() => ({
  state: null as null | (() => unknown),
  secrets: null as null | (() => unknown),
  ensures: [] as Array<{ account: string; resolve: (v: unknown) => void }>,
  loaded: [] as string[],
  persisted: [] as Array<{ owner: string; account: string }>,
  polled: [] as string[],
}));

vi.mock("../../auth/AuthContext", () => ({
  useAuth: () => ({
    state: () => h.state?.(),
    identitySecrets: () => h.secrets?.(),
  }),
}));
vi.mock("../../auth/device-identity-store", () => ({
  ensureDeviceIdentitySeed: async () => new Uint8Array(32),
}));
vi.mock("../../auth/key-store", () => ({ getDeviceKey: async () => ({}) }));
vi.mock("../../auth/device-attest-key", () => ({
  fetchDeviceAttestKey: async () => new Uint8Array(32),
}));
vi.mock("./sync-state-idb", () => ({
  loadSyncState: async (account: string) => {
    h.loaded.push(account);
    return { synced: new TextEncoder().encode(account), pending: [] };
  },
  saveSyncState: async () => undefined,
  syncStateGeneration: () => 0,
}));
vi.mock("../../ds/client", () => ({
  DsClient: class {
    ensureOwnerClaim(): Promise<void> {
      return Promise.resolve();
    }
    // The long poll never answers in this test.
    fetchGroup(groupId: Uint8Array): Promise<never> {
      h.polled.push(new TextDecoder().decode(groupId));
      return new Promise<never>(() => undefined);
    }
  },
  DsHttpError: class extends Error {},
  DsNetworkError: class extends Error {},
}));
vi.mock("../../ds/access", () => ({
  dsAccess: {
    registerGroupSource: () => () => undefined,
    registerOwnerSource: () => () => undefined,
  },
  takeSigner: () => null,
}));
vi.mock("../../wasm", () => ({
  loadWasm: async () => ({
    deviceGroupIdFor: (bytes: Uint8Array) => bytes,
    SyncedState: class {
      readonly owner = "empty";
      maxHlc(): bigint {
        return 0n;
      }
      free(): void {}
      toBytes(): Uint8Array {
        return new Uint8Array(0);
      }
      // ADR-0075 / ADR-0076: the two account registers the provider
      // publishes on every refresh.
      recoveryEmail(): string | undefined {
        return undefined;
      }
      ownUsername(): string | undefined {
        return undefined;
      }
      // ADR-0079: the link-record register, read on every refresh.
      allLinkRecords(): unknown[] {
        return [];
      }
    },
    LinkRecordKind: { Contact: 1, Call: 2 },
  }),
}));
vi.mock("../lifecycle", () => ({
  deviceGroupIdOf: async () => new TextEncoder().encode("device-group"),
  ensureDeviceGroup: (_bytes: Uint8Array, account: string) =>
    new Promise((resolve) => {
      h.ensures.push({ account, resolve });
    }),
  persistDeviceGroupSnapshot: async (identity: FakeHandle, account: string) => {
    h.persisted.push({ owner: identity.owner, account });
  },
  persistDeviceGroupSnapshotWithCursor: async () => 1,
  deviceGroupLineageSeed: async () => null,
  DeviceGroupNotJoinedError: class extends Error {
    readonly reason = "rejoin";
  },
}));
vi.mock("../device-group-snapshot-store", () => ({
  deleteDeviceGroupSnapshot: async () => undefined,
  dropDeviceGroupStateForRejoin: async () => undefined,
  getDeviceGroupJournalCursor: async () => 0,
  getDeviceGroupPending: async () => null,
  putDeviceGroupPending: async () => undefined,
  deleteDeviceGroupPending: async () => undefined,
}));
// ADR-0081 - the DeviceGroup commit outbox and the rejoin: nothing owed here.
vi.mock("../device-group-commit-runtime", () => ({
  commitDeviceGroupChange: async () => null,
  resendOwedDeviceGroupCommitFor: async () => null,
}));
vi.mock("./synced-projection", () => ({
  buildFolderChangeSet: vi.fn(),
  mergeRestoredFold: vi.fn(),
  openPersistedFold: (_wasm: unknown, bytes: Uint8Array) => {
    const owner = new TextDecoder().decode(bytes);
    return {
      owner,
      maxHlc: () => 0n,
      free: () => undefined,
      toBytes: () => bytes,
      recoveryEmail: () => undefined,
      ownUsername: () => undefined,
    };
  },
  projectSyncedState: (synced: { owner: string }) => ({
    drafts: { [`draft-of-${synced.owner}`]: { text: synced.owner } },
    flags: {},
    readCursors: {},
    order: null,
    folders: [],
    lineage: [],
  }),
}));
vi.mock("../../reach/service-envelope-intake", () => ({ receiveServiceEnvelope: vi.fn() }));
vi.mock("../../reach/service-envelope-outbox", () => ({
  flushServiceEnvelopes: async () => undefined,
  registerServiceEnvelopeSender: () => () => undefined,
  setServiceEnvelopeComposer: () => undefined,
  markServiceEnvelopeDirty: () => undefined,
}));
vi.mock("../../chat/folders/folder-model", () => ({
  bytesToHex: () => "",
  stampEdits: vi.fn(),
}));
vi.mock("./hlc", () => ({ mintHlc: () => 1n, observeHlc: () => undefined }));

import { renderUi } from "../../test/ui";
import {
  DeviceGroupSyncProvider,
  useDeviceGroupSync,
  type DeviceGroupSyncApi,
} from "./DeviceGroupSyncContext";

function handles(owner: string): { identity: FakeHandle; group: FakeHandle & Record<string, unknown> } {
  return {
    identity: { owner, free: vi.fn() },
    group: {
      owner,
      free: vi.fn(),
      isActive: () => true,
      groupId: () => new TextEncoder().encode(owner),
      devices: () => [],
    },
  };
}

const session = (account: string): unknown => ({
  kind: "session",
  account: { accountId: account, username: account },
});
const secretsOf = (account: string): unknown => ({
  accountId: new TextEncoder().encode(account),
  accountIdentitySeed: new Uint8Array(32),
  sessionToken: `token-${account}`,
});

let setState: (s: unknown) => void;
let setSecrets: (s: unknown) => void;

function mount(): DeviceGroupSyncApi {
  let api: DeviceGroupSyncApi | undefined;
  const Probe = (): null => {
    api = useDeviceGroupSync();
    return null;
  };
  renderUi(() => (
    <DeviceGroupSyncProvider>
      <Probe />
    </DeviceGroupSyncProvider>
  ));
  if (api === undefined) throw new Error("provider did not mount");
  return api;
}

function signIn(account: string): void {
  setSecrets(secretsOf(account));
  setState(session(account));
}
function signOut(): void {
  setSecrets(null);
  setState({ kind: "anon" });
}

beforeEach(() => {
  h.ensures.length = 0;
  h.loaded.length = 0;
  h.persisted.length = 0;
  h.polled.length = 0;
  const [state, setS] = createSignal<unknown>({ kind: "anon" });
  const [secrets, setSec] = createSignal<unknown>(null);
  h.state = state;
  h.secrets = secrets;
  setState = setS;
  setSecrets = setSec;
});

describe("DeviceGroupSyncProvider bootstrap", () => {
  it("comes up for the account that is signed in", async () => {
    const sync = mount();
    signIn("A");
    await vi.waitFor(() => expect(h.ensures).toHaveLength(1));
    h.ensures[0]?.resolve({ ...handles("A"), bootstrapped: false });
    await vi.waitFor(() => expect(sync.ready()).toBe(true));
    expect(Object.keys(sync.drafts())).toEqual(["draft-of-A"]);
    expect(h.polled).toEqual(["A"]);
  });

  it("installs nothing when the account signed out while its DeviceGroup was loading", async () => {
    const sync = mount();
    signIn("A");
    await vi.waitFor(() => expect(h.ensures).toHaveLength(1));
    signOut();
    const a = handles("A");
    h.ensures[0]?.resolve({ ...a, bootstrapped: false });
    await vi.waitFor(() => expect(a.group.free).toHaveBeenCalledTimes(1));
    expect(a.identity.free).toHaveBeenCalledTimes(1);
    // Give anything that would have continued a chance to.
    await new Promise((r) => setTimeout(r, 20));
    expect(sync.ready()).toBe(false);
    expect(sync.drafts()).toEqual({});
    expect(h.loaded).toEqual([]);
    expect(h.polled).toEqual([]);
  });

  it("gives account B only B's state when A's bootstrap finishes after B signed in", async () => {
    const sync = mount();
    signIn("A");
    await vi.waitFor(() => expect(h.ensures).toHaveLength(1));
    signOut();
    signIn("B");
    await vi.waitFor(() => expect(h.ensures).toHaveLength(2));
    expect(h.ensures.map((e) => e.account)).toEqual(["A", "B"]);

    // A's DeviceGroup arrives first, into B's session.
    const a = handles("A");
    h.ensures[0]?.resolve({ ...a, bootstrapped: false });
    await vi.waitFor(() => expect(a.group.free).toHaveBeenCalledTimes(1));
    expect(a.identity.free).toHaveBeenCalledTimes(1);
    await new Promise((r) => setTimeout(r, 20));
    // Nothing of A is up: not ready, no drafts, no poll, A's row never read.
    expect(sync.ready()).toBe(false);
    expect(sync.drafts()).toEqual({});
    expect(h.loaded).toEqual([]);
    expect(h.polled).toEqual([]);

    // B's own bootstrap completes normally.
    const b = handles("B");
    h.ensures[1]?.resolve({ ...b, bootstrapped: false });
    await vi.waitFor(() => expect(sync.ready()).toBe(true));
    expect(Object.keys(sync.drafts())).toEqual(["draft-of-B"]);
    expect(h.loaded).toEqual(["B"]);
    expect(h.polled).toEqual(["B"]);
    expect(b.group.free).not.toHaveBeenCalled();
  });
});
