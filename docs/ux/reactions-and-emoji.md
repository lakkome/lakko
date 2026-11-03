<!-- SPDX-License-Identifier: AGPL-3.0-or-later -->
<!-- SPDX-FileCopyrightText: 2026 Lakko Contributors -->

# UX spec: reactions and emoji, the Telegram way

**Status:** design, 2026-11-02. Input to an ADR-0023 amendment, the ADR-0087
section 13a reconciliation and the four client builds (web, desktop = web in
Tauri, iOS, Android). The eleven questions of section 20 were decided 2026-11-02, maintainer; the
text follows the decisions.
**Owner surface:** the message menu, the reaction chips under a message or post,
the reactions list, the emoji picker, the composer, the info screen of a group
and of a channel, Settings.
**Brief:** "I want it like Telegram; the approach to reactions and emoji as in
Telegram, in chats, groups and channels." (maintainer, 2026-11-02)
**Builds on:** [`message-actions.md`](message-actions.md) (the menu shape:
reaction row above the bubble, action card below, never on hover),
[`media-messages.md`](media-messages.md) section 4 (the emoji picker),
[`group-and-channel-privacy.md`](group-and-channel-privacy.md) section 13.1
(channel reactions), ADR-0023, ADR-0047, ADR-0087 section 13a.

## 1. Goal

A person reacts to a message the way they do in Telegram, in a one-to-one chat,
a group and a channel: one tap from a short row, a double tap for their usual
reaction, the full set one more tap away, chips under the message that show
what people said and let you join in or take yours back, and a list of who
reacted where that can be known. Owners and admins of groups and channels
choose which reactions their conversation takes. Emoji in text are found fast
(picker, recent, search, typing `:` and a word), and a message of only one to
three emoji is drawn large.

Where Lakko cannot do what Telegram does without telling someone something
Telegram's server knows and Lakko's must not, this spec says what Lakko shows
instead (section 8).

## 2. What Telegram does (summary)

The sourced list is Appendix A; this is what the design copies. "Not
confirmed" means no Telegram source was found and the behaviour is from general
knowledge or is Lakko's own choice.

| Behaviour | Telegram | Confirmed |
|---|---|---|
| Opening reactions | tap a message (Android), press and hold (iOS), right-click (desktop): a reaction row above the context menu | yes |
| Expanding | the row expands to the full set; frequently used reactions come first | yes |
| Quick reaction | double tap (desktop: double click) sends the quick reaction, thumbs up by default, changeable in Settings, synced across devices | yes |
| Chips | under the message, a count per emoji, own reaction marked, tap to add or remove (tap behaviour not confirmed) | partly |
| Avatars on chips | reactors' pictures next to the reaction in small groups; threshold unknown | indirect |
| Who reacted | in groups a list of who reacted with what; in channels reactions are anonymous, counts only | yes |
| Per person | 1 reaction per message for free accounts (3 for Premium); a new one replaces the old | yes |
| Kinds per message | at most 11 different reactions on one message by default; a chat may set its own limit | yes |
| Admin setting | groups and channels: All / Some (a list, up to 100) / None; private chats always on | yes |
| Notifications | for reactions to your messages, from everyone / contacts / nobody | yes |
| Unseen reactions | a heart button in the chat jumps to your messages with unseen reactions | yes |
| Animations | each reaction has its own animation; press and hold in the menu for a big effect | yes |
| Emoji suggestions | type one word for suggestions; type `:` and a keyword for emoji search | yes |
| Picker | categories, recent, search by keyword, skin tones for all People emoji at once (desktop) | partly |
| Large emoji | a message of up to three emoji is shown large and without a bubble (desktop; other apps said to match) | desktop yes |
| Interactive emoji, dice | tap a single animated emoji in a private chat for a shared effect; dice emoji roll | yes |

## 3. What Lakko has today

Checked in the tree on 2026-11-02 (HEAD `db5cd8b6` plus uncommitted work in
the channel crates noted below). Desktop is the web client (ADR-0085) and has
everything web has.

