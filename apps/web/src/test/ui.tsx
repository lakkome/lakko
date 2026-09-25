// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * The web client's UI-test harness: jsdom, the Solid JSX transform, and the
 * smallest render helper that works.
 *
 * ## Why this exists
 *
 * Every rule that could be lifted out of a component has been, and those
 * modules are well covered. What was left uncovered was the layer that keeps
 * breaking: which gesture opens what, what a component hands its collaborators
 * and in what ORDER, where a node ends up in the tree, and which CSS
 * declaration wins. Nothing in a `.tsx` was reachable from a test at all —
 * bare node has no DOM and the vitest config carried no JSX transform — so
 * every defect in that layer was found by a person holding a phone.
 *
 * ## What this harness can catch
 *
 *   * **Wiring.** Which DOM event a handler is on, and therefore which gesture
 *     reaches it. A menu that opens on `mouseover` and one that opens on
 *     `contextmenu` are indistinguishable to a rule test and opposite here.
 *   * **Tree shape.** Whether a node is rendered inside the row it belongs to
 *     or on a line of its own — a real defect class, since "the time sits
 *     under the duration instead of beside it" is a parent, not a pixel.
 *   * **Call order across an await.** Whether production code reaches the
 *     platform on the handler's own stack or after a promise. jsdom cannot
 *     enforce a user activation, but "was `play()` called before this function
 *     returned" is exactly the property the activation rule turns into a
 *     requirement, and it is observable here.
 *   * **The cascade.** Which declaration wins for an element, read out of the
 *     REAL `styles.css`. jsdom resolves selectors and specificity.
 *
 * ## What it cannot
 *
 *   * **Layout.** jsdom computes none: every `offsetHeight` is 0, every
 *     `getBoundingClientRect()` is all zeros. Nothing about a size, a
 *     position, an overlap or a scroll is provable here. A rule that resolves
 *     to `min-height: 40px` can be caught; the 28px the bubble then grew by
 *     cannot.
 *   * **Real media.** `HTMLMediaElement.play()` is a stub that decodes
 *     nothing, and there is no `MediaRecorder`, no `getUserMedia` and no
 *     `AudioContext`. A test that needs those must stand a stub in the
 *     browser's place and confine its assertions to what OUR code did to it.
 *   * **Engine policy.** User activation, autoplay gating, codec support,
 *     touch behaviour, compositing. Those need a real browser.
 *   * **Anything on iOS or Android.** This is web only. There is no
 *     Robolectric or `compose-ui-test` target on Android and no XCUITest
 *     target on iOS, so the same defect class remains uncovered on both.
 */

import { render } from "solid-js/web";
import { afterEach, expect } from "vitest";
import { readFileSync } from "node:fs";
import { resolve } from "node:path";
import type { JSX } from "solid-js";

const mounted: Array<() => void> = [];

/**
 * Mount `code` into a fresh host in the document and return the host.
 *
 * Deliberately not `@solidjs/testing-library`: it is a wrapper around
 * `render` plus `@testing-library/dom`, and the query surface below is the
 * part of it these tests use. The repository's own rule is that a dependency
 * has to earn itself, and thirty lines is cheaper than a transitive tree.
 *
 * The disposer is registered for `afterEach` rather than returned, because a
 * Solid root that is never disposed keeps its effects — and a voice bubble's
 * `onCleanup`, which pauses an element — alive into the next test.
 */
export function renderUi(code: () => JSX.Element): HTMLDivElement {
  const host = document.createElement("div");
  document.body.appendChild(host);
  const dispose = render(code, host);
  mounted.push(() => {
    dispose();
    host.remove();
  });
  return host;
}

afterEach(() => {
  while (mounted.length > 0) mounted.pop()?.();
  // A `Portal` mounts outside the host, and the stylesheet is per-suite.
  document.body.replaceChildren();
  document.head.replaceChildren();
});

/** The one element matching `sel`, asserted to exist. Searches the whole
 *  document, because a `Portal` renders outside the host on purpose. */
export function one<E extends Element = HTMLElement>(sel: string, root: ParentNode = document): E {
  const el = root.querySelector<E>(sel);
  expect(el, `no element matched ${sel}`).not.toBeNull();
  return el as E;
}

