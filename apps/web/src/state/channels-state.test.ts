// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * `channels-state` module evaluation + the hydration contract.
 *
 * Runs in plain `node`: there is no IndexedDB, so the channel-log
 * load / save inside `hydrateChannelsState` reject and are swallowed
 * by design (the state stays good, the next mutation retries). That
 * makes the ORDER and the in-memory effects testable here without a
 * fake IDB; the codec itself is covered in
 * `channels/channel-log-codec.test.ts`.
 */

import { type DeviceCipher } from "../auth/device-cipher";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

// A `localStorage` stub installed BEFORE the module is imported (dynamic
// import so the stub wins): the temporal-dead-zone regression this file
// originally pinned only reproduced when `localStorage` existed at module
// evaluation, which the `node` environment does not provide.
function stubLocalStorage(seed: Record<string, string>): Map<string, string> {
  const map = new Map(Object.entries(seed));
  vi.stubGlobal("localStorage", {
    getItem: (k: string) => map.get(k) ?? null,
    setItem: (k: string, v: string) => void map.set(k, v),
    removeItem: (k: string) => void map.delete(k),
    clear: () => map.clear(),
    key: (i: number) => [...map.keys()][i] ?? null,
    get length() {
      return map.size;
    },
  });
  return map;
}

const LEGACY_KEY = "lakko.channels.subscriber-keys.v1";
const CH = "0123456789abcdef0123456789abcdef";

/** The slice of a wasm `Identity` the hydration touches. */
function fakeIdentity(): {
  signaturePublicKey: Uint8Array;
  restoreStorage: ReturnType<typeof vi.fn>;
  snapshotStorage: ReturnType<typeof vi.fn>;
} {
  return {
    signaturePublicKey: new Uint8Array(32).fill(7),
    restoreStorage: vi.fn(),
    snapshotStorage: vi.fn(() => new Uint8Array([1, 2, 3])),
  };
}

const DEVICE_KEY = {} as unknown as DeviceCipher;

describe("channels-state module evaluation", () => {
  afterEach(() => {
    vi.unstubAllGlobals();
    vi.resetModules();
  });

  it("imports without a temporal-dead-zone crash when localStorage exists", async () => {
    stubLocalStorage({});
    await expect(import("./channels-state")).resolves.toBeDefined();
  });

  it("no longer reads the plaintext legacy subscriber-key row at evaluation", async () => {
    stubLocalStorage({
      [LEGACY_KEY]: JSON.stringify([
        { channelId: CH, secretHex: "11".repeat(32), publicHex: "22".repeat(32) },
      ]),
    });
    const { channelsState } = await import("./channels-state");
    expect(channelsState.subscriberKeysByChannel().size).toBe(0);
    expect(channelsState.hydrated()).toBe(false);
  });
});

