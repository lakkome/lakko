<!-- SPDX-License-Identifier: AGPL-3.0-or-later -->
<!-- SPDX-FileCopyrightText: 2026 Lakko Contributors -->

# Connection state: "Waiting for network..."

Date: 2026-11-24. Scope: web (and the desktop app, which runs the web code),
iOS, Android. Decision: the maintainer, 2026-11-24. Linked from
[`navigation.md`](navigation.md) section 2.3.

## 0. Why

Seen live on 2026-11-24: the iOS simulator lost its network for about half
an hour. Its open one-to-one looked fully live - composer, header, nothing
unusual - while no message could arrive. No client said anything until the
person tried to send. Single actions already have offline copy
(`common.error.offline`), and calls have their own `Connecting...` /
`Reconnecting...`, but the conversation list and an open conversation had
no way to say "this device cannot reach Lakko right now".

## 1. What the person sees

One state for the whole device, shown in the header, as Telegram does:

| state | Chats list title | conversation / channel subtitle |
|---|---|---|
| **Online** (normal) | `Chats` | what it says today: `4 members`, `@username`, `Public channel`, `typing...` |
| **Waiting** | `Waiting for network...` | `Waiting for network...` |
| **Connecting** | `Connecting...` | `Connecting...` |

- **Where it shows:** the title of the Chats list, and the line under the
  name in an open conversation (one-to-one, group) and an open channel. It
  replaces that line, including a live `typing...` (a typing indicator from
  before the network went is stale). The conversation's name stays.
- **Where it does not show:** call screens and the call bar (they have their
  own `Connecting...` / `Reconnecting...`), Feed, Contacts, Calls, Settings,
  sign-in. No banner, no toast, no dialog, nothing over the timeline.
- **The composer stays.** The person can type and send.

### 1.1 What happens to what the person sends

The composer stays and the person can send. A message that could not leave
because of the network waits and goes by itself when the connection is back:
section 7.

## 2. The one signal

The state is decided by this device's requests to the Lakko servers, and by
nothing else that could disagree between clients.

- **A network failure** is a request that got no HTTP answer: no route, DNS
  failure, connection refused or reset, TLS failure, a timeout, a dropped
  connection mid-response, the anti-censorship tunnel failing to connect. On
  the desktop app the host answers such a request itself; it marks that
  answer (`x-lakko-unreachable: 1`) and the page counts it as a network
  failure.
- **Any HTTP answer means the network works**, whatever its status: `401`,
  any `4xx`, `5xx`, including `502` / `503` from the server's own front.
  Those are the server's answers, and the existing per-action copy handles
  them.
- **A request this device cancelled** (a long poll aborted on leaving a
  screen, a request superseded by another) is neither; it changes nothing.
- **The operating system** may say there is no route at all (web
  `navigator.onLine` / the `offline` event, iOS `NWPathMonitor`, Android
  `ConnectivityManager`), and that a route came back. It is an early hint,
  never the last word: a request that gets an HTTP answer always returns the
  state to Online, whatever the OS says. (The iOS simulator, for one, keeps
  reporting a route when the host network is gone; the requests are what
  notice.)

## 3. Transitions

Starting state: Online, or Waiting if the OS already reports no route when
the app starts. Nothing is shown before there is evidence: a cold start does
not show `Connecting...`.

| from | event | to |
|---|---|---|
| any | a request fails with a network failure | Waiting |
| any | the OS reports no route | Waiting |
| Waiting | the OS reports a route again | Connecting |
| any | a request gets an HTTP answer | Online |
| any | a request is cancelled | unchanged |

Thresholds, chosen and not measured:

- **Enter on the first failure.** One request that failed with a network
  failure is enough. A request still in flight never enters the state,
  however long it takes (a long poll parks for 25 seconds on purpose).
- **Connecting lasts until the first answer.** When the OS says the route is
  back, every receive loop of the client retries at once instead of finishing
  its backoff (the open conversation's and channel's loops and the app-wide
  inbox and background loops alike), so `Connecting...` lasts until the
  first real answer, not until the next backoff tick. If that retry fails, the state goes
  back to Waiting.
