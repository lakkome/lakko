// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import { describe, expect, it } from "vitest";

import { BindingEpoch } from "./binding-epoch";

describe("BindingEpoch", () => {
  it("answers true until the binding ends", () => {
    const binding = new BindingEpoch();
    const current = binding.capture();
    expect(current()).toBe(true);
    binding.advance();
    expect(current()).toBe(false);
  });

  it("does not revive a stale capture when the same account binds again", () => {
    const binding = new BindingEpoch();
    const first = binding.capture();
    binding.advance();
    const second = binding.capture();
    expect(first()).toBe(false);
    expect(second()).toBe(true);
  });

  it("keeps work for account A out of account B across an await", async () => {
    const binding = new BindingEpoch();
    const state: { owner: string | null; rows: string[] } = { owner: "A", rows: [] };
    let release: (rows: string[]) => void = () => undefined;
    const fetched = new Promise<string[]>((resolve) => {
      release = resolve;
    });
    async function load(): Promise<void> {
      const current = binding.capture();
      const rows = await fetched;
      if (!current()) return;
      state.rows = rows;
    }
    const pending = load();
    binding.advance();
    state.owner = "B";
    release(["draft of A"]);
    await pending;
    expect(state).toEqual({ owner: "B", rows: [] });
  });
});
