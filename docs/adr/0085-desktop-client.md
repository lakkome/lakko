<!-- SPDX-License-Identifier: AGPL-3.0-or-later -->
<!-- SPDX-FileCopyrightText: 2026 Lakko Contributors -->

# ADR-0085: The desktop client is a Tauri host that keeps the keys and talks to the network; the web client's code runs inside it

- **Status:** Proposed (2026-10-16). S1 spike built on macOS the same day (section 10); S2 built on macOS the same day for the web client and the host (section 11), the Secure Enclave path not yet exercised. The overall direction is the maintainer's
  (2026-10-16): Tauri 2 on macOS, Windows and Linux, the host does the system
  work and keeps the device key, the messaging logic and MLS stay in the web
  client's wasm inside the window (option A of section "Alternatives"). The
  five decisions of section 9 are the maintainer's too (2026-10-16). Beyond
  S1 and the macOS half of S2 nothing is built.
- **Date:** 2026-10-16
- **Amends:** [ADR-0002](0002-rust-core.md) (the desktop row of the
  per-platform UI table, and its open question "is Tauri 2.x mature enough").
- **Related:** [ADR-0026](0026-anti-censorship-wiring.md) (the desktop is one of
  the native clients that can arm the tunnel), [ADR-0038](0038-update-available-notice.md)
  (update notice), [ADR-0040](0040-durable-pending-commit-outbox.md) and
  [ADR-0077](0077-account-binding-and-store-gate.md) (unchanged, because their
  code keeps running where it runs today), [ADR-0070](0070-call-media-through-a-loss-burst.md)
  and [ADR-0071](0071-codec-header-in-the-clear.md) (call media in a webview).
- **UX spec:** [`docs/ux/desktop.md`](../ux/desktop.md).

## Context

ADR-0002 put Windows, macOS and Linux on "Tauri 2.x + Solid.js, native Rust
integration in the host process" and left open whether Tauri would hold up. No
desktop code exists: there is no `src-tauri`, no desktop build, nothing in
`apps/` but `android`, `ios`, `shared` and `web`.

A read of `apps/web` on 2026-10-16 (counted with `grep`, test files left out)
shows what "native Rust integration in the host process" would mean in
practice:

- The web client's messaging logic is TypeScript. wasm (`lakko-ffi-wasm`) is a
  library it calls: 97 files load it through `src/wasm.ts`, and they use about
  285 distinct exports (a lower bound). There is no layer between the screens
  and wasm: routes (`Chat.tsx`, `Channels.tsx`, `Invite.tsx`) and contexts
  (`AuthContext`, `ChatContext`, `DeviceGroupSyncContext`) call it directly.
- The MLS objects live in JavaScript. `Identity` and `Group` are held in Solid
  signals and passed to about fifteen modules; many of the calls are
  synchronous. The per-group lock (`mls/group-lock.ts`), the durable commit
  outbox (ADR-0040, `chat/commit-outbox.ts`, one IndexedDB transaction with the
  snapshot) and the ADR-0077 binding epoch and store gate are written in
  TypeScript around those objects.
- Call media is sealed per frame in a worker inside the page
  (`calls/sframe-worker-core.ts`), because WebRTC runs in the page. A frame
  cannot cross into another process per frame, so the call keys must be inside
  the webview whatever the host does.
- Every request to a Lakko server uses a relative path (`/api/auth`,
  `/api/ds`, ...), resolved against the page's origin; on the web nginx routes
  them. The SFU signaling is the only WebSocket.
- At rest, every local store is AES-GCM under one non-extractable WebCrypto
  `CryptoKey` per account, kept in IndexedDB `lakko-auth` (`auth/key-store.ts`)
  and passed around by about 85 non-test files (57 AES-GCM `encrypt` /
  `decrypt` call sites, all with a 12-byte IV and no additional data). The key
  is not random: `auth/device-key.ts` derives it with HKDF-SHA-512 from the
  OPAQUE export key, or from the account identity seed on a device restored
  without a password. On the web the browser decides how that key is
  protected on disk.

