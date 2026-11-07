// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import { describe, expect, it } from "vitest";

import { isOwnLinePhantom, isOwnSendLine } from "./own-line-track";

describe("isOwnSendLine", () => {
  it("names the lines this device sends on", () => {
    expect(isOwnSendLine("sendrecv")).toBe(true);
    expect(isOwnSendLine("sendonly")).toBe(true);
  });

  it("leaves the lines the SFU offered, and anything unknown, to be a peer's", () => {
    expect(isOwnSendLine("recvonly")).toBe(false);
    expect(isOwnSendLine("inactive")).toBe(false);
    expect(isOwnSendLine("stopped")).toBe(false);
    expect(isOwnSendLine(undefined)).toBe(false);
    expect(isOwnSendLine(null)).toBe(false);
  });
});

describe("isOwnLinePhantom", () => {
  const peer = "8bc80876883bbab133db04f3f99f9662";
  it("sets aside an own send line whose stream is the SFU's random msid", () => {
    expect(isOwnLinePhantom("sendrecv", "0b084ac3-cb3c-4f78-b0bb-008db53310e1")).toBe(true);
  });
  it("keeps a peer's stream on any line", () => {
    expect(isOwnLinePhantom("sendrecv", peer)).toBe(false);
    expect(isOwnLinePhantom("recvonly", peer)).toBe(false);
  });
  it("keeps whatever arrives on a line the SFU offered", () => {
    expect(isOwnLinePhantom("recvonly", "0b084ac3-cb3c-4f78-b0bb-008db53310e1")).toBe(false);
  });
});
