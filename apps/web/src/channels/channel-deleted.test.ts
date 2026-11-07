// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import { afterEach, describe, expect, it, vi } from "vitest";

import {
  channelIdOfUrl,
  installChannelDeletedHandler,
  isChannelDeletedError,
  markChannelDeletedHere,
  noteIfChannelDeleted,
} from "./api";

const ID = "ab".repeat(16);

afterEach(() => installChannelDeletedHandler(null));

async function settle(): Promise<void> {
  await new Promise((r) => setTimeout(r, 0));
}

describe("a deleted channel (ADR-0087 phase B)", () => {
  it("reads the channel id off any channels route", () => {
    expect(channelIdOfUrl(`https://lakko.me/api/channels/v1/channels/${ID}/posts?after=3`)).toBe(ID);
    expect(channelIdOfUrl(`/api/channels/v1/channels/${ID.toUpperCase()}`)).toBe(ID);
    expect(channelIdOfUrl("/api/channels/v1/channels/abc/posts")).toBeNull();
    expect(channelIdOfUrl("")).toBeNull();
  });

  it("is a 410 with the channel-deleted code only", () => {
    expect(isChannelDeletedError(410, "channel-deleted")).toBe(true);
    expect(isChannelDeletedError(410, "subscriber-removed")).toBe(false);
    expect(isChannelDeletedError(404, "channel-deleted")).toBe(false);
  });

  it("ends the channel once for a burst of 410s", async () => {
    const id = "cd".repeat(16);
    const handler = vi.fn();
    installChannelDeletedHandler(handler);
    const url = `/api/channels/v1/channels/${id}/posts`;
    noteIfChannelDeleted(410, "channel-deleted", url);
    noteIfChannelDeleted(410, "channel-deleted", url);
    await settle();
    expect(handler).toHaveBeenCalledTimes(1);
    expect(handler).toHaveBeenCalledWith(id);
  });

  it("does nothing for another error", async () => {
    const handler = vi.fn();
    installChannelDeletedHandler(handler);
    noteIfChannelDeleted(410, "subscriber-removed", `/api/channels/v1/channels/${"ef".repeat(16)}/x`);
    await settle();
    expect(handler).not.toHaveBeenCalled();
  });

  it("does not end again a channel the owner deleted on this device", async () => {
    const id = "12".repeat(16);
    const handler = vi.fn();
    installChannelDeletedHandler(handler);
    markChannelDeletedHere(id);
    noteIfChannelDeleted(410, "channel-deleted", `/api/channels/v1/channels/${id}`);
    await settle();
    expect(handler).not.toHaveBeenCalled();
  });

  it("tries again after a handler that failed", async () => {
    const id = "34".repeat(16);
    const handler = vi.fn().mockRejectedValueOnce(new Error("x")).mockResolvedValue(undefined);
    installChannelDeletedHandler(handler);
    const url = `/api/channels/v1/channels/${id}`;
    noteIfChannelDeleted(410, "channel-deleted", url);
    await settle();
    await settle();
    noteIfChannelDeleted(410, "channel-deleted", url);
    await settle();
    expect(handler).toHaveBeenCalledTimes(2);
  });
});
