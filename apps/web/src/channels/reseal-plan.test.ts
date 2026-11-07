// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import { afterEach, describe, expect, it } from "vitest";

import { resealPlan } from "./rotation-cron";
import { channelsActions } from "../state/channels-state";

const ID = "0123456789abcdef0123456789abcdef";
const KEY_B64 = btoa(String.fromCharCode(...new Uint8Array(32).fill(7)));

afterEach(() => {
  channelsActions.removeChannel(ID);
});

describe("resealPlan (ADR-0087 sections 6 and 7)", () => {
  it("a private channel reseals per key to the rows it let in, and files no public seed", () => {
    channelsActions.updateAccess(ID, (r) => ({
      ...r,
      visibility: "private",
      admittedThroughSeq: 42,
      admissionSeeds: [{ version: 1, seedB64: KEY_B64 }],
    }));
    expect(resealPlan(ID)).toEqual({ readKey: null, perKeyThroughSeq: 42 });
  });

  it("a public channel files the public seed and reseals per key only to its private-era rows", () => {
    channelsActions.updateAccess(ID, (r) => ({ ...r, visibility: "public", readKeyB64: KEY_B64, admittedThroughSeq: 5 }));
    const plan = resealPlan(ID);
    expect(plan.readKey?.length).toBe(32);
    expect(plan.perKeyThroughSeq).toBe(5);
  });

  it("a device that checks no tickets (a channel from before phase B) reseals to every row", () => {
    channelsActions.updateAccess(ID, (r) => ({ ...r, visibility: null, admittedThroughSeq: 3 }));
    expect(resealPlan(ID).perKeyThroughSeq).toBe("all");
  });

  it("a public channel whose read key this device lacks reseals to every row", () => {
    channelsActions.updateAccess(ID, (r) => ({ ...r, visibility: "public", readKeyB64: null }));
    expect(resealPlan(ID)).toEqual({ readKey: null, perKeyThroughSeq: "all" });
  });
});
