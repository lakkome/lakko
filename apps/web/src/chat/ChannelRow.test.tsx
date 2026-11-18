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

import { all, initWasm, one, renderUi, settle } from "../test/ui";
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

type Field = "pinned" | "muted" | "archived";

function mount(
  time: string,
  flags: { pinned?: boolean; muted?: boolean; archived?: boolean } = {},
  onToggle: (field: Field) => void = () => {},
): void {
  renderUi(() => (
    <MemoryRouter>
      <Route
        path="/"
        component={() => (
          <ChannelRow
            row={{ ...row, unread: 3 }}
            time={time}
            pinned={flags.pinned === true}
            muted={flags.muted === true}
            archived={flags.archived === true}
            pinBlocked={false}
            selected={false}
            onMenu={() => {}}
            onToggle={onToggle}
          />
        )}
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

  // Base scenario 29 (2026-11-13): a channel row offered only "Add to
  // folder". Section 12 - the row behaves as a group's: the same pin, mute
  // and archive glyphs and actions.
  it("draws the mute, pin and archive glyphs a group row draws", () => {
    mount("22:39", { pinned: true, muted: true, archived: true });
    expect(all(".lk-row .lk-row-trail > svg")).toHaveLength(3);
    expect(one(".lk-row").classList.contains("archived")).toBe(true);
    expect(one(".lk-row .lk-badge").classList.contains("muted")).toBe(true);
    const labels = all(".lk-row .lk-row-actions button").map((b) => b.getAttribute("aria-label"));
    expect(labels).toHaveLength(3);
  });

  it("toggles pin, mute and archive from its own actions", () => {
    const seen: Field[] = [];
    mount("22:39", {}, (f) => seen.push(f));
    expect(all(".lk-row .lk-row-trail > svg")).toHaveLength(0);
    for (const b of all<HTMLButtonElement>(".lk-row .lk-row-actions button")) b.click();
    expect(seen).toEqual(["pinned", "muted", "archived"]);
  });
});
