// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import { describe, expect, it } from "vitest";

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

  it("a sent request waits for 7 days with no button, after a restart too", () => {
    expect(publisherCardControl({ role: null, busy: false, pending: { requestedAtMs: NOW }, nowMs: NOW })).toBe("waiting");
    expect(
      publisherCardControl({ role: "subscriber", busy: false, pending: { requestedAtMs: NOW - 6 * DAY }, nowMs: NOW }),
    ).toBe("waiting");
  });

  it("after 7 days it reads No answer yet with Ask again, never a refusal", () => {
    expect(
      publisherCardControl({
        role: null,
        busy: false,
        pending: { requestedAtMs: NOW - PUBLISHER_ASK_AGAIN_AFTER_MS },
        nowMs: NOW,
      }),
    ).toBe("no-answer");
    expect(
      publisherCardControl({ role: null, busy: false, pending: { requestedAtMs: NOW - 30 * DAY }, nowMs: NOW }),
    ).toBe("no-answer");
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
