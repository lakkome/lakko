<!--
SPDX-License-Identifier: AGPL-3.0-or-later
SPDX-FileCopyrightText: 2026 Lakko Contributors
-->

# Localization batch 3: conversation list and chat screen keys

ADR-0061 S5, batch 3. Every user-visible string of the merged conversation list and of the chat screen (header, timeline, message menu, reply / forward / pin, selection, composer with voice and video-note recording, emoji and GIF picker, group info with its member list, join requests, invite link, ownership transfer, leave and delete, the ended-membership states, membership refusals and the errors of every chat action) moved from web literals into `l10n/catalog/en.json`. The in-call screens (`routes/Call.tsx`, `calls/*`) are a later batch. The iOS and Android clients map their own literals of the same screens onto these keys; names follow meaning, not the web component.

387 keys added, 10 of them `security_critical` (they render in English until a translation is `reviewed`, and a signed update can never change them).

## Conventions

The batch 1 and 2 conventions (`docs/l10n/batch-1-auth.md`, `docs/l10n/batch-2-settings.md`) hold. In addition:

- A list of names is joined pair by pair through `common.list_pair` (`{first}, {second}`), so each language picks its own separator and nothing says "and". Web: `joinList` in `apps/web/src/l10n/index.ts`; used for group titles, "Read by", "Reacted by" and failed adds.
- Two whole sentences shown together go through `common.sentence_pair`; a refusal followed by when to retry through `chat.refusal.with_retry`.
- File sizes are `common.size.*` with `{size}` already formatted by the platform's number formatter for the interface language (one decimal for kB / MB / GB).
- Spoken durations (screen readers) are `common.duration.*`; the visible clock `0:07` stays in Latin digits and is not a catalog message.
- Dates in the timeline ("16 September") and on list rows (clock time, short weekday, short date) are formatted by the platform for the interface language (web: `Intl.DateTimeFormat(currentLanguage())`); only "Today" and "Yesterday" are catalog messages.
- Names (`{name}`, `{names}`) are text arguments; `{name}` in the lookup and add-member messages is passed with its `@`.
- A destructive confirmation names its target on the button (`chat.delete.confirm_named`, `chat.leave.confirm_named`, `chat.members.remove.confirm_named`); an unnamed target uses the bare verb (`common.delete`, `chat.leave.confirm`, `common.remove`). Web: `namedActionKey` in `ui/confirm-dialog.ts`.
- Errors render from their kind (ADR-0061 section 6): a membership refusal from its `RefusalReason` (`chat.refusal.*`), a contact failure from its variant, and anything else as the action's own `chat.error.*` key. Core, server and browser prose goes to the log only.
- Where an internal code used to be shown raw ("Last call ended (hungUp)."), the line is now one message per code (`chat.call.last_ended.*`).
- `chats.row.people`, `chat.members.pending.*` counts and `chat.select.*` counts are plural messages with `{count}`.

## Reused keys

