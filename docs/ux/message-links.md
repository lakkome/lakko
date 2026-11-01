<!-- SPDX-License-Identifier: AGPL-3.0-or-later -->
<!-- SPDX-FileCopyrightText: 2026 Lakko Contributors -->

# Links in messages

A person gets a link in a message and clicks it. That is the whole flow: no
client asks anyone to copy a link and paste it somewhere.

Until 2026-11-01 a link in a message was plain text on every client, so an
invite sent inside Lakko could only be copied. A call link had a card; nothing
else did.

## 1. What a person sees

- A link in the text of a message or a channel post is underlined in the link
  colour, in the bubble of either side, in place, at its own length. The text
  around it stays as it was. There is no preview: a preview would fetch the
  address from the device or from a server, and either tells someone who read
  the message.
- A message that is one call link of this deployment keeps its invitation card
  (`call-links.md` section 3). Nothing changes there.
- In the feed the text sits inside the card, and a click on the card opens the
  conversation, where the link is live.

## 2. What a click does

| Link | Web | Desktop | iOS, Android |
|---|---|---|---|
| A Lakko link of this deployment: `/i/`, `/c/`, `/g/` with its secret | The invite screen, in place | The invite screen, in place | The invite screen, in place |
| Any other `http` or `https` link | A new browser tab | The system's default browser | The system's default browser |

A Lakko link never leaves the app to come back: the page a browser would show
is the same screen, so a device that is already signed in goes straight to it.

The message menu (right-click, long press) opens as it does on the rest of the
bubble, over a link too. A long press on a link is the menu, never the link.

## 3. What counts as a link

One rule, in `lakko_ui_rules::message_links`, so the clients cannot disagree.

- It starts with `http://` or `https://`, in any case, at the start of the text
  or after something that is not a letter or a digit. A bare `lakko.me` is text:
  guessing at domains colours version numbers and file names.
- It ends at white space, a control or invisible character, or one of
  `< > " \`` `. An invisible character can make an address read as another.
- Sentence punctuation at the end is left out of it, and so is a closing bracket
  that the link did not open: `(see https://a.example/b).` links
  `https://a.example/b`.
- It has a host. It stays plain text when it names a user before the host:
  `https://lakko.me@evil.example/` opens `evil.example` while reading as
  `lakko.me`, and nothing legitimate is sent that way.
- It stays plain text when it is longer than 2048 bytes.

What the rule does not do: decide that a link is safe. A link to a bad site is
coloured like any other. Lakko never fetches it.

## 4. From outside Lakko

A link that arrives in another messenger, a mail or anywhere else opens the
page in the browser, and the page offers the desktop app when one may be
installed (`desktop.md` section 7). A phone opens the app itself through its
universal link or app link.

## 5. Cross-platform shape

One text splitter on the shared core returns the text as pieces, each with or
without a link, and each client draws the pieces in its own text view. Web and
desktop use the same component; a client never searches a message for links on
its own.

| | Web and desktop | iOS | Android |
|---|---|---|---|
| Control | `<a>` in the bubble text | Link attribute on the bubble's text | `LinkAnnotation` on the bubble's text |
| Own links | Router navigation | The app's link intake | The app's link intake |
| Keyboard, screen reader | Focusable link | Link trait | Link semantics |
