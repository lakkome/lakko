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
  30 s (the ADR-0058 growth re-send discipline).
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
