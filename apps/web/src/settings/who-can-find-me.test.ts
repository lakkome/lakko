// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * `docs/ux/who-can-find-me.md` - the presentation of the "who can find me"
 * screen.
 *
 * The transitions themselves are `lakko_ui_rules` and are asserted in Rust;
 * restating them here would be a second copy. These tests cover what only
 * this client can get wrong: which rows are on screen for a setting, the
 * copy, the list, and - through the REAL wasm - that this module reads the
 * setting the rules produce the way the rules mean it (the numeric enum
 * values, the chosen list).
 */
import { beforeAll, describe, expect, it } from "vitest";
import { readFileSync } from "node:fs";
import { fileURLToPath } from "node:url";

import init, * as wasm from "../../wasm/lakko_ffi_wasm";
import {
  channelIdBytes,
  chosenCount,
  chosenKeysOf,
  contactsOfContactsHelper,
  COPY,
  FORM_ALL,
  FORM_CHOSEN,
  foldForSearch,
  groupsHelper,
  MODE_EVERYONE,
  MODE_NOBODY,
  MODE_SOME,
  opensListOnChoose,
  SCOPE_CHANNEL,
  SCOPE_GROUP,
  scopeKey,
  scopeList,
  showsListSearch,
  showsMembersSwitch,
  statusLine,
  viewOf,
  visibleRows,
  type ScopeEntry,
} from "./who-can-find-me";

function entry(kind: "group" | "channel", byte: number, name: string): ScopeEntry {
  const id = new Uint8Array([byte, byte]);
  return { kind, id, key: scopeKey(kind, id), name };
}

describe("rows on screen", () => {
  it("shows the parts only under Some people", () => {
    expect(visibleRows({ mode: "nobody", contactsOfContacts: false, groups: null }).parts).toBe(false);
    expect(visibleRows({ mode: "everyone", contactsOfContacts: false, groups: null }).parts).toBe(false);
    expect(visibleRows({ mode: "some", contactsOfContacts: true, groups: null })).toEqual({
      parts: true,
      groupsForm: false,
      listDisclosure: false,
    });
  });

  it("shows the form only while the groups part is on, and the disclosure only under chosen", () => {
    expect(visibleRows({ mode: "some", contactsOfContacts: false, groups: "all" })).toEqual({
      parts: true,
      groupsForm: true,
      listDisclosure: false,
    });
    expect(visibleRows({ mode: "some", contactsOfContacts: false, groups: "chosen" })).toEqual({
      parts: true,
      groupsForm: true,
      listDisclosure: true,
    });
  });

  it("offers the Members can find me switch only while groups are part of the setting", () => {
    expect(showsMembersSwitch({ mode: "some", contactsOfContacts: true, groups: "all" })).toBe(true);
    expect(showsMembersSwitch({ mode: "some", contactsOfContacts: false, groups: "chosen" })).toBe(true);
    expect(showsMembersSwitch({ mode: "some", contactsOfContacts: true, groups: null })).toBe(false);
    expect(showsMembersSwitch({ mode: "nobody", contactsOfContacts: false, groups: null })).toBe(false);
    // Everyone includes every group, but there is nothing to switch.
    expect(showsMembersSwitch({ mode: "everyone", contactsOfContacts: false, groups: null })).toBe(false);
  });
});

describe("copy", () => {
  it("counts N of M under chosen and describes all otherwise", () => {
    expect(groupsHelper({ mode: "some", contactsOfContacts: false, groups: "chosen" }, 2, 5)).toBe(
      "Only the ones you choose (2 of 5).",
    );
    expect(groupsHelper({ mode: "some", contactsOfContacts: false, groups: "all" }, 2, 5)).toBe(
      "Everyone in the groups and channels you're in.",
    );
  });

  it("says contacts of contacts is spreading only while it is on and was just turned on", () => {
    expect(contactsOfContactsHelper(true, true)).toBe(
      "Contacts of your contacts can find you once your contacts next open Lakko.",
    );
    expect(contactsOfContactsHelper(true, false)).toBe("People your contacts have added.");
    expect(contactsOfContactsHelper(false, true)).toBe("People your contacts have added.");
  });

  it("maps every apply state to what the screen says", () => {
    expect(statusLine("applying")).toEqual({ spinner: true, message: null, retry: false });
    expect(statusLine("offline")).toEqual({
      spinner: false,
      message: "Will apply when you're back online.",
      retry: false,
    });
    expect(statusLine("failed")).toEqual({
      spinner: false,
      message: "Couldn't update who can find you. Try again.",
      retry: true,
    });
    expect(statusLine("applied")).toEqual({ spinner: false, message: null, retry: false });
    expect(statusLine("idle")).toEqual({ spinner: false, message: null, retry: false });
  });

  it("keeps the spec's helper texts verbatim", () => {
    expect(COPY.nobodyHelper).toBe("Only people with your invite link or QR code can reach you.");
    expect(COPY.everyoneHelper).toBe("Anyone can find you by your username or name.");
    expect(COPY.listEmpty).toBe("You're not in any groups or channels yet.");
    expect(COPY.excludedNotice).toBe(
      "Now only the groups and channels you choose. Change in Privacy.",
    );
  });
});

