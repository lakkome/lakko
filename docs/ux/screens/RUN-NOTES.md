<!-- SPDX-License-Identifier: AGPL-3.0-or-later -->
<!-- SPDX-FileCopyrightText: 2026 Lakko Contributors -->

# Screen capture run - 2026-09-21

Every screenshot under `web/`, `ios/` and `android/` was taken from a build
of this commit against the `test.lakko.me` test stack. Nothing here is drawn.

## Builds

| Client | Build |
|---|---|
| iOS | `xcodebuild` Debug, installed on iPhone 17 Pro and iPhone 17 Pro Max (iOS 26.4) |
| Android | `./gradlew installDebug`, Pixel emulator (`lakko_test_35`, API 35) |
| Web | deployed `test.lakko.me` |

## Accounts made for the run

| Account | Device | Password |
|---|---|---|
| `ivan_petrov` (Ivan Petrov) | iPhone 17 Pro Max | `LakkoMatrix2026!` |
| `anna_weber` (Anna Weber) | iPhone 17 Pro | `LakkoMatrix2026!` |
| `maria_lopez` (Maria Lopez) | Android emulator | `LakkoMatrix2026!` |
| `dmitry_kim` (Dmitry Kim) | web | `LakkoMatrix2026!` |

Test accounts on a test stack; the passwords are written down here on purpose
so the run can be repeated.

### Recovery phrases

- `ivan_petrov`: morning fit museum diagram van planet strike paddle valley
  unfair identify nose erupt exotic shift express slab party harvest physical
  chicken brand marble plate
- `anna_weber`: draft brother phone tilt rule outdoor judge job shine cool
  skill diary grief youth dignity parrot fatal kid antique slot obtain indoor
  feature topic
- `maria_lopez`: mandate goddess elder mistake thought erosion alley trial
  napkin ghost dream stumble nasty front grid appear hint detail traffic
  another online smart pride usual

## What the run found

1. **Two conversations with the same contact.** After `dmitry_kim` (web) sent a
   contact request and `anna_weber` (iOS) accepted it, the iPhone lists Dmitry
   Kim twice: one chat carrying the messages, one reading "No messages yet".
   The web side lists one. Captured in `ios/05-chats-list.png`.
2. **Android shows `[mls commit]` to the user.** Creating a group writes a
   system row that every client produces the same way
   (`ChatRepository` -> `"[mls commit]"`), but only iOS and web hide it.
   Android renders it as a message bubble in the group and as the group's
   last-message preview in the list. iOS has `ChatTimeline.isHiddenSystemRow`;
   Android has no equivalent. The design system forbids protocol words in
   product UI. Captured in `android/13-groups.png`, `android/15-group-chat.png`
   against `ios/13-groups.png`.
3. **One group, three different list previews.** iOS reads "No messages yet",
   Android reads "[mls commit]", web reads "4 people".
4. **A contact has no name for the first ~20 seconds.** Right after the accept,
   the web contact list renders all three accepted contacts as "Member"
   (`apps/web/src/profile/member-label.ts`). The names appear once the profile
   envelope arrives. Not a defect, but the empty-looking state is what a new
   user sees first.
5. **Finding a channel by handle fails silently on Android.** `@design_notes`
   was created on web and resolves on iOS: the search offers "Find
   @design_notes - open a channel you are not subscribed to yet", opens the
   reader, and after Subscribe the posts arrive. On Android the same handle,
   typed into "Find a channel" and into the list search, closes the dialog and
   leaves "No channels yet" - no result, no error, nothing in logcat.
   Captured in `android/17b-channel-finding.png` against
   `ios/16b-channel-search.png` and `ios/17-channel-reader.png`.
6. **Android's account actions live on the Passkeys screen.** Delete account,
   Delete my account, Export my data and "Your data" are all rendered there
   (`android/32-passkeys.png`). Web keeps them under Settings > Account
   (`web/36-settings-account.png`).
7. **A 1x1 call from Android ends immediately.** Tapping the phone button in a
   conversation writes an "Outgoing call" row into the timeline within seconds
   and never shows a call screen; the web side, watched throughout, never
   raised an incoming call. This is the area `docs/PRODUCT-READINESS.md`
   already marks PARTIAL, so it is a confirmation, not a new finding.
   Captured in `android/20-call-outgoing.png`.
8. **A channel created without a handle can never get one on web.** The handle
   is claimed only on the create screen (`Channels.tsx:254`); Manage offers
   Name and Description only, so the channel stays unfindable.

## Corrections to the earlier drawn matrix

- iOS Devices does offer both directions ("Show a code on the new device and
  scan it here, or the other way round"), so the drawn note that only web
  offered both was wrong.
- The web auth panel headline is "Your messages, your rules.", not a
  per-screen headline.
