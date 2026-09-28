<!-- SPDX-License-Identifier: AGPL-3.0-or-later -->
<!-- SPDX-FileCopyrightText: 2026 Lakko Contributors -->

# Who can find me

A person decides who can find them by username or by name, and so who can
ask to become their contact. Everything below is a requirement for all three
clients. Where a platform genuinely has to differ, the difference is stated
inline and marked **Per platform**.

Architecture: [ADR-0058](../adr/0058-scoped-discovery.md).

## 1. The rule, as the maintainer set it

There are two ways to become someone's contact:

- **A link or a QR code** they gave you. This always works, whatever the
  setting below. Handing someone a link is the person's own decision to be
  reachable by whoever holds it.
- **Search**, by `@username` or by name. This works only for the people the
  setting allows.

The setting has three positions:

1. **Nobody.** No one finds the person by username or by name.
2. **Some people**, made of two parts that can be on together:
   - **2.1 Contacts of my contacts.** People who are a direct contact of
     one of the person's contacts.
   - **2.2 Members of my groups and channels**, in one of two forms, never
     both:
     - **2.2.1 All of them.** Every member of every group and channel the
       person is in.
     - **2.2.2 Only the ones I choose.** The members of the groups and
       channels the person has switched on, one by one.
3. **Everyone.** Anyone finds the person by username and by name.

Choosing 1 turns 2 and 3 off, choosing 3 turns 1 and 2 off, and 2.1 and 2.2
combine freely.

**Knowing someone is not permission to reach them.** Knowing a person's
username, having seen them in a group, or holding any identifier of theirs
does not let you send them a contact request or a message. Only the setting
(through search) or a link from them does. Existing contacts are unaffected:
a contact can always write.

**Default for a new account: Some people, with 2.1 and 2.2.1 on.**

## 2. Where the setting lives

**Settings > Privacy > Who can find me.** Privacy is a new Settings section,
placed after Profile ([`design-system.md`](../../.claude/skills/ux-ui/design-system.md)
section 4 is updated with it). It is about people, not keys, which is why
it is not under Security.

A second entry point per conversation: **group info and channel info carry a
`Members can find me` switch** (section 5).

A third, once: the last step of signup says in one line where the person
stands and where to change it (section 7).

## 3. The screen

```
Who can find me
Whoever can find you by username or name can ask to be your contact.
People with your invite link or QR code can always reach you.

( ) Nobody
(•) Some people
      [x] Contacts of my contacts
      [x] Members of my groups and channels
            (•) All groups and channels
            ( ) Only the ones I choose          >
( ) Everyone
```

Controls, top to bottom:

- **The three positions** are one single choice (radio rows). Choosing one
  takes effect immediately; there is no Save button.
- **The two parts of Some people** are switches (check rows), shown only
  while Some people is chosen, indented under it.
  - **Turning off the last one that is on selects Nobody.** Some people with
    nothing on means nobody, so the screen says what it means rather than
    showing a choice that contradicts its own contents. Turning either part
    back on selects Some people again.
  - Choosing **Some people** from Nobody or Everyone restores the parts the
    person last had on; if they never had any, the default parts (2.1 and
    2.2.1).
- **All / Only the ones I choose** is a single choice, shown only while
  Members of my groups and channels is on.
- **Only the ones I choose** has a disclosure that opens the list (section
  4). Selecting it for the first time opens the list straight away.

The row descriptions, as muted helper text under each position:

| Position | Helper text |
|---|---|
| Nobody | Only people with your invite link or QR code can reach you. |
| Some people | (the parts speak for themselves) |
| Contacts of my contacts | People your contacts have added. |
| Members of my groups and channels | Everyone in the groups and channels you're in. / Only the ones you choose (N of M). |
| Everyone | Anyone can find you by your username or name. |

The `N of M` count is live.

## 4. The list of groups and channels

