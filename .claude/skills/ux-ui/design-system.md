<!-- SPDX-License-Identifier: AGPL-3.0-or-later -->
<!-- SPDX-FileCopyrightText: 2026 Lakko Contributors -->

# Lakko design system

The single source of truth for how Lakko looks, is organised, and speaks
to the user, on web (SolidJS), iOS (SwiftUI) and Android (Compose). Read
it before touching any user-facing screen. `SKILL.md` owns the process
(states, honesty rules, cross-platform consistency); this file owns the
decisions.

Lakko is a private messenger. The UI must feel like a messenger first
(Telegram / Signal class), calm and trustworthy, never like a crypto
demo. The security is the product's foundation, not its interface.

## 1. Brand

- **Name**: Lakko. Never "lakko.me" in product UI (the domain is not the
  brand).
- **Colour**: indigo, one hue. The old lakko_me palette paired indigo with
  violet (`#764ba2`, and the violet corner of the old icon); violet is
  gone everywhere, replaced by dark indigo. Decided by the maintainer on
  2026-09-14 after comparing a dark-indigo and a light-indigo accent:
  **the accent is dark indigo `#3730a3`** in the light theme.
- **Logo**: the white script "L" on the brand gradient tile. Source files
  in `docs/brand/`: `lakko-icon-512.png` (app icon / favicon source),
  `lakko-icon-128.png`, `lakko-glyph-512.png` (the L alone, transparent,
  for compositing). The glyph is a raster cut from the original icon;
  before a store submission trace it to a vector. Use the tile as the app
  icon, favicon, PWA icon, and once on the auth screens. Do not repeat it
  in every header.
- **Gradient**: `linear-gradient(135deg, #312e81 0%, #818cf8 100%)`, dark
  indigo to light indigo. It appears on the logo tile and on the left
  brand panel of the auth screens, in both themes. **Never** a page, card,
  button or bubble background. Surfaces are flat.
- **Auth screen**: split layout. Left 600px: the gradient panel with the
  logo, one headline, one sentence. Right: a card with the form on a light
  indigo ground (`#eef2ff` light theme; `#4338ca` dark theme with a
  `#1e1b4b` card).
- **Type**: the platform system font (`system-ui` / SF / Roboto). A
  messenger should feel native on each device; no web font.

## 2. Tokens

Names are the web CSS variables; iOS and Android mirror them one-to-one
(`Color("surfaceBg")` asset / `LakkoColors.surfaceBg`). Never hardcode a
hex in a screen, always a token.

### Colour, light

| Token | Value | Use |
|---|---|---|
| `--surface-bg` | `#f6f7fb` | page / window background (cool-tinted white) |
| `--surface-elevated` | `#ffffff` | cards, panes, composer, dialogs |
| `--surface-sunken` | `#eef0f6` | search fields, incoming bubbles, code |
| `--surface-hover` | `#f1f2f8` | list-row hover |
| `--surface-selected` | `#e0e7ff` | selected list row / nav item |
| `--surface-border` | `#e4e7ef` | hairlines, input borders |
| `--surface-fg` | `#1a1d2b` | primary text |
| `--surface-muted` | `#6b7280` | secondary text, timestamps |
| `--surface-faint` | `#9aa1b1` | placeholders, disabled |
| `--accent` | `#3730a3` | primary actions, links, selected icon, outgoing bubble |
| `--accent-strong` | `#312e81` | primary button hover / pressed |
| `--accent-fg` | `#0f1220` | text on accent |
| `--accent-soft` | `#e0e7ff` | accent tint (unread badge bg, avatar tint) |
| avatar tints | indigo `#e0e7ff`/`#3730a3`, teal `#e0f2f6`/`#0f6f80`, slate `#e9ebf3`/`#46506e`, sand `#f6efe3`/`#8a5a17` | chosen by hash of the id; no violet |
| `--success` | `#22c55e` | delivered / connected |
| `--warning` | `#b45309` | recoverable warnings |
| `--danger` | `#dc2626` | destructive actions, errors |
| `--danger-soft` | `#fef2f2` | danger surface tint |
| `--danger-fg` | `#ffffff` | text on a filled danger button |

### Colour, dark

| Token | Value |
|---|---|
| `--surface-bg` | `#0f1220` |
| `--surface-elevated` | `#171b2e` |
| `--surface-sunken` | `#1f2439` |
| `--surface-hover` | `#1c2136` |
| `--surface-selected` | `#26295a` |
| `--surface-border` | `#2a3145` |
| `--surface-fg` | `#e6e9f4` |
| `--surface-muted` | `#9aa3bd` |
| `--surface-faint` | `#6c7590` |
| `--accent` | `#818cf8` (light indigo; dark indigo does not read on a dark ground) |
| `--accent-strong` | `#a5b4fc` |
| `--accent-fg` | `#0f1220` |
| `--accent-soft` | `#26295a` |
| avatar tints | indigo `#26295a`/`#a5b4fc`, teal `#12313a`/`#6fd0e0`, slate `#252a3d`/`#aeb6d0`, sand `#3a2f17`/`#e5b96b` |
| `--success` | `#4ade80` |
| `--warning` | `#f59e0b` |
| `--danger` | `#f87171` |
| `--danger-soft` | `#3a1f24` |
| `--danger-fg` | `#14080a` |

