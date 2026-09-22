// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import { describe, expect, it } from "vitest";

import { applyOp } from "./message-ops";
import {
  foldOpsIntoStored,
  opsFromPersisted,
  persistedWithOps,
  type IncomingOp,
} from "./message-ops-store";
import type { MessageOpState } from "./message-ops";
import type { PersistedMessage } from "./message-log-store";

const T = "2026-09-22T12:00:00Z";
const ALICE = "alice-key-b64";
const BOB = "bob-key-b64";

function row(over: Partial<PersistedMessage> = {}): PersistedMessage {
  return {
    messageId: "7",
    direction: "incoming",
    text: "the original",
    attachments: [],
    createdAt: T,
    authorKeyB64: ALICE,
    ...over,
  };
}

const attachment = {
  blobIdB64: "YmxvYg==",
  keyB64: "a2V5",
  nonceB64: "bm9uY2U=",
  mime: "image/png",
  sizeStr: "10",
  name: "a.png",
};

function state(over: Partial<MessageOpState> = {}): MessageOpState {
  return { reactions: new Map(), ...over };
}

describe("persistedWithOps", () => {
  it("returns the row itself when there is nothing to fold", () => {
    const m = row();
    // Identity, not just equality: an untouched transcript must not
    // re-allocate every row on every debounced persist.
    expect(persistedWithOps(m, undefined)).toBe(m);
    expect(persistedWithOps(m, state())).toBe(m);
  });

  it("writes the edited text as the row's text, with the marker", () => {
    const out = persistedWithOps(row(), state({ editedText: "the edit" }));
    // The text is replaced rather than kept beside the original, so a reader
    // that knows nothing of `edited` still shows the right words.
    expect(out.text).toBe("the edit");
    expect(out.edited).toBe(true);
    expect(out.deleted).toBeUndefined();
  });

  it("writes a deleted row with no text and no attachments", () => {
    const out = persistedWithOps(
      row({ text: "a secret", attachments: [attachment] }),
      state({ deleted: true }),
    );
    // The tombstone renders from the flag. Keeping the plaintext - or the
    // attachment's content key - would put a message the user deleted on
    // disk permanently, for nothing.
    expect(out.deleted).toBe(true);
    expect(out.text).toBe("");
    expect(out.attachments).toEqual([]);
  });

  it("lets a delete win over an earlier edit, keeping neither text", () => {
    const out = persistedWithOps(
      row({ text: "original" }),
      state({ editedText: "edited", deleted: true }),
    );
    expect(out.text).toBe("");
    expect(out.edited).toBeUndefined();
    expect(out.deleted).toBe(true);
  });

  it("keeps reactions on a deleted row", () => {
    // The message is gone; who reacted to it is still the group's state, and
    // the tombstone carries the chips.
    const out = persistedWithOps(
      row(),
      state({ deleted: true, reactions: new Map([["👍", new Set([ALICE])]]) }),
    );
    expect(out.reactions).toEqual([{ reactorKeyB64: ALICE, emoji: "👍" }]);
  });

  it("stores reactions sorted, so an unchanged re-persist is byte-identical", () => {
    const one = persistedWithOps(
      row(),
      state({
        reactions: new Map([
          ["🙏", new Set([BOB, ALICE])],
          ["👍", new Set([BOB])],
        ]),
      }),
    );
    // The same reactions inserted in the other order - which is what a device
    // that read them back off disk produces - must serialize identically.
    const two = persistedWithOps(
      row(),
      state({
        reactions: new Map([
          ["👍", new Set([BOB])],
          ["🙏", new Set([ALICE, BOB])],
        ]),
      }),
    );
    expect(one.reactions).toEqual(two.reactions);
    expect(one.reactions).toEqual([
      { reactorKeyB64: BOB, emoji: "👍" },
      { reactorKeyB64: ALICE, emoji: "🙏" },
      { reactorKeyB64: BOB, emoji: "🙏" },
    ]);
  });
});

describe("opsFromPersisted", () => {
  it("builds no entry for a row that was never touched", () => {
    expect(opsFromPersisted([row()]).size).toBe(0);
  });

  it("restores the edited text so the marker and the next write survive", () => {
    // `editedText` looks redundant against the row's own text and is not: the
    // next persist reads the OVERLAY, so an overlay that forgot the edit
    // would write `edited` straight back off again.
    const stored = persistedWithOps(row(), state({ editedText: "the edit" }));
    const back = opsFromPersisted([stored]).get(7);
    expect(back?.editedText).toBe("the edit");
    expect(persistedWithOps(stored, back).edited).toBe(true);
  });

  it("restores a delete", () => {
    const stored = persistedWithOps(row(), state({ deleted: true }));
    expect(opsFromPersisted([stored]).get(7)?.deleted).toBe(true);
  });

  it("restores reactions as the overlay's emoji-to-reactors map", () => {
    const stored = persistedWithOps(
      row(),
      state({ reactions: new Map([["👍", new Set([ALICE, BOB])]]) }),
    );
    const back = opsFromPersisted([stored]).get(7);
    expect([...(back?.reactions.get("👍") ?? [])].sort()).toEqual([ALICE, BOB].sort());
  });

  it("survives a full write, read, write cycle - the defect this closes", () => {
    // Delete a message, persist, reload, persist again. Before this module the
    // second write put the original text back, and the op row was already
    // behind the cursor, so the message returned for good.
    const first = persistedWithOps(row({ text: "delete me" }), state({ deleted: true }));
    const rebuilt = opsFromPersisted([first]);
    const second = persistedWithOps(first, rebuilt.get(7));
    expect(second.deleted).toBe(true);
    expect(second.text).toBe("");
  });

  it("ignores a row whose id is not a number", () => {
    expect(opsFromPersisted([row({ messageId: "not-an-id", deleted: true })]).size).toBe(0);
  });

  it("drops a stored reaction with an empty emoji", () => {
    // The overlay prunes an emoji whose last reactor toggles off, so a chip
    // with no glyph would be one nothing can remove.
    const back = opsFromPersisted([
      row({ reactions: [{ reactorKeyB64: ALICE, emoji: "" }] }),
    ]);
    expect(back.size).toBe(0);
  });
});

