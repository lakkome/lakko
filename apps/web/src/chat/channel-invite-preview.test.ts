// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import { describe, expect, it, vi } from "vitest";

const results = new Map<string, unknown>();
vi.mock("../channels/channel-invites", () => ({
  openChannelLink: async (url: string) => {
    const r = results.get(url);
    if (r instanceof Error) throw r;
    return r ?? null;
  },
}));

import { channelInviteRowPreview } from "./channel-invite-preview";
import { t } from "../l10n";

const ORIGIN = "https://test.lakko.me";
const settle = (): Promise<void> => new Promise((r) => setTimeout(r, 0));

describe("channelInviteRowPreview", () => {
  it("reads a publisher invitation as the invitation, never the link", async () => {
    const url = `${ORIGIN}/g/AbCdEfGh#0123456789abcdef`;
    results.set(url, { channelIdHex: "00", name: "Pixel Pub Walk", publisherInvite: true });
    // While the link is opened the row never carries its secret (a desktop
    // notification is taken from the row at that moment).
    expect(channelInviteRowPreview(url, ORIGIN)).toBe(`${ORIGIN}/g/AbCdEfGh`);
    await settle();
    expect(channelInviteRowPreview(url, ORIGIN)).toBe(
      t("chats.preview.publisher_invite", { channel: "Pixel Pub Walk" }),
    );
  });

  it("reads a subscriber invitation as an invitation to the channel", async () => {
    const url = `${ORIGIN}/g/ZZZZZZZZ#0123456789abcdef`;
    results.set(url, { channelIdHex: "01", name: "News", publisherInvite: false });
    channelInviteRowPreview(url, ORIGIN);
    await settle();
    expect(channelInviteRowPreview(url, ORIGIN)).toBe(t("chats.preview.channel_invite", { channel: "News" }));
  });

  it("leaves other text, other origins and group links alone", async () => {
    expect(channelInviteRowPreview("hello", ORIGIN)).toBeNull();
    expect(channelInviteRowPreview(null, ORIGIN)).toBeNull();
    expect(channelInviteRowPreview("https://evil.example/g/AbCdEfGh#0123456789abcdef", ORIGIN)).toBeNull();
    const group = `${ORIGIN}/g/GrOuPlNk#0123456789abcdef`;
    channelInviteRowPreview(group, ORIGIN);
    await settle();
    expect(channelInviteRowPreview(group, ORIGIN)).toBeNull();
  });
});
