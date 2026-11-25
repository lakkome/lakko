// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * The web side of chat folders, the pure part: what an edit writes, how a
 * row maps to a reference and back, and the Settings suggestions. The rule
 * itself (`folder_matches`) and the fold are Rust and tested there; these
 * test that the web hands them the right registers - in particular that an
 * edit never rewrites a register it did not change (ADR-0058 section 5a).
 */
import { describe, expect, it } from "vitest";

import {
  b64ToBytes,
  bytesToB64,
  chatCountLabel,
  clipFolderName,
  diffFolderDraft,
  draftCanSave,
  draftIsDirty,
  EXCLUDE_ARCHIVED,
  EXCLUDE_READ,
  INCLUDE_CHANNELS,
  INCLUDE_GROUPS,
  INCLUDE_ONE_TO_ONE,
  membershipOf,
  newFolderDraft,
  openSuggestions,
  refOfRow,
  reorderEdit,
  appendToOrderEdit,
  ruleIdOfRef,
  ruleInputsOf,
  ruleRowId,
  stampEdits,
  tabAriaLabel,
  toggleMembershipEdit,
  unreadConversationCount,
  type ConversationRef,
  type FolderModel,
} from "./folder-model";

const FID = "aa".repeat(16);
const G1: ConversationRef = { kind: 2, id: new Uint8Array(32).fill(1) };
const G2: ConversationRef = { kind: 1, id: new Uint8Array(32).fill(2) };
const CH: ConversationRef = { kind: 3, id: new Uint8Array(32).fill(3) };

function folder(over: Partial<FolderModel> = {}): FolderModel {
  return {
    idHex: FID,
    name: "Work",
    includeKinds: 0,
    excludeFlags: EXCLUDE_ARCHIVED,
    included: [],
    excluded: [],
    pinned: [],
    ...over,
  };
}

describe("row ids and references", () => {
  it("prefix the namespace so a group id and a channel id can never collide", () => {
    expect(ruleRowId("chat", "x")).toBe("g:x");
    expect(ruleRowId("group", "x")).toBe("g:x");
    expect(ruleRowId("channel", "x")).toBe("c:x");
    expect(ruleRowId("group", "x")).not.toBe(ruleRowId("channel", "x"));
  });

  it("map a row to a reference and back to the same rule id", () => {
    const b64 = bytesToB64(G1.id);
    const ref = refOfRow("group", b64);
    expect(ref?.kind).toBe(2);
    expect(ruleIdOfRef(ref as ConversationRef)).toBe(ruleRowId("group", b64));

    const hex = "03".repeat(32);
    const cref = refOfRow("channel", hex);
    expect(cref?.kind).toBe(3);
    expect(ruleIdOfRef(cref as ConversationRef)).toBe(ruleRowId("channel", hex));
  });

  it("treat a one-to-one and a group reference with one id as one conversation", () => {
    const asChat: ConversationRef = { kind: 1, id: G1.id };
    expect(ruleIdOfRef(asChat)).toBe(ruleIdOfRef(G1));
  });

  it("refuse ids that are not what the list writes", () => {
    expect(refOfRow("channel", "zz")).toBeNull();
    expect(refOfRow("group", "***")).toBeNull();
  });

  it("round-trip base64", () => {
    expect(b64ToBytes(bytesToB64(G1.id))).toEqual(G1.id);
  });
});

describe("ruleInputsOf", () => {
  it("hands the rule the bits as held and the hand lists as rule ids", () => {
    const f = folder({ includeKinds: INCLUDE_CHANNELS, included: [G1], excluded: [CH] });
    expect(ruleInputsOf(f)).toEqual({
      includeKinds: INCLUDE_CHANNELS,
      excludeFlags: EXCLUDE_ARCHIVED,
      included: [ruleIdOfRef(G1)],
      excluded: [ruleIdOfRef(CH)],
    });
  });
});

