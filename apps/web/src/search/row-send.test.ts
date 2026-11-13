// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import { describe, expect, it } from "vitest";

import type { PersonResult } from "./people";
import {
  heldStateAfter,
  isOutgoing,
  mayAskAgain,
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
    expect(rowView("idle", true)).toEqual({ add: "shown", sent: false, askAgain: false, message: null });
  });

  it("keeps the button, disabled, while the request goes out", () => {
    expect(rowView("sending", true)).toEqual({ add: "disabled", sent: false, askAgain: false, message: null });
  });

  it("reads Request sent and the button goes away", () => {
    expect(rowView("sent", true)).toEqual({ add: "hidden", sent: true, askAgain: false, message: null });
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
      expect(rowView(s, false)).toEqual({ add: "hidden", sent: false, askAgain: false, message: null });
    }
  });

  it("reads Request sent even when the last check says they cannot be reached", () => {
    expect(rowView("sent", false)).toEqual({ add: "hidden", sent: true, askAgain: false, message: null });
  });

  it("keeps the refusal sentence after the refusal took the offer away", () => {
    const v = rowView("refused", false);
    expect(v.add).toBe("hidden");
    expect(v.message?.text()).toBe(REQUEST_REFUSED());
  });
});

describe("asking again (D2)", () => {
  const DAY = 24 * 60 * 60 * 1000;
  const sentAt = (recipientAccountIdHex: string, sentAtMs: number) => ({ ...record(recipientAccountIdHex), sentAtMs });

  it("right after Add contact the row reads Request sent and the button goes away (UX 6.1)", () => {
    const now = 10 * DAY;
    const outgoing = outgoingOf([sentAt("11", now)]);
    const v = rowView(rowSendState(person(), "idle", outgoing), true, mayAskAgain(person(), outgoing, now));
    expect(v.sent).toBe(true);
    expect(v.askAgain).toBe(false);
    expect(mayAskAgain(person(), outgoing, now + DAY - 1)).toBe(false);
  });

  it("a request unanswered for a day reads Request sent and offers Ask again", () => {
    const now = 10 * DAY;
    const outgoing = outgoingOf([sentAt("11", now - DAY)]);
    const v = rowView(rowSendState(person(), "idle", outgoing), true, mayAskAgain(person(), outgoing, now));
    expect(v.sent).toBe(true);
    expect(v.askAgain).toBe(true);
  });

  it("the newest request to the person decides", () => {
    const now = 10 * DAY;
    const outgoing = outgoingOf([sentAt("11", now - 2 * DAY), sentAt("11", now - 1000)]);
    expect(mayAskAgain(person(), outgoing, now)).toBe(false);
  });

  it("needs a way to reach the person", () => {
    expect(rowView("sent", false, true).askAgain).toBe(false);
  });

  it("is not offered for a person added through their link", () => {
    const outgoing = outgoingOf([record("33")], [{ accountIdHex: "33", signaturePubkeyB64: "KEY" }]);
    expect(mayAskAgain(person({ accountIdHex: "33" }), outgoing, 10 * DAY)).toBe(false);
  });

  it("the repeat on its way, and one that failed, speak over the Sent list", () => {
    const outgoing = outgoingOf([record("11")]);
    expect(rowSendState(person(), "sending", outgoing)).toBe("sending");
    expect(rowSendState(person(), "failed", outgoing)).toBe("failed");
    expect(rowView("sending", true, true).add).toBe("disabled");
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

  it("a person added through their link reads Request sent, never Add contact", () => {
    const invited = [{ accountIdHex: "33", signaturePubkeyB64: "KEY" }];
    expect(rowSendState(person({ accountIdHex: null }), "idle", outgoingOf([], invited))).toBe("sent");
    expect(isOutgoing(person({ keyB64: "NEW", accountIdHex: "33" }), outgoingOf([], invited))).toBe(true);
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