| Area | Web (`apps/web/src`) | iOS (`apps/ios/Lakko`) | Android (`apps/android/app/src/main/kotlin/me/lakko/app`) |
|---|---|---|---|
| Quick row in the menu | six fixed emoji + `+` (`chat/MessageMenu.tsx`, list in `chat/message-ops.ts` `REACTION_EMOJI`) | same six + `+` (`UI/Chat/MessageActionMenu.swift` `MessageReactionRow.palette`) | same six + `+` (`ui/chat/ChatScreen.kt` `REACTION_EMOJIS`, row in `ui/chat/MessageActionMenu.kt`) |
| Row order | fixed; not personal | fixed | fixed |
| Full set | `+` opens the picker in reaction mode (`media/EmojiPicker.tsx`) | `+` opens `Media/MediaPicker.swift` in reaction mode | `+` opens `ui/chat/EmojiPickerSheet.kt` in reaction mode |
| Double tap / double click | none | none | none |
| Chip order | sorted by emoji code point (`chat/TimelineBubble.tsx` `reactionEntries`) | order of first reaction (`Chat/ChatRepository.swift` `reactionSummary`) | order of first reaction (`ReactionChips`) |
| Count on a chip | always, also "1" | hidden when 1 | always, as text `"👍 1"` |
| Own highlighted | accent-soft fill + accent border (`styles.css` `.lk-reaction.mine`) | accent-soft fill + accent stroke | `primary` at 18% alpha, no border (Material colour, not a Lakko token) |
| Tap a chip | toggles own reaction | toggles | toggles |
| Who reacted | a hover title "Reacted by ..." only; no list | long press lists names (`reactedByItems`) | long press does nothing (`onLongClick = {}`) |
| Chips on a deleted message | shown | hidden | hidden |
| Reactions per person | any number of different emoji (ADR-0023 toggles `(reactor, emoji)`) | same | same |
| Kinds per message | no limit | no limit | no limit |
| Allowed-reactions setting | none (groups); channels: none in the client | none | none |
| Reaction notifications | none: a reaction is published with no push wake (`routes/Chat.tsx` `publishQuiet`) | none (NSE shows no preview for a reaction, `LakkoNotificationService/NsePushDecrypt.swift`) | none (`push/PushPreviewDecrypt.kt`) |
| Unseen-reactions button | none | none | none |
| Channel reactions | not built in any client | not built | not built |
| Picker | search, Recent (24, per account, on this device), categories, skin tone by right-click | search, Recent, categories, skin tone by long press (`Media/EmojiIndex.swift`) | same (`media/EmojiIndex.kt`) |
| Emoji data | one file, `media/emoji/emoji-data.json` (Unicode 15.1, CLDR 45 **English** keywords only) | the same file (identical bytes) | the same file, copied at build time |
| `:` suggestions while typing | none | none | none |
| Large emoji-only messages | none | none | none |

Core, today:

- Chat reactions (ADR-0023): an op `react_to` + `emoji` in the chat envelope,
  any member, toggled per `(reactor signature key, emoji)`, so a person may
  give several different reactions to one message. Bound: 1 to 64 bytes; the
  clients add a one-emoji-grapheme check before sending.
- Channel reactions (ADR-0087 section 13a): a row at the channel service per
  `(post, writer key, label)`; the label hides the emoji; readers get counts per
  label; nobody, the owner included, can learn who reacted. Committed (HEAD):
  a fixed list of 64 emoji in `lakko_ui_rules::reaction`. **Uncommitted in the
  working tree on 2026-11-02** (another session): any emoji a chat takes, an
  owner-set `AllowedReactions { All, Off, List(1..=100) }` in the channel
  metadata record (`crates/lakko-channels/src/meta.rs`), a per-reaction writer
  key (`write_key.rs`), and `QUICK_REACTIONS` in `lakko_ui_rules` replacing the
  64. ADR-0087 and `group-and-channel-privacy.md` section 13.1 still describe
  the fixed 64. This spec follows the working tree.

## 4. Entry points

| Goal | Entry point |
|---|---|
| React to a message or post | long press (iOS, Android), right-click (web): the reaction row above the bubble |
| React with my usual reaction | double tap (iOS, Android), double click (web) on the bubble |
| React with any other emoji | the row's trailing expand control |
| Take my reaction back | tap my highlighted chip, or the same emoji in the row again |
| Join someone's reaction | tap their chip |
| See who reacted | long press or right-click a chip (one-to-one and group only) |
| Find messages that got reactions | the unseen-reactions button in the conversation |
| Choose my quick reaction | Settings > Appearance > Quick reaction (D4, decided 2026-11-02, maintainer) |
| Choose what a group or channel takes | Info > Reactions (owner and admins) |
| Put emoji in text | the smiley button in the composer; or type `:` and a word |

## 5. The reaction row

The menu shape is `message-actions.md` section 2 and is not restated: the row is
its own surface above the bubble, the action card below, the screen behind
dimmed.

**What the row holds.** Six emoji and an expand control at the trailing end.

