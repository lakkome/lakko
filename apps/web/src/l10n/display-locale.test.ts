// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * docs/ux/localization.md section 5 — the language decides words, the device
 * decides formats. "The device" on the web is the browser's default locale;
 * these tests hand it in.
 */
import { describe, expect, it } from "vitest";

import { formatListTime } from "../chat/list-time";
import { dayLabelOf, shortTime } from "../chat/timeline-layout";
import { formatLocaleFor } from "./format-locale";

const AT = new Date(2026, 9, 29, 13, 5);
const time = (locale: string): string =>
  new Intl.DateTimeFormat(locale, { hour: "numeric", minute: "2-digit" }).format(AT);

describe("formatLocaleFor", () => {
  it("English words on a 24-hour device: 13:05, never 1:05 PM", () => {
    for (const device of ["de-DE", "en-GB", "ru-RU"]) {
      const locale = formatLocaleFor("en", device);
      expect(time(locale)).toBe("13:05");
      expect(shortTime(AT.toISOString(), locale)).toBe("13:05");
      expect(formatListTime(AT.toISOString(), AT, locale)).toBe("13:05");
    }
  });

  it("another language on a 12-hour device keeps the device's AM/PM form", () => {
    const locale = formatLocaleFor("ru", "en-US");
    expect(time(locale)).not.toContain("13");
    expect(time(locale)).toMatch(/^1:05\s?\S+/);
    // And the same language on a 24-hour device does not.
    expect(time(formatLocaleFor("ru", "ru-RU"))).toBe("13:05");
  });

  it("month names come from the interface language", () => {
    const opts: Intl.DateTimeFormatOptions = { day: "numeric", month: "long" };
    expect(new Intl.DateTimeFormat(formatLocaleFor("en", "de-DE"), opts).format(AT)).toContain("October");
    expect(new Intl.DateTimeFormat(formatLocaleFor("ru", "en-US"), opts).format(AT)).toContain("октября");
    const earlier = new Date(2026, 9, 1, 9, 0).toISOString();
    expect(dayLabelOf(earlier, AT, formatLocaleFor("en", "de-DE"))).toContain("October");
  });

  it("day order and separators follow the device where the engine has the pair", () => {
    const locale = formatLocaleFor("en", "de-DE");
    expect(new Intl.DateTimeFormat(locale, { dateStyle: "short" }).format(AT)).toBe("29/10/2026");
    expect(new Intl.NumberFormat(locale).format(1234.5)).toBe("1.234,5");
    expect(new Intl.NumberFormat(formatLocaleFor("en", "en-US")).format(1234.5)).toBe("1,234.5");
  });

  it("carries the device region and hour cycle on the interface language", () => {
    expect(formatLocaleFor("en", "de-DE")).toBe("en-DE-u-hc-h23");
    expect(formatLocaleFor("ru", "en-US")).toBe("ru-US-u-hc-h12");
    // A device with no region still gets its likely one.
    expect(formatLocaleFor("en", "de")).toBe("en-DE-u-hc-h23");
    // The interface script stays; its region gives way to the device's.
    expect(formatLocaleFor("zh-Hans", "en-GB")).toBe("zh-Hans-GB-u-hc-h23");
    expect(formatLocaleFor("pt-BR", "de-DE")).toBe("pt-DE-u-hc-h23");
  });

  it("falls back to the interface language when the device has no usable locale", () => {
    expect(formatLocaleFor("en", undefined)).toBe("en");
    expect(formatLocaleFor("en", "")).toBe("en");
    expect(formatLocaleFor("en", "not a locale!")).toBe("en");
  });
});
