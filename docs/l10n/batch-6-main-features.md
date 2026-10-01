<!--
SPDX-License-Identifier: AGPL-3.0-or-later
SPDX-FileCopyrightText: 2026 Lakko Contributors
-->

# Localization batch 6: the features `main` brought in

ADR-0061 S5, batch 6. The merge of `main` (`0cc7a7d3`) brought web screens whose text was still English literals: chat folders (ADR-0058, `docs/ux/chat-folders.md`: the folder bar, the row menu's Add to folder, the tab menu, Settings > Chat folders, the folder editor and its chat picker), Who can find me and scoped discovery (ADR-0060, `docs/ux/who-can-find-me.md`: Settings > Privacy and its list of groups and channels, the Members can find me switch in group and channel info, the line at signup, Search people in the Chats search, the channel member list), the code-typing device link (ADR-0049 section 2 as amended 2026-09-28: the "New sign-in request" prompt on the signed-in device, the code field on the new device, the restore and recovery intros), the "This device was removed" screen, the invite-link list's links made on another device, the username-change limit and the "still starting up" refusal. They moved into `l10n/catalog/en.json`. The web is the reference for this batch: the iOS and Android clients map their own literals of the same screens onto these keys; names follow meaning, not the web component.

132 keys added and 2 existing keys given new English (134 entries in all), 33 of them `security_critical` (they render in English until a translation is `reviewed`, and a signed update can never change them).

## Conventions

The conventions of batches 1-5 (`docs/l10n/batch-1-auth.md` to `batch-5-calls.md`) and of `tails.md` hold. In addition:

- A module's copy table that used to hold strings now holds functions rendered on every read, the shape `ChatList.tsx` already used: `COPY` in `settings/who-can-find-me.ts`, `REMOVED_DEVICE_COPY` in `devices/sync/removed-device.ts`, `SEARCH_NOT_FOUND` / `SEARCH_FAILED` / `REQUEST_REFUSED` in `search/search-state.ts`, and every `LINK_*` constant of `devices/device-link.ts` (type `LinkText = () => string`). The device-link states (`limited` / `closed` messages, the `ready` error, `initiatorEndMessage`, the panel's `link-ended` state and request note) hold a `LinkText`, so a line already on screen follows a language switch; so does Search people's note under a person (`SendOutcome.failed.message` is a renderer, built with `contactsErrorText`).
- A folder suggestion's name and description are functions (`Suggestion.name()` / `.description()`); adding one stores the name in the interface language of that moment as the folder's own name, which from then on is the person's text and is never translated (it syncs to their other devices as typed).
- Limits are arguments of plural messages, never digits in the text: `folders.limit` (`MAX_FOLDERS`), `folders.pin_limit` (`MAX_FOLDER_PINS`), `folders.picker.limit_include` / `_exclude` (`MAX_INCLUDED_BY_HAND` / `MAX_EXCLUDED_BY_HAND`).
- A sentence with a link inside is one message with the link as an element argument (`tRich`): `who_can_find_me.members_excluded_notice` with `{privacy}` (the link reads `settings.section.privacy`). Its plain-text form, for the tests, is `COPY.excludedNotice()`.
- The two sentences under Who can find me are joined through `common.sentence_pair`.
- The username-change limit is one whole sentence per case (`username.error.bind_limited_minutes` with a plural `{minutes}`, `username.error.bind_limited_later`), not a lead sentence and a slotted tail.
- The not-found answer of a username lookup in Contacts is now `search.people.not_found` (main replaced batch 4's `chat.members.add.error.unknown_username` there): it reads the same whether the account does not exist or does not let the searcher find it (ADR-0060).
- `Settings.tsx` sections all carry a `labelKey`; the literal-label variant the merge added is gone.

## Reused keys

These existing keys are used: `common.back`, `common.cancel`, `common.checking`, `common.continue`, `common.delete`, `common.done`, `common.loading`, `common.off`, `common.on`, `common.open`, `common.open_lakko`, `common.remove`, `common.retry`, `common.save`, `common.sending`, `common.sentence_pair`, `common.try_again`, `chats.search.label`, `chats.search.placeholder`, `chats.search.no_results`, `contacts.add.intro_label`, `contacts.send.done`, `device_link.request.title`, `device_link.request.decline`, `device_link.request.adding`, `device_link.request.error.generic`, `device_link.request.declined` ("Declined. If this keeps happening, change your password."), `device_link.request.added` ("Device added."), `device_link.error.declined` ("Declined on another device."), `device_link.show_code`, `device_link.qr.show`, `device_link.qr.scan_hint`.

The last three `device_link.*` texts were already in the catalog with the exact wording `main` wrote as literals, so the `LINK_DECLINED_OLD`, `LINK_DEVICE_ADDED` and `LINK_DECLINED_NEW` constants render those keys.

## Changed keys

`restore_identity.device_intro` and `recovery.device.intro` described the old flow ("confirm it there and compare the code"); `main` replaced them with literals describing the new one (the signed-in device shows a code, the person types it here). Their English now carries `main`'s wording and they are rendered again; both were removed from every `l10n/catalog/<lang>.json` so they are retranslated. Both stay `security_critical`.

## Keys of the old compare-a-code flow now unused

No client renders these any more (checked by searching the web, iOS and Android sources for the quoted key): `device_link.compare_hint`, `device_link.comparison_code`, `device_link.confirm_there`, `device_link.qr.hide`, `device_link.request.body`, `device_link.request.code_label`, `device_link.request.confirm`, `device_link.request.confirm_only_if`. They were deleted from the catalog and from every language file at the end of this batch.

## Web-only keys

- `folders.settings.drag_hint`, `folders.settings.move_up`, `folders.settings.move_down`: the web reorders folders with a drag handle plus Move up / Move down for keyboard users; the natives reorder natively.
- `device_link.code.intro`: the web wording of the new device's intro (UX section 10 marks it a web divergence).
- `channels.members.back_a11y`: the web member list is a panel inside channel info with its own back button.

Every other key applies to all three clients.

## Left English, and why

- The code field's placeholder `47 19 03` is sample digits, not a message; the code itself is digits on every language.
- The `https://lakko.me/g/...` and `https://lakko.me/c/…` placeholders are example addresses.
- `JOINED_WRONG_ACCOUNT` (`devices/orchestrator.ts`) and the identity-check sentences thrown in `auth/AuthContext.tsx` predate this merge; they reach a screen through `screenText`'s legacy English-sentence path. `tails.md` already records the missing key for the wrong-account join.
- `"Joined group (Welcome N)"` in `contacts/MailboxContext.tsx` is a stand-in label the title resolver reads as unnamed; it is never displayed.
- The channel member list builds its rows once per load, so the "You" / "Member" stand-ins (`channels.members.you`, `channels.members.unnamed`) follow a language switch at the next load of the list, not in place.
- A folder created from a suggestion keeps the name it was created with (see Conventions).

## Chat folders (ADR-0058, `docs/ux/chat-folders.md`)

| Key | English | security_critical |
|---|---|---|
| `folders.add_to_folder` | Add to folder |  |
| `folders.all_chats` | All chats |  |
| `folders.bar_a11y` | Folders |  |
| `folders.chat_count` | {count, plural, one {# chat} other {# chats}} |  |
| `folders.delete` | Delete folder |  |
| `folders.delete_confirm.body` | The chats in it stay in All chats. |  |
| `folders.delete_confirm.title` | Delete folder "{name}"? |  |
| `folders.edit` | Edit folder |  |
| `folders.edit_all` | Edit folders |  |
| `folders.editor.add_chats` | Add chats |  |
| `folders.editor.discard.confirm` | Discard |  |
| `folders.editor.discard.keep` | Keep editing |  |
| `folders.editor.discard.title` | Discard changes? |  |
| `folders.editor.error.save` | The folder could not be saved. Try again. |  |
| `folders.editor.exclude.archived` | Archived |  |
| `folders.editor.exclude.muted` | Muted |  |
| `folders.editor.exclude.read` | Read |  |
| `folders.editor.exclude_chats` | Exclude chats |  |
| `folders.editor.excluded` | Excluded |  |
| `folders.editor.included` | Included |  |
| `folders.editor.kind.channels` | Channels |  |
| `folders.editor.kind.groups` | Groups |  |
| `folders.editor.kind.one_to_one` | One-to-one chats |  |
| `folders.editor.name_placeholder` | Folder name |  |
| `folders.editor.remove_chat_a11y` | Remove {name} |  |
| `folders.empty` | Nothing here right now |  |
| `folders.limit` | You can have up to {count, plural, one {# folder} other {# folders}}. |  |
| `folders.mark_all_read` | Mark all as read |  |
| `folders.new` | New folder |  |
| `folders.new_ellipsis` | New folder... |  |
| `folders.picker.limit_exclude` | A folder can hold up to {count, plural, one {# chat} other {# chats}} excluded one by one. Use the kind toggles for more. |  |
| `folders.picker.limit_include` | A folder can hold up to {count, plural, one {# chat} other {# chats}} added one by one. Use the kind toggles for more. |  |
| `folders.pin_limit` | You can pin up to {count, plural, one {# chat} other {# chats}} in a folder. |  |
| `folders.remove_from_folder` | Remove from folder |  |
| `folders.reorder` | Reorder folders |  |
| `folders.settings.drag_hint` | Drag to reorder |  |
| `folders.settings.intro` | Group your chats the way you think about them. Folders appear on all your devices. |  |
| `folders.settings.more_a11y` | More for {name} |  |
| `folders.settings.move_down` | Move down |  |
| `folders.settings.move_up` | Move up |  |
| `folders.settings.pending_sync` | Changes will reach your other devices when this device is back online. |  |
| `folders.settings.yours` | Your folders |  |
| `folders.suggested.add` | Add |  |
| `folders.suggested.add_a11y` | Add {name} |  |
| `folders.suggested.channels.description` | Channels you follow |  |
| `folders.suggested.channels.name` | Channels |  |
| `folders.suggested.groups.description` | Group chats |  |
| `folders.suggested.groups.name` | Groups |  |
| `folders.suggested.personal.description` | One-to-one chats |  |
| `folders.suggested.personal.name` | Personal |  |
| `folders.suggested.title` | Suggested |  |
| `folders.suggested.unread.description` | Chats with new messages |  |
| `folders.suggested.unread.name` | Unread |  |
| `folders.tab_a11y` | {name}, {count, plural, one {# unread chat} other {# unread chats}} |  |
| `settings.section.folders` | Chat folders |  |

## Who can find me (ADR-0060, `docs/ux/who-can-find-me.md`)

| Key | English | security_critical |
|---|---|---|
| `settings.section.privacy` | Privacy |  |
| `who_can_find_me.applying_a11y` | Applying |  |
| `who_can_find_me.contacts_of_contacts` | Contacts of my contacts | yes |
| `who_can_find_me.contacts_of_contacts_helper` | People your contacts have added. | yes |
| `who_can_find_me.contacts_of_contacts_spreading` | Contacts of your contacts can find you once your contacts next open Lakko. | yes |
| `who_can_find_me.groups` | Members of my groups and channels | yes |
| `who_can_find_me.groups.all` | All groups and channels | yes |
| `who_can_find_me.groups.chosen` | Only the ones I choose | yes |
| `who_can_find_me.groups_all_helper` | Everyone in the groups and channels you're in. | yes |
| `who_can_find_me.groups_chosen_helper` | Only the ones you choose ({chosen} of {total}). | yes |
| `who_can_find_me.lede` | Whoever can find you by username or name can ask to be your contact. | yes |
| `who_can_find_me.lede_link` | People with your invite link or QR code can always reach you. | yes |
| `who_can_find_me.list.channels` | Channels |  |
| `who_can_find_me.list.empty` | You're not in any groups or channels yet. |  |
| `who_can_find_me.list.groups` | Groups |  |
| `who_can_find_me.list.no_match` | No groups or channels match. |  |
| `who_can_find_me.list.search` | Search groups and channels |  |
| `who_can_find_me.members_can_find_me` | Members can find me | yes |
| `who_can_find_me.members_excluded_notice` | Now only the groups and channels you choose. Change in {privacy}. | yes |
| `who_can_find_me.mode.everyone` | Everyone | yes |
| `who_can_find_me.mode.everyone_helper` | Anyone can find you by your username or name. | yes |
| `who_can_find_me.mode.nobody` | Nobody | yes |
| `who_can_find_me.mode.nobody_helper` | Only people with your invite link or QR code can reach you. | yes |
| `who_can_find_me.mode.some` | Some people | yes |
| `who_can_find_me.signup_after_create` | It opens once your account is created. |  |
| `who_can_find_me.signup_change` | Change |  |
| `who_can_find_me.signup_line` | Contacts of your contacts and people in your groups and channels can find you. You can change this in Settings > Privacy. | yes |
| `who_can_find_me.status.failed` | Couldn't update who can find you. Try again. | yes |
| `who_can_find_me.status.offline` | Will apply when you're back online. |  |
| `who_can_find_me.title` | Who can find me | yes |

## Search people

| Key | English | security_critical |
|---|---|---|
| `search.people.add_contact` | Add contact |  |
| `search.people.channel_result` | Channel {handle} |  |
| `search.people.failed` | Couldn't search. Check your connection and try again. |  |
| `search.people.heading` | People |  |
| `search.people.not_found` | No one found. Check the spelling, or ask them for an invite link. |  |
| `search.people.request_refused` | Couldn't send the request. They may have changed who can find them. |  |
| `search.people.search` | Search people |  |
| `search.people.searching_a11y` | Searching |  |
| `search.people.send_request` | Send request |  |

## Channel members

| Key | English | security_critical |
|---|---|---|
| `channels.members.back_a11y` | Back to channel info |  |
| `channels.members.empty` | No members have shared a card yet. |  |
| `channels.members.error.load` | Could not load the members. Check your connection and try again. |  |
| `channels.members.heading` | Members ({count}) |  |
| `channels.members.info_sub` | Who reads this channel and shared a card. |  |
| `channels.members.loading` | Loading members... |  |
| `channels.members.title` | Members |  |
| `channels.members.truncated` | Showing the first members only. |  |
| `channels.members.unnamed` | Member |  |
| `channels.members.unreadable` | {count, plural, one {# member shared a card under an older key this device does not hold.} other {# members shared cards under an older key this device does not hold.}} |  |
| `channels.members.you` | You |  |

## Device link: the signed-in device

| Key | English | security_critical |
|---|---|---|
| `device_link.request.code_a11y` | Sign-in code | yes |
| `device_link.request.enter_code` | Enter this code on the new device: | yes |
| `device_link.request.handled_elsewhere` | This sign-in request was handled on another device. |  |
| `device_link.request.heading` | New sign-in request | yes |
| `device_link.request.never_share` | Never read this code to anyone. Lakko will never ask for it. | yes |
| `device_link.request.not_recorded` | Could not reach Lakko to finish adding the device. Nothing was added. Try again from the new device. |  |
| `device_link.request.someone_signing_in` | Someone is signing in to your account on a new device. | yes |
| `device_link.request.too_many` | Too many sign-in requests today. Use a QR code, or try again tomorrow. |  |
| `device_link.request.wrong_code` | The code did not match. Nothing was added. | yes |
| `device_link.request.yes_its_me` | Yes, it's me | yes |

## Device link: the new device

| Key | English | security_critical |
|---|---|---|
| `device_link.code.enter_six_digits` | Enter the six digits your other device shows. |  |
| `device_link.code.expired` | This request expired. Start again. |  |
| `device_link.code.field_a11y` | Code from your other device |  |
| `device_link.code.intro` | Open Lakko in a browser or on your phone where you are already signed in. It will show a six-digit code. Enter it here. | yes |
| `device_link.code.restoring` | Restoring... |  |
| `device_link.code.start_again` | Start again |  |
| `device_link.code.unreachable` | Could not reach Lakko. Check your connection, then start again. |  |
| `device_link.code.waiting` | Waiting for your other device... |  |
| `device_link.code.wrong` | That code does not match. Start again and enter the code your other device shows. |  |

## Removed device

| Key | English | security_critical |
|---|---|---|
| `devices.removed.body` | It was removed from your account on another device. New messages no longer reach it. Sign out, then sign in again if you want to use it. | yes |
| `devices.removed.sign_out` | Sign out | yes |
| `devices.removed.title` | This device was removed | yes |

## Invite links, username limit, start-up

| Key | English | security_critical |
|---|---|---|
| `common.error.starting_up` | Lakko is still starting up. Try again in a moment. |  |
| `invite.links.made_elsewhere` | Made on another device |  |
| `invite.links.made_elsewhere_hint` | Created on another device. Copy it there, or revoke it here. |  |
| `username.error.bind_limited_later` | You have changed your username too often. Try again later. |  |
| `username.error.bind_limited_minutes` | You have changed your username too often. Try again in {minutes, plural, one {# minute} other {# minutes}}. |  |

## Changed English (old compare-a-code flow)

| Key | English | security_critical |
|---|---|---|
| `recovery.device.intro` | Enter your username, then open Lakko on a device that is still logged in and type the code it shows. Nothing is sent that Lakko's servers can read. No camera needed; a QR code is there if you prefer. | yes |
| `restore_identity.device_intro` | A device that is still logged in shows a six-digit code; type it here. The transfer is end-to-end encrypted; Lakko's servers cannot read it. No camera needed; a QR code is there if you prefer. | yes |

## iOS additions

| Key | English | security_critical |
|---|---|---|
| `device_link.code.intro_native` | Open Lakko on a device that is already signed in. It will show a six-digit code. Enter it here. | yes |
| `device_link.code.title` | Confirm on another device |  |
| `device_link.code.use_instead` | Confirm on another device instead |  |
| `who_can_find_me.list.choose` | Choose groups and channels |  |

- `device_link.code.intro_native` is the native wording of UX `identity-recovery.md` section 4 (the web keeps `device_link.code.intro`, which names a browser); `device_link.code.title` heads the panel and `device_link.code.use_instead` leads back from the QR code. Android may use all three.
- `who_can_find_me.list.choose` is the iOS row that opens the list under "Only the ones I choose"; the list itself is titled `who_can_find_me.groups.chosen`, as on the web.
- iOS maps onto batch-6 keys otherwise: the new device's "Could not reach the server" and "answered in an unexpected way" failures read `device_link.code.unreachable`; the old device's "could not be added" reads `device_link.request.error.generic`; "Is this you?" is `device_link.request.title` after `device_link.request.someone_signing_in`; a damaged invite link's heading reads `invite.contact.invalid_title` and a fragment-less one `invite.contact.invalid_body` (the web shows its invalid panel for both); "Lakko user" (a search result with no name) reads `contacts.request.someone`; the contact-request sheet's field reads `contacts.add.intro_label`. The add-member form now answers an unresolved name with `search.people.not_found` (it echoed the name through `chat.members.add.error.unknown_username`, which ADR-0060 rules out).
- Not on iOS: the channel member list (`channels.members.*`; iOS uploads and reads cards but has no list), the signup line (`who_can_find_me.signup_*`) and `common.error.starting_up`.

## Android additions

Two keys, neither `security_critical`:

| Key | English | security_critical |
|---|---|---|
| `folders.in_folder_a11y` | In this folder |  |
| `search.people.add_contact_title` | Add {name} |  |

- `folders.in_folder_a11y`: Android's Add to folder is a second step of the row's dropdown with a checkmark beside each folder the chat is in; the checkmark's spoken label. The web marks the same state with a checkbox role.
- `search.people.add_contact_title`: Android sends a contact request to a person found by Search people from a dialog whose title names the person; the web uses an inline form with no title.

Android also uses three keys the web list above calls web-only: `folders.settings.move_up` / `move_down` are the TalkBack custom actions that reorder folders in Settings > Chat folders (the non-drag equivalent of the long-press drag), and it reuses the iOS additions `device_link.code.title`, `device_link.code.intro_native` for the new device's code panel.

Mapping notes:

- Errors render from their kind. Search people's send failure is `contactsErrorText(error, "contacts.error.send")` (as the web's `contactsErrorText`), so a missing identity bundle reads `chat.members.add.error.no_bundle` and no network `common.error.offline`; an exhausted lookup allowance reads `tokens.lookup_allowance_used`. A refused reach grant and an unknown username in Contacts read `search.people.request_refused` / `search.people.not_found` (the two `TODO(ADR-0061)` literal-as-key carries in `ContactsErrorText.kt` are gone; `ContactsError.REACH_REFUSED_COPY` is now a diagnostic exception message, never shown).
- A channel journal claim that does not land at creation reads `common.error.offline` (the third `TODO(ADR-0061)`, in `ChannelsScreen.kt`).
- The new device's failures outside the named states: a limit through `authLimitText`, a `503` (login by name switched off) `device_link.error.ask_unavailable`, no network `device_link.code.unreachable`, anything else `device_link.error.generic`; a confirmed link with no recovery data `device_link.error.no_identity`.
- The old device's "The device could not be added. Open Devices to retry." is `device_link.request.error.generic` ("Could not add the device. Try again."), the web's sentence for a pairing that failed after the code matched.
- The consent step's "Someone is signing in ... / Is this you?" is two lines: `device_link.request.someone_signing_in` and `device_link.request.title`.
- `authLimitText` gained the `handle-bind-limited` arm (`username.error.bind_limited_minutes` / `_later`), so a username claim refused by the limit at signup no longer reads as "Too many attempts".
- A folder suggestion's name and description are `UiText` (`FolderSuggestion.name` / `.description`); adding one stores the name rendered in the interface language of that moment, as on the web. The channel member list's "You" / "Member" are rendered when the list loads, as on the web.
- Settings > Privacy > Who can find me: the explanation is `who_can_find_me.lede` and `lede_link` through `common.sentence_pair`; the notice after excluding a group under All is `who_can_find_me.members_excluded_notice` with `{privacy}` rendered as plain `settings.section.privacy` (Android has no link inside a sentence). The chosen-scopes list gained the `who_can_find_me.list.no_match` line when the search matches nothing.
- The scope list's stand-in names for an unnamed group or channel are `chat.group.fallback_name` / `channels.fallback_name`.

Left English on Android: none of the strings `main` brought. Exception messages (`InviteLinkDamaged`, `ContactsError` diagnostics, `ChatError` messages) and log lines are not displayed.
