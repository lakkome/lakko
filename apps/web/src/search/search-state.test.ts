// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import { describe, expect, it } from "vitest";

import {
  AnonTokenQuotaExhaustedError,
  AnonTokenUnavailableError,
  POOL_EMPTY_REASON,
} from "../tokens/anon-token-pool";
import {
  REQUEST_FAILED,
  REQUEST_REFUSED,
  REQUEST_SENT,
  SEARCH_FAILED,
  SEARCH_NOT_FOUND,
  SEARCH_OUT_OF_SEARCHES,
  foldSearch,
  legOutcomeOf,
  peopleLineOf,
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

  it("says when the searches ran out, and what a row reads after a request", () => {
    expect(SEARCH_OUT_OF_SEARCHES()).toBe("Too many searches for now. Try again later.");
    expect(REQUEST_FAILED()).toBe("Couldn't send the request. Try again.");
    expect(REQUEST_SENT()).toBe("Request sent");
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

  it("reads an empty token pool as out of searches, never as a network failure", () => {
    expect(legOutcomeOf(new AnonTokenUnavailableError("discovery-lookup", POOL_EMPTY_REASON))).toBe(
      "allowance-used",
    );
    // Any other missing token (no session, a refill that failed) is the network line.
    expect(legOutcomeOf(new AnonTokenUnavailableError("discovery-lookup", "refill timed out"))).toBe("failed");
  });
});

describe("searchStateMessage", () => {
  it("maps each state to its line", () => {
    expect(searchStateMessage({ kind: "not-found" })).toBe(SEARCH_NOT_FOUND());
    expect(searchStateMessage({ kind: "failed" })).toBe(SEARCH_FAILED());
    expect(searchStateMessage({ kind: "allowance-used" })).toBe(SEARCH_OUT_OF_SEARCHES());
  });

  it("says nothing in words while searching, when found, or idle", () => {
    expect(searchStateMessage({ kind: "searching" })).toBeNull();
    expect(searchStateMessage({ kind: "found" })).toBeNull();
    expect(searchStateMessage({ kind: "idle" })).toBeNull();
  });
});

describe("peopleLineOf", () => {
  it("says not found only when no person row stands under People", () => {
    expect(peopleLineOf({ kind: "not-found" }, 0)).toBe(SEARCH_NOT_FOUND());
    expect(peopleLineOf({ kind: "not-found" }, 1)).toBeNull();
  });

  it("still says the search failed over a row", () => {
    expect(peopleLineOf({ kind: "failed" }, 1)).toBe(SEARCH_FAILED());
    expect(peopleLineOf({ kind: "allowance-used" }, 1)).toBe(SEARCH_OUT_OF_SEARCHES());
  });
});