- **Where the OS cannot tell** (the request failures are the only signal), the
  header goes from `Waiting for network...` straight back to normal at the
  first answer, with no `Connecting...` in between.

## 4. Copy

| key | English | where |
|---|---|---|
| `connection.waiting` | `Waiting for network...` | Chats title, conversation and channel subtitle |
| `connection.connecting` | `Connecting...` | same |
| `connection.restored_a11y` | `Connected.` | spoken only, when the state returns to Online |
| `chat.message.waiting_network` | `Waiting for network` | under a waiting message, with a clock (section 7) |

Translations are machine translations marked `machine` until reviewed. None
is security-critical.

## 5. Accessibility

- The change is announced **once per transition**, politely, never on every
  poll and never while the state stays the same: entering Waiting speaks
  `Waiting for network...`, entering Connecting speaks `Connecting...`, and
  returning to Online speaks `Connected.`.
- **Per platform.** Web: one visually hidden `aria-live="polite"` region in
  the app shell holds the last announcement. iOS: an accessibility
  announcement posted on the transition. Android: a polite live region on the
  Chats title (it changes only on a transition, so TalkBack speaks it once).
- The state is text, not colour.

## 6. One model, three implementations

Each client has one connection-state source, fed by its own request layer:

- **Web and desktop:** `apps/web/src/net/connection-state.ts`, fed by the DS
  client (every journal, mailbox and statement request), `sessionFetch`
  (every bearer request), and `navigator.onLine` with the `online` /
  `offline` events.
- **iOS:** `ConnectionState` fed by `AntiCensorshipTransport.performOnWire`
  (every REST request of the app, direct or tunnelled) and `NWPathMonitor`.
- **Android:** `ConnectionState` fed by `AntiCensorshipEngine.execute` (every
  Ktor request of the app, direct or tunnelled) and `ConnectivityManager`.

The transition table of section 3 is written three times, with the same
tests on each client (enter on a network failure and not on an HTTP error,
Connecting until an answer, an answer clears). It is small; if it ever grows
a rule beyond this table it belongs in `lakko-ui-rules`.

## 7. Sending while the network is gone (added 2026-11-24)

Decision (maintainer, 2026-11-24): the composer stays, and what the person
sends goes when the network returns.

### 7.1 Two kinds of failed send

- **Waiting.** The send of a message (text, reply, forward, a content
  message such as a poll or a location, or a photo, video, file, voice
  message, video note or GIF, upload included) failed because a request got
  no answer: a network failure in the sense of section 2. The message is not
  failed; it waits. Its bubble shows a clock and `Waiting for network` where
  the time goes (`chat.message.waiting_network`); a media bubble shows the
  same words where it would show `Not sent · Retry`.
- **Not sent.** It failed for any other reason: the server answered and
  refused (any HTTP status), a membership refusal, the account is suspended,
  the file is too large, the message could not be composed, the conversation
  is behind (ADR-0091). The bubble shows `Not sent` / `Not sent · Retry` as
  before, and nothing sends it by itself.

### 7.2 When and how waiting messages go

- **Trigger.** The connection state entering Online from Waiting or
  Connecting (the first answer from the servers). Not the OS signal alone:
  a send is only tried once the servers are known to answer, so an OS that
  reports a route while the servers are still out of reach spends no attempt.
- **Order.** Per conversation, oldest first, one at a time. A later message
  is never sent before an earlier waiting one of the same conversation. A
  pass stops at the first message that fails again for a network reason; the
  ones after it keep waiting and spend no attempt. Conversations are
  independent of each other.
- **New messages queue behind.** A message sent in a conversation that has
  waiting messages, or while its pass is running, is not published ahead of
  them: it joins the end of the queue (shown waiting) and goes in turn.
- **Bound.** Each message gets at most **5** automatic sends (chosen, not
  measured). After the fifth network failure it shows `Not sent · Retry`, and
  the person's tap is the only way it goes again.
- **A tap** on a waiting message starts its conversation's pass now (the
  earlier waiting messages go first).
