// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * ADR-0080 delivery (live run 2026-10-28: a card that never arrived): a
 * failed first publish is retried on the next DeviceGroup poll pass and
 * survives a reload; a growth re-send that falls due while the page is
 * hidden is sent at the next foreground, not from the timer.
 */
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import {
  registerServiceEnvelopeSender,
  resetServiceEnvelopeOutbox,
  retryDirtyServiceEnvelopes,
  setServiceEnvelopeComposer,
} from "../reach/service-envelope-outbox";
import { MemoryStorage } from "../test/fake-storage";
import { cardsStorageKey } from "./device-cards";
import {
  bindDeviceCards,
  noteDeviceAction,
  noteDeviceForeground,
  ownDeviceCard,
  scheduleOwnCardResend,
  setShowCityToOwnDevices,
  unbindDeviceCards,
} from "./device-card-runtime";

const ACCOUNT = "YWNjb3VudC0wMDAwMDAwMDA=";
const ENVELOPE = new Uint8Array([1, 2, 3]);

let unregister: (() => void) | null = null;
let doc: { visibilityState: string };

function row(): { ownUnsent?: boolean; resendOwed?: boolean } {
  return JSON.parse(localStorage.getItem(cardsStorageKey(ACCOUNT)) ?? "{}") as {
    ownUnsent?: boolean;
    resendOwed?: boolean;
  };
}

/** Bind as the sync runtime does, with a composer that needs no wasm. */
function bind(): void {
  bindDeviceCards(ACCOUNT);
  setServiceEnvelopeComposer("deviceCard", async () => (ownDeviceCard() === null ? null : ENVELOPE));
}

beforeEach(() => {
  const g = globalThis as unknown as Record<string, unknown>;
  g["localStorage"] = new MemoryStorage();
  doc = { visibilityState: "visible" };
  g["document"] = doc;
  // No city: the locate call fails, the card goes without one.
  g["fetch"] = vi.fn(async () => {
    throw new Error("offline");
  });
  vi.spyOn(console, "info").mockImplementation(() => undefined);
  vi.spyOn(console, "warn").mockImplementation(() => undefined);
});

afterEach(() => {
  unregister?.();
  unregister = null;
  unbindDeviceCards();
  resetServiceEnvelopeOutbox();
  vi.useRealTimers();
  vi.restoreAllMocks();
  const g = globalThis as unknown as Record<string, unknown>;
  delete g["localStorage"];
  delete g["document"];
  delete g["fetch"];
});

describe("a failed first publish of the own card", () => {
  it("is retried on the next poll pass and survives a reload", async () => {
    bind();
    const sent: Uint8Array[] = [];
    let fail = true;
    unregister = registerServiceEnvelopeSender(async (envelope) => {
      if (fail) throw new Error("not ready");
      sent.push(envelope);
    });

    setShowCityToOwnDevices(false);
    noteDeviceAction(1_000_000);
    await vi.waitFor(() => expect(ownDeviceCard()).not.toBeNull());
    await retryDirtyServiceEnvelopes();
    expect(sent).toHaveLength(0);
    expect(row().ownUnsent).toBe(true);

    // A reload: the outbox forgets the mark, the stored flag brings it back.
    unbindDeviceCards();
    resetServiceEnvelopeOutbox();
    bind();

    // The next poll pass, with the DeviceGroup reachable again.
    fail = false;
    await retryDirtyServiceEnvelopes();
    expect(sent).toHaveLength(1);
    expect(row().ownUnsent).toBeUndefined();

    // Nothing more goes out on later passes.
    await retryDirtyServiceEnvelopes();
    expect(sent).toHaveLength(1);
  });
});

describe("a growth re-send while the page is hidden", () => {
  it("is kept owed and sent at the next foreground", async () => {
    bind();
    const sent: Uint8Array[] = [];
    unregister = registerServiceEnvelopeSender(async (envelope) => {
      sent.push(envelope);
    });
    setShowCityToOwnDevices(false);
    noteDeviceAction(1_000_000);
    await vi.waitFor(() => expect(sent).toHaveLength(1));

    vi.useFakeTimers();
    doc.visibilityState = "hidden";
    scheduleOwnCardResend(() => 0.5);
    await vi.advanceTimersByTimeAsync(30_000);
    await retryDirtyServiceEnvelopes();
    expect(sent).toHaveLength(1);
    expect(row().resendOwed).toBe(true);

    // Still hidden: a foreground notice that is not one sends nothing.
    noteDeviceForeground();
    await retryDirtyServiceEnvelopes();
    expect(sent).toHaveLength(1);

    doc.visibilityState = "visible";
    noteDeviceForeground();
    await retryDirtyServiceEnvelopes();
    expect(sent).toHaveLength(2);
    expect(row().resendOwed).toBeUndefined();
  });

  it("is sent from the timer when the page is visible", async () => {
    bind();
    const sent: Uint8Array[] = [];
    unregister = registerServiceEnvelopeSender(async (envelope) => {
      sent.push(envelope);
    });
    setShowCityToOwnDevices(false);
    noteDeviceAction(1_000_000);
    await vi.waitFor(() => expect(sent).toHaveLength(1));

    vi.useFakeTimers();
    scheduleOwnCardResend(() => 0.1);
    await vi.advanceTimersByTimeAsync(30_000);
    await retryDirtyServiceEnvelopes();
    expect(sent).toHaveLength(2);
  });
});
