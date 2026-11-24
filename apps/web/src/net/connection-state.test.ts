// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import { afterEach, describe, expect, it } from "vitest";
import {
  UNREACHABLE_HEADER,
  connectionAnnouncement,
  connectionPhase,
  isNetworkFailure,
  nextConnectionPhase,
  noteConnectionEvent,
  observeRequest,
  onNetworkReturned,
  resetConnectionStateForTest,
} from "./connection-state";
import { connectionHeaderText } from "./connection-copy";
import { sleepCancellable } from "../ds/long-poll-loop";

afterEach(() => resetConnectionStateForTest());

const answer = (
  status: number,
  headers: Record<string, string> = {},
): Response => new Response(null, { status, headers });

describe("nextConnectionPhase (docs/ux/connection-state.md section 3)", () => {
  it("enters waiting on a network failure or an OS no-route, from any phase", () => {
    for (const from of ["online", "waiting", "connecting"] as const) {
      expect(nextConnectionPhase(from, "network-failure")).toBe("waiting");
      expect(nextConnectionPhase(from, "os-offline")).toBe("waiting");
    }
  });

  it("an answer clears every phase", () => {
    for (const from of ["online", "waiting", "connecting"] as const) {
      expect(nextConnectionPhase(from, "answered")).toBe("online");
    }
  });

  it("the OS route coming back moves waiting to connecting and nothing else", () => {
    expect(nextConnectionPhase("waiting", "os-online")).toBe("connecting");
    expect(nextConnectionPhase("online", "os-online")).toBe("online");
    expect(nextConnectionPhase("connecting", "os-online")).toBe("connecting");
  });
});

describe("isNetworkFailure", () => {
  it("is a fetch TypeError, not an abort or another error", () => {
    expect(isNetworkFailure(new TypeError("Failed to fetch"))).toBe(true);
    expect(isNetworkFailure(new DOMException("aborted", "AbortError"))).toBe(
      false,
    );
    expect(isNetworkFailure(new Error("decode"))).toBe(false);
    const controller = new AbortController();
    controller.abort();
    expect(
      isNetworkFailure(new TypeError("Failed to fetch"), controller.signal),
    ).toBe(false);
  });
});

describe("observeRequest", () => {
  it("a network failure enters waiting; an HTTP error does not", async () => {
    await expect(
      observeRequest(Promise.reject(new TypeError("Failed to fetch"))),
    ).rejects.toThrow();
    expect(connectionPhase()).toBe("waiting");
    await observeRequest(Promise.resolve(answer(503)));
    expect(connectionPhase()).toBe("online");
    await observeRequest(Promise.resolve(answer(401)));
    expect(connectionPhase()).toBe("online");
  });

  it("an aborted request changes nothing", async () => {
    const controller = new AbortController();
    controller.abort();
    await expect(
      observeRequest(
        Promise.reject(new DOMException("aborted", "AbortError")),
        controller.signal,
      ),
    ).rejects.toThrow();
    expect(connectionPhase()).toBe("online");
  });

  it("the desktop host's own unreachable answer counts as a network failure", async () => {
    const r = await observeRequest(
      Promise.resolve(answer(502, { [UNREACHABLE_HEADER]: "1" })),
    );
    expect(r.status).toBe(502);
    expect(connectionPhase()).toBe("waiting");
  });

  it("stays connecting until the first answer, which clears it", async () => {
    noteConnectionEvent("os-offline");
    noteConnectionEvent("os-online");
    expect(connectionPhase()).toBe("connecting");
    await observeRequest(Promise.resolve(answer(200)));
    expect(connectionPhase()).toBe("online");
  });

  it("a failed retry while connecting goes back to waiting", async () => {
    noteConnectionEvent("network-failure");
    noteConnectionEvent("os-online");
    await expect(
      observeRequest(Promise.reject(new TypeError("x"))),
    ).rejects.toThrow();
    expect(connectionPhase()).toBe("waiting");
  });
});

describe("announcements and wake-ups", () => {
  it("announces each transition once and nothing while the phase stays", () => {
    expect(connectionAnnouncement()).toBeNull();
    noteConnectionEvent("network-failure");
    expect(connectionAnnouncement()).toBe("waiting");
    noteConnectionEvent("network-failure");
    expect(connectionAnnouncement()).toBe("waiting");
    noteConnectionEvent("answered");
    expect(connectionAnnouncement()).toBe("online");
  });

  it("the network coming back wakes a sleeping receive loop", async () => {
    let woken = 0;
    onNetworkReturned(() => (woken += 1));
    noteConnectionEvent("network-failure");
    const started = Date.now();
    const sleeping = sleepCancellable(60_000, new AbortController().signal);
    noteConnectionEvent("os-online");
    await sleeping;
    expect(Date.now() - started).toBeLessThan(5_000);
    expect(woken).toBe(1);
  });
});

describe("connectionHeaderText (the title / subtitle swap)", () => {
  it("is null online and the catalog words otherwise", () => {
    expect(connectionHeaderText("online")).toBeNull();
    expect(connectionHeaderText("waiting")).toBe("Waiting for network...");
    expect(connectionHeaderText("connecting")).toBe("Connecting...");
  });
});
