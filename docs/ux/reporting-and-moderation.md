<!-- SPDX-License-Identifier: AGPL-3.0-or-later -->
<!-- SPDX-FileCopyrightText: 2026 Lakko Contributors -->

# Blocking, reporting and moderation

This spec covers four things a person can meet:

- blocking someone;
- reporting someone or something to Lakko's moderators;
- seeing that an account was suspended, from either side;
- the moderators' own panel.

The ADR comes after this spec and has to provide every state it lists
(section 10). Draft of 2026-11-08.

## 0. What the maintainer decided (2026-11-08)

1. A report is **anonymous**. Moderators see what the report contains, but it
   carries nothing that names the person who sent it.
2. Clients **honour a suspension**. They show the account as suspended, and a
   suspended account cannot be added as a new contact.
3. Moderator actions are signed by moderator keys. An offline root key signs
   the list of those keys. Every action is published in a **public moderation
   log**, and a client shows a suspension only if the log contains it.
4. The terms of use and the legal side (the EU Digital Services Act and
   others) are **deferred**. This spec leaves room for them (section 9) and
   does not settle them.
5. In a group, a blocked person's messages are **collapsed** to one muted
   row, not hidden (section 1.2).

Limits that come from end-to-end encryption, stated once because the copy
has to stay true:

- Moderators read only what a reporter chooses to include. They cannot open
  a conversation on their own.
- Suspending an account stops it from signing in and from reaching anyone
  new. It does not remove the account from the conversations it is already
  in; only the people in those conversations can do that, by blocking,
  leaving or removing it.

## 1. Block

### 1.1 Where it is

- The person's card (`conversation-info.md` section 2.1): a `Block` row in
  the danger block, above `Remove contact`. On a blocked person's card the
  row reads `Unblock`.
- A contact request row in Contacts > Requests: `Block` next to Accept and
  Decline, behind the row's overflow menu on a phone.
- The report sheet: `Also block {name}` (section 2.3).
- Settings > Privacy > **Blocked people**: the list, with `Unblock` on each
  row. Empty state: "You haven't blocked anyone."

There is no Block on a group or a channel. To stop hearing from one, a
person leaves it or mutes it.

### 1.2 What blocking does

Confirmation dialog, danger action:

> **Block {name}?**
> They won't be able to message or call you, or send you a contact request.
> They aren't told.
> [Cancel] [Block]

The block covers all of the account's devices. It is an account-level
setting carried between the person's own devices, like folders. A device
that learns of the block applies it within its next sync.

**In the one-to-one with that person:**

- New messages are not shown and do not notify. A banner replaces the
  composer: "You blocked {name}." with `Unblock`.
- Earlier messages stay where they were.
- Their calls do not ring. They are declined without a ring, and no
  missed-call row appears.
- Typing indicators, read receipts and profile updates stop going to them.
- They are taken off the contact list. Their way to write to this account's
  inbox stops working, as with `Remove contact`.

**Contact requests:** a request from a blocked account is dropped unseen,
whichever way it arrives (inbox, invite link, through a group).

**In a group both are in:** a blocked person's messages collapse into one
muted row, "Message from someone you blocked", which shows the message when
tapped. They still count as a member. Blocking does not remove them from the
group; an admin can.

**In a call both are in:** they are still in the call. Their tile shows a
muted "Blocked" label.

**What the blocked person sees:** nothing new. Their messages stay at Sent
and never reach Read. Their calls go unanswered. Their requests stay at
Waiting. None of this is distinguishable from a person who is away.

### 1.3 Unblock

Unblocking makes the person reachable again: their messages, calls and
requests are shown from that moment on. Nothing they sent during the block
is shown afterwards. It does not make them a contact again. Their card
offers `Add to contacts` (`conversation-info.md` section 2.1).

### 1.4 States

