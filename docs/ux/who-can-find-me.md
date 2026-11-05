<!-- SPDX-License-Identifier: AGPL-3.0-or-later -->
<!-- SPDX-FileCopyrightText: 2026 Lakko Contributors -->

# Who can find me

A person decides who can find them by username or by name, and so who can
ask to become their contact. Everything below is a requirement for all three
clients. Where a platform genuinely has to differ, the difference is stated
inline and marked **Per platform**.

Architecture: [ADR-0060](../adr/0060-scoped-discovery.md).

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
     one of the person's contacts, in one of two forms, never both:
     - **2.1.1 All of them.** The contacts of every contact the person has.
     - **2.1.2 Only the ones I choose.** The contacts of the contacts the
       person has switched on, one by one.
   - **2.2 Members of my groups**, in one of two forms, never both:
     - **2.2.1 All of them.** Every member of every group the person is in.
     - **2.2.2 Only the ones I choose.** The members of the groups the person
       has switched on, one by one.
   - **2.3 Subscribers of my channels**, in the same two forms:
     - **2.3.1 All of them.** Every subscriber of every channel the person
       follows.
     - **2.3.2 Only the ones I choose.** The subscribers of the channels the
       person has switched on, one by one.

Choosing 1 turns 2 and 3 off, choosing 3 turns 1 and 2 off, and 2.1, 2.2 and 2.3
combine freely.

**Knowing someone is not permission to reach them.** Knowing a person's
username, having seen them in a group, or holding any identifier of theirs
does not let you send them a contact request or a message. Only the setting
(through search) or a link from them does. Existing contacts are unaffected:
a contact can always write.

**Default for a new account: Some people, with 2.1.1 and 2.2.1 on and 2.3 off**
(decided 2026-10-30). A group is mostly people who know each other; a channel can
be a million strangers, and being findable lets any of them send a request.

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
            (•) All my contacts
            ( ) Only the ones I choose          >
      [x] Members of my groups
            (•) All my groups
            ( ) Only the ones I choose          >
      [ ] Subscribers of my channels
            (•) All my channels
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
    person last had on; if they never had any, the default parts (2.1.1 and
    2.2.1, with 2.3 off).
- **All / Only the ones I choose** is a single choice, one under each part,
  shown only while that part is on.
- **Only the ones I choose** has a disclosure that opens its list (section 4
  for groups and channels, section 4.1 for contacts). Selecting it for the
  first time opens the list straight away.

The row descriptions, as muted helper text under each position:

| Position | Helper text |
|---|---|
| Nobody | Only people with your invite link or QR code can reach you. |
| Some people | (the parts speak for themselves) |
| Contacts of my contacts | People your contacts have added. / Only the contacts of the people you choose (N of M). |
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

### 4.1 The list of contacts

Opens from **Only the ones I choose** under Contacts of my contacts. One row
per contact, each row the avatar, the name and a switch, sorted as the Contacts
screen sorts them, with a search field on top when there are more than a screen
of them.

- **Empty**: "You have no contacts yet."
- A contact added later appears here switched **off**: under Only the ones I
  choose, each one is a decision the person makes. Under **All my contacts** a
  contact added later is included, which is what "all" means.
- A contact removed leaves the list; adding them back brings them in switched
  off.
- With nobody switched on, no one finds the person through their contacts. The
  helper says "0 of M"; the setting does not change by itself.
- The contacts themselves are told nothing, and nothing shows them whether they
  are switched on.

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

### 5.1 The switch on a contact

`Their contacts can find me`, on the contact's screen
([`conversation-info.md`](conversation-info.md) section 2.1), shown only while
Contacts of my contacts is on and only for a person who is a contact. When the
setting does not include contacts of contacts, the row is absent.

- Under **Only the ones I choose**, it is that contact's switch from the list in
  section 4.1, the same value in two places.
- Under **All my contacts**, it shows on. Turning it off switches the part to
  **Only the ones I choose** with every other contact on and this one off, and
  says so once, in place: "Now only the contacts you choose. Change in
  Privacy."

## 6. What other people see

### 6.1 Searching: one field

