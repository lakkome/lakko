// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * Post-build: write the Terms of Service and the Privacy Policy as static
 * pages, `dist/terms/index.html` and `dist/privacy/index.html`
 * (docs/ux/legal-pages.md section 1). Readable without signing in, without
 * JavaScript and without the app; nginx serves them as files.
 *
 * Sources are `docs/legal/terms.md` and `docs/legal/privacy.md`. While a
 * source still says TODO or FIX BEFORE PUBLISHING, its page is a stub that
 * says the page is being prepared, and the build prints a warning (it does
 * not fail). Zero dependencies beyond `node:*`.
 */
import { existsSync, mkdirSync, readFileSync, writeFileSync } from "node:fs";
import { join } from "node:path";
import { fileURLToPath } from "node:url";

import { legalPage } from "./legal-render.mjs";

const HERE = fileURLToPath(new URL(".", import.meta.url));
const DIST = join(HERE, "..", "dist");
const LEGAL = join(HERE, "..", "..", "..", "docs", "legal");

const PAGES = [
  { source: "terms.md", dir: "terms", title: "Terms of Service" },
  { source: "privacy.md", dir: "privacy", title: "Privacy Policy" },
];

if (!existsSync(DIST)) {
  console.error(`legal-pages: ${DIST} does not exist; run vite build first`);
  process.exit(1);
}

for (const page of PAGES) {
  const sourcePath = join(LEGAL, page.source);
  const source = readFileSync(sourcePath, "utf8");
  const { html, stub } = legalPage(source, page.title);
  const outDir = join(DIST, page.dir);
  mkdirSync(outDir, { recursive: true });
  writeFileSync(join(outDir, "index.html"), html);
  if (stub) {
    console.warn(
      `legal-pages: WARN docs/legal/${page.source} still has TODO or FIX BEFORE PUBLISHING; wrote a stub to dist/${page.dir}/index.html`,
    );
  } else {
    console.log(`legal-pages: wrote dist/${page.dir}/index.html`);
  }
}
