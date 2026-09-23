// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import { describe, expect, it, vi } from "vitest";

import {
  bareName,
  findByName,
  findByNameMessage,
  looksLikeAName,
  type NameLookups,
} from "./find-by-name";

const NONE: NameLookups = {
  channel: async () => null,
  person: async () => null,
};

describe("bareName", () => {
  it("drops what people type and no namespace stores", () => {
    expect(bareName("  @anna ")).toBe("anna");
    expect(bareName("anna")).toBe("anna");
    expect(bareName("@@anna")).toBe("anna");
  });
});

describe("looksLikeAName", () => {
  it("accepts a canonical name, with or without the at-sign", () => {
    expect(looksLikeAName("@lakko_news")).toBe(true);
    expect(looksLikeAName("anna")).toBe(true);
  });

  it("refuses what the blinder would refuse, before it costs a proof of work", () => {
    expect(looksLikeAName("ab")).toBe(false);
    expect(looksLikeAName("a".repeat(33))).toBe(false);
    expect(looksLikeAName("anna smith")).toBe(false);
    expect(looksLikeAName("anna!")).toBe(false);
    expect(looksLikeAName("")).toBe(false);
  });
});

describe("findByName", () => {
  it("finds a channel", async () => {
    const found = await findByName("@news", { ...NONE, channel: async () => "abc123" });
    expect(found).toEqual({ kind: "channel", channelIdHex: "abc123", handle: "news" });
  });

  it("finds a person", async () => {
    const found = await findByName("anna", { ...NONE, person: async () => "anna" });
    expect(found).toEqual({ kind: "person", username: "anna" });
  });

  it("asks BOTH namespaces, because the person does not know which one holds the name", async () => {
    const channel = vi.fn(async () => null);
    const person = vi.fn(async () => null);
    await findByName("anna", { channel, person });
    expect(channel).toHaveBeenCalledWith("anna");
    expect(person).toHaveBeenCalledWith("anna");
  });

  it("stops at the channel when it answers, rather than spending a second lookup", async () => {
    const person = vi.fn(async () => null);
    await findByName("news", { channel: async () => "abc123", person });
    expect(person).not.toHaveBeenCalled();
  });

  it("says nothing was found when neither namespace holds it", async () => {
    expect(await findByName("ghost", NONE)).toEqual({ kind: "nothing" });
  });

  it("spends no lookup at all on something that cannot be a name", async () => {
    const channel = vi.fn(async () => null);
    const person = vi.fn(async () => null);
    expect(await findByName("no", { channel, person })).toEqual({ kind: "nothing" });
    expect(channel).not.toHaveBeenCalled();
    expect(person).not.toHaveBeenCalled();
  });

  it("does NOT say a person is missing when the connection is", async () => {
    // The worst thing this could do: tell someone their friend does not
    // exist because the network dropped.
    const dead: NameLookups = {
      channel: async () => {
        throw new Error("offline");
      },
      person: async () => {
        throw new Error("offline");
      },
    };
    expect(await findByName("anna", dead)).toEqual({ kind: "unreachable" });
  });

  it("still answers when only one namespace is reachable", async () => {
    const half: NameLookups = {
      channel: async () => {
        throw new Error("offline");
      },
      person: async () => null,
    };
    expect(await findByName("anna", half)).toEqual({ kind: "nothing" });
  });
});

describe("findByNameMessage", () => {
  it("names what was typed, with the at-sign people expect", () => {
    expect(findByNameMessage({ kind: "nothing" }, "anna")).toBe("Nothing found for @anna.");
  });

  it("blames the connection, not the name, when it could not ask", () => {
    expect(findByNameMessage({ kind: "unreachable" }, "anna")).toContain("connection");
  });

  it("says nothing when there is something to open instead", () => {
    expect(findByNameMessage({ kind: "person", username: "anna" }, "anna")).toBe("");
  });
});
