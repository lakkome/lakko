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

## Still not on screen

- "[unsupported vN message]" comes from the Rust core as bubble text on every client.
- Android: a join answered for a different account shows the generic `device_link.error.generic`; there is no key for that case.
- iOS Info.plist permission strings (need `InfoPlist.strings`).
- A reply or forward author snapshot of "You" / "Member" travels in the sender's interface language.
- Clock digits stay Western on every language.