describe("channels-state hydration", () => {
  beforeEach(() => {
    vi.spyOn(console, "warn").mockImplementation(() => undefined);
  });
  afterEach(() => {
    vi.restoreAllMocks();
    vi.unstubAllGlobals();
    vi.resetModules();
  });

  it("wipes the legacy plaintext row, settles, and releases waiters", async () => {
    const storage = stubLocalStorage({ [LEGACY_KEY]: "[]" });
    const mod = await import("./channels-state");
    const identity = fakeIdentity();

    const waited = mod.awaitChannelsHydration();
    await mod.hydrateChannelsState({
      accountIdB64: "acct",
      deviceKey: DEVICE_KEY,
      identity: identity as never,
    });
    await waited;

    expect(storage.has(LEGACY_KEY)).toBe(false);
    expect(mod.channelsState.hydrated()).toBe(true);
    // Nothing on disk in node: no snapshot to restore, so the provider is
    // never replaced under the caller.
    expect(identity.restoreStorage).not.toHaveBeenCalled();
    // After settling, a waiter resolves at once.
    await expect(mod.awaitChannelsHydration()).resolves.toBeUndefined();
  });

  it("keeps a mutation that landed before the hydration settled", async () => {
    stubLocalStorage({});
    const mod = await import("./channels-state");
    mod.channelsActions.addChannel({
      id: CH,
      localName: "early",
      localDescription: "",
      addedAtMs: 5,
    });
    mod.channelsActions.setSubscriberKeys(CH, {
      secret: new Uint8Array(32).fill(1),
      public: new Uint8Array(32).fill(2),
    });
    await mod.hydrateChannelsState({
      accountIdB64: "acct",
      deviceKey: DEVICE_KEY,
      identity: fakeIdentity() as never,
    });
    expect(mod.channelsState.channels().map((c) => c.id)).toEqual([CH]);
    expect(mod.channelsState.subscriberKeysByChannel().get(CH)?.public).toEqual(
      new Uint8Array(32).fill(2),
    );
  });

  it("a hydration whose session ended arms nothing but still releases waiters", async () => {
    stubLocalStorage({});
    const mod = await import("./channels-state");
    const identity = fakeIdentity();
    const waited = mod.awaitChannelsHydration();
    await mod.hydrateChannelsState({
      accountIdB64: "acct",
      deviceKey: DEVICE_KEY,
      identity: identity as never,
      isCurrent: () => false,
    });
    await waited;
    expect(mod.channelsState.hydrated()).toBe(false);
    // No sink was armed, so no flush reads the provider.
    mod.channelsActions.notePublisherGroupChanged();
    await new Promise((r) => setTimeout(r, 200));
    expect(identity.snapshotStorage).not.toHaveBeenCalled();
  });

  it("clearChannelsSession empties the in-memory state and un-settles", async () => {
    stubLocalStorage({});
    const mod = await import("./channels-state");
    await mod.hydrateChannelsState({
      accountIdB64: "acct",
      deviceKey: DEVICE_KEY,
      identity: fakeIdentity() as never,
    });
    mod.channelsActions.addChannel({ id: CH, localName: "x", localDescription: "", addedAtMs: 1 });
    mod.channelsActions.setSubscribedSeedForEpoch(CH, 3, new Uint8Array(32).fill(9));
    expect(mod.channelsState.hydrated()).toBe(true);

    await mod.clearChannelsSession();

    expect(mod.channelsState.channels()).toEqual([]);
    expect(mod.channelsState.subscribedSeedsByChannel().size).toBe(0);
    expect(mod.channelsState.keysByChannel().size).toBe(0);
    expect(mod.channelsState.hydrated()).toBe(false);
  });

  it("markChannelsHydrationUnavailable settles without a device key", async () => {
    stubLocalStorage({});
    const mod = await import("./channels-state");
    const waited = mod.awaitChannelsHydration();
    mod.markChannelsHydrationUnavailable();
    await waited;
    expect(mod.channelsState.hydrated()).toBe(true);
  });
});

