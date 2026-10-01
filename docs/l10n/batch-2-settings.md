<!--
SPDX-License-Identifier: AGPL-3.0-or-later
SPDX-FileCopyrightText: 2026 Lakko Contributors
-->

# Localization batch 2: Settings keys

ADR-0061 S5, batch 2. Every user-visible string reached from web Settings moved from JSX literals into `l10n/catalog/en.json`: Profile, Security (passkeys, the Devices page and adding a device, the Recovery page with authenticator and email / phone enrolment), Appearance, Media, About, Account (data export, log out, forget this device, delete account and their confirmations), the stale-backup restore warning, and the rest of the web call-link guest page. The iOS and Android clients map their own literals of the same screens onto these keys; names follow meaning, not the web component.

142 keys added, 34 of them `security_critical` (they render in English until a translation is `reviewed`, and a signed update can never change them).

## Conventions

The batch 1 conventions (`docs/l10n/batch-1-auth.md`) hold. In addition:

- Dates and times (`{added}`, `{used}`) are formatted by the platform for the interface language before they are passed.
- `{positions}` is a list of word positions in Latin digits, joined by the platform list formatter (web: `listPositions` in `auth/phrase-confirm.ts`).
- `{contact}` in `settings.recovery.contact.sent_*` and `{key}` in `settings.recovery.totp.key` are rendered as elements (bold, a code font); web uses `tRich`.
- `{method}` and `{prompt}` in `settings.devices.add.no_camera`, and `{consequence}` in `backup.stale_restore.body`, are other catalog messages passed already translated, so each sentence is translated once.
- A destructive dialog names its target on the button (`settings.passkeys.remove.confirm_named`); an unnamed target uses the bare verb key.
- Errors render from their kind or code; each flow has a generic key (`*.error.generic`, `settings.devices.error.load` / `.remove` / `.scan`). Server and browser prose goes to the log only.

## Reused keys

Settings reuses these existing keys rather than adding near-duplicates: `common.cancel`, `common.confirm`, `common.try_again`, `common.loading`, `common.checking`, `common.preparing`, `common.sending`, `common.email`, `common.phone`, `common.error.offline`, `auth.username.label`, `auth.passkey.error.server_disabled`, `recovery.method.contact.title`, `recovery.method.device.title`, `recovery.totp.code_label`, `recovery.totp.error.enter_code`, `recovery.contact.phone_label`, `recovery.contact.send`, `recovery.contact.code_label`, `recovery.contact.error.invalid_email`, `recovery.contact.error.invalid_phone`, `recovery.contact.error.enter_code`, `recovery.contact.error.wrong`, `device_link.pairing.needs_username`, `device_link.request.title`, `device_link.request.adding`, `device_link.qr.waiting`, `devices.add.*` and `devices.scan.*` (the iOS add-device keys from batch 1; where the web wording differed, the catalog wording is used), and the `settings.section.*` / `settings.appearance.*` / `settings.language.*` keys from S2-S4.

## Web-only keys

- `settings.about.lede_web`: "Lakko on the web."
- `settings.passkeys.unsupported_browser`: a browser without WebAuthn.
- `settings.devices.error.no_device_key`: this browser holds no device key.
- `settings.data.export_note`: written inside the web export file.
- `call.guest.default_name`, `call.link.*`: the web call-link page for a visitor without an account (ADR-0032); natives use them only if they build that page.

Every other key applies to all three clients.

## Shared

| Key | English | security_critical |
|---|---|---|
| `common.save` | Save |  |
| `common.saving` | Saving... |  |
| `common.manage` | Manage |  |
| `common.set_up` | Set up |  |
| `common.on` | On |  |
| `common.off` | Off |  |
| `common.remove` | Remove |  |
| `common.adding` | Adding... |  |
| `common.unknown` | unknown |  |
| `common.error.session_not_ready` | Session not ready. |  |
| `common.error.session_expired` | Session expired. Sign in again. |  |
| `common.confirming` | Confirming... |  |
| `common.error.session_ended` | Your session has ended. Sign in again. |  |
| `common.removing` | Removing... |  |
| `common.open_lakko` | Open Lakko |  |

## Profile

| Key | English | security_critical |
|---|---|---|
| `settings.profile.lede` | How you appear to your contacts. |  |
| `settings.profile.name.label` | Your name |  |
| `settings.profile.preview.label` | How others see you |  |
| `settings.profile.preview.no_name` | Others see your key fingerprint until you set a name. |  |
| `settings.profile.username.none` | None |  |
| `settings.profile.saved` | Your name will update for your contacts. |  |
| `settings.profile.error.generic` | Could not save your name. Try again. |  |