**Decided 2026-10-25 by the maintainer:** a search is one input field and
nothing else. No "Search people" row to press, no separate "Add by username"
form, no second field for a message. The field finds people by `@username`
and by name, among the person's own contacts and conversations, through
contacts of contacts, and among people who chose Everyone. Before this, the
network half sat behind a row the person had to press, and Contacts had an
"Add by username" form that asked only the Everyone directory, so a person
whose setting was **Contacts of my contacts** could not be found from the
place most people looked.

**Where.** The same field at the top of **Chats** and at the top of
**Contacts**, with the same behaviour. In Chats it also matches
conversations; in Contacts only people. **Per platform:** pinned above the
list on the web, `.searchable` on iOS, a search bar above the list on
Android.

**When it asks the network.** Nothing to press:

- Local results (contacts, members of the person's groups and channels,
  conversations in Chats) appear as the person types, from the first letter.
- From **3 characters**, after the person stops typing for **500 ms**, the
  field also asks the network for the username and the name at once, and
  shows the network results under the local ones under the heading
  **People**. Both numbers are chosen, not measured.
- A text asked in the last 60 seconds is not asked again; a result already
  shown stays while the person keeps typing, until the new answer arrives.
- A text that cannot be a username and has no word of 3 letters asks
  nothing (`lakko_ui_rules::lookup::looks_like_a_name` and the name-term
  rule); a leading `@` is ignored.

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

**A result row:** avatar, name, `@username`, and **Add contact**. Tapping
Add contact sends the contact request at once, with no introduction text;
the row then reads **Request sent** and the button goes away. A person who
is already a contact shows **Open** instead and opens the conversation. The
row never says *why* the person was found ("contact of Anna", "in Hiking
club"): saying so would tell the searcher who knows whom. A channel found by
its `@handle` shows as a channel row that opens its subscribe page.

States of the People part:

| State | What it shows |
|---|---|
| Fewer than 3 characters | Nothing under **People**; the heading is hidden. |
| Searching | The heading with a spinner after it; the local results and any earlier network results stay. |
| Found | The result rows. |
| Not found | "No one found. Check the spelling, or ask them for an invite link." The same whether the person does not exist or does not let you find them - the searcher is never told which. |
| Network failure | "Couldn't search. Check your connection and try again." Never "not found": telling someone their friend does not exist because the connection dropped is the worst thing this can do. Typing again retries. |
| Out of searches | "Too many searches for now. Try again later." Searching spends anonymous tokens and they ran out; the local results stay. |
| Request refused | On the row, after Add contact: "Couldn't send the request. They may have changed who can find them." |
| Request failed | On the row: "Couldn't send the request. Try again." with the button back. |

**A kept grant is checked before it is offered** (2026-10-30). A person this
device knows from a group it shared with them may have left that group, and
then the grant it kept no longer reaches them. Before a local row offers **Add
contact** or shows `@username`, the device asks the Delivery Service whether a
kept grant still reaches the person, trying every grant it holds for them in
turn, with no 3-character minimum and no anonymous token. Until the answer
arrives the row shows the name alone. If a grant passes, the row offers **Add
contact** with that grant; if none does, the row stays as a name with no button
and no username, the same as any member who cannot be reached. A connection
failure changes nothing: the row behaves as before and the send checks again.
**No one found** is shown only when no person stands under **People**.

**A row says what is true now** (2026-10-30). A person's reach changes while the
field is open, so nothing a row says may need an app restart to become correct:
- Every change of the text checks the kept grants again; the last answer stays
  on screen until the new one arrives.
- **Add contact** tries every grant the device holds for the person at that
  moment, not only the one the row passed with, and is refused only when none
  passes.
- A network answer kept for a text is reused for at most 60 seconds (chosen, not
  measured), a found answer and a not-found one alike.
- A row's refused or failed line lasts until the text changes.
- **Request sent** is read from the Sent list, and from the people added through
  their invite link who have not answered: declined, dismissed or accepted, the
  row goes back to what the check says.
- One person, one row (2026-10-31). A person the list above already shows, a
  contact or someone added through their link, is not repeated under **People**,
  in Contacts and in Chats. A person with a conversation who is no contact keeps
  their row under **People**, and with it the way to add them.
- A contact the list cannot match yet, because their name has not arrived, stands
  under **People** until it can, so the field is never blank.
- The name a person was found under is theirs until their own profile arrives, so
  a new contact is never listed as a stand-in.
- The line that nothing matches shows only when nothing stands in the field, no
  list row and no person under **People**.

**One list (decided 2026-11-04 by the maintainer).** Everything the field
finds, by `@name` or by name, stands in ONE list with no section headings:
first what this device already holds (conversations in Chats, contacts),
then what the network found (people, groups, channels), each row saying
what it is on its second line. There is no **People** heading and no
**Groups and channels** heading, so a search for a group never reads "No
one found" above the group it found. The state lines of the table above
apply to the whole list: the spinner sits at the end of the list while the
network is asked; "Nothing found. Check the spelling, or ask for an invite
link." replaces "No one found" and shows only when the list is empty; a
network failure or an empty token pool shows its line at the end of the
list, under whatever local rows stand. Where the text above says "under
**People**", read "in the list".

**What goes away:** the "Search people" row of the Chats search, the "Add by
username" form in Contacts (its username field, its message field and its
Send button), and the introduction step after Add contact. The Contacts
**Add** menu keeps **Share invite link** (and scanning a code where the
platform has it). An introduction text is no longer sent from search; the
contact request carries the sender's profile as before.

