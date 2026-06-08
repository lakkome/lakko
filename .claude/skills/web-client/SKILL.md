---
name: web-client
description: Background knowledge for Lakko web client development — TypeScript/SolidJS patterns, wasm-bindgen FFI, Vite build, SFrame web worker, WebRTC. Auto-applies when working on apps/web/.
user-invocable: false
---

# Lakko web client — domain knowledge

## Project structure

- `apps/web/src/` — main app (SolidJS + TypeScript)
- `apps/web/wasm/` — wasm-bindgen output from `lakko-ffi-wasm` (gitignored, built by `npm run build:wasm`)
- `apps/web/vite.config.ts` — Vite config (includes `vite-plugin-wasm` for both main + worker bundles)
- `apps/web/package.json` — dependencies (SolidJS, @solidjs/router, qrcode, jsqr)

### Module layout inside `src/`

| Module | Purpose |
|---|---|
| `auth/` | OPAQUE-PAKE + WebAuthn passkey flows, AuthContext |
| `backup/` | Encrypted backup UI |
| `calls/` | WebRTC, SFrame web worker, CallSession, SfuCallSession |
| `channels/` | Channel broadcast UI + rotation-cron |
| `chat/` | Chat.tsx (main chat view), message handling |
| `contacts/` | Contact invite flows |
| `devices/` | Multi-device pairing, orchestrator, DeviceGroupSyncContext |
| `ds/` | MLS Delivery Service client |
| `discovery/` | Username lookup (OPRF) |
| `push/` | Service worker push |
| `recovery/` | Password recovery flow |
| `routes/` | Route definitions (Call.tsx, etc.) |
| `state/` | Shared store (SolidJS signals) |
| `storage/` | Ciphertext blob proxy |
| `sw/` | Service worker |
| `ui/` | Shared UI components |
| `wasm.ts` | Wasm module loader |

## FFI surface

- **wasm-bindgen** bindings from `lakko-ffi-wasm`
- Build: `npm run build:wasm` (release) or `npm run build:wasm:dev`
- Output lands in `apps/web/wasm/` — imported directly by TS code
- The wasm module must be rebuilt after any Rust `lakko-ffi-wasm` change

## SFrame web worker

- `apps/web/src/calls/sframe-transform.ts` — main-thread attach controller
  - `attachSenderSFrame(sender, seed, localLeaf)` — sets `RTCRtpScriptTransform` on sender
  - `attachReceiverSFrame(receiver, seed)` — sets `RTCRtpScriptTransform` on receiver
  - `isSFrameTransformSupported()` — feature gate (invariant #4: THROWS if absent)
- `apps/web/src/calls/sframe-transform-worker.ts` — off-thread engine (module worker)
  - Loads wasm core via `loadWasm`
  - Encrypt: `ParticipantKey.sealFramed` per frame
  - Decrypt: peek header KID via `unwrapSframeFrame`, lazy per-leaf key derivation, `openFramed`
  - Failed frames are dropped, never forwarded as plaintext
- AAD = SFrame wire header per ADR-0013 (cross-platform interop with native shims)
- `vite.config.ts` repeats `vite-plugin-wasm` under `worker.plugins` for the worker bundle

## WebRTC

- Native browser WebRTC API (no library wrapper)
- `apps/web/src/calls/call-session.ts` — 1x1 P2P CallSession
- `apps/web/src/calls/sfu-call-session.ts` — SFU multi-party SfuCallSession
- `apps/web/src/calls/sfu-signaling-client.ts` — WebSocket signaling (`lakko-sfu-v1` subprotocol)
- `apps/web/src/calls/sfu-call-ui.ts` — adapter binding SfuCallSession onto shared callActions store
- SFU wire frames parsed via wasm-bindgen helpers from `lakko-ffi-wasm::sfu_signaling`

## Build & verify

```bash
cd apps/web
npm run build:wasm:dev   # rebuild wasm
npm run dev              # start Vite dev server
tsc --noEmit             # type check (MUST pass before committing)
npm run build            # full production build (wasm + tsc + vite)
```

## Key patterns

- **SolidJS signals** for reactive state (not React hooks — no re-render on every state change)
- `createSignal`, `createEffect`, `createMemo` — SolidJS primitives
- `@solidjs/router` for routing
- `callActions` shared store for call UI state (phase, roster, E2EE, connection quality)
- `DeviceGroupSyncContext` — the receive loop for DeviceGroup journal (attestation unwrap + verify)
- `DeviceGroupJournalEnvelope` — `version || attestation_len || attestation || mls_message` wire format
- `DeviceAttestKey` with build-time pin via `VITE_LAKKO_DEVICE_ATTEST_KEY`
- `sfu-config.ts` reads SFU URL from `VITE_LAKKO_SFU_URL`
- `rotation-cron.ts` drives channel key rotation per C-54 policy

## Environment variables (Vite, .env)

| Variable | Purpose |
|---|---|
| `VITE_LAKKO_DEVICE_ATTEST_KEY` | Build-time pinned Ed25519 verifying key for device attestation |
| `VITE_LAKKO_SFU_URL` | SFU base URL for group calls |
| `VITE_LAKKO_API_URL` | Backend API base URL |

## Important: this is SolidJS, NOT React

- No `useState` / `useEffect` / `useRef` — use `createSignal` / `createEffect` / variable
- Components are called ONCE, not re-rendered — the JSX template is reactive via signals
- `Show`, `For`, `Switch/Match` are SolidJS control-flow components
- Props are accessed directly (not destructured in the function signature for reactivity)
- Do NOT import from `react` or `react-dom`
