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

**Signed out** (amended 2026-10-26, replacing the 2026-10-19 rule that showed
no card). A person with no account, or not signed in on this device, must see
that someone invites them before being asked to sign in:

- Avatar, then the title **Anna invites you to Lakko** (A's display name, else
  `@username`, else "Someone"), A's `@username` under it when A has one, and
  the label line when the link has one.
- One line: **Lakko is a messenger where every chat and call is end-to-end
  encrypted.**
- Buttons: **Create an account** (primary) and **I already have an account**
  (secondary). Both carry the link as the return, so after sign-up or sign-in
  the link is opened again: still working, the default page above; no longer
  working, the Contacts screen (below).
- A link that does not work, opened signed out: title **This invite link no
  longer works**, line **Ask the person who sent it for a new one.**, and the
  same two buttons with no return. A link with no uses left counts (amended
  2026-11-01): the ledger still serves it, flagged `used_up`, because a person
  who used it and is signed in must reach "already in your contacts", and only
  the consume can tell them from anyone else.

The card is built on the device from the link itself (the ledger fetch needs no
session and the name is sealed under the link's secret); nothing is consumed
and nothing is sent until Add.

**Adding.** **Add** shows a spinner in place of its label and disables both
buttons until the request is written. Then the page closes into the chat with A
(section 4). A becomes a contact on B's side only after the server has
confirmed the link still works (the use is counted at that moment); if it
answers that the link no longer works, nothing is kept, no message is held and
B lands on the Contacts screen with nothing else shown (amended 2026-10-19).

**Already contacts.** Title **Anna is already in your contacts**, one button
**Open chat**. Amended 2026-10-24: Open chat still delivers the link's answer,
because A may have removed B and sent the link to get B back (seen on
test.lakko.me: B saw "already in your contacts", nothing was sent, and A still
did not have B). The button shows a spinner, the ledger consume runs, the
request carrying the link answer is sent, and the chat opens; nothing is kept
on B's side (A already is a contact). A link that no longer works sends
nothing and the chat opens. A network failure leaves B on the page with "Could
not reach Anna. Check your connection and try again." On A's side, a request
whose link counts (or was already answered for B) from a person A already holds
a live one-to-one with is not a new contact: each of A's devices puts B back
on its contact list with that conversation, founds nothing and sends nothing,
and a live link records its use.

**Already added, waiting for A** (amended 2026-10-24). B pressed Add on one of
A's links and A's answer has not arrived (A is B's pending contact, section 4).
Seen on the local stack: a second link from A, opened in that window, showed
the Add page again. The page now says **Anna is already in your contacts** and
**Open chat** opens the waiting chat; nothing is sent, because a request on a
second link is decided apart from the first and could give the two a second
one-to-one. The page follows the contact list while it is open, so A's answer
landing turns it into the ordinary "already" state.

**B removed A, the chat remains** (amended 2026-10-24). A's link opens the
ordinary Add page (A is not on B's list). Add sends the request as usual but
writes no pending contact: A's devices answer it by restoring, never by
founding (above), so B puts A back on the list with the conversation they have
once the request is sent, and the page opens that chat.

**Own link.** Title **This is your own invite link**, body "Share it with the
person you want to add.", one button **Done**.

**Link no longer works** (revoked, expired, used up, the id is unknown, or the
link is damaged: the secret after `#` is missing or wrong): there is no page.
B is taken to the Contacts screen and nothing else is shown (amended
2026-10-19; the earlier "This invite link no longer works" page is gone).

**Network failure while opening the link** (added 2026-10-23: the ledger could
not be asked, the network failed or the server answered with an error other
than "no such link" or 429): "Could not open the invite. Check your connection
and try again." and **Try again**, which
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

## 9. A's list of links (2026-10-26)

The Invite links screen (Contacts) lists the account's live links, newest
first, on every device of the account.

- **Create link** makes a link at once with the default terms (no label,
  7 days, one person) and puts it at the top of the list. There is no form
  before it.
- Each row: the label when there is one (no title line otherwise), then the
  facts (created date and time, time left, the limit or the uses left), then
  the link, **Copy**, **QR code** and the trash button **Delete link**.
- **Edit link** (a pencil) is shown only while nobody can hold the link: it
  was never copied on any of the account's devices and nobody was accepted
  through it. One rule decides it for all three clients
  (`LinkRecordEntry::editable` in `lakko-sync`). Showing the QR code does not
  end editing; a scan leads to Add, and a use ends it.
- The pencil opens the link's terms: label, Expires (1 / 7 / 30 days, never,
  custom 1 hour to 365 days), Uses (one, up to 10, unlimited, custom 1 to
  10 000), prefilled with the link's terms, and the line **Saving makes a new
  link. The old one stops working.** **Save** mints a new link with those
  terms first, then deletes the old one, so a failure leaves the old link
  working and says **Could not save the link. Check your connection and try
  again.**; **Cancel** changes nothing.
- **Copy** marks the link copied in this device's synced state before the
  text reaches the clipboard; the mark goes to the account's other devices in
  the background (Copy never waits on the network; a failed send is retried),
  and from then on no device shows the pencil.
- **Delete link** removes the link from the list on every device and makes
  it stop working (the old Revoke). A link that was never copied and never
  used is deleted at once; a link that was copied or used asks first, in a
  Lakko dialog: **Delete this link?** / **Anyone who already has it will no
  longer be able to use it.** / **Delete** / **Cancel**. After it: **Link
  deleted.**; a failure: **Could not delete the link. Check your connection
  and try again.**