describe("diffFolderDraft - the registers a Save writes", () => {
  it("a new folder writes its meta and one member per hand-added chat", () => {
    const edits = diffFolderDraft(FID, null, { ...newFolderDraft(G1), name: " Work " });
    expect(edits).toEqual([
      { t: "meta", folderIdHex: FID, name: "Work", includeKinds: 0, excludeFlags: EXCLUDE_ARCHIVED },
      { t: "member", folderIdHex: FID, ref: G1, state: 1 },
    ]);
  });

  it("an unchanged folder writes nothing", () => {
    const f = folder({ included: [G1], excluded: [CH] });
    expect(diffFolderDraft(FID, f, { ...f })).toEqual([]);
  });

  it("a rename writes only the meta register", () => {
    const f = folder({ included: [G1] });
    expect(diffFolderDraft(FID, f, { ...f, name: "Office" })).toEqual([
      { t: "meta", folderIdHex: FID, name: "Office", includeKinds: 0, excludeFlags: EXCLUDE_ARCHIVED },
    ]);
  });

  it("adding one chat writes one member register, leaving the others alone", () => {
    const f = folder({ included: [G1] });
    expect(diffFolderDraft(FID, f, { ...f, included: [G1, G2] })).toEqual([
      { t: "member", folderIdHex: FID, ref: G2, state: 1 },
    ]);
  });

  it("taking a chat out writes NONE, never deletes the register", () => {
    const f = folder({ included: [G1, G2] });
    expect(diffFolderDraft(FID, f, { ...f, included: [G2] })).toEqual([
      { t: "member", folderIdHex: FID, ref: G1, state: 0 },
    ]);
  });

  it("moving a chat from included to excluded writes one EXCLUDED register", () => {
    const f = folder({ included: [G1] });
    expect(diffFolderDraft(FID, f, { ...f, included: [], excluded: [G1] })).toEqual([
      { t: "member", folderIdHex: FID, ref: G1, state: 2 },
    ]);
  });

  it("keeps a chat this device cannot resolve: nothing is written for it", () => {
    // The editor keeps every hand-listed ref in the draft whether or not it
    // can show it, so an unresolved one is in both lists and diffs to nothing.
    const unresolved: ConversationRef = { kind: 3, id: new Uint8Array([9, 9, 9]) };
    const f = folder({ included: [unresolved, G1] });
    const edits = diffFolderDraft(FID, f, { ...f, included: [unresolved, G1, G2] });
    expect(edits.some((e) => e.t === "member" && e.ref === unresolved)).toBe(false);
  });
});

describe("Save and dirty", () => {
  const valid = (n: string): boolean => n.length > 0;
  it("needs a name and something included", () => {
    expect(draftCanSave({ ...newFolderDraft(), name: "" , includeKinds: INCLUDE_GROUPS }, valid)).toBe(false);
    expect(draftCanSave({ ...newFolderDraft(), name: "Work" }, valid)).toBe(false);
    expect(draftCanSave({ ...newFolderDraft(), name: "Work", includeKinds: INCLUDE_GROUPS }, valid)).toBe(true);
    expect(draftCanSave({ ...newFolderDraft(G1), name: "Work" }, valid)).toBe(true);
    expect(draftCanSave({ ...newFolderDraft(G1), name: "Work" }, () => false)).toBe(false);
  });

  it("a new folder starts with Archived excluded and nothing else", () => {
    const d = newFolderDraft();
    expect(d.excludeFlags).toBe(EXCLUDE_ARCHIVED);
    expect(d.includeKinds).toBe(0);
  });

  it("is dirty only when something changed, regardless of list order", () => {
    const f = folder({ included: [G1, G2] });
    expect(draftIsDirty(f, { ...f, included: [G2, G1] })).toBe(false);
    expect(draftIsDirty(f, { ...f, name: "X" })).toBe(true);
    expect(draftIsDirty(f, { ...f, excludeFlags: 0 })).toBe(true);
  });
});

