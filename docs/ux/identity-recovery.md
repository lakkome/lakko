<!-- SPDX-License-Identifier: AGPL-3.0-or-later -->
<!-- SPDX-FileCopyrightText: 2026 Lakko Contributors -->

# Identity recovery

UX spec for getting an account back: on a device that does not hold its keys
(a new browser, a new phone, a reinstalled app), or on a device that still
holds them when the password is forgotten. Architecture:
[ADR-0064](../adr/0064-recovery-model-simplification.md), which amends
[ADR-0049](../adr/0049-identity-recovery-word-vault.md),
[ADR-0018](../adr/0018-recovery-factor-cascade.md) and
[ADR-0017](../adr/0017-phrase-seeded-identity.md).

**Rewritten 2026-10-02** for the maintainer's decisions of that day. What
changed against the previous revision of this file:

- The recovery phrase is **12 words**, not 24.
- **Three words no longer restore keys.** They sign the person in on a device
  that still has its keys, and nothing more.
- **A passkey can restore keys** on every client, when the person turned that
  on for the passkey and the platform supports it.
- **Another device is the first route** everywhere.
- Guardian shares and the email / phone code are gone.

Build status is recorded in ADR-0064 and `docs/PRODUCT-READINESS.md`, not
here. Two facts about today's clients that this spec changes are stated where
they matter: neither iOS nor Android has a three-word flow (both
`RecoverScreen` headers say so), and both refuse a passkey sign-in on a device
with no cached account.

## 1. Two situations, one vocabulary

Everything below uses two words the UI also uses:

- **Signed in** means the server accepts this device for the account. The
  device can then reach the account's settings and its other devices.
- **Keys** means what makes the person the same person to their contacts and
  lets the device read and send messages. Keys never come from the server
  alone.

| Situation | What is missing | Routes, in display order |
|---|---|---|
| New device (or reinstalled app, cleared browser) | Signed in and keys | Another device, passkey, 12-word phrase; last resort: start over with a new identity |
| Forgot the password, this device still has its keys | Signed in only | Passkey, three words, authenticator code (and every route of the row above also works) |

| Goal | Entry point |
|---|---|
| Forgot the password | Log in > "Forgot password?" > **Get back in** (section 2) |
| Signed in with the password on a device without keys | Log in > password accepted > **Restore your keys** (section 8) |
| Signed in with a passkey that did not bring keys | Log in > Use a passkey > **Restore your keys** (section 8) |
| Add a second device from the first | Settings > Devices > Add a device (unchanged) |

## 2. The recovery screen: Get back in

One list of tappable rows (the `MethodList` component on web, the same row
pattern on iOS and Android), in this order on every client:

```
Get back in
Choose how.

  Brings back your chats
  [devices]  Another device                 >
             If you have one that is still signed in.
  [passkey]  Passkey                        >
             Signs you in. Brings back your chats too if you
             turned that on for this passkey.
  [key]      Recovery phrase                >
             Type your 12 words and choose a new password.

  Signs you in on this device
  [lines]    Three words                    >
             Three words from your phrase, if this device still
             has your chats.
  [shield]   Authenticator code             >
             A code from your authenticator app.

Three words or an authenticator code sign you in. On a device
that has never held this account you then still need another
device, a passkey or your recovery phrase.

Remembered your password? Log in
```

Rules:

- **Another device is first** and carries "if you have one". The list is the
  same for every account: before a session the server must not say whether an
  account has other devices, a passkey, a three-word shortcut or an
  authenticator, so no row is hidden, greyed or badged from server state. The
  order and the subtitles do the guiding.
- The two group labels are plain text headings, not collapsible.
- The note under the list (the access-only note) is always visible; it is the
  one place the screen explains the difference.
- Picking a row replaces the list with that method's form under a heading
  that leads back to the list (existing behaviour).
- The username typed in one form is kept when the person goes back and picks
  another. It is pre-filled from the device's cached account when there is
  one. Passkey is the only method that does not ask for it.

### States every form shares

| State | What the person sees |
|---|---|
| Idle | The form; the primary button is disabled until the input is complete and well-formed, so a typo never spends a server attempt. |
| Working | The primary button shows one word ("Checking..."), inputs are disabled, the back heading is disabled. |
| Wrong input | One sentence under the inputs, the inputs keep focus. The sentence never says which part was wrong (name or secret): "That did not match. Check the name and try again." |
| Rate limited (`429`) | "Too many attempts. Try again in 14:59." with a live countdown from the server's retry time; the primary button stays disabled until it reaches zero; the other methods stay usable through the back heading. |
| Unavailable (`503`) | "This method is not available right now. Choose another one." with a button back to the list. The row stays in the list (hiding it would differ per server, not per account, so it leaks nothing, but a row that vanishes is harder to explain than one that says why). |
| Offline / server unreachable | "Could not reach the server. Check your connection." Inputs keep their contents. |
| Success | Section 7 decides what comes next. |