- The person's quick reaction first, then their most recently used reactions on
  this account (taken from the picker's Recent list, reactions only), then the
  defaults `👍 ❤️ 😂 😮 😢 🙏` to fill six, no repeats. Telegram puts the
  frequently used first; Lakko uses recency because the Recent list already
  exists and is per account. (D1, decided 2026-11-02, maintainer: a personal row, not the fixed six
  every client shows today.)
- Narrowed by the conversation's allowed reactions (section 9): an emoji the
  conversation does not take is left out and the next one moves up. With
  `Some reactions` the row is the first six of the conversation's list in the
  owner's order, personal order first. With `No reactions` there is no row, and
  the menu opens with the action card alone.
- An emoji the person has already given to this message is shown with the
  accent-soft circle behind it; pressing it takes the reaction off.
- When the person already has 3 reactions on this message (section 10), the
  emoji they have not given are shown dimmed and do nothing; a line under the
  row reads "You can add up to 3 reactions. Take one off to add another." Their
  own 3 stay pressable, to take one off. The expand control opens the picker
  with the same rule.

**The expand control** opens the full picker in reaction mode
(`media-messages.md` section 4: Emoji tab only, a tap reacts and closes). With
`Some reactions` it opens the same picker showing only the allowed list, as one
grid in the owner's order with the search field kept and no category strip.
When the allowed list has six or fewer emoji the control is not shown.

**One tap.** Pressing an emoji in the row reacts and closes the menu. A press
held on an emoji does nothing more (Telegram's "big effect" is not built,
section 16).

## 6. Double tap: the quick reaction

- **iOS and Android:** a double tap on the bubble sends the person's quick
  reaction to that message. **Web and desktop:** a double click on the bubble,
  except on its text (a double click on text keeps selecting a word, as people
  expect in a browser) and except on a control (play, link, attachment, quote
  block). (D2, decided 2026-11-02, maintainer.)
- The quick reaction is `👍` until the person picks another.
- A second double tap on the same message takes it back (the chip toggles, as
  every other way of reacting does).
- Nothing happens, silently, when: the conversation does not allow that
  reaction, the message is not sent yet or failed, the membership has ended,
  selection mode is on, or the message is a deleted tombstone.
- When the person already has 3 reactions on the message and the quick
  reaction is not one of them, nothing is sent and the line "You can add up to
  3 reactions. Take one off to add another." shows under the message's chips
  for about three seconds.
- The first tap of a double tap must not also open what a single tap opens
  (a photo, a link): a double tap that starts on a control is the control's,
  not a reaction. Whether a single tap on a bubble's plain area does anything
  on each client today was not checked for this spec; if it does, that tap
  waits for the double-tap window.
- Accessibility: with VoiceOver or TalkBack on, double tap is the system's
  activate gesture and is never taken over. The same reaction is reachable as a
  named action on the bubble ("React 👍", the existing string).
- Feedback: the chip appears (or goes) at once with a short scale; with reduced
  motion it just appears. A light haptic on iOS and Android.

**Choosing the quick reaction.** Settings > Appearance > `Quick reaction`
shows the current emoji; tapping it opens the full picker in reaction mode; a
pick saves and closes. One line under the row: "Double-tap a message to send
it." The choice belongs to the account and follows the person to their other
devices, as in Telegram (D3, decided 2026-11-02, maintainer). The place, Appearance, is D4 (decided 2026-11-02, maintainer):
Settings has no Chats section.

## 7. Reaction chips

Under the bubble, aligned to the bubble's side, wrapping onto more lines when
needed. One chip per emoji.

- **Order:** most reactions first; equal counts in the order they were first
  given. Stable: a chip does not jump while the person looks at it, except when
  its count passes another's.
- **What a chip shows:**
  - One-to-one chat: the emoji alone. A count of 2 when both gave the same
    emoji.
  - Group: the emoji, then up to three reactors' avatars (16 px circles,
    overlapping) when three or fewer people gave it; a number when more did.
    Telegram does this in small groups; the threshold is Lakko's choice
    (D5, decided 2026-11-02, maintainer).
  - Channel: the emoji and the number, always (nobody's avatar can be known).
- **Own reaction:** accent-soft fill and a 1 px accent border, on all three
  clients through the design-system tokens (Android stops using the Material
  `primary` tint). Not colour alone: the screen reader says "your reaction".
- **Tap:** adds the person's reaction of that emoji, or takes it off when it is
  theirs. In a channel the same. When the person already has 3 reactions on the
  message, a tap on a chip that is not theirs changes nothing and shows the
  3-reactions line under the chips for about three seconds.
- **Long press (iOS, Android), right-click (web):** opens the reactions list
  (section 8). In a channel: section 8.3.
- **A message deleted for everyone** shows no chips (web changes to match the
  natives).
- **Membership ended:** chips stay as text and stop reacting to a tap
  (`proposal-based-membership.md` section 6.2.5, unchanged).
- **Too many kinds:** at most 11 chips are shown, the largest; the rest fold
  into a last chip `+N` that opens the reactions list. A sending client does not
  add a twelfth kind: the row and the picker leave new emoji out once a message
  has 11 kinds, and offer only the ones already there (D6, decided 2026-11-02, maintainer; 11 is
  Telegram's default; a modified client can still send more, so the display cap
  is what holds).
- **Animation:** a new chip, or a count that changes while visible, does a short
  scale (about 150 ms). No per-emoji animation, no full-screen effect. Reduced
  motion: none.

## 8. Who reacted

### 8.1 One-to-one and group: the reactions list

Opened by a long press or right-click on any chip, or by the `+N` chip. A bottom
sheet on iOS and Android, a popover at the chip on web (the design system's
`LakkoMenu` surfaces).

- Header row of tabs: `All 14`, then one tab per emoji with its count, in chip
  order; it opens on the tab of the chip that was pressed.
- Rows: avatar, name, and on the `All` tab the emoji at the trailing end. The
  person's own row reads `You` and comes first.
- Names come from the same resolver the timeline uses (contact name, profile
  name, the fallback). A row of a contact opens that person's info; any other
  row is not a control (a name in a group is not a way to reach a person,
  ADR-0048).
- Loading: none; the list is built from what the device holds.
- A reactor no longer in the group stays in the list under the name the device
  last knew.

### 8.2 One-to-one

The same list exists but is rarely needed: the chip's highlight already says
whether a reaction is yours or the other person's. Kept so that one gesture
means one thing everywhere.

### 8.3 Channels: anonymous by construction

**What Telegram shows:** counts only in channels; no list of who reacted.
**What Lakko shows:** the same, for a reason that goes further. A subscriber's
reaction is a counter row the channel service keeps, written under a key that
belongs to that one post and is never reused, so nobody can tell who wrote it:
not other subscribers, not the channel's owner or admins, not Lakko. There is
no list to show and none can be built.

- A long press or right-click on a channel chip opens a small note instead of a
  list: "Reactions in a channel are anonymous. Nobody can see who reacted, not
  even the channel's owner." with one `OK`.
- The person's own reactions are known to their own devices only (they are kept
  in the account's synced state), and drive the highlight.
- Owners and admins see exactly the counts every subscriber sees.
- A reaction can't be removed by the owner, and a person banned from comments
  can still react: nobody knows whose a reaction is
  (`group-and-channel-privacy.md` section 13.3).

## 9. Allowed reactions: owners and admins choose

Telegram's setting, in groups and channels. One-to-one chats always take every
reaction and have no setting (as in Telegram).

**Where:** the info screen of a group or channel, a `Reactions` row, shown to
the owner and admins of a group and to the owner of a channel, with the current value on the right: `All`, `Off`,
or the number chosen ("12 reactions"). Members and subscribers do not see the
row; they see its effect in the menu.

**The screen** (pushed, titled `Reactions`):

- Three choices as a single-choice list:
  - `All reactions`, "Members can react with any emoji." (channel:
    "Subscribers can react with any emoji.")
  - `Some reactions`, "Members can react only with the emoji you choose."
  - `No reactions`, "Members can't react to messages." (channel: "...to posts.")
- With `Some reactions`, below the choices: the chosen emoji as a grid in their
  order (drag to reorder on all three; a tap removes one), then an `Add` button
  opening the full picker, which adds and stays open. Up to 100. At 100 the
  picker's emoji stop responding and a line reads "You can choose up to 100."
  The first time `Some` is picked the grid starts with the six defaults.
- `Save` in the header, enabled when something changed and, with `Some`, at
  least one emoji is chosen ("Choose at least one." under the empty grid).
  Leaving with unsaved changes asks "Save changes?", the dialog of
  [`profile-screen.md`](profile-screen.md).
- Saving: a group sends the setting to every member (section 13); a channel
  re-issues its metadata record. The row updates at once. A failure keeps the
  screen open: "Couldn't save. Try again."

**What changes for everyone:** the row and the picker offer only what is
allowed (section 5); double tap does nothing with a quick reaction that is not
allowed; chips of reactions the conversation no longer takes are hidden on
every device, and come back if they are allowed again (D11, decided
2026-11-02, maintainer). A reaction already
given is not taken off anyone's list: hiding is a display rule. In a channel
the service cannot refuse a reaction it cannot read, so the same hiding is all
there is; in a group the same rule holds for the same reason (a group member's
client is the only one that can refuse).

**Who may change it:** in a group, the owner and admins (ADR-0022 roles,
checked by each member at receipt as for the group name); in a channel, the
owner, since the setting lives in the channel metadata record, whose one writer
is the owner (ADR-0087 sections 13a and 15; corrected 2026-11-03, it read
"its publishers").

## 10. How many reactions one person gives

**Up to 3 different reactions per person per message** (D7, decided 2026-11-02, maintainer). Not
one, as a free Telegram account has, and not unlimited, as Lakko allows today.
This is what Telegram gives Premium accounts.

- **Per person, not per device.** A person's devices share the 3. A chip's
  count is a number of people, and a person counts once on each chip they gave.
- **Adding.** Each emoji is added or taken off on its own; adding one does not
  replace another.
- **The 4th.** A client offers a 4th only after one of the 3 is taken off: in
  the row and the picker the other emoji are dimmed with the line "You can add
  up to 3 reactions. Take one off to add another." (section 5); a chip tap or a
  double tap that would add a 4th sends nothing and shows the same line.
- **What a receiver does with a 4th** (one sent anyway, by a modified client or
  two of a person's devices adding at the same moment): it keeps that person's
  3 earliest reactions on the message, in the conversation's order (the order
  in which the messages reached the conversation, which every member sees the
  same), and ignores the rest. A reaction that was ignored stays ignored; it
  does not take a freed place later. Every member therefore draws the same
  chips.
- **Channels: a client rule only.** Under a channel post nobody can check the
  cap: every reaction is written under a key of its own and nobody knows whose
  it is, so the service cannot count a person's reactions and neither can any
  reader. The person's own app enforces 3 from the account's record of its own
  reactions (shared among its devices); a modified app can give more, which
  inflates counts, the same accepted cost as ADR-0087 section 13a states for
  tokens. Two devices of the account adding at the same moment can briefly give
  4; the next one to read the shared record takes nothing off by itself, and
  the person sees 4 highlighted until they take one off.

## 11. Channels: what else differs

- Counts arrive with the posts in view, in one batched call; until they arrive
  a post shows no chips (no spinner, no placeholder). If the call fails the
  last counts shown stay and nothing is said.
- A reaction appears at once on the person's device. If the device has no
  allowance left to send it, it waits and leaves with the next one; nothing is
  refused and no error is shown (`group-and-channel-privacy.md` section 13.1).
  Taking a reaction off always goes out at once.
- A reaction under a comment works like one under a post.
- An emoji that the person's app does not know (a newer emoji than the app's
  emoji data) cannot be shown by anyone whose app does not know it either: the
  count for it is dropped. The picker only offers emoji the app knows, so this
  only matters across app versions.
