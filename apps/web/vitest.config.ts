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
  // Solid publishes two builds behind export conditions, and under `node` the
  // resolved one is `dist/server.js`, where the reactive primitives are inert
  // stubs: `createSignal` does not notify and `mapArray` never re-diffs. That
  // is right for SSR and useless for a test, so a suite that reaches for a
  // primitive silently asserts nothing.
  //
  // `chat/call-timeline.test.ts` needs the real one. It drives `mapArray` --
  // the reconciler `<For>` is built from -- to prove that an entry whose
  // reference did not change is not re-created, which is what keeps a voice
  // bubble's `onCleanup` from stopping playback every time a message arrives.
  // That needs reactivity, but no DOM and no JSX, so it stays inside this
  // config's plugin-free, jsdom-free shape rather than being unprovable here.
  //
  // Scoped deliberately narrowly: the condition selects a package's browser
  // entry point wherever one exists, so a dependency added later resolves to
  // its browser build in tests too. Measured green over the whole suite when
  // it went in.
  // `ssr.resolve.conditions`, not `resolve.conditions`: the tests run in the
  // node environment, which goes through Vite's SSR resolver, and that one
  // reads its own condition list. Setting only the client list looks right and
  // changes nothing.
  ssr: { resolve: { conditions: ["browser"] } },
  test: {
    environment: "node",
    include: ["src/**/*.test.ts"],
    // The wasm core is large; give the proof-roundtrip tests a
    // little headroom over the default per-test timeout.
    testTimeout: 20000,
  },
});
