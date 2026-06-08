// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import { defineConfig } from "vitest/config";

// A dedicated vitest config kept separate from `vite.config.ts` on
// purpose: the app build pulls in `vite-plugin-solid` (JSX
// transform) and `vite-plugin-wasm`, neither of which the unit
// tests need. The tests run in the plain `node` environment with
// an in-memory `localStorage` shim installed per-suite, and the
// G-4 supersession tests drive the wasm core through its
// Node-friendly `__wbindgen_init` glue (loaded directly from the
// generated `.wasm` bytes). Keeping this config plugin-free avoids
// a jsdom dependency and the Solid transform mangling plain
// `.test.ts` modules.
export default defineConfig({
  test: {
    environment: "node",
    include: ["src/**/*.test.ts"],
    // The wasm core is large; give the proof-roundtrip tests a
    // little headroom over the default per-test timeout.
    testTimeout: 20000,
  },
});
