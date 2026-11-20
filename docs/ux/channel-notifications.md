<!-- SPDX-License-Identifier: AGPL-3.0-or-later -->

# Channel post notifications

Status: decided 2026-11-13 (maintainer). Built in two steps: first the
device notices a new post itself (this document), later a push wake for
channels in its own ADR. Nothing here needs a server or protocol change.

## 1. What the person gets

A new post in a channel the person follows shows a notification, the same
kind a new message in a group shows:

- **Title:** the channel's name, as the Chats row shows it.
- **Body:** the post as the Chats row's second line reads it: the post's
  text, or for a post with no caption the word for its attachment (Photo,
  File name, GIF, Voice message, Video message). With message previews off
  in Settings > Notifications: "New post" is not added; the existing generic
  "New message" is used.
- **Tap:** opens the channel at that post.
- **Grouping:** several posts of one channel collapse into that channel's
  group where the platform groups notifications (iOS thread, Android group,
  one notification per channel on web and desktop, the newest replacing the
  older).

**On by default** (decided 2026-11-13): subscribing turns them on. Mute in
the row menu, or in Channel info, turns them off for that channel, as for a
group.

## 2. When a post does not notify

- The channel is muted.
- The person is a publisher of the channel and the post is their own (any
  of their devices).
- The channel is open and visible on this device (a focused window on
  desktop and web, the foreground screen on a phone).
- The post is not new to the person: it is at or below the channel's read
  marker, or this device already showed a notification for it.
- The post arrived through a refetch of the channel's history: the first
  read after subscribing, after joining as a publisher, after a log format
  change or a restore. Those posts are history, not news; at most the
  newest one may notify on the first read after subscribing, and only when
  it is less than an hour old.
- A metadata record, a ban list, a rotation or any other record that is not
  a post.
- The person banned from comments gets no notification about it (spec
  group-and-channel-privacy section 13.3).

## 3. When the device notices

There is no push for channels yet, so a post is noticed when this device
reads the channel:

- **App open:** the existing channel post sync, every time it runs.
- **App in the background:**
  - **Android:** the periodic background work the app already schedules, or
    a new periodic work at the platform's shortest interval (15 minutes).
  - **iOS:** a background app refresh task; the system decides when it
    runs, so a notification can come late or not at all until the app is
    opened.
  - **Web:** while a Lakko tab is open but hidden. A closed tab notifies
    nothing.
  - **Desktop:** the window may be hidden; the host keeps the page running.
- Honest cost, said in one place only (Settings > Notifications, under the
  channel line, small text): "While Lakko is closed, notifications about new
  channel posts can come late." No other screen mentions it.

## 4. Settings

Settings > Notifications keeps one switch per kind already there. No new
switch for channels: a channel is muted from its row or its info screen, as
a group is.

## 5. Data each client needs

- Per channel: muted (the synced flag, already there), the read marker
  (already there), and a new "last notified post" cursor per channel, kept
  per device (not synced), so a post is never notified twice and a refetch
  does not notify old posts.
- Whether the post's author is this account (publisher posts carry the
  publisher key; the device knows its own).

## 6. Cross-platform shape

One rule decides "notify or not" for a post (section 2) and is the same on
the three clients; it is a candidate for `lakko-ui-rules` so it is written
once. The notification surface is each platform's own (system notification
on iOS, Android, desktop; the Notification API on web).
