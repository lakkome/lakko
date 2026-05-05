<!--
SPDX-License-Identifier: AGPL-3.0-or-later
SPDX-FileCopyrightText: 2026 Lakko Contributors
-->

# Lakko — Web client (Phase 1.16b shell)

Solid.js + Vite + TypeScript app that loads the Rust core through
`lakko-ffi-wasm`. Phase 1.16b is a routing + UI shell: `/login`,
`/signup`, `/`, `/contacts`, `/chat/:id`, `/dev`. Auth is stubbed
in-memory (sessions reset on reload) — real OPAQUE-PAKE arrives
in 1.16c, contacts in 1.16d, 1×1 chat in 1.16e.

The original "Hello core" probe from 1.16a moved to `/dev` and
stays as a smoke surface that verifies the WASM bridge end-to-end
without going through the auth UI.

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
├── index.html               — SPA entry, security meta
├── package.json             — npm config + scripts (Solid + Vite + wasm plugin)
├── tsconfig.json            — strict TS, Solid JSX import source
├── vite.config.ts           — solid + wasm plugins, ES2022 target
├── wasm/                    — wasm-pack output (gitignored)
├── dist/                    — Vite build output (gitignored)
└── src/
    ├── main.tsx             — Solid render() entry, mounts App
    ├── App.tsx              — <AuthProvider> + <Router> root
    ├── styles.css           — global stylesheet (light theme primitives)
    ├── wasm.ts              — typed loadWasm() wrapper, idempotent init
    ├── auth/
    │   ├── AuthContext.tsx  — useAuth() / <AuthProvider>; stubbed in 1.16b
    │   └── guards.tsx       — <RequireSession> / <RequireAnonymous>
    ├── routes/
    │   ├── paths.ts         — typed URL helpers (single source of truth)
    │   ├── routes.tsx       — route table consumed by <Router>
    │   ├── Login.tsx        — /login (stub form, real OPAQUE in 1.16c)
    │   ├── Signup.tsx       — /signup (stub form)
    │   ├── Home.tsx         — / authenticated landing
    │   ├── Contacts.tsx     — /contacts placeholder (real flow in 1.16d)
    │   ├── Chat.tsx         — /chat/:id placeholder (real flow in 1.16e)
    │   ├── DevProbe.tsx     — /dev — original 1.16a "Hello core" probe
    │   └── NotFound.tsx     — catch-all 404
    └── ui/
        └── AppShell.tsx     — sidebar + main layout for protected routes
```

## Routing model

- Public auth routes (`/login`, `/signup`) are wrapped in
  `<RequireAnonymous>` — a signed-in user gets bounced to `/`.
- Protected routes (`/`, `/contacts`, `/chat/:id`, `/dev`) live
  inside `<RequireSession>` and render through the `<AppShell>`
  layout. An anonymous user is redirected to `/login`.
- All URL strings come from `routes/paths.ts`; never hardcode a
  route literal in a component.
- The route table in `routes/routes.tsx` mirrors `paths.ts` 1:1 —
  add or rename routes in both files in the same commit.

## Auth model (Phase 1.16b)

- `useAuth()` returns `{ state, signup, login, logout }`.
- `state` is a discriminated union: `loading` / `anon` /
  `session(account)`.
- `signup(...)` and `login(...)` are placeholders — they flip
  state to a fake session in-memory. Sessions do not persist
  across reloads.
- 1.16c will replace the placeholders with real OPAQUE-PAKE calls
  through `lakko-ffi-wasm` and add IndexedDB persistence.

## Notes for AI agents and contributors

- **Do not bake in 1.16c+ scope.** The auth stub is intentionally
  trivial; do not wire OPAQUE / fetch / IndexedDB into 1.16b.
- **`src/wasm.ts` is the only place** that calls `init()` from
  `lakko_ffi_wasm`. Other code calls `loadWasm()` and gets the
  cached promise.
- **No `!` non-null assertions** in TS. Strict null checks are on;
  use `if (x === null)` and throw with a clear message at the
  source.
- **The wasm-pack output path is `apps/web/wasm/`.** Vite picks it
  up via `vite-plugin-wasm`; do not import directly from
  `crates/lakko-ffi-wasm/pkg`.
- **No emoji in code or docs.** House style; CLAUDE.md.
- **Browser baseline:** evergreen Chromium / Firefox / Safari with
  WebAssembly + ES2022. We do not target legacy browsers.