Theme switch: web `data-theme` on `<html>` (system / dark / light);
iOS and Android follow the system and expose the same override in
Settings > Appearance.

### Shape, space, type

| Token | Value |
|---|---|
| `--radius-sm` | 8px (inputs, buttons, chips) |
| `--radius-md` | 12px (cards, panes, dialogs) |
| `--radius-lg` | 18px (message bubbles) |
| `--radius-pill` | 999px (badges, avatars, pills) |
| `--space-1..8` | 4 / 8 / 12 / 16 / 20 / 24 / 32 / 40 px |
| `--text-xs` | 12px / 16 (timestamps, badges) |
| `--text-sm` | 13px / 18 (secondary, list previews) |
| `--text-md` | 15px / 22 (body, messages, inputs) |
| `--text-lg` | 17px / 24 (list titles, section titles) |
| `--text-xl` | 22px / 28 (screen titles) |
| `--text-2xl` | 28px / 34 (auth headline) |
| weights | 400 body, 500 labels / list titles, 600 headings, 700 only the auth headline |
| `--shadow-1` | `0 1px 2px rgba(16,20,40,.04), 0 1px 3px rgba(16,20,40,.06)` |
| `--shadow-2` | `0 8px 24px rgba(16,20,40,.12)` (dialogs, popovers) |
| control height | 40px desktop / 44px touch minimum |
| content max width | 720px for settings / forms; auth card 400px |

## 3. Components

One vocabulary, three implementations. Names are the web component
names; iOS / Android use the same names.

- **Button**: `primary` (accent fill, white text), `secondary` (elevated
  surface, border), `ghost` (no border, muted text), `danger` (danger
  text, danger border; filled only inside a confirmation dialog). 40px
  high, radius-sm, 15px/500. Loading state replaces the label with a
  spinner, never disables silently.
- **Input**: 40px, radius-sm, border `--surface-border`, focus ring 2px
  accent at 40% alpha. Label above (13px/500). Helper or error text below
  (13px, muted or danger). Errors are inline, never `alert()`.
- **Search field**: sunken surface, no border, leading search icon,
  radius-pill.
- **Avatar**: circle, initials on an accent-soft / secondary tint chosen
  by hash of the id, image when a profile photo exists. Sizes 28 / 40 /
  56 / 96.
- **List row**: 64px (chat list) or 52px (settings), 12px horizontal
  padding, hover `--surface-hover`, selected `--surface-selected`, no
  dividers between rows (whitespace does the work), a hairline only
  between groups.
- **Badge**: unread count, accent fill, white 12px/600, pill; muted grey
  when the chat is muted.
- **Message bubble**: radius-lg, 15px text, 10px 14px padding; outgoing =
  accent fill + accent-fg; incoming = sunken surface + fg. Timestamp and
  delivery state (sent / read) 11px inside the bubble corner. Max width
  65% of the pane.
- **Composer**: elevated surface, hairline top, attach button, growing
  text field (radius-pill, sunken), mic / send swap depending on content.
- **Card / section**: elevated surface, radius-md, hairline border, 20px
  padding, a 17px/600 title, optional 13px muted description under it.
- **Dialog**: radius-md, shadow-2, 440px, title + body + right-aligned
  actions (ghost cancel, primary or danger confirm). Destructive dialogs
  spell out the consequence in one sentence.
- **Banner**: inline, sunken surface, icon + one sentence + one action.
  Used for the incoming-call prompt, update notice, identity change.
- **Empty state**: centred, one line of muted text and one primary
  action. No illustration needed.
- **Nav rail (desktop web)**: 72px, icons with 11px labels, selected =
  accent icon on accent-soft pill. Tablet / phone collapses to a bottom
  tab bar (native) or a drawer (web narrow).

## 4. Information architecture

Top-level destinations, in this order, identical on the three clients:

1. **Chats**: the home. Every conversation, one-to-one, groups and
   channels, in one list (search, pinned, unread). Empty state: "No
   conversations yet" + "New chat".
2. **Feed**: messages and posts from groups and channels, ranked by
   preset or personal rules, plus channel suggestions (`docs/ux/feed.md`).
3. **Contacts**: accepted contacts, incoming requests, add by username,
   invite link.
4. **Calls**: recent calls, "Create call link", join by link.
5. **Settings**: Profile · Privacy · Security · Notifications · Chat folders ·
   Appearance · Language · Data · About.

Settings sections:

- **Profile**: avatar, display name, `@username`.
- **Privacy**: who can find me by username or name, and so who can ask to
  be a contact ([`docs/ux/who-can-find-me.md`](../../../docs/ux/who-can-find-me.md)).
  About people, not keys, so not under Security.
