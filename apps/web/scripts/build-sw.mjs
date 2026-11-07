// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * Post-build pass that turns `dist/sw.js` (Vite copied it from
 * `public/sw.js` during `vite build`) into a deploy-ready service
 * worker by replacing the two placeholders the file ships with:
 *
 *   * `__BUILD_VERSION__` — short SHA-256 prefix over the
 *     concatenated content of every precached asset. Same bytes
 *     ⇒ same version ⇒ no SW reinstall on a no-op rebuild;
 *     anything that changes flips the cache name and triggers an
 *     `activate` cleanup of the prior version.
 *   * `__PRECACHE_MANIFEST__` — JSON array literal listing every
 *     asset the SW must seed into the cache on `install`. We
 *     enumerate from `dist/` directly rather than parse Vite's
 *     internal manifest because the script then keeps working
 *     under any future Vite version that reshapes its own
 *     `manifest.json`.
 *
 * Source maps (`.map`) and the SW itself (`sw.js`) are excluded
 * — the maps are dev-aid artefacts and caching the SW would
 * defeat the browser's own SW update fetch path.
 *
 * Zero-dep Node stdlib only, by design (mirrors
 * `bundle-report.mjs`'s policy from 1.16f1).
 */
import { createHash } from "node:crypto";
import {
  copyFileSync,
  existsSync,
  mkdirSync,
  readdirSync,
  readFileSync,
  writeFileSync,
} from "node:fs";
import { join, relative } from "node:path";
import { fileURLToPath } from "node:url";

const HERE = fileURLToPath(new URL(".", import.meta.url));
const DIST = join(HERE, "..", "dist");
const SW_OUT = join(DIST, "sw.js");

/** wasm-pack output (`npm run build:wasm`) — the SW loads a copy of
 *  this for the 1.21 silent background decrypt. */
const WASM_SRC_DIR = join(HERE, "..", "wasm");
/** Stable, non-hashed dist path the module SW dynamic-`import()`s.
 *  Deliberately OUTSIDE the precache (excluded below) so a cold
 *  install never downloads the ~11 MB wasm — it is fetched on demand
 *  only when a push actually needs a background decrypt. */
const SW_WASM_DIR = join(DIST, "sw-wasm");
const SW_WASM_FILES = ["lakko_ffi_wasm.js", "lakko_ffi_wasm_bg.wasm"];

/** Top-level paths we must precache so an offline cold-start
 *  works. The HTML shell is the entry point; everything else is
 *  computed from the asset directory walk below. */
const ROOT_ASSETS = ["/", "/index.html", "/manifest.webmanifest"];

/** Files we never count toward the precache (dev-aid / SW
 *  surface itself). */
const EXCLUDE_RE = /\.map$|^sw\.js$|^manifest\.webmanifest$|^robots\.txt$/;

function walk(dir) {
  const out = [];
  for (const entry of readdirSync(dir, { withFileTypes: true })) {
    const path = join(dir, entry.name);
    if (entry.isDirectory()) out.push(...walk(path));
    else if (entry.isFile()) out.push(path);
  }
  return out;
}

/** Copy the wasm-pack output into `dist/sw-wasm/` for the SW's
 *  on-demand background decrypt. Fatal if the wasm build is missing —
 *  the SW's `import()` would 404 at runtime and every push would fall
 *  back to generic, so fail loudly at build time instead. */
function copySwWasm() {
  mkdirSync(SW_WASM_DIR, { recursive: true });
  for (const file of SW_WASM_FILES) {
    const src = join(WASM_SRC_DIR, file);
    if (!existsSync(src)) {
      console.error(
        `build-sw: ${src} missing — run \`npm run build:wasm\` before the SW build`,
      );
      process.exit(1);
    }
    copyFileSync(src, join(SW_WASM_DIR, file));
  }
  console.log(`build-sw: copied ${SW_WASM_FILES.length} wasm file(s) → dist/sw-wasm/`);
}

function main() {
  let allFiles;
  try {
    allFiles = walk(DIST);
  } catch (err) {
    console.error(
      `build-sw: cannot read dist/ — did you run \`npm run build\` first?`,
    );
    console.error(err.message);
    process.exit(1);
  }

  // Copy the wasm-pack output into `dist/sw-wasm/` so the module SW
  // can dynamic-`import()` it for the silent background decrypt. Done
  // BEFORE the precache walk so the exclusion below can keep it out of
  // the install manifest (it is fetched on demand, never precached).
  copySwWasm();

  // Compute the precache list. We turn absolute file paths into
  // root-relative URLs (`/assets/foo.js`) — that's the shape `fetch`
  // sees when the page asks for an asset.
  const precache = new Set(ROOT_ASSETS);
  for (const path of allFiles) {
    const rel = relative(DIST, path);
    if (EXCLUDE_RE.test(rel) || EXCLUDE_RE.test(rel.split("/").pop())) continue;
    // The SW's on-demand wasm copy must NOT be precached — an 11 MB
    // blob on every cold install (even for users who never get a
    // push) would be a footgun, and it is loaded lazily via a
    // separate dynamic import anyway.
    if (rel === "sw-wasm" || rel.startsWith("sw-wasm/") || rel.startsWith("sw-wasm\\")) {
      continue;
    }
    // Link preview pages and their picture (scripts/link-previews.mjs) are
    // for the fetchers of other apps, never for the app offline.
    if (rel.startsWith("link-preview/") || rel.startsWith("link-preview\\")) continue;
    if (/^assets[\\/]link-preview-[0-9a-f]+\.png$/.test(rel)) continue;
    if (rel === "index.html") continue; // already covered by ROOT_ASSETS
    precache.add("/" + rel);
  }
  const manifest = [...precache].sort();

  // Build version: short prefix of a SHA-256 over the concatenated
  // bytes of every precached asset, in stable order. Independent of
  // wall-clock time so a deterministic rebuild produces the same
  // version (matches the workspace's reproducible-build posture from
  // ROADMAP §8).
  const hasher = createHash("sha256");
  for (const url of manifest) {
    if (url === "/") continue; // alias of /index.html
    const file = url === "/index.html"
      ? join(DIST, "index.html")
      : join(DIST, url.slice(1));
    try {
      hasher.update(readFileSync(file));
    } catch (err) {
      console.error(`build-sw: cannot hash ${file}: ${err.message}`);
      process.exit(1);
    }
  }
  const buildVersion = hasher.digest("hex").slice(0, 12);

  // Read the SW template Vite copied from public/, swap the
  // placeholders, write back.
  let sw;
  try {
    sw = readFileSync(SW_OUT, "utf8");
  } catch (err) {
    console.error(
      `build-sw: ${SW_OUT} missing — did Vite's public/ copy run?`,
    );
    console.error(err.message);
    process.exit(1);
  }

  const beforeVersion = sw.includes("__BUILD_VERSION__");
  const beforeManifest = sw.includes("__PRECACHE_MANIFEST__");
  if (!beforeVersion || !beforeManifest) {
    console.error(
      "build-sw: sw.js is missing one or both placeholders — has it already been processed?",
    );
    process.exit(1);
  }

  sw = sw
    .replace(/__BUILD_VERSION__/g, buildVersion)
    .replace("__PRECACHE_MANIFEST__", JSON.stringify(manifest, null, 2));

  writeFileSync(SW_OUT, sw, "utf8");

  console.log(`build-sw: BUILD_VERSION=${buildVersion}`);
  console.log(`build-sw: ${manifest.length} asset(s) in precache manifest`);
  for (const url of manifest) console.log(`  ${url}`);
}

main();
