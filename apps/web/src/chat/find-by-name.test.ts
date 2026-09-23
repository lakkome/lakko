// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import { describe, expect, it, vi } from "vitest";

import { findByName, findByNameMessage, type NameLookups } from "./find-by-name";
import type { UiRules } from "./ui-rules";

const NONE: NameLookups = {
  channel: async () => null,
  person: async () => null,
};

/**
 * The GATE is not faked into agreement here - `looks_like_a_name` lives in
 * `crates/lakko-ui-rules` and is tested there against the very rule the
 * discovery blinder enforces. This stands in for it with the same shape so
 * the orchestration below - which namespace is asked, and what "not found"
 * means versus "could not ask" - can be tested without wasm, which vitest
 * does not have here.
 */
const RULES = {
  bareName: (typed: string) => typed.trim().replace(/^@+/, "").trim(),
  looksLikeAName: (typed: string) =>
    /^[a-z0-9_]{3,32}$/.test(typed.trim().replace(/^@+/, "").trim().toLowerCase()),
} as unknown as UiRules;

describe("findByName", () => {
  it("finds a channel", async () => {
    const found = await findByName("@news", { ...NONE, channel: async () => "abc123" }, RULES);
    expect(found).toEqual({ kind: "channel", channelIdHex: "abc123", handle: "news" });
  });

  it("finds a person", async () => {
    const found = await findByName("anna", { ...NONE, person: async () => "anna" }, RULES);
    expect(found).toEqual({ kind: "person", username: "anna" });
  });

  it("asks BOTH namespaces, because the person does not know which one holds the name", async () => {
    const channel = vi.fn(async () => null);
    const person = vi.fn(async () => null);
    await findByName("anna", { channel, person }, RULES);
    expect(channel).toHaveBeenCalledWith("anna");
    expect(person).toHaveBeenCalledWith("anna");
  });

  it("stops at the channel when it answers, rather than spending a second lookup", async () => {
    const person = vi.fn(async () => null);
    await findByName("news", { channel: async () => "abc123", person }, RULES);
    expect(person).not.toHaveBeenCalled();
  });

  it("says nothing was found when neither namespace holds it", async () => {
    expect(await findByName("ghost", NONE, RULES)).toEqual({ kind: "nothing" });
  });

  it("spends no lookup at all on something that cannot be a name", async () => {
    const channel = vi.fn(async () => null);
    const person = vi.fn(async () => null);
    expect(await findByName("no", { channel, person }, RULES)).toEqual({ kind: "nothing" });
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
    expect(await findByName("anna", dead, RULES)).toEqual({ kind: "unreachable" });
  });

  it("still answers when only one namespace is reachable", async () => {
    const half: NameLookups = {
      channel: async () => {
        throw new Error("offline");
      },
      person: async () => null,
    };
    expect(await findByName("anna", half, RULES)).toEqual({ kind: "nothing" });
  });
});

describe("findByNameMessage", () => {
  it("names what was typed, with the at-sign people expect", () => {
    expect(findByNameMessage({ kind: "nothing" }, "anna", RULES)).toBe("Nothing found for @anna.");
  });

  it("blames the connection, not the name, when it could not ask", () => {
    expect(findByNameMessage({ kind: "unreachable" }, "anna", RULES)).toContain("connection");
  });

  it("says nothing when there is something to open instead", () => {
    expect(findByNameMessage({ kind: "person", username: "anna" }, "anna", RULES)).toBe("");
  });
});
