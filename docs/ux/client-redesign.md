<!-- SPDX-License-Identifier: AGPL-3.0-or-later -->
<!-- SPDX-FileCopyrightText: 2026 Lakko Contributors -->

# UX spec: client redesign (web, iOS, Android)

Status: built, phases A to E landed 2026-09-16; kept as the reference for the client shape. Written after the first native live run, which the
maintainer stopped: the clients do not look or behave like one modern messenger.
This spec is the contract every client is rebuilt against. It defines the
navigation model, the visual system, the per-screen shape and the copy rules.
Feature semantics (membership, recovery, media, second device) stay as their
own UX specs define them; this document changes how they are presented, not
what they do.

Order of work agreed with the maintainer: redesign, then stabilisation, then a
verified readiness table, then the next feature arc.

Progress: Phase A (section 8) landed on all three clients on 2026-09-16 and
was checked by screenshots on the iOS simulator, the Android emulator and
the web test stack. Known leftovers from that pass, all unbuilt: the iOS
accent still renders as the system blue (the asset catalog colour is not
applied to the tab and bubble tint); the iOS tab bar stays visible inside a
chat; no client persists a conversation kind, so a group created with two
people lists under Chats until a third person joins; the shared display-name
resolver still falls back to a key fingerprint for a person without a name;
Android has no app icon and iOS has an empty icon set; the web row actions
have no touch path.

Phase B landed on all three the same day and was checked live: rows with
avatar, time, preview and unread badge, search, swipe and long-press actions,
date separators, message times, delivery glyphs, run grouping, the tab bar
hidden inside an iOS chat, a persisted conversation kind on every client,
and the iOS tint moved onto the tokens directly (the catalog accent only
applies on a fresh install). A web-to-Android exchange and an iOS-to-Android
exchange rendered correctly in the new timelines. Leftovers from this pass:
a two-person group created by someone else lists under Chats on the joiner
(the kind is local, not carried by the Welcome or the pairing bundle);
Android swipe covers pin and archive only, mute stays in the long-press menu;
no loading skeleton; a chat title can show a key fingerprint for the moment
between the Welcome and the contact acknowledgement; the iOS composer still
drops the first burst of typed characters on a freshly opened chat (a
stabilisation item, not a design one); Enter-to-send on web was not verified
by hand.

Stabilisation pass (same day, natives): the iOS shell no longer observes
whole repositories (a non-observable services bundle, Equatable root views),
which removed the flicker and the lost first keystrokes; a 58-character
burst typed immediately after opening a chat landed in full. Android moved
every MLS, FFI and persistence section of the repository onto a background
dispatcher with a debug-only main-thread guard, joins an in-flight poll
instead of spinning or sleeping, and bootstraps off the main thread; a chat
opens without an ANR. Time to first frame on Android did not improve in the
debug build (process start and the first Compose composition dominate).
Still open: the native QR pairing and re-admission were not run on a device
(no camera on the simulator or emulator, no camera-free path on the
natives); iOS still decrypts and persists on the main actor inside the poll.

Phase C landed on all three the same day: the Contacts tab as a searchable
list with a Requests section, a collapsed Sent group and letter sections,
an add menu (username, invite link, call link; no contact QR flow exists on
any client), invite links as rows with Copy / Share / Revoke, a contact
detail with Message / Call / Video, Verify safety number, Rename and Remove
(Remove was Android and web only until 2026-09-23, when iOS built the
operation it had been carrying a comment denying the existence of), a
Recovery screen and an About section in Settings, a device list with one
Add-a-device flow, and a neutral "Member" / "Someone" label instead of a key
fingerprint. The live pass through the new screens exposed three contact
defects that predate the redesign (recorded in docs/PRODUCT-READINESS.md):
the web signed requests with the device identity when a route lacked the
account provider, iOS discarded acknowledgements, and both natives kept the
inbox cursor in memory only. Still open after Phase C: "Mark as verified"
(no store), a delete-account action on Android, device added dates, the
"Message" action label wrapping on the Android detail.

Phase D landed on all three the same day: a Welcome screen on the natives
(web keeps the login card as its entry), log in with a password toggle, a
passkey button and a plain-language "How Lakko protects your password"
disclosure, sign-up with a guided phrase step (numbered two-column grid,
Copy, a three-word check at random positions before Continue, social
recovery behind More options), recovery methods as rows, and the restore
prompt on the tokens. Both native agents found and fixed the same defect:
the phrase screen after sign-up was unreachable because the session state
replaced the anonymous root first. Three-word recovery has no native client
flow, so the row is web-only. Screenshots: iOS Welcome on a spare simulator
and the web screens; Android's anonymous screens were not captured (the
only emulator holds the test account and the disk had no room for a second
one).

