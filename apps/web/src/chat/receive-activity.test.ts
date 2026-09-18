// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import { describe, expect, it } from "vitest";

import { backgroundReceiveIsActive, mayParkReceive } from "./receive-activity";

describe("mayParkReceive", () => {
  it("never parks a visible tab", () => {
    for (const pushWake of ["reachable", "unreachable", "unknown"] as const) {
      expect(
        mayParkReceive({ documentHidden: false, callInProgress: false, pushWake }),
      ).toBe(false);
    }
  });

  it("parks a hidden tab a push wake can reach", () => {
    expect(
      mayParkReceive({ documentHidden: true, callInProgress: false, pushWake: "reachable" }),
    ).toBe(true);
  });

  it("keeps receiving in a hidden tab push cannot reach", () => {
    // THE defect. With notifications denied there is no notification path, so
    // the in-app incoming call screen is the only one a call has
    // (`docs/ux/calls.md` §2.1) — and it needs the offer row to have been
    // fetched.
    expect(
      mayParkReceive({ documentHidden: true, callInProgress: false, pushWake: "unreachable" }),
    ).toBe(false);
  });

  it("keeps receiving while the push path is not established", () => {
    // Failing toward staying awake: the cost is battery, the cost of the
    // other direction is a call that can never arrive.
    expect(
      mayParkReceive({ documentHidden: true, callInProgress: false, pushWake: "unknown" }),
    ).toBe(false);
  });

  it("never parks during a call, whatever push could do", () => {
    expect(
      mayParkReceive({ documentHidden: true, callInProgress: true, pushWake: "reachable" }),
    ).toBe(false);
  });

  it("never parks while a wake window is open", () => {
    expect(
      mayParkReceive({
        documentHidden: true,
        callInProgress: false,
        pushWake: "reachable",
        wakeWindowOpen: true,
      }),
    ).toBe(false);
  });
});

describe("backgroundReceiveIsActive", () => {
  it("is inactive without a session, whatever else is true", () => {
    expect(
      backgroundReceiveIsActive({
        signedIn: false,
        documentHidden: false,
        callInProgress: true,
        pushWake: "unreachable",
      }),
    ).toBe(false);
  });

  it("sweeps a hidden tab with notifications denied", () => {
    expect(
      backgroundReceiveIsActive({
        signedIn: true,
        documentHidden: true,
        callInProgress: false,
        pushWake: "unreachable",
      }),
    ).toBe(true);
  });

  it("still parks a hidden tab push covers, so the wake keeps its purpose", () => {
    expect(
      backgroundReceiveIsActive({
        signedIn: true,
        documentHidden: true,
        callInProgress: false,
        pushWake: "reachable",
      }),
    ).toBe(false);
  });
});
