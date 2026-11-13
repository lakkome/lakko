// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * A channel's row in the merged list carries its newest post's time where a
 * group's row carries its last message's time (docs/ux/group-and-channel-
 * privacy.md section 12: the row is "the same" as a group's). Seen live
 * 2026-11-12: the owner's row read only "Walk 22 channel" and "GIF", with no
 * time, while a group row beside it read "20:53".
 */

// @vitest-environment jsdom

import { beforeAll, describe, expect, it } from "vitest";
import { MemoryRouter, Route } from "@solidjs/router";

import { all, initWasm, renderUi, settle } from "../test/ui";
import type { ConversationRow } from "./conversation-list";

let ChannelRow: typeof import("./ChannelRow")["ChannelRow"];

beforeAll(async () => {
  await initWasm();
  ({ ChannelRow } = await import("./ChannelRow"));
  await settle();
});

const row: ConversationRow = {
  id: "ab".repeat(16),
  kind: "channel",
  title: "Walk 22 channel",
  preview: "GIF",
  lastActivityMs: 1_000,
  postedAtMs: 1_000,
  unread: 0,
  pinned: false,
  muted: false,
  archived: false,
};

function mount(time: string): void {
  renderUi(() => (
    <MemoryRouter>
      <Route
        path="/"
        component={() => <ChannelRow row={row} time={time} pinned={false} selected={false} onMenu={() => {}} />}
      />
    </MemoryRouter>
  ));
}

describe("a channel row", () => {
  it("shows the newest post's time beside the title, as a group row does", () => {
    mount("22:39");
    expect(all(".lk-row .lk-row-time").map((e) => e.textContent)).toEqual(["22:39"]);
    expect(all(".lk-row .lk-row-sub").map((e) => e.textContent)).toEqual(["GIF"]);
  });

  it("draws no time slot while nothing has been posted", () => {
    mount("");
    expect(all(".lk-row .lk-row-time")).toEqual([]);
  });
});
