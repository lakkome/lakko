// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import { describe, expect, it } from "vitest";
import { platformFromEnv, publicOriginFor } from "./index";

describe("publicOriginFor", () => {
  it("is the page origin on the web", () => {
    expect(publicOriginFor("web", undefined, "https://test.lakko.me")).toBe("https://test.lakko.me");
    expect(publicOriginFor("web", "https://ignored.example", "https://lakko.me")).toBe("https://lakko.me");
  });

  it("is the host-provided origin on the desktop, never the app scheme", () => {
    expect(publicOriginFor("desktop", "https://test.lakko.me", "lakkoapp://localhost")).toBe(
      "https://test.lakko.me",
    );
  });

  it("takes the local stack the host also takes", () => {
    expect(publicOriginFor("desktop", "http://127.0.0.1:5173", "lakkoapp://localhost")).toBe(
      "http://127.0.0.1:5173",
    );
    expect(() => publicOriginFor("desktop", "http://127.0.0.2:5173", "lakkoapp://localhost")).toThrow();
  });

  it("refuses on the desktop without a usable host value", () => {
    for (const v of [undefined, "", "lakkoapp://localhost", "http://lakko.me", "https://lakko.me/", 7]) {
      expect(() => publicOriginFor("desktop", v, "lakkoapp://localhost")).toThrow();
    }
  });
});

describe("platformFromEnv", () => {
  it("is desktop only for the exact flag", () => {
    expect(platformFromEnv("desktop")).toBe("desktop");
    expect(platformFromEnv(undefined)).toBe("web");
    expect(platformFromEnv("")).toBe("web");
    expect(platformFromEnv("Desktop")).toBe("web");
  });
});