## 3. Another device

Unchanged from the previous revision in mechanics (the code is shown on the
signed-in device and typed on the new one, consent first, one try per
request); what changes is its position and copy. It brings both sign-in and
keys.

### On the new device

```
Another device

Which account?   [ @name            ]

Open Lakko on a device that is still signed in. It will ask
whether this is you, then show a six-digit code. Enter it here.

[ __ __ __ ]

[ Show a QR code instead ]
```

States: **waiting for the other device** (field disabled, "Waiting for your
other device..."), **checking**, **wrong code** ("That code does not match.
Start again and enter the code your other device shows." - one try per
request; Start again posts a new request), **confirmed** ("Restoring..." then
the app opens), **declined** ("Declined on another device."), **expired**
after 10 minutes ("This request expired. Start again."), plus the shared
`429` and offline states. A name nobody holds behaves exactly like a real
account whose devices never answer.

### On the signed-in device

A modal over whatever screen is open (web), or a notification that opens it
(iOS, Android).

Step 1: "Someone is signing in to your account on a new device. Is this you?"
with **Yes, it's me** and **It is not me**. Nothing is sent for the request
until **Yes, it's me**.

Step 2: the code, large, three pairs, with "Never read this code to anyone.
Lakko will never ask for it." and **It is not me**.

Then "Adding the device..." and "Adding it to your chats... (12 of 30)"; it
can be dismissed and continues in the background; a failure surfaces as a
banner ("Some chats could not be added. Open Devices to retry."). A wrong
code closes the modal with "The code did not match. Nothing was added."
**It is not me** closes it with "Declined. If this keeps happening, change
your password." The modal never shows a device name or location.

## 4. Passkey

One row, two outcomes, and the form says which one happened.

```
Passkey

Use the passkey you saved for Lakko. Your device will ask for
your face, fingerprint or screen lock.

[ Use a passkey ]
```

- No username field: the system sheet lists the passkeys available for
  Lakko.
- The system sheet is the platform's own (browser dialog, iOS sheet, Android
  bottom sheet). Lakko shows nothing over it.

| State | What the person sees |
|---|---|
| Working | "Waiting for your passkey..." while the system sheet is up, then "Checking...". |
| Cancelled in the system sheet | Back to idle, no error text. |
| No passkey for Lakko on this device | "No passkey for Lakko was found on this device. Choose another way." |
| Signed in, keys restored | "Signed in. Your chats are being restored..." then section 7. |
| Signed in, this passkey does not carry keys | Section 8 opens with its passkey line: "Your passkey signed you in. It does not carry your keys." |
| Signed in, the stored keys did not open or did not match | Section 8 opens with: "Your passkey signed you in, but the keys saved with it could not be used. Nothing was changed." This is the fail-closed check against the account's published identity; it is never retried silently. |
| Passkeys not supported here | The row stays; the form reads "This device or browser does not support passkeys. Choose another way." |

A device that already holds the account's keys simply lands signed in.

## 5. Recovery phrase (12 words)

The existing phrase input with twelve numbered slots: autocomplete from the
word list after two letters, a red outline for a word not in the list, paste
of a whole phrase into any slot fills all twelve.

```
Recovery phrase

Which account?   [ @name            ]

Type your 12 words in order, then choose a new password.

 1 [______]  2 [______]  3 [______]  4 [______]
 5 [______]  6 [______]  7 [______]  8 [______]
 9 [______] 10 [______] 11 [______] 12 [______]

New password      [ ............ ]
Repeat password   [ ............ ]

[ Recover ]
```

- **Recover** is disabled until all twelve slots hold listed words, the
  phrase passes its built-in check, and the two passwords match the password
  rules. A phrase that fails the built-in check shows "These words do not
  form a valid recovery phrase. Check the order and the spelling." before
  anything is sent.
- Wrong phrase for this name (server refusal): the shared wrong-input
  sentence.
- Right phrase, but the keys it gives are not the ones published for the
  account: "This phrase belongs to a different identity than the one
  published for @name. Nothing was changed."
