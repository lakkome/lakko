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

render(() => <App />, root);

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