- Posts the device cannot open (older than its keys) show no chips.

## 12. Notifications and unseen reactions

Built as below, one switch, on by default (D8, decided 2026-11-02, maintainer).

**Notification.** When someone reacts to the person's own message in a
one-to-one or a group, the person's devices show "Anna reacted ❤️ to your
message" (group: title is the group name, body "Anna reacted ❤️ to your
message"). No excerpt of the message on iOS, since the background process does
not hold the conversation's messages. Settings > Notifications gets a
`Reactions` switch, "When someone reacts to your messages", on by default
(Telegram's default was not confirmed). A muted conversation stays silent. A
reaction that is taken back within a few seconds is not notified if the device
learns of the removal before showing it. Channels: no reaction notifications.

Cost to say plainly: today a reaction wakes nobody. A notified reaction wakes
every device in the conversation, the same as a message does, and every device
but the author's decides to stay silent.

**Unseen reactions button.** In a conversation, above the scroll-to-bottom
button, a round button with a heart and a count appears when others reacted to
the person's own messages and those messages have not been on screen since.
Tapping scrolls to the oldest such message and briefly highlights its chips
(the reply-jump highlight); the count drops as each is seen; at zero the button
goes. Local to the device. In the chat list, a conversation with unseen
reactions shows a small heart beside the unread badge. One-to-one and groups
only.

## 13. Emoji in text

The picker stays as specified in `media-messages.md` section 4 (one picker,
Emoji and GIF tabs, search, Recent, categories, skin tones, data bundled, the
platform's own emoji font). Changes:

- **Skin tone for all at once.** In the People category header, a tone control
  (five tones and "no tone") sets the default tone for every emoji that has
  tones; a long press or right-click on one emoji still overrides it for that
  emoji. Stored per account on this device, as today.
- **Search in the interface language.** Today keywords are English only (CLDR
  45 `en`). Bundle the CLDR annotations of every supported interface language
  and search the current language plus English.
- **`:` suggestions.** Typing `:` at the start of a word followed by two or
  more letters shows a strip of up to eight matching emoji above the composer
  (the same search, Recent ones first). Tapping one replaces `:word` with the
  emoji. Escape, a space or moving the caret away closes the strip. On web,
  arrow keys move along the strip and Enter or Tab picks. Telegram also
  suggests emoji for a plain typed word without `:`; Lakko does not (D9,
  decided 2026-11-02, maintainer: noisy in a language whose words match many keywords).
- **Emoji only, shown large.** A message whose text is one to three emoji and
  nothing else (spaces allowed), with no attachment, no reply quote and no
  forward line, is drawn without a bubble background, in the platform emoji font
  at about 56 px for one, 44 for two, 36 for three (chosen). The time, the
  edited mark and the delivery state sit in a small pill under it on the
  bubble's side. Menu, double tap and chips work as on any message. An edit that
  adds text returns it to a normal bubble. In a channel post the same.
- No animated or interactive emoji, no dice, no custom emoji (section 16).

## 14. States

| Surface | State | What it shows |
|---|---|---|
| Row | conversation takes no reactions | no row; action card alone |
| Row | message not sent, failed, or deleted | no row |
| Row, picker | person already has 3 reactions here | their 3 pressable, the rest dimmed; "You can add up to 3 reactions. Take one off to add another." |
| Chip tap, double tap | would add a 4th | nothing sent; the same line under the chips for about 3 s |
| Chips | none | nothing under the bubble |
| Chips | channel counts loading | nothing |
| Chips | channel counts failed | last known counts, no error |
| Reaction send | failed (chat) | the chip goes back; inline error "Could not send the reaction. Try again." (existing key `chat.error.react`) |
| Reaction send | channel, out of allowance | chip shown; leaves later; no error |
| Reactions list | one person | still a list; tabs hidden when only one emoji |
| Reactions list | channel | the anonymity note (8.3) |
| Picker | no search result | "No emoji found" (existing) |
| Picker | allowed list only | one grid, no categories |
| Allowed reactions | saving | `Save` shows a spinner |
| Allowed reactions | save failed | "Couldn't save. Try again." |
| Allowed reactions | `Some`, empty | `Save` disabled, "Choose at least one." |
| Allowed reactions | 100 chosen | "You can choose up to 100." |
| Quick reaction | not allowed here | double tap does nothing |
| Unseen button | zero | not shown |

## 15. Controls and what each drives

| Control | Drives |
|---|---|
| Emoji in the row, chip tap, double tap, picker in reaction mode | the conversation's reaction op (sent) or a channel reaction write |
| Long press / right-click on a chip | local list, nothing sent |
| Quick reaction setting | the account's synced state (D3) |
| Reactions row in Info | the group's reactions setting (sent to every member) or the channel metadata record |
| Unseen reactions button | local only |
| Reactions notification switch | this device's notification filter |
| `:` suggestions, skin tone, Recent | local only |

## 16. Left out, and why

Premium-only or paid in Telegram, not built:

- custom emoji in text and custom emoji reactions (Telegram Premium; channel
  custom reactions unlocked by boosts);
- star (paid) reactions and their per-post top-sender list;
- Saved Messages tags made of reactions (Premium);
- emoji statuses next to names (Premium);
- message effects (mostly Premium).

Up to three reactions per message is Premium in Telegram and is built in Lakko
for everyone (section 10).

Free in Telegram, not in this spec:

- per-reaction animations and the press-and-hold big effect (Telegram ships
  animation files per reaction; Lakko draws the platform's emoji font);
- interactive animated emoji and their synchronised effect, emoji sounds;
- dice and other game emoji;
- reactions on service messages, in calls and on stories;
- a per-person choice of who may notify you of reactions (contacts / everyone):
  one switch here;
- admins deleting one person's reactions (possible in a group, where the
  reactor is known; not designed);
- a reaction shown as the conversation's last message in the chat list (not
  confirmed in Telegram).

Telegram draws Apple's emoji on every platform; Lakko keeps each platform's own
emoji font (`media-messages.md` section 4), so the same emoji looks different
on an Android phone and an iPhone.

## 17. Data the core needs

What the UI above requires that the core and schema do not carry today. Input
to the ADR step.

1. **Group reactions setting.** A new owner-or-admin group envelope (the
   group-name template, ADR-0051): `All` / `Off` / a list of 1 to 100 emoji,
   stamped, newest wins per group, re-sent verbatim on growth, accepted only
   from the owner or an admin resolved at receipt, suppressed by the background
   filters. One codec shared with the channel record: today `AllowedReactions`
   lives in `lakko-channels::meta` (uncommitted); the group envelope belongs in
   `lakko-contacts`, so the type should move somewhere both can use.
2. **Channel reactions reconciled.** ADR-0087 section 13a and
   `group-and-channel-privacy.md` section 13.1 still say "a fixed list of 64";
   the working tree says any emoji plus the owner's allowed list. The ADR and UX
   text must follow before clients build.
3. **Up to 3 reactions per person (D7).**
   - **Explicit add and remove.** The chat reaction op says `add` or `remove`
     instead of toggling (ADR-0023 amendment, a fuzzed codec change in
     `lakko-attachments`; no old form kept, there is no deployment). With a
     toggle, two of a person's devices pressing at once cancel each other, and
     an ignored 4th add would turn into a removal the next time the same emoji
     is sent; with explicit ops an `add` of an emoji the person holds and a
     `remove` of one they do not are no-ops.
   - **One fold every member computes alike.** Per message and per person,
     apply ops in the conversation's journal order; an `add` that would give the
     person a 4th is ignored and stays ignored. Written once in Rust (next to
     the chat body parser, or in `lakko_ui_rules`) and called on both FFI
     surfaces, so the three clients cannot disagree.
   - **"Person" is the account.** The cap is per account, so the fold keys on
     the account key a leaf resolves to (ADR-0050), not on a device's leaf key.
     What the clients key reactions on today (`chat/message-ops.ts` reactor,
     `reactionSummary` on iOS, `ReactionChips` on Android: a "signature key")
     was not checked against ADR-0050 for this spec; if it is the leaf key, a
     person reacting from two devices already counts twice today.
   - **Channels.** No core rule can hold the cap (per-write keys). The client
     reads the person's own reactions on the post from
     `lakko-sync::synced::channel_reaction` (working tree) and refuses a 4th
     locally.
4. **Shared presentation rules in `lakko_ui_rules`, on both FFI surfaces,** so
   the clients stop disagreeing as they do now (section 3):
   `QUICK_REACTIONS` (exists uncommitted, not yet on FFI), the quick-row rule
   (personal order, allowed set, six), the chip rule (order, count vs avatars,
   11-kind cap and `+N`), and `emoji_only_count(text)` for large emoji.
5. **Grapheme rule.** "One emoji" and "one to three emoji only" need the same
   answer on all clients. Today web uses `Intl.Segmenter`, iOS Swift
   `Character`, Android its own check. In Rust this needs a grapheme segmenter
   and Extended_Pictographic data; `unicode-segmentation` is not in the
   workspace. Add the dependency and put the rule in `lakko_ui_rules` (D10,
   decided 2026-11-02, maintainer), not three implementations.
6. **Quick reaction (D3).** A `lakko-sync` register for the
   account's quick reaction, like the own-username register.
7. **Reaction notifications (D8).** A wake on a reaction publish (today it
   is `publishQuiet`), and a way for the background process to know the
   reaction is to the person's own message without the message log: either the
   reaction op carries the target message's author key (an optional field; every
   member already knows it), or each platform keeps an own-message index the
   background process can read.