- Success: signed in, keys restored, password replaced; section 7.

Grid: four columns on wide layouts, three on tablets, two on phones. The slot
numbers are Latin digits in every language.

## 6. Three words and the authenticator code (sign-in only)

### Three words

```
Three words

Which account?   [ @name            ]
[ Continue ]
```

then, with the positions the server returned (ascending; the same every time
for the account):

```
Type words 3, 7 and 11 of your recovery phrase.

  3 [______]    7 [______]    11 [______]

This signs you in. It does not bring your chats to a device
that has never held this account.

[ Sign in ]
```

- Fields autocomplete and refuse unlisted words like the phrase slots, so a
  typo never spends an attempt.
- Pasting a full 12-word phrase into any field fills the three positions from
  it.
- **Wrong words:** fields shake and clear, "Those words did not match."
  After the first miss, muted text shows the client's own count for this
  session: "N tries left before a 15-minute pause."
- **Rate limited:** the shared `429` state.
- **The shortcut is off, or was switched off by too many wrong tries:** the
  server answers these exactly like wrong words, on purpose. The client
  therefore cannot say "off". After the fifth miss in a session the form adds
  one line: "If these are the right words, the three-word shortcut may be off
  for this account. Choose another way." It keeps accepting tries.
- **Success:** section 7. On a device that holds the keys the next screen is
  "Choose a new password" (section 7.1).

A name nobody holds gets positions too and then always answers "did not
match".

### Authenticator code

Unchanged: username, a six-digit code, **Sign in**; wrong code, `429` and
`503` as in the shared table; its subtitle and intro lose every mention of
email or phone.

## 7. After a method succeeds

| The device now has | Came from | Next screen |
|---|---|---|
| Sign-in and keys, password already replaced | Recovery phrase | The app. |
| Sign-in and keys | Another device; a passkey that carries keys | 7.1 if the person came through "Forgot password?", else the app. |
| Sign-in, and the device already held the keys | Three words, authenticator code, passkey | 7.1. |
| Sign-in only, no keys on this device | Three words, authenticator code, a passkey without keys | Section 8. |

### 7.1 Choose a new password

Shown once, right after a successful forgot-password route on a device that
holds the keys.

```
Choose a new password

You are signed in. Set a new password so you can log in next time.

New password      [ ............ ]
Repeat password   [ ............ ]

[ Save password ]        Not now
```

- The device proves it holds the account's keys to replace the password; the
  person types nothing but the new password.
- **Not now** opens the app; Settings > Security > Password offers the same
  form later. Until then the old (forgotten) password stays valid.
- States: working, password rules not met (inline, per field), `429`,
  offline, and "Could not save the password. Try again." Success opens the
  app with a one-line confirmation.

### 7.2 What the chat list shows afterwards

| Route | Chat list |
|---|---|
| Another device | Fills as the other device adds this one to each chat; banner "Catching up with your other device..." until done. |
| Recovery phrase or passkey, another device of the account is still signed in | Fills as that device notices this one and adds it (this can take until that device is next open); banner "Your other devices are adding this one to your chats." |
| Recovery phrase or passkey, no other device left | Empty list with a card: "Your keys are restored. Your chats were on devices you no longer have. Your contacts will see you as the same person and can add you back." with a button to Contacts. Not a loading state; no spinner. |

## 8. Restore your keys (signed in, no keys on this device)

Shown instead of the app whenever the device is signed in but holds no keys:
after a password login on a new device, after three words or an authenticator
code on a new device, after a passkey that did not bring keys.

```
Restore your keys

You are signed in as @name on a device that does not have this
account's keys yet. Until it does, you cannot read or send
messages here.

  [devices]  Another device                 >
             If you have one that is still signed in.
  [passkey]  Passkey                        >
             If you turned on "restore keys" for a passkey.
  [key]      Recovery phrase                >
             Type your 12 words.

  I have none of these  v

Log out
```

- The optional first line states how the person got here when it helps:
  "Your passkey signed you in. It does not carry your keys." or "Your
  passkey signed you in, but the keys saved with it could not be used.
  Nothing was changed."
- **Three words and the authenticator are not on this screen.** They cannot
  restore keys, and offering them here would promise what they do not do.
- **Passkey** runs the passkey ceremony again and asks it for the keys. It is
  hidden when the person just arrived from a passkey that did not carry keys
  and the device has only that one passkey (the client knows what it just
  tried); it stays when they arrived another way.
- **Another device** is the section-3 flow without the username field (the
  account is known). It can also show a QR code.
