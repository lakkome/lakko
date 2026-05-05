// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * Solid.js entry point — mounts <App /> into the #root div in
 * `index.html`. This file is intentionally tiny: anything that needs
 * to evolve (routing, providers, error boundaries) belongs in
 * `App.tsx` or its descendants, not here.
 *
 * Phase 1.16a smoke goal: confirm Solid mounts, the WASM loader runs
 * once, and the Rust core round-trip renders. Phase 1.16b adds
 * routing, signup/login, contacts and 1×1 chat.
 */
import { render } from "solid-js/web";
import App from "./App";
import "./styles.css";

const root = document.getElementById("root");
if (root === null) {
  // index.html is shipped by us; if #root is missing the page is
  // broken at the source-tree level, not at runtime — fail loud.
  throw new Error("apps/web: #root element missing from index.html");
}

render(() => <App />, root);
