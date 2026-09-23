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

The call screen gains one control: **`Invite`**.

It offers the link to this call - the same `call_id` the people already in
the call are using, so the person who follows it arrives in THIS call and not
a new one. **Per platform**, it goes to the platform's own share mechanism
(iOS and Android share sheets, `navigator.share` on web where it exists);
where there is none, it copies and says so.

The first use of `Invite` in a call is what mints the capability
([ADR-0032](../adr/0032-multi-joiner-call-links.md)), which costs a network
round trip and a proof-of-work solve. Doing it here rather than at `New call`
is what keeps starting a call instant: the cost is paid by the person who
actually wants a link, at the moment they want it, and never by everyone else.
A second `Invite` in the same call re-offers the link already minted.

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
| Signed out mid-flow | One sentence: sign in again. |

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
