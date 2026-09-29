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
fingerprint stays, as the one thing on the page the inviter cannot fake, but it
stops being the headline.

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
Invited by <fingerprint>            (small, muted, one line)
```

- The inviter's name is the headline's subject. A missing inviter name reads
  `You are invited to`.
- A missing group name reads `a group` in the headline (`Anna invites you to a
  group`) and the group line is omitted.
- `1 member` / `N members`; the count is the roster at mint time.
- The fingerprint moves to the foot of the card in the muted style, prefixed
  `Invited by`, so a person who was told the digits out of band can still
  compare them. It is never the only thing on the page when a preview exists.

**Without a preview** (a link minted without one, or a link whose fragment was
lost - a chat app that strips fragments, a hand-typed URL):

```
You are invited to a group
[ Request to join ]
Invited by <fingerprint>
```

Nothing invents a name. The page does not say why the preview is missing.

**Damaged link** (amended 2026-09-28, short links): the fragment is present but
malformed, or present while the capability carries no preview, or the preview
does not open under it - a wrong or truncated key, a tampered link, or a
capability a server swapped in under a live short id (the preview key is bound
to the group id and the issuer, so a swapped capability never opens). The page
refuses: `This invite link is damaged. Ask for a new link.` and nothing else.
There is no preview-less fallback once a fragment is present; before the short
ids this read "identical to without a preview", which was safe only while the
id was a hash of the capability. A malformed fragment is refused before any
request is made.

**Too many requests** (the ledger answered 429 on fetch or consume, any link
kind): `Too many requests from this network right now. Try again in a minute.`
and `Try again`. Never shown as an invalid link.

**Invalid or expired link** - unchanged: `This invite link is invalid or has
expired.` and a link to open Lakko.

**Not signed in** - unchanged: the preview is shown above the sign-up / log-in
buttons when it opens, so the person knows what they are signing up to join.

**After `Request to join`** - unchanged (`Request sent. An admin approves it,
then the change is applied to the group.` and `Request again`); the preview
stays on the card while they wait, and the page moves to the chat the moment
the join lands.

## 4. What the link looks like

`https://lakko.me/g/<slug>#<link secret, base64url, 16 chars>` for both the
pointer form (`<slug>` = the ledger id, 8 chars) and the inline form (`<slug>` =
the whole capability). A pointer link is therefore about 45 characters
(ADR-0028 amendment of 2026-09-27). `Copy link` copies the fragment with it. A link without
a fragment still works; it only loses the preview.

## 5. Copy rules

Sentence case, no exclamation marks, no protocol words. The headline is a
sentence with the inviter as its subject; names are shown as the inviter's
device wrote them, unlinked, with no verification mark (the same rule as the
`Forwarded from` line in [`message-actions.md`](message-actions.md) section 5).

## 6. Data the UI needs

- On mint: the group's name (empty = none), the issuer's own profile display
  name (empty = none), the member count - all already on the device.
- On open: the capability's `hasPreview`, and `openPreview(linkKey)` giving
  `groupName?`, `inviterName?`, `memberCount`, or nothing when the fragment is
  absent; a present fragment whose preview does not open is the damaged state
  (section 3), not an absent preview.
