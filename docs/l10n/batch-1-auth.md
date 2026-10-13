<!--
SPDX-License-Identifier: AGPL-3.0-or-later
SPDX-FileCopyrightText: 2026 Lakko Contributors
-->

# Localization batch 1: sign-in, sign-up and recovery keys

ADR-0061 S5, batch 1. Every user-visible string of the screens a person sees before and while getting into an account moved from web JSX literals into `l10n/catalog/en.json`. The iOS and Android clients map their own literals of the same screens onto these keys; names follow meaning, not the web component.

> 2026-10-02: the recovery keys were reworked (ADR-0072: 12-word phrase, three words checked on the device, passkeys that restore keys; guardian shares and email / phone codes removed). Rows for deleted keys were dropped and the English column was refreshed; keys added since are not listed here. `l10n/catalog/en.json` is the source of truth.

224 keys added, 66 of them `security_critical` (they render in English until a translation is `reviewed`, and a signed update can never change them).

## Conventions

- Arguments: `{name}` is text; `{count, plural, ...}` is a number the platform formats for the locale.
- Word positions, lists of positions (`{positions}`, `{position}`), HTTP statuses (`{status}`) and codes are passed as **text in Latin digits**, never through the number formatter (ADR-0061 open question 2). A list of positions is joined by the platform list formatter for non-English languages (web: `Intl.ListFormat`), keeping the digits Latin.
- `{link}`, and `{username}` / `{positions}` where the comment says "shown bold", are parts rendered as elements (a link, bold text) inside the sentence; web renders them with `tRich` (`apps/web/src/l10n/rich.tsx`). `{username}` is passed as `@name`.
- Errors render from their code, never from the core or server message (ADR-0061 section 6). Each flow has a `*.error.generic` fallback key.
- The brand name `Lakko` and the example code `123456` stay literal.

## Web-only keys

- `chats.empty.select`: the empty chat pane; iOS/Android have their own empty states.
- `call.guest.working.title`: guest account from a web call link (ADR-0032); natives use it only if they build the guest path.

Every other key applies to all three clients.

## Shared

