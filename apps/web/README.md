<!--
SPDX-License-Identifier: AGPL-3.0-or-later
SPDX-FileCopyrightText: 2026 Lakko Contributors
-->

# Lakko — Web client (Phase 1.16a skeleton)

Minimum viable Solid.js + WASM scaffold that proves the JS&nbsp;↔&nbsp;WASM
bridge works end-to-end. Routing, signup / login, contacts, and chat
arrive in Phase 1.16b — this skeleton only confirms that the Rust
core (via `lakko-ffi-wasm`) loads in a browser and can produce an
`Identity` + a `KeyPackage`.

## Toolchain prerequisites

| Tool | Version | Install |
|---|---|---|
| Node.js | ≥ 20 LTS | https://nodejs.org/ or `nvm install 20` |
| npm | ≥ 10 | bundled with Node |
| Rust toolchain | stable (matches `rust-toolchain.toml`) | https://rustup.rs |
| `wasm32-unknown-unknown` target | — | `rustup target add wasm32-unknown-unknown` |
| `wasm-pack` | ≥ 0.13 | `cargo install wasm-pack` |

## First-time setup

```bash
cd apps/web
npm install
```

`npm install` does not build the WASM — that is a separate step
driven by `wasm-pack` against `crates/lakko-ffi-wasm`.

## Dev server

```bash
npm run dev
```

This script:
1. Runs `wasm-pack build` against `crates/lakko-ffi-wasm` in `--dev`
   mode, emitting `apps/web/wasm/`.
2. Starts the Vite dev server on `http://127.0.0.1:5173`.

Open the URL — you should see "Lakko web client" with a "Core probe"
section listing the identity public-key prefix, the KeyPackage size,
and the MLS group's member count after creation. If the WASM loader
fails the page renders the error inline.

## Production build

```bash
npm run build
```

This script:
1. Runs `wasm-pack build` against `crates/lakko-ffi-wasm` in
   `--release` mode (size-optimised wasm).
2. Runs `tsc --noEmit` for type-checking.
3. Runs `vite build`, writing to `apps/web/dist/`.

The Phase 1.16c bundle target is < 2 MB gzipped — measure with
`du -sh dist/` and `gzip -k dist/assets/*.js && du -sh dist/assets/*.gz`.
Optimisation work itself lands in 1.16c.

## Useful npm scripts

| Script | Purpose |
|---|---|
| `npm run dev` | Dev build of WASM + Vite dev server |
| `npm run build` | Release build (WASM + TS check + Vite production) |
| `npm run preview` | Serve `dist/` locally to verify the prod build |
| `npm run typecheck` | `tsc --noEmit` (no Vite, no WASM rebuild) |
| `npm run build:wasm` | Just `wasm-pack build --release` |
| `npm run build:wasm:dev` | Just `wasm-pack build --dev` |
| `npm run clean` | Remove `wasm/`, `dist/`, Vite cache |

## File layout

```
apps/web/
├── index.html             — single-page-app entry, security meta
├── package.json           — npm config + scripts (Solid + Vite + wasm plugin)
├── tsconfig.json          — strict TS, Solid JSX import source
├── vite.config.ts         — solid + wasm plugins, ES2022 target
├── public/                — static assets served as-is (currently empty)
├── wasm/                  — wasm-pack output (gitignored)
├── dist/                  — Vite build output (gitignored)
└── src/
    ├── main.tsx           — Solid render() entry, mounts App into #root
    ├── App.tsx            — Phase 1.16a "core probe" component
    └── wasm.ts            — typed `loadWasm()` wrapper, idempotent init
```

## Notes for AI agents and contributors

- **No router, no global state yet.** Phase 1.16a is "Hello, core" —
  do not introduce solid-router, signals stores, or service workers
  here. They land in 1.16b / 1.16c with the corresponding ADR or
  ROADMAP bullet.
- **`src/wasm.ts` is the only place** that calls `init()` from
  `lakko_ffi_wasm`. Other code calls `loadWasm()` and gets the
  cached promise.
- **No `unwrap()`-style `!` non-null assertions** in TS. Strict
  null checks are on; use `if (x === null)` and throw with a clear
  message at the source.
- **The wasm-pack output path is `apps/web/wasm/`.** Vite picks it up
  via `vite-plugin-wasm`; do not import directly from
  `crates/lakko-ffi-wasm/pkg`.
- **Browser baseline:** evergreen Chromium / Firefox / Safari with
  WebAssembly + ES2022. We do not target legacy browsers.
