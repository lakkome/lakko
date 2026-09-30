<!--
SPDX-License-Identifier: AGPL-3.0-or-later
SPDX-FileCopyrightText: 2026 Lakko Contributors
-->

# Localization batch 5: calls

ADR-0058 S5, batch 5. Every user-visible string of the call surfaces moved from web literals into `l10n/catalog/en.json`: the call screen (`routes/Call.tsx` and the rule modules behind it: status line, controls, self-view, participant tiles and list, the call's chat panel, the encryption chip and safety-number sheet, the connection-quality popover, the closing screen), the full-screen incoming call and the muted-call panel, the call bar a put-away call collapses into (`docs/ux/calls.md` 4.5), the call history (timeline rows and the Calls screen), the one-call-at-a-time refusals, the group-call start outcomes and the group-call banner on other screens, the call-from-a-link strip and the `/c/` failure text, the in-call Invite line, the capture fallbacks and camera failures, `calls/invite.ts`, and the words of the web call notification. The iOS and Android clients map their own literals of the same screens onto these keys; names follow meaning, not the web component.

149 keys added, 13 of them `security_critical` (they render in English until a translation is `reviewed`, and a signed update can never change them).

## Conventions

The conventions of batches 1-4 (`docs/l10n/batch-1-auth.md` to `batch-4-contacts-channels.md`) hold. In addition:

- A call flow's refusal or failure is held as the way to render it, not as text (`CallText = () => string` in `calls/call-slot.ts`): `callBusyMessage`, the `busy` / `failed` outcomes of `startGroupCall` and `startLinkCall`, the link slot's `errorMessage`, `CAMERA_UNAVAILABLE_NOTICE`. A screen keeps it in a `createLiveText` signal, so a refusal already on screen follows a language switch.
- Failures render from their kind (`calls/call-errors.ts`): a browser without media encryption (`call.error.no_media_encryption`), a call-admission `429` (`call.error.not_accepting`), `401` (`common.error.session_expired`), `503` (`call.error.unavailable`), no network (`common.error.offline`), refused or missing microphone (`call.error.microphone_not_allowed` / `_not_found`), else the flow's own fallback (`call.error.start_group`, `call.error.join_group`, `call.link.error.generic`). Core, SFU and server prose goes to the log only; before this batch a `429` reached the screen as "... failed with HTTP 429: ...".
- The one-call-at-a-time refusal is one whole sentence per (running call, start or join): `call.busy.*`, never a phrase slotted into a frame.
- A spoken duration is built from `common.duration.hours` / `minutes` / `seconds`, joined pair by pair through `common.duration.pair` (`calls/duration-words.ts`). The running time on screen stays a clock value (`12:04`).
- The closing screen's "Call ended · 12:04" is `call.status.ended` and the clock value joined through `common.fact_pair`.
- The connection popover's numbers are formatted by the platform for the interface language; the unit is its own message (`call.quality.ms`, `bps`, `kbps`, `mbps`), and the quality tier a word of its own (`call.quality.tier.*`).
- The call bar's spoken name is a list joined with `joinList` (`common.list_pair`): `call.bar.return_a11y`, then the state, then `call.bar.encrypted_a11y` / `call.bar.camera_on_a11y` when they apply.
- A participant the device cannot name: `call.participant.unnamed` on the call screen, `chat.peer.fallback_name` where the web roster used "Peer"; own tile `call.participant.you`. The encryption sheet treats both stand-ins, in English or in the interface language, as no name (`call.encryption.sentence_group`).
- The failure of a call from a link, off its route, is two sentences through `common.sentence_pair`: `call.link.error.generic` and the reason.
- Web notification: the page renders the notification words (`call.kind.*`, `call.incoming.accept` / `decline`, `call.notification.incoming`) into Cache Storage whenever the language changes (`src/l10n/notification-strings.ts`); the service worker reads them at push time and falls back to English word by word (`notificationStrings` in `public/sw-call-notification.js`).

## Reused keys

`common.connecting`, `common.copy`, `common.copied`, `common.done`, `common.cancel`, `common.dismiss`, `common.fact_pair`, `common.sentence_pair`, `common.list_pair`, `common.duration.minutes`, `common.duration.seconds`, `common.error.offline`, `common.error.session_ended`, `common.error.session_expired`, `nav.calls`, `chat.composer.placeholder`, `chat.composer.send`, `chat.peer.fallback_name`, `chat.day.today`, `chat.day.yesterday`, `chat.call.voice`, `chat.call.video`, `chat.call.join_short`, `chat.call.group_in_progress`, `chat.call.unavailable`, `chats.row.call.label`, `call.incoming.accept`, `call.incoming.decline`, `call.link.waiting`, `call.link.ended`, `call.link.dead`, `call.link.failed`, `call.link.error.generic`, `invite.uses.*`, `invite.expiry.*`.

Where the old web wording differed from a reused key, the catalog wording is used: the call-from-a-link strip says "Waiting for the owner to admit you..." and "This call has ended or is not active.", and a failure with a reason reads "The call failed. <reason>" instead of "The call failed: <reason>".

## Web-only keys

- `call.error.no_media_encryption`: the browser has no `RTCRtpScriptTransform`; the natives check their SFrame shim at boot instead.
- `call.control.speaker_default_device`, `call.control.speaker_next_device`: stand-ins for an output device the browser lists without a name.
- `call.notification.incoming`: the service worker's generic call notification when the background decrypt could not name the call.

Every other key applies to all three clients.

## Left for other batches or not moved

- `public/sw.js` still writes "New message" for the generic message notification: it belongs with the chat notification text. The notification-words mechanism added here can carry it.
- `public/sw-push-decrypt.js` still fills the preview's `body` with "Voice call" / "Video call" for a caller that ignores the `call` field; `public/sw.js` builds the real ring from `call` with the stored words, so that body is never shown.
- The design route's mock participant name ("Anna Weber") and the example link `https://lakko.me/c/…` are not messages.
- `NewCallArgError` and the SFU / signaling argument errors are developer errors that never reach a screen.
- The running time and the call history's clock values (`12:04`) use Western digits on every language.

## In-call controls

| Key | English | security_critical |
|---|---|---|
| `call.control.bar_a11y` | Call controls |  |
| `call.control.camera` | Camera |  |
| `call.control.camera_start_a11y` | Start camera |  |
| `call.control.camera_stop_a11y` | Stop camera |  |
| `call.control.chat` | Chat |  |
| `call.control.chat_hide_a11y` | Hide chat |  |
| `call.control.chat_show_a11y` | Show chat |  |
| `call.control.end` | End |  |
| `call.control.end_a11y` | End call |  |
| `call.control.invite` | Invite |  |
| `call.control.invite_a11y` | Invite someone to this call with a link |  |
| `call.control.mute` | Mute |  |
| `call.control.mute_a11y` | Mute microphone |  |
| `call.control.participants` | Participants |  |
| `call.control.participants_hide_a11y` | Hide participants |  |
| `call.control.participants_show_a11y` | Show participants |  |
| `call.control.speaker` | Speaker |  |
| `call.control.speaker_a11y` | Play sound through {device} |  |
| `call.control.speaker_default_device` | the default speaker |  |
| `call.control.speaker_next_device` | the next speaker |  |
| `call.control.unmute_a11y` | Unmute microphone |  |

## Status line and names

| Key | English | security_critical |
|---|---|---|
| `call.participant.unnamed` | Participant |  |
| `call.participant.you` | You |  |
| `call.status.calling` | Calling... |  |
| `call.status.calling_spoken` | Calling |  |
| `call.status.connecting_spoken` | Connecting |  |
| `call.status.ended` | Call ended |  |
| `call.status.group_count` | Group call · {count} |  |
| `call.status.reconnecting` | Reconnecting... |  |
| `call.status.reconnecting_spoken` | Reconnecting |  |
| `common.duration.hours` | {count, plural, one {# hour} other {# hours}} |  |
| `common.duration.pair` | {first} {second} |  |

## Encryption chip and sheet

| Key | English | security_critical |
|---|---|---|
| `call.encryption.chip` | Encrypted | yes |
| `call.encryption.chip_a11y` | This call is encrypted. Show the safety number | yes |
| `call.encryption.compare_hint` | Read these numbers aloud. If they match on both phones, no one is listening. | yes |
| `call.encryption.number_a11y` | Safety number: {groups} | yes |
| `call.encryption.number_unavailable` | Safety number unavailable | yes |
| `call.encryption.sentence_group` | Only the people on this call can hear it. | yes |
| `call.encryption.sentence_named` | Only you and {name} can hear this call. | yes |
| `call.encryption.setting_up` | Setting up... | yes |
| `call.encryption.title` | This call is encrypted | yes |

## The call bar (a call put away, docs/ux/calls.md 4.5)

| Key | English | security_critical |
|---|---|---|
| `call.bar.camera_on_a11y` | your camera is on |  |
| `call.bar.encrypted_a11y` | encrypted | yes |
| `call.bar.region_a11y` | Call in progress |  |
| `call.bar.return_a11y` | Return to the call. {name} |  |

## Connection quality

| Key | English | security_critical |
|---|---|---|
| `call.quality.a11y` | Connection quality: {tier} |  |
| `call.quality.bps` | {value} bps |  |
| `call.quality.details_a11y` | Connection details |  |
| `call.quality.direct` | Direct |  |
| `call.quality.down` | Down |  |
| `call.quality.kbps` | {value} kbps |  |
| `call.quality.mbps` | {value} Mbps |  |
| `call.quality.ms` | {value} ms |  |
| `call.quality.relay` | Relay |  |
| `call.quality.round_trip` | Round trip |  |
| `call.quality.tier.bad` | bad |  |
| `call.quality.tier.excellent` | excellent |  |
| `call.quality.tier.fair` | fair |  |
| `call.quality.tier.good` | good |  |
| `call.quality.tier.poor` | poor |  |
| `call.quality.title` | Connection: {tier} |  |
| `call.quality.up` | Up |  |

## Call screen pieces

| Key | English | security_critical |
|---|---|---|
| `call.chat.a11y` | Call chat |  |
| `call.chat.close_a11y` | Close chat |  |
| `call.chat.empty` | No messages yet. Messages here are end-to-end encrypted. | yes |
| `call.chat.joined` | {name} joined the call |  |
| `call.chat.left` | A participant left the call |  |
| `call.collapse_a11y` | Put the call away and return to the app |  |
| `call.collapse_title` | Put the call away |  |
| `call.fill.remote_camera_a11y` | {name}'s camera |  |
| `call.participant.muted` | Muted |  |
| `call.participant.muted_a11y` | Microphone muted |  |
| `call.participant.pinned` | Pinned |  |
| `call.participants.close_a11y` | Close participants |  |
| `call.participants.title` | Participants ({count}) |  |
| `call.self.camera_a11y` | Your camera |  |
| `call.self.flip_a11y` | Switch to the other camera |  |
| `call.self.starting_camera` | Starting camera... |  |

## Incoming call

| Key | English | security_critical |
|---|---|---|
| `call.e2ee` | End-to-end encrypted | yes |
| `call.incoming.accept_video_a11y` | Accept video call from {name} |  |
| `call.incoming.accept_voice_a11y` | Accept voice call from {name} |  |
| `call.incoming.accept_without_video` | Accept without video |  |
| `call.incoming.accept_without_video_a11y` | Accept call from {name} without video |  |
| `call.incoming.decline_video_a11y` | Decline video call from {name} |  |
| `call.incoming.decline_voice_a11y` | Decline voice call from {name} |  |
| `call.incoming.video_a11y` | Incoming video call from {name} |  |
| `call.incoming.voice_a11y` | Incoming voice call from {name} |  |
| `call.kind.video` | Video call |  |
| `call.kind.voice` | Voice call |  |

## Call history (timeline rows and the Calls tab)

| Key | English | security_critical |
|---|---|---|
| `call.history.empty` | No calls yet |  |
| `call.history.join_by_link` | Join by link |  |
| `call.history.join_invalid` | That does not look like a call link. Paste the whole link and try again. |  |
| `call.history.new_call` | New call |  |
| `call.history.paste_label` | Paste a call link |  |
| `call.history.unknown_conversation` | Unknown |  |
| `call.log.a11y.call_back` | {head}. Call back |  |
| `call.log.a11y.lasted` | {head}, lasted {duration} |  |
| `call.log.a11y.video_declined` | Video call declined |  |
| `call.log.a11y.video_incoming` | Video call, incoming |  |
| `call.log.a11y.video_missed` | Missed video call |  |
| `call.log.a11y.video_outgoing` | Video call, outgoing |  |
| `call.log.a11y.voice_declined` | Call declined |  |
| `call.log.a11y.voice_incoming` | Call, incoming |  |
| `call.log.a11y.voice_missed` | Missed call |  |
| `call.log.a11y.voice_outgoing` | Call, outgoing |  |
| `call.log.declined` | Declined |  |
| `call.log.incoming` | Incoming call |  |
| `call.log.incoming_duration` | Incoming call, {duration} |  |
| `call.log.missed` | Missed call |  |
| `call.log.outgoing` | Outgoing call |  |
| `call.log.outgoing_duration` | Outgoing call, {duration} |  |
| `common.open` | Open |  |

## Busy and start outcomes

| Key | English | security_critical |
|---|---|---|
| `call.busy.call_join` | You are already in a call. End it before you join another one. |  |
| `call.busy.call_start` | You are already in a call. End it before you start another one. |  |
| `call.busy.group_join` | You are already in a group call. End it before you join another one. |  |
| `call.busy.group_start` | You are already in a group call. End it before you start another one. |  |
| `call.busy.link_join` | You are already in a call from a link. End it before you join another one. |  |
| `call.busy.link_start` | You are already in a call from a link. End it before you start another one. |  |
| `call.camera_failed.not_allowed` | Lakko was not allowed to use the camera. The call continues without it. |  |
| `call.camera_failed.not_found` | No camera was found. The call continues without it. |  |
| `call.camera_failed.other` | The camera could not be started. It may be in use by another app. |  |
| `call.capture.both.failed` | The microphone and camera could not be started. You can listen and watch. |  |
| `call.capture.both.not_allowed` | Lakko was not allowed to use the microphone or camera. You can listen and watch. |  |
| `call.capture.both.not_found` | No microphone or camera was found. You can listen and watch. |  |
| `call.capture.camera.failed` | The camera could not be started. You joined with your microphone. |  |
| `call.capture.camera.not_allowed` | Lakko was not allowed to use the camera. You joined with your microphone. |  |
| `call.capture.camera.not_found` | No camera was found. You joined with your microphone. |  |
| `call.start.no_camera` | No camera available, so this is a voice call. |  |

## Invite from inside a call (docs/ux/call-links.md 5)

| Key | English | security_critical |
|---|---|---|
| `call.invite.copied` | Link copied. Anyone you send it to can ask to join. |  |
| `call.invite.minting` | Preparing a link to this call. This takes a few seconds. |  |
| `call.invite.shared` | Link shared. |  |
| `call.invite.shown` | Copy this link: {url} |  |
| `call.invite.signed_out` | You are signed out. Sign in again to get a link for this call. |  |
| `call.invite.unavailable` | The link could not be prepared. The call is unaffected, so you can try again. |  |

## A call from a link, seen from another screen

| Key | English | security_critical |
|---|---|---|
| `call.link.notice.a11y` | Call from a link |  |
| `call.link.notice.connecting` | Connecting to the call... |  |

## Call failures (mapped from the kind of failure, never from its prose)

| Key | English | security_critical |
|---|---|---|
| `call.error.join_group` | Could not join the group call. Try again. |  |
| `call.error.microphone_not_allowed` | Lakko was not allowed to use the microphone. |  |
| `call.error.microphone_not_found` | No microphone was found. |  |
| `call.error.no_media_encryption` | This browser cannot encrypt call media, so the call did not start. | yes |
| `call.error.not_accepting` | This call is not accepting new participants right now. |  |
| `call.error.not_member` | You are no longer a member of this chat. |  |
| `call.error.signed_out_start` | You are signed out. Sign in again to start a call. |  |
| `call.error.start_group` | Could not start the group call. Try again. |  |
| `call.error.unavailable` | Calls are not available right now. Try again later. |  |

## Group call banner on other screens

| Key | English | security_critical |
|---|---|---|
| `call.group_banner.join_video` | Video |  |
| `call.group_banner.open_chat` | Open chat |  |
| `call.group_banner.title` | Group call in {name} |  |

## Web notification (service worker)

| Key | English | security_critical |
|---|---|---|
| `call.notification.incoming` | Incoming call |  |

## iOS additions

Keys the iOS client needed that neither web nor Android had. Named by meaning, so Android can reuse them. The five `call.undecryptable.*` keys are `security_critical`: they are the screen saying a participant's media is refused by the decryptor rather than stalled, and a signed update must not be able to turn that into something else. The iOS group call has an overflow menu (`call.control.more`, `more_a11y`, `chat_unread`) and an in-call settings sheet (`call.settings.*`); its speaker button toggles the loudspeaker (`call.control.speaker_on_a11y` / `speaker_off_a11y`) where the web picks an output device. iOS reuses the Android additions `call.permission.camera.title` / `camera.body`, `call.permission.microphone.body` and `call.error.encryption_setup`.

| Key | English | security_critical |
|---|---|---|
| `call.alert.title` | Call |  |
| `call.control.chat_unread` | {count, plural, one {Chat (# unread)} other {Chat (# unread)}} |  |
| `call.control.more` | More |  |
| `call.control.more_a11y` | More controls |  |
| `call.control.speaker_off_a11y` | Turn off speaker |  |
| `call.control.speaker_on_a11y` | Turn on speaker |  |
| `call.encryption.copy_a11y` | Copy the safety number |  |
| `call.history.call_back_a11y` | Call back |  |
| `call.history.call_back_video_a11y` | Call back with video |  |
| `call.history.empty_body` | Calls you make and receive appear here. |  |
| `call.history.paste_hint` | The person who created the call shares the link. |  |
| `call.invite.link_a11y` | Call link |  |
| `call.invite.ready` | Anyone with this link can ask to join this call. You decide who comes in. |  |
| `call.label.group` | Group call |  |
| `call.settings.devices` | Devices |  |
| `call.settings.low_data` | Low-data mode |  |
| `call.settings.low_data_hint` | Caps video at 360p — useful on metered mobile connections. |  |
| `call.settings.microphone` | Microphone |  |
| `call.settings.mirror` | Mirror local preview |  |
| `call.settings.system_default` | System default |  |
| `call.settings.video` | Video |  |
| `call.undecryptable.named` | Can't decrypt {name}'s media | yes |
| `call.undecryptable.notice` | Can't decrypt the incoming media | yes |
| `call.undecryptable.notice_many` | {count, plural, one {Can't decrypt media from # participant} other {Can't decrypt media from # participants}} | yes |
| `call.undecryptable.tile` | Can't decrypt | yes |
| `call.undecryptable.tile_a11y` | Can't decrypt this participant's media | yes |

On iOS every call surface reads the catalog: `CallScreen` (status line, controls, encryption chip and sheet, quality popover, tiles, participant list, call chat, settings sheet, closing screen), `CollapsedCallBar`, `IncomingCallScreen`, the quiet incoming panel and row, `CallInviteSheet`, `CallHostOverlay` (the error alert and the link notice), `CallsScreen`, and the rules behind them (`CallStatusText`, `CallScreenPresentation`, `UndecryptableMedia`, `CallBarPresentation`, `CallTranscriptRow`, `CallHistory`, `CallPeerName`, `CallSlot.busyMessage`, `LinkCallHostRules.notice`, `CallInviteOfferRules`, `CallMediaChoice`, `QuietIncomingCallPresentation`, `IncomingCallReport.unknownCaller`, the permission messages). Failures render from their kind through `CallErrorText` (in `CallSlot.swift`: call-admission `429` / `401` / `503`, no network, signed out, else the flow's `call.error.start_group` / `join_group` / `call.link.error.generic`; a mid-call camera failure `call.camera_failed.not_found` / `other`); the prose goes to the log. A spoken duration is `CallTranscriptRow.durationWords` (the web `durationWords`), so it now says hours. The call bar's spoken name is `call.bar.return_a11y` and the parts joined with `Localization.joinList`, as on web. The connection popover formats its numbers in the interface locale, and the Calls screen its day headings and times.

Where the iOS wording differed from an existing key, the catalog wording is used: the Calls row's spoken name drops "Double tap to open the conversation", and the transcript row's "Double tap to call back" became `call.log.a11y.call_back` (it reads as a voice call: the row does not know the kind); the camera downgrade notice is `call.start.no_camera`; the invite panel's busy line is `call.invite.minting`, a failure `call.invite.unavailable` without the reason, signed out `call.invite.signed_out`; the call chat's empty line is `call.chat.empty`; the chevron's spoken name is `call.collapse_a11y` (the hint is gone); the chip's is `call.encryption.chip_a11y`; the incoming call's spoken summary is `call.incoming.voice_a11y` / `video_a11y` and `call.e2ee` joined as a list; the link notice says "Waiting for the owner to admit you..." and "This call has ended or is not active."; the permission messages point to "the app settings" rather than "Settings > Lakko > Camera"; a group call with no SFU endpoint says `chat.call.unavailable`, a link call with none `call.error.unavailable`. A nameless participant is `chat.members.unnamed` (the one placeholder the chat roster uses, kept so the two rosters agree) rather than `call.participant.unnamed`; "Peer" is `chat.peer.fallback_name`; the own tile is `call.participant.you`, rendered at the tile so it follows a language switch. `call.notification.incoming`, listed above as web-only, is also the iOS CallKit caller name before a call is decrypted.

The notification extension renders its call words (`call.kind.*`, `call.notification.incoming`, `call.log.missed`) and the media labels `chat.media.voice_message` / `video_message` from `SharedStorage.notificationWordsKey`, a dictionary the app writes into the App Group suite whenever it loads a language; the extension falls back to English word by word. Its "Sent an attachment" / "Sent N attachments" lines and the server's generic "New message" stay English.

Not moved on iOS: error and notice strings held as `String` on the call hosts (`callError`, `callNotice`, the link slot's `errorMessage`, a call chat's system lines) are rendered when produced, so one already on screen keeps its language until it is dismissed; the SFU / session / multi-joiner error descriptions (`SfuCallError`, `CallSessionError`, `MultiJoinerCallError`) reach the log only. The design preview's mock names stay English.

## Android additions

Keys the Android client needed that neither web nor iOS had. Named by meaning, so iOS can reuse them. Android reuses the iOS additions `call.control.speaker_on_a11y` / `speaker_off_a11y`, `call.control.more` / `more_a11y`, `call.control.chat_unread`, `call.label.group` and `call.settings.*`.

| Key | English | security_critical |
|---|---|---|
| `call.control.lower_hand` | Lower hand |  |
| `call.control.raise_hand` | Raise hand |  |
| `call.control.share_screen` | Share screen |  |
| `call.control.stop_share_screen` | Stop sharing screen |  |
| `call.error.encryption_setup` | Could not set up call encryption, so the call did not start. | yes |
| `call.invite.retry_a11y` | Try preparing the invite link again |  |
| `call.invite.share_a11y` | Share the invite link |  |
| `call.notification.channel_description` | Incoming Lakko calls |  |
| `call.participant.hand_raised` | Hand raised |  |
| `call.participant.numbered` | Participant {number} |  |
| `call.participant.sharing` | Sharing |  |
| `call.participant.sharing_a11y` | Sharing screen |  |
| `call.permission.audio_only` | Audio only |  |
| `call.permission.camera.body` | Video needs the camera. You can continue with audio only, or allow camera access for Lakko in the app settings. |  |
| `call.permission.camera.title` | Camera access needed |  |
| `call.permission.microphone.body` | Calls need the microphone. Allow microphone access for Lakko in the app settings, then try again. |  |
| `call.permission.microphone.title` | Microphone access needed |  |
| `call.settings.no_devices` | No devices reported yet. They appear here once you grant microphone or camera permission. |  |
| `call.status.connected` | Connected |  |
| `call.status.in_call_spoken` | in call, {duration} |  |
| `call.status.ringing` | Ringing... |  |
| `notification.channel.messages` | Messages |  |
| `notification.channel.messages_desc` | Wake notifications for new Lakko messages |  |
| `notification.message.attachments` | {count, plural, one {Sent an attachment} other {Sent # attachments}} |  |
| `notification.message.new` | New message |  |

`call.permission.*` exist because the iOS `call.permission.*_denied_app` name the iOS Settings path; the Android dialogs send the person to the app's settings page with `chat.record.open_settings`. `call.participant.numbered` is the ADR-0021 fallback for a group-call peer this device cannot name (`unresolvedParticipantLabel`), so two of them stay apart. `notification.*` are the generic message notification (`LakkoMessagingService`) and its channel; web's `sw.js` and the iOS extension can take them. The raise-hand and screen-share menu entries have no real backing yet; they are moved because they are on screen.

On Android the call surfaces read the catalog: `CallScreen` (status line, lock chip and encryption sheet, connection popover, tiles, participant list, overflow menu, call chat, settings sheet), `CallControlBar`, `CollapsedCallBar`, `IncomingCallScreen` and `IncomingCallActivity`, the quiet incoming panel and row actions, `CallPermissions`, `HostedCallOverlays` (the link-call notice bar), `JoinCallScreen`, `CallsScreen` / `CallsHost`, the incoming-call notification and its channel (`IncomingCallRinger`), and the rules behind them: `callControlRow` and `callInviteButton` hold `UiText`; `callStatusText`, `callStatusSpokenLabel`, `callRowText`, `callRowSpokenLabel`, `callBarStatusText`, `callScreenTitle`, `unresolvedParticipantLabel`, `safetyNumberSpokenLabel`, `callRingContent` and `requestedCallKindLabel` take the `Strings` on screen; `callBusyMessage`, `callEncryptionSentence`, `callInviteFailureMessage`, `newCallStartMessage`, `callEntryNotice`, the chat's call notice, the group-call start outcomes (`GroupCallStartOutcome.Busy` / `Failed`), the link-call busy outcome, the link slot's error and `linkCallNotice` are `UiText`, so a notice already on screen follows a language switch. A spoken duration is `spokenCallDuration` (the web `durationWords`). The call bar's spoken name is `call.bar.return_a11y`, the state in words, then `call.bar.encrypted_a11y` / `call.bar.camera_on_a11y`, joined with `joinList`. The connection popover formats its numbers with the interface language's `NumberFormat`; the Calls screen's day headings and times use the interface language.

Failures render from their kind through `calls/CallErrorText.kt` (`callErrorReason` / `callErrorText`: call-admission `429` / `401` / `503`, no network, a refused microphone, else the flow's own key: `call.error.start_group` / `join_group`, `call.link.error.generic`); core, SFU and server prose goes to the log. `LinkCallStartOutcome.Failed` keeps its diagnostic string for the log only. A link call that failed with a known kind reads `call.link.error.generic` and the reason through `common.sentence_pair`, on the notice bar and on the `/c/` page.

The own tile and the own call-chat sender are empty data and render `call.participant.you` at the tile, so they follow a switch; the "Peer" fallback is gone (a peer with no pseudonym renders `call.participant.unnamed`). `PLACEHOLDER_MEMBER` and `PLACEHOLDER_SOMEONE` now read `chat.members.unnamed` / `contacts.request.someone` in the interface language when a name is resolved. The call notification and the generic message notification load the language in `LakkoMessagingService` and `IncomingCallActivity`, which can run before `MainActivity`; the call channel's name and description follow the language on the next ring.

Where the Android wording differed from an existing key, the catalog wording is used: "Call ended, 12:04" is `call.status.ended` and the clock joined through `common.fact_pair`; the preparing invite control reads `common.preparing` with `call.invite.minting` under it; an invite link that failed for the network reads `common.error.offline`; `New call` failing reads `chat.call.error.start`; the pre-existing "Encrypted call. Double tap to see the safety number." is `call.encryption.chip_a11y`; "Put the call away. The call keeps running." is `call.collapse_a11y`; "Switch camera" is `call.self.flip_a11y`; "Muted" on a tile is `call.participant.muted_a11y`; "Close participant list" is `call.participants.close_a11y`; "Send a message" is `chat.composer.placeholder`; "Unknown" on a tile is `call.participant.unnamed`; the popover's "RTT" line is `call.quality.round_trip` and the arrows `call.quality.down` / `up`, each joined to its value with `common.fact_pair`, and the tier word `call.quality.title`; "Accept without\nvideo" lost its hard line break; the link notice says "Waiting for the owner to admit you..." and "This call has ended or is not active."; the Calls screen's paste field is `call.history.paste_label`, its error `call.history.join_invalid` and its button `chat.call.join_short`; a nameless one-to-one call title is `chat.call.voice`; a missing group-call record reads `call.error.join_group`, a group call cancelled or failing to announce or connect `call.error.start_group` / `join_group`, and "Sign in again to use group calls." is `call.error.signed_out_start`.

Not moved on Android: the design route's mock names (`Alice`, `Bob`, `Carl`); the running time and history clock values (`12:04`) stay Western digits; the `Lakko` brand word in notification titles.
