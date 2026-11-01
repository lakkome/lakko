// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import { describe, expect, it } from "vitest";

import { CHAT_STARTING_WINDOW_MS, messageAction } from "./message-action";

const NOW = 1_000_000;

describe("messageAction (D32)", () => {
  it("is the chat itself once there is one", () => {
    expect(messageAction({ hasChat: true, canStartChat: false, acceptedAtMs: NOW, nowMs: NOW })).toBe("go");
  });

  it("in the seconds after an accept the chat is starting, known account or not", () => {
    for (const canStartChat of [true, false]) {
      expect(messageAction({ hasChat: false, canStartChat, acceptedAtMs: NOW - 2_000, nowMs: NOW })).toBe("starting");
    }
  });

  it("afterwards a known account can start one and an unknown one cannot", () => {
    const old = NOW - CHAT_STARTING_WINDOW_MS;
    expect(messageAction({ hasChat: false, canStartChat: true, acceptedAtMs: old, nowMs: NOW })).toBe("go");
    expect(messageAction({ hasChat: false, canStartChat: false, acceptedAtMs: old, nowMs: NOW })).toBe("unavailable");
  });

  it("a clock that went backwards is not a fresh accept", () => {
    expect(messageAction({ hasChat: false, canStartChat: false, acceptedAtMs: NOW + 1, nowMs: NOW })).toBe("unavailable");
  });

  it("with no accepted time the old rule stands", () => {
    expect(messageAction({ hasChat: false, canStartChat: true, acceptedAtMs: null, nowMs: NOW })).toBe("go");
  });
});