8. **Unseen reactions (D8).** Per conversation, the set of own messages with
   reactions from others not yet on screen, kept with the message log; a count
   the chat list can read.
9. **Localized emoji keywords.** `apps/web/scripts/gen-emoji-data.mjs` takes
   CLDR annotations for every supported interface language; the one bundled file
   grows.
10. **Channel own reactions** are already in the working tree
    (`lakko-sync::synced::channel_reaction`); nothing new.

## 18. Cross-platform shape

One model: a row above, chips below, double tap for the quick one, a list of
who reacted where it can be known, the owner's choice of what is allowed.

| | Web / desktop | iOS | Android |
|---|---|---|---|
| Open the row | right-click | long press | long press |
| Quick reaction | double click outside text | double tap | double tap |
| Reactors list | popover at the chip | bottom sheet | bottom sheet |
| Open the list | right-click a chip | long press a chip | long press a chip |
| Picker | popover above the composer | bottom sheet | bottom sheet |
| Skin tone of one emoji | right-click | long press | long press |
| `:` strip | arrows, Enter / Tab | tap | tap |
| Haptic on reacting | none | light | light |

Never on hover (`client-redesign.md`). Tokens only; no Material default tints.
The action set and the states are the same on every client.

## 19. Copy

New strings (English; to the catalog):