**Token budget.** One network search spends up to three anonymous
lookup tokens (username, name, channel handle), and a device keeps a pool of
them that a scheduled refill tops up (ADR-0078 section 8). With this change
the pool keeps 32 lookup tokens instead of 16 (32 is the most a pool holds,
since one refill is one issuance of at most 32) and the daily lookup quota is
300 instead of 100 (both chosen, not measured), so searching as one types
does not run dry after a handful of searches.

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

### 6.4 A request inside a group you share (decided 2026-11-05)

A contact can stand on this device without anything that reaches their
inbox: someone first met in a group, or a contact row older than the
grant it should carry. Message on their card must start a chat, and a chat
needs them to accept. **In that case the contact request goes inside a
group both people are in**, as a message for that one person
([ADR-0086](../adr/0086-circle-directories.md) section 6; built ahead of the
rest of that ADR). It is sealed to that person, so the other members cannot
tell whom it is for: each device tries it, and every device but the
person's drops it unread. Only a current member can write in a group, so
someone who left cannot be asked through it.

**Which way a request goes.** One order, everywhere a request is sent or a
one-to-one is started (Message on a contact card, Add to contacts on a
person's card, Add contact on a search row):

1. A grant this device holds still reaches the person: the request (or the
   chat) goes to their inbox, as today.
2. Otherwise, both people are current members of a group (not a
   one-to-one): the request goes inside that group. With several, the one
   with the newest message.
3. Otherwise: nothing is sent; the card shows the cannot-message line below.

Entry points as of 2026-11-05: **Message on a contact card** is the case
found live and the one this section is for. Add contact on a group member
row stays as section 6.2 says (shown only where the person lets group
members find you); it uses the same order, so a grant that stopped working
falls back to the group. No new button appears anywhere.

**What the sender sees.**

| State | What it shows |
|---|---|
| Sent | The card shows "Request sent. You can write once {name} accepts." under Message. The request stands in Contacts > Sent with **Waiting**, as any request. No chat is created. |
| Message pressed again | The same line; nothing is sent again. One request per person until it is answered or removed from Sent. |
| Accepted | The chat opens as for any accepted request; the line goes away. |
| Declined, ignored, or refused by the person's setting | Nothing changes: the request stays **Waiting** until the sender removes it from Sent. The sender is never told which, as with search (section 6.1). |
| Cannot be sent (no grant, no shared group, or the sender left the group) | "You can't message {name} yet. Ask them for an invite link." under Message. |
| Sending failed (network) | "Couldn't send the request. Try again." with Message available again. |

**What the person asked sees.** An ordinary request in Contacts >
Requests, with Accept and Decline, and one more line naming where it came
from: "Via {group}" (the group's name, or "Via a group you're both in" when
it has none). Accept does what accepting any request does: the chat is
started through the inbox key the request carries, so the reply does not go
through the group. Decline and ignore are as today. A request through a
group from someone who is already a contact, while this device holds no
one-to-one with them, is shown the same way and its Accept starts the
chat; it is never answered silently, as a mailbox request from a contact
is, which would leave both people without a chat. An opened request is
stored on the device before the group is read past it, so closing the app
in between does not lose it.

**When the person does not want it.** A request through a group is shown
only when the person's own setting lets the members of that group find
them (section 1, part 2.2, or Everyone), or when the sender is already
their contact. Otherwise their device drops it without a trace, and the
sender sees **Waiting** as above. This is a filter on the person's own
device, not a refusal before it reaches them (section 10 is amended
accordingly): the sender is already able to write to them in that group,
so nothing new is reachable.

**What the other members can tell** (decided 2026-11-05: they must not learn
whom it is for). Their devices see that one member posted a request of some
size, cannot tell to whom, and drop it; nothing is shown to them.

**Checks the person's device makes** before showing it: the request opens
with this device's key (a device it does not open for drops it and logs
nothing that names anyone); the member who sent it in the group is the
person the request says it is from; the request's signature holds and it is
not stale (the ordinary request checks). Any failure drops it.

