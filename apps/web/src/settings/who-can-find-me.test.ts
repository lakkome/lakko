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
import { accountIdsHexOf, concatAccountIds, contactsNarrowed, NO_ACCOUNT_ID } from "./contact-visibility";
import {
  channelIdBytes,
  chosenCount,
  chosenKeysOf,
  contactEntries,
  contactList,
  contactsOfContactsHelper,
  CONTACTS_FORM_ALL,
  CONTACTS_FORM_CHOSEN,
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
  showsTheirContactsSwitch,
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
    expect(visibleRows({ mode: "nobody", contactsOfContacts: false, groups: null, contactsForm: "all" }).parts).toBe(false);
    expect(visibleRows({ mode: "everyone", contactsOfContacts: false, groups: null, contactsForm: "all" }).parts).toBe(false);
    expect(visibleRows({ mode: "some", contactsOfContacts: true, groups: null, contactsForm: "all" })).toEqual({
      parts: true,
      groupsForm: false,
      listDisclosure: false,
      contactsForm: true,
      contactsDisclosure: false,
    });
  });

  it("shows the form only while the groups part is on, and the disclosure only under chosen", () => {
    expect(visibleRows({ mode: "some", contactsOfContacts: false, groups: "all", contactsForm: "all" })).toEqual({
      parts: true,
      groupsForm: true,
      listDisclosure: false,
      contactsForm: false,
      contactsDisclosure: false,
    });
    expect(visibleRows({ mode: "some", contactsOfContacts: false, groups: "chosen", contactsForm: "all" })).toEqual({
      parts: true,
      groupsForm: true,
      listDisclosure: true,
      contactsForm: false,
      contactsDisclosure: false,
    });
  });

  it("offers the Members can find me switch only while groups are part of the setting", () => {
    expect(showsMembersSwitch({ mode: "some", contactsOfContacts: true, groups: "all", contactsForm: "all" })).toBe(true);
    expect(showsMembersSwitch({ mode: "some", contactsOfContacts: false, groups: "chosen", contactsForm: "all" })).toBe(true);
    expect(showsMembersSwitch({ mode: "some", contactsOfContacts: true, groups: null, contactsForm: "all" })).toBe(false);
    expect(showsMembersSwitch({ mode: "nobody", contactsOfContacts: false, groups: null, contactsForm: "all" })).toBe(false);
    // Everyone includes every group, but there is nothing to switch.
    expect(showsMembersSwitch({ mode: "everyone", contactsOfContacts: false, groups: null, contactsForm: "all" })).toBe(false);
  });
});

describe("the rows of Contacts of my contacts (UX section 3)", () => {
  const some = { mode: "some", contactsOfContacts: true, groups: null } as const;

  it("shows the form while the part is on, and the disclosure only under chosen", () => {
    expect(visibleRows({ ...some, contactsForm: "all" })).toMatchObject({
      contactsForm: true,
      contactsDisclosure: false,
    });
    expect(visibleRows({ ...some, contactsForm: "chosen" })).toMatchObject({
      contactsForm: true,
      contactsDisclosure: true,
    });
    expect(visibleRows({ ...some, contactsOfContacts: false, contactsForm: "chosen" })).toMatchObject({
      contactsForm: false,
      contactsDisclosure: false,
    });
  });

  it("offers Their contacts can find me only while the part is on, under either form", () => {
    expect(showsTheirContactsSwitch({ ...some, contactsForm: "all" })).toBe(true);
    expect(showsTheirContactsSwitch({ ...some, contactsForm: "chosen" })).toBe(true);
    expect(showsTheirContactsSwitch({ ...some, contactsOfContacts: false, contactsForm: "all" })).toBe(false);
    expect(showsTheirContactsSwitch({ ...some, mode: "everyone", contactsForm: "all" })).toBe(false);
  });
});

describe("the list of contacts (UX section 4.1)", () => {
  const person = (name: string, byte: string, accountIdHex?: string) => ({
    signaturePubkeyB64: byte,
    name,
    ...(accountIdHex === undefined ? {} : { accountIdHex }),
  });
  const id = (b: string): string => b.repeat(16);

  it("leaves out a contact with no known account id, who cannot be chosen, so M does not count them", () => {
    const entries = contactEntries(
      [person("Anna", "a", id("0a")), person("No Id", "b"), person("Bad", "c", "zz")],
      (c) => c.name,
    );
    expect(entries.map((e) => e.name)).toEqual(["Anna"]);
  });

  it("sorts as the Contacts screen does and lower-cases the id", () => {
    const entries = contactEntries(
      [person("bob", "b", id("0B")), person("Ánna", "a", id("0a")), person("7", "c", id("0c"))],
      (c) => c.name,
    );
    expect(entries.map((e) => e.name)).toEqual(["Ánna", "bob", "7"]);
    expect(entries[1]?.accountIdHex).toBe(id("0b"));
  });

  it("filters by any part of the name, case and accents aside", () => {
    const entries = contactEntries([person("Ánna Weber", "a", id("0a")), person("Bob", "b", id("0b"))], (c) => c.name);
    expect(contactList(entries, "webe").map((e) => e.name)).toEqual(["Ánna Weber"]);
    expect(contactList(entries, "anna")).toHaveLength(1);
    expect(contactList(entries, "")).toHaveLength(2);
  });
});

