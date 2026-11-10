// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * Post-build security pass for `apps/web/dist/index.html`
 * (Phase 1.17a). Two responsibilities, both deterministic and
 * dep-free:
 *
 *   1. **Subresource-Integrity hashes.** Vite emits
 *      `<script type="module" src="/assets/...js">`,
 *      `<link rel="modulepreload" href="/assets/...js">` and
 *      `<link rel="stylesheet" href="/assets/...css">` with the
 *      `crossorigin` attribute already in place; we add an
 *      `integrity="sha384-<base64>"` attribute referencing the
 *      file content. Browsers verify the hash before executing /
 *      applying the resource, defeating CDN tampering and most
 *      classes of bytewise MITM (TLS already covers wire MITM).
 *
 *   2. **Strict Content-Security-Policy meta tag.** Injected as
 *      a fallback for browsers that ignore the HTTP header (none
 *      of our production targets do, but defense-in-depth). The
 *      same policy lives in `nginx.conf.example` for the real
 *      header — that copy is the source of truth for the
 *      directives the meta tag can't carry (`frame-ancestors`,
 *      `report-uri`).
 *
 * The script runs BEFORE `build-sw.mjs` so the SW precaches the
 * post-SRI / post-CSP HTML; that way a network-first navigation
 * fetch landing on the cache fallback still gets the hardened
 * shell.
 *
 * Zero deps beyond `node:*`. Mirrors the policy of
 * `bundle-report.mjs` and `build-sw.mjs`.
 */
import { createHash } from "node:crypto";
import { existsSync, readFileSync, writeFileSync } from "node:fs";
import { join } from "node:path";
import { fileURLToPath } from "node:url";

const HERE = fileURLToPath(new URL(".", import.meta.url));
const DIST = join(HERE, "..", "dist");
const HTML = join(DIST, "index.html");
/** The desktop build's second page: the collapsed call's own window
 *  (`docs/ux/calls.md` section 4.5.4a). Absent from a browser build; given
 *  the same SRI and CSP when present. */
const CALL_WINDOW_HTML = join(DIST, "call-window.html");

/**
 * Production CSP. Kept in lock-step with `nginx.conf.example`
 * (which carries the directives a `<meta>` tag can't, like
 * `frame-ancestors`).
 *
 * Notes:
 *   * `'wasm-unsafe-eval'` is required — `WebAssembly.instantiate`
 *     counts as eval under CSP. The `'wasm-unsafe-eval'` keyword
 *     specifically permits WASM without permitting JS eval.
 *     Browsers: Chrome 95+ / Firefox 100+ / Safari 16+ — all
 *     within our deployment-target window.
 *   * `'unsafe-inline'` on `style-src` is the cost of Solid.js'
 *     inline `style={...}` props. Eliminating it requires moving
 *     every inline style to a class (or hashing each); not worth
 *     it in 1.17a. CSP-3 hashing arrives in a follow-up.
 *   * `connect-src 'self'` covers `/api/*` (auth / discovery /
 *     ds) — the Vite dev proxy makes them same-origin in dev,
 *     production nginx routes them locally. The two endpoints that
 *     are NOT same-origin — the SFU WebSocket (`VITE_LAKKO_SFU_URL`)
 *     and the transparency service (`VITE_LAKKO_TRANSPARENCY_URL`) —
 *     are appended as exact origins read from the same env the build
 *     inlined, so the meta allows precisely what this bundle dials
 *     and nothing else. Without the SFU origin every group call fails
 *     at the WebSocket open.
 *   * `img-src` / `media-src` carry `blob:` — decrypted attachments
 *     are previewed from `URL.createObjectURL` object URLs, which
 *     `'self'` does not match.
 */
const CROSS_ORIGIN_ENDPOINT_VARS = [
  "VITE_LAKKO_SFU_URL",
  "VITE_LAKKO_TRANSPARENCY_URL",
];

/** Read `VITE_*` values the way `vite build` (mode `production`) does:
 *  `process.env` wins, then `.env.production.local`, `.env.production`,
 *  `.env.local`, `.env`. A minimal `KEY=VALUE` reader — enough for the
 *  plain URLs these two variables hold — so the script stays dep-free. */