## Settings list and section heads

| Key | English | security_critical |
|---|---|---|
| `settings.me.guest` | Guest |  |
| `settings.account.lede` | Your data, signing out, this device, and deleting your account. |  |
| `settings.security.lede` | How you sign in, which devices can read your messages, and how you get back in. |  |
| `settings.appearance.lede` | The theme on this device. |  |
| `settings.media.lede` | Playback on this device. |  |
| `settings.about.lede_web` | Lakko on the web. |  |

## Security

| Key | English | security_critical |
|---|---|---|
| `settings.security.devices_and_recovery` | Devices and recovery |  |
| `settings.devices.title` | Devices |  |
| `settings.security.devices.sub` | See, add and remove the devices on this account. |  |
| `settings.recovery.title` | Recovery |  |
| `settings.security.recovery.sub` | An authenticator app, an email address or a phone number that gets you back in. |  |
| `settings.security.safety_numbers.title` | Safety numbers | yes |
| `settings.security.safety_numbers.sub` | Each contact has a safety number. Compare it in person or on a call; matching numbers mean nobody is in between. Find it in the contact's details. | yes |

## Recovery page

| Key | English | security_critical |
|---|---|---|
| `settings.recovery.lede` | Your recovery phrase restores everything. These get you back into the account when you have lost your password; your messages then come from the phrase or from another of your devices. | yes |
| `settings.recovery.totp.title` | Authenticator app |  |

## About and Media

| Key | English | security_critical |
|---|---|---|
| `settings.about.version` | Version |  |
| `settings.about.licence` | Licence |  |
| `settings.about.licence_body` | {licence}. The source code is free to read, use and improve. |  |
| `settings.about.protection.title` | How Lakko protects you | yes |
| `settings.about.protection.body` | Messages, calls and contact requests are end-to-end encrypted: only the people in a conversation can read them, never Lakko's servers. Your password never leaves your device, and each contact has a safety number you can compare in person. | yes |
| `settings.media.autoplay_video_notes.title` | Autoplay video notes |  |
| `settings.media.autoplay_video_notes.sub` | Incoming video notes start playing, muted, as they scroll into view. Off by default because autoplay downloads media. |  |

## Passkeys

| Key | English | security_critical |
|---|---|---|
| `settings.passkeys.title` | Passkeys |  |
| `settings.passkeys.lede` | Sign in with Touch ID, Face ID or a security key instead of your password. Your recovery phrase always works too. |  |
| `settings.passkeys.unsupported_browser` | This browser cannot manage passkeys. Use a recent Safari, Chrome, Firefox or Edge. |  |
| `settings.passkeys.empty` | No passkeys yet. |  |
| `settings.passkeys.unnamed` | Unnamed passkey |  |
| `settings.passkeys.added_last_used` | Added {added} · last used {used} |  |
| `settings.passkeys.added_never_used` | Added {added} · never used |  |
| `settings.passkeys.name.label` | Passkey name |  |
| `settings.passkeys.name.placeholder` | Name this passkey (e.g. MacBook Touch ID) |  |
| `settings.passkeys.add` | Add passkey |  |
| `settings.passkeys.added` | Passkey "{name}" added. |  |
| `settings.passkeys.removed` | Removed "{name}". | yes |
| `settings.passkeys.error.name_required` | Give the passkey a name. |  |
| `settings.passkeys.error.cancelled` | Cancelled. No passkey was created. |  |
| `settings.passkeys.error.unsupported` | This browser / device can't create passkeys right now. |  |
| `settings.passkeys.error.server_status` | Server error ({status}). |  |
| `settings.passkeys.error.network` | Network error. Check your connection and retry. |  |
| `settings.passkeys.error.generic` | Could not update your passkeys. Try again. |  |
| `settings.passkeys.remove.title` | Remove this passkey? | yes |
| `settings.passkeys.remove.body` | The device that owns the matching private key will no longer be able to sign in with it. This cannot be undone. | yes |
| `settings.passkeys.remove.confirm` | Remove | yes |
| `settings.passkeys.remove.confirm_named` | Remove {name} | yes |

## Account: your data (export)

| Key | English | security_critical |
|---|---|---|
| `settings.data.title` | Your data |  |
| `settings.data.lede` | Download a copy of what Lakko's servers hold about your account. Your messages and contacts are end-to-end encrypted and stay on your devices, so they are not part of this file. |  |
| `settings.data.export` | Export my data |  |
| `settings.data.downloaded` | Downloaded {file}. |  |
| `settings.data.export_note` | Your messages, contacts, and group state are end-to-end encrypted and live only on your devices. |  |
| `settings.data.error.generic` | Could not export your data. Try again. |  |