| State | What shows |
|---|---|
| Blocking in progress | The Block button shows progress. The block takes effect on this device at once, and on the account's other devices when they sync. |
| Block saved, sync failed | Nothing visible. The block holds on this device and syncs later. |
| Blocked, card opened | `Unblock`, plus a muted line under the name: "Blocked". |

## 2. Report

### 2.1 Where it is

| Entry point | Target | What is included |
|---|---|---|
| Message menu, on someone else's message, destructive group | The message's author | That message (see 2.2 for adding more) |
| Person's card, danger block | That person | Messages the reporter picks, optional |
| Contact request row, overflow | The requester | The request itself, including its intro text |
| Group info, danger block | The group | Messages the reporter picks, optional, plus the group's name and `@name` |
| Channel info, danger block | The channel | Posts the reporter picks, optional, plus the channel's name and `@name` |
| Post menu in a channel | The channel | That post |
| Discover card overflow | The channel | Its listing (name, description, `@name`) |

Report is never offered on your own messages, on your own account, or in a
conversation you are not part of. A group is reported only when it has an
`@name`, and never by its owner: nothing else names a group to the
moderators, so in a group without one a person reports the members'
messages from the message menu (ADR-0099 open question 3). A channel is
reported by its subscribers, not by its owner or publishers.

### 2.2 The sheet

One sheet, three parts, on every client.

**1. Why.** One choice, required:

- Spam or scam
- Harassment or threats
- Hate
- Violence or graphic content
- Sexual content
- A child's safety
- Pretending to be someone else
- Something else

The list order and the codes are fixed in the core (section 10) and shared
with the panel and the log.

