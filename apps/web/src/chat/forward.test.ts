// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * ADR-0052 decision 2 / `docs/ux/message-actions.md` sections 5 and 5.1 —
 * what a forward sends, what the bubble says, where a tap goes, and the
 * persisted round trip.
 */
import { describe, expect, it } from "vitest";

import { historyRowId } from "./lineage";
import {
  forwardJumpRow,
  forwardJumpTarget,
  forwardLineLabel,
  forwardPayloadOf,
  forwardRefFromPersisted,
  persistedFromForwardRef,
} from "./forward";

const ORIGIN = "b3JpZ2lu";
const PHOTO = { name: "photo.jpg" };

describe("forwardPayloadOf (section 5.1, what is sent)", () => {
  it("a first forward carries the text verbatim, the source refs, the author label and the origin", () => {
    const p = forwardPayloadOf(
      { text: "see you at 5?", attachments: [PHOTO], messageId: 41 },
      "Anna",
      ORIGIN,
      false,
    );
    expect(p).toEqual({
      text: "see you at 5?",
      attachments: [PHOTO],
      forward: { author: "Anna", origin: { groupIdB64: ORIGIN, messageId: 41 } },
    });
  });

  it("sends the EDITED text when the message was edited", () => {
    const p = forwardPayloadOf(
      { text: "before", editedText: "after", attachments: [], messageId: 1 },
      "You",
      ORIGIN,
      false,
    );
    expect(p.text).toBe("after");
  });

  it("names an own message the way the timeline does, You", () => {
    expect(forwardPayloadOf({ text: "hi", attachments: [], messageId: 1 }, "You", ORIGIN, false).forward.author).toBe(
      "You",
    );
  });

  it("a forward of a forward keeps the ORIGINAL name and origin, never the forwarder's", () => {
    const first = { groupIdB64: "Zmlyc3Q=", messageId: 7 };
    const p = forwardPayloadOf(
      {
        text: "hi",
        attachments: [],
        messageId: 99,
        forward: { author: "Anna", origin: first },
      },
      "Boris", // the forwarder, as the second timeline names them
      "c2Vjb25k",
      false,
    );
    expect(p.forward).toEqual({ author: "Anna", origin: first });
  });

  it("a forward of a forward that carried no name or origin invents neither", () => {
    const p = forwardPayloadOf(
      { text: "hi", attachments: [], messageId: 99, forward: {} },
      "Boris",
      "c2Vjb25k",
      false,
    );
    expect(p.forward).toEqual({});
    expect("author" in p.forward).toBe(false);
    expect("origin" in p.forward).toBe(false);
  });

  it("carries no reply quote and no reactions: the payload has only text, refs and the attribution", () => {
    const p = forwardPayloadOf(
      {
        text: "yes",
        attachments: [],
        messageId: 5,
        // A source that is a reply, with a quote; the type does not even
        // admit one, so it is passed through an index signature.
        ...({ reply: { target: 4, author: "Anna", excerpt: "at five?" } } as object),
      },
      "You",
      ORIGIN,
      false,
    );
    expect(Object.keys(p).sort()).toEqual(["attachments", "forward", "text"]);
    expect(JSON.stringify(p)).not.toContain("at five?");
  });
});

describe("forwardPayloadOf — ADR-0053 contract point 10, a one-to-one source", () => {
  it("a first forward out of a one-to-one carries the name and NO origin", () => {
    const p = forwardPayloadOf(
      { text: "see you at 5?", attachments: [PHOTO], messageId: 41 },
      "Anna",
      ORIGIN,
      true,
    );
    expect(p.forward).toEqual({ author: "Anna" });
    expect("origin" in p.forward).toBe(false);
    expect(JSON.stringify(p)).not.toContain(ORIGIN);
  });

  it("a forward of a forward out of a one-to-one drops the carried origin too", () => {
    const first = { groupIdB64: "Zmlyc3Q=", messageId: 7 };
    const p = forwardPayloadOf(
      { text: "hi", attachments: [], messageId: 99, forward: { author: "Anna", origin: first } },
      "Boris",
      "c2Vjb25k",
      true,
    );
    expect(p.forward).toEqual({ author: "Anna" });
  });

  it("the same source out of a group keeps its origin", () => {
    const p = forwardPayloadOf(
      { text: "hi", attachments: [], messageId: 41 },
      "Anna",
      ORIGIN,
      false,
    );
    expect(p.forward.origin).toEqual({ groupIdB64: ORIGIN, messageId: 41 });
  });
});

