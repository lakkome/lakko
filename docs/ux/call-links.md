<!-- SPDX-License-Identifier: AGPL-3.0-or-later -->
<!-- SPDX-FileCopyrightText: 2026 Lakko Contributors -->

# Starting a call, and inviting someone who is not in Lakko

Everything below is a requirement for all three clients. Where a platform
genuinely has to differ, the difference is stated inline and marked
**Per platform**.

## 0. What was wrong

Checked on all three clients on 2026-09-23, signed in, against a deployed
stack. The Calls destination offered exactly two actions: `Create call link`
and `Join by link`.

**You could not place a call from the Calls screen.** To call someone you
went to Contacts or to a chat. The place named after the act did not perform
the act.

What stood in its place was a form. `Create call link` asked four questions -
audio or video, when the link expires, how many uses, what to label it - and
produced a string of text, which the person then carried somewhere else
themselves. Three clients asked those four questions three different ways,
and two of them disagreed about what one of the answers meant (`Once`
counting redemptions against `1 person` counting people).

The vocabulary and the controls were the visible part. The real fault was the
shape: a person was made to build, configure and carry a **link**, when what
they wanted was to **be in a call**.

## 1. The decision

**The maintainer's, on 2026-09-23**, and it is simpler than anything
proposed to them:

> A person in the contact list can already be called without a special
> button. So the button makes a **group video call, always** - one kind of
> thing rather than several - and the person who pressed it **lands in the
> call**. From inside the call they can copy a link, to bring in someone who
> is not in their contacts or not on Lakko at all. Lifetime limits,
> participant limits and labels are not wanted here.

Three consequences, each of which deletes something:

**The link stops being an object.** It is not created, configured, named and
carried. It is a property of a call that is already happening, the way a
meeting's link is - available when wanted, absent from the mind when not.

**The settings go away entirely.** Not "collapsed behind a disclosure" - away.
They existed because the link was a standalone artefact that had to be given
a shape; once it belongs to a live call, its lifetime is the call's and its
audience is whoever the host admits. Nothing is left to decide.

**There is one kind of call.** Group, video. A voice call is a video call
with the camera off, which the in-call controls already do, so offering the
choice up front only asks a person to predict something they can change in
one tap.

## 2. The Calls destination

The history ([`navigation.md`](navigation.md) section 5) and one action.

```
Calls                                    [ New call ]

  Yesterday
  Anna Weber      Outgoing call, 12:04      1:40 PM   [call back]
  ...
```

`New call` starts a group video call and shows the call screen. Nothing is
asked first: no media choice, no participants, no name. A person who wanted
to call one specific contact has a faster route from the conversation or the
contact, which is why this button does not try to be that route.

`Join by link` stays, for a link someone has sent that did not open by
itself. It is not the destination's main action and is not given the weight
of one.

**Per platform** — the action's idiom: web a button in the header, Android a
FAB or a header action, iOS a toolbar button. What must be true everywhere is
that it is ONE action and it starts a call rather than opening a form.

## 3. Inviting from inside the call

The call screen has one control for the people of the call,
**`Participants`**, and inviting lives in it (until 2026-10-02 `Invite` was a
button of its own). On a call that can hand out a link it opens a menu of the
app's own, above the control bar:

- **`Participants`** opens the list of who is in the call. The item is there
  only once someone else has joined.
- **`Copy invite link`** puts the link to this call on the clipboard, and one
  line under the status row says so. Where the clipboard cannot be reached the
  line shows the link itself to be copied by hand.
- **`Invite contacts`** opens the list of the person's **contacts**, by name,
  with a search field, under the title `Invite to call`. Contacts, not
  conversations: an invitation is addressed to a person, and a contact the
  person has never written to is as invitable as one they talk to daily (the
  first version listed conversations, so someone with contacts and no chats
  had nobody to invite; corrected 2026-10-02). Groups are not in the list.
  - Each row is the contact's picture, name and `@username` when one is known.
  - Choosing a contact sends the invitation into the one-to-one conversation
    with them. When this device holds no such conversation it is created
    first, the way `Message` on the contact's card creates it; the contact
    then finds a new conversation whose first message is the invitation.
  - The list stays open so several contacts can be invited; the chosen row
    shows a check and `Invited` and cannot be chosen twice, and the line under
    the status row says `Invitation sent to <name>.`
  - A failure leaves the row pressable and shows the reason; the call is
    untouched.
  - The item is there for every account. With no contacts the list reads
    `No contacts yet. Copy the invite link and send it another way.` (An
    earlier version hid the item when the list would be empty, which read as a
    missing feature.) Only a guest who came by a link (section 9), who has no
    contacts, does not get the item.
  - **Open (2026-10-02): who gets the menu.** On web everyone in the call
    who has an account gets it; on iOS and Android only the person who
    started the call does. Not aligned yet, because a link made by someone
    who joined names that person as the call's owner, and people who came in
    by different links can then disagree about who admits a newcomer once the
    person who started the call has left (ADR-0032's election reads the
    owner from the link). Until that is settled the two rules stand as they
    are.

