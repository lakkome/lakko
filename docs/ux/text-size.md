<!-- SPDX-License-Identifier: AGPL-3.0-or-later -->

# Text size

Decided 2026-11-04 by the maintainer. Owner surface: Settings > Appearance.

## What it is

One setting, **Text size**, with three steps: **Small**, **Medium**,
**Large**. It changes the text of messages and the reactions under them, and
nothing else: chat lists, menus, settings and the rest of the interface keep
following the system text size, as they do today. A person who wants the
whole interface larger uses the system setting.

The setting belongs to the device. Each device keeps its own choice (a phone
can be Large and a computer Small); nothing is sent to the account's other
devices or to any server. Default: Medium.

## What scales

Every size below is multiplied by the step's factor: Small 0.87, Medium 1.0,
Large 1.2 (chosen, not measured; Medium is today's design system). On iOS the
result is further scaled by Dynamic Type, as today.

- Message bubble text (design system: 15) and the bubble's timestamp and
  delivery mark (11).
- The reply quote inside a bubble and the reply / edit strip above the
  composer.
- The composer's text field.
- Link previews' title and description, forwarded-from line, and the sender
  name line of a group bubble.
- Reaction chips under a message or post (design system "Reaction chip":
  height 32, padding 10, emoji 18, avatars 22, overlap 4, count 14, spacing
  6), all values together.
- The quick reaction bar in the message menu (emoji 28) scales with the step,
  but its tap target never goes below 44. Where the bar is wider than the
  screen less its margins, it scrolls sideways rather than shrinking its
  targets.

Not scaled: bubble width limits, avatars beside bubbles, the chat header, the
date separators, attachments and media frames.

## Settings screen

Settings > Appearance, below Theme: a row **Text size** with the current step
as trailing text. Opening it shows the three steps as a single choice
(the platform's standard list with a check mark, not a slider), and above
them a preview: one incoming and one outgoing bubble with a reaction chip
under the incoming one, drawn at the selected step. Choosing a step applies at
once, everywhere, with no Save button and no restart.

Copy (catalog keys under `settings.text_size.*`):
- Row: "Text size"
- Steps: "Small", "Medium", "Large"
- Preview incoming text: "Is this size easy to read?"
- Preview outgoing text: "Change it here at any time."

## States

There are no loading or error states: the value is local and always readable.
A stored value the client does not recognise reads as Medium.

## Platforms

- Web: a CSS custom property for the factor on the chat root; stored in
  `localStorage` under a per-device key; read before the first render.
- iOS: `UserDefaults` (device only, not the App Group); the factor multiplies
  the scaled metrics the chat already uses.
- Android: `SharedPreferences`; the factor multiplies the sp/dp values of the
  chat bubble and reaction composables.
