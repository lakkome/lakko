// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import { describe, expect, it } from "vitest";

import { AnonTokenQuotaExhaustedError } from "../tokens/anon-token-pool";
import { LOOKUP_ALLOWANCE_USED } from "../tokens/token-copy";
import {
  REQUEST_REFUSED,
  SEARCH_FAILED,
  SEARCH_NOT_FOUND,
  foldSearch,
  legOutcomeOf,
  searchStateMessage,
} from "./search-state";

describe("the copy (docs/ux/who-can-find-me.md section 6.1, verbatim)", () => {
  it("says the same thing for a person who does not exist and one who is hidden", () => {
    expect(SEARCH_NOT_FOUND()).toBe("No one found. Check the spelling, or ask them for an invite link.");
  });

  it("blames the connection on a network failure", () => {
    expect(SEARCH_FAILED()).toBe("Couldn't search. Check your connection and try again.");
  });

  it("says a request was refused without saying why", () => {
    expect(REQUEST_REFUSED()).toBe("Couldn't send the request. They may have changed who can find them.");
  });
});

describe("foldSearch", () => {
  it("shows what was found, whatever else failed", () => {
    expect(foldSearch(2, ["failed", "answered"])).toEqual({ kind: "found" });
  });

  it("says not found only when every question was answered", () => {
    expect(foldSearch(0, ["answered", "answered"])).toEqual({ kind: "not-found" });
  });

  it("never says not found when a question got no answer", () => {
    expect(foldSearch(0, ["answered", "failed"])).toEqual({ kind: "failed" });
    expect(foldSearch(0, ["failed"])).toEqual({ kind: "failed" });
  });

  it("says the allowance is used when a question hit the daily quota", () => {
    expect(foldSearch(0, ["answered", "allowance-used", "failed"])).toEqual({ kind: "allowance-used" });
  });
});

describe("legOutcomeOf", () => {
  it("tells the daily quota from any other failure", () => {
    expect(legOutcomeOf(new AnonTokenQuotaExhaustedError("discovery-lookup", null))).toBe("allowance-used");
    expect(legOutcomeOf(new Error("offline"))).toBe("failed");
  });
});

describe("searchStateMessage", () => {
  it("maps each state to its line", () => {
    expect(searchStateMessage({ kind: "not-found" })).toBe(SEARCH_NOT_FOUND());
    expect(searchStateMessage({ kind: "failed" })).toBe(SEARCH_FAILED());
    expect(searchStateMessage({ kind: "allowance-used" })).toBe(LOOKUP_ALLOWANCE_USED());
  });

  it("says nothing in words while searching, when found, or idle", () => {
    expect(searchStateMessage({ kind: "searching" })).toBeNull();
    expect(searchStateMessage({ kind: "found" })).toBeNull();
    expect(searchStateMessage({ kind: "idle" })).toBeNull();
  });
});