Catalog keys (proposed; to be added with the client work):

| Key | Text |
|---|---|
| `contacts.detail.request_sent` | Request sent. You can write once {name} accepts. |
| `contacts.detail.cannot_message` | You can't message {name} yet. Ask them for an invite link. |
| `contacts.detail.request_failed` | Couldn't send the request. Try again. |
| `contacts.request.via_group` | Via {group} |
| `contacts.request.via_unnamed_group` | Via a group you're both in |

**Per platform:** the line sits under the Message button on the web card,
in the iOS `ContactDetailScreen` header section, and under the Android
`ContactDetailScreen` actions; the Requests row's second line carries the
via line on all three.

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
  not yet sent is refused (section 6.1). This holds for switching a contact
  off or moving to Only the ones I choose: the people who found the person
  only through that contact stop finding them, and the people reachable
  through a contact still switched on find them again once that contact's
  device has the new record.
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

**Under the mechanism of [ADR-0086](../adr/0086-circle-directories.md)** (proposed,
not built): a contact switched off, and a person removed from a contact list, stop
working at once, and no other device has to do anything. A new device finds people
through contacts more completely over its first sessions, starting with the contacts
it uses most; nothing on screen speaks of it, and a person not found yet reads like
anyone not found.

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

- The setting itself: the position, the two parts, the All/chosen form of
  each part, the set of chosen groups and channels and the set of chosen
  contacts, **synced across the person's own devices** (changing it on the
  phone changes it on the laptop).
- The part-restoration memory of section 3 (last parts on), stored with it.
- Per member of a group or channel: can I find this person - which decides
  Add contact and the `@username` in section 6.2.
- Network search by username **and by word prefix of the name**, returning a
  list, with a not-found answer identical to a refusal.
- Refusal of a contact request from anyone the setting does not allow, and
  from anyone who does not hold a link from the person, **before it
  reaches them** - not a filter on their device that the sender can defeat.
  **Amended 2026-11-05 for requests inside a group (section 6.4):** a
  request sent inside a group both people are in is filtered by the
  person's own device against their setting, since the sender can already
  write to them there.
- For section 6.4: per group, whether the other person is a current
  member (from the group's roster), and per request, the group it came
  through.
- An apply status (applied / pending offline / failed) and, for contacts of
  contacts, whether it has spread.
- A channel member list visible to members (section 6.3).

## 11. Cross-platform shape

One screen, one model: a single choice of three, two switches under the
middle one, a single choice of two under each switch, a list behind each.

**Per platform**: web a Settings page with radio and checkbox rows; iOS a
`Form` with an inline `Picker` for the single choices and `Toggle`s for the
parts, the list pushed on the `NavigationStack`; Android radio `ListItem`s
and `Switch`es in a `LazyColumn`, the list pushed on the nav graph.

**The rule that turns taps into a setting is one copy, in Rust**
(`lakko_ui_rules`, [`navigation.md`](navigation.md) section 8): "turning off
the last part selects Nobody", "choosing Some people restores the last
parts", "turning a group or a contact off under All switches that part to
chosen with the rest on".
Three hand-written copies of those would drift, and the drift would be a
person hidden from, or shown to, people they did not choose.

## 12. Reaching someone who is not a contact, in every form

The rule of section 1 is about reaching a person, not about one screen, so
it covers every way a non-contact could put something in front of them:

- **A contact request** - section 6, including one sent inside a group both
  are in (section 6.4).
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
