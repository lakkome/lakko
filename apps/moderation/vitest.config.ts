// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import { defineConfig } from "vitest/config";

/** The panel's pure rules: bare node, no DOM, no JSX, no wasm. */
export default defineConfig({
  test: {
    name: "panel-rules",
    environment: "node",
    include: ["src/**/*.test.ts"],
  },
});