- `Quick reaction` / "Double-tap a message to send it."
- `Reactions` (info row, screen title, list title)
- `All` / `Off` / "{count, plural, one {# reaction} other {# reactions}}" (row values)
- `All reactions` / "Members can react with any emoji."
- `Some reactions` / "Members can react only with the emoji you choose."
- `No reactions` / "Members can't react to messages."
- Channel variants: "Subscribers can react with any emoji." / "Subscribers can
  react only with the emoji you choose." / "Subscribers can't react to posts."
- `Add` / "Choose at least one." / "You can choose up to 100."
- "Couldn't save. Try again."
- "You can add up to 3 reactions. Take one off to add another."
- `All {count}` (list tab)
- "Reactions in a channel are anonymous. Nobody can see who reacted, not even
  the channel's owner."
- "{name} reacted {emoji} to your message"
- Notifications switch: `Reactions` / "When someone reacts to your messages"
- Accessibility: "React {emoji}" (exists, `chat.message.react_with`), "More reactions" (exists),
  "{emoji}, {count, plural, one {# reaction} other {# reactions}}",
  "{emoji}, your reaction", "New reactions to your messages, {count}",
  "Show who reacted"

Existing and kept: `chat.reaction.reacted_by` ("Reacted by {names}") as the
screen-reader hint, `chat.error.react`, `chat.reaction.single_emoji`.

