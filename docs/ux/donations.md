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
  working.
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
2. Heading **Support Lakko**.
3. One paragraph: "Lakko is built and run by one person. Donations pay for
   the servers and keep the work going."
4. One card per payment method, in the order the server sends them.

### 2.1 A payment card (SBP, Revolut, Buy Me a Coffee)

- Title: the method's own name (`SBP`, `Revolut`, `Buy Me a Coffee`). Brand
  names are not translated.
- One line under it: who it is for ("For people in Russia." /
  "From anywhere.").
- The QR code, when the server has one and its hash matches the signed one
  (scan with a banking app).
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
- The QR code of the address, drawn by the client.
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
    description line by kind);
  - `link` (optional): the payment URL, `https://` only;
  - `qrSha256` (optional, 32 bytes): a QR image exists, and its SHA-256;
  - `hasPhone`: a phone number exists (the number itself is not in this
    answer);
  - for `crypto`: `coin`, `network`, `address` (and never a link, a QR or a
    phone: the client draws the address's QR itself);
  - `signedAtMillis`: when it was signed.
- `GET /discovery/donate/{id}/qr` returns the QR image (PNG), country rule
  applied. The client hashes the bytes and shows the image only when the
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
   the client from the verified `address` with the QR view the app already
   has (iOS `UI/Devices/BrandedQr.swift`, Android
   `ui/devices/BrandedQr.kt`, zxing), the address in a monospace, selectable, wrapping field,
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
| `donate.lede` | Lakko is built and run by one person. Donations pay for the servers and keep the work going. |
| `donate.for_russia` | For people in Russia. |
| `donate.worldwide` | From anywhere. |
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
