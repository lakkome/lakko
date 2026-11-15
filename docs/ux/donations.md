<!-- SPDX-License-Identifier: AGPL-3.0-or-later -->
<!-- SPDX-FileCopyrightText: 2026 Lakko Contributors -->

# UX spec: Support Lakko (donations)

**Status:** decided by the maintainer 2026-11-15; built on web (and so the
desktop app), with a Settings row on iOS and Android that opens the web page.
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
- **Settings > About > Support Lakko** on iOS and Android: opens
  `<web origin>/donations` in the system browser (maintainer decision,
  2026-11-15). The row carries the external-link icon so the person knows
  they are leaving the app.

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
- The QR code, when the server has one (scan with a banking app).
- **Donate** button: opens the payment link outside Lakko (new tab on web,
  the system browser on desktop).
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
| Nothing for this country | "No way to donate is available in your country right now." |
| Error (network, server) | "Couldn't load the ways to donate. Check your connection and try again." + **Try again**. |
| Show phone failed | Under the mask: "Couldn't show the number. Try again." The mask stays. |
| Copy failed (no clipboard access) | The text is selected so the person can copy it by hand. |

## 3. Data the UI needs (the server contract)

Served by `services/discovery`, which already knows the country of the
request's address (`/discovery/locate`). No session, no token; the per-IP
limit of the other anonymous routes.

- `GET /discovery/donate` returns `{"methods": [...]}` with only the methods
  allowed for the request's country. Each method:
  - `id`: a stable short id (`sbp`, `revolut`, `bmc`, `btc`, ...);
  - `kind`: `sbp` | `revolut` | `bmc` | `crypto` (the client picks the
    description line by kind);
  - `link` (optional): the payment URL;
  - `has_qr` (bool): a QR image exists;
  - `has_phone` (bool): a phone number exists (the number itself is not in
    this answer);
  - for `crypto`: `coin`, `network`, `address`.
- `GET /discovery/donate/{id}/qr` returns the QR image (PNG), country rule
  applied.
- `POST /discovery/donate/{id}/phone` with `{"action":"reveal"}` returns
  `{"phone": "..."}`, country rule applied. A POST, so link previews and
  crawlers that follow GET links do not collect it.

A country rule per method, as in the previous app: a list of allowed
countries, or a list of refused ones (`!RU`), or none (everywhere). An
unknown country passes a refuse-list and fails an allow-list.

## 4. Cross-platform shape

- Web and desktop: one page component, public route `/donations`.
- iOS and Android: one Settings row, no screen of their own.

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
