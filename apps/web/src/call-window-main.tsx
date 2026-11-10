// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * Entry of the collapsed call's own window in the desktop app
 * (`call-window.html`, `docs/ux/calls.md` section 4.5.4a). Mounts only that
 * window: no app, no wasm, no service worker, no storage.
 */
import { render } from "solid-js/web";

import "./styles.css";
import { DesktopCallWindowPage } from "./calls/DesktopCallWindowPage";

const root = document.getElementById("root");
if (root !== null) {
  document.documentElement.classList.add("lk-call-window-doc");
  render(() => <DesktopCallWindowPage />, root);
}
