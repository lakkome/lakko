// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * Bundle-size report for `apps/web` (Phase 1.16f1).
 *
 * Walks `dist/` after a production build, prints a table of
 * raw + gzipped + brotli sizes per artifact, and a summary
 * total. Source maps are excluded from the totals — they are
 * dev-aid artefacts, not user-served bytes.
 *
 * The script has zero dependencies beyond Node's stdlib so it
 * runs cleanly in CI without `npm install` on the report path.
 *
 * Why the totals matter:
 *   * Anti-censorship transports (REALITY, MASQUE, …) often
 *     run over throttled or jittery links. Every kB shaved
 *     directly improves cold-load latency for users in the
 *     RU/IR/CN deployment targets.
 *   * The 1.16f budget is < 2 MB gzipped end-to-end. We are
 *     well under that today; the report is a **trend gate**,
 *     not a hard threshold — it's there so a drift like
 *     "WASM grew 200 kB on Tuesday" gets caught before the
 *     month-end review.
 */
import { readdirSync, readFileSync, statSync } from "node:fs";
import { gzipSync, brotliCompressSync, constants as zlibConstants } from "node:zlib";
import { join, relative } from "node:path";
import { fileURLToPath } from "node:url";

const HERE = fileURLToPath(new URL(".", import.meta.url));
const DIST = join(HERE, "..", "dist");

/** Files we never count toward totals — dev-only artefacts. */
const EXCLUDE_RE = /\.(map)$/;

/** Brotli setting — Q11 matches what real CDNs serve at edge.
 *  Q9 is closer to nginx-with-brotli at default tuning. We pick
 *  Q11 to keep the report optimistic about what users actually
 *  download from a properly-configured CDN. */
const BROTLI_OPTS = {
  params: {
    [zlibConstants.BROTLI_PARAM_QUALITY]: 11,
  },
};

/** Recursively walk a directory, returning absolute file paths. */
function walk(dir) {
  const out = [];
  for (const entry of readdirSync(dir, { withFileTypes: true })) {
    const path = join(dir, entry.name);
    if (entry.isDirectory()) {
      out.push(...walk(path));
    } else if (entry.isFile()) {
      out.push(path);
    }
  }
  return out;
}

/** Pretty-print a byte count with a comma-thousands separator
 *  and a fixed minimum width so the columns line up. */
function fmt(bytes) {
  return bytes.toLocaleString("en-US").padStart(10);
}

function main() {
  let allFiles;
  try {
    allFiles = walk(DIST);
  } catch (err) {
    console.error(`bundle-report: cannot read dist/ — did you run \`npm run build\`?`);
    console.error(err.message);
    process.exit(1);
  }

  const rows = [];
  let totalRaw = 0;
  let totalGz = 0;
  let totalBr = 0;

  for (const path of allFiles) {
    if (EXCLUDE_RE.test(path)) continue;
    const bytes = readFileSync(path);
    const raw = bytes.length;
    const gz = gzipSync(bytes, { level: 9 }).length;
    const br = brotliCompressSync(bytes, BROTLI_OPTS).length;
    rows.push({ path: relative(DIST, path), raw, gz, br });
    totalRaw += raw;
    totalGz += gz;
    totalBr += br;
  }

  // Sort largest first (gzipped) — what dominates the wire is
  // what we're trying to drive down.
  rows.sort((a, b) => b.gz - a.gz);

  const colPath = Math.max(20, ...rows.map((r) => r.path.length));
  const header =
    "file".padEnd(colPath) +
    "  " +
    "raw".padStart(10) +
    "  " +
    "gzip".padStart(10) +
    "  " +
    "brotli".padStart(10);
  console.log(header);
  console.log("-".repeat(header.length));
  for (const r of rows) {
    console.log(
      `${r.path.padEnd(colPath)}  ${fmt(r.raw)}  ${fmt(r.gz)}  ${fmt(r.br)}`,
    );
  }
  console.log("-".repeat(header.length));
  console.log(
    `${"total".padEnd(colPath)}  ${fmt(totalRaw)}  ${fmt(totalGz)}  ${fmt(totalBr)}`,
  );

  // 1.16f budget: < 2 MB gzipped. The threshold is advisory —
  // we exit non-zero past 2× the budget to catch runaway
  // regressions, but a normal sprint should stay well under.
  const budgetGz = 2 * 1024 * 1024;
  const hardCap = 2 * budgetGz;
  console.log("");
  console.log(
    `gzip total: ${(totalGz / 1024).toFixed(1)} kB  (budget: ${(budgetGz / 1024).toFixed(0)} kB, hard cap: ${(hardCap / 1024).toFixed(0)} kB)`,
  );
  if (totalGz > hardCap) {
    console.error("bundle-report: HARD CAP EXCEEDED");
    process.exit(2);
  }
}

main();
