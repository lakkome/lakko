<!-- SPDX-License-Identifier: AGPL-3.0-or-later -->
<!-- SPDX-FileCopyrightText: 2026 Lakko Contributors -->

# ADR-0085: The desktop client is a Tauri host that keeps the keys and talks to the network; the web client's code runs inside it

- **Status:** Proposed (2026-10-16). S1 spike built on macOS the same day (section 10); S2 built on macOS the same day for the web client and the host (section 11), the Secure Enclave path not yet exercised. The overall direction is the maintainer's
  (2026-10-16): Tauri 2 on macOS, Windows and Linux, the host does the system
  work and keeps the device key, the messaging logic and MLS stay in the web
  client's wasm inside the window (option A of section "Alternatives"). The
  five decisions of section 9 are the maintainer's too (2026-10-16). S3 is
  built on macOS (section 12, 2026-10-29), with system notifications not yet
  seen on screen; the Windows and Linux host compiles and has never been run
  (section 13, 2026-10-30); S4 is built and the tunnel runs on the test stack (section 14, 2026-10-30), with
  no call placed through it; S5 is built in part on macOS (2026-10-31): files and
  `lakko://` links, not yet walked by hand; the TUF repository, install and
  packaging are not started.
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
  `calls/sfu-signaling-client.ts` opens it through the host when the setting
  is on (`tunneled_websocket`, reached over the app scheme, section 14);
  otherwise the page opens it itself.
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
- A web client's receive loop parks only where a push wake reaches it
  (`chat/receive-activity.ts`), which is never in the desktop app. The mailbox
  loop parked on `hidden` alone until 2026-11-01, so a closed window took no
  contact request and no Welcome; seen with no other device running, fixed.
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
  `https://lakko.me/i/`, `/c/` or `/g/` link. The web invite pages offer "Open
  in the app", which follows the `lakko://` form. On macOS in Chrome and Firefox
  the page also tries it by itself, once per link (maintainer, 2026-10-31),
  because people follow the `https` link and should land in the app as they do
  in a meeting link. Safari does not: measured 2026-10-31, with no handler for
  the scheme it shows "Safari cannot open the page because the address is
  invalid". Chrome and Firefox kept the page working through a scripted link
  click with no handler.
- **A link is clicked, never copied (maintainer, 2026-11-01; replaces the rule
  of 2026-10-31 that the secret never rides a command line).** Where the OS can
  hand the `https` link to the app, it does: a Universal Link on macOS, a
  packaged app's URI handler on Windows. The host accepts this deployment's
  `https` link as it accepts `lakko://`, and tao delivers a Universal Link through
  the same event; what it takes is S5's signed, packaged build. Where the OS
  cannot (Linux, and every unpackaged build), the whole `lakko://` link travels.
  On Windows and Linux that is a command line: a second start lives for
  milliseconds, but the process the link started keeps it in its arguments for as
  long as it runs, readable by the user's other processes and, on Linux with the
  default `/proc`, by other local users. Accepted: the link reached the person
  through some other messenger and sits in the browser's history, and it admits
  nobody by itself. Re-executing the first process without the argument was
  rejected: it cannot be run here, and a race on the single-instance lock would
  leave a click with no app. The "Open a link" item and its paste dialog are
  removed. Links in messages are live
  ([`message-links.md`](../ux/message-links.md)), and the host gained `POST
  open-url` for the system's default browser.

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
3. **Links: the `lakko://` scheme on all three OSes, and `https://lakko.me`
   claimed where the OS allows** (section 5, amended 2026-11-01). Claiming it
   needs S5: the macOS `com.apple.developer.associated-domains` entitlement with
   the desktop app id in `apple-app-site-association`, and on Windows a packaged
   app with a `uap3:AppUriHandler` and `windows-app-web-link` on the site.
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

### 12. S3 on macOS (2026-10-29)