- **Recovery phrase** is the twelve-slot input without the password fields.
  Mismatch: "This phrase belongs to a different identity than the one
  published for @name. Nothing was changed." Not published / unreachable keep
  their existing copy.
- **I have none of these** expands to the existing start-over panel: a
  warning ("This creates a new identity for your account. Your contacts will
  need to verify you again, messages on your lost devices stay unreadable,
  and you get a new 12-word recovery phrase to save. Only do this if you have
  lost your recovery phrase, every other device and every passkey that
  restores keys."), a typed confirmation, then the new phrase screen of
  section 9 with its acknowledge gate.
- **Log out** leaves the screen without changing anything.
- States per route: the shared table of section 2.

## 9. The phrase at signup

Three steps after the account form, the same on all three clients.

**Step 1, save.**

```
Your recovery phrase

These 12 words bring your account back on a new device if you
lose your password and your devices. Write them down in order
and keep them somewhere safe. Lakko cannot show them to you
again on another device and cannot reset them.

 1 lamp     2 river    3 cotton   4 ...
 ...                           12 ...

[ Copy ]   [ I wrote them down ]
```

- Numbered grid, same column rule as section 5.
- Words at the three-word positions are **highlighted** (accent outline, plus
  a small dot so the mark does not rely on colour), with one line under the
  grid: "If you forget your password on a device you already use, Lakko asks
  for words 3, 7 and 11 only." The line and the highlight are absent when the
  three-word shortcut could not be set up (server has it off, or the request
  failed); signup does not fail because of it.
- **Copy** copies the phrase and shows "Copied. Clear your clipboard after
  you paste it somewhere safe."

**Step 2, confirm.** Re-enter three of the words at positions chosen at
random for this screen (not the shortcut positions; the person must not
learn to keep only three words). A mismatch names the positions that are
wrong and keeps the right ones. **Back** returns to step 1.

**Step 3, offer a passkey** (only where passkeys are supported): "Add a
passkey so you can sign in with your face, fingerprint or screen lock." with
**Add a passkey** and **Not now**. Adding one continues into the choice of
section 10.2. This step is skippable and never blocks signup.

The same save and confirm steps are used for the phrase a start-over mints
(section 8).

## 10. Settings > Security

### 10.1 Recovery

```
Recovery

Brings back your chats on a new device
  Recovery phrase        12 words. Saved on this device.     >
  Passkeys               2, one restores keys.               >
  Other devices          1 signed in.                        >

Signs you in when you forget your password
  Three-word shortcut    On: words 3, 7 and 11.              >
  Authenticator app      Not set up.                         >
```

- **Recovery phrase** opens a screen that states whether this device holds
  the phrase ("Saved on this device" / "Not saved on this device") and, when
  it does, **Show phrase** behind the device's own unlock (system biometric
  or screen lock on iOS and Android; the account password on web). No server
  call.
- **Passkeys** opens 10.2. **Other devices** opens Devices.
- **Three-word shortcut** opens 10.3. **Authenticator app** opens the
  existing setup, unchanged.
- There is no email, phone or guardian row.

### 10.2 Passkeys, and letting a passkey restore keys

The list shows each passkey with its name, when it was added and last used,
and one of two labels: **Signs in and restores keys** or **Signs in only**.

Adding a passkey:

1. **Add a passkey** > name it > the system sheet creates it.
2. If the platform returned what is needed to protect keys with this passkey,
   the choice screen follows. Otherwise the result screen reads: "Passkey
   added. It signs you in. On this device and passkey provider it cannot
   restore your keys, so keep your recovery phrase." and the row is **Signs
   in only**.

The choice screen:

```
Let this passkey restore your keys?

If you turn this on, this passkey alone can bring your account's
keys to a new device. Passkeys are usually synced by Apple,
Google or your password manager. Whoever can use that account's
passkeys can then read your future messages and act as you.

If you leave it off, the passkey only signs you in. You restore
keys with your recovery phrase or another device.

You can change this later for each passkey.

[ Turn on ]        [ Sign-in only ]
```

- Neither button is pre-selected or styled as the default; both are the same
  weight. The person must pick one.