Phase E landed on all three the same day: Channels in the list vocabulary
(avatar, name, handle, subscriber count when known, last post time, search,
a compose menu), posts as cards with a chat-shaped publish composer and a
Subscribe action for readers, a Manage screen for owners (subscribers as
"Subscriber N", co-publisher requests, an invite-link row, one plain line
of key-rotation status), the internal channel routes moved onto the app
shell on Android, and the call surfaces on the dark half of the tokens with
56 px round controls and a prejoin card; the second palettes are gone on
every client. A channel post carries no author on the wire, so no author
line exists. Known and unchanged: the channel list is in-memory on every
client and empties on relaunch; hand-raise and screen-share stay mock-backed.
Every phase of section 8 is now built; what remains is the per-row
verification in docs/PRODUCT-READINESS.md, which is the product truth.

## 1. What is wrong today

Findings from a code inventory of the three clients (2026-09-16).

Navigation
- iOS and Android open on a "home" screen that is a vertical stack of seven
  full-width buttons (Contacts, Chats, Channels, Passkeys, Devices, Profile,
  Anti-censorship) plus Log out and Forget device. Every screen returns to it.
- Neither native uses the platform navigation container: iOS switches a
  `@State` enum with a hand-made "Home" toolbar button in each leaf screen;
  Android switches a destination state with a `"<- Back"` text button and has
  no system back handling at all. Opening a chat from Contacts and pressing
  Back lands on the chat list.
- Web has a navigation rail with five destinations (Chats, Contacts, Channels,
  Calls, Settings); Calls has no history, only a create-link panel.

Visual system
- Android has no theme: a bare `MaterialTheme {}` renders the Material 3
  baseline purple in light mode only, while Calls and Channels each carry a
  private hard-coded dark palette. No edge-to-edge handling, so headers sit
  under the status bar.
- iOS has no asset catalog, no accent colour, no app icon set; Channels forces
  dark mode on itself, Calls hard-codes the same dark palette a second time,
  every other screen follows the system.
- Web has real tokens (light and dark), an inline SVG icon set and a two-pane
  layout, but Channels bypasses the tokens with inline styles and a local
  colour map, long strings never wrap, and the chat header keeps 32 px side
  padding down to phone width.

Icons and copy
- Android uses emoji as icons (pin, mute, archive, call, video, attach,
  emoji picker) and text buttons everywhere else; Home shows the raw base64
  account id and a "Contacts (1.15e) and 1x1 chat (1.15f) are live" hint.
- iOS shows the raw account id on Home, identifies groups by a hex prefix,
  and puts five buttons in the chat toolbar.
- All three explain OPAQUE-PAKE on the login and signup screens, mention MLS,
  rendezvous pseudonyms, epochs and cursors in end-user copy, and show
  signature-key hex prefixes as member and device labels.
- Every client carries manual Refresh buttons (chat, contacts, devices,
  channel subscribers) that predate the automatic receive loops.

Timeline
- Neither native renders a message time. No date separators, no avatars, no
  grouping. Read state is the text "checkmark Sent" under own bubbles.

## 2. Principles

1. One product, three native shells. The information architecture, the
   states and the copy are identical; the controls are the platform's own.
2. The platform owns navigation. iOS: `TabView` and `NavigationStack` with
   the system back button and edge swipe. Android: Navigation Compose with a
   Material 3 `NavigationBar`, `TopAppBar` and predictive back. Web: the
   router and browser history.
3. Nothing the app does by itself gets a button. Receiving is automatic on
   every client, so there is no Refresh anywhere. Natives keep
   pull-to-refresh on the chat list and contact list as the platform gesture
   users expect, and it runs the same poll the app already runs.
4. No protocol in the interface. Users see names, avatars, usernames and
   safety numbers. They never see MLS, OPAQUE, PAKE, OPRF, epochs, cursors,
   leaf indices, pseudonyms, hex keys, base64 ids or sprint numbers.
5. Icons are icons. SF Symbols on iOS, Material Symbols on Android, the
   existing inline SVG set on web. Emoji appear only as content (messages,
   reactions, the picker).
6. Honest indicators, as before: no lock or "encrypted" chip unless the
   surface is actually end-to-end encrypted, no fake presence, no fake
   quality.

## 3. Navigation model

Five primary destinations, in this order, on every client:

| Destination | Contents |
|---|---|
| Chats | One-to-one conversations: the list, new chat, the chat screen |
| Groups | Group conversations: the list, new group, the group chat screen, group info |
| Channels | Channel list, channel view, create, subscribe |
| Contacts | Contact list, requests, add by username, invite links, QR, contact detail |
| Settings | Profile, security (passkeys, devices, recovery), appearance, media, network, account |