describe("copy", () => {
  it("counts N of M under chosen and describes all otherwise", () => {
    expect(groupsHelper({ groups: "chosen" }, 2, 5)).toBe(
      "Only the ones you choose (2 of 5).",
    );
    expect(groupsHelper({ groups: "all" }, 2, 5)).toBe(
      "Everyone in the groups and channels you're in.",
    );
  });

  it("says contacts of contacts is spreading only while it is on and was just turned on", () => {
    const on = { contactsOfContacts: true, contactsForm: "all" } as const;
    const off = { contactsOfContacts: false, contactsForm: "all" } as const;
    expect(contactsOfContactsHelper(on, 0, 0, true)).toBe(
      "Contacts of your contacts can find you once your contacts next open Lakko.",
    );
    expect(contactsOfContactsHelper(on, 0, 0, false)).toBe("People your contacts have added.");
    expect(contactsOfContactsHelper(off, 0, 0, true)).toBe("People your contacts have added.");
  });

  it("counts N of M under Only the ones I choose, 0 of M included", () => {
    const chosen = { contactsOfContacts: true, contactsForm: "chosen" } as const;
    expect(contactsOfContactsHelper(chosen, 2, 5, false)).toBe(
      "Only the contacts of the people you choose (2 of 5).",
    );
    expect(contactsOfContactsHelper(chosen, 0, 5, false)).toBe(
      "Only the contacts of the people you choose (0 of 5).",
    );
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
    expect(COPY.nobodyHelper()).toBe("Only people with your invite link or QR code can reach you.");
    expect(COPY.everyoneHelper()).toBe("Anyone can find you by your username or name.");
    expect(COPY.listEmpty()).toBe("You're not in any groups or channels yet.");
    expect(COPY.excludedNotice()).toBe(
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
    expect(CONTACTS_FORM_ALL).toBe(wasm.ContactsForm.All);
    expect(CONTACTS_FORM_CHOSEN).toBe(wasm.ContactsForm.Chosen);
    expect(SCOPE_GROUP).toBe(wasm.ScopeKind.Group);
    expect(SCOPE_CHANNEL).toBe(wasm.ScopeKind.Channel);
  });

  it("reads the default for a new account as Some people with both parts, all groups", () => {
    const s = wasm.defaultVisibilitySetting();
    expect(viewOf(s)).toEqual({ mode: "some", contactsOfContacts: true, contactsForm: "all", groups: "all" });
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

  it("reads the contacts form and the chosen contacts the rules keep", () => {
    const a = new Uint8Array(16).fill(1);
    const b = new Uint8Array(16).fill(2);
    const s0 = wasm.defaultVisibilitySetting();
    expect(viewOf(s0).contactsForm).toBe("all");
    const s1 = wasm.excludeContactUnderAll(s0, concatAccountIds(["01".repeat(16), "02".repeat(16)]), b);
    expect(viewOf(s1).contactsForm).toBe("chosen");
    expect(accountIdsHexOf(s1.chosenContacts)).toEqual(["01".repeat(16)]);
    expect(wasm.allowsContact(s1, a)).toBe(true);
    expect(wasm.allowsContact(s1, b)).toBe(false);
    s0.free();
    s1.free();
  });

  describe("narrowing moves the fof key (ADR-0060 section 4)", () => {
    const a = new Uint8Array(16).fill(1);
    const b = new Uint8Array(16).fill(2);
    const ids = concatAccountIds(["01".repeat(16), "02".repeat(16)]);

    it("switching a contact off narrows, switching one on does not", () => {
      const s0 = wasm.defaultVisibilitySetting();
      const both = wasm.setContactChosen(wasm.setContactChosen(wasm.setContactsForm(s0, wasm.ContactsForm.Chosen), a, true), b, true);
      const one = wasm.setContactChosen(both, b, false);
      expect(contactsNarrowed(wasm, both, one)).toBe(true);
      expect(contactsNarrowed(wasm, one, both)).toBe(false);
    });

    it("moving from All to Only the ones I choose narrows, and back does not", () => {
      const all = wasm.defaultVisibilitySetting();
      const chosen = wasm.setContactsForm(all, wasm.ContactsForm.Chosen);
      expect(contactsNarrowed(wasm, all, chosen)).toBe(true);
      expect(contactsNarrowed(wasm, chosen, all)).toBe(false);
    });

    it("excluding one under All narrows", () => {
      const all = wasm.defaultVisibilitySetting();
      expect(contactsNarrowed(wasm, all, wasm.excludeContactUnderAll(all, ids, b))).toBe(true);
    });

    it("leaving Everyone for a chosen list narrows", () => {
      const chosen = wasm.setContactsForm(wasm.defaultVisibilitySetting(), wasm.ContactsForm.Chosen);
      const everyone = wasm.chooseVisibilityMode(chosen, wasm.VisibilityMode.Everyone);
      expect(contactsNarrowed(wasm, everyone, chosen)).toBe(true);
    });

    it("with the part off there is no key to move", () => {
      const all = wasm.defaultVisibilitySetting();
      expect(contactsNarrowed(wasm, all, wasm.setContactsOfContacts(all, false))).toBe(false);
    });

    it("a contact with no known account id is allowed under All and not under a chosen list", () => {
      const all = wasm.defaultVisibilitySetting();
      expect(wasm.allowsContact(all, NO_ACCOUNT_ID)).toBe(true);
      expect(wasm.allowsContact(wasm.setContactsForm(all, wasm.ContactsForm.Chosen), NO_ACCOUNT_ID)).toBe(false);
    });

    it("a stored setting of the old format reads as nothing: the store falls back to the default", () => {
      expect(() => wasm.decodeVisibilityEnvelope(new Uint8Array(8))).toThrow();
    });
  });
});
