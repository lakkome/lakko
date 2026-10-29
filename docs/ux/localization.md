<!-- SPDX-License-Identifier: AGPL-3.0-or-later -->
<!-- SPDX-FileCopyrightText: 2026 Lakko Contributors -->

# Interface language

Status: accepted with ADR-0061, 2026-09-28. Decisions taken by the maintainer on
2026-09-28: the first language set below, system language by default with
English as the fallback, language packs built into the app plus signed
updates, machine translation with human review of security-critical
strings. The architecture is [ADR-0061](../adr/0061-interface-localization.md).

## 1. User goal and entry points

A person reads Lakko in their own language, including a right-to-left one,
and can change it at any moment without restarting the app.

Entry points:

- **First launch.** No choice to make: Lakko shows the system language if
  it is in the supported set, otherwise English.
- **Auth screens.** A language button in the top corner of the sign-in /
  sign-up screens (globe icon + the current language's own name). A person
  who cannot read English must be able to switch before creating an
  account.
- **Settings > Language.** A top-level Settings row, placed after
  Appearance. The row's trailing text is the current language in its own
  name ("Deutsch", "فارسی").

## 2. Supported languages, first set

| Code | Row title (own name) | Row subtitle (in the current UI language) | Direction |
|---|---|---|---|
| `en` | English | English | LTR |
| `ar` | العربية | Arabic | RTL |
| `es` | Español | Spanish | LTR |
| `fa` | فارسی | Persian | RTL |
| `he` | עברית | Hebrew | RTL |
| `hi` | हिन्दी | Hindi | LTR |
| `pt-BR` | Português (Brasil) | Portuguese (Brazil) | LTR |
| `ru` | Русский | Russian | LTR |
| `tr` | Türkçe | Turkish | LTR |
| `uk` | Українська | Ukrainian | LTR |
| `zh-Hans` | 简体中文 | Chinese (Simplified) | LTR |

Hebrew and Chinese are in the first set so RTL and CJK input and layout
are proven early. More languages are added by adding a catalog file; no
screen changes.

Unlike Telegram (English name as title, own name as subtitle), the **own
name is the title**: a person looking for their language recognises it
in its own script, and the title must not change when the UI language
changes.

## 3. Flow: Settings > Language

1. Settings > Language opens a list screen.
2. Top: a search field (matches both the own name and the translated
   name, case- and diacritic-insensitive).
3. First row: **"Same as system"** with the resolved language in the
   subtitle ("Same as system · Deutsch"). Selected by default.
4. Then the languages, sorted by the translated name in the current UI
   language. A check mark on the selected row.
5. Tap a row: the whole interface switches immediately, on this screen
   too (the list re-sorts, the screen's own title changes, the layout
   flips if the direction changed). No confirmation, no restart, no
   "Apply" button. Back navigation stays on the same stack.
6. Choosing "Same as system" returns to following the system; when the
   system language later changes, Lakko follows.

States:

- **Default**: the list, current choice checked.
- **Search, no match**: "No languages found" (empty state, no action).
- **Switching**: no visible state. On web the language file loads before
  the switch is shown; if it takes longer than a moment the tapped row
  shows a spinner in place of the check mark and the rest of the UI stays
  in the old language. Never a half-translated screen.
- **Language file failed to load (web only)**: the UI stays in the old
  language, the row shows the inline error "Couldn't load this language.
  Check your connection and try again." Tapping again retries.

The auth-screen language button opens the same list as a sheet / dialog.

## 4. Scope of the setting

- **Per device.** The choice is stored on this device and is never sent
  to Lakko's servers. It does not follow the account to another device.
  (A language is personal data about the person; keeping it off the
  server is the same rule as not storing a phone number.)
- It survives sign-out, and "Forget this device" does not reset it: it
  is a device preference, not account data.
- Android 13+: the choice is the system per-app language, so it also
  appears in system Settings > Apps > Lakko > Language and a change there
  is reflected in Lakko. iOS: the system per-app language (Settings >
  Lakko > Language) is also honoured; a choice made inside Lakko
  overrides it for Lakko only.
- **iOS permission prompts are the exception.** The camera, microphone
  and photo-library prompts are shown by iOS, not by Lakko, in the
  language iOS uses for Lakko: Settings > Lakko > Language if set, else
  the system language. A language chosen only inside Lakko does not reach
  them; the prompt text itself is translated (ADR-0061).
- **Platform words.** A string names the platform's own controls: on
  iOS "Open Settings" and Touch ID / Face ID, on Android "Open app
  settings" and fingerprint / face / screen lock, on the web the
  browser's site settings, "click" for mouse-only hints, and no brand or
  operating system in examples. A string shared by every platform says
  neither tap nor click.

## 5. What gets translated and what never does

Translated: every label, title, button, helper and error text, empty
state, notification text Lakko itself writes ("New message", "Incoming
call"), accessibility labels, dates, times, relative times ("5 min ago"),
numbers, file sizes, durations, plural forms.

Never translated and never altered:

- anything a person wrote: messages, names, group and channel names,
  bios;
- `@username`, invite links, the safety number, the recovery phrase
  words (BIP39 English wordlist; the instructions around them are
  translated, the words are not).

**The language decides words, the device decides formats (2026-10-29,
maintainer).** Seen live: a device set to a 24-hour clock showed `12:58 PM`
in Lakko because English was the interface language. The interface
language supplies only words: month and weekday names, the words of
relative times, the `AM`/`PM` marker when one is shown. Everything else
comes from the device's own regional settings, whatever the interface
language: 12- or 24-hour clock, the order of day, month and year, the
first day of the week, and the decimal and grouping separators. On iOS and
Android that is the system's current locale and its time setting (iOS
`Locale.autoupdatingCurrent` and its hour cycle; Android
`DateFormat.is24HourFormat` and the system region). A browser does not
expose the operating system's clock setting, so the web takes these from
the browser's own default locale (`navigator.language`), which follows the
browser's language and region settings; a browser set to US English keeps
a 12-hour clock even on a 24-hour system.

Digits: the safety number and recovery-phrase positions always use Latin
digits `0-9` so two people can compare them across languages. Elsewhere
numbers follow the language (Arabic and Persian may use their own
digits; follow the platform formatter for the locale).

## 6. Right-to-left

In Arabic, Persian and Hebrew the layout mirrors:

- navigation: back arrow points right, drawers and the nav rail sit on
  the right, list chevrons point left;
- chat: outgoing bubbles on the left, incoming on the right; the
  composer's send button on the left; bubble tails mirrored;
- progress and sliders run right to left; the voice-message waveform
  plays right to left.

Not mirrored: media playback controls (play / pause / seek keep their
universal shape), the camera preview, clocks, QR codes, the safety
number.

Bidirectional text (applies in every language, not only RTL UIs):

- each message, name and preview is laid out in its own direction,
  detected from its first strong character, isolated from the surrounding
  UI text (an Arabic name inside an English sentence must not reorder the
  sentence, and vice versa);
- usernames, links, the safety number and code-like strings are always
  forced LTR;
- a text field's direction follows what is typed.

## 7. Signed translation updates

Translations built into the app are complete on their own. Lakko may also
receive a newer translation between app releases.

- Invisible in normal use: no banner, no prompt. Settings > About shows
  "Translations updated <date>" when an update is active.
- An update never changes the strings marked security-critical (safety
  number, recovery phrase, device removal, account deletion, identity
  change warnings, E2EE indicators). Those change only with an app
  release.
- If an update is missing, stale or does not verify, the built-in
  translation is used. There is no visible error.
- Fetching updates must not tell the server which language this device
  uses (the ADR decides how; the UX requirement is that it is not
  observable).

## 8. Copy rules for translators

The design-system voice applies in every language: second person, short
sentences, sentence case where the script has case, no exclamation marks,
none of the banned protocol words (and none of their transliterations).
Security-critical strings are flagged in the catalog and are not
released until a native speaker has reviewed them.

## 9. Data the UI needs (input for the ADR)

- A catalog of string keys with English source text, a
  `security-critical` flag per key, a translator comment per key, and
  plural / placeholder syntax that all three platforms can render.
- Per language: own name, direction, and the platform locale identifiers
  it maps to (e.g. `zh-Hans` from `zh-CN`, `zh-SG`).
- Resolution rule, shared by the three clients: selected language, else
  the first system preferred language in the supported set (matching by
  language + script, then language), else English.
- The currently active translation version (built-in or updated, and
  its date) for Settings > About.
- Refusal codes from the core (e.g. membership refusal reasons) are keys
  the client translates; the core never provides UI prose.

## 10. Cross-platform shape

| | Web | iOS | Android |
|---|---|---|---|
| Storage of the choice | local storage on this origin | UserDefaults in the App Group, so the notification extension renders the same language | per-app locale (`AppCompatDelegate.setApplicationLocales`) |
| Live switch | reactive dictionary, `lang` + `dir` on `<html>` | environment locale + layout direction on the root view | configuration change recreates the activity, Compose re-renders |
| Mirroring | CSS logical properties (`margin-inline-start`, ...) | `.environment(\.layoutDirection)`, `.flipsForRightToLeftLayoutDirection` for icons | `LayoutDirection` from the locale, `autoMirrored` icons |
| Language list | route `/settings/language` + dialog on auth | `NavigationStack` screen + sheet on auth | screen + dialog on auth |

The notification extension (iOS) and the background decrypt (Android,
web service worker) render notification text in the chosen language.
