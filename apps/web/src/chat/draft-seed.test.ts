// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import { describe, expect, it } from "vitest";

import { createDraftSeed } from "./draft-seed";

describe("createDraftSeed", () => {
  it("fills an empty composer once", () => {
    const seed = createDraftSeed();
    expect(seed.offer("from the phone", "")).toBe("from the phone");
    expect(seed.offer("newer", "")).toBeNull();
  });

  it("leaves text the person is typing", () => {
    expect(createDraftSeed().offer("from the phone", "mine")).toBeNull();
  });

  it("does not bring back a sent message when its own draft arrives late", () => {
    const seed = createDraftSeed();
    seed.touched();
    expect(seed.offer("just sent", "")).toBeNull();
  });
});