**Never the system's share sheet.** The first version handed the link to
`navigator.share` and the iOS and Android share sheets: a foreign surface in
the middle of a call, different on every platform, offering a dozen
destinations where two are wanted (found live 2026-10-02). The rule is general
and lives in the design system: a Lakko screen uses an operating-system element
only where the platform leaves no other way.

Both items offer the link to THIS call - the same `call_id` the people already
in the call are using, so the person who follows it arrives in this call and
not a new one.

**The invitation is a message.** Its whole text is the call's link, and every
client draws a message that is exactly one call link of its own deployment as
an invitation: a phone glyph, `Call invitation`, and a `Join` button that opens
the call inside the app. A link to another host, or a link with other text
around it, stays ordinary text. No new wire field: a client that predates this
shows the link. **It does not ring the invited person**; they see a message.
Ringing an invited contact is not built.

The first use of `Invite` in a call is what mints the capability
([ADR-0032](../adr/0032-multi-joiner-call-links.md)), which costs a network
round trip. Doing it here rather than at `New call` is what keeps starting a
call instant: the cost is paid by the person who actually wants a link, at the
moment they want it, and never by everyone else. Later uses in the same call
reuse the link already minted.

Arrivals are admitted by the host exactly as today
([ADR-0032](../adr/0032-multi-joiner-call-links.md),
[ADR-0027](../adr/0027-external-join-approval.md)). That admission - not the
link's secrecy - is the boundary, which is the reason the removed limits cost
nothing: a link nobody is admitted through admits nobody.

## 4. The values the link carries

`CallInviteCapability` has fields for expiry, use count, media and label. The
person is no longer asked about any of them, so the client fills them:

| Field | Value | Why |
|---|---|---|
| `call_id` | the live call's | What makes the link lead HERE. |
| `media` | audio + video | One kind of call. |
| `expiry_ms` | a fixed window, long enough to outlast a call | Never `CALL_NEVER_EXPIRES`: a link that works forever is the stalking risk the field's own documentation names, and nothing needs it. |
| `max_uses` | unbounded | The host admits each arrival, so a count here would cap a meeting for no gain. |
| `label` | empty | Nothing to say that the call screen does not. |

These are one decision in one place, shared by the three clients rather than
written out three times - the reason [`navigation.md`](navigation.md) section
8 gives.

## 5. States

| State | What it shows |
|---|---|
| Calls, empty | The history's empty state and `New call`. |
| `New call` pressed | The call screen, connecting. Never a form. |
| `Invite`, first time in this call | Busy while minting, which includes a proof-of-work solve, so it says what it is doing rather than only spinning. |
| `Invite`, link ready | The platform's share mechanism, or a copy with confirmation. |
| Mint failed | One sentence on what happened and that it can be tried again. The call is unaffected and does not end. |
| Opening a damaged link (the arriving side) | `This invite link is damaged. Ask for a new link.` and nothing else. Shown when the fragment is missing or malformed, or the link does not open under it (see below). |
| The link could not be fetched (the arriving side; added 2026-10-23) | `Could not reach Lakko. Check your connection and try again.` and `Try again`. Shown when the network failed or the server answered with an error other than "no such link" or 429; never `This call link is invalid or has expired.` |
| Signed out mid-flow | One sentence: sign in again. |
| A call joined from a link ends (the arriving side, signed in) | Back in the app where the person was, never the link page with `The call has ended.` (added 2026-10-03). A guest stays on the link page, where section 9 asks about an account. |

### The link's shape (amended 2026-09-28)

`https://lakko.me/c/<8-char id>#<16-char secret>`, or
`https://lakko.me/c/<sealed capability>#<16-char secret>` when the upload
failed. The capability is encrypted under the fragment's secret
([ADR-0009](../adr/0009-link-based-call-invitations.md) and
[ADR-0028](../adr/0028-group-join-ticket.md), the sealed-link amendment), so
the share action and any copy must carry the fragment: **a link without its
fragment is damaged**, the same state and sentence as a `/g/` or `/i/` link
([`group-invite-page.md`](group-invite-page.md) section 3). A missing or
malformed fragment is refused before any request is made. This is what stops a
guessed or leaked short id from being a seat in the call.

## 6. What is deleted

The create-link form, on all three clients: web `CreateCallLinkPanel`, iOS
`CreateCallLinkSheet`, the Android dialog, and the preset lists, labels and
defaults that fed them. Whatever survives is only what section 4 needs.

## 7. What the data has to provide

Nothing new. `CallInviteCapability` already carries every field, and minting
already reports failure. What changes is when it is minted, by whom, and that
four of its fields stop being questions.

## 8. One model, three implementations

