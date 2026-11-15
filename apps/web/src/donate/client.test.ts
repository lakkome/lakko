// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import { describe, expect, it } from "vitest";
import { parseDonateMethods } from "./client";

describe("parseDonateMethods", () => {
  it("keeps the methods it can draw, in order", () => {
    const methods = parseDonateMethods({
      methods: [
        { id: "sbp", kind: "sbp", has_qr: true, has_phone: true },
        { id: "bmc", kind: "bmc", link: "https://buymeacoffee.com/x", has_qr: false, has_phone: false },
        { id: "usdt", kind: "crypto", coin: "USDT", network: "TRON", address: "Tabc", has_qr: false, has_phone: false },
      ],
    });
    expect(methods.map((m) => m.id)).toEqual(["sbp", "bmc", "usdt"]);
    expect(methods[2]?.crypto).toEqual({ coin: "USDT", network: "TRON", address: "Tabc" });
  });

  it("drops what it cannot draw safely", () => {
    const methods = parseDonateMethods({
      methods: [
        { id: "x", kind: "paypal", link: "https://x" },
        { id: "y", kind: "bmc", link: "javascript:alert(1)" },
        { id: "z", kind: "crypto", coin: "BTC" },
        { id: "Bad Id", kind: "bmc", link: "https://x" },
        null,
      ],
    });
    expect(methods).toEqual([]);
  });

  it("reads a malformed answer as no methods", () => {
    expect(parseDonateMethods(null)).toEqual([]);
    expect(parseDonateMethods({ methods: "no" })).toEqual([]);
  });
});
