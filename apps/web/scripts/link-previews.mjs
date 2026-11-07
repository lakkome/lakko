// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * Link preview tags for the web build.
 *
 * A Lakko link pasted into another app (Telegram, WhatsApp, iMessage, Slack,
 * X, Facebook, LinkedIn, Discord) is fetched by that app's server, which
 * reads the page's `<head>` and draws a card from the `og:` / `twitter:`
 * tags. The web client is a static page with no server-side rendering, so
 * this script writes those tags at build time:
 *
 *   * `dist/index.html` gets the home tags. nginx serves it for `/` and for
 *     every path no other rule names, so any other Lakko URL shows the home
 *     card.
 *   * `dist/link-preview/{i,g,c,feed}.html` are the same page (same
 *     scripts, same SRI and CSP) with the tags of one link kind. nginx
 *     serves them for `/i/...` (contact invite), `/g/...` (group invite; a
 *     channel invite rides the same path, and which of the two a link is
 *     sits in its sealed capability, so the copy covers both), `/c/...`
 *     (call link) and `/feed/import` (a shared feed, its rules after `#`).
 *     A person opening the link gets the same app as from index.html.
 *   * `apps/web/link-preview/lakko.png` (rendered by
 *     `swift tools/brand/render-link-preview.swift`) is copied to
 *     `dist/assets/link-preview-<hash>.png`, under the long-cache rule nginx
 *     already has for `/assets/`.
 *
 * Privacy: the tags name nothing from the link. The link secret is after
 * `#` and never reaches a server; the slug before it is not repeated
 * either. Each kind has one fixed text. `og:url` is set on the home tags
 * only: a link page cannot know its own path without the server writing the
 * request into the HTML, and every preview fetcher uses the URL it fetched
 * when `og:url` is absent.
 *
 * The image URL must be absolute, so the public origin is a build input:
 * `VITE_LAKKO_PUBLIC_ORIGIN` (read like the `VITE_*` variables in
 * `post-build.mjs`: the environment first, then the `.env*` files),
 * default `https://lakko.me`. The test stack's deploy sets it to its own
 * origin.
 *
 * Runs after `post-build.mjs` (so every copy carries the SRI hashes and the
 * CSP meta) and before `build-sw.mjs` (which leaves these files out of the
 * service worker's precache). Idempotent: the tags sit between two marker
 * comments and a re-run replaces them. Zero deps beyond `node:*`.
 */
import { createHash } from "node:crypto";
import { copyFileSync, existsSync, mkdirSync, readFileSync, writeFileSync } from "node:fs";
import { join } from "node:path";
import { fileURLToPath } from "node:url";

const HERE = fileURLToPath(new URL(".", import.meta.url));
const WEB = join(HERE, "..");
const DIST = join(WEB, "dist");
const HTML = join(DIST, "index.html");
const IMAGE_SRC = join(WEB, "link-preview", "lakko.png");
const IMAGE_WIDTH = 1200;
const IMAGE_HEIGHT = 630;
const VARIANT_DIR = join(DIST, "link-preview");

const ORIGIN_VAR = "VITE_LAKKO_PUBLIC_ORIGIN";
const DEFAULT_ORIGIN = "https://lakko.me";

const START = "<!-- link-preview:start -->";
const END = "<!-- link-preview:end -->";

const DESCRIPTION = "Private messages and calls, end-to-end encrypted.";

/** The copy per link kind. `file` is the variant's name in `dist/link-preview/`;
 *  `null` is the home page itself. */
const KINDS = [
  { file: null, title: "Lakko", description: DESCRIPTION },
  {
    file: "i.html",
    title: "Add me on Lakko",
    description:
      "Open the link to add me as a contact. Messages and calls on Lakko are end-to-end encrypted.",
  },
  {
    file: "g.html",
    title: "Join a group or channel on Lakko",
    description:
      "Open the link to see the invitation. Messages on Lakko are end-to-end encrypted.",
  },
  {
    file: "c.html",
    title: "Join a call on Lakko",
    description: "Open the link to join the call. Calls on Lakko are end-to-end encrypted.",
  },
  {
    file: "feed.html",
    title: "A feed for Lakko",
    description: "Open the link to add this feed to Lakko.",
  },
];

const IMAGE_ALT = "The Lakko logo";

/** Read one variable the way `vite build` (mode `production`) does:
 *  `process.env` wins, then `.env.production.local`, `.env.production`,
 *  `.env.local`, `.env`. Same minimal reader as `post-build.mjs`. */
function readBuildEnv(name) {
  const fromEnv = process.env[name];
  if (typeof fromEnv === "string" && fromEnv.trim().length > 0) return fromEnv.trim();
  let value;
  for (const f of [".env", ".env.local", ".env.production", ".env.production.local"]) {
    const path = join(WEB, f);
    if (!existsSync(path)) continue;
    for (const line of readFileSync(path, "utf8").split(/\r?\n/)) {
      const m = /^\s*(?:export\s+)?([A-Za-z_][A-Za-z0-9_]*)\s*=\s*(.*?)\s*$/.exec(line);
      if (m === null || m[1] !== name) continue;
      let v = m[2];
      if (/^(["']).*\1$/.test(v)) v = v.slice(1, -1);
      if (v.trim().length > 0) value = v.trim();
    }
  }
  return value;
}

/** The public origin, `https://host[:port]`. A malformed value fails the
 *  build: a preview card with a broken image URL fails silently in every
 *  app it is pasted into. Plain `http` only for a local host. */
function publicOrigin() {
  const raw = readBuildEnv(ORIGIN_VAR) ?? DEFAULT_ORIGIN;
  let url;
  try {
    url = new URL(raw);
  } catch {
    throw new Error(`link-previews: ${ORIGIN_VAR}=${raw} is not an absolute URL`);
  }
  const local = url.hostname === "localhost" || url.hostname === "127.0.0.1";
  if (url.protocol !== "https:" && !(url.protocol === "http:" && local)) {
    throw new Error(`link-previews: ${ORIGIN_VAR}=${raw} must be https`);
  }
  if (url.pathname !== "/" || url.search !== "" || url.hash !== "") {
    throw new Error(`link-previews: ${ORIGIN_VAR}=${raw} must be an origin with no path`);
  }
  return url.origin;
}

function escapeAttr(s) {
  return s
    .replace(/&/g, "&amp;")
    .replace(/"/g, "&quot;")
    .replace(/</g, "&lt;")
    .replace(/>/g, "&gt;");
}

function tags({ title, description, url, image }) {
  const meta = (attr, key, value) => `<meta ${attr}="${key}" content="${escapeAttr(value)}" />`;
  const lines = [
    meta("name", "description", description),
    meta("property", "og:type", "website"),
    meta("property", "og:site_name", "Lakko"),
    meta("property", "og:title", title),
    meta("property", "og:description", description),
  ];
  if (url !== null) lines.push(meta("property", "og:url", url));
  lines.push(
    meta("property", "og:image", image),
    meta("property", "og:image:type", "image/png"),
    meta("property", "og:image:width", String(IMAGE_WIDTH)),
    meta("property", "og:image:height", String(IMAGE_HEIGHT)),
    meta("property", "og:image:alt", IMAGE_ALT),
    meta("name", "twitter:card", "summary_large_image"),
    meta("name", "twitter:title", title),
    meta("name", "twitter:description", description),
    meta("name", "twitter:image", image),
    meta("name", "twitter:image:alt", IMAGE_ALT),
  );
  return lines;
}

function withTags(html, lines) {
  const start = html.indexOf(START);
  const end = html.indexOf(END);
  if (start === -1 || end === -1 || end < start) {
    throw new Error(
      "link-previews: the link-preview marker comments are missing from dist/index.html",
    );
  }
  // Keep the indentation of the start marker for every inserted line.
  const lineStart = html.lastIndexOf("\n", start) + 1;
  const indent = html.slice(lineStart, start);
  const block = [START, ...lines, END].join(`\n${indent}`);
  return html.slice(0, start) + block + html.slice(end + END.length);
}

function main() {
  if (!existsSync(HTML)) {
    console.error(`link-previews: cannot read ${HTML} — did \`vite build\` run?`);
    process.exit(1);
  }
  if (!existsSync(IMAGE_SRC)) {
    console.error(
      `link-previews: ${IMAGE_SRC} missing — run \`swift tools/brand/render-link-preview.swift\``,
    );
    process.exit(1);
  }
  const origin = publicOrigin();

  const imageBytes = readFileSync(IMAGE_SRC);
  const imageHash = createHash("sha256").update(imageBytes).digest("hex").slice(0, 12);
  const imageName = `link-preview-${imageHash}.png`;
  mkdirSync(join(DIST, "assets"), { recursive: true });
  copyFileSync(IMAGE_SRC, join(DIST, "assets", imageName));
  const image = `${origin}/assets/${imageName}`;

  const page = readFileSync(HTML, "utf8");
  mkdirSync(VARIANT_DIR, { recursive: true });
  for (const kind of KINDS) {
    const html = withTags(
      page,
      tags({
        title: kind.title,
        description: kind.description,
        url: kind.file === null ? `${origin}/` : null,
        image,
      }),
    );
    const out = kind.file === null ? HTML : join(VARIANT_DIR, kind.file);
    writeFileSync(out, html, "utf8");
    console.log(`link-previews: ${kind.file ?? "index.html"}  "${kind.title}"`);
  }
  console.log(`link-previews: origin ${origin}, image /assets/${imageName}`);
}

main();