Chats and Groups are two lists over the same conversation store, split by
kind. The kind is set when the conversation is created and never changes
([ADR-0060](../adr/0060-conversation-kind.md)): a conversation started with
one person (contact accept, New chat) is a chat, a conversation started
through the new-group flow is a group - even while it has two people. Every
device reads the kind from the conversation itself, so it is the same on
every member's device; it is never derived from how many people are in it.

A chat is always the same two people. Nobody can be added to it or removed
from it; each person's own devices still come and go. Adding somebody starts
a new group.

- iOS: bottom `TabView`; each tab is its own `NavigationStack`.
- Android: Material 3 `NavigationBar`; one `NavHost` per tab or a single host
  with tab-level start destinations. Predictive back enabled.
- Web: the existing left rail on wide screens, bottom tab bar below 720 px,
  with the same five destinations. The rail's "Calls" destination is
  removed; call links move to the Contacts "+" menu ("Create call link")
  and to the chat header.
- The app opens on Chats. There is no home screen.
- Devices, passkeys, recovery factors and anti-censorship are rows inside
  Settings, never primary destinations.
- Deep links (`/i/`, `/g/`, `/c/`) open as a sheet or a pushed screen over
  the current tab, not as a replacement of the whole app.

Back behaviour: always to the screen the user came from. A chat opened from a
contact detail returns to that detail.

## 4. Visual system

One token set, defined once per platform from the same values. The web
`styles.css` tokens are the source; iOS gets an asset catalog with light and
dark variants, Android gets a `lightColorScheme` and `darkColorScheme` built
from the same values (dynamic colour off, so the brand is the same on every
device).

Colour (light / dark):

| Token | Light | Dark |
|---|---|---|
| background | #F6F7FB | #0F1220 |
| surface | #FFFFFF | #171B2E |
| surface sunken | #EEF0F6 | #1F2439 |
| surface selected | #E0E7FF | #26295A |
| border | #E4E7EF | #2A3145 |
| text | #1A1D2B | #E6E9F4 |
| text muted | #6B7280 | #9AA3BD |
| accent | #3730A3 | #818CF8 |
| accent foreground | #FFFFFF | #0F1220 |
| accent soft | #E0E7FF | #26295A |
| outgoing bubble | accent | accent |
| incoming bubble | surface sunken | surface sunken |
| danger | #DC2626 | #F87171 |
| success | #16A34A | #4ADE80 |

- Light and dark follow the system on all three; web keeps its
  system / light / dark switch in Settings, natives add the same switch.
- Calls are dark by design on every platform and use the dark half of the
  same tokens. Channels use the normal scheme; the forced dark mode and the
  two duplicated palettes are deleted.
- Type: the platform system font (SF Pro, Roboto, system-ui). Sizes come from
  the platform ramp (Dynamic Type, Material type scale) mapped to the web
  ramp: title 22, section 17, body 15, meta 13.
- Shape: radius 8 for controls, 12 for cards and rows, 18 for bubbles,
  pill for badges. Consistent on all three.
- Spacing: 4 px base, 8 / 12 / 16 / 24 steps. Page side padding 16 on phones,
  24 on wide layouts.
- Avatars: initials on one of four tints derived from the identity (the web
  `Avatar` rule); groups get a two-letter initial from the group name or a
  group glyph when unnamed. Sizes 40 (rows), 32 (headers), 72 (details).

Platform-specific layout hygiene, all required:
- Android: `enableEdgeToEdge()`, every screen inside a `Scaffold`, content
  padded by the scaffold insets, status and navigation bar transparent.
- iOS: every screen scrolls; no fixed `VStack` pages; toolbars use the
  system layout, at most two trailing items plus an overflow menu.
- Web: `overflow-wrap: anywhere` on message text, names and ids; the chat
  header padding follows the page breakpoints; the split breakpoints and the
  rail breakpoint are the same value (720 px).

## 5. Screens

### 5.1 Chats list and Groups list

One list component, used twice. Everything below applies to both; the
differences are the title, the compose action and the row title rule.

- Top bar: title "Chats" or "Groups", a search field (inline on Android and
  iOS, in the pane head on web), a compose action: New chat (opens the
  contact picker) on Chats, New group (the docs/ux/new-group.md flow) on
  Groups.
- Row: avatar, name (bold when unread), time of the last message on the
  right, last message preview on the second line (sender name prefix in
  groups, "Photo", "Voice message", "GIF" for media), an unread count badge,
  a small muted icon when muted, a pin glyph when pinned. Pinned rows sort
  first.
