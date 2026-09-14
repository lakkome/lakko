// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * ADR-0047 §3 — regenerate the bundled emoji picker data.
 *
 * The picker never fetches emoji at runtime (UX spec §4). Its data is a
 * trimmed JSON file committed at `src/media/emoji/emoji-data.json`, derived
 * from two pinned Unicode sources and nothing else:
 *
 *   * Unicode Emoji 15.1 `emoji-test.txt` — the RGI set, CLDR order,
 *     groups, CLDR short names, and the single-tone skin-tone variants.
 *   * CLDR 45 English annotations (the `cldr-json` release tag) — search
 *     keywords. CLDR 45 is the CLDR release that ships Emoji 15.1.
 *
 * Both are published under the Unicode License v3; the generated file is a
 * derivative and is annotated `Unicode-3.0` in `REUSE.toml`.
 *
 * Every download is checked against a pinned SHA-256, so a regeneration can
 * only ever produce data from exactly these bytes. Emoji 15.1 rather than a
 * newer release is deliberate: the picker renders with the platform emoji font
 * (UX spec §4), and a newer set shows empty boxes on systems whose font lags.
 *
 * Usage:
 *   node scripts/gen-emoji-data.mjs              # download + verify + write
 *   node scripts/gen-emoji-data.mjs --from DIR   # read DIR/emoji-test.txt and
 *                                                # DIR/annotations.json instead
 *
 * Not part of `npm run build`: the output is committed, so a build never
 * touches the network.
 */
import { createHash } from "node:crypto";
import { readFileSync, writeFileSync } from "node:fs";
import { join } from "node:path";
import { fileURLToPath } from "node:url";

const HERE = fileURLToPath(new URL(".", import.meta.url));
const OUT = join(HERE, "..", "src", "media", "emoji", "emoji-data.json");

const SOURCES = {
  emojiTest: {
    file: "emoji-test.txt",
    url: "https://www.unicode.org/Public/emoji/15.1/emoji-test.txt",
    sha256: "d876ee249aa28eaa76cfa6dfaa702847a8d13b062aa488d465d0395ee8137ed9",
  },
  annotations: {
    file: "annotations.json",
    url: "https://raw.githubusercontent.com/unicode-org/cldr-json/45.0.0/cldr-json/cldr-annotations-full/annotations/en/annotations.json",
    sha256: "a9554de7f8fe295aeb4b4ca615ec787e0cf74c017e8027f96a45e9ed2ff87366",
  },
};

const TONES = ["light", "medium-light", "medium", "medium-dark", "dark"];
/** Groups the picker does not show: skin-tone and hair components are not
 *  emoji a user sends on their own. */
const SKIPPED_GROUPS = new Set(["Component"]);

async function load(source, fromDir) {
  let bytes;
  if (fromDir !== null) {
    bytes = readFileSync(join(fromDir, source.file));
  } else {
    const res = await fetch(source.url);
    if (!res.ok) throw new Error(`gen-emoji-data: ${source.url} -> ${res.status}`);
    bytes = Buffer.from(await res.arrayBuffer());
  }
  const digest = createHash("sha256").update(bytes).digest("hex");
  if (digest !== source.sha256) {
    throw new Error(
      `gen-emoji-data: ${source.file} sha256 ${digest} does not match the pinned ${source.sha256}`,
    );
  }
  return bytes.toString("utf8");
}

function parseEmojiTest(text) {
  const groups = [];
  const entries = [];
  const byName = new Map();
  let group = null;
  for (const line of text.split("\n")) {
    const g = /^# group: (.+)$/.exec(line);
    if (g !== null) {
      group = g[1].trim();
      if (!SKIPPED_GROUPS.has(group)) groups.push(group);
      continue;
    }
    const m = /^([0-9A-F ]+?)\s*;\s*fully-qualified\s*#\s*(\S+)\s+E\d+\.\d+\s+(.+)$/.exec(line);
    if (m === null || group === null || SKIPPED_GROUPS.has(group)) continue;
    const codepoints = m[1].trim().split(/\s+/).map((h) => parseInt(h, 16));
    const emoji = String.fromCodePoint(...codepoints);
    const name = m[3].trim();
    const tone = /^(.+): (light|medium-light|medium|medium-dark|dark) skin tone$/.exec(name);
    if (tone !== null) {
      // A single-tone variant of an emoji listed just above it. Multi-tone
      // variants ("handshake: light skin tone, dark skin tone") do not match
      // this shape and are left out: one tone per emoji is what the picker
      // offers.
      const base = byName.get(tone[1]);
      if (base !== undefined) {
        base.tones ??= new Array(TONES.length).fill(null);
        base.tones[TONES.indexOf(tone[2])] = emoji;
      }
      continue;
    }
    if (name.includes("skin tone")) continue;
    const entry = { emoji, name, group: groups.length - 1, tones: undefined };
    entries.push(entry);
    byName.set(name, entry);
  }
  return { groups, entries };
}

function keywordsFor(annotations, emoji, name) {
  const table = annotations.annotations.annotations;
  const hit = table[emoji] ?? table[emoji.replace(/️/g, "")];
  if (hit === undefined || !Array.isArray(hit.default)) return "";
  const lowerName = name.toLowerCase();
  const words = [];
  for (const k of hit.default) {
    const lower = String(k).toLowerCase();
    if (lower === lowerName || words.includes(lower)) continue;
    words.push(lower);
  }
  return words.join("|");
}

async function main() {
  const fromIdx = process.argv.indexOf("--from");
  const fromDir = fromIdx === -1 ? null : process.argv[fromIdx + 1] ?? null;
  const emojiTest = await load(SOURCES.emojiTest, fromDir);
  const annotations = JSON.parse(await load(SOURCES.annotations, fromDir));
  const { groups, entries } = parseEmojiTest(emojiTest);

  // Compact rows: [emoji, name, keywords, groupIndex] or, for an emoji with a
  // complete set of five single-tone variants, [..., tones[5]].
  const rows = entries.map((e) => {
    const row = [e.emoji, e.name, keywordsFor(annotations, e.emoji, e.name), e.group];
    if (e.tones !== undefined && e.tones.every((t) => t !== null)) row.push(e.tones);
    return row;
  });
  const payload = {
    source: "Unicode Emoji 15.1 emoji-test.txt + CLDR 45 en annotations",
    license: "Unicode-3.0",
    groups,
    emoji: rows,
  };
  writeFileSync(OUT, `${JSON.stringify(payload)}\n`);
  process.stdout.write(
    `gen-emoji-data: wrote ${rows.length} emoji in ${groups.length} groups to ${OUT}\n`,
  );
}

await main();
