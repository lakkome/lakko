// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import { defineConfig } from "vite";
import solid from "vite-plugin-solid";
import wasm from "vite-plugin-wasm";

/**
 * The moderators' panel (ADR-0099 section 7). It has no copy of the design
 * system or the wasm core: it imports `apps/web/src/styles.css`, two of the
 * web client's UI components and `apps/web/wasm/lakko_ffi_wasm` (build it
 * with `npm run build:wasm` in apps/web first).
 *
 * `dedupe` matters: the web components import `solid-js` from apps/web's own
 * node_modules, and a second Solid instance would render nothing reactive.
 *
 * In production nginx serves the built `dist/` from a static root at `/`
 * and proxies only `/panel/api/*` to the moderation service's panel
 * listener, so the files and the API are one origin but two processes. The
 * app therefore assumes nothing about who serves its files: every asset URL
 * is relative to `base` and every API call is the relative `/panel/api`.
 * The service's LAKKO_MODERATION_PANEL_DIR (serving `dist/` itself) is for
 * local development only. In development the dev server forwards the API to
 * the panel listener.
 */
const appsDir = decodeURIComponent(new URL("..", import.meta.url).pathname);

export default defineConfig({
  // The app is served at the root of its own host.
  base: "/",
  plugins: [solid(), wasm()],
  resolve: {
    dedupe: ["solid-js", "solid-js/web", "solid-js/store"],
  },
  server: {
    port: 5180,
    strictPort: true,
    fs: { allow: [appsDir] },
    proxy: {
      "/panel/api": {
        target: "http://127.0.0.1:8099",
        changeOrigin: false,
      },
    },
  },
  build: {
    target: "es2022",
    sourcemap: true,
  },
});