- **Security**: passkeys (list + add), linked devices (list, link new via
  QR, remove), recovery phrase (view once / verify), two-step (TOTP),
  recovery contact.
- **Notifications**: push on/off, previews, sounds, per-chat mutes.
- **Chat folders**: the person's folders (order, create, edit, delete) and
  suggested folders; see `docs/ux/chat-folders.md`. The folder bar above the
  Chats list appears once one folder exists.
- **Appearance**: theme system/light/dark, text size, autoplay media.
- **Language**: "Same as system" + the supported languages, each titled
  in its own name; switches the whole UI at once, stored on this device
  only. Spec: [`docs/ux/localization.md`](../../../docs/ux/localization.md).
- **Data**: encrypted backup, export my data, storage usage.
- **About**: version, licences, privacy, "How Lakko protects you" (the one
  place the crypto is explained, in plain words).
- **Account (danger zone)**: at the bottom of Settings, visually
  separated: "Log out", "Forget this device" (with one sentence on what
  it does), "Delete account" (behind a typed confirmation).

Rules:

- A destructive action is never on a landing screen and never one click
  away from login.
- Developer tooling (`/dev`, probes, raw state) is behind a build flag or
  a direct URL, never linked from product UI.
- Roadmap, phase and sprint identifiers never appear in the UI.

## 5. Voice and copy

Plain English, second person, short sentences, sentence case, no
exclamation marks. Say what happens, not how it is implemented.

**Never in product UI** (labels, subtitles, helper text, errors, tooltips):
OPAQUE, PAKE, OPRF, MLS, KeyPackage, Welcome, DS / delivery service,
journal, pseudonym, epoch, commit, proposal, exporter, HPKE, BLAKE3,
Ed25519, SFrame, KT log, rendezvous, "by design", phase / sprint / ticket
ids, crate or function names.

**Say instead**:

| Instead of | Say |
|---|---|
| "OPAQUE-PAKE login. Wrong username and wrong password produce the same error by design." | (nothing; the login screen needs no subtitle) |
| "The server only sees an OPRF-blinded envelope." | "Your password never leaves your device." |
| "Start MLS group" | "Start chat" / "New group" |
| "Pairing is E2EE: the server sees only the rendezvous pseudonym + opaque MLS bytes." | "Linking is end-to-end encrypted. Lakko's servers cannot read it." |
| "Removing a device revokes its access via an MLS commit." | "This device will lose access to your messages." |
| "Refused by design" | "Not allowed from a signed-in session. Use your recovery phrase instead." |
| "Identity mismatch / supersession refused" | "Alex's security code changed. Verify it before continuing." |

Error copy: one sentence on what went wrong, one on what to do. A
fail-closed refusal (invariant #4) reads as a clear stop ("Couldn't
establish a secure connection. Nothing was sent."), never as a shrug.

Security is surfaced where the user benefits: the E2EE lock in a call
when it is real, the safety number, "Verify security code" on a contact,
the recovery phrase warning. Everywhere else it is silent.

## 6. Cross-platform mapping

| Concept | Web | iOS | Android |
|---|---|---|---|
| tokens | `styles.css` CSS variables | `Assets.xcassets` colour sets + `LakkoTheme` | `ui/theme/Color.kt` + `LakkoTheme` (Material 3 scheme built from tokens) |
| top-level nav | nav rail / drawer | `TabView` | `NavigationBar` |
| list row | `ListRow` | `List` row with `.listRowInsets` | `ListItem` |
| dialog | `ConfirmDialog` | `.confirmationDialog` / `.alert` | `AlertDialog` |
| long-press menu | right-click / hover menu | context menu | long-press dropdown |
| back nav | breadcrumb / browser back | `NavigationStack` | system back |

Platform-idiomatic divergence is allowed in mechanics (gesture, transition,
system controls), never in vocabulary, states or IA.

**No operating-system surface inside a Lakko screen unless the platform leaves
no other way.** A menu, a sheet, a confirmation or a way to hand something out
is drawn by the app, with the app's own components and words. The system share
sheet (`navigator.share`, `UIActivityViewController` / `ShareLink`, an
`ACTION_SEND` chooser), the browser's `confirm` / `alert` / `prompt`, and a
stock system dialog where an app menu would do are all out: they look foreign,
differ on every platform and offer choices nobody asked for. A link is handed
out by `Copy link` and, where it makes sense, by sending it to a contact inside
the app. Allowed, because nothing else can do the job: permission prompts, the
file and photo picker, the passkey sheet, the system's incoming-call screen and
notifications. The web client enforces the first two with tests
(`ui/no-system-share.test.ts`, `ui/no-window-confirm.test.ts`).

## 7. Language and direction

Every user-facing string comes from the shared catalog, never a literal
in a screen (see [`docs/ux/localization.md`](../../../docs/ux/localization.md)).
Layout is written in logical terms (start / end, never left / right) so
Arabic, Persian and Hebrew mirror without per-screen work. Text a person
wrote is laid out in its own direction and isolated from surrounding UI
text; usernames, links, the safety number and the recovery phrase are
always LTR with Latin digits.