`nav.chats` (the list's title and accessible name), `common.cancel`, `common.save`, `common.saving`, `common.adding`, `common.remove`, `common.dismiss`, `common.close`, `common.copy`, `common.copied`, `common.sending`, `common.looking_up`, `common.more_options`, `common.error.offline`, `common.error.session_ended`.

## Web-only keys

- `chat.composer.drop_files`: dropping files onto the page.
- `chat.composer.attach_limit`: tooltip of the attach button.
- `chat.record.denied_microphone`, `chat.record.denied_camera`, `chat.record.unsupported_voice`, `chat.record.unsupported_video`: browser permission and `MediaRecorder` wording; the natives have their own permission copy.
- `chat.record.voice_tooltip`, `chat.record.video_tooltip`, `chat.record.switch_to_voice`, `chat.record.switch_to_video`: the web keyboard / pointer affordances of the record button.
- `chat.reauth.*`: the web page shown when the tab holds no identity secrets.
- `group.join.wait_primary`, `group.join.wait_secondary`, `group.join.request_again`, `group.join.not_taking_requests`: the web `/g/` page; natives use them if their join landing shows the same lines.

Every other key applies to all three clients.

## Left for other batches

- The in-call screens and everything under `calls/`: the muted-call panel and its row label (`quietCallKindLabel`), the call history rows in the timeline (`calls/call-log.ts`), `callBusyMessage`, the group-call start outcomes and the camera-check notice (`calls/call-start-flow.ts`).
- The channel row's second line in the merged list (`channels/channel-row.ts`) goes with the channels batch.
- Contact-screen copy reached from the chat (`contacts/supersession-message.ts`, `contacts/errors.ts`, `ContactDetailView`).
- Emoji names (from the bundled Unicode data) stay English; only the category names are catalog messages.

## Shared

| Key | English | security_critical |
|---|---|---|
| `common.delete` | Delete |  |
| `common.duration.minutes` | {count, plural, one {# minute} other {# minutes}} |  |
| `common.duration.minutes_seconds` | {minutes} {seconds} |  |
| `common.duration.seconds` | {count, plural, one {# second} other {# seconds}} |  |
| `common.list_pair` | {first}, {second} |  |
| `common.retry` | Retry |  |
| `common.sentence_pair` | {first} {second} |  |
| `common.size.bytes` | {size} B |  |
| `common.size.gb` | {size} GB |  |
| `common.size.kb` | {size} kB |  |
| `common.size.mb` | {size} MB |  |

## Conversation list

| Key | English | security_critical |
|---|---|---|
| `chats.action.archive` | Archive |  |
| `chats.action.mute` | Mute |  |
| `chats.action.pin` | Pin |  |
| `chats.action.unarchive` | Unarchive |  |
| `chats.action.unmute` | Unmute |  |
| `chats.action.unpin` | Unpin |  |
| `chats.compose` | New |  |
| `chats.create.channel` | Create channel |  |
| `chats.create.chat` | New chat |  |
| `chats.create.group` | Create group |  |
| `chats.create.menu` | Create |  |
| `chats.empty.action` | Start one |  |
| `chats.empty.title` | No conversations yet |  |
| `chats.lookup.action` | Look up {name} |  |
| `chats.lookup.nothing` | Nothing found for {name}. |  |
| `chats.lookup.unreachable` | Could not look that up. Check your connection and try again. |  |
| `chats.preview.own` | You: {text} |  |
| `chats.preview.sender` | {name}: {text} |  |
| `chats.row.actions` | Conversation actions |  |
| `chats.row.call.label` | {kind} from {name} |  |
| `chats.row.ended` | You are no longer a member. |  |
| `chats.row.muted` | Muted |  |
| `chats.row.people` | {count, plural, one {# person} other {# people}} |  |
| `chats.row.pinned` | Pinned |  |
| `chats.row.unread` | {count, plural, one {# unread message} other {# unread messages}} |  |
| `chats.row.unread_overflow` | 99+ |  |
| `chats.search.label` | Search chats |  |
| `chats.search.no_results` | No results |  |
| `chats.search.placeholder` | Search |  |
| `chats.synchronizing` | Synchronizing... |  |

## Incoming call on a list row

| Key | English | security_critical |
|---|---|---|
| `call.incoming.accept` | Accept |  |
| `call.incoming.accept_from` | Accept call from {name} |  |
| `call.incoming.decline` | Decline |  |
| `call.incoming.decline_from` | Decline call from {name} |  |

## Chat header, info and not-found / sign-in pages

| Key | English | security_critical |
|---|---|---|
| `chat.group.default_name` | New group |  |
| `chat.group.fallback_name` | Group |  |
| `chat.group.title_more` | {names} +{count} |  |
| `chat.header.back` | Back to chats |  |
| `chat.info.channel` | Channel info |  |
| `chat.info.contact` | Contact info |  |
| `chat.info.group` | Group info |  |
| `chat.not_found.back` | Back to Chats |  |
| `chat.not_found.body` | This link does not match a conversation on this device. |  |
| `chat.not_found.title` | Conversation not found |  |
| `chat.peer.fallback_name` | Peer |  |
| `chat.reauth.action` | Sign in again |  |
| `chat.reauth.body` | The keys needed to decrypt and sign messages are not available in this session. Sign in again to open this chat. |  |
| `chat.reauth.title` | Sign in again |  |
| `chat.typing.named` | {name} is typing... |  |
| `chat.typing.one_to_one` | typing... |  |
| `chat.typing.several` | Several people are typing... |  |

## Group calls from the chat screen

| Key | English | security_critical |
|---|---|---|
| `chat.call.error.start` | Could not start the call. Try again. |  |
| `chat.call.group_ended` | That group call has ended. |  |
| `chat.call.group_in_progress` | Group call in progress |  |
| `chat.call.group_people` | {count, plural, one {# person in the call} other {# people in the call}} |  |
| `chat.call.group_starting` | Starting… |  |
| `chat.call.group_status_e2ee` | {status} · end-to-end encrypted | yes |
| `chat.call.join` | Join call |  |
| `chat.call.join_short` | Join |  |
| `chat.call.join_video` | Join with video |  |
| `chat.call.last_ended.busy` | Last call ended: the other person was busy. |  |
| `chat.call.last_ended.declined` | Last call ended: declined. |  |
| `chat.call.last_ended.error` | Last call ended because of an error. |  |
| `chat.call.last_ended.hung_up` | Last call ended. |  |
| `chat.call.last_ended.timed_out` | Last call ended: no answer. |  |
| `chat.call.video` | Video call |  |
| `chat.call.voice` | Call |  |

## Timeline

| Key | English | security_critical |
|---|---|---|
| `chat.author.a_member` | A member |  |
| `chat.author.unknown` | Unknown sender |  |
| `chat.author.you` | You |  |
| `chat.day.today` | Today |  |
| `chat.day.yesterday` | Yesterday |  |
| `chat.forward.empty` | No conversations to forward to |  |
| `chat.forward.error.generic` | Could not forward the message. Try again. |  |
| `chat.forward.from` | Forwarded from {name} |  |
| `chat.forward.label` | Forwarded |  |
| `chat.forward.list_label` | Conversations |  |
| `chat.forward.picker_title` | Forward to |  |
| `chat.forward.search_label` | Search conversations |  |
| `chat.message.actions` | Message actions |  |
| `chat.message.deleted` | Message deleted |  |
| `chat.message.edit` | Edit |  |
| `chat.message.edited` | (edited) |  |
| `chat.message.forward` | Forward |  |
| `chat.message.more_reactions` | More reactions |  |
| `chat.message.not_sent` | Not sent |  |
| `chat.message.pin` | Pin |  |
| `chat.message.react` | React |  |
| `chat.message.react_with` | React {emoji} |  |
| `chat.message.read` | Read |  |
| `chat.message.read_by` | Read by {names} |  |
| `chat.message.reply` | Reply |  |
| `chat.message.select` | Select |  |
| `chat.message.sending` | Sending... |  |
| `chat.message.sent` | Sent |  |
| `chat.message.unpin` | Unpin |  |
| `chat.pin.detail_personal` | Only on this device |  |
| `chat.pin.detail_shared` | Every member sees it |  |
| `chat.pin.for_both` | Pin for both |  |
| `chat.pin.for_everyone` | Pin for everyone |  |
| `chat.pin.for_me` | Pin for me |  |
| `chat.pin.sheet_title` | Pin message |  |
| `chat.pinned_bar.all` | All pinned messages |  |
| `chat.pinned_bar.for_you` | For you |  |
| `chat.pinned_bar.label` | Pinned message |  |
| `chat.pinned_bar.label_of` | Pinned message {index} of {count} |  |
| `chat.pinned_bar.list_title` | Pinned messages |  |
| `chat.reaction.add_title` | React - Reacted by {names} |  |
| `chat.reaction.reacted_by` | Reacted by {names} |  |
| `chat.reaction.remove_title` | Remove your reaction - Reacted by {names} |  |
| `chat.reaction.single_emoji` | A reaction must be a single emoji. |  |
| `chat.reply.cancel` | Cancel reply |  |
| `chat.reply.fallback_excerpt` | Message |  |
| `chat.reply.quote` | Reply |  |
| `chat.reply.quote_to` | Reply to {name} |  |
| `chat.reply.strip_label` | Replying to {name} |  |
| `chat.select.count` | {count, plural, one {# selected} other {# selected}} |  |
| `chat.select.count_limit` | {count, plural, one {# selected, the limit} other {# selected, the limit}} |  |
| `chat.select.delete_helper` | Only your own messages can be deleted. |  |
| `chat.select.delete_title` | {count, plural, one {Delete # message?} other {Delete # messages?}} | yes |
| `chat.select.deleted_of` | Deleted {done} of {total}. |  |
| `chat.select.forwarded_of` | Forwarded {done} of {total}. |  |
| `chat.select.toolbar` | Selected messages |  |
| `chat.send.failed` | Could not send. |  |
| `chat.system.join_request` | Join request |  |
| `chat.system.members_changed` | Members changed |  |
| `chat.timeline.empty` | No messages yet. Say hi. |  |
| `chat.unpin.for_both` | Unpin for both |  |
| `chat.unpin.for_everyone` | Unpin for everyone |  |
| `chat.unpin.for_me` | Unpin for me |  |
| `chat.unpin.sheet_title` | Unpin message |  |

## Media and attachments

| Key | English | security_critical |
|---|---|---|
| `chat.attachment.error.download` | Could not download the attachment. Try again. |  |
| `chat.attachment.error.gone` | The attachment is no longer available on the server. |  |
| `chat.attachment.error.preview` | Could not show the picture. Try again. |  |
| `chat.attachment.error.too_large` | That message is {size} — over the {limit} limit. |  |
| `chat.attachment.error.too_large_named` | "{name}" is {size} — over the {limit} limit. |  |
| `chat.attachment.error.upload` | Could not upload "{name}". Try again. |  |
| `chat.attachment.hide` | Hide |  |
| `chat.attachment.image_alt` | image attachment |  |
| `chat.attachment.open` | Open |  |
| `chat.attachment.unnamed` | (unnamed) |  |
| `chat.media.and_more` | {label} and {count, plural, one {# more} other {# more}} |  |
| `chat.media.file` | File |  |
| `chat.media.gif` | GIF |  |
| `chat.media.not_sent_retry` | Not sent · Retry |  |
| `chat.media.photo` | Photo |  |
| `chat.media.play_failed` | Couldn't play this message |  |
| `chat.media.video` | Video |  |
| `chat.media.video_message` | Video message |  |
| `chat.media.voice_message` | Voice message |  |
| `chat.photo.load_failed` | Couldn't load photo |  |
| `chat.photo.load_failed_retry` | Couldn't load photo, tap to retry |  |
| `chat.photo.loading` | Photo, loading |  |
| `chat.upload.encrypting` | Encrypting "{name}"… |  |
| `chat.upload.encrypting_of` | Encrypting "{name}" ({index}/{total})… |  |
| `chat.upload.uploading` | Uploading "{name}"… |  |
| `chat.upload.uploading_of` | Uploading "{name}" ({index}/{total})… |  |
| `chat.upload.working` | Encrypting + uploading… |  |
| `chat.video.load_failed` | Couldn't load video |  |
| `chat.video.load_failed_retry` | Couldn't load video, tap to retry |  |
| `chat.video.loading` | Loading video, {size} |  |
| `chat.video.play` | Play video, {size} |  |
| `chat.video_note.play` | Play video message, {duration} |  |
| `chat.video_note.play_sound` | Play video message with sound, {duration} |  |
| `chat.video_note.tap_sound` | Tap for sound |  |
| `chat.voice.not_played` | Not played |  |
| `chat.voice.pause` | Pause voice message |  |
| `chat.voice.play` | Play voice message, {duration} |  |
| `chat.voice.position` | {position} of {duration} |  |
| `chat.voice.seek` | Seek voice message |  |
| `chat.voice.speed` | Playback speed {rate} |  |

## Composer, recording, emoji and GIF

| Key | English | security_critical |
|---|---|---|
| `chat.composer.attach` | Attach files |  |
| `chat.composer.attach_limit` | Attach files (up to {limit} each) |  |
| `chat.composer.drop_files` | Drop files to attach |  |
| `chat.composer.emoji_gifs` | Emoji and GIFs |  |
| `chat.composer.placeholder` | Message |  |
| `chat.composer.remove_attachment` | Remove attachment |  |
| `chat.composer.send` | Send |  |
| `chat.emoji.categories` | Emoji categories |  |
| `chat.emoji.choose_reaction` | Choose a reaction |  |
| `chat.emoji.group.activities` | Activities |  |
| `chat.emoji.group.animals` | Animals & Nature |  |
| `chat.emoji.group.flags` | Flags |  |
| `chat.emoji.group.food` | Food & Drink |  |
| `chat.emoji.group.objects` | Objects |  |
| `chat.emoji.group.people` | People & Body |  |
| `chat.emoji.group.smileys` | Smileys & Emotion |  |
| `chat.emoji.group.symbols` | Symbols |  |
| `chat.emoji.group.travel` | Travel & Places |  |
| `chat.emoji.no_results` | No emoji found |  |
| `chat.emoji.recent` | Recent |  |
| `chat.emoji.search` | Search emoji |  |
| `chat.emoji.skin_tones` | Skin tones for {name} |  |
| `chat.emoji.tab` | Emoji |  |
| `chat.gif.attribution` | Powered by GIPHY |  |
| `chat.gif.load_failed` | Couldn't load this GIF · Retry |  |
| `chat.gif.loading` | Loading GIFs |  |
| `chat.gif.no_results` | No GIFs found |  |
| `chat.gif.note_dismiss` | Got it |  |
| `chat.gif.open_full` | Open GIF full screen |  |
| `chat.gif.privacy_note` | GIF search goes through Lakko's server, which sees what you search for. Sent GIFs are encrypted like any file. | yes |
| `chat.gif.rate_limited` | Too many searches — try again in a moment |  |
| `chat.gif.search` | Search GIF |  |
| `chat.gif.send` | Send GIF |  |
| `chat.gif.trending` | Trending |  |
| `chat.gif.unavailable` | GIFs are unavailable right now |  |
| `chat.record.bar_video` | Recording a video message |  |
| `chat.record.bar_voice` | Recording a voice message |  |
| `chat.record.cancelled` | Cancelled |  |
| `chat.record.delete` | Delete recording |  |
| `chat.record.denied_camera` | Lakko needs camera access to record video messages. Allow camera access in your browser's site settings. |  |
| `chat.record.denied_microphone` | Lakko needs microphone access to record voice messages. Allow microphone access in your browser's site settings. |  |
| `chat.record.finishing` | Finishing recording |  |
| `chat.record.flip_camera` | Flip camera |  |
| `chat.record.hint_hold` | Hold to record |  |
| `chat.record.in_call` | Unavailable during a call |  |
| `chat.record.no_camera` | No camera found |  |
| `chat.record.pause` | Pause recording |  |
| `chat.record.pause_video` | Pause video message |  |
| `chat.record.play` | Play recording, {duration} |  |
| `chat.record.play_video_sound` | Play video message with sound |  |
| `chat.record.silent_mic` | No sound from the microphone |  |
| `chat.record.slide_cancel` | ‹ Slide to cancel |  |
| `chat.record.start_failed` | Couldn't start recording |  |
| `chat.record.starting` | Starting… |  |
| `chat.record.stop` | Stop recording |  |
| `chat.record.stop_short` | Stop |  |
| `chat.record.switch_to_video` | Switch to video message |  |
| `chat.record.switch_to_voice` | Switch to voice message |  |
| `chat.record.unsupported_video` | Video messages can't be recorded in this browser |  |
| `chat.record.unsupported_voice` | Voice messages can't be recorded in this browser |  |
| `chat.record.video` | Record video message |  |
| `chat.record.video_ready` | Video message ready |  |
| `chat.record.video_tooltip` | Record video message: hold to record, tap to switch to voice |  |
| `chat.record.voice` | Record voice message |  |
| `chat.record.voice_tooltip` | Record voice message: hold to record, tap to switch to video |  |

## Group info: name, members, join requests, invite link

| Key | English | security_critical |
|---|---|---|
| `chat.delete.body` | This removes the conversation and its messages from this device. There is no copy anywhere else and this cannot be undone. | yes |
| `chat.delete.confirm_named` | Delete {name} |  |
| `chat.delete.control` | Delete this chat |  |
| `chat.delete.title` | Delete this chat? |  |
| `chat.group_name.error.character` | The name contains a character that cannot be shown. |  |
| `chat.group_name.error.generic` | That name cannot be used. |  |
| `chat.group_name.error.line_break` | The name must be one line. |  |
| `chat.group_name.error.save` | Could not rename the group. Try again. |  |
| `chat.group_name.error.too_long` | The name is too long. |  |
| `chat.group_name.label` | Group name |  |
| `chat.group_name.only_admins` | Only admins can rename this group |  |
| `chat.invite_link.cleared` | Link cleared (an inline link has nothing to revoke). |  |
| `chat.invite_link.copy` | Copy link |  |
| `chat.invite_link.create` | Create invite link |  |
| `chat.invite_link.label` | Invite link |  |
| `chat.invite_link.revoke` | Revoke |  |
| `chat.invite_link.revoked` | Invite link revoked. |  |
| `chat.invite_link.share` | Share |  |
| `chat.leave.action` | Leave group |  |
| `chat.leave.body` | You will keep receiving messages until an owner or an admin applies your request. You will need a new invite to rejoin. | yes |
| `chat.leave.confirm` | Leave |  |
| `chat.leave.confirm_named` | Leave {name} |  |
| `chat.leave.requested` | Leave requested. It applies when an owner or an admin applies the change. |  |
| `chat.leave.title` | Leave this group? |  |
| `chat.members.add.action` | Add member |  |
| `chat.members.add.done` | {name} added. |  |
| `chat.members.add.error.no_bundle` | This person has not finished setting up Lakko yet. Try again later. |  |
| `chat.members.add.error.some_failed` | Could not add {names}. Add them from the members panel. |  |
| `chat.members.add.error.unknown_username` | No account is registered as {name}. |  |
| `chat.members.add.placeholder` | @username |  |
| `chat.members.add.requested` | {name} requested. They join when an owner or an admin applies the change. |  |
| `chat.members.add.submit` | Add |  |
| `chat.members.admin.demoted` | Removed as admin. |  |
| `chat.members.admin.make` | Make admin |  |
| `chat.members.admin.marker` | Admin |  |
| `chat.members.admin.promoted` | Promoted to admin. |  |
| `chat.members.admin.remove` | Remove admin |  |
| `chat.members.admin.requested` | Change requested. It applies when an owner or an admin applies the change. |  |
| `chat.members.devices` | {count, plural, one {# device} other {# devices}} |  |
| `chat.members.frozen` | This list stopped updating when your membership ended. |  |
| `chat.members.heading` | Members ({count}) |  |
| `chat.members.join.approve` | Approve |  |
| `chat.members.join.approved_added` | Join approved. They have been added to the group. |  |
| `chat.members.join.approved_requested` | Join approved. They join when an owner or an admin applies the change. |  |
| `chat.members.join.heading` | Join requests ({count}) |  |
| `chat.members.join.link_unknown` | Invite link not known |  |
| `chat.members.join.reject` | Reject |  |
| `chat.members.join.rejected` | Join request rejected. |  |
| `chat.members.join.via_former` | via {name} · not a current member | yes |
| `chat.members.join.via_member` | via {name} · member |  |
| `chat.members.pending.adds_heading` | Waiting to be added ({count}) |  |
| `chat.members.pending.count` | {count, plural, one {# membership change is waiting to apply.} other {# membership changes are waiting to apply.}} |  |
| `chat.members.pending.liveness` | Membership changes are applied by the group owner or an admin. They will apply automatically when one does. |  |
| `chat.members.pending.queue` | {count, plural, one {# change is now waiting to apply.} other {# changes are now waiting to apply.}} |  |
| `chat.members.pending.waiting_added` | Waiting to be added. |  |
| `chat.members.pending.waiting_leave` | Waiting to leave. |  |
| `chat.members.pending.waiting_removed` | Waiting to be removed. |  |
| `chat.members.remove.body` | They will keep receiving messages until an owner or an admin applies the change. | yes |
| `chat.members.remove.confirm_named` | Remove {name} |  |
| `chat.members.remove.done` | Member removed. |  |
| `chat.members.remove.requested` | Removal requested. It applies when an owner or an admin applies the change. | yes |
| `chat.members.remove.requested_short` | Removal requested. |  |
| `chat.members.remove.title` | Remove this member? |  |
| `chat.members.row_label` | {name}, {state} |  |
| `chat.members.title` | Members |  |
| `chat.members.unapplied.add_line` | Not added. You can add them again. |  |
| `chat.members.unapplied.explanation` | These changes did not apply and will not be retried on their own. |  |
| `chat.members.unapplied.heading` | Not applied ({count}) |  |
| `chat.members.unapplied.join_line` | Not applied. Ask them to request to join again. |  |
| `chat.members.unnamed` | Member |  |
| `chat.transfer.action` | Transfer ownership |  |
| `chat.transfer.action_named` | Make {name} the owner |  |
| `chat.transfer.become_member` | Become a regular member |  |
| `chat.transfer.become_member_detail` | You give up every group power immediately. Only the new owner can give any of it back. |  |
| `chat.transfer.candidate_admin` | {name}, admin |  |
| `chat.transfer.done_admin` | {name} is now the group owner. You are still an admin. |  |
| `chat.transfer.done_member` | {name} is now the group owner. You are now a regular member. |  |
| `chat.transfer.irreversible_body` | Only an owner can change roles. Once you transfer this group you cannot take it back, you cannot restore your own owner role, and you cannot change your own role again unless the new owner does it for you. | yes |
| `chat.transfer.irreversible_title` | This cannot be undone. | yes |
| `chat.transfer.leave_blocked` | Transfer ownership first — a group cannot be left without an owner. |  |
| `chat.transfer.new_owner` | You are now the owner of this group. |  |
| `chat.transfer.new_owner_body` | You can add and remove members, change admins, and transfer ownership to someone else. |  |
| `chat.transfer.new_owner_legend` | New owner |  |
| `chat.transfer.new_owner_named` | {name} made you the owner of this group. |  |
| `chat.transfer.no_candidates` | There is no one else in this group to transfer it to. |  |
| `chat.transfer.pending` | Ownership transfer waiting to apply. |  |
| `chat.transfer.pending_named` | Waiting to make {name} the owner. |  |
| `chat.transfer.requested` | Ownership transfer requested. It applies when an owner or an admin applies the change. |  |
| `chat.transfer.role_change_pending` | A role change is already waiting to apply. Wait for it to land, then try again. |  |
| `chat.transfer.role_legend` | Your role afterwards |  |
| `chat.transfer.stay_admin` | Stay an admin |  |
| `chat.transfer.stay_admin_detail` | You can still add and remove members. You can give this up later by asking the new owner. |  |
| `chat.transfer.unlocks_leave` | Once the change is applied, you can leave the group like any other member. |  |

## Membership refusals

| Key | English | security_critical |
|---|---|---|
| `chat.refusal.add_not_admin` | Only the group owner or an admin can add members. |  |
| `chat.refusal.device_binding` | Something is wrong with this device's key. Remove it from Devices and add it again. |  |
| `chat.refusal.device_limit` | {count, plural, one {Up to # device per account. Remove one first.} other {Up to # devices per account. Remove one first.}} |  |
| `chat.refusal.fallback` | This change could not be requested. |  |
| `chat.refusal.join_not_kept` | A join request arrived while too many were already waiting, so it was not kept. Ask the person to send it again later. |  |
| `chat.refusal.joiner_member` | That person has already joined this group. |  |
| `chat.refusal.no_join_request` | That join request is no longer waiting. |  |
| `chat.refusal.not_owner` | Only the group owner can change admins or transfer ownership. |  |
| `chat.refusal.owner_cannot_leave` | The group owner cannot leave the group. |  |
| `chat.refusal.owner_not_member` | Ownership can only be held by a current member of this group. |  |
| `chat.refusal.owner_unremovable` | The group owner cannot be removed. |  |
| `chat.refusal.pending_full` | Too many changes are already waiting to apply. |  |
| `chat.refusal.remove_not_admin` | Only the group owner or an admin can remove members. |  |
| `chat.refusal.retry_many` | Wait for them to land, then try again. |  |
| `chat.refusal.retry_one` | Wait for it to land, then try again. |  |
| `chat.refusal.role_change_pending` | A role change is already waiting to apply. |  |
| `chat.refusal.with_retry` | {fact} {retry} |  |

## Ended membership

| Key | English | security_critical |
|---|---|---|
| `chat.ended.composer` | You can no longer send messages to this group. |  |
| `chat.ended.consequence` | The messages you already received are still here. No new ones will arrive. |  |
| `chat.ended.left` | You left this group. |  |
| `chat.ended.no_longer_member` | You are no longer a member of this group. |  |
| `chat.ended.unsent_draft` | Unsent draft |  |

## Errors

| Key | English | security_critical |
|---|---|---|
| `chat.error.add_member` | Could not add this person. Try again. |  |
| `chat.error.approve_join` | Could not approve the join request. Try again. |  |
| `chat.error.change_admin` | Could not change the admin role. Try again. |  |
| `chat.error.conversation_gone` | That conversation is no longer here. |  |
| `chat.error.create_link` | Could not create an invite link. Try again. |  |
| `chat.error.delete_chat` | Could not delete this chat. Try again. |  |
| `chat.error.delete_message` | Could not delete the message. Try again. |  |
| `chat.error.edit` | Could not edit the message. Try again. |  |
| `chat.error.leave` | Could not leave the group. Try again. |  |
| `chat.error.membership_request` | A membership change you asked for could not be sent again. |  |
| `chat.error.message_gone` | That message is no longer here. |  |
| `chat.error.pin` | Could not change the pin. Try again. |  |
| `chat.error.react` | Could not send the reaction. Try again. |  |
| `chat.error.refresh` | Could not load new messages. Try again. |  |
| `chat.error.reject_join` | Could not reject the join request. Try again. |  |
| `chat.error.remove_member` | Could not remove this member. Try again. |  |
| `chat.error.revoke_link` | Could not revoke the invite link. Try again. |  |
| `chat.error.transfer` | Could not transfer ownership. Try again. |  |

## Shared with other screens

| Key | English | security_critical |
|---|---|---|
| `ds.clock_skew` | Check this device's date and time. |  |
| `group.join.not_taking_requests` | This group isn't taking requests yet. Try again later. |  |
| `group.join.request_again` | Request again |  |
| `group.join.wait_primary` | Request sent. An admin approves it, then the change is applied to the group. |  |
| `group.join.wait_secondary` | Neither step has a time limit. If nothing happens, you can send the request again. |  |
| `tokens.action_allowance_used` | You have used today's allowance for this. Try again tomorrow. |  |
| `tokens.lookup_allowance_used` | You have looked up a lot of people today. Try again tomorrow. |  |

## iOS additions

Keys the iOS client needed that web did not have. Named by meaning, so Android can reuse them. None is `security_critical`.

| Key | English | security_critical |
|---|---|---|
| `chat.attachment.error.open_photo` | Couldn't open this photo. Try again. |  |
| `chat.attachment.error.open_video` | Couldn't open this video. Try again. |  |
| `chat.attachment.error.prepare_video` | Couldn't prepare this video. Videos up to {limit} can be sent. |  |
| `chat.composer.photo_library` | Photo Library |  |
| `chat.edit.title` | Edit message |  |
| `chat.group_name.empty` | No name yet |  |
| `chat.media.audio` | Audio |  |
| `chat.media.open_full_hint` | Opens it full screen |  |
| `chat.members.count` | {count, plural, one {# member} other {# members}} |  |
| `chat.message.media_fallback` | Media message |  |
| `chat.photo.viewer_hint` | Pinch to zoom. Swipe down to close. |  |
| `chat.pinned_bar.hint` | Shows the message |  |
| `chat.reaction.reacted_by_title` | Reacted by |  |
| `chat.record.denied_camera_app` | Lakko needs camera access to record video messages |  |
| `chat.record.denied_microphone_app` | Lakko needs microphone access to record voice messages |  |
| `chat.record.locked_hint` | Starts a locked recording |  |
| `chat.record.open_settings` | Open Settings |  |
| `chat.record.recording_duration` | Recording, {duration} |  |
| `chat.record.send` | Send recording |  |
| `chat.select.state_not_selected` | Not selected |  |
| `chat.select.state_selected` | Selected |  |
| `chat.timeline.setting_up` | This conversation is still being set up on this device. |  |
| `chat.video_note.loading` | Video message, loading |  |
| `chat.video_note.tap_mute` | Tap to mute |  |
| `chats.row.no_messages` | No messages yet |  |

Where the iOS wording differed from an existing key, the catalog wording is used: the empty-list button reads "Start one", the create menu's accessible name "Create", the name-lookup miss drops "Check the spelling.", a second attachment reads "Photo and 1 more" rather than "Photo +1", typing lines end in "..." rather than "…", the group-call banner says "Starting…" for an empty call, the title button's hint names the destination ("Contact info") rather than "Opens contact info", the pin sheet's title is "Pin message" / "Unpin message", both shared-pin rows say "Every member sees it", the slide-to-cancel line carries its own glyph (the separate chevron image is gone), and a token outage reads `common.error.offline`. "Send again" on a failed bubble became `common.retry`; a self-removal attempt, which the UI never offers, answers `chat.refusal.fallback`.

On iOS a failed action's message now renders from its kind through `ChatViewModel.errorText(for:fallbackKey:)` (refusal reason, clock skew, used-up allowance, else the action's `chat.error.*` key); `sendError`, `membershipError` and `membershipNotice` are `UiText`, so a message on screen follows a live language switch. The add-member form maps `ContactsError.usernameUnknown` and `.noIdentityBundle` to `chat.members.add.error.unknown_username` / `no_bundle`. Attachment failures render from `AttachmentBridgeError.userText(fileName:)`.

Not moved on iOS in this batch: `NewGroupScreen` (the create-group flow and the notice it hands the chat), `JoinGroupLandingScreen` and `GroupInvitePage` (the `/g/` landing), the join success copy in `DsProof.swift`, and the in-call surfaces (`QuietIncomingCall*`, call history rows, `CallTranscriptRow`), which go with the calls batch. The playback-speed chip (`1.5x`) stays a formatted number.

## Android additions

Keys the Android client needed that neither web nor iOS had. Named by meaning, so iOS can reuse them. None is `security_critical`. `chats.new_chat.*` and `chats.new_group.*` are the New chat and New group screens the list's create menu opens (not moved on iOS in this batch; iOS can take them when it does). Android reuses the iOS additions `chats.row.no_messages`, `chat.edit.title`, `chat.group_name.empty`, `chat.members.count` (the group header's second line), `chat.timeline.setting_up`, `chat.select.state_selected` / `chat.select.state_not_selected`, `chat.record.denied_microphone_app` / `chat.record.denied_camera_app` and `chat.record.open_settings`.

| Key | English | security_critical |
|---|---|---|
| `chat.attachment.error.attach` | Could not attach the selected item. Try again. |  |
| `chat.attachment.error.attach_named` | Could not attach "{name}". Try again. |  |
| `chat.attachment.save_title` | Save attachment |  |
| `chat.call.busy` | Another call is in progress. |  |
| `chat.call.unavailable` | Group calls are not available right now. |  |
| `chat.composer.attach_media` | Photo or video |  |
| `chat.emoji.choose` | Choose |  |
| `chat.emoji.insert` | Insert |  |
| `chat.media.row_not_sent` | {label}, not sent |  |
| `chat.media.row_sending` | {label}, sending |  |
| `chat.media.with_duration` | {label}, {duration} |  |
| `chat.message.close_actions` | Close message actions |  |
| `chat.photo.open` | Open photo |  |
| `chat.photo.viewer` | Photo viewer |  |
| `chat.record.flip_failed` | Couldn't switch camera |  |
| `chat.record.position` | Recording position |  |
| `chat.record.unsupported_video_device` | Video messages can't be recorded on this device |  |
| `chat.upload.preparing` | Preparing "{name}"… |  |
| `chat.upload.selected_item` | Selected item |  |
| `chat.video.open` | Play video |  |
| `chat.video.play_failed` | Couldn't play this video |  |
| `chat.video.player` | Video player |  |
| `chat.video_note.poster` | Video message poster |  |
| `chat.voice.change_speed` | Change playback speed |  |
| `chats.new_chat.add_contact` | Add a contact |  |
| `chats.new_chat.no_contacts` | No contacts yet |  |
| `chats.new_chat.no_contacts_body` | A chat starts from someone you know. |  |
| `chats.new_chat.not_available` | Chat not available on this device yet |  |
| `chats.new_group.accept_again` | Accept this contact again to add them to a group. |  |
| `chats.new_group.creating` | Creating... |  |
| `chats.new_group.error` | Could not create the group. Try again. |  |
| `chats.new_group.name_label` | Group name (optional) |  |
| `chats.new_group.no_contacts` | A group starts from people you already know. |  |
| `chats.new_group.open_contacts` | Go to Contacts |  |
| `chats.new_group.title` | New group |  |
| `chats.search.clear` | Clear search |  |
| `common.not_now` | Not now |  |
| `common.ok` | OK |  |

Where the Android wording differed from an existing key, the catalog wording is used: the typing lines end in "...", the edited marker reads "(edited)", the tombstone "Message deleted", a second attachment in a row preview reads "Photo and 1 more", the group-call banner says "Starting…" for an empty call, the lookup row's busy label is "Looking up...", the attach button is "Attach files", the slide-to-cancel line carries its own glyph, the size reads "kB" with the unit from `common.size.*`, the locked recording's discard control is "Delete recording", an attachment that could not be added names the kind of failure only through the catalog (`chat.attachment.error.too_large_named` / `upload` / `attach_named` / `attach`), a token outage reads `common.error.offline`, and the members heading, the join-request heading and the waiting-to-be-added heading use `chat.members.heading` / `chat.members.join.heading` / `chat.members.pending.adds_heading`. "Send again" on a failed bubble became `common.retry` (as on iOS); a self-removal attempt answers `chat.refusal.fallback`. The selection mark reads `{label}, Selected` through `common.list_pair` with `chat.select.state_selected`, so no new key was needed.

On Android the chat view model holds `sendError`, `membershipError`, `membershipNotice`, `groupNameError`, the typing line and the bulk-selection status as `UiText` (`l10n/UiText.kt`, which gained `parts`: other catalog messages rendered first in the same language, for `chat.refusal.with_retry`, `common.sentence_pair` and nested `common.list_pair`). A failed action renders from its kind through `chat/ChatErrorText.kt` `chatErrorText` (typed refusal reason, a `ChatError` that carries its own `userText`, used-up allowance, token outage, clock skew, else the action's `chat.error.*` key); core and server prose goes to the log only. The rename error maps the core's display-name refusals to `chat.group_name.error.*`. Dates, clock times and list times use the interface language (`Locale.forLanguageTag` of the current language), not the system locale.

Not moved on Android in this batch: the `/g/` landing (`JoinGroupLandingScreen`, `GroupInvitePage`, `LinkJoinKnock` copy) apart from its three wait lines, which already render `group.join.wait_primary` / `wait_secondary` / `request_again` because their constants live with the unapplied-requests copy, the in-call surfaces and call rows (`calls/*`, `ui/call/*`, `callRowText`, `callBusyMessage`, the group-call start outcomes, which still reach the chat's call notice as English), the Contacts screen's `sendError` (it is not on these screens), and the channel row's second line in the merged list.
