<!-- SPDX-License-Identifier: AGPL-3.0-or-later -->
<!-- SPDX-FileCopyrightText: 2026 Lakko Contributors -->

# UX spec: Lakko works in one tab at a time

**Status:** accepted 2026-11-21 (maintainer chose one active tab, the WhatsApp Web
shape). Built on web and the desktop app the same day, unit-tested, not walked.
**Surfaces:** web, desktop app. iOS and Android have one process per app and are not
affected.

## 1. Why

A tab holds the whole state of the person's conversations in memory and writes it back
to the browser's storage after every change. Two tabs of one browser share that storage
but not the memory, so a tab that is behind can write an old copy over a newer one; on
2026-11-21 that left an owner's only device unable to read its own group again. So only
one tab runs Lakko; the others wait.

## 2. The rule

One Lakko tab per browser runs at a time. It is per browser, not per account: a tab that
is not running does not know which account it would sign in to, and every tab of a
browser shares the storage of every account on it.

## 3. States

| State | What the tab shows | What runs |
|---|---|---|
| Active | Lakko as usual. | Everything. |
| Inactive | The notice below, nothing else. | Nothing that reads or writes conversations: no polls, no sending, no calls, no saving. The service worker still shows notifications (section 5). |
| Taking over | The notice with the button showing "Opening here..." and disabled. | Waiting for the other tab to stop. |

The notice (public layout, logo, one card):

| Element | Key | Text |
|---|---|---|
| Title | `tab.other.title` | Lakko is open in another tab. |
| Line | `tab.other.body` | Lakko works in one tab at a time. Use it here and the other tab stops. |
| Button | `tab.other.use_here` | Use here |
| Button while waiting | `tab.other.opening` | Opening here... |

There is no Close button: closing the tab is the browser's.

## 4. Flow

1. A tab opens (a new tab, a link such as `/i/...` or `/g/...`, a reload). If no other
   Lakko tab runs, it becomes active and opens where the address pointed.
2. If another tab runs, the new tab shows the notice. The address it was opened with is
   kept: after Use here it opens there.
3. Use here: the button shows "Opening here...". The running tab saves the draft being
   typed (below), then stops (nothing more is saved or sent from it), reloads and shows
   the notice itself. The new tab then
   starts Lakko from what is saved, exactly as after a reload.
4. If the running tab does not answer within 10 seconds (a frozen or crashed tab), the
   new tab takes over anyway. Anything that tab still writes afterwards is refused by
   the storage, which keeps the newer copy (section 6).
5. Closing the running tab does not start another one by itself: a waiting tab stays on
   the notice until its button is pressed.

Nothing typed is lost. Before it reloads, the tab that stops saves the open
composer's draft (as it does after a pause in typing) and writes it to the browser, for
at most a second; the new tab opens the conversation with the draft in place. The draft
is kept in the browser before it is sent to the person's other devices, so a slow or
refused send never costs it. If the write fails or takes longer, the tab reloads anyway. It never asks: a question would
leave two tabs running. A tab taken over after 10 seconds without an answer (step 4)
has already lost the lock and saves nothing more.

## 5. Notifications and the service worker

- Notifications are shown by the service worker whatever the tabs do; nothing changes
  for the person.
- A tab that is not running counts as no tab: a call or message notification is shown
  even when the only visible tab is one showing the notice.
- Clicking a notification raises the running tab and opens the conversation there. With
  no running tab it opens a new one, which runs if nothing else does.

## 6. When the browser cannot tell tabs apart

The running tab holds a browser lock (`navigator.locks`, every browser Lakko supports
since 2022; secure pages only). Where the lock is missing, tabs ask each other over a
browser channel (`BroadcastChannel`) at start: a tab that hears no answer within a third
of a second runs. Two tabs started in the same instant can both run there. Where both
are missing, every tab runs as before this change.

In every case the storage keeps the backstop: a tab that loaded an older copy than the
one saved now cannot save over it; it reloads instead (and then shows the notice if
another tab runs).

It reloads once. A Lakko page that does not take part in the lock (an older version
still open somewhere in this browser) can keep saving over the copy; seen 2026-11-21,
every 30 seconds. If the save is refused again within two minutes of that reload, the
tab stops saving and shows the notice with a different line: "Another Lakko tab or
window in this browser is still running. Close it, then use Lakko here." The button
reloads only when the person presses it. It never reloads on its own a second time,
so nothing typed is wiped by a loop.

## 7. Desktop app

The desktop app has one window and is always the running tab. The collapsed call window
is not a Lakko tab and is not counted.

## 8. Data the client needs

- A lock name per browser: `lakko-active-tab`.
- A channel name: `lakko-active-tab`, messages `takeover`, `probe`, `active`.
- No server change, nothing new stored.