Opens from **Only the ones I choose**. One row per group and per channel the
person is in, grouped **Groups** then **Channels**, each row the avatar, the
name and a switch. A search field on top when there are more than a screen
of them.

- **Empty**: "You're not in any groups or channels yet." A group joined
  later appears here switched **off**: under Only the ones I choose, each
  one is a decision the person makes.
- Under **All groups and channels**, a group joined later is included
  automatically, which is what "all" means.

## 5. The switch in group and channel info

`Members can find me`, in the group info ([`conversation-info.md`](conversation-info.md)
section 2.2) and the channel info (section 2.3), shown only while Members of
my groups and channels is on. When the setting does not include groups at
all, the row is absent rather than dead: a switch that cannot be moved is
noise.

- Under **Only the ones I choose**, it is that group's switch from the list
  in section 4, the same value in two places.
- Under **All groups and channels**, it shows on. Turning it off switches the
  setting to **Only the ones I choose** with every other group and channel
  on and this one off, and says so once, in place: "Now only the groups and
  channels you choose. Change in Privacy." That is the one way to exclude
  one group without first visiting Settings.

## 6. What other people see

### 6.1 Searching

The Chats search field keeps its two halves ([`navigation.md`](navigation.md)
section 4): the local filter, and the one network row.

- Local results (contacts, members of the person's groups and channels)
  appear as the person types, from the first letter.
- Text of at least 2 characters also offers **Search people** under them,
  which asks the network for both the username and the name and shows one
  result list. A username returns at most one person; a name can return
  several.

**Matching a name** (decided 2026-09-28: whole-name matching was rejected as
poor UX). Letter case, accents and repeated spaces never matter.

- **Among people this device already knows** - contacts, and the members of
  groups and channels whose profiles it holds - matching is as loose as the
  platform's local search: any part of any word.
- **Through contacts of contacts and among people who chose Everyone**, the
  network can only match the **start of a word**, from 3 letters: `ann` and
  `web` find `Anna Weber`, `nna` does not. A word shorter than 3 letters is
  matched whole (`ли` finds `Ли Вэй`). Several words narrow the result: `anna
  web` finds `Anna Weber`, `anna x` does not. Typos are not forgiven.
- Whoever can find a person by name finds them by the name in their profile
  at the moment it was last published.

A result row: avatar, name, `@username`, and the action **Add contact**,
which opens the existing contact-request flow with its introduction. The row
never says *why* the person was found ("contact of Anna", "in Hiking club"):
saying so would tell the searcher who knows whom.

States:

| State | What it shows |
|---|---|
| Searching | The row shows a spinner in place of its chevron; the local results stay. |
| Found | The result list. |
| Not found | "No one found. Check the spelling, or ask them for an invite link." The same whether the person does not exist or does not let you find them - the searcher is never told which. |
| Network failure | "Couldn't search. Check your connection and try again." Never "not found": telling someone their friend does not exist because the connection dropped is the worst thing this can do. |
| Request refused | The person changed their setting after you found them: "Couldn't send the request. They may have changed who can find them." |

### 6.2 In a group or a channel

A member row shows the member's name as today. What changes is what you can
do from it:

- **Someone who lets you find them** (by any part of their setting): their
  row offers **Add contact**, and their `@username` is shown.
- **Someone who does not**: their name only - no `@username`, no Add
  contact, no Message. They are in the group and you read what they write
  there; that is all.
- **A contact**: as today, whatever their setting.

The username goes with the permission because a username is a handle for
finding someone; showing it to people who cannot use it teaches them to try.

### 6.3 Channels show their members

A channel lists its members to its members, the same as a group: a
`Members` row in channel info opening the member list. Without it, part 2.2
of the setting would have nothing to point at for channels. This is a change
to channels in its own right, with its own section in the ADR, and it ships
before or with this feature.

## 7. Signup

The last signup step gains one line under the username field's success
state:

> Contacts of your contacts and people in your groups and channels can find
> you. You can change this in Settings > Privacy.

With a `Change` link that opens the screen from section 3. No choice is
forced during signup.

## 8. What changing the setting does, and how fast

The screen must not claim more than has happened.

- **Narrowing takes effect at once.** Whoever loses the ability to find the
  person stops finding them on their next search, and a request they had
  not yet sent is refused (section 6.1).
- **Widening can take a while for contacts of contacts**, because it travels
  through the person's contacts' devices. Under **Contacts of my contacts**,
  after it is turned on, the helper text reads "Contacts of your contacts
  can find you once your contacts next open Lakko." until the client knows
  it has reached them, then goes back to its usual text. Groups, channels
  and Everyone widen at once.
- **Requests already received** stay: narrowing does not throw away a
  pending request the person can still accept or decline.
- **Offline**: the choice is kept and applied when the device reconnects;
  the screen shows "Will apply when you're back online." under the title
  until then. The radio position shows the person's choice, not a stale
  server value.
- **A failure to apply** (the server refused, the device lost its session):
  "Couldn't update who can find you. Try again." with a Retry action, and
  the rows show what is actually in effect.

## 9. States of the screen

| State | What it shows |
|---|---|
| Default | Section 3. |
| Applying | The chosen row is selected at once; a small spinner beside the title while it applies. |
| Offline | As section 8. |
| Failed | As section 8. |
| Contacts of my contacts spreading | As section 8. |
| No groups or channels | Members of my groups and channels is still offered (it covers future groups under All); Only the ones I choose opens the empty list of section 4. |

## 10. Data the UI needs (input to the ADR)

- The setting itself: the position, the two parts, the All/chosen form, the
  set of chosen groups and channels, **synced across the person's own
  devices** (changing it on the phone changes it on the laptop).
- The part-restoration memory of section 3 (last parts on), stored with it.
- Per member of a group or channel: can I find this person - which decides
  Add contact and the `@username` in section 6.2.
- Network search by username **and by word prefix of the name**, returning a
  list, with a not-found answer identical to a refusal.
- Refusal of a contact request from anyone the setting does not allow, and
  from anyone who does not hold a link from the person, **before it
  reaches them** - not a filter on their device that the sender can defeat.
- An apply status (applied / pending offline / failed) and, for contacts of
  contacts, whether it has spread.
- A channel member list visible to members (section 6.3).

## 11. Cross-platform shape

One screen, one model: a single choice of three, two switches under the
middle one, a single choice of two under the second switch, a list behind
it.

**Per platform**: web a Settings page with radio and checkbox rows; iOS a
`Form` with an inline `Picker` for the single choices and `Toggle`s for the
parts, the list pushed on the `NavigationStack`; Android radio `ListItem`s
and `Switch`es in a `LazyColumn`, the list pushed on the nav graph.

**The rule that turns taps into a setting is one copy, in Rust**
(`lakko_ui_rules`, [`navigation.md`](navigation.md) section 8): "turning off
the last part selects Nobody", "choosing Some people restores the last
parts", "turning a group off under All switches to chosen with the rest on".
Three hand-written copies of those would drift, and the drift would be a
person hidden from, or shown to, people they did not choose.

## 12. Reaching someone who is not a contact, in every form

The rule of section 1 is about reaching a person, not about one screen, so
it covers every way a non-contact could put something in front of them:

- **A contact request** - section 6.
- **Being added to a group** by someone who is not a contact: allowed only
  if that someone can find the person, or the person asked to join (a
  group link they opened). A contact can always add them.
- **A reply to the person's own request** - the admission to a group they
  asked to join, a contact accepting their request - always arrives: they
  asked for it.
- **The person's own devices** always reach each other.

Everything else addressed to them by a non-contact is refused before it
reaches them.

## 13. Out of scope

- **A separate "who can add me to groups" setting.** Section 12 applies this
  setting to it; a finer-grained setting would be its own design.
- **Who can see my name and photo.** Not changed; what a group member sees
  of you in the group is section 6.2.
