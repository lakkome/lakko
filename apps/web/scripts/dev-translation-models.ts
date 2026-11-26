// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * Dev server only (ADR-0100): serves `/translation-models/<sha256>` from the
 * directory `LAKKO_TRANSLATION_MODELS_DIR` names (the `build` output of
 * `tools/translation-models`), with byte ranges, as production nginx does.
 * Unset: the path is not served and Lakko's engine fails to download, as it
 * would against a server without the files.
 */

import { createReadStream, statSync } from "node:fs";
import { join } from "node:path";

import type { Plugin } from "vite";

export function devTranslationModels(): Plugin {
  return {
    name: "lakko-dev-translation-models",
    apply: "serve",
    configureServer(server) {
      const dir = process.env.LAKKO_TRANSLATION_MODELS_DIR;
      server.middlewares.use((req, res, next) => {
        const m = /^\/translation-models\/([0-9a-f]{64})$/.exec(req.url ?? "");
        if (m === null) return next();
        if (dir === undefined || dir.length === 0 || (req.method !== "GET" && req.method !== "HEAD")) {
          res.statusCode = 404;
          res.end();
          return;
        }
        const path = join(dir, m[1] ?? "");
        let size: number;
        try {
          size = statSync(path).size;
        } catch {
          res.statusCode = 404;
          res.end();
          return;
        }
        res.setHeader("Content-Type", "application/octet-stream");
        res.setHeader("Accept-Ranges", "bytes");
        const range = /^bytes=(\d+)-(\d+)$/.exec(req.headers.range ?? "");
        let first = 0;
        let last = size - 1;
        if (range !== null) {
          first = Number(range[1]);
          last = Math.min(Number(range[2]), size - 1);
          if (first > last) {
            res.statusCode = 416;
            res.end();
            return;
          }
          res.statusCode = 206;
          res.setHeader("Content-Range", `bytes ${first}-${last}/${size}`);
        }
        res.setHeader("Content-Length", String(last - first + 1));
        if (req.method === "HEAD") {
          res.end();
          return;
        }
        createReadStream(path, { start: first, end: last }).pipe(res);
      });
    },
  };
}
