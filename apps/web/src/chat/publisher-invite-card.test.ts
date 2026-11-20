// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import { describe, expect, it } from "vitest";

import { ASK_AGAIN_AFTER_MS } from "./ask-again";
import { askAgainFailure, PUBLISHER_ASK_AGAIN_AFTER_MS, publisherCardControl } from "./publisher-invite-card";

const NOW = 1_800_000_000_000;
const DAY = 24 * 60 * 60 * 1000;

describe("publisherCardControl (UX 15.2, 15.2.2)", () => {
  it("offers Accept to a person who is not a publisher yet, subscribed or not", () => {
    expect(publisherCardControl({ role: null, busy: false, pending: null, nowMs: NOW })).toBe("accept");
    expect(publisherCardControl({ role: "subscriber", busy: false, pending: null, nowMs: NOW })).toBe("accept");
  });

  it("shows the waiting line on the disabled button while sending or asking again", () => {
    expect(publisherCardControl({ role: null, busy: true, pending: null, nowMs: NOW })).toBe("busy");
    expect(
      publisherCardControl({ role: null, busy: true, pending: { requestedAtMs: NOW - 8 * DAY }, nowMs: NOW }),
    ).toBe("busy");
  });

  it("a sent request waits for a day with no button, after a restart too", () => {
    expect(publisherCardControl({ role: null, busy: false, pending: { requestedAtMs: NOW }, nowMs: NOW })).toBe("waiting");
    expect(
      publisherCardControl({
        role: "subscriber",
        busy: false,
        pending: { requestedAtMs: NOW - DAY + 1 },
        nowMs: NOW,
      }),
    ).toBe("waiting");
  });

  it("after a day it reads No answer yet with Ask again, never a refusal", () => {
    expect(PUBLISHER_ASK_AGAIN_AFTER_MS).toBe(DAY);
    expect(
      publisherCardControl({
        role: null,
        busy: false,
        pending: { requestedAtMs: NOW - PUBLISHER_ASK_AGAIN_AFTER_MS },
        nowMs: NOW,
      }),
    ).toBe("no-answer");
    expect(
      publisherCardControl({ role: null, busy: false, pending: { requestedAtMs: NOW - 2 * DAY }, nowMs: NOW }),
    ).toBe("no-answer");
  });

  it("uses the same rule as a group request, not a copy", () => {
    expect(PUBLISHER_ASK_AGAIN_AFTER_MS).toBe(ASK_AGAIN_AFTER_MS);
  });

  it("a link Ask again found dead keeps the invalid-link line", () => {
    expect(
      publisherCardControl({
        role: null,
        busy: false,
        pending: { requestedAtMs: NOW - 30 * DAY, linkDead: true },
        nowMs: NOW,
      }),
    ).toBe("dead");
  });

  it("opens the channel once this account posts there, whatever the request", () => {
    expect(
      publisherCardControl({ role: "publisher", busy: false, pending: { requestedAtMs: NOW, linkDead: true }, nowMs: NOW }),
    ).toBe("open");
    expect(publisherCardControl({ role: "owner", busy: true, pending: null, nowMs: NOW })).toBe("open");
  });
});

describe("askAgainFailure", () => {
  it("a revoked, expired or damaged link is dead; no network keeps Ask again", () => {
    expect(askAgainFailure("link-invalid")).toEqual({ kind: "dead" });
    expect(askAgainFailure("link-damaged")).toEqual({ kind: "dead" });
    expect(askAgainFailure("offline")).toEqual({ kind: "offline" });
    expect(askAgainFailure("failed")).toEqual({ kind: "failed" });
    expect(askAgainFailure(null)).toEqual({ kind: "failed" });
  });
});
