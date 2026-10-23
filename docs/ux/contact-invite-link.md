<!-- SPDX-License-Identifier: AGPL-3.0-or-later -->
<!-- SPDX-FileCopyrightText: 2026 Lakko Contributors -->

# Adding a contact by invite link

What happens when one person shares a contact invite link (`/i/`) and another
opens it, on web, iOS and Android. Proposed 2026-10-16 after the walk on
`test.lakko.me` that day, where the flow was found to be wrong on every client.

## 1. What was wrong

- The page the invited person saw was headed by the inviter's key fingerprint
  and a twelve-group safety number with advice to compare it. A person cannot
  act on either; identity is the system's job (the same verdict as the group
  invite page, `group-invite-page.md` section 1, and the rule that the user is
  never asked to check keys).
- **Accept** only sent the inviter a contact request. The invited person did
  not get the inviter as a contact, and the inviter had to accept a "wants to
  connect" card from someone they had just invited.

A link is the inviter's consent. Opening it and pressing **Add** makes the two
people contacts, on both sides, with nothing more to do.

## 2. Flow

1. **A** creates a link (Contacts, add menu, Share invite link; unchanged) and
   sends it any way they like.
2. **B** opens it. The page says who invites, by name.
3. B presses **Add**.
4. A appears in B's contacts at once, and B can open the chat and write.
5. A's app, the next time it is online, takes B as a contact with no card and
   no button, and the chat opens on both sides. Messages B wrote meanwhile are
   delivered then.

## 3. The invite page (B)

Shown at `/i/<id>#<secret>` on web, and by the deep link on iOS and Android.

**Default.**

- Avatar (initials of A's name, the avatar colour rule of the contact list).
- Title: **Add Anna to your contacts?** where *Anna* is A's display name.
  Below it A's username, `@anna`, when A has one.
- Optional label line, when A gave the link a label: the label as written.
- Buttons: **Add** (primary), **Not now** (closes the page; nothing is sent).

No fingerprint, no safety number, no expiry, no use count. The safety number
stays where it is today, on the contact's info screen, for anyone who wants it.

**Signed out** (amended 2026-10-19). No inviter card at all: the first screen
is the ordinary **Sign in** / **Create an account** choice. After either, the
link is checked again: if it still works B sees the default page above; if it
no longer works B lands on the Contacts screen (below).

**Adding.** **Add** shows a spinner in place of its label and disables both
buttons until the request is written. Then the page closes into the chat with A
(section 4). A becomes a contact on B's side only after the server has
confirmed the link still works (the use is counted at that moment); if it
answers that the link no longer works, nothing is kept, no message is held and
B lands on the Contacts screen with nothing else shown (amended 2026-10-19).

**Already contacts.** Title **Anna is already in your contacts**, one button
**Open chat**. Nothing is sent.

**Own link.** Title **This is your own invite link**, body "Share it with the
person you want to add.", one button **Done**.

**Link no longer works** (revoked, expired, used up, the id is unknown, or the
link is damaged: the secret after `#` is missing or wrong): there is no page.
B is taken to the Contacts screen and nothing else is shown (amended
2026-10-19; the earlier "This invite link no longer works" page is gone).

**Network failure while opening the link** (added 2026-10-23: the ledger could
not be asked, the network failed or the server answered with an error other
than "no such link" or 429): "Invite not accepted" and **Try again**, which
fetches the link again. Never taken for a link that no longer works; anything
after the ledger answered is.

**Network failure while adding:** the page stays, the button returns, one line
under it: "Could not add Anna. Check your connection and try again."

## 4. B after Add: A is a contact before A's app has answered

- A appears in B's contact list and chat list at once, under A's name.
- The chat with A opens. Until A's app has answered (section 5), the chat shows
  one line at the top of the timeline: **Anna will get your messages when
  their app is next online.** No spinner, no "pending" badge in the lists.
- B can write (decided by the maintainer 2026-10-16). Messages are kept on B's device and shown as sending (the clock
  mark used for any unsent message); they go out, in order, as soon as the
  chat is open on both sides.
- B cannot call A yet: the call buttons are disabled with the tooltip / long
  press hint "Available once Anna is connected."
- When A's app answers, the line goes away and the queued messages send. No
  notification for this on B's side.

If A's app never answers, B's chat keeps the line; the next time B opens the
chat after 7 days without an answer the line reads **Anna has not answered
yet. You can ask them for a new link.**

If A's app refuses (section 5, last bullet), A disappears from B's contacts and
chats and the messages B wrote are dropped, silently: no message, no notice
(amended 2026-10-19). There is no button for B to delete a pending contact.

## 5. A: the invited person becomes a contact on their own

- A's app, on receiving a request written with one of A's own live links,
  accepts it by itself. B appears in A's contact list and chat list; the
  normal notification for a new message tells A when B writes, and nothing
  separate announces the new contact.
- The link's own record is updated: a single-use link is used up and
  disappears from A's link list; a multi-use link counts down.
- **No request card** is shown for such a request.
- **When a link counts** (amended 2026-10-19). The moment B pressed **Add**
  decides, as the server recorded it on the request: a request written while
  the link worked is taken whenever A's app comes online, even after the
  link's expiry. A revocation wins until A's app has answered: a link A
  revoked before A's app took the request refuses it, even if B pressed Add
  earlier.
- A request that reaches A with a link that no longer counts (revoked before
  A's app answered, expired before B pressed Add, used up, or unknown) is
  dropped silently: no card, nothing shown to A. A's app answers B with a
  refusal, and B's app removes A from B's contacts silently (section 4). (This
  overturns the 2026-10-16 decision to show such a request as a card with the
  line "Sent with an invite link that no longer works"; that card and its line
  are gone.)

## 6. A's other devices

A's other devices see B as a contact through the normal contact sync. Only one
device answers: the first that sees the request. A second device that finds
the link already used treats the request as answered, not as an expired link.

## 7. Data the UI needs (feeds the ADR)

1. **A's display name and username inside the link.** Today the sealed `/i/`
   capability carries a label and A's key, not a name. Add A's profile (display
   name, optional username) to the sealed capability, under the link secret
   like everything else in it, so the server still sees ciphertext only.
2. **Proof the request came with A's link.** The contact request's sealed
   plaintext gains an optional field: the link's id and secret as B received
   them. A's app checks them against its own link records (it holds every link
   it made, `own-links` on each client) before it accepts by itself. The
   server never sees the field.
3. **A's own link records must know use counts and the used-up state** across
   A's devices (they already travel in the DeviceGroup sync of link records,
   to be confirmed per client).
4. **B's queue of messages for a chat that does not exist yet**, kept on B's
   device under the account store gate, bound to A's account key, cleared
   when sent or when B deletes the contact.
5. **The "not answered for 7 days" marker**: the time B pressed Add, kept with
   the pending contact.
6. **When B pressed Add, as the server saw it** (2026-10-19): the time the
   server stamped on the request row in A's mailbox. B cannot back-date it. A's
   app keeps it with a request it has not decided yet, across a restart.
7. **A refusal back to B** (2026-10-19): a small message signed by A's account
   key naming the link and B, sent the way the request said to answer, and
   checked by B against A's key before B removes the pending contact, so
   nobody else can remove it.

## 8. Same on all three clients

The page, the copy and the states are the same on web, iOS and Android. Web
shows the page as a centred card; iOS and Android as a full-screen sheet opened
by the deep link. The strings go into the catalog (`l10n/catalog`) once and are
used by all three.
