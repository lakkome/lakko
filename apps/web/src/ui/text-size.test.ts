// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

// @vitest-environment jsdom

/**
 * `docs/ux/text-size.md` — the per-device text size: what is stored, what an
 * unknown value reads as, and that the factor reaches the bubble text and the
 * reaction chips (the rules in `styles.css` that read `--lk-text-scale`; the
 * measured sizes were checked in Chromium, jsdom computes no `calc()`).
 */
import { readFileSync } from "node:fs";
import { resolve } from "node:path";
import { afterEach, beforeEach, describe, expect, it } from "vitest";

import { MemoryStorage } from "../test/fake-storage";
import {
  TEXT_SIZE_CSS_PROPERTY,
  TEXT_SIZE_FACTOR,
  TEXT_SIZE_STORAGE_KEY,
  bootstrapTextSize,
  setTextSize,
  textSizeOf,
  useTextSize,
} from "./text-size";

const factor = (): string => document.documentElement.style.getPropertyValue(TEXT_SIZE_CSS_PROPERTY);

beforeEach(() => {
  Object.defineProperty(globalThis, "localStorage", { value: new MemoryStorage(), configurable: true });
  document.documentElement.style.removeProperty(TEXT_SIZE_CSS_PROPERTY);
});
afterEach(() => {
  Object.defineProperty(globalThis, "localStorage", { value: undefined, configurable: true });
});

describe("the stored step", () => {
  it("nothing stored reads as Medium, factor 1", () => {
    bootstrapTextSize();
    expect(useTextSize()).toBe("medium");
    expect(factor()).toBe("1");
  });

  it("a value this build does not know reads as Medium", () => {
    for (const raw of ["huge", "", "Large", "1.2"]) {
      localStorage.setItem(TEXT_SIZE_STORAGE_KEY, raw);
      bootstrapTextSize();
      expect(useTextSize()).toBe("medium");
      expect(factor()).toBe("1");
    }
    expect(textSizeOf(null)).toBe("medium");
    expect(textSizeOf(undefined)).toBe("medium");
  });

  it("a stored step applies its factor before the first render", () => {
    localStorage.setItem(TEXT_SIZE_STORAGE_KEY, "large");
    bootstrapTextSize();
    expect(useTextSize()).toBe("large");
    expect(factor()).toBe("1.2");
  });

  it("choosing applies at once and is kept on this device; Medium keeps nothing", () => {
    setTextSize("small");
    expect(factor()).toBe("0.87");
    expect(localStorage.getItem(TEXT_SIZE_STORAGE_KEY)).toBe("small");
    setTextSize("medium");
    expect(factor()).toBe("1");
    expect(localStorage.getItem(TEXT_SIZE_STORAGE_KEY)).toBeNull();
  });

  it("the factors are the spec's", () => {
    expect(TEXT_SIZE_FACTOR).toEqual({ small: 0.87, medium: 1, large: 1.2 });
  });
});

describe("the factor reaches message text and reactions", () => {
  const css = readFileSync(resolve(process.cwd(), "src/styles.css"), "utf8");
  const block = css.slice(css.indexOf("Text size (docs/ux/text-size.md"));

  /** The declarations of `selector` inside the text-size block. */
  function declarations(selector: string): string {
    const at = block.indexOf(`\n${selector} {`);
    expect(at, `${selector} is not scaled`).toBeGreaterThan(-1);
    return block.slice(at, block.indexOf("}", at));
  }

  it("is the last block of the stylesheet, so each rule wins over the one it scales", () => {
    expect(block.length).toBeGreaterThan(0);
    expect(block.indexOf(".lk-text-size-preview-list")).toBeGreaterThan(-1);
  });

  it.each([
    [".lk-bubble", ["font-size: calc(15px * var(--lk-text-scale))"]],
    [".lk-bubble-meta", ["font-size: calc(11px * var(--lk-text-scale))"]],
    [".lk-composer-input", ["font-size: calc(15px * var(--lk-text-scale))"]],
    [
      ".lk-reaction",
      [
        "height: calc(32px * var(--lk-text-scale))",
        "padding: 0 calc(10px * var(--lk-text-scale))",
        "font-size: calc(14px * var(--lk-text-scale))",
      ],
    ],
    [".lk-reaction-emoji", ["font-size: calc(18px * var(--lk-text-scale))"]],
    [
      ".lk-reaction-avatars .lk-reaction-avatar",
      ["width: calc(22px * var(--lk-text-scale))", "height: calc(22px * var(--lk-text-scale))"],
    ],
    [".lk-reaction-avatars .lk-reaction-avatar + .lk-reaction-avatar", ["margin-left: calc(-4px * var(--lk-text-scale))"]],
    [".lk-reactions", ["gap: calc(6px * var(--lk-text-scale))"]],
    [".lk-msg-menu-reactions button", ["font-size: calc(28px * var(--lk-text-scale))"]],
  ])("%s", (selector, wanted) => {
    const body = declarations(selector);
    for (const w of wanted) expect(body).toContain(w);
  });

  it("the quick bar's 44px target is not scaled", () => {
    expect(declarations(".lk-msg-menu-reactions button")).not.toMatch(/width|height/);
  });
});

describe("the quick reaction bar on a narrow window", () => {
  const css = readFileSync(resolve(process.cwd(), "src/styles.css"), "utf8");
  const rule = (selector: string): string => {
    const at = css.indexOf(`\n${selector} {`);
    expect(at, selector).toBeGreaterThan(-1);
    return css.slice(at, css.indexOf("}", at));
  };

  it("stops at the window less the menu's margins and scrolls; targets never shrink", () => {
    expect(rule(".lk-msg-menu-reactions")).toContain("max-width: calc(100vw - 16px)");
    expect(rule(".lk-msg-menu-reaction-row")).toContain("overflow-x: auto");
    expect(rule(".lk-msg-menu-reaction-row > button")).toContain("flex: 0 0 auto");
    expect(rule(".lk-msg-menu-reactions button")).toContain("width: 44px");
  });
});