- **Built earlier with S2 (`app_shell.rs`):** one window titled "Lakko" with
  its geometry restored on a connected display, close hides it unless "Keep
  running" is off, quit during a call asks "End the call and quit?", the Dock
  badge with the All chats unread count, "Open Lakko when I sign in" as a
  per-user LaunchAgent started with `--hidden`, the macOS menu bar, and the
  shortcuts of `desktop.md` section 5 in the page (`platform/shortcuts.ts`).
- **System notifications (`notify.rs`):** the page asks the host with
  `POST /__host/app/notify {tag, title, body, target}`; the host shows an
  `NSUserNotification` through `mac-notification-sys` on a thread of its own
  and, on a click, shows the window and sends the page
  `{kind: "open-target", target}`, which navigates there. `target` must be an
  app path, checked by both sides (`notify::is_app_path`, `isAppPath`). At
  most 16 notifications wait for a click at once; later ones are shown
  without waiting. The page raises one for a message when a conversation's
  unread count rises (not muted, not the conversation open in the focused
  window; the first snapshot after start announces nothing), for a 1x1 call
  ringing behind the window (a muted call raises nothing; with the window in
  front the Dock icon bounces once, `POST /__host/app/attention`), and for
  "A device was added" (target Settings > Devices). Rules in
  `platform/desktop-notifications.ts`, host-tested and unit-tested.
- **A notification needs a bundle:** macOS attributes one to the app's bundle
  identifier, so `just dev-desktop` now builds `target/debug/Lakko.app`
  (Info.plist, icon, signature), registers it with Launch Services and
  starts it with `open` (since 2026-11-07; before, the executable inside was
  run as a child of the shell, and macOS then asked for the camera and the
  microphone on behalf of that shell's program: started from Claude Code the
  camera was refused with no prompt, while the microphone worked because
  that program may use it). Without a registered bundle the host logs that
  notifications are unavailable and the route answers
  `503 notifications-unsupported`.
- **Not built:** "Open system settings" when notifications are off. Nothing
  of the notifications has been seen on screen. The rest of this list was
  built on 2026-10-30, section 13.

### 13. The rest of S3, Windows and Linux (2026-10-30)

Nothing here has been run on Windows or Linux, or seen on screen on macOS.
Compiled and clippy-checked in a Linux container for `x86_64-unknown-linux-gnu`
and `x86_64-pc-windows-gnu` (the shipped Windows target is MSVC); host unit
tests run on macOS and Linux.

- **Withdrawing (`notify.rs`):** the host remembers what it delivered by tag.
  macOS: `mac-notification-sys` hands out no identifier, so the notification is
  found again by its title and text in `NSUserNotificationCenter` and removed
  (`objc2-foundation`, safe bindings, no `unsafe` in the host). Two delivered
  notifications with one title and text are removed together. Windows: a toast
  tag plus `ToastNotificationManager.History`. Linux: `CloseNotification`. A
  notification with a tag already delivered replaces it, on all three.
- **Accept and Decline:** `call: {accept, decline, callId, video}` on
  `POST notify`. A press reaches the page as `{kind: "call-action", ...}` and
  enters `calls/CallActionListener.tsx` as a third source of the same intent,
  so `decideCallAction` applies: only the call ringing with that id is
  answered. Accept shows the window, Decline leaves it where it is. The ring's
  end withdraws the notification. On macOS the buttons depend on the private
  `_showsButtons` key the library sets; whether a banner shows them is not
  verified. On Windows an Action Center click after the toast timed out does
  nothing (no COM activator).
- **The refusal screen (`platform/capture-gate.ts`):** `desktop.md` section 4.2
  named a prejoin screen, which a voice call no longer has, so it is a dialog
  raised by `captureGate` in `dial`, `accept` and `startGroupCall`. It probes
  each wanted device with `getUserMedia`; only `not-allowed` stops. The browser
  build skips it. "Open settings" uses a fixed table of OS links, nothing the
  page sends reaches the command. Whether WKWebView sees a permission changed
  in System Settings without a restart is not known.
