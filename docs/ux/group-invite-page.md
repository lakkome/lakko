<!-- SPDX-License-Identifier: AGPL-3.0-or-later -->
<!-- SPDX-FileCopyrightText: 2026 Lakko Contributors -->

# The group invite page

What a person sees when they open a `/g/` link, on web, iOS and Android.
Companion to [`proposal-based-membership.md`](proposal-based-membership.md)
(what happens after `Request to join`) and to
[ADR-0028](../adr/0028-group-join-ticket.md) (what the link carries).

## 1. Why this page was rewritten

Until 2026-09-26 the page read `Invited by:` followed by twelve groups of five
digits - the inviter's key fingerprint. The maintainer's verdict, on seeing it:
it means slightly less than nothing to the person invited. A link is an
invitation, and an invitation says who is inviting you and to what. The
fingerprint stayed at the foot of the card until 2026-11-01, when it left the
page altogether.

## 2. What the page needs, and where it comes from

The page needs three facts the link did not carry: the group's name, the
inviter's name and how large the group is. The link now carries them as a
**sealed preview** inside the signed capability, encrypted under a **link key**
that lives in the URL fragment (`#<key>`). The fragment is the one part of a
URL a browser never sends and an app never uploads, so the discovery ledger
(which stores pointer-link capabilities) and every access log stay
ciphertext-only. Anyone who holds the full link can read the preview - that is
the point of a link one shares. The preview is what the inviter's device knew
at mint time; a rename after that is not reflected, and the count is a
snapshot.

## 3. States

Every state shows one card. The card's headline changes; the controls do not.

**With a preview (the normal case):**

```
<Inviter name> invites you to
<Group name>
<N> members

[ Request to join ]
Invited by <Inviter name>           (small, muted, one line)
```

- The inviter's name is the headline's subject. A missing inviter name reads
  `You are invited to`.
- A missing group name reads `a group` in the headline (`Anna invites you to a
  group`) and the group line is omitted.
- `1 member` / `N members`; the count is the roster at mint time.
- The foot line carries the inviter's name in the muted style, prefixed
  `Invited by`. With no name the line is gone. Amended 2026-11-01: digits never
  appear on this page; they meant nothing to the person reading it.

**Without a preview** (a link minted without one; since 2026-09-28 a link
whose fragment was lost is no longer this state, it is damaged - below):

```
You are invited to a group
[ Request to join ]
```

Nothing invents a name. The page does not say why the preview is missing.

**Damaged link** (amended 2026-09-28, sealed links): the fragment is
missing, malformed (not 16 base64url characters), or does not open the link -
since the sealed-link amendment of ADR-0028 the whole capability is encrypted
under the fragment's secret, so a lost fragment (a chat app that strips
fragments, a hand-typed URL), a wrong or truncated secret, a tampered link, or
a blob a server swapped in under a live short id all fail the same way, and so
does a present preview that does not open. The page refuses: `This invite link
is damaged. Ask for a new link.` and nothing else - no fingerprint, no
`Request to join`, because nothing was opened. A missing or malformed fragment
is refused before any request is made. **The same state and the same sentence
apply to every link kind** - `/i/` (`This invite link is damaged. Ask for a
new link.`) and `/c/` (see [`call-links.md`](call-links.md) section 5).

**Too many requests** (the ledger answered 429 on fetch or consume, any link
kind): `Too many requests from this network right now. Try again in a minute.`
and `Try again`. Never shown as an invalid link.

**Could not load the link** (added 2026-10-23; the ledger could not be asked:
the network failed, or the server answered with an error other than "no such
link", on `/g/` and `/c/`): `Could not reach Lakko. Check your connection and
try again.` and `Try again`, which fetches the link again. Never shown as an
invalid link. Only the fetch can end here; anything after the ledger answered
is damaged or invalid. `/i/` has the same rule with its own copy
([`contact-invite-link.md`](contact-invite-link.md)).

**Invalid or expired link** - unchanged: `This invite link is invalid or has
expired.` and a link to open Lakko. Amended 2026-11-01: a link with no uses left reads the same to a visitor with
no session, from the `used_up` flag of the ledger fetch. With a session the page
stays, and the consume decides.

**Not signed in** - unchanged: the preview is shown above the sign-up / log-in
buttons when it opens, so the person knows what they are signing up to join.

**After `Request to join`** - unchanged (`Request sent. An admin approves it,
then the change is applied to the group.` and `Request again`); the preview
stays on the card while they wait, and the page moves to the chat the moment
the join lands.

## 4. What the link looks like

`https://lakko.me/g/<slug>#<link secret, base64url, 16 chars>`, `<slug>` = the
ledger id, 8 chars, so a link is about 45 characters (ADR-0028 amendment of
2026-09-27). There is no other form (the inline form that carried the whole
sealed capability in the path was removed on 2026-10-19): when the upload
fails, a network failure included, no link is made and the person sees the
ordinary error with the button back for another try; a link in the old inline
shape opens as an invalid link. `Copy link` copies the fragment
with it, and must: since 2026-09-28 a link without its fragment opens nothing
and is shown as damaged. The same shape holds for `/i/` and `/c/`.

## 5. Copy rules

Sentence case, no exclamation marks, no protocol words. The headline is a
sentence with the inviter as its subject; names are shown as the inviter's
device wrote them, unlinked, with no verification mark (the same rule as the
`Forwarded from` line in [`message-actions.md`](message-actions.md) section 5).

## 6. Data the UI needs

- On mint: the group's name (empty = none), the issuer's own profile display
  name (empty = none), the member count - all already on the device.
- On open: `GroupInviteCapability.openLink(sealed, linkSecret)` (a refusal is
  the damaged state), then the capability's `hasPreview`, and
  `openPreview(linkSecret)` giving
  `groupName?`, `inviterName?`, `memberCount`, or nothing when the fragment is
  absent; a present fragment whose preview does not open is the damaged state
  (section 3), not an absent preview.
