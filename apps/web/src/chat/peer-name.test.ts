// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * The one chat-title rule the header, the confirmations and the ringing
 * banner share. The regression pinned here: a two-person GROUP renamed via
 * the members panel showed the new name in the list but the other person's
 * name in the header, because the size-based 1x1 branch won over the kind.
 */
import { describe, expect, it } from "vitest";

import { peerName } from "./peer-name";

const OWN = "own";
const PEER = "peer";
const resolveName = (k: string): string => (k === PEER ? "Ivan IOS" : "Someone");

describe("peerName", () => {
  it("a 1x1 is the other person", () => {
    expect(
      peerName({
        ownKeyB64: OWN,
        memberKeysB64: [OWN, PEER],
        resolveName,
        linkedContactName: undefined,
        label: "Web group",
      }),
    ).toBe("Ivan IOS");
  });

  it("a two-person GROUP is titled by its label, not by the other person (ADR-0051)", () => {
    expect(
      peerName({
        ownKeyB64: OWN,
        memberKeysB64: [OWN, PEER],
        resolveName,
        linkedContactName: "Ivan (contact)",
        label: "Web group",
        isGroup: true,
      }),
    ).toBe("Web group");
  });

  it("a group with an empty label falls back to 'Group', never to a person", () => {
    expect(
      peerName({
        ownKeyB64: OWN,
        memberKeysB64: [OWN, PEER],
        resolveName,
        linkedContactName: undefined,
        label: "",
        isGroup: true,
      }),
    ).toBe("Group");
  });

  it("a linked contact names an accept-created chat that is not a 1x1 roster", () => {
    expect(
      peerName({
        ownKeyB64: OWN,
        memberKeysB64: [OWN],
        resolveName,
        linkedContactName: "Mum",
        label: "x",
      }),
    ).toBe("Mum");
  });

  it("falls back to the label, then to 'Peer'", () => {
    const base = { ownKeyB64: OWN, memberKeysB64: [OWN, "a", "b"], resolveName, linkedContactName: undefined };
    expect(peerName({ ...base, label: "Trio" })).toBe("Trio");
    expect(peerName({ ...base, label: "" })).toBe("Peer");
  });
});
