// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import { describe, expect, it } from "vitest";

import {
  androidIntentUrl,
  androidPackageFromEnv,
  appPathOf,
  DEFAULT_ANDROID_PACKAGE,
  iosAppUrl,
  mayHaveDesktopApp,
  openInAppUrl,
  opensAppByItself,
  parseLakkoLink,
  phoneAppLinkOf,
  phoneAppPlatform,
} from "./lakko-link";

const ORIGIN = "https://lakko.me";
const ID = "AbCd_-12";
const SECRET = "0123456789abcdef";

describe("parseLakkoLink", () => {
  it("reads each kind, around stray spaces", () => {
    for (const kind of ["i", "c", "g"] as const) {
      expect(parseLakkoLink(` ${ORIGIN}/${kind}/${ID}#${SECRET}\n`, ORIGIN)).toEqual({ kind, id: ID, secret: SECRET });
    }
  });

  it("refuses what is not a whole Lakko link", () => {
    for (const text of [
      "hello",
      `https://evil.example/i/${ID}#${SECRET}`,
      `http://lakko.me/i/${ID}#${SECRET}`,
      `${ORIGIN}/x/${ID}#${SECRET}`,
      `${ORIGIN}/i/short#${SECRET}`,
      `${ORIGIN}/i/${ID}`,
      `${ORIGIN}/i/${ID}#short`,
      `${ORIGIN}/i/${ID}?a=1#${SECRET}`,
      `lakko://i/${ID}#${SECRET}`,
    ]) {
      expect(parseLakkoLink(text, ORIGIN), text).toBeNull();
    }
  });
});

describe("appPathOf", () => {
  it("is the route the web shows", () => {
    expect(appPathOf({ kind: "g", id: ID, secret: SECRET })).toBe(`/g/${ID}#${SECRET}`);
  });
});

describe("openInAppUrl", () => {
  it("carries the whole link", () => {
    expect(openInAppUrl({ kind: "c", id: ID, secret: SECRET })).toBe(`lakko://c/${ID}#${SECRET}`);
  });
});

describe("opensAppByItself", () => {
  const mac = "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/537.36 (KHTML, like Gecko)";

  it("tries in the browsers that stay quiet with no app", () => {
    expect(opensAppByItself(`${mac} Chrome/156.0.0.0 Safari/537.36`, true)).toBe(true);
    expect(opensAppByItself("Mozilla/5.0 (Macintosh; rv:159.0) Gecko/20100101 Firefox/159.0", true)).toBe(true);
  });

  it("leaves Safari to the button: it reports a missing app as an error", () => {
    expect(opensAppByItself(`${mac} Version/26.6.2 Safari/605.1.15`, true)).toBe(false);
  });

  it("leaves Windows and Linux to the button: nothing was measured there", () => {
    expect(opensAppByItself(`${mac} Chrome/156.0.0.0 Safari/537.36`, false)).toBe(false);
  });
});

describe("mayHaveDesktopApp", () => {
  it("is a computer, not a phone or a tablet", () => {
    expect(mayHaveDesktopApp("Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7)", 0)).toBe(true);
    expect(mayHaveDesktopApp("Mozilla/5.0 (Windows NT 10.0; Win64; x64)", 0)).toBe(true);
    expect(mayHaveDesktopApp("Mozilla/5.0 (X11; Linux x86_64)", 0)).toBe(true);
    expect(mayHaveDesktopApp("Mozilla/5.0 (iPhone; CPU iPhone OS 18_0)", 5)).toBe(false);
    expect(mayHaveDesktopApp("Mozilla/5.0 (Linux; Android 15) Mobile", 5)).toBe(false);
    expect(mayHaveDesktopApp("Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7)", 5)).toBe(false);
  });
});

describe("phoneAppLinkOf", () => {
  it("reads the three ledger links and a shared feed", () => {
    for (const kind of ["i", "c", "g"]) {
      expect(phoneAppLinkOf(`/${kind}/${ID}`, `#${SECRET}`, ORIGIN)).toEqual({ path: `/${kind}/${ID}`, fragment: SECRET });
    }
    expect(phoneAppLinkOf("/feed/import", "#AbC_-9", ORIGIN)).toEqual({ path: "/feed/import", fragment: "AbC_-9" });
    expect(phoneAppLinkOf("/feed/import/", "#AbC", ORIGIN)).toEqual({ path: "/feed/import", fragment: "AbC" });
  });

  it("offers nothing for any other page, or a link without its secret", () => {
    for (const [path, hash] of [
      ["/", ""],
      ["/login", ""],
      [`/c/${ID}`, ""],
      [`/c/${ID}`, "#short"],
      ["/feed/import", ""],
      ["/feed/import", "#not base64!"],
      ["/feed/import", `#${"A".repeat(5463)}`],
      ["/feed", "#AbC"],
    ] as const) {
      expect(phoneAppLinkOf(path, hash, ORIGIN), `${path}${hash}`).toBeNull();
    }
  });
});