- Row actions: swipe on natives (pin, mute, archive), hover actions and a
  context menu on web. Never three permanent buttons in the row.
- States: loading skeleton on first open; empty state with an illustration
  glyph, "No chats yet" and "Start a chat" on Chats, "No groups yet" and
  "Create a group" on Groups; the single "Synchronizing..." label on both
  lists while the device is still being admitted to the account's
  conversations (docs/ux/second-device.md section 2).
- Row title: the contact's name on Chats; the group name on Groups, and a
  group without a name is titled from its members ("Anna, Ivan, Maria" and
  "+2" beyond three), never from a hex id.
- A new conversation appears in the list its kind belongs to and stays
  there: the kind never changes, so nothing ever moves between the two
  lists.

### 5.2 Chat

- Top bar: system back; avatar + name; second line: "@username" in a 1x1
  chat, "N members" in a group, "typing..." while a typing indicator is live.
  Trailing: call, video call, overflow menu (Members or Group info, Search
  later, Mute, Leave). On phone widths the two call actions stay and
  everything else is in the overflow.
- Timeline: date separators; consecutive messages from one sender within
  five minutes are grouped (sender name and avatar once per group, in
  groups only); each bubble carries the time in the corner and, for own
  messages, a delivery glyph: one check sent, two checks read. Incoming
  bubbles left on the sunken surface, outgoing right on the accent. Own
  messages sent from another device of the same account are outgoing.
- Reactions as chips under the bubble. The reaction bar and the message
  actions (Reply later, Edit, Delete, Copy) open on a **deliberate** gesture
  and on nothing else: a right-click on web, a long-press on iOS and Android.
  **Never on hover.** Hover is not an act - a pointer crossing a bubble on its
  way somewhere else asks for nothing - and a strip that appears over the
  bubble under the pointer takes the click the person was actually making. It
  did exactly that to a voice message: the play button sat under the strip, so
  pressing play paused it again and the message could not be listened to at
  all. That strip is why nothing may ever appear over a bubble's controls
  WITHOUT a deliberate gesture - and there is no such thing left. The menu a
  right-click or long press opens is different: on web it opens at the pointer
  and over the message, the way Telegram's does, because the person has just
  acted on this message and a menu placed off to its side makes them travel to
  it; the only thing that moves it is the viewport's edge. A keyboard reaches
  it with the context-menu key or Shift+F10 on the focused bubble, which are
  the same deliberate act.
- The deliberate gesture reaches the menu from **anywhere on the bubble,
  including over the bubble's own controls**, and a press long enough to be
  that gesture is not also a tap on the control underneath it. Both halves are
  the rule, and the second is what makes the first safe: a long press that
  opened the menu AND seeked the audio under it would have done something
  nobody asked for. Written down because "opens on a long press" was read as a
  property of the bubble while every control inside it kept claiming the press
  for itself. On Android that left a voice message reachable only by the
  padding no control covered - the waveform seeked and the play disc played,
  whatever the press's duration - where iOS had already given its waveform a
  rule refusing a press long enough to open the menu. A control that answers a
  tap must therefore decline a press that outlives the platform's long-press
  threshold, and must let that press through to the bubble.
- **Once the menu has opened, the control underneath it receives nothing more
  from that press** - not a tap on release, and not a drag. A finger still down
  when the menu appears has already been answered: the person asked for the menu
  and got it, so anything further from the same press is the app acting on an
  instruction nobody gave. The case this decides is a press held over a voice
  message's waveform and then slid sideways, which would open the menu AND move
  the playhead under it. Two separate gestures stay two separate outcomes - a
  long press opens the menu, a drag along the waveform seeks - and this says
  only that one press may not produce both. As far as the control is concerned
  the press ends at the moment the menu appears.
- Message actions and the composer keep the ADR-0023, ADR-0024 and ADR-0047
  behaviour; only the chrome changes.
- Composer: attach, text field growing to five lines, emoji or GIF picker,
  a trailing button that is a microphone with an empty draft and a send
  arrow otherwise. Voice and video note recording as docs/ux/media-messages.md
  specifies.
- Membership states (request pending, withdrawn, evicted) keep the shapes of
  docs/ux/proposal-based-membership.md; their copy is rewritten without
  protocol terms.
- States: loading spinner only on first open; "No messages yet. Say hi." as
  the empty state; a plain error bar for a send failure with a retry.

### 5.3 Contacts

- Top bar: "Contacts", search, an add action with a menu: Add by username,
  Share invite link, Show my QR code, Scan QR code, Create call link.
