// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import { beforeAll, describe, expect, it } from "vitest";
import { readFileSync } from "node:fs";
import { fileURLToPath } from "node:url";

import init, { attachmentMenu, ConversationKind } from "../../wasm/lakko_ffi_wasm";
import { menuEntriesOf, menuRole, nextEnabled } from "./attach-menu";

beforeAll(async () => {
  const wasmPath = fileURLToPath(new URL("../../wasm/lakko_ffi_wasm_bg.wasm", import.meta.url));
  await init({ module_or_path: readFileSync(wasmPath) });
});

function rows(kind: ConversationKind, role: string, camera = true, location = true, inCall = false) {
  return menuEntriesOf(
    attachmentMenu(kind, role, camera, location, inCall).map((r) => {
      const v = { item: r.item, section: r.section, disabled: r.disabled };
      r.free();
      return v;
    }),
  );
}

describe("the paperclip menu (docs/ux/attachments.md section 2)", () => {
  it("offers the nine rows in order in a group and to a channel publisher, and no Poll in a one-to-one", () => {
    const order = ["photo-or-video", "camera", "file", "audio", "poll", "checklist", "location", "contact", "article"];
    expect(rows(ConversationKind.Chat, "member").map((e) => e.item)).toEqual(order.filter((i) => i !== "poll"));
    expect(rows(ConversationKind.Group, "member").map((e) => e.item)).toEqual(order);
    expect(rows(ConversationKind.Channel, "admin").map((e) => e.item)).toEqual(order);
  });

  it("gives a channel subscriber no paperclip", () => {
    expect(rows(ConversationKind.Channel, "member")).toEqual([]);
  });

  it("shows Camera and Location disabled with a reason, never absent", () => {
    const r = rows(ConversationKind.Group, "member", false, false);
    expect(r.find((e) => e.item === "camera")?.disabled).toBe("no-camera");
    expect(r.find((e) => e.item === "location")?.disabled).toBe("no-location");
    const inCall = rows(ConversationKind.Group, "member", true, true, true);
    expect(inCall.find((e) => e.item === "camera")?.disabled).toBe("camera-in-call");
  });

  it("moves over disabled rows with the arrow keys and wraps", () => {
    const r = rows(ConversationKind.Group, "member", false, true);
    // Row 1 (Camera) is disabled: down from 0 lands on 2.
    expect(nextEnabled(r, 0, 1)).toBe(2);
    expect(nextEnabled(r, 2, -1)).toBe(0);
    expect(nextEnabled(r, r.length - 1, 1)).toBe(0);
    expect(nextEnabled([], 0, 1)).toBe(-1);
  });

  it("drops an item this client does not know and an unknown reason", () => {
    expect(menuEntriesOf([{ item: "wallet", section: 0, disabled: undefined }])).toEqual([]);
    expect(menuEntriesOf([{ item: "file", section: 0, disabled: "mystery" }])[0]?.disabled).toBeUndefined();
  });

  it("maps the person's role", () => {
    expect(menuRole({ owner: true, admin: false })).toBe("owner");
    expect(menuRole({ owner: false, admin: true })).toBe("admin");
    expect(menuRole({ owner: false, admin: false })).toBe("member");
  });
});