## Account: session, device, deletion

| Key | English | security_critical |
|---|---|---|
| `settings.account.vault.unavailable` | Three-word recovery: not available on this server. Your 24-word phrase and your other devices still work. | yes |
| `settings.account.vault.on` | Three-word recovery: on (words {positions}). Another signed-in device and your 24-word phrase work too. | yes |
| `settings.account.vault.not_set_up` | Three-word recovery: not set up on this account yet. It is set up the next time you sign in on a device that holds your phrase. | yes |
| `settings.account.log_out` | Log out |  |
| `settings.account.log_out.sub` | Signs you out on this device. Your data stays here for the next login. | yes |
| `settings.account.forget_device.label` | Forget this device | yes |
| `settings.account.forget_device.sub` | Removes your account data from this browser. Your messages stay on your other devices; to come back here you will need your recovery phrase or another device. | yes |
| `settings.account.forget_device.title` | Forget this device? | yes |
| `settings.account.forget_device.body` | You'll lose the cached account on this browser. The only way back is the 24-word recovery passphrase you wrote down at signup. | yes |
| `settings.account.forget_device.confirm` | Forget device | yes |
| `settings.account.delete.sub` | Permanently erases your account and everything in it, and wipes this device. This cannot be undone. | yes |
| `settings.account.delete.title` | Delete your account? | yes |
| `settings.account.delete.body` | This erases your server-side identity — username, keys, recovery factors and backup — and signs you out everywhere. Your local messages are wiped from this device too. This cannot be undone. | yes |
| `settings.account.delete.confirm` | Delete account | yes |
| `settings.account.delete.error.generic` | Could not delete your account. Try again. |  |

## Recovery page: authenticator

| Key | English | security_critical |
|---|---|---|
| `settings.recovery.totp.lede` | A six-digit code from an authenticator app lets you sign in again if you lose your password. |  |
| `settings.recovery.totp.set_up` | Set up authenticator |  |
| `settings.recovery.totp.scan` | Scan this code with your authenticator app, or type the key in by hand, then enter the six-digit code the app shows. | yes |
| `settings.recovery.totp.key` | Key: {key} | yes |
| `settings.recovery.totp.error.wrong` | That code did not work. Wait for the next code in the app and try again. |  |
| `settings.recovery.totp.done` | Authenticator set up. A code from the app can now get you back into your account. |  |
| `settings.recovery.totp.already_enabled` | An authenticator is already set up for this account. To replace it, sign in again with your recovery phrase or from another device first. |  |
| `settings.recovery.totp.unavailable` | Authenticator setup is not available right now. |  |
| `settings.recovery.totp.error.generic` | Could not set up the authenticator. Try again. |  |

## Recovery page: email or phone

| Key | English | security_critical |
|---|---|---|
| `settings.recovery.contact.lede` | A one-time code sent to your email or phone lets you sign in again if you lose your password. |  |
| `settings.recovery.contact.error.bad_contact` | That does not look right. Check it and try again. |  |
| `settings.recovery.contact.sent_email` | We sent a six-digit code to {contact} by email. Enter it below. |  |
| `settings.recovery.contact.sent_phone` | We sent a six-digit code to {contact} by text message. Enter it below. |  |
| `settings.recovery.contact.done` | Done. A code sent there can now get you back into your account. |  |
| `settings.recovery.contact.already_enabled` | An email or phone is already set for this account. To replace it, sign in again with your recovery phrase or from another device first. |  |
| `settings.recovery.contact.unavailable` | Email and phone recovery are not available right now. |  |
| `settings.recovery.contact.error.generic` | Could not set up the email or phone. Try again. |  |

## Devices

