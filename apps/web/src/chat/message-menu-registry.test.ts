// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import { describe, expect, it, vi } from "vitest";

import { openMessageMenu, registerMessageMenu } from "./message-menu-registry";

describe("opening a message menu from outside its bubble (UX attachments 11.3)", () => {
  it("calls the opener of exactly that conversation and message", () => {
    const a = vi.fn();
    const b = vi.fn();
    const offA = registerMessageMenu("g1", 5, a);
    const offB = registerMessageMenu("g2", 5, b);
    expect(openMessageMenu("g1", 5)).toBe(true);
    expect(a).toHaveBeenCalledTimes(1);
    expect(b).not.toHaveBeenCalled();
    offA();
    offB();
  });

  it("reports a message that is not on screen", () => {
    expect(openMessageMenu("g1", 99)).toBe(false);
  });

  it("an old unregister does not remove a newer registration", () => {
    const first = vi.fn();
    const second = vi.fn();
    const offFirst = registerMessageMenu("g", 1, first);
    const offSecond = registerMessageMenu("g", 1, second);
    offFirst();
    expect(openMessageMenu("g", 1)).toBe(true);
    expect(second).toHaveBeenCalledTimes(1);
    offSecond();
    expect(openMessageMenu("g", 1)).toBe(false);
  });
});