So the iOS and Android clients are not "UI over a Rust core" either: their
logic is Swift and Kotlin. Moving the logic into the host would be writing a
fourth client in Rust (alternative C); proxying every wasm call over IPC would
turn hundreds of synchronous call sites asynchronous and move none of the
disciplines (alternative B). What the desktop needs from the host is what a
browser cannot give: a device key held by the operating system, a network path
that can be tunnelled (ADR-0026), a process and notifications that outlive the
window, links from other apps, and updates.

## Decision

**The desktop client is a Tauri 2 app whose Rust host keeps the device key,
carries every request to Lakko's servers, and owns the window, tray,
notifications, links, autostart and updates; the web client's code, wasm and
MLS included, runs unchanged inside the window, switched to desktop mode by a
build flag.** The host never holds a message key, never decrypts a message and
never keeps MLS state.

### 1. Layout

- `apps/desktop/` holds the Tauri project: `src-tauri/` (the host crate, a
  Cargo workspace of its own like the fuzz crates, so `cargo check
  --workspace` never needs a platform webview's system libraries) and the
  configuration. It builds `apps/web` with
  `VITE_LAKKO_PLATFORM=desktop` and bundles the output. There is one copy of
  the screens.
- In desktop mode the web client does not register a service worker, does not
  subscribe to Web Push and does not show the narrow (drawer) layout. Every
  other difference goes through one module, `apps/web/src/platform/`, which
  answers "am I the desktop app" and exposes the host's commands. No screen
  imports Tauri directly.
- The host depends on the domain crates it needs (`lakko-transport` for the
  tunnel, `lakko-tuf` for updates), not on
  `lakko-ffi-uniffi`.

### 2. The device key lives in the operating system's key store

- **The host derives the key and the page never sees it (decision 9.1).** The
  page hands the host the derivation input it already has (the OPAQUE export
  key, or the identity seed) and the host runs the same HKDF-SHA-512 as
  `auth/device-key.ts` (salt `lakko-device-root-v1`, info
  `lakko/device-root/v1|<account>` or
  `lakko/device-root/from-identity-seed/v1|<account>`), so a desktop and a
  browser signed in to one account derive the same key; the host pins this
  against WebCrypto in a test. The host keeps derived and loaded keys in
  memory behind random handles for the life of the process and does the
  AES-256-GCM itself; the ciphertext is byte-identical to WebCrypto's
  (`ciphertext ‖ tag`, the caller keeps the IV where it keeps it today).
- **The host's routes are on the app scheme, not Tauri IPC commands:**
  `/__host/device-key/{derive,put,get,delete,encrypt,decrypt}`
  (`apps/desktop/src-tauri/src/device_key.rs`), reached with `fetch` from the
  page. This needs no Tauri JavaScript package in `apps/web` and no IPC
  permission set; anything that can run script in the page can call them,
  which is the same reach an IPC command would have.
- **The seam in the web client is `DeviceCipher`**
  (`apps/web/src/auth/device-cipher.ts`): `encrypt(iv, plaintext)` and
  `decrypt(iv, ciphertext)`, async. On the web it wraps the same
  non-extractable `CryptoKey` in the same IndexedDB record (the service
  worker reads that record and keeps working); on the desktop it holds a host
  handle. `auth/key-store.ts` and the two derive functions return it, and
  every device-key caller moves from `crypto.subtle` to it.
- **Where the key rests, per OS:**
  - macOS: the key is encrypted to a Secure Enclave P-256 key (ECIES,
    `kSecKeyAlgorithmECIESEncryptionCofactorVariableIVX963SHA256AESGCM`) and
    the result stored as a generic-password item per account
    (`src/key_store.rs`). **Measured 2026-10-16:** an unsigned binary can
    create an ephemeral Secure Enclave key and wrap and unwrap with it, but
    making the key permanent fails with `-34018` (errSecMissingEntitlement)
    in both the data-protection and the file keychain, so the Secure Enclave
    path needs the app signed with a keychain entitlement (decision 9.5,
    packaging in S5). Until then: a **release** build that cannot keep a
    Secure Enclave key refuses (`hardware-key-unavailable`), so a signed build
    with a broken entitlement cannot fall to software protection unnoticed;
    a **debug** build keeps the key itself in the login keychain and logs a
    warning, so development runs unsigned. The Secure Enclave success path is
    therefore not yet exercised.
  - Windows: wrapped by a TPM key through CNG (the Microsoft Platform Crypto
    Provider). Not built; the host refuses (`unsupported-os`).
  - Linux, and Windows without a usable TPM: the OS's software store (Secret
    Service; the DPAPI-protected user store), under the exception of decision
    9.2. Not built; the host refuses (`unsupported-os`).