| Key | English | security_critical |
|---|---|---|
| `common.continue` | Continue |  |
| `common.cancel` | Cancel |  |
| `common.try_again` | Try again |  |
| `common.done` | Done |  |
| `common.copy` | Copy |  |
| `common.copied` | Copied |  |
| `common.dismiss` | Dismiss |  |
| `common.loading` | Loading... |  |
| `common.checking` | Checking... |  |
| `common.looking_up` | Looking up... |  |
| `common.sending` | Sending... |  |
| `common.preparing` | Preparing... |  |
| `common.more_options` | More options |  |
| `common.error.offline` | Could not reach Lakko. Check your connection and try again. |  |
| `common.error.unavailable` | That is not available right now. Try again later. |  |
| `common.error.server_unreachable` | Could not reach the server. Check your connection. |  |
| `common.error.too_many_attempts` | Too many attempts. Wait a moment, then try again. |  |
| `common.error.too_many_attempts_minutes` | Too many attempts. Try again in {minutes, plural, one {# minute} other {# minutes}}. |  |

## Auth frame (AuthLayout)

| Key | English | security_critical |
|---|---|---|
| `auth.brand.tagline` | Your messages, your rules. |  |
| `auth.brand.body` | Chats, groups, channels and calls, end-to-end encrypted by default. Lakko's servers never see your messages. | yes |
| `auth.brand.foot` | Open source. No phone number required. |  |

## Login

| Key | English | security_critical |
|---|---|---|
| `auth.username.label` | Username |  |
| `auth.password.label` | Password |  |
| `auth.password.show` | Show password |  |
| `auth.password.hide` | Hide password |  |
| `auth.login.title` | Welcome back |  |
| `auth.login.lede` | Log in to continue. |  |
| `auth.login.submit` | Log in |  |
| `auth.login.submitting` | Logging in... |  |
| `auth.login.passkey` | Use passkey |  |
| `auth.login.forgot` | Forgot password? |  |
| `auth.login.no_account` | New to Lakko? {link} |  |
| `auth.error.wrong_credentials` | Wrong username or password. Check them and try again. | yes |
| `auth.passkey.error.generic` | Could not sign in with the passkey. Try again, or use your password. |  |
| `auth.passkey.error.cancelled` | Passkey sign-in cancelled. |  |
| `auth.passkey.error.unsupported` | This browser doesn't support passkeys. |  |
| `auth.passkey.error.server_disabled` | Server hasn't enabled passkey support. |  |
| `auth.passkey.error.not_registered` | No passkey is registered for this account. Sign in with password first, then add a passkey from Home. |  |
| `auth.passkey.error.failed_status` | Passkey sign-in failed ({status}). |  |
| `auth.passkey.error.network` | Network error during passkey sign-in. |  |

## Password explainer (login + signup)

| Key | English | security_critical |
|---|---|---|
| `auth.explainer.title` | How Lakko protects your password |  |
| `auth.explainer.never_leaves` | Your password never leaves your device. Lakko checks it without ever receiving it, so nobody at Lakko can read it, and nobody can reset it for you. | yes |
| `auth.explainer.same_answer` | A wrong username and a wrong password get the same answer, so nobody can use the login screen to find out which usernames exist. |  |
| `auth.explainer.ways_back` | If you forget it, you can get back in with another device that is still logged in, a passkey, your recovery phrase, three words from it, or an authenticator app. |  |

## Signup

| Key | English | security_critical |
|---|---|---|
| `auth.signup.title` | Create your account |  |
| `auth.signup.lede` | Your password never leaves your device. You will get a recovery phrase to write down next. | yes |
| `auth.signup.submit` | Create account |  |
| `auth.signup.name.label` | Name (optional) |  |
| `auth.signup.have_account` | Already have an account? {link} |  |
| `auth.signup.error.enter_username` | Enter a username. |  |
| `auth.signup.error.fix_name` | Fix your name before continuing. |  |
| `auth.signup.error.username_taken` | That username is taken. Choose another one. |  |
| `auth.signup.error.generic` | Could not create the account. Try again. |  |
| `auth.signup.error.rate_limited` | Too many new accounts from this network right now. Try again later. |  |
| `auth.signup.working.title` | Creating your account... |  |
| `auth.signup.working.line` | This takes a little while, once. You will not wait like this again. |  |
| `call.guest.working.title` | Getting you into the call... |  |
| `auth.password.confirm.label` | Confirm password |  |
| `auth.password.error.too_short` | Use at least {count, plural, one {# character} other {# characters}}. |  |
| `auth.password.error.mismatch` | The passwords do not match. Type the same password twice. |  |
| `username.error.empty` | Enter a username |  |
| `username.error.too_short` | At least {count, plural, one {# character} other {# characters}} |  |
| `username.error.too_long` | At most {count, plural, one {# character} other {# characters}} |  |
| `username.error.invalid_characters` | Only Latin letters, digits and _ |  |
| `username.error.unknown` | This username can't be used |  |
| `display_name.error.line_break` | One line only |  |
| `display_name.error.control` | Remove invisible or text-direction characters | yes |
| `display_name.error.too_long` | At most {count, plural, one {# character} other {# characters}} |  |
| `display_name.error.too_many_bytes` | This name is too long |  |
| `display_name.error.unknown` | This name can't be used |  |

## Recover (method list)

| Key | English | security_critical |
|---|---|---|
| `recovery.title` | Get back in |  |
| `recovery.lede` | Choose how. |  |
| `recovery.access_only_note` | An authenticator code logs you in. On a device that has never held this account you then still need another device, a passkey or your recovery phrase. | yes |
| `recovery.remembered` | Remembered your password? {link} |  |
| `recovery.methods.back` | Back to all methods |  |
| `recovery.method.phrase.title` | Recovery phrase |  |
| `recovery.method.phrase.sub` | Type your 12 words and choose a new password. |  |
| `recovery.method.phrase.restore_sub` | Type your 12 words. |  |
| `recovery.method.device.title` | Another device |  |
| `recovery.method.device.sub` | If you have one that is still logged in. |  |
| `recovery.method.words.title` | Three words from your phrase |  |
| `recovery.method.words.sub` | Type three words of your recovery phrase. This device checks them. |  |
| `recovery.method.totp.title` | Authenticator code |  |
| `recovery.method.totp.sub` | A code from your authenticator app. |  |
| `recovery.username_required` | Enter the username of the account. |  |
| `recovery.username_required_to_recover` | Enter the username of the account to recover. |  |

## Recover: phrase

| Key | English | security_critical |
|---|---|---|
| `recovery.phrase.intro` | Type your 12 words in order, then choose a new password. | yes |
| `auth.password.new.label` | New password |  |
| `auth.password.new_confirm.label` | Confirm new password |  |
| `recovery.submit` | Recover account |  |
| `recovery.error.no_match` | That did not match this account. Check the words and the username, then try again. | yes |
| `recovery.error.generic` | Could not recover the account. Check the words and try again. |  |

## Recover: three words

| Key | English | security_critical |
|---|---|---|
| `recovery.words.prompt` | Type words {positions} of your recovery phrase. | yes |
| `recovery.words.tries_left` | {count, plural, one {# try left} other {# tries left}} before a 15-minute pause. | yes |
| `recovery.words.error.no_match` | Those words did not match. | yes |
| `recovery.words.error.generic` | The words matched, but logging in failed. Try again. |  |

## Recover: another device (new device side)

| Key | English | security_critical |
|---|---|---|
| `recovery.device.intro` | Enter your username, then open Lakko on a device that is still logged in and type the code it shows. Nothing is sent that Lakko's servers can read. No camera needed; a QR code is there if you prefer. | yes |
| `device_link.show_code` | Show pairing code |  |
| `device_link.qr.show` | Show a QR code instead |  |
| `device_link.qr.scan_hint` | On your other device, open Settings, then Devices, then Add a device, and scan this code. | yes |
| `device_link.transferring` | Transferring... |  |
| `device_link.restored` | Restored. |  |
| `device_link.error.declined` | Declined on another device. | yes |
| `device_link.error.no_identity` | The other device confirmed but did not send your identity. Update Lakko on it and try again, or use your recovery phrase. |  |
| `device_link.error.timeout` | The other device did not answer in time. Try again. |  |
| `device_link.error.ask_unavailable` | Asking your other device is not available right now; the QR code still works. |  |
| `device_link.error.ask_unreachable` | Could not reach the server to ask your other device; the QR code still works. |  |
| `device_link.error.request_limited` | Too many requests to your other devices. Try again in a few minutes. |  |
| `device_link.error.not_let_in` | Your other device sent the identity, but this device could not be let in. Try again, or use your recovery phrase. |  |
| `device_link.error.identity_mismatch` | The identity received from your other device doesn't match this account's published identity — refusing to use it. | yes |
| `device_link.error.generic` | Could not restore from your other device. Try again, or use your recovery phrase. |  |

## Device link: signed-in device side (DeviceRequestPrompt)

| Key | English | security_critical |
|---|---|---|
| `device_link.request.title` | Is this you? | yes |
| `device_link.request.decline` | It is not me | yes |
| `device_link.request.adding` | Adding the device... |  |
| `device_link.request.declined` | Declined. If this keeps happening, change your password. | yes |
| `device_link.request.added` | Device added. |  |
| `device_link.request.error.generic` | Could not add the device. Try again. |  |
| `device_link.request.error.no_device_key` | No device key on this browser. Sign in again. |  |
| `device_link.pairing.wrong_account` | This code is for a different account. Check that the new device uses the same username as this one. | yes |
| `device_link.pairing.unsupported` | This code comes from a different version of Lakko. Update Lakko on both devices and try again. |  |
| `device_link.pairing.needs_username` | Set a username first to add another device. |  |

## Recover: authenticator

| Key | English | security_critical |
|---|---|---|
| `recovery.totp.intro` | Use the six-digit code from the authenticator app you set up in Settings. It logs you in; on a device that has never held this account you then still need another device, a passkey or your recovery phrase. |  |
| `recovery.totp.code_label` | Code from the app |  |
| `recovery.totp.error.enter_code` | Enter the six-digit code from your authenticator app. |  |
| `recovery.totp.error.wrong` | That code did not work. Check the app and try the next one. |  |
| `recovery.totp.error.wrong_username` | That code did not work. Check the username and the app, then try again. |  |
| `recovery.totp.error.unavailable` | Authenticator code is not available here. Choose another way. |  |
| `recovery.totp.error.generic` | Could not check the code. Try again. |  |

## Recovery phrase screens (signup phrase, fresh phrase, entry, grid)

| Key | English | security_critical |
|---|---|---|
| `recovery_phrase.label` | Recovery phrase |  |
| `recovery_phrase.word_label` | Word {position} | yes |
| `recovery_phrase.word_placeholder` | word {position} | yes |
| `recovery_phrase.copy_failed` | Could not copy. Write the words down instead. | yes |
| `recovery_phrase.save.title` | Save your recovery phrase | yes |
| `recovery_phrase.step` | Step {current} of {total} |  |
| `recovery_phrase.save.lede` | These 12 words bring your account back on a new device if you lose your password and your devices. Write them down in order and keep them somewhere safe. | yes |
| `recovery_phrase.save.no_screenshot` | Do not keep them as a screenshot or in a plain text file. | yes |
| `recovery_phrase.save.written` | I have written them down | yes |
| `recovery_phrase.check.title` | Check your notes | yes |
| `recovery_phrase.check.lede` | Type words {positions} from what you wrote down. | yes |
| `recovery_phrase.check.acknowledge` | I understand that without these words, another logged-in device or a passkey that restores keys, my chats cannot be brought back. | yes |
| `recovery_phrase.check.back` | Back to the words |  |
| `recovery_phrase.check.mismatch_one` | Word {position} does not match. Check your notes and try again. | yes |
| `recovery_phrase.check.mismatch_many` | Words {positions} do not match. Check your notes and try again. | yes |
| `recovery_phrase.fresh.title` | Save your new recovery phrase | yes |
| `recovery_phrase.fresh.lede` | The account keys for {username} were reset. These 12 words bring the account back on a new device if you lose access again. Write them down in order and keep them somewhere safe. | yes |
| `recovery_phrase.fresh.safety_note` | Your contacts will see that your safety number changed and will need to verify you again before messaging you securely. | yes |
| `recovery_phrase.guest.lede` | Your account has no password. These 12 words bring it back on another device, or on this one after its data is cleared. Write them down in order and keep them somewhere safe. | yes |
| `recovery_phrase.fresh.acknowledge` | I have written down these 12 words and understand that without them, another logged-in device or a passkey that restores keys, my chats cannot be brought back. | yes |
| `recovery_phrase.fresh.finishing` | Finishing... |  |
| `recovery_phrase.fresh.error.generic` | Could not finish. Try again. |  |

## Restore identity (signed in on a device without the identity)

| Key | English | security_critical |
|---|---|---|
| `restore_identity.title` | Restore your keys | yes |
| `restore_identity.lede` | You are logged in as {username} on a device that does not have this account's keys yet. Until it does, you cannot read or send messages here. | yes |
| `restore_identity.device_intro` | A device that is still logged in shows a six-digit code; type it here. The transfer is end-to-end encrypted; Lakko's servers cannot read it. No camera needed; a QR code is there if you prefer. | yes |
| `restore_identity.submit` | Restore |  |
| `restore_identity.submitting` | Restoring... |  |
| `restore_identity.log_out` | Log out |  |
| `restore_identity.error.nothing` | Nothing to restore. Log in first. |  |
| `restore_identity.error.phrase_mismatch` | That recovery phrase doesn't match this account's identity. Check the phrase, or pair an existing device instead. | yes |
| `restore_identity.error.not_published` | No published identity was found to verify the recovered identity. Try again, or restore from a device that still has your account. | yes |
| `restore_identity.error.unreachable` | Could not reach the directory to verify the recovered identity. Check your connection and try again. |  |
| `restore_identity.error.invalid_phrase` | That is not a valid recovery phrase. Check each word and try again. | yes |
| `restore_identity.error.generic` | Could not restore your identity. Try again. |  |
| `restore_identity.lost.summary` | I have none of these | yes |
| `restore_identity.lost.warning` | This resets your account keys. Your contacts will need to verify you again, messages on your lost devices stay unreadable, and you get a new 12-word recovery phrase to save. Only do this if you have lost your recovery phrase, every other device and every passkey that restores keys. | yes |
| `restore_identity.lost.create` | Reset account keys | yes |
| `restore_identity.lost.creating` | Resetting account keys... |  |
| `restore_identity.lost.error.generic` | Could not reset the account keys. Try again. |  |

## Security alert (signed-in banner)

| Key | English | security_critical |
|---|---|---|
| `security.identity_changed.title` | Your account's security key was changed. | yes |
| `security.identity_changed.body` | If that was you, on another device, dismiss this. If it was not, verify your safety numbers with your contacts and change your password and passkeys. | yes |

## Chats (signed-in home)

| Key | English | security_critical |
|---|---|---|
| `chats.empty.select` | Select a chat to start messaging |  |

## Android additions

Keys the Android client needed that neither web nor iOS had. Named by meaning, so iOS can reuse them.

| Key | English | security_critical |
|---|---|---|
| `common.back` | Back |  |
| `common.confirm` | Confirm |  |
| `common.optional` | Optional |  |
| `username.hint.shown_as` | Shown as {username} |  |
| `auth.passkey.error.wrong_account` | This passkey belongs to a different account than the one on this device. Log in with your password first. |  |
| `recovery.lede_account` | Choose how to get back into {username}. |  |
| `recovery.device.restoring` | Restoring {username} |  |
| `recovery_phrase.save.anyone_with_words` | Anyone who has these words can read your messages and act as you. Do not share them. | yes |
| `restore_identity.phrase_intro` | Type the 12 words in order. Lakko checks them against your account before continuing. | yes |
| `restore_identity.lost.sub` | Reset account keys | yes |
| `restore_identity.not_you` | Not your account? |  |
| `device_link.qr.intro` | Show this code to a device that still has your account. It adds this device and transfers your identity end-to-end encrypted, with no phrase to type. | yes |
| `devices.scan.error.not_a_code` | That is not a Lakko device code. Point the camera at the code on your other device. |  |

`{username}` is passed with `@`, as elsewhere.

## iOS additions

Keys the iOS client needed that web did not have. Named by meaning, so Android can reuse them. `devices.add.*` and `devices.scan.*` are the signed-in device's QR flow (Settings, Devices, Add a device), which is how iOS links a device today; iOS has no "Is this you?" prompt, so the `device_link.request.*` confirm keys are not used there.

| Key | English | security_critical |
|---|---|---|
| `auth.password.hint` | At least {count, plural, one {# character} other {# characters}}. |  |
| `auth.signup.username.hint` | Latin letters, digits and _. Others find you by this name. |  |
| `auth.signup.name.hint` | Shown to your contacts. Leave it empty to show your username. |  |
| `common.paste` | Paste |  |
| `common.error.a11y` | Error: {message} |  |
| `username.a11y_hint` | Username: {message} |  |
| `recovery_phrase.error.incomplete` | Enter all 12 words of your recovery phrase. |  |
| `recovery.cached_account` | {username} on this device |  |
| `recovery.device.intro_qr` | Your other device links this one and carries your account across. |  |
| `device_link.qr.waiting` | Waiting for your other device to scan this code... |  |
| `device_link.error.name_other_account` | The name you typed belongs to a different account. Nothing was changed on this device. | yes |
| `restore_identity.error.bad_saved_account` | Something is wrong with the account saved on this device. Log out and log in again, or use your recovery phrase. |  |
| `devices.add.title` | Add a device |  |
| `devices.add.e2ee_note` | Linking is end-to-end encrypted between your two devices. | yes |
| `devices.add.side.show` | Show a code |  |
| `devices.add.side.scan` | Scan a code |  |
| `devices.add.side.show_detail` | This is the new device: it joins your account once another device scans its code. |  |
| `devices.add.side.scan_detail` | This device is already signed in: it scans the new device's code and lets it in. | yes |
| `devices.add.sign_in_first` | Sign in first to add a device. |  |
| `devices.add.joining` | Linking this device... |  |
| `devices.add.linked` | Linked. This device is now part of your account. |  |
| `devices.add.error.generic` | Could not link this device. Try again. |  |
| `devices.scan.intro` | Point your camera at the code your new device is showing. |  |
| `devices.scan.open_camera` | Open camera |  |
| `devices.scan.confirm.title` | Link this device? | yes |
| `devices.scan.confirm.body` | This gives the scanned device full access to your account: your messages, your contacts and your identity. Continue only if the code is on your own new device. | yes |
| `devices.scan.confirm.submit` | Link device | yes |
| `devices.scan.error.no_camera` | No back camera is available on this device. |  |
| `devices.scan.error.camera_denied` | Lakko cannot use the camera. Allow camera access in Settings, then try again. |  |
| `devices.scan.error.camera_failed` | The camera could not start. Try again. |  |

`{username}` is passed with `@`; `{message}` is an already rendered sentence.
