<!-- SPDX-License-Identifier: AGPL-3.0-or-later -->
<!-- SPDX-FileCopyrightText: 2026 Lakko Contributors -->

# UX spec: Support Lakko (donations)

**Status:** decided by the maintainer 2026-11-15; built on web (and so the
desktop app). Signed methods (section 3) decided 2026-11-16; the core, the
tool, discovery, the units and the web client's verification are built (web:
every method statement, the QR hash and the phone statement verified against
`VITE_LAKKO_DONATIONS_KEY`; unit-tested only, not walked in a browser).
Native screens on iOS and Android decided by the maintainer 2026-11-16 (in
place of the Settings row that opened the web page in the system browser):
the UniFFI verify surface is built (`verifyDonationMethod`,
`verifyDonationPhone`, `crates/lakko-ffi-uniffi/src/donation.rs`) and the two
screens are built (iOS `Donate/Donations.swift` + `UI/Settings/DonationsScreen.swift`,
Android `donate/` + `ui/donate/DonationsScreen.kt`; unit-tested only, not run
on a device, and with no key pinned in any build yet they show "none
available").
Round 2 decided by the maintainer 2026-11-18 after a look at
`test.lakko.me`: method statements carry a signed coin-mark byte (`icon`,
version 2 of the method statement), crypto QR codes carry the coin's mark and
never the Lakko logo, a payment QR is the operator's colour image (limit
raised to 512 KiB), and the web page uses the app's page width with a card
grid in two groups (sections 2, 2.4, 3, 4). The core, the tool, discovery and
both FFI surfaces are built for it; the three clients follow.
**Pattern parent:** the donations page of the previous app (`lakko_me`,
`public/web/donations.html`, `/api/donate/*`): the same page, the same three
payment methods, the same per-country rule, the same hidden phone number;
plus crypto wallets.

## 1. User goal and entry points

A person who likes Lakko wants to give money towards it. They need to see
which ways work where they are, pick one and finish the payment outside
Lakko.

Entry points:

- **The address `lakko.me/donations`.** Public: works signed in or signed
  out, shareable. It is the address the previous app used, so old links keep
  working. On web it is the one page whose address bar shows its path, opened
  directly or from Settings (every other page shows the origin only,
  `navigation.md` section 1), so the person can copy the link from it.
- **Settings > About > Support Lakko** on web and in the desktop app: opens
  the same page inside the app.
- **Settings > About > Support Lakko** on iOS and Android: opens the app's
  own donations screen (section 4; maintainer decision 2026-11-16, replacing
  the 2026-11-15 row that opened `<web origin>/donations` in the system
  browser). An ordinary in-app row with a chevron, no external-link icon:
  the person leaves the app only when they press **Donate**.

There is no floating "Support" button on the chat list (the previous app had
one); Settings is where a person looks for things about Lakko itself.

## 2. The page

From top to bottom:

1. A back control (to Settings when opened from there, else to the home or
   the sign-in page).
2. Heading **Support Lakko**, in the app's page-title size.
3. One paragraph in the app's body size: "Donations pay for the servers
   and keep the work going."
4. The cards, in two groups when both exist, each under a small section
   heading: **Bank and card** (`donate.section_payment`: SBP, Revolut, Buy Me
   a Coffee) first, then **Crypto** (`donate.section_crypto`: the wallets).
   When only one group has cards, its heading is still shown. Within a group
   the cards keep the order the server sends them.

### 2.0 Layout (web and desktop)

Design system: `.claude/skills/ux-ui/design-system.md`.

- The app's normal page width and type scale, not a narrow column: content
  at most about 1040 px wide, centred, with the side padding of the other
  wide pages.
- The cards of a group in a responsive grid: one column on a phone, two from
  about 640 px of content width, three from about 960 px; the cards of one
  row have equal height.
- A card, top to bottom: title row (service or coin name), for crypto
  the network line, the QR at 200 px square, then the
  actions (**Donate** full width; the phone row; for crypto the address in
  monospace that wraps, **Copy address**, the warning line).