**2. What's included.** One line, "Included: 1 message", with an `Add
messages` link. The link opens a "Choose messages" list inside the sheet,
newest first, limited to the messages that may be picked (below), with
`Done`; up to 20 can be picked (the number is chosen, not measured). Attachments of the picked messages are
always included: a message is checked as a whole, and its files belong to
it.

What can be picked depends on the target:

- **A person or a message:** only that person's own messages. Nothing anyone
  else wrote, the reporter included, goes into the report.
- **A group or a channel:** any message or post in it, except the
  reporter's own. Group messages picked this way show their authors to the
  moderators.

A report with nothing included is allowed (a person's card, a group, a
channel). Moderators then have only the reason and the comment, and the
sheet says so: "Reports with messages included are reviewed first."

**3. Anything to add.** An optional text field, up to 500 characters (the
limit is chosen).

Then:

- `Also block {name}`: a checkbox, on by default, shown when the target is a
  person.
- `Leave the group` / `Unsubscribe`: a checkbox, off by default, shown when
  the target is a group or a channel.
- A line above the button: "Lakko's moderators will see what's included.
  {name} isn't told who reported them." For a group or a channel the second
  sentence is left out.
- The button: `Send report` (primary).

### 2.3 Sending and after

| State | What shows |
|---|---|
| Sending | The button shows progress and accepts no second tap. |
| Sent | The sheet closes. A short notice reads "Report sent. Moderators will review it." The block or leave, if checked, is applied at the same time. |
| No network | "Couldn't send the report. Try again." The sheet stays filled in. |
| Daily limit reached | "You've sent many reports today. Try again tomorrow." The sheet stays filled in. The block still applies if checked. |
| Reporting not available on this server | "Reporting isn't available on this server." The block still applies if checked. |
| Evidence can't be prepared (a message is no longer on this device) | That message is left out and the line reads "Included: 2 of 3 messages". |

**Your reports.** Settings > Privacy > **Your reports** lists the reports
sent from this device. Each row shows the target name as the reporter saw
it, the reason, the date, and a status: `Received`, `Action taken` or `No
action`. The status is fetched with the report's receipt and nothing else.
The list lives on this device only and is not synced, because syncing it
would tie the reports together. Empty state: "You haven't sent any
reports."

The reporter is never told what action was taken against whom. A status
says only whether the report led to one.

## 3. A suspended account, seen by others

A client shows a suspension only after checking the suspension entry in the
moderation log. A suspension the client cannot confirm in the log is not
shown.

| Where | What shows |
|---|---|
| The one-to-one | A banner at the top: "{name}'s account is suspended for breaking Lakko's rules." The conversation still works. The banner has no action; Block is on the card. |
| Their card | A muted line under the name: "Account suspended". |
| A group's member list | "Suspended" after the name, in muted text. An admin also sees `Remove from group` on that row. |
| Search, an invite link, a contact request from them | Search does not list them. An invite link from them opens to "This account is suspended. You can't add it." A request from them is dropped unseen. |
| A channel that was removed | Its info shows "This channel was removed for breaking Lakko's rules." Posts already on the device stay readable. Discover no longer lists it. |
| A `@name` that was taken back | The name finds nothing. A conversation that held it shows no `@name`. |

When a suspension ends (it expires, or is lifted on appeal), every mark goes
away on the next check, and nothing about it is kept.

## 4. A suspended account, seen by its owner

At the next sign-in or the next session refresh, the app opens to one screen:

> **Your account is suspended**
> Reason: {reason}. {Until {date}. | This suspension has no end date.}
> You can read your chats but you can't send messages, add contacts or make
> calls.
> [Appeal] [Read my chats] [Sign out]

- `Read my chats` opens the app read-only. Every composer reads "Your
  account is suspended" and has no send control. Calls and adding contacts
  are not offered. Settings is open, except for actions that need the
  server.
- `Appeal` opens a sheet with one text field (up to 1,000 characters) and
  `Send appeal`. It can be sent once per suspension. After it is sent, the
  screen reads "Appeal sent. You'll see the decision here." Later it reads
  "Your appeal was declined." or the suspension simply ends.
- `Delete account` stays available under Settings > Account. A suspended
  person can always leave. On a device that holds no chats for the account
  (signed in afresh, no keys yet), there is nothing to read and Settings
  cannot be reached: `Read my chats` is replaced by `Delete account`, which
  opens the usual confirmation (decided 2026-11-20 while building).
- While suspended, files in chats still open: the restricted session may draw
  the file-read passes, and nothing else.
- When a timed suspension ends, the screen goes away at the next refresh
  without any notice.

This is the one screen that says *why*. Others see only that the account is
suspended, never the reason (section 3).

## 5. The moderators' panel

A separate web app for moderators, not linked from the product and not
served on a public hostname. It uses the design system's tokens and
components, but its own navigation.

### 5.1 Signing in and confirming

A moderator signs in with their security key. Every decision (section 5.3)
asks for one more touch of the key, and the panel says what it is about to
sign: "Suspend this account for 7 days for spam. This will be published in
the moderation log." A decision is never one click.

### 5.2 The queue

Reports are grouped into **cases**, one per target (an account, a group, a
channel, an `@name`). Each row shows:

- the target. The server knows a username only as its lookup key and a
  contact request's profile is sealed to its recipient, so the panel cannot
  show the `@username` itself; the case header says "Has a username" and
  whether the account is in Everyone (changed 2026-11-20 while building);
- the reasons with counts ("Spam 14 · Harassment 2");
- how many reports include messages that were checked as written by the
  target, and how many include none;
- first and last report (day only);
- the status: New, In review, Decided.

Filters: reason, status, target kind, "has checked messages". Default order:
cases with checked messages first, then by the number of reports.

A case one moderator has opened shows "In review by {moderator}" to the
others.

### 5.3 A case

**Header:**

- the target;
- how old the account is (a band: "under a day", "under a week", "under a
  month", "older");
- whether it is suspended now;
- earlier decisions from the log.

**Each report:**

- the reason, the comment, the date;
- the conversation kind it came from: one-to-one, group, channel or contact
  request;
- the included messages, in order. Each message carries a mark:
  - `Written by this account` when its signature checks out;
  - `Not verified` otherwise. Not-verified messages are shown greyed and
    count for nothing in the default order.

Photos and videos are blurred until clicked. Files open only on an explicit
click.

**Decisions**, each with a required reason code (the section 2.2 list) and
an optional internal note:

- `No action`;
- `Suspend` for 1 day, 7 days, 30 days or with no end date;
- `Take back @username`;
- for a channel: `Remove from Discover`, `Block the @name`, `Remove posts`;
- for a group: `Remove from search`, `Block the @name`.

The internal note is never published. The log carries the action, the
target, the reason code, the time and the moderator's key.

Deciding closes every open report on the target. The reporters' statuses
change to `Action taken` or `No action`.

### 5.4 Appeals

A queue of appeals. Each one shows:

- the appeal text;
- the decision it appeals;
- the case behind that decision.

The moderator chooses `Keep the suspension` or `Lift the suspension`. A lift
is a new log entry; nothing is deleted. A moderator should not decide an
appeal against their own decision. The panel warns about it and does not
block it.

### 5.5 The log and the moderators

- **Log**: the public moderation log as entries, newest first, with search
  by target. Read-only.
- **Moderators**: the current list of moderator keys, with names and
  when each was added. Read-only. The list is changed with the offline tool;
  the panel says so.
- **Numbers**: reports per reason per week, decisions per kind per week,
  appeals and their outcomes. These are counts only, for a transparency
  report.

## 6. Group admins

Not in the first build. Later, a report made from inside a group could also
go to that group's admins: an option in the sheet, "Also tell the group's
admins". It would be sealed to them and shown in the group's info under
`Reports`.

## 7. Copy

Proposed catalog keys, to be added with the client work:

| Key | Text |
|---|---|
| `block.action` | Block |
| `block.unblock` | Unblock |
| `block.confirm.title` | Block {name}? |
| `block.confirm.body` | They won't be able to message or call you, or send you a contact request. They aren't told. |
| `block.banner` | You blocked {name}. |
| `block.group_row` | Message from someone you blocked |
| `block.call_label` | Blocked |
| `block.card_line` | Blocked |
| `block.list.title` | Blocked people |
| `block.list.empty` | You haven't blocked anyone. |
| `report.action` | Report |
| `report.reason.spam` | Spam or scam |
| `report.reason.harassment` | Harassment or threats |
| `report.reason.hate` | Hate |
| `report.reason.violence` | Violence or graphic content |
| `report.reason.sexual` | Sexual content |
| `report.reason.child_safety` | A child's safety |
| `report.reason.impersonation` | Pretending to be someone else |
| `report.reason.other` | Something else |
| `report.included` | {n, plural, one {Included: # message} other {Included: # messages}} |
| `report.included_partial` | Included: {n} of {total} messages |
| `report.add_messages` | Add messages |
| `report.empty_hint` | Reports with messages included are reviewed first. |
| `report.comment` | Anything to add (optional) |
| `report.also_block` | Also block {name} |
| `report.also_leave` | Leave the group |
| `report.also_unsubscribe` | Unsubscribe |
| `report.who_sees` | Lakko's moderators will see what's included. |
| `report.not_told` | {name} isn't told who reported them. |
| `report.send` | Send report |
| `report.sent` | Report sent. Moderators will review it. |
| `report.failed` | Couldn't send the report. Try again. |
| `report.limited` | You've sent many reports today. Try again tomorrow. |
| `report.unavailable` | Reporting isn't available on this server. |
| `report.list.title` | Your reports |
| `report.list.empty` | You haven't sent any reports. |
| `report.status.received` | Received |
| `report.status.action` | Action taken |
| `report.status.none` | No action |
| `suspended.banner` | {name}'s account is suspended for breaking Lakko's rules. |
| `suspended.card_line` | Account suspended |
| `suspended.member` | Suspended |
| `suspended.cannot_add` | This account is suspended. You can't add it. |
| `suspended.channel_removed` | This channel was removed for breaking Lakko's rules. |
| `suspended.self.title` | Your account is suspended |
| `suspended.self.reason` | Reason: {reason}. |
| `suspended.self.until` | Until {date}. |
| `suspended.self.no_end` | This suspension has no end date. |
| `suspended.self.body` | You can read your chats but you can't send messages, add contacts or make calls. |
| `suspended.self.read` | Read my chats |
| `suspended.self.composer` | Your account is suspended |
| `suspended.appeal` | Appeal |
| `suspended.appeal.send` | Send appeal |
| `suspended.appeal.sent` | Appeal sent. You'll see the decision here. |
| `suspended.appeal.declined` | Your appeal was declined. |

The panel's strings are not in the product catalog. The panel is English
only.

## 8. Cross-platform shape

- Web, desktop, iOS and Android all implement sections 1 to 4, with the same
  entry points, the same sheet, the same states and the same copy.
- The report sheet is a `LakkoMenu`-style bottom sheet on a phone and a
  dialog-sized card on wide web.
- The reason list is a list of radio rows, not a `LakkoPicker`, because the
  choice is the point of the screen.
- `Add messages` is a list inside the sheet on every client, not the
  timeline's select mode (changed 2026-11-08 while building: the sheet stays
  open and filled in while the person picks).
- One shared rule decides where Block and Report appear in each info block:
  `lakko_ui_rules::info` gains the `Block` and `Report` sections. No client
  decides this itself.
- The panel (section 5) is web only.

## 9. Left for the legal phase

Recorded so the build leaves room for them, not decided here:

- the terms of use and their link at sign-up;
- what the law requires for each reason, above all child safety, where the
  law may require reporting to authorities and keeping evidence;
- deadlines for a decision;
- a written statement of reasons to the suspended person;
- an out-of-court dispute path;
- a published transparency report (section 5.5 provides its numbers).

## 10. Data the UI needs (input to the ADR)

- **Reason codes:** one closed list (section 2.2) with frozen values, shared
  by the client sheet, the report format, the panel, the log and the
  suspended person's screen.
- **A block register** carried between the account's own devices, keyed on
  the blocked account's key. Its fold rule is final until unblock. Every
  receive path (one-to-one, contact requests on every route, group
  timeline, calls, push wakes, the background notification paths) consults
  it.
- **The report:**
  - target kind (account, group, channel, `@name`), and the target;
  - reason;
  - comment;
  - the included messages, each with what lets a moderator check it was
    written by the target: the signed content and the binding of the
    signing device to the target account;
  - attachment keys when included;
  - conversation kind;
  - the size of the request: a report can carry up to 20 messages and their
    attachments.

  It is sent with no session. It is limited per account per day without
  naming the account, and the client can tell the daily-limit refusal and
  the not-available refusal apart.
- **A report receipt** that lets the reporter's device fetch the report's
  status (`Received` / `Action taken` / `No action`) without a session and
  without linking its reports to each other.
- **A suspension mark** the client can get for any account it shows (a
  contact, a group member, a search result, an invite link's issuer) and
  check against the moderation log. The ADR has to say how a client learns
  of a new suspension without a lookup per contact per day; that fails the
  scale requirement. The client needs whether the account is suspended, not
  the reason.
- **The owner's suspension:** a refusal from auth that the client can tell
  apart from every other sign-in failure, carrying the reason code, the end
  time or no end, and the appeal state (none, sent, declined). A suspended
  session must still be accepted for: sending an appeal, deleting the
  account, signing out, and fetching that state.
- **Channel and group removal** and **`@name` take-back** marks, readable by
  the clients and checked against the log the same way.
- **For the panel:**
  - cases grouped by target;
  - the account age band;
  - the account's earlier decisions;
  - who has a case open;
  - a signed decision per action;
  - the appeals queue;
  - counts for the numbers page.
