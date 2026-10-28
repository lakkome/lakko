// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import { defineConfig, type Plugin } from "vite";
import solid from "vite-plugin-solid";
import wasm from "vite-plugin-wasm";
import { fileURLToPath } from "node:url";
import { dirname, resolve } from "node:path";

const HERE = dirname(fileURLToPath(import.meta.url));
/** `apps/web` — the root stays the app's, so `src/` imports and the wasm
 *  output resolve exactly as they do in `npm run dev`. */
const APP = resolve(HERE, "../..");

/**
 * Substitute the two name-resolver contexts `TimelineBubble` reads.
 *
 * The real-browser equivalent of the `vi.mock` calls at the top of
 * `TimelineBubble.test.tsx`, and confined to the same two modules for the same
 * reason (`context-stubs.ts` states it). It is done here, by module id, rather
 * than by editing `src/` — the components under test must be the ones that
 * ship, byte for byte.
 *
 * Scoped to importers inside `src/`, so the stub cannot be reached by accident
 * from the harness itself, and matched on the resolved specifier's tail rather
 * than on a path, because the importers use different relative depths.
 */
function stubContexts(): Plugin {
  const stub = resolve(HERE, "context-stubs.ts");
  return {
    name: "lakko-harness-stub-contexts",
    enforce: "pre",
    resolveId(source, importer) {
      if (importer === undefined || !importer.includes(`${APP}/src/`)) return null;
      return /(^|\/)(ChatContext|ProfileContext)(\.tsx?)?$/.test(source) ? stub : null;
    },
  };
}

export default defineConfig({
  root: APP,
  plugins: [stubContexts(), solid(), wasm()],
  // `src/ui/app-version.ts` reads this constant; the harness does not render
  // Settings, but a define that is missing is a runtime ReferenceError in any
  // module that transitively imports it.
  define: { __LAKKO_PKG_VERSION__: JSON.stringify("0.0.0-harness") },
  // The call page runs the real SFrame transform worker, which imports the
  // wasm core: the worker bundle needs the wasm plugin too, as in the app.
  worker: {
    format: "es",
    plugins: () => [wasm()],
  },
  server: {
    // Its own port, so a `npm run dev` already running on 5173 is untouched.
    port: 5174,
    strictPort: true,
    // Pinned to the v4 loopback rather than left at `localhost`, which on a
    // machine that resolves it to `::1` first leaves nothing listening on
    // 127.0.0.1 — and the specs, the `baseURL` and the readiness probe all
    // speak v4. A name that resolves differently per machine is not an address.
    host: "127.0.0.1",
  },
  optimizeDeps: {
    // Only the harness page. Left to itself Vite scans every HTML file under
    // the root, finds the app's own `index.html`, and follows it into
    // `App.tsx` — which imports `ChatProvider` and `ProfileProvider`, the two
    // exports the stub deliberately does not have. The app entry is not in
    // this project's graph at all, so naming the one entry is the fix rather
    // than widening the stub to satisfy a page nobody opens.
    entries: [
      "playwright/harness/index.html",
      "playwright/harness/call.html",
      "playwright/harness/group-call.html",
    ],
    exclude: ["@lakko/ffi-wasm"],
  },
});