- Body text in the app's body size, secondary text in its secondary size,
  never smaller.
- Works in light and dark, and at 360 px wide without horizontal scroll.

### 2.1 A payment card (SBP, Revolut, Buy Me a Coffee)

- Title: the method's own name (`SBP`, `Revolut`, `Buy Me a Coffee`). Brand
  names are not translated.
- No line under the title: the server sends only the methods allowed for
  the visitor's country, so a "who it is for" line would say nothing
  (removed 2026-11-18, maintainer).
- The QR code, when the server has one and its hash matches the signed one
  (scan with a banking app): the operator's PNG exactly as served, in colour,
  never recoloured, scaled to the card's QR size (section 2.4).
- **Donate** button: opens the payment link outside Lakko (new tab on web,
  the system browser on desktop, iOS and Android).
- Phone number, when the server has one: shown masked as `+7 9•• ••• •• ••`
  with **Show** next to it. Show fetches the number and replaces the mask;
  then **Copy** appears. Copy says "Copied" for two seconds.

Any of QR, Donate and phone may be absent; a card with none of them is not
sent by the server.

### 2.2 A crypto card

- Title: the coin (`Bitcoin`, `USDT`, ...), and under it the network
  ("Network: TRON (TRC-20)").
- The QR code of the address, drawn by the client, with the coin's mark in
  the centre when the method names one (section 2.4).
- The address in a monospace field that wraps, never cut, and a **Copy
  address** button.
- One warning line, because it is something the person acts on: "Send only
  USDT on the TRON network to this address. Anything else is lost."

### 2.3 States

| State | What is shown |
|---|---|
| Loading | Heading and paragraph, then two placeholder cards. |
| Loaded | The cards. |
| Nothing for this country | "No way to donate is available in your country right now." Also shown when the server sent methods but none verified, and in a build with no pinned donations key (section 3). |
| Error (network, server) | "Couldn't load the ways to donate. Check your connection and try again." + **Try again**. |
| Show phone failed | Under the mask: "Couldn't show the number. Try again." The mask stays. |
| Copy failed (no clipboard access) | The text is selected so the person can copy it by hand. |

### 2.4 QR codes

Both kinds of QR are shown at the same size on a card (web: 200 px square;
iOS and Android: section 4).

- **Crypto.** Drawn by the client from the verified `address`, error
  correction level H (high), quiet zone 4 modules, dark modules in the app's
  text colour on a white square (white in dark mode too, so wallets scan it).
  In the centre the coin mark the signed `icon` names, on a white circle
  that clears the modules beneath it:
  - `btc`: an orange circle with a white Bitcoin "B";
  - `eth`: the white Ethereum octahedron on a blue (`#627EEA`) circle;
  - `ton`: a blue circle with the white TON diamond;
  - `none`: a plain QR, no centre mark.
  One small SVG per coin, authored by hand as simple geometry, the SAME
  artwork on web (inline SVG strings), iOS (asset-catalog SVG or SwiftUI
  `Path`) and Android (vector drawable). No artwork is downloaded. **Never
  the Lakko logo**: the device-pairing branded QR is not reused here.
- **Payment (SBP, Revolut, Buy Me a Coffee).** The operator's PNG exactly as
  served (its SHA-256 checked against the signed `qrSha256`, as in section
  3), in colour, no recolouring, no centre mark added.

## 3. Data the UI needs (the server contract)

Served by `services/discovery`, which already knows the country of the
request's address (`/discovery/locate`). No session, no token; the per-IP
limit of the other anonymous routes.

**Every method is signed offline** (maintainer decision, 2026-11-16). The
payment link, the SHA-256 of the QR image, the crypto coin, network and
address, and the phone number are Ed25519 statements signed by a dedicated
donations key that is never on a server (`lakko-attest`, module `donation`;
signed by `tools/donations`, `just sign-donations`). The client verifies each
statement against the public key pinned into its build and shows only what
verifies. The server can hide a method (it still chooses which methods a
country sees); it cannot change one.

