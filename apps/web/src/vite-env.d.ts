// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/// <reference types="vite/client" />

/** The `version` field of `package.json`, injected by `define` in
 *  `vite.config.ts`. Undefined outside a Vite build (the vitest `node`
 *  environment), so read it through `typeof` first. */
declare const __LAKKO_PKG_VERSION__: string | undefined;