- **Windows and Linux:** a tray icon with "Open Lakko" and "Quit Lakko", the
  labels sent by the page (`tray.rs`). A Linux with no StatusNotifier watcher
  has no tray, and closing the window minimises it. "Lakko is still running"
  once per computer. Start at login: the Run key, an XDG autostart entry (the
  AppImage's own path when run from one). The unread count: an overlay dot on
  Windows (the taskbar has no number; `desktop.md` section 2 said a number);
  `set_badge_count` on Linux.
- **The key store on Windows and Linux (`key_store.rs`):** it refused every
  account before, so neither OS could sign in. Through the `keyring` crate:
  Credential Manager, the Secret Service. Software stores, the exception of
  decision 9.2. **The TPM path decision 9.2 names for Windows is not built**,
  so a Windows computer with a TPM is software-backed too, which is below what
  that decision says. For `lakko-security`.
- **Packaging is S5:** `tauri.conf.json` has `bundle.active: false`, and a
  Windows toast needs a Start menu shortcut carrying the app id, so toasts need
  the installer.

### 14. S4: anti-censorship and SFU signaling through the host (2026-10-30)

`src/tunnel.rs`, `platform/host-socket.ts`, `AntiCensorshipCard` in
`settings/DesktopSettings.tsx`.

- **The setting on means never direct.** iOS and Android stay on the direct
  path while the config is being fetched and when it could not be. The desktop
  refuses instead (`503 tunnel-unavailable`): a person who turned this on did
  so because a direct connection is what they cannot afford, and section 3
  already said a failed tunnel does not go direct. It holds from the first
  request: until the setting has been read from disk nothing leaves. The one
  exception is a build with no pinned trust anchor, which cannot honour the
  setting and says "Not available in this build".
- **Build-time pin:** `LAKKO_DESKTOP_BOOTSTRAP_TRUST_ANCHOR` (standard base64),
  `_DOH_DOMAIN`, `_CONFIG_PATH`. A debug build may read the config from
  `LAKKO_DESKTOP_BOOTSTRAP_FILE`, still verified against the anchor; a release
  build has no such path.
- **A long-lived process outlives a config.** iOS and Android fetch it at boot.
  The host fetches it again halfway through its life, and every 60 s after a
  failure; a held config stays in use until it expires, then requests are
  refused.
- **The socket:** a channel out of a Tauri command would have been the only
  IPC of its kind here, so the socket follows the rest of the host: the app
  scheme, `POST /__host/tunnel/ws/open`, `send`, `close`, and a `recv` that
  waits up to 25 s for frames. The page names no address: the host dials the
  SFU it was configured with (`LAKKO_DESKTOP_SFU_URL`), so this is no relay.
  Frames are sent one at a time, since two requests in flight may arrive in
  either order.
- **Call media stays direct**, as on the native clients (section 3).
- `/discovery/locate` is not asked with the setting on (ADR-0080).
- **Settings > Security**, not Settings > Data: the Data section no longer
  exists.

**Defects found in the shared core, fixed in `lakko-transport`, which reach
iOS and Android too:**

- REALITY and Shadowsocks-2022 never read the signed `remote`. They dialled
  the endpoint they were handed, which every client passes as the API host and
  port: the blocked address, where no obfuscated listener runs. `BootstrapDialer`
  now pins each to its signed `remote`.
- A dropped `recv` closed a Shadowsocks connection for good and could lose
  bytes on WebTransport. Both the relay and `ByteStreamConnection` `select!`
  `recv` against outbound data, so any tunnelled socket over Shadowsocks died
  at its first send. `recv` now survives being dropped on both.
- A body above 16 KiB could not cross Shadowsocks: one frame per request, and
  an AEAD chunk per frame. Requests and socket writes are now cut into chunks.
- A chunked response reached the caller undecoded.

`tools/bootstrap-config` (`just gen-bootstrap-key`, `just sign-bootstrap-config`)
is new: nothing could sign a config before.

**Limits, same as the native clients:** a response above 8 MiB fails (the
tunnel client's cap), so a large attachment cannot be fetched through the
tunnel, and each request dials a tunnel of its own.

**Deployed on the test stack** (`just deploy-test-obfs`,
`infra/test-stack/deploy-obfs-proxy.sh`; the first time this service ran on any
server): REALITY on 8444 and Shadowsocks-2022 on 8388, relaying to a
loopback-only plain listener in the test vhost, the config signed on the
developer's machine and served at `/bootstrap.json`. The script restarts nothing
else and leaves the web client alone.

**Verified:** host tests through a real Shadowsocks listener and the real
`obfs-proxy` relay, for a request and for the socket; `lakko-transport` tests.
From a Mac over the public internet, `live_every_transport_reaches_the_test_stack`
fetched the config over DoH, verified it against the anchor, and through REALITY
and through Shadowsocks alone got `200` from auth and `401` from the SFU for a
made-up admission token, which is the SFU itself answering a socket that came
through the tunnel. The app ran against a local `obfs-proxy` with about 1450
requests through it, no failure, journal writes among them, then against the
stack with the real pin: it armed over DoH and the server took 121 tunnels in
two minutes (119 REALITY, 2 Shadowsocks), one per request.

**Group call through the tunnel (2026-10-31, macOS debug build, the setting on,
against the test stack):** the desktop app and a second account in a group
call. During the call the app's process held TCP connections to the server on
the REALITY port 8444 and on no other port, while the SFU forwarded audio and
video between both peers over UDP with no loss reported in the sampled seconds.
Sound and picture as a person perceives them are not recorded here.

**Found in the same run:** every voice and video message failed with
`blob-hash-mismatch`. WebKit hands a `Blob` request body to a custom scheme as a
stream, and wry reads it empty: on the macOS probe page a `Blob` arrived as 0
bytes through XHR and through `fetch`, at 64 KiB and 3 MiB, and the same size as
bytes arrived whole. The upload now sends bytes. The probe page keeps this
measurement. Separately, a media bubble's cleanup read a parent getter that throws
during teardown, which stopped the route change: the chats would not switch until
the app restarted. The cleanups now release the last key they saw.

The first take after each launch was lost: the macOS webview answers `prompt`
to the Permissions API on every launch while the system grant stands, so the
recorder read a stream that arrived in 53 ms as a prompt and released it. A grant
under a pointer still down now records. Confirmed on the app.

The same run spent the `discovery-lookup` quota (300 a day) with nobody searching.
The introduction and Everyone publication evaluates one OPRF term per word prefix,
up to 64 and a token each, and web and Android kept the outputs only at the end.
Discovery's address limit (burst 10) refused the eleventh, the outputs were lost,
and the five-minute tick paid for the same terms again without ever finishing.
All three clients now keep each output as it arrives and wait once on a `429`; on
iOS and Android the Everyone step keeps them too. A finished publication spends
nothing on later runs. Through the tunnel every client shares `127.0.0.1`, so one
person's publication draws on the budget of all (the limit named above).

**Not done:** nothing here ran on Windows or Linux. WebTransport has no
listener configured; MASQUE stays unfit for this (ADR-0026).

**Found running it:** through the tunnel every client reaches the server from
`127.0.0.1`, so per-address limits count all tunnel users as one address. A real
deployment needs those limits to treat the relay differently; not built. The
discovery lookup answered `429` to this one client on the stack, whose env file
does not carry the raised discovery limits, so that answer is not shown to come
from the tunnel.

**Found on the first run with an account's real journals (2026-10-31):** a
REALITY stream lost data on any socket read above 4096 bytes, which is all
rustls takes per `read_tls`, so a response of more than about 4 KiB failed
(`no header terminator`) or arrived cut off, and one group's poll failed every
time. The fix feeds rustls the whole read, on both sides and in both handshake
drivers, and `tunneled_request` refuses a body shorter than its `Content-Length`,
because a tunnel that dies mid-response ends the read like a close does.
`reality_tunnel_carries_a_large_request_and_response` failed before the fix
with that same error. Shadowsocks has no such path.

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