- `GET /discovery/donate` returns
  `{"methods": [{"id": "...", "statement_b64": "..."}]}`, only the methods
  allowed for the request's country, in the order to show them.
  `statement_b64` is standard base64 (with padding) of a method statement.
  The client calls `verifyDonationMethod(statement, key)` (wasm on web and
  desktop, UniFFI on iOS and Android; the same name on both, the UniFFI one
  returning a `DonationMethod` record whose `kind` is an enum) and reads from
  the result:
  - `id`: a stable short id (`sbp`, `revolut`, `bmc`, `btc`, ...); a card
    whose verified `id` differs from the entry's `id` is not shown;
  - `kind`: `sbp` | `revolut` | `bmc` | `crypto` (the client picks the
    description line and the group by kind);
  - `icon`: the coin mark in the centre of a crypto QR (section 2.4); on
    wasm the string `"none"` | `"btc"` | `"eth"` | `"ton"`, on UniFFI the enum
    `DonationIcon` (`.none` / `.btc` / `.eth` / `.ton`, Kotlin `NONE` /
    `BTC` / `ETH` / `TON`). Always `none` for a payment method; a closed list
    the statement signs (an unknown value does not verify);
  - `link` (optional): the payment URL, `https://` only;
  - `qrSha256` (optional, 32 bytes): a QR image exists, and its SHA-256;
  - `hasPhone`: a phone number exists (the number itself is not in this
    answer);
  - for `crypto`: `coin`, `network`, `address` (and never a link, a QR or a
    phone: the client draws the address's QR itself);
  - `signedAtMillis`: when it was signed.
- `GET /discovery/donate/{id}/qr` returns the QR image (PNG, at most
  512 KiB; the client refuses a larger answer), country rule applied. The client hashes the bytes and shows the image only when the
  SHA-256 equals the method's `qrSha256`; otherwise the card shows no QR.
- `POST /discovery/donate/{id}/phone` with `{"action":"reveal"}` returns
  `{"statement_b64": "..."}`, a phone statement; the client calls
  `verifyDonationPhone(statement, key, id)` and shows the number it returns.
  A POST, so link previews and crawlers that follow GET links do not collect
  it.

A statement that does not verify is dropped without a word
(`verifyDonationMethod` / `verifyDonationPhone` throw: on wasm an `Error`
with a `donationError` code, on UniFFI `FfiError.Donation` /
`FfiException.Donation` whose message starts with the same code, e.g.
`bad-signature: ...`): the card is not shown, or for a phone the "Show phone failed"
state below. A build with no pinned key shows no method at all.

A country rule per method, as in the previous app, kept server-side and
unsigned: a list of allowed countries, or a list of refused ones (`!RU`), or
none (everywhere). An unknown country passes a refuse-list and fails an
allow-list.

**What the signature does not stop** (`docs/security/known-gaps.md` G-57):
the web client and its pinned key come from the same server, so a server
takeover that also replaces the web bundle defeats the check in a browser.
The desktop app serves the web build bundled in the app, and the iOS and
Android screens (section 4) verify against a key built into the app, so
there the check holds. The server can also serve an
older statement signed for the same id (a retired address).

## 4. Cross-platform shape

One screen, the same on every client: the page of section 2 with its states
(section 2.3) and the contract of section 3.

- **Web and desktop:** one page component, public route `/donations`.
- **iOS and Android:** the same two groups with their headings and the same
  order (section 2); one card per row, the QR at its current size (the card
  width, centred; no separate size cap); the same coin marks as web (section
  2.4); a payment PNG in colour.
- **iOS and Android:** a screen of the app's own (SwiftUI / Compose), pushed
  from **Settings > About > Support Lakko** with the platform's back
  navigation. It is reachable only from Settings (there is no deep link to it;
  `lakko.me/donations` stays a web page). It works signed in or not
  (the routes take no session), but in practice it is opened from Settings.

### 4.1 What the native screen does

1. **Pinned key.** Read once from the build: iOS Info.plist key
   `LakkoDonationsKey` (set from the `LAKKO_DONATIONS_KEY` build setting in
   the xcconfig, empty by default), Android `BuildConfig.LAKKO_DONATIONS_KEY`
   (from the gradle property `lakko.donations.key`, empty by default). The
   value is the 32-byte Ed25519 public key as base64url without padding (43
   characters), the same form as `VITE_LAKKO_DONATIONS_KEY`. Empty, or not
   decoding to 32 bytes: the screen shows the **Nothing for this country**
   state at once and asks the server nothing. The key is never taken from the
   server.
2. **Loading.** `GET /discovery/donate` through the app's discovery base and
   its ordinary transport (the anti-censorship tunnel when armed, as for every
   other discovery call). Heading and lede, then two placeholder cards.
3. **Verify.** For each entry: base64-decode `statement_b64`, call
   `verifyDonationMethod(statement, key)`, and keep the card only if it
   verifies and its `id` equals the entry's `id`. Keep the server's order.
   A refusal is dropped silently (logged by code only, never by content).
   No card left: **Nothing for this country**.
4. **QR (SBP, Revolut, Buy Me a Coffee).** Only for a method with
   `qrSha256`: fetch `GET /discovery/donate/{id}/qr`, SHA-256 the bytes, and
   draw the image only when the digest equals `qrSha256` (compare all 32
   bytes). A mismatch, a fetch failure or an image that does not decode: the
   card shows no QR and nothing else changes. The card is shown at once; the
   QR fills in when verified.
5. **Donate.** Shown only when `link` is present. Opens the link in the
   system browser (Safari / the default browser, not an in-app web view);
   the link is `https://` (the core refuses any other), and the client checks
   the scheme again before opening.
6. **Phone.** Shown only when `hasPhone`. The masked line and **Show**;
   Show posts `POST /discovery/donate/{id}/phone` `{"action":"reveal"}`,
   verifies with `verifyDonationPhone(statement, key, id)`, and replaces the
   mask with the number it returns, then **Copy**. Any failure (network,
   refusal, `id-mismatch`): the **Show phone failed** line, the mask stays,
   Show can be pressed again. The number lives in screen state only and is
   gone when the screen closes.
7. **Crypto card.** Coin, `donate.crypto_network`, the address QR drawn by
   the client from the verified `address` per section 2.4 (error correction
   H, the coin mark of `icon` in the centre, never the Lakko logo, so not the
   device-pairing `BrandedQr`), the address in a monospace, selectable, wrapping field,
   **Copy address**, and the `donate.crypto_warning` line.
8. **Copy.** Writes to the system clipboard; the button reads **Copied** for
   two seconds. The native clipboard is always writable, so the web
   "Copy failed" state does not exist on the phones.
9. **Error.** A network or server failure of step 2 (not a refusal of a
   statement): `donate.error` with **Try again** (`common.try_again`), which
   repeats step 2.

Accessibility: the QR images carry `common.qr_code` as their label; the
masked number is read as hidden until shown; Dynamic Type / font scaling
reflows the cards, and the address wraps instead of shrinking.

## 5. Copy (catalog keys `donate.*`)

| Key | Text |
|---|---|
| `donate.title` | Support Lakko |
| `donate.section_payment` | Bank and card |
| `donate.section_crypto` | Crypto |
| `donate.lede` | Donations pay for the servers and keep the work going. |
| `donate.button` | Donate |
| `donate.phone_show` | Show |
| `donate.copy` | Copy |
| `donate.copied` | Copied |
| `donate.crypto_network` | Network: {network} |
| `donate.crypto_copy` | Copy address |
| `donate.crypto_warning` | Send only {coin} on the {network} network to this address. Anything else is lost. |
| `donate.none` | No way to donate is available in your country right now. |
| `donate.error` | Couldn't load the ways to donate. Check your connection and try again. |
| `donate.phone_error` | Couldn't show the number. Try again. |
| `settings.about.support` | Support Lakko |