/** Every element matching `sel`. */
export function all<E extends Element = HTMLElement>(sel: string, root: ParentNode = document): E[] {
  return [...root.querySelectorAll<E>(sel)];
}

/** Whether anything matches `sel`. The negative form is what a "this gesture
 *  must open nothing" assertion needs. */
export function present(sel: string, root: ParentNode = document): boolean {
  return root.querySelector(sel) !== null;
}

/**
 * The production stylesheet, parsed by jsdom into the document.
 *
 * The real file, not a fixture: the point of a cascade test is that the rule
 * shipping to people is the one under test. jsdom drops declarations it
 * cannot parse, so a caller must assert on a control element as well as the
 * one it cares about — otherwise "no rule matched" and "the right rule won"
 * look the same.
 */
export function installStylesheet(): void {
  const el = document.createElement("style");
  el.textContent = readFileSync(resolve(process.cwd(), "src/styles.css"), "utf8");
  document.head.appendChild(el);
}

/** The wasm core, initialised once per worker from the built bytes. Under
 *  jsdom there is no `fetch` of a `file:` URL, so the bytes are handed in. */
let wasmReady: Promise<void> | null = null;
export async function initWasm(): Promise<void> {
  wasmReady ??= (async () => {
    const mod = await import("../../wasm/lakko_ffi_wasm");
    await mod.default({
      module_or_path: readFileSync(resolve(process.cwd(), "wasm/lakko_ffi_wasm_bg.wasm")),
    });
  })();
  return wasmReady;
}

/** Dispatch a `PointerEvent`. jsdom 29 implements the constructor but not
 *  `setPointerCapture`; every call site in this app guards that one. */
export function pointer(
  el: Element,
  type: "pointerdown" | "pointermove" | "pointerup" | "pointercancel",
  init: PointerEventInit = {},
): void {
  el.dispatchEvent(
    new PointerEvent(type, { bubbles: true, cancelable: true, pointerId: 1, button: 0, ...init }),
  );
}

/** Dispatch a `MouseEvent`, including the hover ones a menu must ignore. */
export function mouse(
  el: Element,
  type: "click" | "mouseover" | "mouseenter" | "mousemove" | "contextmenu",
  init: MouseEventInit = {},
): void {
  el.dispatchEvent(
    new MouseEvent(type, { bubbles: type !== "mouseenter", cancelable: true, ...init }),
  );
}

/** Let queued microtasks and a zero timer run. */
export function settle(): Promise<void> {
  return new Promise((r) => setTimeout(r, 0));
}

/**
 * A working `localStorage`, because in this environment there is not one.
 *
 * Node now ships its own `localStorage` global, and it is only functional when
 * the process was started with a valid `--localstorage-file`. It is not, so the
 * global is an object whose `getItem` is `undefined` — and it SHADOWS the one
 * jsdom provides, which would have worked. Measured: `localStorage ===
 * window.localStorage` is `true` and `typeof localStorage.getItem` is
 * `"undefined"`.
 *
 * That combination is the worst one for this codebase. Every module that reads
 * the store guards with `typeof localStorage === "undefined"` — which PASSES —
 * and then throws `localStorage.getItem is not a function` on the next line. So
 * a component reading a persisted preference does not degrade, it fails to
 * render, and the failure names the product module rather than the harness.
 *
 * Opt-in rather than installed for everyone: no existing suite needs it, and a
 * global that appears halfway through a project's life is worth asking for by
 * name. In-memory, and cleared for each test that asks.
 */
export function installLocalStorage(seed: Record<string, string> = {}): void {
  const data = new Map(Object.entries(seed));
  const store: Storage = {
    get length() {
      return data.size;
    },
    clear: () => data.clear(),
    getItem: (k: string) => data.get(k) ?? null,
    key: (i: number) => [...data.keys()][i] ?? null,
    removeItem: (k: string) => void data.delete(k),
    setItem: (k: string, v: string) => void data.set(k, String(v)),
  };
  for (const target of [globalThis, globalThis.window]) {
    if (target === undefined) continue;
    Object.defineProperty(target, "localStorage", {
      value: store,
      configurable: true,
      writable: true,
    });
  }
}