- **Turn on** needs the recovery phrase on this device. If the device does
  not hold it, a twelve-slot input follows ("Type your recovery phrase to
  turn this on."), checked against the account's published identity before
  anything is stored.
- Some platforms can only produce what is needed when the passkey is used,
  not when it is created. Then **Turn on** triggers one more system sheet
  ("Confirm with your passkey to finish.").
- States: working, cancelled (stays **Signs in only**), `503` ("Restoring
  keys with a passkey is not available on this server."), offline, success
  ("This passkey now restores your keys.").

Per passkey, later: a detail screen with the label and one action, **Turn on
restoring keys** (the same choice screen) or **Stop restoring keys**
(confirmation: "This passkey will only sign you in. Keys already restored on
other devices stay there."). Removing a passkey also removes what it stored
for restoring keys; the existing passkey-confirmation step for removal is
unchanged.

A passkey created before this feature, or on another client, shows **Signs in
only** until the person turns it on.

### 10.3 Three-word shortcut

```
Three-word shortcut

If you forget your password on a device you already use, type
words 3, 7 and 11 of your recovery phrase to sign in. It does not
bring your chats to a new device.

On                                   [ Turn off ]
```

| State | Screen |
|---|---|
| On | The positions, **Turn off** (confirmation: "You will need a passkey, an authenticator code, another device or your full phrase if you forget your password."). |
| Off | "Off." and **Turn on**. Turning on needs the phrase on this device (else the twelve-slot input, as in 10.2), takes about a second ("Setting up..."), then shows the positions: "Remember: words 3, 7 and 11." |
| Switched off by wrong tries | "Off. It was switched off after too many wrong tries. If that was not you, change your password." and **Turn on** (new positions are drawn). |
| Not available on this server (`503`) | "Not available on this server." No action. |

The positions are shown here at any time; they are not a secret.

## 11. Copy that changes

Candidates for the catalog (`l10n/catalog/en.json`); this spec does not edit
it. Keys are existing ones unless marked new.

Strings that say 24:

| Key | New source text |
|---|---|
| `recovery.method.phrase.sub` | Type your 12 words and choose a new password. |
| `recovery.method.phrase.restore_sub` | Type your 12 words. |
| `recovery.phrase.intro` | Type your 12 words in order, then choose a new password. |
| `recovery_phrase.error.incomplete` | Enter all 12 words of your recovery phrase. |
| `recovery_phrase.fresh.acknowledge` | I have written down these 12 words and understand that losing them means losing access to this account. |
| `recovery_phrase.fresh.lede` | You created a new identity for {username}. These 12 words are the only way to recover it if you lose access again. Write them down in order and keep them somewhere safe. |
| `recovery_phrase.guest.lede` | Your account was created without a password. These 12 words are the only way back into it on another device or after clearing this browser. Write them down in order and keep them somewhere safe. |
| `recovery_phrase.save.lede` | These 12 words bring your account back on a new device if you lose your password and your devices. Write them down in order and keep them somewhere safe. |
| `restore_identity.phrase_intro` | Type the 12 words in order. Lakko checks them against your account before continuing. |
| `settings.account.forget_device.body` | You'll lose the cached account on this browser. The way back is another device, a passkey that restores keys, or the 12-word recovery phrase you wrote down at signup. |
| `settings.account.vault.on` | Three-word shortcut: on (words {positions}). It signs you in on a device you already use. |
| `settings.account.vault.unavailable` | Three-word shortcut: not available on this server. |
| `settings.recovery.phrase.sub` | The 12 words you wrote down at signup restore your keys and contacts on a new device. |

Strings whose meaning changes:

| Key | New source text |
|---|---|
| `recovery.title` | Get back in |
| `recovery.access_only_note` | Three words or an authenticator code sign you in. On a device that has never held this account you then still need another device, a passkey or your recovery phrase. |
| `recovery.method.device.sub` | If you have one that is still signed in. |
| `recovery.method.words.sub` | Three words from your phrase, if this device still has your chats. |
| `recovery.words.intro` | Type three words from your recovery phrase to sign in. First, which account? |
| `recovery.words.prompt` | Type words {positions} of your recovery phrase. |
| `recovery_phrase.save.vault_note` | If you forget your password on a device you already use, Lakko asks for words {positions} only. |
| `restore_identity.title` | Restore your keys |
| `restore_identity.lede` | You are signed in as {username} on a device that does not have this account's keys yet. Until it does, you cannot read or send messages here. |
| `restore_identity.lost.summary` | I have none of these |
| `restore_identity.lost.warning` | (section 8 text) |
| `settings.recovery.lede` | (replaced by the two group headings of 10.1) |
| `settings.passkeys.lede`, `settings.passkeys.lede_device` | Sign in with your face, fingerprint or screen lock instead of your password. A passkey can also restore your keys on a new device if you turn that on. |
| `auth.passkey.error.no_cached` | retired (a passkey now works on a new device) |

Strings to retire: `recovery.words.prompt_restore`,
`recovery.method.words.restore_sub`, `recovery.words.switched_off`,
`recovery.words.switched_off_restore`, `recovery.words.error.open_failed`,
`restore_identity.lede_no_words`, every `recovery.contact.*`,
`recovery.method.contact.*`, `recovery.error.code_send_limited` and
`settings.recovery.contact.*` key (the last three groups belong to the
removal already under way).

New keys (names are proposals): `recovery.group.keys`,
`recovery.group.signin`, `recovery.method.passkey.title`,
`recovery.method.passkey.sub`, `recovery.passkey.intro`,
`recovery.passkey.none_found`, `recovery.passkey.unsupported`,
`recovery.words.signin_only`, `recovery.words.maybe_off`,
`recovery.error.rate_limited` (with `{time}`), `recovery.error.unavailable`,
`recovery.new_password.title`, `recovery.new_password.lede`,
`recovery.new_password.not_now`, `restore_identity.from_passkey_no_keys`,
`restore_identity.from_passkey_unusable`,
`restore_identity.method.passkey.sub`, `chats.restored.other_devices_adding`,
`chats.restored.no_devices_card`, `settings.recovery.group.keys`,
`settings.recovery.group.signin`, `settings.recovery.words.*` (title, lede,
on, off, off_by_tries, turn_on, turn_off, turn_off_confirm, setting_up,
remember), `settings.passkeys.label.restores_keys`,
`settings.passkeys.label.signin_only`, `settings.passkeys.keys_choice.*`
(title, body_on, body_off, later, turn_on, signin_only, confirm_again, done,
unavailable), `settings.passkeys.added_no_keys`,
`settings.passkeys.stop_restoring.*`.

Every string in the passkey choice screen, the three-word "signs you in"
lines and the start-over warning is security-critical in the ADR-0061 sense
(shown translated only when reviewed).

`docs/ux/client-redesign.md` section 5.6 still lists "Email or phone" and the
old order; it needs the same edit (not made here).

## 12. Data the UI needs

From the server (all in ADR-0064):

- Three words: `positions[3]` (each 1..=12), `salt` and the stretch cost for a
  name; on a correct answer the same sign-in result the authenticator route
  returns (account id, session, expiry). Never a remaining-tries count, never
  whether the shortcut exists.
- `429` with a retry time in seconds on every pre-session route; `503` when a
  method is switched off server-wide.
- Signed-in only: whether the three-word shortcut is on, its positions, and
  whether it was switched off by wrong tries (so 10.3 can say so).
- Per passkey: whether keys are stored for it (for the two labels), a way to
  store them, fetch them after a passkey sign-in, and **remove** them (there
  is no remove route today).
- A way to replace the password from a device that holds the keys, without
  typing the phrase (7.1).

From the client core:

- A 12-word phrase type: generate, parse, words, the built-in check.
- The three-word answer as a sign-in proof only (no "open" step).
- Sealing and opening the phrase under a passkey's secret, on iOS and Android
  as well as web.
- From each platform's passkey API: the secret that protects the keys, when
  the platform gives one, at creation or at use; and a clear "none" otherwise.
- A passkey sign-in on a device with no cached account, on iOS and Android
  (web has it).

## 13. Cross-platform shape

One model, the same rows in the same order, the same copy.

- **Web:** the signed-in-device prompt of section 3 is a modal in an open
  tab. The passkey sheet is the browser's. "Show phrase" asks for the account
  password.
- **iOS and Android:** the prompt arrives as a notification that opens the
  modal. The passkey sheet is the system's. "Show phrase" asks for the device
  unlock.
- **Android has no three-word flow today and iOS has none either;** this spec
  requires sections 6, 9 (the highlight) and 10.3 on both.
- **iOS and Android refuse a passkey sign-in on a device with no cached
  account today;** this spec requires section 4 on both.
- Where a platform or passkey provider cannot give the secret that protects
  keys, the passkey is **Signs in only** there, the choice screen is not
  shown, and the copy of 10.2 says so. A passkey that restores keys on one
  client may be sign-in only on another; the label always describes what was
  stored for the passkey, and section 4 reports what actually happened.
- Accessibility: every row is a button with its title and subtitle as one
  label; the highlight in section 9 has a non-colour mark; the countdown in
  the `429` state is announced once, not every second; word slots announce
  their number.