Checked against the design system: no protocol words, no statements about what
servers can or cannot see (the channel note names people: subscribers and the
owner).

## 20. Decisions

All decided 2026-11-02, maintainer.

| # | Question | Decided |
|---|---|---|
| D1 | Quick row: personal or the fixed six | personal (quick reaction, then recent, then the defaults) |
| D2 | Web double click reacts | yes, outside text and controls |
| D3 | Quick reaction synced across the account's devices | synced |
| D4 | Where the quick reaction setting lives | Settings > Appearance |
| D5 | Avatars on group chips when 3 or fewer reacted | yes |
| D6 | Cap of 11 kinds per message, `+N` beyond | yes |
| D7 | Reactions per person per message | up to 3; a 4th only after one is taken off; receivers keep a person's 3 earliest; a client rule in channels |
| D8 | Reaction notifications + unseen-reactions button | yes, one switch, on by default |
| D9 | Emoji suggestions for a plain word (no `:`) | no |
| D10 | Grapheme rule in Rust (new dependency) or three implementations | Rust, in `lakko_ui_rules` |
| D11 | Hide chips of no-longer-allowed reactions | hide |

## Appendix A. Telegram, with sources

Researched 2026-11-02 from Telegram's blog, FAQ, API documentation and the
Telegram Desktop changelog. "Not confirmed" = no source found.

