// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import { describe, expect, it } from "vitest";

import { channelErrorCode, channelErrorPlace } from "./channel-error-copy";

describe("channel error codes", () => {
  it("reads the code of a channel refusal and nothing else", () => {
    expect(channelErrorCode(Object.assign(new Error("x"), { code: "handle-held" }))).toBe("handle-held");
    expect(channelErrorCode(Object.assign(new Error("x"), { code: "something-else" }))).toBeNull();
    expect(channelErrorCode(new Error("x"))).toBeNull();
    expect(channelErrorCode(null)).toBeNull();
  });

  it("puts a held name under the field, a photo failure under the photo, the rest on the form", () => {
    expect(channelErrorPlace("handle-held")).toBe("handle");
    expect(channelErrorPlace("handle-malformed")).toBe("handle");
    expect(channelErrorPlace("photo-upload")).toBe("photo");
    expect(channelErrorPlace("photo-limit")).toBe("photo");
    expect(channelErrorPlace("offline")).toBe("form");
    expect(channelErrorPlace(null)).toBe("form");
  });
});
