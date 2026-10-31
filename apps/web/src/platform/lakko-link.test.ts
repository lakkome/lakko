// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import { describe, expect, it } from "vitest";

import { appPathOf, mayHaveDesktopApp, openInAppAction, opensAppByItself, parseLakkoLink } from "./lakko-link";

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

describe("openInAppAction", () => {
  const link = { kind: "c", id: ID, secret: SECRET } as const;

  it("carries the secret in the URL only on macOS", () => {
    expect(openInAppAction(link, true)).toEqual({ url: `lakko://c/${ID}#${SECRET}`, copyFirst: false });
  });

  it("keeps the secret off a command line elsewhere", () => {
    const action = openInAppAction(link, false);
    expect(action).toEqual({ url: `lakko://c/${ID}`, copyFirst: true });
    expect(action.url).not.toContain(SECRET);
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

  it("never tries where the secret cannot travel in the link", () => {
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
