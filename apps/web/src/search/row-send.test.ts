// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import { describe, expect, it } from "vitest";

import type { PersonResult } from "./people";
import {
  heldStateAfter,
  isOutgoing,
  outgoingOf,
  rowSendState,
  rowView,
  withoutRowLines,
  type RowSendState,
} from "./row-send";
import { REQUEST_FAILED, REQUEST_REFUSED } from "./search-state";

const person = (over: Partial<PersonResult> = {}): PersonResult => ({
  keyB64: "KEY",
  keyHex: "aa",
  accountIdHex: "11",
  name: "Anna",
  username: null,
  reach: "grant",
  grantWireB64: "G",
  ...over,
});
const record = (recipientAccountIdHex: string, recipientKeyB64?: string) => ({
  recipientUsername: "",
  recipientAccountIdHex,
  sentAtMs: 1,
  ...(recipientKeyB64 === undefined ? {} : { recipientKeyB64 }),
});

describe("a found person's row after Add contact (UX 6.1)", () => {
  it("offers Add contact before anything is sent", () => {
    expect(rowView("idle", true)).toEqual({ add: "shown", sent: false, message: null });
  });

  it("keeps the button, disabled, while the request goes out", () => {
    expect(rowView("sending", true)).toEqual({ add: "disabled", sent: false, message: null });
  });

  it("reads Request sent and the button goes away", () => {
    expect(rowView("sent", true)).toEqual({ add: "hidden", sent: true, message: null });
  });

  it("on a refusal says they may have changed who can find them, with no button", () => {
    const v = rowView(heldStateAfter({ kind: "refused" }), true);
    expect(v.add).toBe("hidden");
    expect(v.sent).toBe(false);
    expect(v.message?.error).toBe(true);
    expect(v.message?.text()).toBe(REQUEST_REFUSED());
  });

  it("on any other failure says so and gives the button back", () => {
    const v = rowView(heldStateAfter({ kind: "failed", message: () => "offline" }), true);
    expect(v.add).toBe("shown");
    expect(v.message?.text()).toBe(REQUEST_FAILED());
  });

  it("offers nothing on a row that cannot be added (a contact, a hidden person)", () => {
    for (const s of ["idle", "sending", "failed"] as const) {
      expect(rowView(s, false)).toEqual({ add: "hidden", sent: false, message: null });
    }
  });

  it("reads Request sent even when the last check says they cannot be reached", () => {
    expect(rowView("sent", false)).toEqual({ add: "hidden", sent: true, message: null });
  });

  it("keeps the refusal sentence after the refusal took the offer away", () => {
    const v = rowView("refused", false);
    expect(v.add).toBe("hidden");
    expect(v.message?.text()).toBe(REQUEST_REFUSED());
  });
});

describe("Request sent comes from the Sent list (UX 6.1)", () => {
  it("a send keeps nothing: the list says it went", () => {
    expect(heldStateAfter({ kind: "sent" })).toBe("idle");
  });

  it("reads sent while the list holds the person, by key or by account id", () => {
    expect(rowSendState(person(), "idle", outgoingOf([record("11")]))).toBe("sent");
    expect(rowSendState(person({ accountIdHex: null }), "idle", outgoingOf([record("22", "KEY")]))).toBe("sent");
  });

  it("a rotated key does not lose the request: the account id still names them", () => {
    expect(isOutgoing(person({ keyB64: "NEW" }), outgoingOf([record("11", "OLD")]))).toBe(true);
  });

  it("declined and dismissed, or accepted, the row goes back to what the check says", () => {
    const empty = outgoingOf([]);
    expect(rowSendState(person(), "sent", empty)).toBe("idle");
    expect(rowSendState(person(), "idle", empty)).toBe("idle");
    expect(rowView(rowSendState(person(), "sent", empty), true).add).toBe("shown");
  });

  it("a held state stands for anyone the list does not hold", () => {
    expect(rowSendState(person(), "refused", outgoingOf([record("99")]))).toBe("refused");
  });
});

describe("a row's line lasts until the text changes (UX 6.1)", () => {
  it("clears refused and failed, and a request on its way stays claimed", () => {
    const held = new Map<string, RowSendState>([
      ["a", "refused"],
      ["b", "failed"],
      ["c", "sending"],
    ]);
    expect([...withoutRowLines(held)]).toEqual([["c", "sending"]]);
  });

  it("hands back the same map when nothing changes", () => {
    const held = new Map<string, RowSendState>([["c", "sending"]]);
    expect(withoutRowLines(held)).toBe(held);
  });
});