describe("foldOpsIntoStored", () => {
  const del: IncomingOp = {
    op: "delete",
    target: 7,
    text: "",
    emoji: undefined,
    senderKey: ALICE,
  };

  it("returns the input itself when there are no ops", () => {
    const messages = [row()];
    expect(foldOpsIntoStored(messages, [])).toBe(messages);
  });

  it("applies the author's own delete", () => {
    const [out] = foldOpsIntoStored([row()], [del]);
    expect(out?.deleted).toBe(true);
    expect(out?.text).toBe("");
  });

  it("refuses a delete from someone who is not the author", () => {
    // The gate that stops one member rewriting another's history, running on
    // the background door exactly as it does in the open chat.
    const out = foldOpsIntoStored([row()], [{ ...del, senderKey: BOB }]);
    expect(out[0]?.deleted).toBeUndefined();
  });

  it("refuses an edit from someone who is not the author", () => {
    const out = foldOpsIntoStored(
      [row()],
      [{ op: "edit", target: 7, text: "not yours", emoji: undefined, senderKey: BOB }],
    );
    expect(out[0]?.text).toBe("the original");
    expect(out[0]?.edited).toBeUndefined();
  });

  it("drops an unattributable op", () => {
    const out = foldOpsIntoStored([row()], [{ ...del, senderKey: undefined }]);
    expect(out[0]?.deleted).toBeUndefined();
  });

  it("fails closed on a row stored before the author identity was", () => {
    // The field is ABSENT, not null: a v1 row. Both read back as an unknown
    // author, and an unknown author authorizes nobody.
    const { authorKeyB64: _omitted, ...withoutAuthor } = row();
    const out = foldOpsIntoStored([withoutAuthor], [del]);
    expect(out[0]?.deleted).toBeUndefined();
  });

  it("drops an op whose target it does not hold", () => {
    const messages = [row()];
    expect(foldOpsIntoStored(messages, [{ ...del, target: 99 }])).toBe(messages);
  });

  it("accepts a reaction from any member, not only the author", () => {
    const out = foldOpsIntoStored(
      [row()],
      [{ op: "reaction", target: 7, text: "", emoji: "👍", senderKey: BOB }],
    );
    expect(out[0]?.reactions).toEqual([{ reactorKeyB64: BOB, emoji: "👍" }]);
  });

  it("toggles a reaction off when the same member sends it twice", () => {
    const react: IncomingOp = {
      op: "reaction",
      target: 7,
      text: "",
      emoji: "👍",
      senderKey: BOB,
    };
    const out = foldOpsIntoStored([row()], [react, react]);
    expect(out[0]?.reactions).toBeUndefined();
  });

  it("drops a reaction carrying no emoji", () => {
    const messages = [row()];
    const out = foldOpsIntoStored(messages, [
      { op: "reaction", target: 7, text: "", emoji: "", senderKey: BOB },
    ]);
    expect(out).toBe(messages);
  });

  it("applies ops on top of what the row already carries", () => {
    // The slot may already hold a delete from an earlier batch; a later
    // reaction must not undo it.
    const stored = persistedWithOps(row(), state({ deleted: true }));
    const out = foldOpsIntoStored(
      [stored],
      [{ op: "reaction", target: 7, text: "", emoji: "❤️", senderKey: BOB }],
    );
    expect(out[0]?.deleted).toBe(true);
    expect(out[0]?.reactions).toEqual([{ reactorKeyB64: BOB, emoji: "❤️" }]);
  });

  it("leaves rows the ops do not name alone", () => {
    const other = row({ messageId: "8", text: "untouched" });
    const out = foldOpsIntoStored([row(), other], [del]);
    expect(out[1]).toEqual(other);
  });

  it("agrees with the open chat's overlay for the same ops", () => {
    // The two doors must not drift: the background fold is the same
    // `applyOp` the chat view uses, and this pins that they agree.
    const viaOverlay = persistedWithOps(
      row(),
      applyOp(new Map(), { op: "edit", target: 7, text: "v2" }).get(7),
    );
    const viaStore = foldOpsIntoStored(
      [row()],
      [{ op: "edit", target: 7, text: "v2", emoji: undefined, senderKey: ALICE }],
    )[0];
    expect(viaStore).toEqual(viaOverlay);
  });
});