- Sections: Requests (incoming, with Accept and Decline on the row; sent
  requests as a collapsed "Sent" group with a waiting label), then the
  alphabetical contact list with section index letters on natives.
- Contact detail: avatar, name, @username; actions Message, Call, Video;
  a "Verify safety number" row that opens the number and the QR; Rename;
  Remove. No key fingerprints on the detail; the safety number is the
  verification surface.
- Invite links are managed in a sheet ("Invite links": active links with
  copy, share and revoke), never as a textarea with a raw URL.

### 5.4 Channels

- Same list and detail vocabulary as chats: avatar, name, @handle,
  subscriber count, last post time.
- Channel view: posts as cards with time; the publish composer for owners.
- Subscribers, co-publishers and rotation live under a "Manage" screen for
  owners. Key material, epochs, cursors and seed status are not shown; an
  unreadable post renders "Not available" with a one-line reason.

### 5.5 Settings

- Header: avatar, name, @username; tapping opens Profile.
- Sections and rows:
  - Account: Profile (name), Username.
  - Security: Passkeys, Devices, Recovery (phrase, three-word vault,
    authenticator, email or phone), Safety numbers explanation.
  - Appearance: theme (System, Light, Dark).
  - Media: autoplay video notes.
  - Network (natives only): Anti-censorship toggle and status.
  - About: version, licences.
  - Danger zone: Log out, Forget this device, Delete account, each with the
    confirmation the current flows already have.
- Devices screen: the list of this account's devices (name or platform,
  "This device" marker, added date, Remove), one "Add a device" button that
  opens the pairing flow (show QR or scan QR, and the another-device prompt
  of docs/ux/identity-recovery.md). The five-way segmented control is
  removed; TOTP and contact enrolment move under Recovery.

### 5.6 Auth

- Welcome: brand mark, one line, "Create account" and "Log in".
- Log in: username, password, Log in, "Use passkey", "Forgot password?".
  No paragraph about how the protocol works; a small "How Lakko protects
  your password" link may open an explainer sheet.
- Sign up: username with the live availability hint, name, password,
  confirm password; then the recovery phrase step as a guided screen
  (numbered words in a grid, copy, confirm by re-entering three words).
- Recover: a list of methods (Recovery phrase, Another device, Three words,
  Authenticator, Email or phone) as tappable rows, not a segmented control.
- Copy rule for every auth error: say what to do next, not what failed
  internally.

### 5.7 Calls

- Existing call UI is kept; it is restyled from the shared dark tokens and
  loses the second copy of the palette. Raised hand becomes an icon.

## 6. Copy rules

- Never on screen: MLS, OPAQUE, PAKE, OPRF, KeyPackage, epoch, cursor, leaf,
  pseudonym, rendezvous, commit, proposal (as a protocol word), sprint or
  version numbers such as "1.15e", hex or base64 identifiers.
- Identity is shown as name and @username. Verification is the safety
  number, shown in the contact detail and the group member detail.
- Loading and progress use one word each: "Loading...", "Sending...",
  "Synchronizing...".
- Errors are one sentence plus an action. Example: "Could not send. Retry."
- Empty states are one sentence plus one action.

## 7. Data the UI needs from the core and the repositories

- Message time (`created_at` from the DS row) surfaced per message on all
  three; the natives already carry it in their message records.
- A display name and avatar tint for every account key: from the profile
  envelope (ADR-0048), the contact rename, or the username; the group title
  derived from member names when the group has none.
- Delivery state per own message (sent, read) as today.
- A conversation kind (chat or group) per conversation, read from the
  conversation itself where its creator set it (ADR-0060), so the two lists
  split the same way on every device without a server change.
- Device rows with a human label (platform and model, or a user-set name)
  and an added date; the leaf index and key stay internal.
- Nothing new from the server.

## 8. Build order

Phase A, foundations, all three clients in parallel:
- tokens and theme (iOS asset catalog, Android colour schemes, web token
  cleanup in Channels), dark mode everywhere;
- the navigation shell (tabs, native back, no home screen);
- icons instead of emoji and text; the Refresh buttons removed; pull-to-
  refresh on the two lists;
- insets and scrolling hygiene; copy pass removing protocol terms and raw
  ids.

Phase B: the Chats and Groups lists and the chat screen (rows, timeline
with time, grouping, delivery glyphs, header, composer).

Phase C: contacts and settings (including the devices screen).

Phase D: auth and recovery screens.

Phase E: channels and calls restyle.

Each phase ends with a screenshot pass on all three clients and an update of
docs/PRODUCT-READINESS.md; stabilisation of the native receive paths follows
Phase B, before Phase C.
