<!-- SPDX-License-Identifier: AGPL-3.0-or-later -->
<!-- SPDX-FileCopyrightText: 2026 Lakko Contributors -->

# UX spec — profile names

**Status:** design, the input to ADR-0048 and the three client builds.
**Owner surface:** cross-platform (web SolidJS / iOS SwiftUI / Android Compose): signup,
settings, contacts, chat headers, message authors, member lists, call rosters, requests.
**Decision:** the maintainer's (2026-09-13) — each person sets their own profile name, and
it travels with their username to the people they talk to, end-to-end encrypted.

## 1. Why this exists

Today nobody has a name. A contact shows up as `@a1b2c3d4` — the first four bytes of their
public key with an `@` in front, which reads like a username and is not one. Even someone
who typed `@alice` to add a contact ends up with `@a1b2c3d4` in their list.

## 2. The two names

| | Username | Profile name |
|---|---|---|
| Example | `@alice` | `Alice Petrova` |
| Who sets it | the person, once, at signup | the person, any time |
| Unique | yes | no |
| Characters | Latin letters, digits, `_`; 3-32 | any printable, 1-64 |
| Used for | finding someone | showing who someone is |
| Server sees it | only as a blinded hash | never |
| Who sees it | anyone who talks to you | anyone who talks to you |

## 3. What a viewer sees, in order of precedence

1. **A name the viewer gave this contact themselves** (local rename). Never sent anywhere.
2. **The person's profile name.**
3. **The person's verified `@username`.**
4. **The key fingerprint without `@`** (`a1b2 c3d4`), labelled as a fingerprint in
   detail views.

A verified username is shown **under** the name wherever there is room (contact row,
profile sheet, request card, member list), e.g. `Alice Petrova · @alice`. Chat bubbles
and call tiles show only the resolved name.

**A username is shown only when verified** (§6). An unverified claim is not shown at all —
not greyed out, not with a question mark, simply absent — because a displayed `@pavel`
that is not Pavel is exactly the impersonation this must not enable.

## 4. Setting your name

- **Signup:** after the username, one optional field `Your name` (placeholder: the
  username). Skippable; an empty name means others see your `@username`.
- **Settings → Profile:** name field (max 64 characters, one line), a live preview of how
  others see you (`Alice Petrova · @alice`), Save. The username is shown read-only.
- **Guest accounts** (call links, no username): `Your name` is asked on the join screen,
  prefilled `Guest`.
- Saving shows `Your name will update for your contacts.` There is no per-contact name.
- **Your other devices (2026-09-30):** the name is the person's, not the device's. A name
  saved or cleared on one device shows on every other device of the account (Settings header,
  Settings → Profile) as soon as that device next syncs; the most recent change wins. A device
  just added to the account shows the name right after it is added. Until the name arrives a
  device shows `@username`, as for an account with no name. No new control, no new message.

Username at signup: Latin letters, digits and underscore only, 3–32 characters, validated as
the user types with the reason shown (`Only Latin letters, digits and _`, `At least 3
characters`, `At most 32 characters`); uppercase is accepted and shown lowercased. Search
fields apply the same rule and do not send a lookup for a name that fails it.

Profile name validation: trimmed; 1–64 characters; no line breaks; no invisible or text-direction
control characters (the right-to-left override among them). Emoji sequences, flags and
Persian/Indic joiners are allowed. Empty after trimming = no name.

## 5. Where names appear

- Contacts list, contact detail, incoming request card (`Alice Petrova · @alice wants to
  connect`, or `@alice` / the fingerprint when that is all there is).
- Chat list and chat header (1x1: the other person; group: the group label, unchanged).
- Message author labels in groups, typing indicators, reactions ("Liked by …"), read
  receipts, join requests, member list, admin actions ("Alice removed Bob").
- Call rosters and incoming-call banners. A group-call tile is matched to a person through
  the per-call tag the SFU emits, which a device can compute only for an account id it
  holds; the profile carries the account id (since 2026-09-19) so a member who is not a
  contact is named on the tile as in the members panel, and a name that arrives during the
  call replaces the placeholder in place.
- Notifications (the background-decrypt path shows the resolved name when it is already
  known on the device; it does not fetch or verify anything).

## 6. States

| State | UI |
|---|---|
| Profile not yet received | Fallback per §3 (username if verified, else fingerprint). Updates in place when it arrives. The device does not only wait: it asks the group to introduce itself right after joining, and again when it opens a group where someone is still unnamed (at most once a day per group), so a name arrives as soon as its owner's device next reads the group |
| Username being verified | Show the name without the username; add `· @alice` when verification completes |
| Username verification failed or mismatched | Username never shown for this person; a quiet "Username not verified" line in the contact detail only |
| Name changed by the other person | Updates everywhere on next receipt; no notification, no history |
| Local rename set | The rename wins everywhere; contact detail also shows `Their name: Alice Petrova` |
| Two contacts with the same name | Both shown as-is; the username (or fingerprint) under the name disambiguates in lists |

## 7. Cross-platform shape

One resolver function, the same precedence on all three clients, fed by one per-key
profile store. Settings → Profile lives where each platform's settings already are (web
AppShell menu, iOS settings sheet, Android settings screen).

## 8. Data the UI needs (input to the ADR)

- A profile record per person: `display_name`, `username` (as the person states it),
  `updated_at_ms` — carried end-to-end encrypted, bound to the sender's authenticated key.
- A verification result per `(signature key, username)`.
- The local rename (exists today as `displayName` on web contacts; iOS/Android have the
  same field).

## 9. Out of scope

Avatars, bios, per-chat nicknames, name history, and showing names to people you have no
chat or request with.
