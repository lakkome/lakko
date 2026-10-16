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

**Signed out.** The same page with one primary button, **Sign in to add Anna**,
and a secondary **Create an account**. After either, B returns to this page
with the default state, not to the chat list.

**Adding.** **Add** shows a spinner in place of its label and disables both
buttons until the request is written. Then the page closes into the chat with A
(section 4).

**Already contacts.** Title **Anna is already in your contacts**, one button
**Open chat**. Nothing is sent.

**Own link.** Title **This is your own invite link**, body "Share it with the
person you want to add.", one button **Done**.

**Link no longer works** (revoked, expired, used up, or the id is unknown):
title **This invite link no longer works**, body "Ask the person who sent it
for a new one.", one button **Done**. The name is not shown: a revoked link's
contents are gone.

**Damaged link** (the secret after `#` is missing or wrong): the same copy as
"no longer works". A person cannot tell the two apart and should do the same
thing.

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

If A's app refuses (section 5, last bullet), B's chat keeps the line. B is told
nothing more; the next time B opens the chat after 7 days without an answer the
line reads **Anna has not answered yet. You can ask them for a new link.**

## 5. A: the invited person becomes a contact on their own

- A's app, on receiving a request written with one of A's own live links,
  accepts it by itself. B appears in A's contact list and chat list; the
  normal notification for a new message tells A when B writes, and nothing
  separate announces the new contact.
- The link's own record is updated: a single-use link is used up and
  disappears from A's link list; a multi-use link counts down.
- **No request card** is shown for such a request.
- A request that names a link A no longer holds as live (revoked, expired,
  used up on another of A's devices, or unknown) is **not** taken on its own.
  It is shown as an ordinary request card, with one line under the name:
  "Sent with an invite link that no longer works." A decides. (Decided by the
  maintainer 2026-10-16 over dropping such a request silently.)

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

## 8. Same on all three clients

The page, the copy and the states are the same on web, iOS and Android. Web
shows the page as a centred card; iOS and Android as a full-screen sheet opened
by the deep link. The strings go into the catalog (`l10n/catalog`) once and are
used by all three.