| Key | English | security_critical |
|---|---|---|
| `settings.devices.lede` | The phones, tablets and computers signed in to your account. Adding one is end-to-end encrypted; Lakko's servers cannot read it. | yes |
| `settings.devices.sign_in_first` | Sign in first to manage devices. |  |
| `settings.devices.add.lede` | Sign in to Lakko on the new device first. Then pick which of the two is showing a code. |  |
| `settings.devices.add.no_camera` | No camera on either device? Sign in on the new one and choose "{method}": an "{prompt}" prompt with a code to compare appears here. | yes |
| `settings.devices.list.title` | Your devices |  |
| `settings.devices.list.lede` | A removed device loses access to your messages from that moment on. Removing one may ask for your passkey. | yes |
| `settings.devices.this_device` | This device |  |
| `settings.devices.this_device.sub` | The device you are using now. |  |
| `settings.devices.device_n` | Device {number} |  |
| `settings.devices.error.no_device_key` | This browser is not set up for your account. Sign in again. |  |
| `settings.devices.error.load` | Could not load your devices. Try again. |  |
| `settings.devices.error.remove` | Could not remove the device. Try again. | yes |
| `settings.devices.error.scan` | Could not add the device. Try again. |  |
| `settings.devices.show.lede` | Scan this code with a device that is already signed in. Once it confirms, this device joins your account. | yes |
| `settings.devices.show.start` | Show the code |  |
| `settings.devices.show.error.timeout` | The other device did not scan in time. Try again. |  |
| `settings.devices.scan.opening` | Opening the camera... |  |
| `settings.devices.scan.done` | Device added. It will appear in the list once it has finished joining. |  |

## Call link (guest join, web)

| Key | English | security_critical |
|---|---|---|
| `call.guest.default_name` | Guest |  |
| `call.link.title_start` | Start your call |  |
| `call.link.title_join` | Join a call |  |
| `call.link.loading` | Loading call link... |  |
| `call.link.invalid` | This call link is invalid or has expired. |  |
| `call.link.host` | Host: {name} |  |
| `call.link.end_current` | End current call |  |
| `call.link.start` | Start call |  |
| `call.link.join` | Join call |  |
| `call.link.connecting` | Connecting... |  |
| `call.link.waiting` | Waiting for the owner to admit you... |  |
| `call.link.waiting_hint` | You can leave this screen; you will join when you are admitted. |  |
| `call.link.live` | You are in this call. |  |
| `call.link.dead` | This call has ended or is not active. |  |
| `call.link.ended` | The call has ended. |  |
| `call.link.failed` | Failed: {reason} |  |

## Backup restore (stale backup warning)

| Key | English | security_critical |
|---|---|---|
| `backup.stale_restore.title` | Restore this older backup? | yes |
| `backup.stale_restore.consequence` | Restoring it will roll your account back to older state — older messages, and devices you removed may reappear. | yes |
| `backup.stale_restore.body` | This backup is older than expected ({reason}). {consequence} | yes |
| `backup.stale_restore.confirm` | Restore anyway | yes |

## iOS additions

Keys the iOS client needed that web did not have. Named by meaning, so Android can reuse them. The iOS Settings has a Network section (anti-censorship, ADR-0026, native only), a danger zone with its own footer, a Log out confirmation, a device-wording Forget this device body (the web one speaks of a browser), and the passkey / account-deletion prompts can be dismissed, which web does not report separately.