One action in Calls that starts one kind of call; one `Invite` inside the
call; no settings anywhere. The **Per platform** notes above - the action's
idiom and the share mechanism - are the only differences, and each is there
because the platform gives a person a different idiom for the same act.

## 9. Joining without an account (rewritten 2026-10-02)

A person who is sent a call link and has no Lakko account wants the call. The
first version made them an account on `Join` and then stood the 24-word
recovery phrase between them and the call: a screen about keeping an account
they had not asked for, at the one moment they are in a hurry. Whether they
want an account is a question for after the call.

### 9.1 The flow

1. **The link page.** A name field prefilled `Guest`, `Join call`, and one
   line under the button: `You join as a guest. No account is needed.`
2. **`Join call`.** A guest account is made silently (the busy row of
   [`anti-abuse.md`](anti-abuse.md) section 1 while it is) and the call starts.
   No recovery phrase, no username, no password, no notification prompt.
3. **In the call** a guest is a participant like any other, under the name
   they typed. `Invite contacts` is absent (section 3): a guest has no
   contacts.
4. **A guest never lands in the app.** Its chat list, contacts and settings
   are an empty shell for someone with no account of their own. Wherever the
   app would open:
   - while a call is running: `You are in a call as a guest.` and
     `Return to call`;
   - otherwise: the question below.
5. **The question**, shown when the guest is not in a call - on the link page
   under `The call has ended.`, after a join that failed, after the page or the
   app is reopened:

   > **Keep using Lakko?**
   > You are here as a guest. Create an account to keep your name and to call
   > or message people again.
   > `Create account` · `No, thanks`
   > If you decline, the guest account is deleted right away.

6. **`No, thanks`** deletes the account at once: on the servers and every
   trace of it on the device. No confirmation dialog; the line under the
   button has already said what it does. The link page then reads
   `The guest account was deleted.` and offers Join again. If an earlier
   `Create account` had already reserved a username, that username is
   released first. While it runs the button reads
   `Deleting...`. If the servers cannot be reached:
   `The guest account could not be deleted now. Try again; it is also deleted
   automatically within a day.`, and both buttons stay.
7. **`Create account`** opens a short form: username (with the live rule hint
   of the sign-up screen), password, confirm password, `Create account`,
   `Back`. On success the recovery phrase is shown by the same two-step screen
   an ordinary sign-up ends with - and by the same rule, before any other
   screen and again after a restart until it is acknowledged - and then the
   app opens.
8. **Unanswered.** A person who closes the tab or the app without choosing
   leaves nothing behind for long. A guest account lives less than a day:
   - the servers delete an account that was never given a username a day
     after it was made;
   - the device stops an hour earlier (23 hours): the next time the app or
     the page opens after that, the guest account is deleted without a
     question, on the servers and on the device, and the person sees the
     ordinary signed-out screen (on the link page: Join, as for any visitor);
   - a guest whose sign-in can no longer be restored on the device (it
     expired, or storage was partly cleared) is wiped from the device the same
     way. A guest account is never left as a remembered, signed-out account:
     nobody knows its password.
   Coming back within the 23 hours shows the question again.

### 9.2 States of `Create account`

| State | What it shows |
|---|---|
| Working | The button reads `Creating your account...`, the fields are disabled. |
| Username taken | `That username is taken. Choose another one.`, the form stays filled. |
| Username already reserved by an earlier attempt | In place of the field: `Your username is @name.` A username cannot be changed, so a second attempt after a failure goes on with the one already taken. |
| No connection, too many attempts, service unavailable | The sentences the sign-up screen uses. |
| Anything else | `The account could not be created. Try again.` |
| The device no longer holds the guest's recovery phrase (storage was cleared) | `This guest account cannot be kept on this device. You can create a new account from the sign-up screen.`, and only `No, thanks` remains. An account whose phrase nobody ever saw could not be recovered. |

### 9.3 What a guest account is, in a sentence for each

- It is a full account with no username and a password nobody knows, so it
  can be in an end-to-end encrypted call like anyone else.
- While it is a guest, nothing about it is kept on a server beyond what the
  call needs: no backup, no three-word recovery, no push registration.
- Its recovery phrase stays on the device, encrypted, until the person decides:
  `Create account` shows it, `No, thanks` destroys it with the account.

### 9.4 What the data has to provide

- The device: a marker that the account is a guest (and the username an
  unfinished `Create account` took), and the phrase under a key of its own.
- `services/auth` and `services/discovery`: each deletes, on a timer, accounts
  older than a day with no username (and, at auth, no passkey).
  [ADR-0032](../adr/0032-multi-joiner-call-links.md), "Guest lifecycle".
- Nothing new on the wire: `Create account` is the existing username claim
  followed by the existing phrase-proved password reset; `No, thanks` is the
  existing account deletion.

### 9.5 One model, three implementations

The same question, the same two buttons, the same form and the same sentences
on web, iOS and Android. No system alert or confirmation dialog on any of
them.