describe("forwardLineLabel (section 5.1, the bubble)", () => {
  it("names the original author", () => {
    expect(forwardLineLabel({ author: "Anna" })).toBe("Forwarded from Anna");
  });
  it("reads Forwarded when no name was carried, or an empty one", () => {
    expect(forwardLineLabel({})).toBe("Forwarded");
    expect(forwardLineLabel({ author: "" })).toBe("Forwarded");
  });
});

describe("forwardJumpTarget (section 5.1, tap to jump)", () => {
  const origin = { groupIdB64: ORIGIN, messageId: 41 };
  it("is the origin when this device holds that conversation", () => {
    expect(forwardJumpTarget({ origin }, (id) => id === ORIGIN)).toEqual(origin);
  });
  it("is nothing when it does not, and nothing when no origin was carried", () => {
    expect(forwardJumpTarget({ origin }, () => false)).toBeNull();
    expect(forwardJumpTarget({}, () => true)).toBeNull();
  });
});

describe("the persisted round trip", () => {
  it("stores the origin id as a string and reads it back as a number", () => {
    const ref = { author: "Anna", origin: { groupIdB64: ORIGIN, messageId: 41 } };
    const stored = persistedFromForwardRef(ref);
    expect(stored).toEqual({ author: "Anna", origin: { groupIdB64: ORIGIN, messageId: "41" } });
    expect(forwardRefFromPersisted(stored)).toEqual(ref);
  });

  it("keeps absent halves absent in both directions", () => {
    expect(persistedFromForwardRef({ author: "Anna" })).toEqual({ author: "Anna" });
    expect(persistedFromForwardRef({})).toEqual({});
    expect(forwardRefFromPersisted({ author: "Anna" })).toEqual({ author: "Anna" });
    expect(Object.keys(forwardRefFromPersisted({}) ?? { x: 1 })).toEqual([]);
  });

  it("an absent row reads as no forward; an unparseable origin id reads as no origin", () => {
    expect(forwardRefFromPersisted(undefined)).toBeUndefined();
    expect(
      forwardRefFromPersisted({ author: "A", origin: { groupIdB64: ORIGIN, messageId: "nope" } }),
    ).toEqual({ author: "A" });
  });
});

describe("forwardJumpRow: the jump into a conversation that moved (ADR-0065 R.5)", () => {
  const args = { messageId: 12, headGroupIdB64: "S2", heldHistory: ["G", "S1"], rowId: historyRowId };
  it("the current group: the message id itself", () => {
    expect(forwardJumpRow({ ...args, originGroupIdB64: "S2" })).toBe(12);
  });
  it("an older segment: the id its row is drawn under, by distance from the head", () => {
    expect(forwardJumpRow({ ...args, originGroupIdB64: "S1" })).toBe(historyRowId(12, 1));
    expect(forwardJumpRow({ ...args, originGroupIdB64: "G" })).toBe(historyRowId(12, 2));
  });
  it("a segment this device does not hold: nowhere", () => {
    expect(forwardJumpRow({ ...args, originGroupIdB64: "other" })).toBeNull();
  });
  it("a conversation that never moved is unchanged", () => {
    expect(
      forwardJumpRow({ originGroupIdB64: "G", messageId: 5, headGroupIdB64: "G", heldHistory: [], rowId: historyRowId }),
    ).toBe(5);
  });
});
