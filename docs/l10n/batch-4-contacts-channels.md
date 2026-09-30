<!--
SPDX-License-Identifier: AGPL-3.0-or-later
SPDX-FileCopyrightText: 2026 Lakko Contributors
-->

# Localization batch 4: contacts, groups, channels and invite links

ADR-0058 S5, batch 4. Every user-visible string of the contacts screens (list, requests, add by username, the contact page with its safety number, rename and remove, the identity-change notices), the new-group form, the channels screens (list, create, the channel page, info, posts, subscribe and stop following, the manage screen with its subscribers, co-publishers and key refresh, join as co-publisher) and the three invite pages (`/i/` contact invite, `/g/` group invite, the rate-limited and damaged-link lines of `/c/`) moved from web literals into `l10n/catalog/en.json`. The in-call UI stays with the calls batch. The iOS and Android clients map their own literals of the same screens onto these keys; names follow meaning, not the web component.

269 keys added, 37 of them `security_critical` (they render in English until a translation is `reviewed`, and a signed update can never change them).

## Conventions

The conventions of batches 1-3 (`docs/l10n/batch-1-auth.md`, `batch-2-settings.md`, `batch-3-chat.md`) hold. In addition:

- Short facts on one line ("Created 16 Sep · Used up", "@news · 40 subscribers") are joined pair by pair through `common.fact_pair` (`{first} · {second}`). Web: `joinFacts` in `apps/web/src/l10n/index.ts`.
- A failure that shows a reason from the server's error code does it as two sentences through `common.sentence_pair`: a fixed first sentence (`channels.publish.error.prefix`, `channels.meta.error.save_prefix`, `channels.subscribe.error.prefix`, `channels.subscribers.error.deliver_prefix`) and the code's own sentence (`channels.error.*`).
- The channels service's `detail` prose is never shown: a known code maps to `channels.error.*` (`channels-unreachable`, `unknown-channel`, `post-too-large`, `empty-post`, `envelope-not-ready`), anything else to `channels.error.refused`.
- A contacts failure renders from its kind (`contacts/errors.ts` `contactsErrorText`): an allowance used up (`tokens.*`), a changed identity key (`contacts.identity_changed.*`), an unknown username (`chat.members.add.error.unknown_username`), a person who has not finished setting up (`chat.members.add.error.no_bundle`), a missing session (`common.error.session_ended`), a rate limit (`common.error.too_many_attempts`), no network (`common.error.offline`), and anything else the action's own key (`contacts.error.send`, `contacts.error.start_chat`, `invite.links.error.load`, `chat.error.create_link`, `chat.error.revoke_link`, `chat.error.delete_chat`, `chats.new_group.error`). Core and server prose goes to the log only.
- The identity-change notice has one message per verdict of the transparency log (`reset`, `unexplained`, `unconfirmed`) and an `_unnamed` twin of each, used when the contact's name is not known, instead of passing "This contact" as an argument.
- The group invite headline is one sentence per shape: `invite.group.lead_named` / `lead_anonymous` when the group has a name (shown on its own line under the lead), `lead_named_unnamed` / `lead_anonymous_unnamed` when it has none. The member count reuses `chat.members.count`.
- Counts and durations are plural messages (`invite.expiry.*`, `invite.links.uses_left`, `invite.links.expires_option_days`, `channels.info.subscribers`, `common.time.*`); the compact list facts `invite.links.expires_days` / `_hours` / `_minutes` take `{count}` as a plain number with a unit abbreviation.
- Dates in these screens ("Created 16 Sep", a post's time) are formatted by the platform for the interface language; only the words around them are catalog messages.
- `{handle}` in the channel messages and `{name}` in `contacts.send.done` carry their `@`.

## Reused keys

`nav.contacts`, `nav.channels`, `common.cancel`, `common.save`, `common.saving`, `common.sending`, `common.loading`, `common.remove`, `common.delete`, `common.dismiss`, `common.copied`, `common.retry`, `common.try_again`, `common.manage`, `common.looking_up`, `common.open_lakko`, `common.sentence_pair`, `common.error.offline`, `common.error.session_ended`, `common.error.too_many_attempts`, `auth.login.submit`, `chat.reauth.title`, `chat.reauth.action`, `chat.attachment.open`, `chat.attachment.hide`, `chat.call.voice`, `chat.media.video`, `chat.members.title`, `chat.members.count`, `chat.members.join.approve`, `chat.members.join.reject`, `chat.members.add.error.unknown_username`, `chat.members.add.error.no_bundle`, `chat.invite_link.label`, `chat.invite_link.create`, `chat.invite_link.copy`, `chat.invite_link.share`, `chat.invite_link.revoke`, `chat.error.create_link`, `chat.error.revoke_link`, `chat.error.delete_chat`, `chat.delete.control`, `chat.group_name.label`, `chat.select.count`, `chats.compose`, `chats.search.placeholder`, `chats.search.no_results`, `chats.lookup.unreachable`, `chats.create.group`, `chats.new_group.title`, `chats.new_group.creating`, `chats.new_group.error`, `chats.new_group.open_contacts`, `chats.new_group.accept_again`, `group.join.not_taking_requests`, `ds.clock_skew`, `tokens.*`.

## Web-only keys

- `contacts.reauth.body`: the web panel shown when the tab holds no identity secrets.
- `channels.index_hint`, `new_group.index_hint`: the hint in the empty main pane of the wide layout.
- `new_group.back`: the narrow-screen back button to the groups list.
- `group.join.succeeded_opening`: the interim line while web opens the chat it just joined.

Every other key applies to all three clients.

## Left for other batches

- `/c/` call-link landing: the call host's own failure and busy sentences (`outcome.message` from `calls/call-start-flow.ts`, rendered through `call.link.failed`) go with the calls batch, as does `calls/invite.ts` (`describeExpiry` / `describeMaxUses` for call links, currently unused by any screen).
- Stored stand-in labels (`Direct chat`, `New group`, `Joined group (Welcome N)`) are registry data recognised by `chat/group-title.ts`, never shown as such; they stay English.
- The example link `https://lakko.me/g/...` in the co-publisher field is not a message.

## Shared

| Key | English | security_critical |
|---|---|---|
| `common.fact_pair` | {first} · {second} |  |
| `common.time.days_ago` | {count, plural, one {# day ago} other {# days ago}} |  |
| `common.time.hours_ago` | {count, plural, one {# hour ago} other {# hours ago}} |  |
| `common.time.just_now` | just now |  |
| `common.time.minutes_ago` | {count, plural, one {# minute ago} other {# minutes ago}} |  |

## Contact list and requests

| Key | English | security_critical |
|---|---|---|
| `contacts.accept.error.old_version` | This request came from an older version of Lakko. Ask them to update and send it again. |  |
| `contacts.add.share_invite` | Share invite link |  |
| `contacts.error.send` | Could not send the request. Try again. |  |
| `contacts.error.start_chat` | Could not start the chat. Try again. |  |
| `contacts.list.add` | Add |  |
| `contacts.list.empty.action` | Add contact |  |
| `contacts.list.empty.body` | Add someone by username, or share your invite link. |  |
| `contacts.list.empty.title` | No contacts yet |  |
| `contacts.list.no_match` | No contacts match. |  |
| `contacts.list.requests` | Requests |  |
| `contacts.list.search` | Search contacts |  |
| `contacts.list.sent` | Sent ({count}) |  |
| `contacts.list.sent_dismiss` | Remove from this list |  |
| `contacts.list.sent_waiting` | Waiting |  |
| `contacts.mailbox.join_failed` | You were added to a group, but the invitation could not be opened on this device. Ask the person who added you to add you again. |  |
| `contacts.mailbox.poll_failed` | Couldn't check for new contact requests. Trying again. |  |
| `contacts.mailbox.sign_in` | Sign in again to check for new requests. |  |
| `contacts.reauth.body` | The keys this session needs for contacts are not available. Sign in again to use Contacts. |  |
| `contacts.request.accept` | Accept | yes |
| `contacts.request.decline` | Decline |  |
| `contacts.request.no_intro` | Wants to connect |  |
| `contacts.send.done` | Request sent to {name}. It waits under Sent until they accept. |  |

## Add a contact

| Key | English | security_critical |
|---|---|---|
| `contacts.add.by_username` | Add by username |  |
| `contacts.add.intro_label` | Message (optional) |  |
| `contacts.add.intro_placeholder` | Hi, this is Alice from the conference. |  |
| `contacts.add.send` | Send |  |
| `contacts.add.subtitle` | Requests are end-to-end encrypted. Lakko's servers cannot read them. | yes |
| `contacts.add.title` | Add a contact |  |
| `contacts.add.username_label` | Username |  |
| `contacts.add.username_placeholder` | @alice |  |

## Contact page (detail, safety number, rename, remove)

| Key | English | security_critical |
|---|---|---|
| `chat.delete.sub` | Removes this conversation and everything in it from this device. It cannot be undone. | yes |
| `contacts.detail.actions` | Contact actions |  |
| `contacts.detail.call_needs_chat` | Send a message first to start a call. |  |
| `contacts.detail.message` | Message |  |
| `contacts.detail.message_unavailable` | This contact was added before chats could be started from here. Ask them to message you. |  |
| `contacts.detail.their_name` | Calls themselves {name} |  |
| `contacts.detail.username_not_verified` | Username not verified | yes |
| `contacts.remove.body` | Any chats you share stay as they are; only your contact list changes. You can add them again later. | yes |
| `contacts.remove.confirm_named` | Remove {name} |  |
| `contacts.remove.control` | Remove contact |  |
| `contacts.remove.sub` | Takes {name} off your contact list on this account. | yes |
| `contacts.remove.title` | Remove this contact? |  |
| `contacts.rename.action` | Rename |  |
| `contacts.rename.field` | Your name for this contact |  |
| `contacts.rename.placeholder` | Leave blank to use their own name |  |
| `contacts.rename.sub_local` | Your name for this contact. Only you see it. |  |
| `contacts.rename.sub_theirs` | Shown as they set it. You can give them your own name. |  |
| `contacts.rename.title` | Name |  |
| `contacts.safety.body` | Compare it with {name} in person or on a call. Matching numbers mean nobody is between you. | yes |
| `contacts.safety.error` | Could not compute the safety number. Try again. |  |
| `contacts.safety.number` | Safety number | yes |
| `contacts.safety.show` | Show |  |
| `contacts.safety.sign_in` | Sign in again to see the safety number. |  |
| `contacts.safety.title` | Verify safety number | yes |

## Identity-change notices

| Key | English | security_critical |
|---|---|---|
| `contacts.identity_changed.reset` | {name} reset their secure identity (for example, they lost their device). The transparency log confirms this was an owner-initiated reset. If you trust this is really them, re-verify their safety number out of band before continuing. | yes |
| `contacts.identity_changed.reset_unnamed` | This contact reset their secure identity (for example, they lost their device). The transparency log confirms this was an owner-initiated reset. If you trust this is really them, re-verify their safety number out of band before continuing. | yes |
| `contacts.identity_changed.unconfirmed` | {name}'s secure identity changed and could not be confirmed against the transparency log. Verify out of band before trusting. | yes |
| `contacts.identity_changed.unconfirmed_unnamed` | This contact's secure identity changed and could not be confirmed against the transparency log. Verify out of band before trusting. | yes |
| `contacts.identity_changed.unexplained` | {name}'s secure identity changed, and the transparency log does NOT show an owner reset — treat this as a possible account compromise. Verify out of band before trusting. | yes |
| `contacts.identity_changed.unexplained_unnamed` | This contact's secure identity changed, and the transparency log does NOT show an owner reset — treat this as a possible account compromise. Verify out of band before trusting. | yes |

## Invite links (your list and the create form)

| Key | English | security_critical |
|---|---|---|
| `contacts.invite_links.subtitle` | Share a link with someone you want to hear from. |  |
| `contacts.invite_links.title` | Invite links |  |
| `invite.links.already_removed` | "{label}" had already been removed. |  |
| `invite.links.already_removed_unnamed` | This link had already been removed. |  |
| `invite.links.create` | Create link |  |
| `invite.links.created` | Created {date} |  |
| `invite.links.desc` | Anyone who opens one of your links can send you a contact request until it expires or is used up. | yes |
| `invite.links.empty` | No active links. |  |
| `invite.links.error.copy` | Could not copy the link. |  |
| `invite.links.error.load` | Could not load your invite links. Try again. |  |
| `invite.links.error.sign_in_create` | Sign in again to create an invite link. |  |
| `invite.links.error.sign_in_list` | Sign in again to see your invite links. |  |
| `invite.links.error.sign_in_revoke` | Sign in again to revoke an invite link. |  |
| `invite.links.expired` | Expired |  |
| `invite.links.expires_days` | Expires in {count}d |  |
| `invite.links.expires_field` | Expires |  |
| `invite.links.expires_hours` | Expires in {count}h |  |
| `invite.links.expires_minutes` | Expires in {count}m |  |
| `invite.links.expires_option_days` | {count, plural, one {In # day} other {In # days}} |  |
| `invite.links.expires_option_never` | Never |  |
| `invite.links.for_new_user` | For someone new to Lakko |  |
| `invite.links.inline_note` | Lakko could not shorten this link, so it is long. It works the same way and cannot be revoked. | yes |
| `invite.links.label_field` | Label (optional) |  |
| `invite.links.label_placeholder` | Shown to whoever opens the link |  |
| `invite.links.list` | Active invite links |  |
| `invite.links.never_expires` | Never expires |  |
| `invite.links.referral_option` | For someone without a Lakko account yet |  |
| `invite.links.revoked` | Link revoked. |  |
| `invite.links.revoking` | Revoking... |  |
| `invite.links.unlimited_uses` | Unlimited uses |  |
| `invite.links.used_up` | Used up |  |
| `invite.links.uses_field` | Uses |  |
| `invite.links.uses_left` | {count, plural, one {# use left} other {# uses left}} |  |
| `invite.links.uses_option_one` | One person |  |
| `invite.links.uses_option_unlimited` | Unlimited |  |
| `invite.links.uses_option_up_to` | {count, plural, one {Up to # person} other {Up to # people}} |  |

## Contact invite page (/i/)

| Key | English | security_critical |
|---|---|---|
| `invite.contact.already_accepted` | You have already accepted this invite. Your request is on its way to {name}; check Contacts for the reply. |  |
| `invite.contact.check_body` | Ask the person for their safety number in a way you already trust (in person, on a call). If it does not match, this link came from someone else. | yes |
| `invite.contact.check_title` | Check it before you accept. | yes |
| `invite.contact.expired` | This invite has expired. Ask the inviter for a fresh link. |  |
| `invite.contact.expiry` | Expiry |  |
| `invite.contact.fingerprint` | Inviter fingerprint | yes |
| `invite.contact.intro_label` | Intro message (optional) |  |
| `invite.contact.invalid_body` | The link is broken or was changed on the way. Ask the person who invited you for a new one. | yes |
| `invite.contact.invalid_title` | This invite link is invalid |  |
| `invite.contact.needs_auth_body` | You need an account to accept. Create one, or log in, and you will come back here automatically. |  |
| `invite.contact.needs_auth_title` | Someone wants to connect on Lakko |  |
| `invite.contact.no_longer_valid` | This link is no longer valid. Ask the person for a new one. |  |
| `invite.contact.referral` | Referral link: your request is sent automatically. |  |
| `invite.contact.sending` | Sending your request to {name}... |  |
| `invite.contact.sent` | Request sent to {name}. |  |
| `invite.contact.title` | Connect with {name}? | yes |
| `invite.contact.type` | Type |  |
| `invite.contact.used_up` | This link has been used up. Ask the person for a new one. |  |
| `invite.contact.uses` | Uses allowed |  |
| `invite.expiry.ago_days` | {count, plural, one {expired # day ago} other {expired # days ago}} |  |
| `invite.expiry.ago_hours` | {count, plural, one {expired # hour ago} other {expired # hours ago}} |  |
| `invite.expiry.ago_minutes` | {count, plural, one {expired # minute ago} other {expired # minutes ago}} |  |
| `invite.expiry.in_days` | {count, plural, one {in # day} other {in # days}} |  |
| `invite.expiry.in_hours` | {count, plural, one {in # hour} other {in # hours}} |  |
| `invite.expiry.in_minutes` | {count, plural, one {in # minute} other {in # minutes}} |  |
| `invite.expiry.never` | never expires |  |
| `invite.go_home` | Go to Lakko home |  |
| `invite.sign_up` | Sign up |  |
| `invite.uses.single` | single-use |  |
| `invite.uses.unlimited` | unlimited |  |
| `invite.uses.up_to` | {count, plural, one {up to # use} other {up to # uses}} |  |

## Any invite page

| Key | English | security_critical |
|---|---|---|
| `invite.link.damaged` | This invite link is damaged. Ask for a new link. | yes |
| `invite.link.invalid` | This invite link is invalid or has expired. |  |
| `invite.link.loading` | Loading invite... |  |
| `invite.link.rate_limited` | Too many requests from this network right now. Try again in a minute. |  |

## Group invite page (/g/)

| Key | English | security_critical |
|---|---|---|
| `group.join.needs_account` | You need an account to ask to join. Create one, or log in. |  |
| `group.join.request` | Request to join |  |
| `group.join.request_failed` | Your request didn't go through. Check your connection and try again. |  |
| `group.join.sending` | Sending request... |  |
| `group.join.succeeded` | You joined the group. |  |
| `group.join.succeeded_opening` | You joined the group. Opening the chat... |  |
| `group.join.title` | Join a group |  |
| `invite.group.invited_by` | Invited by {fingerprint} | yes |
| `invite.group.lead_anonymous` | You are invited to | yes |
| `invite.group.lead_anonymous_unnamed` | You are invited to a group | yes |
| `invite.group.lead_named` | {name} invites you to | yes |
| `invite.group.lead_named_unnamed` | {name} invites you to a group | yes |

## New group

| Key | English | security_critical |
|---|---|---|
| `new_group.back` | Groups |  |
| `new_group.error.sign_in` | Sign in again to create a group. |  |
| `new_group.index_hint` | Select a group to start messaging |  |
| `new_group.name_hint` | Every member sees the name. It is sent encrypted; the server never reads it. | yes |
| `new_group.name_placeholder` | Weekend plans |  |
| `new_group.no_contacts` | Add contacts first; a group starts from people you already know. |  |
| `new_group.subtitle` | You will be the group's owner. You can add more people later. |  |

## Channel list and search

| Key | English | security_critical |
|---|---|---|
| `channels.empty.action` | Create a channel |  |
| `channels.empty.title` | No channels yet |  |
| `channels.fallback_name` | Channel |  |
| `channels.find.action` | Find {handle} |  |
| `channels.find.none` | No channel found for {handle}. |  |
| `channels.index_hint` | Select a channel, or create one |  |
| `channels.new_menu` | New channel or join one |  |
| `channels.row.own` | Your channel |  |
| `channels.search.label` | Search channels or find one by handle |  |
| `channels.search.placeholder` | Search or find by @handle |  |

## New channel

| Key | English | security_critical |
|---|---|---|
| `channels.field.description_optional` | Description (optional) |  |
| `channels.field.name` | Name |  |
| `channels.new.create` | Create |  |
| `channels.new.error.handle` | Could not claim the handle. Try again. |  |
| `channels.new.error.handle_offline` | Could not claim the handle. Check your connection and try again. |  |
| `channels.new.error.handle_taken` | {handle} is already taken. Pick another, or leave it blank. |  |
| `channels.new.error.no_name` | Give the channel a name. |  |
| `channels.new.error.sign_in` | Sign in again to create a channel. |  |
| `channels.new.handle_hint` | Others can find and subscribe to the channel by it. | yes |
| `channels.new.handle_label` | Public @handle (optional) |  |
| `channels.new.handle_placeholder` | @my-channel |  |
| `channels.new.subtitle` | Subscribers see the name and description. They travel encrypted; only ciphertext reaches the server. | yes |
| `channels.new.title` | New channel |  |

## Join as co-publisher

| Key | English | security_critical |
|---|---|---|
| `channels.join.default_name` | Co-published channel |  |
| `channels.join.error.sign_in` | Sign in again to join a channel. |  |
| `channels.join.link_label` | Link |  |
| `channels.join.name_label` | Name on this device (optional) |  |
| `channels.join.no_approval` | No approval yet. Try again once the owner has approved. |  |
| `channels.join.requesting` | Requesting... |  |
| `channels.join.subtitle` | Paste the link a channel owner shared. Once they approve, this device can post to the channel. |  |
| `channels.join.title` | Join as co-publisher |  |
| `channels.join.waiting` | Waiting for the channel owner to approve... |  |

## Channel page, info and posts

| Key | English | security_critical |
|---|---|---|
| `channels.back` | Back to channels |  |
| `channels.compose.label` | New post |  |
| `channels.compose.placeholder` | Write a post |  |
| `channels.compose.publish` | Publish |  |
| `channels.compose.publishing` | Publishing... |  |
| `channels.info.manage_sub` | Its name and description, who reads it, and who else may post. |  |
| `channels.info.subscribers` | {count, plural, one {# subscriber} other {# subscribers}} |  |
| `channels.info.unsubscribe_sub` | Takes the channel off your list and stops this device reading it. You can subscribe again later. | yes |
| `channels.manage.back` | Back to posts |  |
| `channels.manage.title` | Manage channel |  |
| `channels.post.not_available` | Not available |  |
| `channels.post.older_key` | This post is older than what this device can read. |  |
| `channels.post.unreadable` | This post could not be opened on this device. |  |
| `channels.posts.empty` | No posts yet. |  |
| `channels.posts.empty_owner` | No posts yet. Write the first one below. |  |
| `channels.publish.error.generic` | Could not publish. Try again. |  |
| `channels.publish.error.not_publisher` | This device cannot post to this channel. |  |
| `channels.publish.error.prefix` | Could not publish. |  |
| `channels.subscribe.action` | Subscribe |  |
| `channels.subscribe.error.generic` | Could not subscribe. Try again later. |  |
| `channels.subscribe.error.prefix` | Could not subscribe. |  |
| `channels.subscribe.prompt_body` | Posts appear here once the channel lets this device in. |  |
| `channels.subscribe.prompt_title` | Subscribe to read this channel |  |
| `channels.subscribe.status.not_subscribed` | Not subscribed |  |
| `channels.subscribe.status.subscribed` | Subscribed |  |
| `channels.subscribe.status.subscribing` | Subscribing... |  |
| `channels.subscribe.status.waiting` | Waiting to be let in... |  |
| `channels.subscribe.waiting_notice` | Subscribed. Posts appear once the channel lets this device in; that usually takes a moment. |  |
| `channels.unsubscribe.action` | Stop following |  |
| `channels.unsubscribe.body` | It leaves your list and this device stops reading it. You can subscribe again later. | yes |
| `channels.unsubscribe.title` | Stop following this channel? |  |
| `channels.unsubscribe.title_named` | Stop following {name}? |  |

## Manage channel

| Key | English | security_critical |
|---|---|---|
| `channels.copublishers.desc` | People who can post to this channel. Share a link; approve them when they ask. | yes |
| `channels.copublishers.error.approve` | Could not approve the request. Try again. |  |
| `channels.copublishers.error.poll` | Could not check for requests. Try again. |  |
| `channels.copublishers.error.reject` | Could not reject the request. Try again. |  |
| `channels.copublishers.link` | Co-publisher invite link | yes |
| `channels.copublishers.request_row` | Someone asked to co-publish |  |
| `channels.copublishers.requests` | Requests ({count}) |  |
| `channels.copublishers.title` | Co-publishers |  |
| `channels.manage.error.open` | Could not open the channel's settings. Try again. |  |
| `channels.manage.error.sign_in` | Sign in again to manage this channel. |  |
| `channels.meta.desc` | The name and description reach every subscriber, encrypted. The server never reads them. | yes |
| `channels.meta.error.description_bad_character` | The description contains a character that cannot be shown. |  |
| `channels.meta.error.description_too_long` | The description is too long. |  |
| `channels.meta.error.generic` | That name or description cannot be used. |  |
| `channels.meta.error.name_bad_character` | The name contains a character that cannot be shown. |  |
| `channels.meta.error.name_empty` | A channel needs a name |  |
| `channels.meta.error.name_line_break` | The name must be one line. |  |
| `channels.meta.error.name_too_long` | The name is too long. |  |
| `channels.meta.error.save` | Could not save. Try again. |  |
| `channels.meta.error.save_prefix` | Could not save. |  |
| `channels.meta.saved` | Saved. Subscribers see the new name with their next posts. |  |
| `channels.meta.title` | Channel |  |
| `channels.rotation.desc` | Lakko refreshes the channel's protection on its own. Refresh it now if you think a subscriber's device was compromised. | yes |
| `channels.rotation.error.check` | Could not check the channel's protection. |  |
| `channels.rotation.error.refresh` | Could not refresh. Try again in a moment. |  |
| `channels.rotation.never` | Not refreshed yet. |  |
| `channels.rotation.refresh_now` | Refresh now |  |
| `channels.rotation.refreshed` | Refreshed {when}. |  |
| `channels.rotation.refreshing` | Refreshing... |  |
| `channels.rotation.title` | Protection |  |
| `channels.subscribers.count` | ({count}) |  |
| `channels.subscribers.desc` | New subscribers are let in from this device automatically. |  |
| `channels.subscribers.empty` | No subscribers yet. Share the channel's handle so people can subscribe. |  |
| `channels.subscribers.error.deliver` | Could not set up this subscriber. Try again. |  |
| `channels.subscribers.error.deliver_prefix` | Could not set up this subscriber. |  |
| `channels.subscribers.error.load` | Could not load subscribers. Try again. |  |
| `channels.subscribers.retrying` | Retrying... |  |
| `channels.subscribers.row` | Subscriber {number} |  |
| `channels.subscribers.status.not_ready` | Not ready |  |
| `channels.subscribers.status.ready` | Ready |  |
| `channels.subscribers.status.setting_up` | Setting up... |  |
| `channels.subscribers.title` | Subscribers |  |

## Channel errors

| Key | English | security_critical |
|---|---|---|
| `channels.error.empty_post` | Write something to post. |  |
| `channels.error.not_let_in` | The channel has not let this device in yet. Try again later. |  |
| `channels.error.post_too_large` | This post is too long to publish. |  |
| `channels.error.refused` | The server refused the request. Try again later. |  |
| `channels.error.unknown_channel` | This channel does not exist on the server. |  |
| `channels.error.unreachable` | The channels service is not reachable right now. Try again in a moment. |  |
| `channels.legacy.channel` | This channel was created by an older version of Lakko and cannot be opened in this build. Posts you already published stay readable for your subscribers. To publish again, create a new channel — this one cannot be repaired. |  |
| `channels.legacy.copublisher_link` | This co-publisher invite link was created by an older version of Lakko and no longer works. Ask the channel owner to send you a new invite link. |  |

## Android additions

Keys the Android client needed that neither web nor iOS had. Named by meaning, so iOS can reuse them.

| Key | English | security_critical |
|---|---|---|
| `channels.new.error.create` | Could not create the channel. Try again. |  |
| `channels.posts.error.load` | Could not load posts. |  |
| `common.clipboard.link` | Lakko link |  |
| `common.clipboard.message` | Lakko message |  |
| `common.creating` | Creating... |  |
| `common.open_chat` | Open chat |  |
| `common.qr_code` | QR code |  |
| `contacts.request.someone` | Someone |  |
| `contacts.safety.body_unnamed` | Compare it with your contact in person or on a call. Matching numbers mean nobody is between you. | yes |
| `contacts.safety.row_sub` | Compare a number in person to confirm this is really them. | yes |
| `devices.pairing.qr_description` | Pairing QR code |  |
| `invite.contact.accept_failed` | Could not accept the invite. Try again. |  |
| `invite.contact.accepting` | Accepting invite... |  |
| `invite.contact.already_accepted_unnamed` | You have already accepted this invite. Check Contacts for this person. |  |
| `invite.contact.connected` | Connected. |  |
| `invite.contact.page_title` | Invite |  |

`common.clipboard.*` are the clip labels Android shows in its paste preview. `invite.contact.*` above exist because the Android `/i/` page accepts on open (no confirm step, no inviter name) and lands on a "Connected." panel that shows the inviter fingerprint with `invite.contact.fingerprint` and `invite.contact.check_body`. `contacts.request.someone` replaces the resolver's `Someone` placeholder on the request row; the resolver's `Member` placeholder (`PLACEHOLDER_MEMBER`) is data shared with the chat screens and is not moved here.

On Android the contacts view model holds `sendError`, `inviteError` and `activeInvitesError` as `UiText`; a failure renders from its kind through `ui/contacts/ContactsErrorText.kt` `contactsErrorText` (unknown username, not set up, token pool, clock skew, 401 / 429 / no network from discovery, else the action's key: `contacts.error.send`, `chat.error.create_link`, `chat.error.revoke_link`, `invite.links.error.load`). The identity-change notice is `supersessionRefusedMessage` → `contacts.identity_changed.*` (the `_unnamed` twin when no handle was typed). Channel failures render through `ui/channels/ChannelsErrorText.kt` (`channelsErrorText`: error code → `channels.error.*`, joined after a fixed first sentence where the web does; `channelMetaErrorText`: the core's reason code → `channels.meta.error.*`). `LegacyChannelException.text` carries `channels.legacy.*`. The `/g/` page and the co-publisher join read `joinFailureCopy` (now a `UiText`), `groupInvitePageLines` and `invitedByLine` (`invite.group.*`, `chat.members.count`), `invite.link.invalid` / `damaged` / `rate_limited` (the last also on the `/c/` page). Channel rows, headers and the rotation line take the `Strings` on screen (`common.fact_pair`, `channels.info.subscribers`, `common.time.*`, `channels.rotation.*`).

Where the Android wording differed from an existing key, the catalog wording is used: the add-contact sheet title is `contacts.add.title`, its intro field `contacts.add.intro_label` and its button `contacts.add.send`; the refresh failure line is `contacts.mailbox.poll_failed`; the re-sign-in panel is `chat.reauth.title` with `contacts.reauth.body` (so that key is no longer web-only); the empty list gained `contacts.list.empty.body`; the invite-links sheet description is `invite.links.desc`; the remove dialog is `contacts.remove.title` with `contacts.remove.sub` + `contacts.remove.body`; the rename dialog is `contacts.rename.action` / `contacts.rename.placeholder` / `contacts.rename.field`; "Their name: X" is `contacts.detail.their_name`; "Chat not available on this device yet" is `contacts.detail.message_unavailable`; the safety-number screen is `contacts.safety.number` / `contacts.safety.body` / `contacts.safety.error`; the `/i/` states are `invite.contact.expired` / `used_up` / `no_longer_valid`, `invite.link.damaged` (malformed link) and `common.error.session_ended`; channel handle fields are `channels.new.handle_label` / `handle_placeholder` / `handle_hint`; channel status lines are `channels.row.own` / `channels.subscribe.status.subscribed`; relative times read `common.time.*` ("5 minutes ago", "just now" also at the start of a post's time); the key-rotation card is the web's Protection card (`channels.rotation.*`); subscriber rows read `channels.subscribers.row` / `status.ready` / `status.setting_up` under `channels.subscribers.desc`; a co-publisher request row reads `channels.copublishers.request_row` (no number), its link `channels.copublishers.link` (the separate "Anyone with it can request to publish" line was dropped: the card description says it); the co-publisher join fields are `channels.join.*` and the request button `group.join.request`.

## iOS additions

Keys the iOS client needed that neither web nor Android had. Named by meaning, so Android can reuse them. None is `security_critical`. The `/i/` page on iOS accepts the invite as soon as it opens (ADR-0005 section 13.4) and then shows one result card, headline and explanation; `invite.contact.result.*` are those headlines, with the explanation from the existing `invite.contact.*` / `invite.link.*` sentences where one fits. iOS reuses the Android additions `invite.contact.page_title`, `invite.contact.accepting`, `invite.contact.accept_failed`, `contacts.request.someone`, `contacts.safety.row_sub`, `contacts.safety.body_unnamed`, `channels.posts.error.load` and `channels.new.error.create`.

| Key | English | security_critical |
|---|---|---|
| `channels.error.sign_in` | Sign in again to use channels. |  |
| `contacts.detail.gone_body` | This person is no longer in your contacts. |  |
| `contacts.detail.gone_title` | Contact not found |  |
| `contacts.list.section_index` | Section index |  |
| `group.join.open_chat` | Open chat |  |
| `invite.contact.result.already_body` | You already accepted this invite on this device. Find the person under Contacts. |  |
| `invite.contact.result.already_title` | Already accepted |  |
| `invite.contact.result.expired_title` | Invite expired |  |
| `invite.contact.result.failed_title` | Invite not accepted |  |
| `invite.contact.result.not_found_title` | Invite not found |  |
| `invite.contact.result.rate_limited_title` | Not right now |  |
| `invite.contact.result.sent_body` | Your contact request is on its way. Once they accept, the chat appears under Chats. |  |
| `invite.contact.result.sent_title` | Request sent |  |
| `invite.contact.result.used_up_title` | Invite used up |  |
| `invite.links.new_badge` | New |  |
| `new_group.name_not_sent` | The group name was not sent. Set it from the members panel. |  |

On iOS the contacts view model holds `sendError`, `inviteError` and `activeInvitesError` as `UiText`; a failure renders from its kind through `ContactsViewModel.contactsErrorText(for:fallbackKey:quotaKey:)` (token pool, a changed identity key, unknown username, not set up, 429, no network, else the action's key: `contacts.error.send`, `chat.error.create_link`, `chat.error.revoke_link`, `invite.links.error.load`). The inbox's `lastError` now holds a catalog key (`contacts.mailbox.poll_failed` / `join_failed`). `NewGroupScreen` hands the chat a `UiText` notice (`new_group.name_not_sent`, `chat.members.add.error.some_failed` with `Localization.joinList`, joined by `common.sentence_pair`). `groupInviteInvalidMessage`, `groupInviteDamagedMessage`, `inviteRateLimitedMessage`, `joinSucceededMessage`, `joinOpenChat`, `joinTryAgain` and `JoinRequestFailure.message` are catalog reads; `GroupInviteHeadline` renders `invite.group.*` and `chat.members.count`. Channel failures render through `ChannelsErrorCopy` (code to `channels.error.*` after a fixed first sentence; `detail` is never shown); `LegacyChannelV1` messages read `channels.legacy.*`; the channel row, header and rotation line use `common.fact_pair`, `channels.info.subscribers`, `common.time.*`, `chat.day.yesterday` and `channels.rotation.*`.

Where the iOS wording differed from an existing key, the catalog wording is used: the empty contact list says `contacts.list.empty.body`; the add sheet's footer is `contacts.add.subtitle` and its intro field `contacts.add.intro_label` / `intro_placeholder`; the re-sign-in panel is `chat.reauth.title` with `contacts.reauth.body`; the invite-links footer is `invite.links.desc` (the iOS line "works once and for 7 days, added without seeing your username" is gone), the list header `invite.links.list`, uses left `invite.links.uses_left` / `invite.links.used_up` (no more "3 of 10 left"), expiry the compact `invite.links.expires_*`; the contact page footer is `contacts.safety.row_sub`, the rename alert `contacts.rename.action` / `placeholder` / `sub_local`, the remove dialog `contacts.remove.title` / `confirm_named` / `body` (not `contacts.remove.sub`: iOS removes the contact from this device's list, which "on this account" would misstate), "Chat not set up yet" `chats.new_chat.not_available`; the safety screen is `contacts.safety.body` / `body_unnamed` (its second sentence replaces the separate "If both of you see the same number" line); the `/i/` result card's safety block keeps only `invite.contact.check_body` (`check_title` says "before you accept", and iOS has already accepted); the new-group footer is `new_group.name_hint`; the channel access card is the Protection card (`channels.rotation.*`), subscriber rows `channels.subscribers.row` / `status.ready` / `status.not_ready` with `common.retry` / `channels.subscribers.retrying` on the per-row button, an unreadable post `channels.post.older_key` / `channels.post.unreadable`, a reader's prompts `channels.subscribe.prompt_title` / `prompt_body` / `waiting_notice`, the co-publisher request row `channels.copublishers.request_row` (no number), the co-publisher join `channels.join.*` with `group.join.request` / `group.join.sending`.