1. **Opening the row.** "Double-tap a message to send a quick thumbs-up. For
   more reactions, tap once on the message (on iOS: press and hold)."
   https://telegram.org/blog/reactions-spoilers-translations . Expandable panel,
   "The reactions you use frequently will show up at the top."
   https://telegram.org/blog/infinite-reactions-statuses . Desktop: reaction
   selector above the right-click menu (4.1.2 beta), expandable menu with most
   used first (4.2):
   https://raw.githubusercontent.com/telegramdesktop/tdesktop/dev/changelog.txt .
   Recent and top reactions: https://core.telegram.org/api/reactions .
   Not confirmed: how many reactions the collapsed row shows; a hover button on
   desktop.
2. **Quick reaction.** Thumbs up by default, changeable (Android Chat Settings,
   iOS Stickers and Emoji): blog URL above. Synced as `reactions_default`:
   https://core.telegram.org/api/reactions . Desktop double click (3.6.3 beta):
   changelog URL above. Not confirmed: switching it off.
3. **Chips and who reacted.** Counts, own reactions (`chosen_order`), recent
   reactors, `can_see_list`: https://core.telegram.org/constructor/messageReactions .
   Who reacted in groups: https://core.telegram.org/method/messages.getMessageReactionsList .
   Reactor picture next to the reaction in a group (bug report):
   https://bugs.telegram.org/c/25442 . Not confirmed: the avatar threshold; tap
   and long-press behaviour of a chip.
4. **Channels anonymous.** "For channel posts, we only have a list of anonymous
   reactions... It is not possible to obtain a list of users who reacted."
   https://grammy.dev/guide/reactions (mirror of Bot API text) and
   https://core.telegram.org/bots/api .
5. **Per-person and per-message limits.** `reactions_user_max_default` 1,
   `reactions_user_max_premium` 3, `reactions_uniq_max` 11,
   `reactions_in_chat_max` 100: https://core.telegram.org/api/config ; Premium
   up to 3: https://telegram.org/blog/infinite-reactions-statuses .
6. **Admin setting.** `chatReactionsAll` / `chatReactionsSome` /
   `chatReactionsNone`: https://core.telegram.org/api/reactions ;
   `reactions_limit`, `paid_enabled`:
   https://core.telegram.org/method/messages.setChatAvailableReactions ; private
   chats always on (blog 2021). Not confirmed: switching reactions off in a
   one-to-one.
7. **Notifications and unseen reactions.** Reaction notification settings:
   https://core.telegram.org/api/reactions ; the heart button for unseen
   reactions: https://telegram.org/blog/video-stickers-better-reactions ; desktop
   3.5 jump button (changelog). Not confirmed: the default of the setting; a
   reaction as the chat-list preview.
8. **Animations.** Own animation per reaction; press and hold for a larger
   effect; synchronised: https://telegram.org/blog/video-stickers-better-reactions .
9. **Emoji in text.** Suggestions for one typed word and `:` keyword search:
   https://telegram.org/faq . Emoji categories:
   https://core.telegram.org/api/emoji-categories . Desktop: change all skin
   tones from the People section, remove a recent emoji (4.9.2), keyword search
   (6.8.0): changelog URL above. Telegram uses Apple's emoji design:
   https://www.emojipedia.org/telegram (third party). Not confirmed: the mobile
   skin-tone gesture.
10. **Large emoji.** "Any message with up to three emoji shows them large and
    without a bubble." https://t.me/s/desktop (Telegram Desktop's channel). Not
    confirmed for iOS and Android separately.
11. **Interactive emoji and dice.** https://telegram.org/blog/video-stickers-better-reactions ,
    https://telegram.org/blog/custom-emoji ; dice list `emojies_send_dice`:
    https://core.telegram.org/api/config .
12. **Premium and paid.** Custom emoji: https://telegram.org/blog/custom-emoji ;
    star reactions: https://telegram.org/blog/superchannels-star-reactions-subscriptions ;
    Saved Messages tags: https://telegram.org/blog/new-saved-messages-and-9-more ;
    channel custom reactions by boost level:
    https://core.telegram.org/bots/sandbox-premium-faq-channels (a "sandbox"
    page).
