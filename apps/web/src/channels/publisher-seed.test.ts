// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import { describe, expect, it } from "vitest";

import { channelSeedFor, notePublisherCommitLanded, notePublisherSeedAt } from "./channel-access";
import { composerSeedFor, outgoingPublisherSeed, readsPublicSeeds } from "./publisher-seed";
import { channelsActions, channelsState } from "../state/channels-state";

const OLD = new Uint8Array(32).fill(1);
const NEW = new Uint8Array(32).fill(2);
const SIGNING = new Uint8Array(32).fill(9);

describe("outgoingPublisherSeed", () => {
  it("keeps the replaced seed under the epoch it served", () => {
    const kept = outgoingPublisherSeed(OLD, 3, NEW, () => false);
    expect(kept?.epoch).toBe(3);
    expect(kept?.seed).toEqual(OLD);
    // A copy, not the caller's buffer.
    expect(kept?.seed).not.toBe(OLD);
  });

  it("keeps nothing when the seed did not change", () => {
    expect(outgoingPublisherSeed(NEW, 3, NEW.slice(), () => false)).toBeNull();
  });

  it("keeps nothing without a previous seed or a known epoch", () => {
    expect(outgoingPublisherSeed(undefined, 3, NEW, () => false)).toBeNull();
    expect(outgoingPublisherSeed(OLD, undefined, NEW, () => false)).toBeNull();
    expect(outgoingPublisherSeed(OLD, -1, NEW, () => false)).toBeNull();
  });

  it("never replaces a seed already held for that epoch", () => {
    expect(outgoingPublisherSeed(OLD, 3, NEW, (e) => e === 3)).toBeNull();
  });
});

describe("readsPublicSeeds", () => {
  it("a publisher reads the public seeds of a public channel, or of one whose type it does not know", () => {
    expect(readsPublicSeeds(true, true, "public")).toBe(true);
    expect(readsPublicSeeds(true, true, null)).toBe(true);
  });

  it("a publisher does not ask for the public seeds of a private channel", () => {
    expect(readsPublicSeeds(true, true, "private")).toBe(false);
  });

  it("nobody reads them without the read key; a reader with it always does", () => {
    expect(readsPublicSeeds(true, false, "public")).toBe(false);
    expect(readsPublicSeeds(false, false, "public")).toBe(false);
    expect(readsPublicSeeds(false, true, "private")).toBe(true);
  });
});

describe("notePublisherCommitLanded", () => {
  it("after an approve the owner still opens the record at creation, and posts under the new seed", () => {
    // Seen live: the owner's own new channel, after it approved a publisher.
    const id = "ab".repeat(16);
    channelsActions.setKeys(id, { seed: OLD.slice(), signingKeySeed: SIGNING });
    channelsActions.setEpoch(id, 0);
    expect(channelSeedFor(id, 0)).toEqual(OLD);

    notePublisherCommitLanded(id, 1, NEW.slice());

    expect(channelSeedFor(id, 0)).toEqual(OLD);
    expect(channelSeedFor(id, 1)).toEqual(NEW);
  });
});

describe("composerSeedFor", () => {
  const held = (m: Record<number, Uint8Array>) => (e: number): Uint8Array | undefined => m[e];

  it("seals with the composer's seed under its epoch once the channel reached it", () => {
    expect(composerSeedFor({ seed: NEW, seedEpoch: 2 }, 2, held({}))).toEqual({ epoch: 2, seed: NEW });
  });

  it("keeps sealing with the earlier seed under the committed epoch while the owner's reseal is pending", () => {
    expect(composerSeedFor({ seed: NEW, seedEpoch: 2 }, 1, held({ 1: OLD }))).toEqual({ epoch: 1, seed: OLD });
  });

  it("never seals under an epoch the channel has not reached", () => {
    // No earlier seed on this device (a reload in the window): no post.
    expect(composerSeedFor({ seed: NEW, seedEpoch: 2 }, 1, held({}))).toBeNull();
  });

  it("a channel ahead of the group (commits not applied yet) still gets the seed's own epoch", () => {
    expect(composerSeedFor({ seed: NEW, seedEpoch: 2 }, 3, held({}))).toEqual({ epoch: 2, seed: NEW });
  });

  it("keys with no seed epoch keep the channel's current epoch", () => {
    expect(composerSeedFor({ seed: NEW }, 4, held({}))).toEqual({ epoch: 4, seed: NEW });
    expect(composerSeedFor({ seed: NEW }, undefined, held({}))).toEqual({ epoch: 0, seed: NEW });
  });
});

describe("notePublisherSeedAt (a co-publisher applying the owner's commit)", () => {
  it("takes the group's seed, keeps the old one, and seals under the committed epoch until the reseal lands", () => {
    const id = "cd".repeat(16);
    channelsActions.setKeys(id, { seed: OLD.slice(), signingKeySeed: SIGNING, seedEpoch: 1 });
    channelsActions.setEpoch(id, 1);

    // The owner's rotation applied from the journal; its reseal not yet in.
    notePublisherSeedAt(id, 2, NEW.slice());

    expect(channelSeedFor(id, 1)).toEqual(OLD);
    expect(channelSeedFor(id, 2)).toEqual(NEW);
    const keys = channelsState.keysByChannel().get(id);
    if (keys === undefined) throw new Error("keys gone");
    expect(keys.seedEpoch).toBe(2);
    expect(keys.signingKeySeed).toEqual(SIGNING);
    const held = (e: number): Uint8Array | undefined => channelsActions.currentSeed(id, e);
    expect(composerSeedFor(keys, 1, held)).toEqual({ epoch: 1, seed: OLD });

    // The reseal moved the channel: posts go out under the new seed.
    channelsActions.setEpoch(id, 2);
    expect(composerSeedFor(keys, 2, held)).toEqual({ epoch: 2, seed: NEW });
  });

  it("does nothing when the group did not move", () => {
    const id = "ef".repeat(16);
    channelsActions.setKeys(id, { seed: OLD.slice(), signingKeySeed: SIGNING, seedEpoch: 3 });
    const before = channelsState.keysByChannel().get(id);
    notePublisherSeedAt(id, 3, OLD.slice());
    expect(channelsState.keysByChannel().get(id)).toBe(before);
  });
});
