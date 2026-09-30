// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * Solid.js entry point — mounts <App /> into the #root div in
 * `index.html`. This file is intentionally tiny: anything that needs
 * to evolve (routing, providers, error boundaries) belongs in
 * `App.tsx` or its descendants, not here.
 *
 * Phase 1.16a smoke goal: confirm Solid mounts, the WASM loader runs
 * once, and the Rust core round-trip renders. Phase 1.16b added
 * routing, signup/login, contacts and 1×1 chat. Phase 1.16f2 wires
 * the service worker registration *after* the app mounts so a
 * registration failure / network blip can't block the first paint.
 */
import { render } from "solid-js/web";
import App from "./App";
import "./styles.css";
import { registerServiceWorker } from "./sw/register";
import { runBootUpdateCheck } from "./update/release-check";
import { bootstrapTheme } from "./ui/theme";
import { bootstrapL10n, LANGUAGE_STORAGE_KEY } from "./l10n";
import { publishNotificationStrings } from "./l10n/notification-strings";

/** Longest the first paint waits for a non-English catalog. Chosen, not measured. */
const FIRST_PAINT_WAIT_MS = 2000;

/**
 * A cheap guess, before the core has loaded, whether the resolved language
 * is not English. It only decides whether the first paint waits; the
 * language itself is still resolved by the core.
 */
function likelyNonEnglish(): boolean {
  let stored: string | null = null;
  try {
    stored = localStorage.getItem(LANGUAGE_STORAGE_KEY);
  } catch {
    stored = null;
  }
  if (stored !== null && stored !== "") return stored !== "en";
  const first = navigator.languages?.[0] ?? navigator.language ?? "en";
  return !first.toLowerCase().startsWith("en");
}

const root = document.getElementById("root");
if (root === null) {
  // index.html is shipped by us; if #root is missing the page is
  // broken at the source-tree level, not at runtime — fail loud.
  throw new Error("apps/web: #root element missing from index.html");
}

// 1.18f — apply the persisted `data-theme` BEFORE the Solid
// app renders so the very first frame doesn't flash the wrong
// theme. Synchronous reads of localStorage + a single
// setAttribute; ~0.5 ms typical.
bootstrapTheme();

// ADR-0058: resolve the interface language (stored choice, else the system
// list, else English) and load its catalog. Until the core has loaded every
// string renders from the English source. When the language is likely not
// English, the first paint waits for the catalog so the screen does not show
// English for a moment and then switch; the wait is capped so a slow or
// failed load never keeps the app from rendering. Never rejects.
const l10nReady = bootstrapL10n();
// ADR-0058: the service worker's notification words follow the language.
void l10nReady.then(publishNotificationStrings);
const mount = (): void => {
  render(() => <App />, root);
};
if (likelyNonEnglish()) {
  void Promise.race([l10nReady, new Promise((done) => setTimeout(done, FIRST_PAINT_WAIT_MS))]).then(
    mount,
  );
} else {
  mount();
}

// Fire-and-forget — the helper logs and swallows its own errors.
// Skipped in dev builds (Vite HMR + SW conflict) and in browsers
// without `navigator.serviceWorker`.
void registerServiceWorker();

// ADR-0038 S5 — the shared "update available" verify core. Fetches +
// verifies the signed release manifest and, if a newer release exists,
// emits a single console.info. Web renders NO banner (it defers to the
// SW-driven SwUpdateBanner, which is the web's real update path); this
// is the shared-core landing + a dormant future hook. Dormant unless
// VITE_LAKKO_RELEASE_KEY is pinned; swallows its own errors.
void runBootUpdateCheck();
