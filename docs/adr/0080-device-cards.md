<!-- SPDX-License-Identifier: AGPL-3.0-or-later -->
<!-- SPDX-FileCopyrightText: 2026 Lakko Contributors -->

# ADR-0080: Device cards: each device tells the account's other devices what it is

- **Status:** Accepted (2026-10-16, maintainer). The maintainer decided the behaviour the
  same day (`docs/ux/second-device.md` section 3): rows like other
  messengers' "active sessions", the city looked up on a server that holds the
  city database, the city shown by default with a switch in Privacy, last
  activity meaning the person's last action on that device.
- **Date:** 2026-10-16
- **Context:** multi-device (ADR-0007, ADR-0050), DeviceGroup synced state
  (ADR-0058), DeviceGroup service envelopes (ADR-0060 section 7), servers
  that cannot link accounts (ADR-0078)

## Context

The Devices screen listed "Device 1", "Device 2" and so on: the only facts a
device had about another were its leaf key, its platform when known and the
date it joined. Nothing tells a person which row is the phone in their hand
and which is a laptop at home, which is what the screen is for: spotting a
device that should not be there.

## Decision

### 1. The device card

`lakko-contacts::device_card`, a bounded, fail-closed envelope in the shape of
the profile envelope: `lakko/device-card/v1\0 ‖ version ‖ updated_at_ms(8) ‖
last_active_ms(8) ‖ platform(1) ‖ model ‖ app_version ‖ os ‖ city ‖ country`,
each string one-byte-length-prefixed and bounded (model 64, app version 32,
system name and version 64, city 64, country a 2-letter code or empty), no
control or format characters, rejected as a whole on any violation, compose
refusing what decode refuses. `platform` is a closed set (`ios`, `ipados`,
`macos`, `android`, `web`, `desktop`). Dispatched as its own
`IncomingEnvelope` kind so it never renders. Fuzz target `device_card_decode`.

### 2. Carried to the account's own devices only

Each device sends its card as a DeviceGroup `ServiceEnvelope` (field 20 of the
DeviceGroup payload, the ADR-0060 carrier). It is end-to-end encrypted by the
DeviceGroup's MLS group, so only the account's devices read it; the server sees
a DeviceGroup row of the size bucket every DeviceGroup row has.

A receiver stores the card under the **sender's device leaf key resolved at
receipt** (the C-68 rule: never a leaf index), replacing a stored card only
when `updated_at_ms` is strictly greater. A card naming nothing about its
sender is the design: the sender is the MLS-authenticated leaf. When a leaf
leaves the DeviceGroup its card is dropped.

### 3. When a device sends its card

- After the person does something on that device (opens a chat, sends,
  presses a control), when the device's last sent card is older than 30
  minutes. `last_active_ms` is the time of that action.
- When a field other than the times changes (app or system version, city,
  the city switch).
- When the DeviceGroup grows, so a new device learns the others: each existing
  device re-sends its stored own card, unchanged, after a random delay of up to
  300 ms (the ADR-0058 growth re-send discipline; 30 s until 2026-10-29, when
  the maintainer found a joined device waiting most of that for a name).
- Never on a timer, and never while the app is in the background: a device
  nobody touches sends nothing, so its last-active time stays put.

### 4. The city

