<!-- SPDX-License-Identifier: AGPL-3.0-or-later -->
<!-- SPDX-FileCopyrightText: 2026 Lakko Contributors -->

# translation-models

Mirrors Mozilla's Firefox Translations models for Lakko's own translation
engine in the web client and the desktop app
([ADR-0100](../../docs/adr/0100-lakko-translation-engine-web.md)).

The operator runs it on their own machine. A client never runs it and never
contacts Mozilla: clients download the files this tool writes from Lakko's
server, at `/translation-models/<sha256>`, and check them against the
manifest in the web build.

## What it does

- Reads the Remote Settings collection `translations-models-v2` (the
  changeset at `firefox.settings.services.mozilla.com`).
- Keeps the records a desktop browser uses: no filter, or the filter
  `env.appinfo.OS != 'Android'`. Android-only and nightly-only records are
  left out.
- Per direction (a language to or from English), takes the highest version
  that has a complete file set (a model, a lexical shortlist, and one
  vocabulary or a source and a target vocabulary), with every file of that
  version. Versions compare the way Firefox compares them (`3.0a1` <
  `3.0` < `3.1`).
- Downloads each file from `firefox-settings-attachments.cdn.mozilla.net`
  and checks Mozilla's SHA-256 of the `.zst` and, after decompressing,
  Mozilla's `decompressedHash` and `decompressedSize`. Any mismatch stops
  the tool.
- Groups the languages three by three (the table in ADR-0100 section 4,
  `GROUPS` in the script) and writes one file per group: Mozilla's `.zst`
  files concatenated byte for byte, in a fixed order (the group's languages
  in table order; for each, from English before to English; within a
  direction model, lex, vocab, srcvocab, trgvocab). The file is named by its
  SHA-256.
- Writes the manifest `apps/web/src/translation/lakko-engine/models.json`:
  per group its file hash and size; per direction its group, architecture,
  version and, per file, the offset and length in the group file and the
  compressed and decompressed SHA-256 and size. Rebuilding from the same
  changeset gives the same bytes.

Standard library only; Python 3.14 or later (`compression.zstd`).

## Commands

```bash
# Download (about 2.5 GB; resumable, a cached file with the right hash is kept)
python3 -I tools/translation-models/translation_models.py fetch --cache CACHE

# Write the group files and the manifest
python3 -I tools/translation-models/translation_models.py build \
    --cache CACHE --out OUT \
    --manifest apps/web/src/translation/lakko-engine/models.json

# Re-check every group file and every model file in it
python3 -I tools/translation-models/translation_models.py verify \
    --manifest apps/web/src/translation/lakko-engine/models.json --dir OUT
```

Or through the justfile: `just translation-models-fetch CACHE`,
`just translation-models-build CACHE OUT` (build, then verify).

`CACHE` holds `changeset.json` and `files/<sha256>.zst`; keep it outside the
repository. `OUT` holds only the group files.

## Where the files go

Every file in `OUT` goes to `/var/www/lakko-translation-models/` on the
server, which nginx serves at `/translation-models/<sha256>` (see
`infra/README.md`, "Translation models"). On the test stack:
`just mirror-translation-models-test OUT`. Copy the files before deploying a
web build whose manifest names them. Files are only added, never replaced
or deleted, so an older web build still finds its own.

The manifest is generated: commit it as the tool writes it, never by hand.
A model update is a new `fetch` + `build`, a mirror, and a web deploy.

## Licence

The model files are Mozilla's, under the MPL 2.0 (ADR-0100 section 2). The
group files are their unchanged bytes concatenated.
