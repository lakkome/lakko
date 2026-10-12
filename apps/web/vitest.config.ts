// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import { defineConfig } from "vitest/config";
import solid from "vite-plugin-solid";

/**
 * Two projects, deliberately, rather than one config that grew a DOM.
 *
 * The rule tests (`*.test.ts`) run in bare `node` with no plugins and no
 * jsdom, exactly as they did before the UI project existed. The UI tests
 * (`*.test.tsx`) run in jsdom with the Solid JSX transform. Keeping them
 * apart is not tidiness: `vite-plugin-solid`'s own `config` hook rewrites
 * `resolve.conditions` and `test.server.deps.external` for the whole project
 * it is in, and one of those rewrites (externalising `solid-js`) is precisely
 * the mistake the `node` project's condition list exists to prevent — see
 * below. A project boundary is what makes "the 2400 rule tests are unaffected"
 * a fact about the configuration rather than a hope.
 */
export default defineConfig({
  test: {
    projects: [
      // ----- the rule tests: bare node, no DOM, no JSX --------------------
      //
      // Byte-for-byte the config this file used to be. Its comments are kept
      // whole because they record a trap that is still live.
      {
        // Solid publishes two builds behind export conditions, and under `node`
        // the resolved one is `dist/server.js`, where the reactive primitives
        // are inert stubs: `createSignal` does not notify and `mapArray` never
        // re-diffs. That is right for SSR and useless for a test, so a suite
        // that reaches for a primitive silently asserts nothing.
        //
        // `chat/call-timeline.test.ts` needs the real one. It drives `mapArray`
        // -- the reconciler `<For>` is built from -- to prove that an entry
        // whose reference did not change is not re-created, which is what keeps
        // a voice bubble's `onCleanup` from stopping playback every time a
        // message arrives. That needs reactivity, but no DOM and no JSX, so it
        // stays inside this project's plugin-free, jsdom-free shape rather than
        // being unprovable here.
        //
        // Scoped deliberately narrowly: the condition selects a package's
        // browser entry point wherever one exists, so a dependency added later
        // resolves to its browser build in tests too. Measured green over the
        // whole suite when it went in.
        // `ssr.resolve.conditions`, not `resolve.conditions`: the tests run in
        // the node environment, which goes through Vite's SSR resolver, and
        // that one reads its own condition list. Setting only the client list
        // looks right and changes nothing.
        ssr: { resolve: { conditions: ["browser"] } },
        test: {
          name: "rules",
          environment: "node",
          include: ["src/**/*.test.ts"],
          // No unit test may reach the network: every transport global
          // refuses unless the test installs its own (`src/test/no-network.ts`).
          setupFiles: ["src/test/no-network.ts"],
          // The wasm core is large; give the proof-roundtrip tests a
          // little headroom over the default per-test timeout.
          testTimeout: 20000,
        },
      },

      // ----- the UI tests: jsdom + the Solid JSX transform -----------------
      //
      // What this project buys, and what it does not, is stated in
      // `src/test/ui.tsx`. In short: a component's tree, its wiring and the
      // order in which it calls the platform are reachable here; anything that
      // needs layout, real media decoding or a real user activation is not.
      {
        plugins: [solid()],
        test: {
          name: "ui",
          environment: "jsdom",
          include: ["src/**/*.test.tsx"],
          setupFiles: ["src/test/no-network.ts"],
          // Solid must be the BROWSER build here for the same reason the
          // project above pins the condition, and for a second one: the
          // plugin's `config` hook adds `/solid-js/` to `server.deps.external`
          // when the config does not already speak about it, and an
          // externalised module is loaded by Node's own resolver, which reads
          // the `node` condition and lands back on `dist/server.js` — where
          // `render` produces no DOM. `inline` takes precedence over
          // `external` in Vitest, so saying it here settles it whatever the
          // plugin adds.
          server: { deps: { inline: [/solid-js/] } },
          testTimeout: 20000,
        },
      },
    ],
  },
});