The device asks an anonymous route on `services/discovery`,
`GET /discovery/locate`, which answers `{ city, country }` or `{}` for the
request's own source address. The route takes no session, no token and no
account; it keeps nothing, writes no log line naming the address or the
answer (the ADR-0078 section 11 lint applies), and is limited per address in
memory like the other anonymous discovery routes. The city database (DB-IP
City Lite in MaxMind format, CC BY 4.0, attribution in the app's About screen)
lives on the server and is read with the `maxminddb` crate (a new dependency,
ISC licence, to pass `cargo deny`); the database file is deployed beside the
binary and refreshed monthly.

The server already sees the address of every request, so the lookup tells it
nothing new; what must not exist is a stored link between an account, a device
and a place, and none is created: the answer goes only into the encrypted
card. A device on the anti-censorship connection (ADR-0026) does not ask: its
source address is Lakko's relay. The person can turn the city off in Settings
> Privacy ("Show my city to my other devices", on by default); off means no
lookup and an empty city in the card.

Residual (ADR-0078 class (c)): an operator watching both discovery and the
Delivery Service can see a locate request followed shortly by a DeviceGroup
publish from the same address. The publish names no account (ADR-0053 epoch
keys), so this is the existing network-origin residual, not a new link.

## Alternatives

- **The server stores device metadata per account** (the usual design):
  materialises account, device and place on the server. Rejected (invariants
  #1, #5, ADR-0078).
- **A city database on every client**: tens of megabytes, licence and update
  burden on three clients. The maintainer chose the server-held database.
- **Activity on a timer**: tells the account's devices when the app is
  running, not when the person used it, and costs traffic for nothing. The
  maintainer chose activity on the person's action.

## Consequences

- `lakko-contacts`: the envelope, dispatch kind, fuzz target.
- Both FFI surfaces; the NSE extension decodes it only to suppress it.
- `services/discovery`: the locate route, the database loader, a test that the
  route stores nothing and logs no address.
- Three clients: card composing (platform facts from each OS), sending rules,
  the store keyed by leaf key, the Devices screen of the UX spec, the Privacy
  switch, the "Remove all other devices" action over the existing ADR-0074
  removal.
- No `lakko-mls` change (invariant #3 untouched).

## Documentation this ADR implies

CLAUDE.md ADR row and module map rows (`lakko-contacts`, `services/discovery`);
PRODUCT-READINESS multi-device row; `docs/ux/second-device.md` section 3 is
the UX source of truth.

## As built (2026-10-16)

Built the same day in the core, both FFI surfaces, the NSE surface,
`services/discovery` and the three clients; host-tested only. Nothing was
walked on a device or against a server, and nothing is deployed.

**Core and FFI.** `lakko-contacts::device_card` as section 1, with the bounds
read as **bytes** (model, system 64; app version 32; city 64; country empty or
two ASCII capital letters). Every string may be empty and must be trimmed;
control characters, U+2028 / U+2029 and every format character of the
profile's table are refused, with none of the display name's joiner
exceptions. Platform wire bytes are frozen: ios 1, ipados 2, macos 3,
android 4, web 5, desktop 6. `DeviceCard::normalized` removes what the
bounds refuse from operating-system text (line breaks become spaces, a string
is cut on a character boundary, a country that is not two letters is emptied)
and is the identity on a valid card; the FFI composers normalize before
composing, so a client never fails to compose from a user agent or a city
answer. Dispatched as `IncomingEnvelope::DeviceCard` right after the
key-rotation envelope. UniFFI kind `DeviceCard` is appended last (ordinal
safety) and the `device_card` field defaults to `None`; wasm kind 19. The
extension crate answers only `isDeviceCardEnvelope` and the length range, and
the NSE filter has an arm for it, defaulted like the key-rotation arm.

**Discovery.** `GET /discovery/locate` sits in the rate-limited router, so it
shares the per-IP bucket (and, if a bearer token is sent, the account bucket)
of the other anonymous routes. The source address is the one
`rate_limit::source_ip` already trusts (X-Real-IP, else the rightmost
X-Forwarded-For entry, else the socket); no nginx change was needed. The
answer is the database's English city name and its ISO code, a half when only
one is known, or `{}`; a city longer than the card's bound or carrying a
control character is dropped. `Cache-Control: no-store`. A configured but
unreadable file does not stop discovery: it logs one warning and answers `{}`
(chosen so a broken monthly refresh cannot take discovery down). The file is
read once at startup; a refresh is a restart. `maxminddb` 0.32 with default
features off; `cargo deny check licenses` passed with it.

**Clients (the same on web, iOS and Android).**

- The own card: web `platform = web`, model "Browser on System" from the user
  agent (Chrome, Edge, Opera, Firefox, Safari; iOS, iPadOS, Android, macOS,
  Windows, ChromeOS, Linux; the frozen macOS 10.15 and Windows NT 10.0 versions
  are left out), app version `web` so the row reads "Lakko web". iOS: model
  from `utsname` (the simulated model in the Simulator) through a table of
  iPhone 13 to iPhone 16 identifiers, else the identifier; `ipados` on an iPad;
  `macos` / "Mac" for the iOS app on a Mac. Android: `Build.MANUFACTURER` and
  `Build.MODEL` without repeating the maker, `Android <release>`.
- An action is any pointer or key event on web (document, capture phase,
  ignored while the page is hidden), a touch on any window on iOS (a
  recogniser that cancels nothing), `Activity.onUserInteraction` on Android
  (with a foreground flag from `onResume` / `onPause`). Each action asks the
  rule; a card is sent when none exists, a field other than the times changed,
  or the last is 30 minutes old.
- The city is asked for at most every 6 hours (chosen, not measured), only on
  an action, never with the switch off and never while the native
  anti-censorship connection is armed (web has none). Turning the switch off
  sends a card with no city at once.
- **"Show my city to my other devices" is per device and not synced**: it
  governs what this device puts in its own card, and one device's choice
  about its location should not change another's.
- The growth re-send: every device that was in the DeviceGroup before a
  device joined re-sends its stored card unchanged after a uniform random
  delay below 300 ms (30 s until 2026-10-29), only while in the foreground. The pairing hand-over also
  carries the own card (web through the service-envelope outbox, iOS through
  `handOverEnvelopes`; Android only through the growth re-send).
- Received cards are stored per account (web `localStorage`, iOS
  `UserDefaults.standard`, Android one `SharedPreferences` file), keyed by the
  sender's leaf signature key in hex, resolved from the MLS-authenticated
  sender leaf against the roster at receipt; pruned whenever the roster is
  read. Forget device / Delete account remove the keys (each client's wipe
  list).
- The Devices screen: section "This device" (one row, "online"), "Remove all
  other devices" with its line when others exist, section "Other devices"
  ordered by last activity with cardless rows last. Activity uses each
  client's chat-list time format (time today, weekday within six days, else a
  date). The country name is the platform's name for the code in the
  interface locale.
- **Remove one** keeps the ADR-0074 reason pair ("I do not use it any more" /
  "It was lost or stolen") between the new title and body and the buttons,
  because removing it would remove the only entry to the key rotation. The
  title, body and button take the new copy.
- **Remove all** asks the fresh-auth proof once, then runs each device's own
  removal in turn; each row shows "Removing..." and then either disappears
  or keeps "Could not remove. Try again." with its own Remove. **With a
  passkey on the account, each removal still asks its own passkey assertion**
  (the ADR-0015 attestation is bound to one leaf), so "one prompt" holds for
  the proof and not for the attestations.
- **Not built: the added date on a row whose card has not arrived.** No FFI
  exposes the leaf binding's `issued_at`, and adding it is a `lakko-mls`
  change this ADR rules out; such a row shows its generic icon and "Device".

**Tests.** Core: 15 unit tests in `device_card.rs`, fuzz target
`device_card_decode` (compile-checked, not run). FFI: round-trip and dispatch
tests on UniFFI (2), wasm (1), extension (4, including the golden of the NSE
pre-filter shape). Discovery: 3 unit tests in `locate.rs`, 4 route tests in
`main.rs` (answers for the trusted address without a session and over a pool
never reached, `{}` without a database, the per-address bucket, and a
TRACE-level capture that finds neither the address nor the answer). Clients:
web `device-cards.test.ts` (store rules, send rule, order, row text, user
agent, sender resolution, codec round trip through the real wasm); iOS
`DeviceCardsTests.swift`; Android `DeviceCardsTest.kt`.

## Amendment (2026-10-29): the card travels in the pairing payload; this device's row; a joined device sends at once

Seen live: after scanning a code the signed-in device asked "Link this
device?" and said "Device added.", naming nothing; both rows of the Devices
screen read "Device"; a newly joined web device appeared by name on the
iPhone after 22 s. UX `docs/ux/second-device.md` section 2.1.

- **The card rides in the pairing payload.** `PairingQr` field 17
  `device_card` carries the new device's card envelope (at most 270 bytes,
  `lakko_mls::pairing::PAIRING_DEVICE_CARD_MAX_LEN`, pinned equal to
  `DEVICE_CARD_ENVELOPE_MAX_LEN`), empty when absent and on a rejoin knock.
  The ADR-0049 code route seals the same bytes. It is the new device's own
  statement and nothing trusts it: the signed-in device shows it in the
  confirmation ("Link "Chrome on macOS"?" with the row's detail lines, the
  warning kept) and in the result ("..." added.), and stores it for the new
  leaf at stamp 0 so the device's own card over the DeviceGroup replaces it.
  `lakko-mls` bounds the length and does not decode it; both FFI surfaces
  decode it for display (`DecodedPairingQr.deviceCard`, an undecodable card
  is absent, never an error). Built on web, iOS and Android; host-tested only.
  The new device asks for its city when the code screen opens, signed in or
  not, and waits at most 1.5 s before showing the code (maintainer,
  2026-10-29; the ADR-0056 rule that preparing a payload makes no request is
  narrowed by this one anonymous `GET /discovery/locate`, which names no
  account and reaches a server that already sees the device's address at its
  next request). No city with anti-censorship armed or the city switch off.
- **This device's own row** is built from the device's own current fields,
  never from the received cards (where its own card never is). All three
  clients looked it up there and read "Device".
- **A device that has just joined sends its card at once**, without the
  growth delay; if its card runtime is not bound to the account yet, the
  send goes when it binds (in memory).
- **The growth re-send delay** of the devices already there is 0-300 ms (was
  0-30 s).
