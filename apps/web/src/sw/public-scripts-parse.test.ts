// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import { execFileSync } from "node:child_process";
import { readdirSync } from "node:fs";
import { join } from "node:path";

import { describe, expect, it } from "vitest";

// The service worker scripts in `public/` are served as written, and no
// other test loads them whole: a syntax error there stopped the worker from
// starting on the test stack (2026-10-22) with every unit test green.
const PUBLIC_DIR = join(__dirname, "..", "..", "public");
const scripts = readdirSync(PUBLIC_DIR).filter((f) => f.endsWith(".js"));

describe("public scripts", () => {
  it("finds the service worker scripts", () => {
    expect(scripts).toContain("sw.js");
  });

  it.each(scripts)("%s parses", (file) => {
    // `node --check` parses the file as the module or script it is.
    expect(() =>
      execFileSync(process.execPath, ["--check", join(PUBLIC_DIR, file)], { stdio: "pipe" }),
    ).not.toThrow();
  });
});
