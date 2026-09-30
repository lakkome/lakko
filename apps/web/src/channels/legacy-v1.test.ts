// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * Retired-v1 PublisherGroup error mapping.
 *
 * The point of these tests is **discrimination**, not just that the new copy
 * appears: a network / server / codec failure must keep its own text, or the
 * mapping has simply swallowed every channel error.
 */

import { describe, expect, it } from "vitest";
import {
  LEGACY_CHANNEL_MESSAGE,
  LEGACY_COPUBLISHER_LINK_MESSAGE,
  LegacyChannelError,
  channelErrorMessage,
  isLegacyPublisherGroupError,
  isLegacyV1PublisherGroupId,
  rethrowAsLegacyChannel,
} from "./legacy-v1";

/** The refusal `refuse_if_legacy_v1_present` raises, as it reaches JS through
 *  `JsError::new(&format!("{e}"))` over `Error::Group`'s Display impl. */
const RUST_REFUSAL =
  "group operation failed: legacy v1 PublisherGroup state present for this channel: " +
  "the v1 group id was 41 bytes, which the Delivery Service rejects, and an MLS group id " +
  "is bound into the GroupContext so it cannot be re-keyed to the 32-byte v2 id. " +
  "The v1 state is left intact; this channel needs a migration or a fresh channel.";

function v1GroupId(fill = 0xab): Uint8Array {
  const prefix = "lakko/publisher-group/v1/";
  const out = new Uint8Array(prefix.length + 16).fill(fill);
  for (let i = 0; i < prefix.length; ++i) out[i] = prefix.charCodeAt(i);
  return out;
}

function v2GroupId(fill = 0xab): Uint8Array {
  const prefix = "lakko/pubgrp/v2/";
  const out = new Uint8Array(32).fill(fill);
  for (let i = 0; i < prefix.length; ++i) out[i] = prefix.charCodeAt(i);
  return out;
}

describe("isLegacyV1PublisherGroupId", () => {
  it("recognises a 41-byte v1 id", () => {
    expect(isLegacyV1PublisherGroupId(v1GroupId())).toBe(true);
  });

  it("rejects a current 32-byte v2 PublisherGroup id", () => {
    expect(isLegacyV1PublisherGroupId(v2GroupId())).toBe(false);
  });

  it("rejects a 32-byte chat group id", () => {
    expect(isLegacyV1PublisherGroupId(new Uint8Array(32).fill(0x7f))).toBe(false);
  });

  it("rejects a 41-byte buffer that does not carry the v1 prefix", () => {
    expect(isLegacyV1PublisherGroupId(new Uint8Array(41).fill(0x00))).toBe(false);
  });

  it("rejects an empty id", () => {
    expect(isLegacyV1PublisherGroupId(new Uint8Array(0))).toBe(false);
  });
});

describe("isLegacyPublisherGroupError", () => {
  it("recognises the Rust refusal", () => {
    expect(isLegacyPublisherGroupError(new Error(RUST_REFUSAL))).toBe(true);
  });

  it("recognises it as a bare string too", () => {
    expect(isLegacyPublisherGroupError(RUST_REFUSAL)).toBe(true);
  });

  // Discrimination: the unrelated failures must NOT be captured.
  it.each([
    ["a network failure", new TypeError("Failed to fetch")],
    ["a server rejection", new Error("channels service returned 503")],
    ["a bad channel id", new Error("channel id hex is not valid: zzzz")],
    ["an unrelated MLS group error", new Error("group operation failed: no such group")],
    ["a chat-group /g/ link", new Error("co-publisher link does not target a channel PublisherGroup")],
  ])("does not fire on %s", (_label, err) => {
    expect(isLegacyPublisherGroupError(err)).toBe(false);
  });
});

describe("rethrowAsLegacyChannel", () => {
  it("converts the Rust refusal into the channel copy", () => {
    expect(() => rethrowAsLegacyChannel(new Error(RUST_REFUSAL))).toThrow(LegacyChannelError);
    try {
      rethrowAsLegacyChannel(new Error(RUST_REFUSAL));
    } catch (e) {
      expect(e).toBeInstanceOf(LegacyChannelError);
      expect((e as LegacyChannelError).kind).toBe("channel");
      expect((e as Error).message).toBe(LEGACY_CHANNEL_MESSAGE());
    }
  });

  it("re-throws an unrelated failure untouched", () => {
    const original = new TypeError("Failed to fetch");
    try {
      rethrowAsLegacyChannel(original);
      throw new Error("should have thrown");
    } catch (e) {
      expect(e).toBe(original);
      expect(e).not.toBeInstanceOf(LegacyChannelError);
    }
  });
});

describe("channelErrorMessage", () => {
  const fallback = (e: unknown): string => `Could not open the channel group: ${String(e)}`;

  it("returns the channel copy for the Rust refusal", () => {
    expect(channelErrorMessage(new Error(RUST_REFUSAL), fallback)).toBe(LEGACY_CHANNEL_MESSAGE());
  });

  it("returns the channel copy for an already-wrapped LegacyChannelError", () => {
    expect(channelErrorMessage(new LegacyChannelError("channel"), fallback)).toBe(
      LEGACY_CHANNEL_MESSAGE(),
    );
  });

  it("returns the invite-link copy for a link LegacyChannelError", () => {
    expect(channelErrorMessage(new LegacyChannelError("invite-link"), fallback)).toBe(
      LEGACY_COPUBLISHER_LINK_MESSAGE(),
    );
  });

  // Discrimination: a network failure still reads as a network failure.
  it("falls back for a network failure", () => {
    const msg = channelErrorMessage(new TypeError("Failed to fetch"), fallback);
    expect(msg).toContain("Failed to fetch");
    expect(msg).not.toBe(LEGACY_CHANNEL_MESSAGE());
  });
});

describe("copy", () => {
  it("says all three things the channel message must say, in order", () => {
    const older = LEGACY_CHANNEL_MESSAGE().indexOf("older version");
    const readable = LEGACY_CHANNEL_MESSAGE().indexOf("stay readable");
    const create = LEGACY_CHANNEL_MESSAGE().indexOf("create a new channel");
    expect(older).toBeGreaterThanOrEqual(0);
    expect(readable).toBeGreaterThan(older);
    expect(create).toBeGreaterThan(readable);
    expect(LEGACY_CHANNEL_MESSAGE()).toContain("cannot be repaired");
  });

  it("offers no repair or retry affordance", () => {
    for (const msg of [LEGACY_CHANNEL_MESSAGE(), LEGACY_COPUBLISHER_LINK_MESSAGE()]) {
      expect(msg.toLowerCase()).not.toMatch(/\b(migrat|retry|try again|repair this|recover)\b/);
    }
  });

  it("tells the joiner to ask for a new link", () => {
    expect(LEGACY_COPUBLISHER_LINK_MESSAGE()).toContain("older version");
    expect(LEGACY_COPUBLISHER_LINK_MESSAGE()).toContain("new invite link");
  });
});
