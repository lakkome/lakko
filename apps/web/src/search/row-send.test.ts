// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import { describe, expect, it } from "vitest";

import { rowStateAfter, rowView } from "./row-send";
import { REQUEST_FAILED, REQUEST_REFUSED } from "./search-state";

describe("a found person's row after Add contact (UX 6.1)", () => {
  it("offers Add contact before anything is sent", () => {
    expect(rowView("idle", true)).toEqual({ add: "shown", sent: false, message: null });
  });

  it("keeps the button, disabled, while the request goes out", () => {
    expect(rowView("sending", true)).toEqual({ add: "disabled", sent: false, message: null });
  });

  it("reads Request sent and the button goes away", () => {
    expect(rowView(rowStateAfter({ kind: "sent" }), true)).toEqual({ add: "hidden", sent: true, message: null });
  });

  it("on a refusal says they may have changed who can find them, with no button", () => {
    const v = rowView(rowStateAfter({ kind: "refused" }), true);
    expect(v.add).toBe("hidden");
    expect(v.sent).toBe(false);
    expect(v.message?.error).toBe(true);
    expect(v.message?.text()).toBe(REQUEST_REFUSED());
  });

  it("on any other failure says so and gives the button back", () => {
    const v = rowView(rowStateAfter({ kind: "failed", message: () => "offline" }), true);
    expect(v.add).toBe("shown");
    expect(v.message?.text()).toBe(REQUEST_FAILED());
  });

  it("offers nothing on a row that cannot be added (a contact, a hidden person)", () => {
    for (const s of ["idle", "sending", "sent", "refused", "failed"] as const) {
      expect(rowView(s, false)).toEqual({ add: "hidden", sent: false, message: null });
    }
  });
});