describe("Add to folder (UX spec section 5)", () => {
  it("not in it: adds by hand", () => {
    const f = folder();
    const m = membershipOf(f, G1, false);
    expect(m).toBe("not-in");
    expect(toggleMembershipEdit(f, G1, m)).toMatchObject({ t: "member", state: 1 });
  });

  it("excluded by hand: removes the exclusion instead", () => {
    const f = folder({ includeKinds: INCLUDE_GROUPS, excluded: [G1] });
    const m = membershipOf(f, G1, false);
    expect(m).toBe("excluded-by-hand");
    expect(toggleMembershipEdit(f, G1, m)).toMatchObject({ t: "member", state: 0 });
  });

  it("in it by hand: takes it out", () => {
    const f = folder({ included: [G1] });
    const m = membershipOf(f, G1, true);
    expect(m).toBe("in-by-hand");
    expect(toggleMembershipEdit(f, G1, m)).toMatchObject({ t: "member", state: 0 });
  });

  it("in it by a kind toggle: excludes it by hand", () => {
    const f = folder({ includeKinds: INCLUDE_GROUPS });
    const m = membershipOf(f, G1, true);
    expect(m).toBe("in-by-kind");
    expect(toggleMembershipEdit(f, G1, m)).toMatchObject({ t: "member", state: 2 });
  });

  it("writes the exact reference the folder holds", () => {
    // The chat grew into a group after it was added: the folder still holds
    // the one-to-one reference, and taking it out must name THAT register.
    const held: ConversationRef = { kind: 1, id: G1.id };
    const f = folder({ included: [held] });
    const e = toggleMembershipEdit(f, G1, "in-by-hand");
    expect(e).toEqual({ t: "member", folderIdHex: FID, ref: held, state: 0 });
  });
});

describe("pins and order", () => {
  it("reorder writes the whole order once", () => {
    const fs = ["01", "02", "03"].map((x) => folder({ idHex: x.repeat(16) }));
    expect(reorderEdit(fs, 2, 0)).toEqual({
      t: "order",
      folderIdsHex: ["03".repeat(16), "01".repeat(16), "02".repeat(16)],
    });
    expect(reorderEdit(fs, 1, 1)).toBeNull();
    expect(reorderEdit(fs, 0, 3)).toBeNull();
  });

  it("a create puts the new folder at the end of the written order", () => {
    // Without the order a rename moved the folder to the end (live 2026-09-30).
    const fs = ["01", "02"].map((x) => folder({ idHex: x.repeat(16) }));
    expect(appendToOrderEdit(fs, "03".repeat(16))).toEqual({
      t: "order",
      folderIdsHex: ["01".repeat(16), "02".repeat(16), "03".repeat(16)],
    });
    expect(appendToOrderEdit([], "03".repeat(16))).toEqual({
      t: "order",
      folderIdsHex: ["03".repeat(16)],
    });
    // Never listed twice.
    expect(appendToOrderEdit(fs, "02".repeat(16))).toEqual({
      t: "order",
      folderIdsHex: ["01".repeat(16), "02".repeat(16)],
    });
  });
});

describe("stampEdits", () => {
  it("stamps each register separately, in order", () => {
    let n = 100n;
    const out = stampEdits(
      [
        { t: "removed", folderIdHex: FID },
        { t: "order", folderIdsHex: [] },
      ],
      () => (n += 1n),
    );
    expect(out.map((e) => e.hlc)).toEqual([101n, 102n]);
  });
});

describe("suggestions", () => {
  it("offers only what the person does not have, compared by rule not name", () => {
    const renamedUnread = folder({
      name: "Inbox",
      includeKinds: INCLUDE_ONE_TO_ONE | INCLUDE_GROUPS | INCLUDE_CHANNELS,
      excludeFlags: EXCLUDE_READ,
    });
    const keys = openSuggestions([renamedUnread]).map((s) => s.key);
    expect(keys).toEqual(["personal", "groups", "channels"]);
    // Same name, different rule: still offered.
    const namedGroups = folder({ name: "Groups", includeKinds: INCLUDE_ONE_TO_ONE });
    expect(openSuggestions([namedGroups]).map((s) => s.key)).toContain("groups");
  });
});

describe("counts and labels", () => {
  it("counts conversations with unread, muted ones not counted", () => {
    expect(
      unreadConversationCount([
        { muted: false, hasUnread: true },
        { muted: true, hasUnread: true },
        { muted: false, hasUnread: false },
      ]),
    ).toBe(1);
  });

  it("labels", () => {
    expect(chatCountLabel(1)).toBe("1 chat");
    expect(chatCountLabel(12)).toBe("12 chats");
    expect(tabAriaLabel("Work", 3)).toBe("Work, 3 unread chats");
    expect(tabAriaLabel("Work", 0)).toBe("Work");
  });

  it("clips a name to 24 characters without cutting an emoji", () => {
    expect(Array.from(clipFolderName("x".repeat(30))).length).toBe(24);
    const withEmoji = "a".repeat(23) + "\u{1F600}" + "b";
    expect(clipFolderName(withEmoji)).toBe("a".repeat(23) + "\u{1F600}");
  });
});