| Key | English | security_critical |
|---|---|---|
| `backup.rollback_refused.body` | The server returned an altered or outdated backup. Your account is safe; we continued without restoring the older data. | yes |
| `backup.rollback_refused.title` | Backup could not be restored safely | yes |
| `call.link.error.generic` | The call failed. |  |
| `common.error.qr_render` | Could not draw the code. Try again. |  |
| `common.connecting` | Connecting... |  |
| `settings.account.danger_zone.footer` | Log out keeps your data on this device. Forget this device removes it; you can only come back with your recovery phrase or another signed-in device. Delete account erases the account on the server and wipes this device; this cannot be undone. | yes |
| `settings.account.delete.error.cancelled` | Cancelled. Your account was not deleted. | yes |
| `settings.account.forget_device.body_device` | Everything on this device is removed and the device leaves your account. You will need your recovery phrase or another signed-in device to use this account here again. | yes |
| `settings.account.log_out.body` | Your messages stay on this device. Log back in with your password or passkey. | yes |
| `settings.account.log_out.title` | Log out of Lakko? |  |
| `settings.anti_censorship.footer` | When on, Lakko connects in a way that is harder for a network to detect and block. Your messages stay end-to-end encrypted either way. | yes |
| `settings.anti_censorship.status` | Status |  |
| `settings.anti_censorship.status.active` | Active |  |
| `settings.anti_censorship.status.active.sub` | Connections are hidden from network blocking. |  |
| `settings.anti_censorship.status.direct` | Direct connection |  |
| `settings.anti_censorship.status.unavailable` | Unavailable |  |
| `settings.anti_censorship.status.unavailable.sub` | Could not set up the hidden connection. Check your network, then turn the switch off and on to try again. |  |
| `settings.anti_censorship.status.unconfigured` | Not available in this build |  |
| `settings.anti_censorship.status.unconfigured.sub` | This build was made without the setting. |  |
| `settings.anti_censorship.title` | Anti-censorship |  |
| `settings.anti_censorship.toggle` | Hide Lakko from network blocking |  |
| `settings.data.ready` | Export ready. Tap Share to save it. |  |
| `settings.data.share` | Share export |  |
| `settings.passkeys.remove.error.cancelled` | Cancelled. The passkey was not removed. | yes |
| `settings.profile.name.hint` | Shown to the people you talk to. Not unique, at most {count, plural, one {# character} other {# characters}}. |  |
| `settings.recovery.phrase.not_saved_here` | Not saved on this device. Keep your written copy safe. | yes |
| `settings.recovery.phrase.saved_here` | Saved on this device. | yes |
| `settings.recovery.phrase.sub` | The 24 words you wrote down at signup restore your identity, contacts and messages on a new device. | yes |
| `settings.recovery.sign_in_first` | Sign in first to manage recovery. |  |
| `settings.section.danger_zone` | Danger zone |  |
| `settings.section.network` | Network |  |

`settings.profile.name.hint` takes `count` (64). Where the iOS wording differed from an existing key, the catalog wording is used: the devices list footer (`settings.devices.list.lede`), the recovery page lede, the authenticator and email / phone enrolment copy, the passkey lede and nickname placeholder (`settings.passkeys.name.placeholder` names a MacBook, fine for a phone too), SMS is shown as `common.phone`, the call-link title reads "Join a call", and the rollback alarm's button is `common.dismiss` rather than "OK". The call-link page also uses `call.guest.working.title` (iOS builds the guest path).

## Android additions

Keys the Android client needed that neither web nor iOS had. Named by meaning, so iOS can reuse them. Android reuses the iOS additions above (Network section, danger zone footer, Log out confirmation, `settings.account.forget_device.body_device`, the cancelled passkey / deletion lines, the recovery-phrase status lines, `settings.profile.name.hint`, `call.link.error.generic`, `common.connecting`, `backup.rollback_refused.*`).

| Key | English | security_critical |
|---|---|---|
| `backup.stale_restore.reason.anchor_unreadable` | this device's record of your last backup is unreadable or was tampered with | yes |
| `backup.stale_restore.reason.below_floor` | older than the last backup restored on this device | yes |
| `backup.stale_restore.reason.head_stripped` | its signature was removed and it differs from the last backup this device knew | yes |
| `backup.stale_restore.reason.too_old` | it was signed longer ago than a current backup would be | yes |
| `common.collapse` | Collapse |  |
| `common.expand` | Expand |  |
| `settings.anti_censorship.status.connecting.sub` | Setting up the hidden connection. |  |
| `settings.devices.remove.body` | This device will lose access to your messages from that moment on. You may be asked for your passkey. | yes |
| `settings.devices.remove.confirm_named` | Remove {device} | yes |
| `settings.devices.remove.title` | Remove {device}? | yes |
| `settings.notifications.off.body` | Notifications are off, so Lakko can only ring while it is open. Turn them on so calls and messages reach you when it is not. |  |
| `settings.notifications.off.title` | Turn on notifications |  |
| `settings.passkeys.lede_device` | Sign in with your fingerprint, face or screen lock, or a security key, instead of your password. Your recovery phrase always works too. |  |
| `settings.recovery.contact.sub` | A one-time code sent to your email or phone. |  |
| `settings.recovery.section.access` | Access |  |
| `settings.recovery.section.identity` | Identity |  |

`backup.stale_restore.reason.*` fill `{reason}` of `backup.stale_restore.body`: the restore verdict carries a reason code (Android `RestoreReason`), never an English sentence; the English diagnostic goes to the log only. The hard refusal shows `backup.rollback_refused.*` with no reason. `{device}` in `settings.devices.remove.*` is the rendered row label (`settings.devices.this_device` / `settings.devices.device_n`). `settings.passkeys.lede_device` exists because `settings.passkeys.lede` names Touch ID and Face ID.

Where the Android wording differed from an existing key, the catalog wording is used: the Recovery and Devices page ledes, the authenticator and email / phone enrolment copy (the code field labels are `recovery.totp.code_label` / `recovery.contact.code_label`, SMS is `common.phone`), the Profile preview label and saved line, the add-device option details (`devices.add.side.*_detail`), the device-list footer, the delete-account copy, the data section, the autoplay line, and the call-link page (`call.link.*`, "Go back" is `common.back`). The passkey nickname field uses `settings.passkeys.name.label` without an example. Removing a passkey now asks first (`settings.passkeys.remove.*`), as on web.