describe("the list", () => {
  const entries = [
    entry("channel", 9, "Zebra news"),
    entry("group", 1, "Hiking club"),
    entry("group", 2, "Álbum de família"),
    entry("channel", 8, "Anna's notes"),
  ];

  it("puts groups before channels, each by name", () => {
    const list = scopeList(entries, "");
    expect(list.groups.map((e) => e.name)).toEqual(["Álbum de família", "Hiking club"]);
    expect(list.channels.map((e) => e.name)).toEqual(["Anna's notes", "Zebra news"]);
  });

  it("filters by any part of the name, ignoring case, accents and repeated spaces", () => {
    expect(scopeList(entries, "ALBUM").groups.map((e) => e.name)).toEqual(["Álbum de família"]);
    expect(scopeList(entries, "de   fam").groups).toHaveLength(1);
    expect(scopeList(entries, "news").channels.map((e) => e.name)).toEqual(["Zebra news"]);
    const none = scopeList(entries, "nothing like it");
    expect(none.groups).toHaveLength(0);
    expect(none.channels).toHaveLength(0);
    expect(foldForSearch("  Émile  Zola ")).toBe("emile zola");
  });

  it("offers a search field only past a screen of rows", () => {
    expect(showsListSearch(3)).toBe(false);
    expect(showsListSearch(40)).toBe(true);
  });

  it("counts the chosen among the scopes the person is in", () => {
    const on = new Set([entries[1]!.key, "group:ffff"]);
    expect(chosenCount(entries, (e) => on.has(e.key))).toBe(1);
  });

  it("opens the list on choosing only while nothing is chosen yet", () => {
    expect(opensListOnChoose(0)).toBe(true);
    expect(opensListOnChoose(2)).toBe(false);
  });

  it("reads a channel id's hex as bytes and refuses anything else", () => {
    expect(channelIdBytes("0a0B")).toEqual(new Uint8Array([10, 11]));
    expect(channelIdBytes("abc")).toBeNull();
    expect(channelIdBytes("zz")).toBeNull();
    expect(channelIdBytes("")).toBeNull();
  });
});

describe("against the real rules", () => {
  beforeAll(async () => {
    const wasmPath = fileURLToPath(new URL("../../wasm/lakko_ffi_wasm_bg.wasm", import.meta.url));
    await init({ module_or_path: readFileSync(wasmPath) });
  });

  it("agrees with the wasm enum values", () => {
    expect(MODE_NOBODY).toBe(wasm.VisibilityMode.Nobody);
    expect(MODE_SOME).toBe(wasm.VisibilityMode.Some);
    expect(MODE_EVERYONE).toBe(wasm.VisibilityMode.Everyone);
    expect(FORM_ALL).toBe(wasm.GroupsForm.All);
    expect(FORM_CHOSEN).toBe(wasm.GroupsForm.Chosen);
    expect(SCOPE_GROUP).toBe(wasm.ScopeKind.Group);
    expect(SCOPE_CHANNEL).toBe(wasm.ScopeKind.Channel);
  });

  it("reads the default for a new account as Some people with both parts, all groups", () => {
    const s = wasm.defaultVisibilitySetting();
    expect(viewOf(s)).toEqual({ mode: "some", contactsOfContacts: true, groups: "all" });
    s.free();
  });

  it("reads Nobody after the last part is turned off, and Everyone when chosen", () => {
    const s0 = wasm.defaultVisibilitySetting();
    const s1 = wasm.setContactsOfContacts(s0, false);
    const s2 = wasm.setGroupsVisible(s1, false);
    expect(viewOf(s2).mode).toBe("nobody");
    expect(visibleRows(viewOf(s2)).parts).toBe(false);
    const s3 = wasm.chooseVisibilityMode(s2, wasm.VisibilityMode.Everyone);
    expect(viewOf(s3)).toMatchObject({ mode: "everyone" });
    for (const s of [s0, s1, s2, s3]) s.free();
  });

  it("reads the chosen list the rules keep, and excluding one under all leaves the rest", () => {
    const a = entry("group", 1, "A");
    const b = entry("group", 2, "B");
    const c = entry("channel", 3, "C");
    const s0 = wasm.defaultVisibilitySetting();
    const all = [a, b, c].map((e) => new wasm.ScopeId(e.kind === "group" ? SCOPE_GROUP : SCOPE_CHANNEL, e.id));
    const excluded = new wasm.ScopeId(SCOPE_GROUP, b.id);
    const s1 = wasm.excludeScopeUnderAll(s0, all, excluded);
    expect(viewOf(s1).groups).toBe("chosen");
    const chosen = s1.chosen;
    const keys = chosenKeysOf(chosen);
    for (const x of chosen) x.free();
    expect(keys.has(a.key)).toBe(true);
    expect(keys.has(b.key)).toBe(false);
    expect(keys.has(c.key)).toBe(true);
    expect(chosenCount([a, b, c], (e) => keys.has(e.key))).toBe(2);
    expect(groupsHelper(viewOf(s1), 2, 3)).toBe("Only the ones you choose (2 of 3).");
    excluded.free();
    s0.free();
    s1.free();
  });
});
