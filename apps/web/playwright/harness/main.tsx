// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * The harness entry: read `?case=`, mount that fixture, say when it is up.
 *
 * `data-ready` on the mount point is the handshake rather than a timeout,
 * because a case may await its bytes before it renders and a spec that
 * measured a bubble mid-mount would be measuring a race. A case that throws
 * writes `data-error` instead, so a broken fixture fails a spec loudly rather
 * than as "the element never appeared".
 */

import { render } from "solid-js/web";

import { cases } from "./cases";
// The media hub asks the core which MIME types may be rendered inline
// (`inlineMediaType`), and a wasm-bindgen `--target web` export throws until
// the module is instantiated. Without this the voice attachment's render
// decision fails and the bubble comes out as a plain file chip — which looks
// exactly like a bubble whose controls were never built.
import { loadWasm } from "../../src/wasm";
// The production stylesheet, unmodified: the height and the same-line
// measurements are only worth anything against the cascade that ships.
import "../../src/styles.css";

const root = document.getElementById("harness");
if (root === null) throw new Error("harness: no mount point");

const name = new URL(window.location.href).searchParams.get("case") ?? "";
const factory = cases[name];

if (factory === undefined) {
  root.dataset["error"] = `unknown case: ${name || "(none)"}`;
} else {
  void (async () => {
    try {
      await loadWasm();
      const view = await factory();
      render(() => view, root);
      // One frame after the mount, so the first layout has happened before a
      // spec is told it may measure.
      requestAnimationFrame(() => {
        requestAnimationFrame(() => {
          root.dataset["ready"] = "true";
        });
      });
    } catch (err) {
      root.dataset["error"] = err instanceof Error ? `${err.name}: ${err.message}` : String(err);
    }
  })();
}