- **Never twice from this device.** One pass per conversation at a time; a
  message being sent is not picked again by a pass or a tap; a row is
  identified by the client's own local id for the row (web `tempId`, iOS and
  Android `localId`). What this cannot rule out is unchanged from the manual
  Retry: a publish that reached the server and lost its answer is delivered
  twice, because the send is not idempotent and nothing on the server can
  de-duplicate it.

### 7.3 Where unsent messages are kept: the outbox

Decision (maintainer, 2026-11-24, round 3): what was sent goes out when the
network returns, also after leaving the conversation, reloading the web page
or quitting and restarting the app.

- **What is kept.** Every unsent message of a conversation, waiting or
  `Not sent`, is an entry of the account's **outbox** on this device:
  conversation, the client's local id (web `tempId`, iOS and Android
  `localId`), the time it was written (the order), its state (waiting or not
  sent), its automatic-send count, and what is needed to send it again with
  nothing else: the text, the reply and forward snapshots, the content fields
  of a content message (poll, checklist, location, contact card, article),
  and for a photo, video, file, voice message, video note or GIF either the
  attachment references of an upload that finished or, before that, the
  prepared file the upload sends (the bytes after any resizing or
  re-encoding, with its name, type and media metadata).
- **Where.** Per account, on this device only, under the device key and
  behind the ADR-0077 store gate: web and the desktop app in IndexedDB
  through the device cipher (the message fields in the message log, the
  prepared file bytes in their own store, each entry a separate record);
  iOS and Android in the store the message log uses (the prepared file a
  file of its own under the device key in the app's private, backup-excluded
  storage). Never sent to a server, never in the backup, the conversation
  vault or the account's DeviceGroup.
- **Written** before the first send attempt, and again on every change of
  state or count. **Deleted** when the message is published (and, for media,
  its prepared file when its upload finished and its references are kept),
  or when the person deletes the unsent row. A failed write of the outbox
  never stops the send; the message is then kept only while the app runs, as
  before this section.
- **Read back** at the session landing (app start, page load, sign-in) and
  shown in its conversation: unsent rows sit at the end of the timeline in the
  order they were written, as an unsent row always has (it has no message id
  yet), with the state they had: waiting (`Waiting for network`) or
  `Not sent · Retry`.
- **Who sends.** The app, not the conversation screen: at the session landing
  (once the servers answer) and at every return to Online, every
  conversation with waiting entries gets its pass (section 7.2: oldest first,
  stop at the first network failure, at most 5 automatic sends, one pass per
  conversation, a row being sent is never picked again). An open
  conversation only shows the entries and starts a pass on a tap.
- **Media and text share one order.** The upload is part of a media
  message's send: a voice message, video note or GIF, and a photo, video or
  file from the composer tray whose upload had not finished when Send was
  pressed (`docs/ux/media-messages.md` section 11). A message sent after a
  waiting media message waits behind it, as behind waiting text, on every
  client.
- **A conversation that cannot take it.** An entry whose conversation ended
  for this device (removed, left, deleted) or is behind (ADR-0091) becomes
  `Not sent` when its pass reaches it, as a send there fails today; an entry
  of a conversation this device no longer holds at all is dropped.
- **Wipe.** Forget device, the wipe of a signed-out or reset device and an
  account deletion clear the outbox with the rest of the account's local
  data.
- **What is unchanged.** Two sends started back to back with no message
  waiting still go out side by side: if the first fails for the network and
  the second gets through, the second lands first. Ordering is guaranteed
  from the moment a message waits. A publish that reached the server and lost
  its answer is still delivered twice (section 7.2). Channel posts are not
  covered: a post that fails keeps `Not sent · Retry`.

### 7.4 Constants

| constant | value | where |
|---|---|---|
| automatic sends per message | 5 | web `chat/waiting-sends.ts` `MAX_AUTO_SENDS`, iOS `WaitingSends.maxAutoAttempts`, Android `MAX_AUTO_RESENDS` |
| a send that fails while the state already reads Online is retried after | 3 s | web and Android `WAITING_WHILE_ONLINE_RECHECK_MS`, iOS `WaitingSends.waitingWhileOnlineRecheckMs` |

All chosen, not measured.