describe("channels-state publish counters are account data", () => {
  const ACCOUNT_A = "AAECAwQFBgcICQoLDA0ODw==";
  const ACCOUNT_B = "+/+/+/+/+/+/+/+/+/+/+w==";
  const rowOf = (account: string): string =>
    `lakko.channels.publish-counter.v2.${account}.${CH}`;
  const KEYS = { seed: new Uint8Array(32).fill(3), signingKeySeed: new Uint8Array(32).fill(4) };

  beforeEach(() => {
    vi.spyOn(console, "warn").mockImplementation(() => undefined);
  });
  afterEach(() => {
    vi.restoreAllMocks();
    vi.unstubAllGlobals();
    vi.resetModules();
  });

  /** A fresh module graph, with `account` signed in and hydrated. */
  async function signIn(
    mod: typeof import("./channels-state"),
    contacts: typeof import("../contacts/local-store"),
    account: string,
  ): Promise<void> {
    contacts.bindContactsAccount(account);
    await mod.hydrateChannelsState({
      accountIdB64: account,
      deviceKey: DEVICE_KEY,
      identity: fakeIdentity() as never,
    });
  }

  async function signOut(
    mod: typeof import("./channels-state"),
    contacts: typeof import("../contacts/local-store"),
  ): Promise<void> {
    await mod.clearChannelsSession();
    contacts.bindContactsAccount(null);
  }

  it("two accounts keep separate counters, and B never starts from A's floor", async () => {
    const storage = stubLocalStorage({ [rowOf(ACCOUNT_A)]: "5", [rowOf(ACCOUNT_B)]: "2" });
    const mod = await import("./channels-state");
    const contacts = await import("../contacts/local-store");

    await signIn(mod, contacts, ACCOUNT_A);
    expect(mod.channelsState.counterByChannel().get(CH)).toBe(5n);
    mod.channelsActions.setKeys(CH, KEYS);
    expect(mod.channelsActions.nextCounter(CH)).toBe(5n);

    await signOut(mod, contacts);
    expect(mod.channelsState.counterByChannel().size).toBe(0);

    await signIn(mod, contacts, ACCOUNT_B);
    expect(mod.channelsState.counterByChannel().get(CH)).toBe(2n);
    mod.channelsActions.setKeys(CH, KEYS);
    expect(mod.channelsActions.nextCounter(CH)).toBe(2n);

    expect(storage.get(rowOf(ACCOUNT_A))).toBe("6");
    expect(storage.get(rowOf(ACCOUNT_B))).toBe("3");
  });

  it("an account with no counters starts at 0 after another account posted", async () => {
    stubLocalStorage({});
    const mod = await import("./channels-state");
    const contacts = await import("../contacts/local-store");

    await signIn(mod, contacts, ACCOUNT_A);
    mod.channelsActions.setKeys(CH, KEYS);
    for (let i = 0; i < 4; i += 1) mod.channelsActions.nextCounter(CH);
    await signOut(mod, contacts);

    await signIn(mod, contacts, ACCOUNT_B);
    expect(mod.channelsState.counterByChannel().size).toBe(0);
    mod.channelsActions.setKeys(CH, KEYS);
    expect(mod.channelsActions.nextCounter(CH)).toBe(0n);
  });

  it("Log out keeps the account's counter for its next sign-in", async () => {
    const storage = stubLocalStorage({});
    const mod = await import("./channels-state");
    const contacts = await import("../contacts/local-store");

    await signIn(mod, contacts, ACCOUNT_A);
    mod.channelsActions.setKeys(CH, KEYS);
    for (let i = 0; i < 3; i += 1) mod.channelsActions.nextCounter(CH);
    await signOut(mod, contacts);
    expect(storage.get(rowOf(ACCOUNT_A))).toBe("3");

    await signIn(mod, contacts, ACCOUNT_A);
    mod.channelsActions.setKeys(CH, KEYS);
    expect(mod.channelsActions.nextCounter(CH)).toBe(3n);
  });

  it("un-follow keeps the persisted counter, and a re-follow resumes it", async () => {
    const storage = stubLocalStorage({});
    const mod = await import("./channels-state");
    const contacts = await import("../contacts/local-store");

    await signIn(mod, contacts, ACCOUNT_A);
    mod.channelsActions.setKeys(CH, KEYS);
    mod.channelsActions.nextCounter(CH);
    mod.channelsActions.nextCounter(CH);
    mod.channelsActions.removeChannel(CH);
    expect(mod.channelsState.counterByChannel().has(CH)).toBe(false);
    expect(storage.get(rowOf(ACCOUNT_A))).toBe("2");

    mod.channelsActions.setKeys(CH, KEYS);
    expect(mod.channelsActions.nextCounter(CH)).toBe(2n);
  });

  it("a hydration whose session ended loads no counters into the next session", async () => {
    stubLocalStorage({ [rowOf(ACCOUNT_A)]: "5" });
    const mod = await import("./channels-state");
    const contacts = await import("../contacts/local-store");
    contacts.bindContactsAccount(ACCOUNT_B);
    await mod.hydrateChannelsState({
      accountIdB64: ACCOUNT_A,
      deviceKey: DEVICE_KEY,
      identity: fakeIdentity() as never,
      isCurrent: () => false,
    });
    expect(mod.channelsState.counterByChannel().size).toBe(0);
    expect(mod.channelsState.channels()).toEqual([]);
  });

  it("the wipe removes only the wiped account's counters, after the channel log", async () => {
    const storage = stubLocalStorage({
      [rowOf(ACCOUNT_A)]: "5",
      [rowOf(ACCOUNT_B)]: "2",
      [`lakko.channels.publish-counter.v1.${CH}`]: "7",
    });
    const { FakeIndexedDb } = await import("../test/fake-storage");
    vi.stubGlobal("indexedDB", new FakeIndexedDb());
    const mod = await import("./channels-state");

    await mod.wipeChannelsForAccount(ACCOUNT_A);

    expect(storage.has(rowOf(ACCOUNT_A))).toBe(false);
    expect(storage.has(`lakko.channels.publish-counter.v1.${CH}`)).toBe(false);
    expect(storage.get(rowOf(ACCOUNT_B))).toBe("2");
  });

  it("the wipe keeps the counters when the channel log delete fails", async () => {
    // No IndexedDB in node: the delete of the row holding the signing seeds
    // rejects, so the counters that guard those seeds must stay.
    const storage = stubLocalStorage({ [rowOf(ACCOUNT_A)]: "5" });
    const mod = await import("./channels-state");

    await expect(mod.wipeChannelsForAccount(ACCOUNT_A)).rejects.toBeDefined();
    expect(storage.get(rowOf(ACCOUNT_A))).toBe("5");
  });
});