function readBuildEnv(names) {
  const files = [".env", ".env.local", ".env.production", ".env.production.local"];
  const fromFiles = {};
  for (const f of files) {
    const path = join(HERE, "..", f);
    if (!existsSync(path)) continue;
    for (const line of readFileSync(path, "utf8").split(/\r?\n/)) {
      const m = /^\s*(?:export\s+)?([A-Za-z_][A-Za-z0-9_]*)\s*=\s*(.*?)\s*$/.exec(line);
      if (m === null) continue;
      let value = m[2];
      if (/^(["']).*\1$/.test(value)) value = value.slice(1, -1);
      fromFiles[m[1]] = value;
    }
  }
  const out = {};
  for (const name of names) {
    const value = process.env[name] ?? fromFiles[name];
    if (typeof value === "string" && value.trim().length > 0) out[name] = value.trim();
  }
  return out;
}

/** The exact origins (`wss://host[:port]`, `https://host[:port]`) of the
 *  configured cross-origin endpoints. A malformed value fails the build: a
 *  CSP that silently omitted it would ship a client whose calls cannot open. */
function crossOriginEndpoints() {
  const env = readBuildEnv(CROSS_ORIGIN_ENDPOINT_VARS);
  const origins = [];
  for (const [name, raw] of Object.entries(env)) {
    let origin;
    try {
      origin = new URL(raw).origin;
    } catch {
      throw new Error(`post-build: ${name}=${raw} is not an absolute URL`);
    }
    if (origin === "null") {
      throw new Error(`post-build: ${name}=${raw} has no origin`);
    }
    if (!origins.includes(origin)) origins.push(origin);
  }
  return origins;
}

const CONNECT_ORIGINS = crossOriginEndpoints();

const CSP = [
  "default-src 'self'",
  "script-src 'self' 'wasm-unsafe-eval'",
  "style-src 'self' 'unsafe-inline'",
  "img-src 'self' data: blob:",
  "media-src 'self' blob:",
  "font-src 'self'",
  ["connect-src 'self'", ...CONNECT_ORIGINS].join(" "),
  "worker-src 'self'",
  "manifest-src 'self'",
  "base-uri 'self'",
  "form-action 'self'",
  "object-src 'none'",
].join("; ");

/** The production header copy of the CSP. Both policies apply to a page
 *  served with the header AND the meta, so an origin missing from the header
 *  blocks the connection whatever the meta says. */
const NGINX_CONF = join(HERE, "..", "..", "..", "infra", "nginx", "sites-available", "lakko.conf");

/** Warn when an origin this build dials is absent from the nginx header CSP.
 *  A warning, not a failure: a local build legitimately points at a local
 *  `ws://` SFU that production nginx must not allow. */
function warnOnHeaderDrift() {
  if (!existsSync(NGINX_CONF)) return;
  const conf = readFileSync(NGINX_CONF, "utf8");
  for (const origin of CONNECT_ORIGINS) {
    if (!conf.includes(origin)) {
      console.warn(
        `post-build: WARNING ${origin} is in this build's connect-src but not in ` +
          "infra/nginx/sites-available/lakko.conf — the production header CSP " +
          "will block it. Add it there before deploying this build.",
      );
    }
  }
}

/** Compute the SRI tag for a file's bytes. SHA-384 is the
 *  recommended SRI strength — SHA-256 is permitted but weaker,
 *  SHA-512 is overkill. The base64 encoding is the standard
 *  per the SRI spec (W3C Subresource Integrity §3.5). */
function sriFor(bytes) {
  const digest = createHash("sha384").update(bytes).digest("base64");
  return `sha384-${digest}`;
}

/** Add an `integrity="..."` attribute to the first matching tag.
 *  Idempotent: running the script twice on the same HTML keeps
 *  the file as-is past the first run.
 *
 *  We do not use a full HTML parser — Vite emits a stable shape
 *  (single space between attrs, `crossorigin` always present)
 *  and a regex on that shape is robust enough. A future Vite
 *  reshape that breaks this assumption surfaces as the script
 *  failing loud (`No matching tag` error), not as a silent
 *  miss. */
function injectIntegrity(html, attrName, attrValue, integrity) {
  // Find a tag like `<script ... ${attrName}="${attrValue}" ...>` or
  // `<link ... ${attrName}="${attrValue}" ...>`. The non-greedy
  // body match `[^>]*?` stops at the first `>`, keeping the
  // single-tag scope.
  const escaped = attrValue.replace(/[.*+?^${}()|[\]\\]/g, "\\$&");
  const pattern = new RegExp(
    `(<(?:script|link)[^>]*?${attrName}="${escaped}"[^>]*?)(\\s*/?>)`,
    "g",
  );

  // Track separately:
  //   * `matched`  — the tag was found at all (the only failure mode
  //     we want to throw on);
  //   * `injected` — we actually mutated the HTML (informational).
  // A re-run on an already-SRI'd HTML hits `matched=true,
  // injected=false` and silently no-ops — keeps the script
  // idempotent so callers can chain without state-tracking.
  let matched = false;
  let injected = false;
  const next = html.replace(pattern, (match, head, tail) => {
    matched = true;
    if (head.includes(' integrity="')) return match;
    injected = true;
    return `${head} integrity="${integrity}"${tail}`;
  });

  if (!matched) {
    throw new Error(
      `post-build: no matching ${attrName}="${attrValue}" tag in dist/index.html`,
    );
  }
  return { html: next, injected };
}

/** Inject the CSP meta right after the `X-Content-Type-Options`
 *  meta tag — keeps the security headers grouped in one block.
 *  Idempotent: a second run replaces the existing CSP meta
 *  in-place rather than duplicating it. */
function injectCsp(html, csp) {
  const cspMeta = `<meta http-equiv="Content-Security-Policy" content="${csp}" />`;

  if (html.includes('http-equiv="Content-Security-Policy"')) {
    // Replace the existing meta. Greedy until the next `>` so we
    // catch single-line and self-closing variants.
    return html.replace(
      /<meta\s+http-equiv="Content-Security-Policy"[^>]*?\/?>/,
      cspMeta,
    );
  }

  // Insert after X-Content-Type-Options. Vite preserves the
  // source order of meta tags in dist/index.html, so the anchor
  // is stable.
  const anchor = '<meta http-equiv="X-Content-Type-Options" content="nosniff" />';
  if (!html.includes(anchor)) {
    throw new Error(
      "post-build: anchor `X-Content-Type-Options` meta missing — did the source HTML drift?",
    );
  }
  return html.replace(anchor, `${anchor}\n    ${cspMeta}`);
}

function main() {
  processPage(HTML);
  if (existsSync(CALL_WINDOW_HTML)) processPage(CALL_WINDOW_HTML);
  warnOnHeaderDrift();
}

function processPage(HTML) {
  let html;
  try {
    html = readFileSync(HTML, "utf8");
  } catch (err) {
    console.error(`post-build: cannot read ${HTML} — did \`vite build\` run?`);
    console.error(err.message);
    process.exit(1);
  }

  // 1. Pull every /assets/* reference out of the HTML and compute
  //    its SRI hash. We match `<script src="/assets/...">` and
  //    `<link href="/assets/...">`.
  const refs = [
    ...html.matchAll(
      /<(?:script|link)[^>]*?(?:src|href)="(\/assets\/[^"]+)"/g,
    ),
  ].map((m) => m[1]);

  if (refs.length === 0) {
    throw new Error(
      "post-build: no /assets/* references in dist/index.html — was Vite's bundle a single inline script?",
    );
  }

  const integrities = new Map();
  for (const ref of refs) {
    const file = join(DIST, ref.slice(1)); // strip leading `/`
    let bytes;
    try {
      bytes = readFileSync(file);
    } catch (err) {
      console.error(`post-build: cannot read referenced asset ${file}: ${err.message}`);
      process.exit(1);
    }
    integrities.set(ref, sriFor(bytes));
  }

  // 2. Inject `integrity` next to each `src` / `href` we just
  //    hashed. Use whichever attribute the tag uses (script→src,
  //    link→href).
  let injectedCount = 0;
  for (const [ref, integrity] of integrities) {
    const usesSrc = new RegExp(`<script[^>]*?src="${ref.replace(/[.*+?^${}()|[\]\\]/g, "\\$&")}"`).test(html);
    const result = injectIntegrity(html, usesSrc ? "src" : "href", ref, integrity);
    html = result.html;
    if (result.injected) injectedCount += 1;
  }

  // 3. Inject the CSP meta.
  html = injectCsp(html, CSP);

  writeFileSync(HTML, html, "utf8");

  if (injectedCount === 0) {
    console.log("post-build: SRI + CSP already in place (idempotent re-run)");
  } else {
    console.log(`post-build: SRI + CSP injected (${injectedCount} new SRI tag(s))`);
  }
  console.log(`  page: ${HTML}`);
  console.log(`  csp:  ${CSP}`);
  for (const [ref, integrity] of integrities) {
    console.log(`  sri:  ${ref}  ${integrity.slice(0, 24)}…`);
  }
}

main();