describe("phoneAppPlatform", () => {
  it("names the phone app a browser could have beside it", () => {
    expect(phoneAppPlatform("Mozilla/5.0 (Linux; Android 15; Pixel 9) Mobile Safari/537.36", 5)).toBe("android");
    expect(phoneAppPlatform("Mozilla/5.0 (Linux; Android 15) Firefox/140.0", 5)).toBe("android");
    expect(phoneAppPlatform("Mozilla/5.0 (iPhone; CPU iPhone OS 18_0 like Mac OS X)", 5)).toBe("ios");
    expect(phoneAppPlatform("Mozilla/5.0 (iPad; CPU OS 18_0 like Mac OS X)", 5)).toBe("ios");
    // iPadOS Safari reports a Mac with a touch screen.
    expect(phoneAppPlatform("Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7)", 5)).toBe("ios");
  });

  it("is nothing on a computer", () => {
    expect(phoneAppPlatform("Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7)", 0)).toBeNull();
    expect(phoneAppPlatform("Mozilla/5.0 (Windows NT 10.0; Win64; x64)", 0)).toBeNull();
    expect(phoneAppPlatform("Mozilla/5.0 (X11; Linux x86_64)", 0)).toBeNull();
  });

  it("never overlaps the desktop offer", () => {
    for (const [ua, touch] of [
      ["Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7)", 0],
      ["Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7)", 5],
      ["Mozilla/5.0 (Linux; Android 15) Mobile", 5],
      ["Mozilla/5.0 (iPhone; CPU iPhone OS 18_0)", 5],
      ["Mozilla/5.0 (Windows NT 10.0; Win64; x64)", 0],
    ] as const) {
      expect(mayHaveDesktopApp(ua, touch) && phoneAppPlatform(ua, touch) !== null, ua).toBe(false);
    }
  });
});

describe("androidPackageFromEnv", () => {
  it("defaults to the one package every Android build has", () => {
    expect(androidPackageFromEnv(undefined)).toBe(DEFAULT_ANDROID_PACKAGE);
    expect(androidPackageFromEnv("  ")).toBe("me.lakko.dev");
    expect(androidPackageFromEnv("me.lakko")).toBe("me.lakko");
  });

  it("refuses what is not a package name", () => {
    for (const value of ["lakko", "me..lakko", "me.lakko;S.x=y", "1me.lakko", "me.lakko#"]) {
      expect(androidPackageFromEnv(value), value).toBeNull();
    }
  });
});

describe("androidIntentUrl", () => {
  it("names the app and keeps the secret in the data", () => {
    const url = androidIntentUrl({ path: `/c/${ID}`, fragment: SECRET }, "https://test.lakko.me", "me.lakko.dev");
    expect(url).toBe(
      `intent://test.lakko.me/c/${ID}#${SECRET}#Intent;scheme=https;package=me.lakko.dev;` +
        `S.browser_fallback_url=${encodeURIComponent(`https://test.lakko.me/c/${ID}#${SECRET}`)};end`,
    );
  });

  it("leaves one # after the data, so Android reads the extras from the right place", () => {
    const url = androidIntentUrl({ path: "/feed/import", fragment: "AbC_-9" }, ORIGIN, "me.lakko.dev");
    // Android splits an intent address at its LAST `#`, which must start the extras.
    expect(url.slice(url.lastIndexOf("#"))).toMatch(/^#Intent;/);
    expect(url.slice(0, url.lastIndexOf("#"))).toBe("intent://lakko.me/feed/import#AbC_-9");
  });

  it("keeps a local stack's scheme and port", () => {
    const url = androidIntentUrl({ path: `/i/${ID}`, fragment: SECRET }, "http://127.0.0.1:5173", "me.lakko.dev");
    expect(url.startsWith(`intent://127.0.0.1:5173/i/${ID}#${SECRET}#Intent;scheme=http;`)).toBe(true);
  });
});

describe("iosAppUrl", () => {
  it("is the app's own scheme with the whole link", () => {
    expect(iosAppUrl({ path: `/g/${ID}`, fragment: SECRET })).toBe(`lakko://g/${ID}#${SECRET}`);
    expect(iosAppUrl({ path: "/feed/import", fragment: "AbC" })).toBe("lakko://feed/import#AbC");
  });
});
