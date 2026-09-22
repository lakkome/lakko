<!-- SPDX-License-Identifier: AGPL-3.0-or-later -->
<!-- SPDX-FileCopyrightText: 2026 Lakko Contributors -->

# Screen inventory — what exists, what is captured

Built from the sources on 2026-09-22: web routes in `apps/web/src/routes/routes.tsx`,
iOS screens under `apps/ios/Lakko/UI/`, Android screens, sheets and dialogs under
`apps/android/.../ui/`. "Captured" means a screenshot of a running build exists
under `docs/ux/screens/`.

Legend: **OK** captured · **-** exists, not captured yet · **none** the screen
does not exist on that platform.

## Onboarding

| Screen | Web | iOS | Android |
|---|---|---|---|
| Welcome | **none** | OK | OK |
| Create account | OK | OK | OK |
| Recovery phrase | OK | OK | OK |
| Split the phrase | OK | OK | OK |
| Phrase check | OK | OK | OK |
| Log in | - | - | - |
| Recover / forgot password | - | - | - |
| Restore identity on a fresh device | - | - | - |
| Mint a fresh phrase (lost everything) | - | - | - |
| Password explainer | - | - | - |

## Chats

| Screen | Web | iOS | Android |
|---|---|---|---|
| Chats, empty | OK | OK | OK |
| Chats, populated | OK | OK | OK |
| Conversation | OK | OK | OK |
| New chat / contact picker | - | - | - |
| Message action menu (edit, delete, react) | - | - | - |
| Edit message | - | - | - |
| Emoji and GIF picker | - | - | - |
| Media bubbles, voice and video notes | - | - | - |
| Video player | - | - | - |
| Delete chat | - | - | - |

## Groups

| Screen | Web | iOS | Android |
|---|---|---|---|
| Groups list | OK | OK | OK |
| New group | OK | - | - |
| Group conversation | OK | - | OK |
| Members panel (roles, add, remove, invite link, transfer ownership) | - | - | - |
| Leave group | - | - | - |
| Join by group link | - | - | - |

## Contacts

| Screen | Web | iOS | Android |
|---|---|---|---|
| Requests | OK | OK | OK |
| Connected | OK | OK | OK |
| Contact detail (safety number, rename, remove) | OK | - | - |
| Invite links | - | - | - |
| Call links | - | - | - |
| Invite landing | - | - | - |

## Channels

| Screen | Web | iOS | Android |
|---|---|---|---|
| Channels, empty | OK | OK | OK |
| Find a channel | OK | OK | OK |
| Channel reader | OK | OK | **blocked** |
| New channel | OK | - | - |
| Manage channel | OK | - | - |
| Co-publisher join | - | - | - |

Android is marked blocked, not missing: the lookup by handle fails there, so the
reader cannot be reached at all (see `RUN-NOTES.md` finding 5).

## Calls

| Screen | Web | iOS | Android |
|---|---|---|---|
| Pre-join / camera check | - | - | - |
| Outgoing | - | - | **blocked** |
| Incoming | **blocked** | - | - |
| In call, one to one | - | - | - |
| In call, group | - | - | - |
| More menu and settings sheet | - | - | - |
| Participants | - | - | - |
| In-call chat | - | - | - |
| Safety number | - | - | - |
| Join by call link | - | - | - |

Nothing in this section is captured. A call placed from Android ended before a
call screen appeared and the web side never raised an incoming call, which is
the area `docs/PRODUCT-READINESS.md` already marks PARTIAL.

## Settings

| Screen | Web | iOS | Android |
|---|---|---|---|
| Settings root | OK | OK | OK |
| Profile | OK | - | OK |
| Passkeys | OK | - | OK |
| Devices | OK | OK | OK |
| Recovery factors | OK | - | OK |
| Appearance | OK | in root | in root |
| Media | OK | in root | in root |
| About | OK | in root | in root |
| Account / danger zone | OK | - | OK |
| Network / anti-censorship | **none** | - | - |
| Pair a device (show and scan a code) | - | - | - |

## Where it stands

Roughly 59 distinct screens across the three clients; 18 rows are captured, which
is about a third of them. The gaps fall into three kinds:

1. **Not captured yet** — most of the list. A second pass through the emulators
   reaches them; the sheets and menus (members panel, message actions, emoji
   picker, invite links) are the biggest block.
2. **Blocked by a defect** — the Android channel reader and everything under
   Calls. These cannot be captured until the defect is fixed, so they belong on
   the issue list rather than in a capture pass.
3. **Does not exist** — the web Welcome screen and the web Network section. The
   second is deliberate (anti-censorship cannot work in a browser); the first is
   a gap.
