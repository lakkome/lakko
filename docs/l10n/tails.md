<!--
SPDX-License-Identifier: AGPL-3.0-or-later
SPDX-FileCopyrightText: 2026 Lakko Contributors
-->

# Localization tails after batches 1-5

ADR-0061 S5. The strings the five batches left in English, or held as text that did not follow a language switch.

## Moved

- Message notifications: "New message" and the attachment line (`notification.message.new`, `notification.message.attachments`) on web (service worker) and iOS (notification extension). Neither process can run plural rules, so the app stores the attachment line already rendered per count (web 1-99, iOS 1-50); a larger count falls back to English. The iOS extension also replaces the server's English "New message" / "Incoming call" placeholders before any decrypt.
- iOS call errors, notices, the call-chat system lines and the Calls-screen alerts are held as `UiText` and render at display time. The call-peer name mirror in the App Group is rewritten on a language switch.
- iOS "Member" / "Someone" placeholders read `chat.members.unnamed` / `contacts.request.someone`.
- The update-available banner on iOS and Android, the web update banner, the web not-found page, the Android language screen's Back, the Android avatar's group glyph test (it compared against English "Group").
- Android group invite link: the caption (`chat.invite_link.label`, a heading), Share (`chat.invite_link.share`, the system share sheet) beside Copy and Revoke, and after Revoke `chat.invite_link.revoked` for a ledger link or `chat.invite_link.cleared` for an inline one (the web rule; inline links remain as the censored-region fallback). Android channel details: `channels.meta.saved` after a save, cleared by the next edit or save. Android folder editor: `folders.editor.error.save` when a save is refused, cleared when the next save starts.
- iOS, the same three: the group invite link in the group info sheet gains Share (`ShareLink`) and its caption as a VoiceOver heading, and after Revoke shows `chat.invite_link.revoked` or `chat.invite_link.cleared` from the repository's revoked/not-revoked result (`ChatViewModel.inviteLinkRevokeNotice`); the channel name panel shows `channels.meta.saved` after a save, cleared by the next save or an edit; the folder editor stays open with `folders.editor.error.save` when the publish throws (it used to close and drop the error), cleared when the next save starts, and a retry of a new folder reuses its id.
- Web stand-in labels ("Direct chat", "Group") are named constants: markers that are always stored in English and never displayed, since every display path titles them through `groupTitle`. iOS and Android store none.

## Keys added

| Key | English | Clients |
|---|---|---|
| `app.not_found.title` | Page not found | web |
| `app.not_found.body` | There is nothing at this address. | web |
| `app.update.ready` | A new version of Lakko is ready. | web |
| `app.update.reload` | Reload | web |
| `app.update.reloading` | Reloading... | web |
| `nav.rail_a11y` | Primary | web |
| `update.available.message` | A new version of Lakko is available. | iOS, Android |
| `update.available.update` | Update | iOS, Android |
| `update.available.opens_app_store_hint` | Opens the App Store | iOS |
| `update.available.update_play_a11y` | Update, opens the Play Store | Android |
| `display_name.error.signed_out` | Sign in to set your name. | iOS |

None is `security_critical`.

## Web, after the remainder pass

- Restore identity checks (`SupersessionPhraseMismatch`, `IdentityVerificationUnavailable`) render from their kind wherever they surface, through `screenText`: the three-word form and the recovery forms used to pass the English `message` through. The mismatch error carries its `source` (phrase or another device), replacing the English `mismatchMessage` argument.
- The typed-name check of a device restore (`TypedNameOtherAccount`, `TypedNameCheckUnavailable`) is keyed: `device_link.error.name_other_account` and `common.error.offline`.

## Android, after the remainder pass

- Newer-wire-version messages: `MessageBodyKind.UNSUPPORTED` becomes a timeline row carrying `unsupportedVersion` (persisted in the message log, empty text); the bubble, the conversation-row preview, the selection label and the background notification (`ProcessedKind.UNSUPPORTED`) draw `chat.message.unsupported`. Such a row has no action menu (nothing to copy, edit, quote or forward). The unused `AttachmentBridge.parseEnvelopeBytes` wrapper, the only Android path to the core's "[unsupported vN message]" text, is removed.
- A join answered for a different account throws `JoinWrongAccount` and renders `device_link.error.wrong_account` on the restore panel and the code panel.
- The contacts inbox error is held as a kind (`InboxError`): a poll failure renders `contacts.mailbox.poll_failed`, a Welcome that could not be joined renders the new `contacts.mailbox.chat_join_failed` (it used to read as the poll failure). `contacts.mailbox.join_failed` says "added to a group" and does not fit a one-to-one chat, so it was not reused.

## Still not on screen

- "[unsupported vN message]" comes from the Rust core as bubble text. Closed on web and Android (iOS is its own pass): `parseBodyEnvelope` returns `{kind: "unsupported", version}`, the timeline keeps the message as a row with `unsupportedVersion` (persisted) and draws `chat.message.unsupported` in the bubble and the chat list preview, and the service worker's notification body reads the same key from the stored words (`unsupportedMessage`). The legacy `parseEnvelope` (no production caller on web) returns empty text instead of the placeholder.
- iOS Info.plist permission strings (need `InfoPlist.strings`).
- A reply or forward author snapshot of "You" / "Member" travels in the sender's interface language.
- Clock digits stay Western on every language.