- A store that is missing, locked or refusing answers `503` and the page shows
  the UX state of `desktop.md` 1.1: sign-in stops. Nothing falls back to a
  file or to IndexedDB (invariant #7, fail-closed).
- "Forget this device" deletes the key from the OS store and every handle the
  host holds, together with the webview's data folder.

### 3. Every request to a Lakko server goes through the host

- The host registers the app's own URI scheme and serves the bundled web build
  from it. A relative request the page makes to `/api/<service>/...` arrives at
  the same handler, which forwards it to the configured server over the host's
  HTTP client (the method, headers and body unchanged, the response streamed
  back). So no `fetch` call site in `apps/web` changes, there is no CORS (the
  page and its API share an origin), and long polls (`?wait=25`) are an ordinary
  slow response.
- When anti-censorship is armed, the same handler sends the request through
  `lakko-transport`'s `TunneledHttpClient` over the verified bootstrap config,
  exactly as the UniFFI `tunneled_http_request` does on iOS and Android
  (ADR-0026). Armed and the tunnel unavailable: the request fails, never goes
  direct (invariant #4).
- The SFU signaling WebSocket cannot ride a URI scheme. In desktop mode
  `calls/sfu-signaling-client.ts` opens it through a host command that returns
  a channel (direct or, when armed, `tunneled_websocket`).
- Call media (WebRTC UDP) goes from the webview directly, as on the native
  clients, which do not tunnel media either.
- **Links name the public origin, never the app scheme.** Every link the web
  client mints or recognises (`/i/`, `/c/`, `/g/`, a feed rule, a
  co-publisher link) used to take the page's own `location.origin`, which in
  the window is `lakkoapp://localhost` and opens nowhere (found in the S3
  walk, 2026-10-16). The host now sets the origin it forwards `/api/*` to as a
  read-only page global before any page script runs, and the web client takes
  every such origin from `publicOrigin()` in `platform/`; on the desktop
  without that global it throws rather than mint a link.
- Requests to anything that is not a Lakko server are refused by the handler.
  Links in messages open in the system browser, never inside the window.

### 4. The process outlives the window

- Closing the window hides it; the host and the webview keep running
  (`desktop.md` section 3). Quit ends both.
- Messages and calls while the window is hidden arrive through the web
  client's existing receive loop (`BackgroundReceiveContext`, the DS long
  poll), which keeps running in the hidden webview, and the web client asks
  the host to show the notification. **Unverified:** whether each OS's webview
  keeps JavaScript timers and network running in a hidden window. This is the
  first thing to measure (section 8, S1). If a webview is throttled, the host
  instead keeps one DS long poll per joined group open itself, using only the
  public journal id and the proof headers the page registers with it, and on a
  new row wakes the page to fetch and decrypt; the host still decrypts nothing
  and a hidden-and-throttled page shows the generic "New message".
- Unread badge, tray icon and menu, start at login, and the "keep running"
  setting are host features driven by the page through `platform/`.

### 5. Notifications and links

- The page asks the host to show a notification (title, body, conversation
  reference, call actions); the host shows it with the OS notification API and
  sends clicks and Accept / Decline back to the page. The text is what the page
  decrypted; the host only displays it.
- The app registers the `lakko://` scheme on all three OSes (decision 9.3).
  A link handed over by the OS at launch or while running is passed to the
  page, which shows the screen it shows on the web for the matching
  `https://lakko.me/i/`, `/c/` or `/g/` link. The web invite pages gain "Open
  in the app", which navigates to the `lakko://` form. The in-app "Open a link"
  (`desktop.md` 7) takes a pasted `https` link.
- **The link secret never rides a command line.** On Windows and Linux the OS
  starts the app with the URL as an argument, readable by the user's other
  processes. A `lakko://` link therefore carries only what is safe to expose
  (the kind and the ledger id); how the secret after `#` reaches the app is
  settled in S5, before the scheme ships. Until then the pasted link is the
  only route that carries the secret.

### 6. Calls

- The web call code runs as is: WebRTC from the webview, SFrame sealed in the
  page's worker through `RTCRtpScriptTransform` or `createEncodedStreams`
  (ADR-0071), and a webview with neither is refused by the existing
  `link-join-gate.ts` before anything is created.
- Which engine has which API is **unverified**: Windows uses WebView2
  (Chromium), macOS WKWebView, Linux WebKitGTK. S1 measures it. A platform
  whose webview cannot seal media has no calls on that platform until a
  separate ADR moves call media into the host (native libwebrtc, as on iOS and
  Android); this ADR does not.
- The ringtone, the call notification and the "end the call and quit?" question
  of `desktop.md` are host plus page work.

### 7. Updates

The app downloads, verifies and installs its own updates; it restarts only
when the person taps "Restart to update", never during a call.

- **Verification is `lakko-tuf` from the first version (decision 9.4):** the
  host fetches the TUF metadata and the target through its own HTTP client,
  verifies the chain against a root pinned in the binary (`verify_root_chain`,
  `verify_chain`, `verify_target_bytes`), and only a verified artifact reaches
  the installer. The update repository's keys are offline and separate from
  the ADR-0038 release-manifest key. Tauri's updater plugin is not used.
- **Installing is ours to write, per OS:** macOS replaces the `.app` bundle
  and keeps the Developer ID signature check; Windows runs the verified
  installer after the app exits (a running executable cannot replace itself);
  Linux replaces the AppImage file. A `.deb` installed through the package
  manager is updated by the package manager, not by the app, so the `.deb`
  build turns self-update off.
- The TUF repository layout and the signing tool are part of S5.

### 8. Plan

| Sprint | What | Done when |
|---|---|---|
| S1 | Spike, macOS first: a Tauri shell loading `apps/web` in desktop mode, the URI handler proxying `/api/*`, sign-in against the test stack. Measure on each OS's webview: hidden-window timers and long polls, `RTCRtpScriptTransform` / `createEncodedStreams`, WebCrypto non-extractable import | The measurements are written into this ADR; sections 4 and 6 are settled |
| S2 | `DeviceCipher` in the web client (every device-key caller moved, web behaviour unchanged); the host-side key and AES-GCM (section 2) on macOS, then Windows and Linux | Web tests unchanged; on the desktop sign-in, restart and Forget device work and a missing store stops sign-in |
| S3 | Window, close-is-not-quit, tray, badge, notifications, start at login, keyboard, menus | The flows of `desktop.md` 2-5 walked on macOS |
| S4 | Anti-censorship through the host, SFU signaling through the host | A message and a call with the tunnel armed against the test stack |
| S5 | `lakko://` links and how the secret reaches the app, files (drop, paste, save), the TUF repository and signing tool, per-OS install, packaging (signed and notarized `.dmg`; Windows and Linux builds) | Installable builds; an update applied from a TUF repository on each OS; Windows and Linux walked on real or virtual machines |

### 9. Decisions (maintainer, 2026-10-16)

1. **The device key never enters the webview.** The host does the AES-GCM
   (section 2). Chosen over handing the bytes to the page once per launch,
   which would have changed no caller; the cost accepted is the move of every
   device-key caller to the async `DeviceCipher` and an IPC round trip per
   local store read and write (not measured; S2 measures it).
2. **Hardware where the machine has it; otherwise the OS's software store,
   recorded as an exception to invariant #7.** macOS (Secure Enclave) and
   Windows with a TPM are hardware-backed. Every Linux in v1 (the Secret
   Service; a TPM path through `tpm2-tss` is a possible follow-up) and Windows
   without a usable TPM are not. The exception, for the invariant's text:
   *on a desktop computer without hardware key storage the device key is
   protected by the operating system's software key store; a machine with no
   key store at all is refused.* No UI says which case a computer is in.
3. **Links: the `lakko://` scheme on all three OSes plus "Open a link"
   inside the app** (section 5). Claiming `https://lakko.me` links is left for
   later: on Windows it needs a packaged (MSIX) app.
4. **Updates: `lakko-tuf` from the first version** (section 7). Chosen over
   Tauri's updater for threshold signing, key rotation and protection against
   rollback and freeze; the cost accepted is writing the fetch and the
   per-OS install ourselves.
5. **Code signing: an Apple Developer ID exists; no Windows Authenticode
   certificate.** macOS builds are signed and notarized, which section 2 needs
   for the Secure Enclave key. Windows builds are unsigned until a certificate
   exists, so SmartScreen warns that the publisher is unknown. Linux packages
   are not code-signed; the AppImage update is covered by TUF.

### 10. S1 measurements (2026-10-16, macOS only)

Measured with the spike in `apps/desktop/src-tauri` (Tauri 2.12.4, macOS
26.6.2, Apple silicon, a debug build run with `cargo run`, not bundled). The
probe page is `src-tauri/probe/probe.html`, started with
`LAKKO_DESKTOP_PROBE=1`; every report is logged by the host with its own
clock.

- **The app scheme** `lakkoapp://localhost` is a secure context in
  WKWebView. Non-extractable WebCrypto import plus an AES-GCM round trip,
  IndexedDB, module workers, WebSocket, WebAssembly and `navigator.locks` all
  work.
- **The `/api/*` forward works:** `GET /api/auth/health` through the handler
  answered 200 from `https://test.lakko.me`. The bundled `apps/web` build
  (`VITE_LAKKO_PLATFORM=desktop`) loads in full, wasm included, and shows the
  sign-in screen; the screen makes no request before the person acts.
  **Not walked:** a sign-in, which needs the person's own credentials on the
  test stack.
- **A hidden window keeps running (section 4), for the 120 s measured.** With
  the window hidden by the host, the page's one-second timer kept firing but
  throttled (59 ticks in 120 s, gaps of 1.5 to 2.5 s), and a loop of
  five-second slow requests through the handler ran without a break (23 in
  120 s). `document.visibilityState` reported `hidden`. Longer hidden periods
  (App Nap after minutes) are not measured.
- **Call media (section 6):** WKWebView has `RTCRtpScriptTransform`, and one
  attaches to an audio sender; `createEncodedStreams` is absent; an offer
  carries audio and video with Opus and VP8. `getUserMedia` exists but was not
  called, because an unbundled binary has no microphone usage description and
  macOS stops a process that reads the microphone without one. No call was
  placed.
- **One web-client difference found:** Cache Storage refuses a request URL in
  the app scheme, so the page could not store the localized notification
  words for the service worker. The desktop has no service worker, and the
  desktop build now skips it (`l10n/notification-strings.ts`).
- **Development note:** the macOS app cannot start inside Claude Code's Bash
  sandbox; it stalls in `dyld` before `main`. Run it outside the sandbox.

Not measured: Windows (WebView2) and Linux (WebKitGTK), a real call, a
hidden window for longer than two minutes.

### 11. S2 on macOS (2026-10-16)

- **Web client:** every device-key caller moved to `DeviceCipher` (about 85
  files, all 57 AES-GCM call sites; no key that turned out not to be the
  device key). On the web the IndexedDB record is unchanged. On the desktop
  the store gate (ADR-0077) is still asked in the page before the host call.
  `KeyStoreUnavailableError` from the host shows the refusal screen of
  `desktop.md` 1.1 (`auth/KeyStoreUnavailableScreen.tsx`, two catalog keys,
  the ten non-English ones machine-translated) and is no longer read as "no
  key" in the nine AuthContext paths that used to catch a `getDeviceKey`
  failure; several of those would otherwise have wiped or discarded state on
  a keychain outage. Try again repeats the page-load restore; after a
  sign-in or recovery step it only clears the screen, because keeping that
  step's arguments would keep a password, phrase or export key in memory
  without bound. `tsc --noEmit` clean, the full vitest suite 6092 passed.
- **Host:** `src/device_key.rs` (derive, put, get, delete, encrypt, decrypt;
  one handle per stored account key per process) and `src/key_store.rs`
  (section 2). Host tests: the HKDF output pinned against WebCrypto for both
  sources, the route round trip and tamper refusal, the handle cache; an
  ignored test does a real keychain save / load / delete (passed on this
  machine).
- **Walked on this Mac against the test stack:** sign-in through the shell
  stored the key through the host; a restart restored the session from the
  keychain and opened on the chat list. The maintainer reports "very many"
  keychain prompts on that restart; the build running then had no handle
  cache (every page read went to the keychain, and an unsigned binary is
  asked again for each read until "Always Allow"). With the cache a launch
  reads the keychain once per account; an unsigned build still prompts once
  after every rebuild, because the item's access list names the binary's
  signature. Walked again with the cache in place: the host log shows one
  keychain read at launch (about 7 s after the page loaded, the length of a
  keychain prompt) and none after it while the session ran.
- **Not built:** the Secure Enclave success path (needs the signed build of
  S5), Windows and Linux stores (the host refuses with `unsupported-os`), and
  the release-build refusal was not run. Open from the migration, not
  audited: best-effort readers outside AuthContext (`contacts/minted-links.ts`,
  `contacts/sent-request-intros.ts`, `reach/reach-store.ts`,
  `channels/ChannelsHydration.tsx`, `chat/rehydrate.ts`,
  `backup/device-phrase.ts`) still turn any `getDeviceKey` failure into
  "no key"; and the reset landing now stops on a refusal partway through a
  key reset, which relies on the ADR-0073 owed record to finish later.

## Alternatives

- **B. A proxy for every wasm call.** The host holds the MLS objects and every
  wasm export becomes an async IPC command. Rejected: about 285 exports and
  hundreds of synchronous call sites become asynchronous, the ADR-0040 /
  ADR-0077 disciplines stay in TypeScript but now around remote handles, call
  media still seals in the webview, and the security gain over the decision is
  small (the webview still holds decrypted messages and the call keys).
- **C. The client logic in Rust in the host.** A new Rust application layer
  (chats, device sync, contacts, channels) with the web screens as a view.
  Rejected for now as the largest option: a fourth implementation of every
  client discipline. It stays the way to get one copy of those disciplines for
  all clients, and would deserve its own ADR.
- **The website in a window, no host work.** Rejected: a browser's key
  storage, no tunnel, no process outliving the window, no notifications
  without Web Push (which a webview does not have).
- **macOS from the iOS SwiftUI code, Tauri for Windows and Linux.** Not chosen
  by the maintainer: a fourth UI code base and macOS slices for
  `LakkoCore.xcframework` and the SFrame library.
- **Electron.** Rejected in ADR-0002 (a bundled Chromium and Node in the
  process).

## Consequences

- **Positive:** one copy of the screens and the messaging logic for web and
  desktop; the device key off the disk image; the anti-censorship tunnel on
  the desktop; a desktop that receives while its window is closed; nothing in
  `lakko-mls` changes (invariant #3 and G-39 untouched).
- **Negative:** each OS's webview engine becomes a supported browser the
  project has to test, and on Linux calls may not work at all (unverified);
  Tauri and its plugins are new dependencies (supply chain); the desktop
  inherits every web-client limitation (MLS state in a webview, decrypted
  messages in the webview process).
- **Neutral:** the web client gains a `platform/` module and a desktop build
  mode; the server side is unchanged (requests arrive from the host's HTTP
  client instead of a browser).

**Invariants touched:** #4 (an armed tunnel never falls back to direct, a
missing key store never falls back to a file), #7 (section 9.2), #2 and #5
unchanged (the host is a transport, not a server). For `lakko-security` before
merge: the URI handler (what it forwards, to where), the host-side key and its
commands (who may ask to decrypt), the TUF root pinning, and the `lakko://`
secret handling.

## References

- [ADR-0002](0002-rust-core.md), [ADR-0026](0026-anti-censorship-wiring.md),
  [ADR-0038](0038-update-available-notice.md), [ADR-0040](0040-durable-pending-commit-outbox.md),
  [ADR-0071](0071-codec-header-in-the-clear.md), [ADR-0077](0077-account-binding-and-store-gate.md)
- [`docs/ux/desktop.md`](../ux/desktop.md)
- [Tauri 2](https://tauri.app/)
