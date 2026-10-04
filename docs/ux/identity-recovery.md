<!-- SPDX-License-Identifier: AGPL-3.0-or-later -->
<!-- SPDX-FileCopyrightText: 2026 Lakko Contributors -->

# Identity recovery

UX spec for getting an account back: on a device that does not hold its keys
(a new browser, a new phone, a reinstalled app), or on a device that still
holds them when the password is forgotten. Architecture:
[ADR-0064](../adr/0064-recovery-model-simplification.md), which amends
[ADR-0049](../adr/0049-identity-recovery-word-vault.md),
[ADR-0018](../adr/0018-recovery-factor-cascade.md) and
[ADR-0017](../adr/0017-phrase-seeded-identity.md); and
[ADR-0065](../adr/0065-account-key-reset.md) for what resetting the account
keys does to the other devices (section 15, added 2026-10-03) and what a
contact of the person sees and does afterwards (section 16, added 2026-10-03,
designed and unbuilt).

**Rewritten 2026-10-02**, then brought in line the same day with two later
decisions and with what was built on web. The model:

- The recovery phrase is **12 words**.
- **Three words are checked by the device itself**, on a device that already
  holds the account's keys and phrase. They sign the person in there and
  nothing more. There is nothing to set up and nothing about them on a
  server.
- **A passkey can restore keys** when the person turned that on for the
  passkey and the platform supports it.
- **Another device is the first route** everywhere.
- **Every device that has the keys also has the phrase**, so it can show it,
  check three words and hand the keys to a further device.
- Guardian shares and the email / phone code are gone.

Build status is recorded in ADR-0064 and `docs/PRODUCT-READINESS.md`. In
short: the web client is built to this spec and was not walked in a browser
against a server; iOS and Android are being brought to it on the same day.
Where this spec follows the web build rather than the earlier text, or keeps
something the build does not have yet, section 14 lists it.

## 1. Two situations, one vocabulary

Everything below uses two words the UI also uses:

- **Signed in** means the server accepts this device for the account. The
  device can then reach the account's settings and its other devices.
- **Keys** means what makes the person the same person to their contacts and
  lets the device read and send messages. Keys never come from the server
  alone.

| Situation | What is missing | Routes, in display order |
|---|---|---|
| New device (or reinstalled app, cleared browser) | Signed in and keys | Another device, passkey, 12-word phrase; last resort: **Reset account keys** (the code calls it `mintFreshIdentity`, ADR-0017) |
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
  [lines]    Three words from your phrase   >
             Type three words of your recovery phrase. This
             device checks them.
  [shield]   Authenticator code             >
             A code from your authenticator app.

Three words or an authenticator code sign you in. On a device
that has never held this account you need another device, a
passkey or your recovery phrase.

Remembered your password? Log in
```

Rules:

- **Another device is first** and carries "if you have one".
- **No row depends on server state.** Before a session the server must not
  say whether an account has other devices, a passkey or an authenticator,
  so those rows are never hidden, greyed or badged.
- **The three-word row depends on a local fact** and is the one row that can
  be absent: it is shown only when this device holds the phrase and the keys
  for its saved account and that account has a username. On every other
  device (a new phone, a cleared browser, a guest account) the row is not in
  the list and the note under it reads "An authenticator code signs you in.
  On a device that has never held this account you then still need another
  device, a passkey or your recovery phrase."
- The two group labels are plain text headings, not collapsible.
- The note under the list is always visible; it is the one place the screen
  explains the difference.
- Picking a row replaces the list with that method's form under a heading
  that leads back to the list.
- The username typed in one form is kept when the person goes back and picks
  another. It is pre-filled from the device's saved account when there is
  one. Passkey and three words do not ask for it.

### States every form shares

| State | What the person sees |
|---|---|
| Idle | The form; the primary button is disabled until the input is complete and well-formed, so a typo never spends an attempt. |
| Working | The primary button shows one word ("Checking..."), inputs are disabled, the back heading is disabled. |
| Wrong input | One sentence under the inputs, the inputs keep focus. The sentence never says which part was wrong (name or secret). |
| Rate limited (`429`) | "Too many attempts. Try again in 14:59." with a live countdown from the server's retry time; the primary button stays disabled until it reaches zero; the other methods stay usable through the back heading. |
| Unavailable (`503`) | "This method is not available right now. Choose another one." with a button back to the list. The row stays in the list. |
| Offline / server unreachable | "Could not reach Lakko. Check your connection and try again." Inputs keep their contents. |
| Success | Section 7 decides what comes next. |

## 3. Another device

The code is shown on the signed-in device and typed on the new one, consent
first, one try per request. It brings both sign-in and keys, and the new
device also receives the recovery phrase (section 12).

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

**The signed-in device does not have the phrase.** It can still add the new
device but cannot hand over the keys. It says so: "This device could not
hand over your keys to the new device. On the new device, use a passkey or
your recovery phrase instead." The new device then lands in section 8.

## 4. Passkey

One row, two outcomes, and the form says which one happened.

```
Passkey

Use the passkey you saved for Lakko. Your device will ask for
your face, fingerprint or screen lock.

[ Use a passkey ]
```

- No username field: the system sheet lists the passkeys available for
  Lakko. This works on a device that has never held the account.
- The system sheet is the platform's own (browser dialog, iOS sheet, Android
  bottom sheet). Lakko shows nothing over it.

| State | What the person sees |
|---|---|
| Working | "Waiting for your passkey..." while the system sheet is up, then "Checking...". |
| Cancelled in the system sheet | Back to idle. |
| Signed in, this device already held the keys | Section 7. |
| Signed in, keys restored from the passkey | Section 7. |
| Signed in, this passkey does not carry keys | Section 8 opens with: "Your passkey signed you in. It does not carry your keys." |
| Signed in, the stored keys did not open or did not match | Section 8 opens with: "Your passkey signed you in, but the keys saved with it could not be used. Nothing was changed." This is the fail-closed check against the account's published identity; it is never retried silently. |
| Passkeys not supported here | The row stays; the form reads "This device or browser does not support passkeys. Choose another way." |

## 5. Recovery phrase (12 words)

Twelve numbered slots: autocomplete from the word list, an outline for a
word not in the list, paste of a whole phrase into any slot fills all twelve.

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
  rules.
- Wrong phrase for this name (server refusal): "That did not match this
  account. Check the words and the username, then try again."
- Right phrase, but the keys it gives are not the ones published for the
  account: nothing is changed and the form says so.
- Success: signed in, keys restored, password replaced, the phrase saved on
  this device; section 7.

### The grid

**Three columns by four rows at phone width, four columns by three rows on
wide screens.** Numbered **row by row** (1 2 3 / 4 5 6 / ...), never column
by column. The same grid is used wherever the phrase appears: shown at
signup, shown after resetting the account keys, shown in Settings, and typed in. The slot
numbers are Latin digits in every language. The drawing above is the wide
layout.

## 6. Three words and the authenticator code (sign-in only)

### Three words from your phrase

Offered only on a device that holds the phrase (section 2). The device
checks the words itself, the way a wallet app does; nothing is sent anywhere
until they match.

```
Three words from your phrase

@name on this device

Type words 7, 3 and 12 of your recovery phrase.

  7 [______]    3 [______]    12 [______]

This device checks the words itself and signs you in. You then
choose a new password.

[ Sign in ]
```

- **No username field.** The account is the one saved on this device, shown
  as "@name on this device".
- **The positions are chosen at random for every prompt** and shown in the
  order they were drawn, not sorted. The inputs are in the same order as the
  sentence.
- **Sign in** is enabled only when all three fields hold words of the word
  list, so a typo never counts as a try.
- **Wrong words:** "Those words did not match." and "4 tries left before a
  15-minute pause." **New positions are drawn** and the fields are cleared.
- **Paused:** after the fifth wrong try, "Too many wrong tries. Try again in
  14:59." with a live countdown; the fields and the button are disabled. The
  count starts again from zero after the pause. The pause survives closing
  and reopening the app and cannot be shortened by changing the device
  clock. The other methods stay usable through the back heading.
- **Words matched, signing in failed.** The words are right; the tries are
  not charged.

  | Cause | Copy |
  |---|---|
  | The server limits sign-in attempts (`429`) | The shared rate-limited line with its countdown. |
  | The phrase saved on this device is no longer the account's (the account keys were reset on another device) | "The recovery phrase saved on this device no longer signs in to this account. Choose another way." |
  | Server-side unavailable (`503`) | The shared unavailable line and the button back to the list. |
  | Offline | The shared offline line. |
  | Anything else | "The words matched, but signing in failed. Try again." |

- **The phrase is no longer on this device** (it was there when the list was
  drawn): "Three words are not available on this device. Choose another
  way."
- **Success:** section 7.1, "Choose a new password".

### Authenticator code

Username, a six-digit code, **Sign in**; wrong code, `429` and `503` as in
the shared table. Its intro says what it does not do: "It signs you in; on a
device that has never held this account you then still need another device,
a passkey or your recovery phrase."

## 7. After a method succeeds

| The device now has | Came from | Next screen |
|---|---|---|
| Sign-in and keys, password already replaced | Recovery phrase | The app. |
| Sign-in and keys | Another device; a passkey that carries keys | 7.1 if the person came through "Forgot password?", else the app. |
| Sign-in, and the device already held the keys | Three words, authenticator code, passkey | 7.1. |
| Sign-in only, no keys on this device | Authenticator code, a passkey without keys | Section 8. |

Three words cannot end in the last row: they exist only where the keys are.

### 7.1 Choose a new password

Shown once, right after a successful forgot-password route on a device that
holds the keys.

```
Choose a new password

You are signed in. Set a new password so you can log in next time.

New password           [ ............ ]
Confirm new password   [ ............ ]

[ Save password ]        Not now
```

- The device proves it holds the account's keys to replace the password; the
  person types nothing but the new password.
- **Not now** opens the app; Settings > Security > Password offers the same
  form later ("Choose a new password for logging in. This device already
  holds your account's keys, so you only type the new password."). Until
  then the old (forgotten) password stays valid.
- Not offered to an account without a username.
- States: working, password rules not met (inline, per field), `429`,
  offline, and "Could not save the password. Try again." Success: "Your
  password was changed."

### 7.2 What the chat list shows afterwards

| Route | Chat list |
|---|---|
| Another device | Fills as the other device adds this one to each chat; banner "Catching up with your other device..." until done. |
| Recovery phrase or passkey, another device of the account is still signed in | Fills as that device notices this one and adds it (this can take until that device is next open); banner "Your other devices are adding this one to your chats." |
| Recovery phrase or passkey, no other device left | Empty list with a card: "Your keys are restored. Your chats were on devices you no longer have. Your contacts will see you as the same person and can add you back." with a button to Contacts. Not a loading state; no spinner. |

## 8. Restore your keys (signed in, no keys on this device)

Shown instead of the app whenever the device is signed in but holds no keys:
after a password login on a new device, after an authenticator code on a new
device, after a passkey that did not bring keys, after another device that
could not hand over the keys.

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
- **Three words and the authenticator are never on this screen.** They
  cannot restore keys.
- **Passkey** runs the passkey ceremony again and asks it for the keys. The
  device is signed in here, so it may ask the server: the row is hidden when
  passkeys are not supported on this device, or when none of the account's
  passkeys restores keys.
- **Another device** is the section-3 flow without the username field (the
  account is known). It can also show a QR code.
- **Recovery phrase** is the twelve-slot input without the password fields.
  Mismatch: "That recovery phrase doesn't match this account's identity.
  Check the phrase, or pair an existing device instead." Not published /
  unreachable keep their own copy. Nothing is written before the check
  passes.
- **I have none of these** expands to the **Reset account keys** panel: a
  warning, a typed confirmation, then section 9.1. The warning's text is
  section 15.1 (ADR-0065 widened what a reset does; the earlier text here,
  "This resets your account keys. Your contacts will need to verify you
  again, messages on your lost devices stay unreadable, and you get a new
  12-word recovery phrase to save. Only do this if ...", is replaced by it).
- **Log out** leaves the screen without changing anything.
- States per route: the shared table of section 2.

## 9. The phrase at signup

Up to three steps after the account form, the same on all three clients,
each headed "Step N of M".

**Step 1, save.**

```
Save your recovery phrase                          Step 1 of 3

These 12 words bring your account back on a new device if you
lose your password and your devices. Write them down in order
and keep them somewhere safe.

 1 lamp     2 river    3 cotton   4 ...
 ...                           12 ...

Anyone who has these words can read your messages and act as
you. Do not share them.
Do not keep them as a screenshot or in a plain text file.

[ Copy ]   [ I have written them down ]
```

- The grid of section 5.
- **Copy** copies the phrase; if copying fails: "Could not copy. Write the
  words down instead."
- **No word is highlighted.** There are no fixed three-word positions any
  more, so there is nothing to point at, and pointing at three words would
  teach people to keep only those.

**Step 2, check your notes.** Re-enter three of the words at positions
chosen at random for this screen. A mismatch names the positions that are
wrong ("Word 7 does not match. Check your notes and try again."). **Back to
the words** returns to step 1. An acknowledgement ("I understand that losing
these words means losing access to this account.") gates the step.

**Step 3, offer a passkey** (only where passkeys are supported and the
account has a username): "Add a passkey so you can sign in with your face,
fingerprint or screen lock." with **Add a passkey** and **Not now**. Adding
one continues into the choice of section 10.2. This step never blocks
signup: every outcome, a failure included ("Could not add the passkey. Try
again, or choose Not now."), leaves a way on.

The phrase counts as saved when the last step ends; closing the app in the
middle shows the phrase again from step 1.

### 9.1 The new phrase after resetting the account keys

Resetting the account keys (section 8) gives the account a new phrase. The old phrase
must stop working and the new one must start working before the person is
asked to write it down.

| State | Screen |
|---|---|
| Finishing | "Save your new recovery phrase" and "Finishing...". The words are not shown. |
| Could not finish | "Your account keys were reset, but Lakko could not finish setting up your new recovery phrase. Until that is done the new words would not sign you in, so they are not shown yet. Try again." with **Try again** and **Log out**. Try again never resets the keys a second time; it only repeats the last step. |
| Rate limited | The shared `429` line with its countdown; Try again is disabled until it reaches zero. |
| Offline | "Could not finish. Check your connection and try again." |
| Ready | The new phrase in the grid: "The account keys for @name were reset. These 12 words bring the account back on a new device if you lose access again. Write them down in order and keep them somewhere safe.", "Your contacts will see that your safety number changed and will need to verify you again before messaging you securely.", and the acknowledge gate "I have written down these 12 words and understand that losing them means losing access to this account." |

If the app is closed in the "could not finish" state, the next time it opens
signed in it retries in the background, and the screen returns until the
step is done.

## 10. Settings > Security

### 10.1 Recovery

```
Recovery

Brings back your chats on a new device
  Recovery phrase     The 12 words you wrote down at signup
                      restore your keys and contacts on a new
                      device. Saved on this device.
                                         [ Show recovery phrase ]
  Passkeys            2 passkeys. 1 restores keys.     [ Manage ]
  Other devices                                        [ Manage ]

Signs you in when you forget your password
  Three words from    If you forget your password on this device,
  your phrase         type three words of your recovery phrase to
                      sign in. There is nothing to set up.

Authenticator app
  ...
```

- **Recovery phrase** states whether this device holds the phrase ("Saved on
  this device." / "Not saved on this device. Keep your written copy safe.")
  and, when it does, **Show recovery phrase** behind re-authentication: the
  device's own unlock (biometric or screen lock) on iOS and Android; the
  account password on web ("Type your password to show your recovery
  phrase.", "That password is not right."). Shown with "Make sure nobody
  else can see your screen.", the grid of section 5, "Anyone who has these
  words can read your messages and act as you. Do not share them." and
  **Hide**.
- **Passkeys** summarises ("None added." / "2 passkeys. 1 restores keys.")
  and opens 10.2. **Other devices** opens Devices.
- **Three words from your phrase** is one informational row with no control:
  the line above, or "Not available on this device: your recovery phrase is
  not saved here."
- **Authenticator app** is the existing setup, unchanged.
- There is no email, phone or guardian row, and no three-word switch.

### 10.2 Passkeys, and letting a passkey restore keys

The list shows each passkey with its name, when it was added and last used,
and one of two labels: **Signs in and restores keys** or **Signs in only**.

Adding a passkey:

1. **Add passkey** > name it > the system sheet creates it. **Nothing about
   the keys is stored by adding a passkey.**
2. If the platform returned what is needed to protect keys with this passkey,
   the choice screen follows. Otherwise the result reads: "Passkey added. It
   signs you in. On this device and passkey provider it cannot restore your
   keys, so keep your recovery phrase." and the row is **Signs in only**.

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
  turn this on."); the typed phrase must be this account's ("That recovery
  phrase does not belong to this account. Nothing was changed.").
- Some platforms can only produce what is needed when the passkey is used,
  not when it is created. Then **Turn on** triggers one more system sheet
  for that passkey. If a different passkey answers: "A different passkey
  answered. Try again and choose the passkey you are turning this on for."
- States: working; cancelled ("Cancelled. This passkey still only signs you
  in."); the passkey cannot carry keys here ("This passkey can sign you in,
  but on this device and passkey provider it cannot carry your keys. Keep
  your recovery phrase."); `503` ("Restoring keys with a passkey is not
  available on this server."); offline; success ("This passkey now restores
  your keys.").

Per passkey, later: **Turn on restoring keys** (the same choice screen) or
**Stop restoring keys** (confirmation "Stop restoring keys with this
passkey?" / "This passkey will only sign you in. Keys already restored on
other devices stay there."; then "This passkey now only signs you in.").
Removing a passkey also removes what it stored for restoring keys; the
existing passkey-confirmation step for removal is unchanged.

A passkey created before this feature, or on another client, shows **Signs in
only** until the person turns it on.

### 10.3 Password

A Password card in Settings > Security opens the form of 7.1.

## 11. Copy

The source of truth is `l10n/catalog/en.json`; this spec quotes it and does
not edit it. The groups that carry this spec: `recovery.*`,
`recovery.words.*`, `recovery.passkey.*`, `recovery.new_password.*`,
`recovery_phrase.*` (including `recovery_phrase.fresh.*`),
`restore_identity.*`, `settings.recovery.*`, `settings.passkeys.*` (including
`keys_choice.*`, `stop_restoring.*`, `label.*`), `settings.password.*`,
`auth.signup.passkey.*`, `device_link.keys_not_sent`.

Every string in the passkey choice screen, the three-word lines, the
start-over warning and the "could not finish" state is security-critical in
the ADR-0061 sense (shown translated only when reviewed).

Copy still to be changed in the catalog (not done by this spec):

- `settings.passkeys.lede` / `lede_device` still read "Your recovery phrase
  always works too." and do not mention that a passkey can restore keys.
- `auth.passkey.error.no_cached` and `auth.passkey.error.not_registered`
  describe the time when a passkey did not work on a new device; whether any
  path still shows them was not checked.
- `restore_identity.lede_no_words`, `settings.recovery.lede`,
  `settings.recovery.section.{access,identity}` look unused by the new
  screens; whether they can be retired was not checked.
- `settings.account.forget_device.body_device` and `.sub` name "your
  recovery phrase or another signed-in device" and leave out a passkey that
  restores keys.

## 12. Data the UI needs

From the server (all in ADR-0064):

- **Nothing for three words.** No positions, no verifier, no setting.
- `429` with a retry time in seconds on every limited route; `503` when a
  method is switched off server-wide.
- A passkey sign-in that needs no saved account.
- Per passkey, when signed in: whether keys are stored for it (the two
  labels, and whether the restore prompt shows the passkey row), a way to
  store them, fetch them, and remove them.
- A sign-in and a password replacement from the phrase seed the device
  already holds (the existing recovery login and password reset).
- A way to make a new phrase the account's recovery phrase after starting
  over, limited per account, answered with a retry time when limited.

From the client core:

- A 12-word phrase type: generate, parse, words, the built-in check, to and
  from its 16 bytes.
- Three random positions, and "do these three words match the phrase at
  these positions".
- Sealing and opening the phrase under a passkey's secret, on every client.
- From each platform's passkey API: the secret that protects the keys, when
  the platform gives one, at creation or at use; and a clear "none"
  otherwise. It never leaves the device.

Kept by the client:

- **The phrase, on every device that has the keys**, protected like the
  keys. Every way a device gets keys also stores the phrase: signup, guest
  signup, the phrase typed anywhere, a passkey restore, another device, and
  resetting the account keys.
- The three-word try count and pause, per account, across restarts; cleared
  on success, on forgetting the device and on account deletion.
- "A new phrase is waiting to become the recovery phrase" (9.1), across
  restarts.
- What another device hands over: the phrase itself, so the new device ends
  up like any other device with keys.

## 13. Cross-platform shape

One model, the same rows in the same order, the same copy.

- **Web:** the signed-in-device prompt of section 3 is a modal in an open
  tab. The passkey sheet is the browser's. "Show recovery phrase" asks for
  the account password.
- **iOS and Android:** the prompt arrives as a notification that opens the
  modal. The passkey sheet is the system's. "Show recovery phrase" asks for
  the device unlock.
- Where a platform or passkey provider cannot give the secret that protects
  keys, the passkey is **Signs in only** there, the choice screen is not
  shown, and the copy of 10.2 says so. A passkey that restores keys on one
  client may be sign-in only on another; the label always describes what was
  stored for the passkey, and section 4 reports what actually happened.
- Accessibility: every row is a button with its title and subtitle as one
  label; the countdowns (the `429` state and the three-word pause) are
  announced once, not every second; word slots announce their number ("Word
  7"); the three-word inputs are read in the order the sentence names them.

## 14. Where this spec follows the build, and what is still open

Follows the web build (the earlier text of this spec said otherwise):

- Three words are checked on the device; random positions per prompt in
  drawn order; no username field; the row is absent where the device has no
  phrase; two versions of the note under the list. (Earlier: server-held
  positions, fixed and ascending, with a username step.)
- No highlight and no three-word line on the signup phrase screen.
- No three-word switch in Settings (earlier section 10.3); one informational
  row instead. Section 10.3 is now Password.
- The grid is 3 x 4 at phone width and 4 x 3 on wide screens, row by row.
  (Earlier: four, three or two columns.)
- The restore prompt hides the passkey row from what the server says about
  the account's passkeys, since the device is signed in there. (Earlier:
  from what the device had just tried.)
- "Show recovery phrase" on web checks the account password with the server.
  (Earlier: "no server call".)
- The row title is "Three words from your phrase".
- The 9.1 pending state, the 7.1 form and the Password card are new.

Open, not built on web:

- **7.2**: the two banners and the empty-list card have no strings in the
  catalog. Kept in the spec as required.
- **"No passkey for Lakko was found on this device."** (section 4 in the
  earlier text): there is no such string; what the form shows when the
  system sheet has no passkey to offer was not checked. Removed from the
  state table; open.
- **Pasting a whole phrase into a three-word field** to fill the three
  positions: not specified any more; not built.
- **A device whose saved phrase is refused at log in** (for example the
  account keys were reset on another device): the three-word route is paused
  on that device (row absent, Settings shows
  `settings.recovery.words.suspended`) until the account is restored there
  again; the phrase itself is kept. The device is not told that its identity
  is stale; open.
- **"Show recovery phrase"** is withheld while 9.1's "could not finish"
  state is pending: the row shows the pending explanation with Try again.
- The catalog items of section 11.
- Nothing in this spec was walked in a browser or on a device against a
  server.

## 15. Resetting the account keys: the other devices, the passkeys, the signed-out device

Added 2026-10-03 with [ADR-0065](../adr/0065-account-key-reset.md).
**Status (2026-10-03): built on web, iOS and Android the same day, unit-tested
(web vitest, iOS `LakkoTests` on a simulator, Android host unit tests), NOT
walked on any device or in a browser against a server; nobody has seen any
of these screens rendered.** Per-subsection status lines below. The ADR's
"What the build changed / found" section lists where the build differs from
the ADR; the strings of 15.7 now exist in the catalog. It extends section 8's
"I have none of these" and section 9.1 and adds two screens. The model the
maintainer decided on 2026-10-03: resetting the account keys **signs out
every other device of the account and removes it from the account, deletes
the account's passkeys, and the signed-out device wipes everything it holds
for the account**. The authenticator app and the password keep working.

### 15.1 The warning before a reset

Status: built on web / iOS / Android, not walked (catalog group
`restore_identity.reset.*`).

Replaces the warning text of section 8. Shown expanded under "I have none
of these", above the typed confirmation:

```
Reset account keys

This gives the account new keys and a new 12-word recovery
phrase. Only do this if you have lost your recovery phrase, every
other device and every passkey that restores keys.

What it does:
  - Every other device of this account is signed out and
    removed. They lose their chats and contacts for this account.
  - Every passkey of this account is deleted. You can add
    passkeys again afterwards.
  - Your contacts will see that your safety number changed and
    must verify you again before messaging you securely.
  - Messages sent before today cannot be read with the new keys.
  - Your password and your authenticator app keep working.

Type RESET to confirm
[            ]

[Reset account keys]      Cancel
```

- The button is disabled until the typed word matches; the word is the same
  Latin letters in every language (like the slot numbers of the grid), and
  the line names it: "Type RESET to confirm".
- Nothing is sent until the button is tapped. The device does not know how
  many other devices or passkeys the account has here (asking would be a
  lookup the server answers before a session on some paths, and the number
  changes nothing about the decision), so the bullets are unconditional.
- After the tap: a full-screen "Resetting..." with no cancel (the server
  step is one operation and cannot be half done), then section 9.1 in its
  new form (15.4).
- Failures before anything changed, shown inline under the button and
  leaving the panel open: the shared `429` line with its countdown; "Could
  not reset. Check your connection and try again." (offline); "Could not
  reset. Try again later." (any other server refusal). A failure **after**
  the server step is 15.4, never this panel.

### 15.2 The signed-out screen (every other device)

Status: built on web / iOS / Android (the `RemovedDeviceScreen` state reused
with a second variant; the wipe runs by itself; catalog group
`devices.signed_out.*`), not walked: no two-device run has shown it.

Each other device learns of the reset the next time it talks to the server
(within a few minutes while it is open; at launch otherwise). From then on
it must not pretend to be the account's device. It shows one full-screen
state instead of the app, the same shape as "This device was removed"
(`second-device.md` 3.1), with its own copy:

```
Your account keys were reset

The keys of @name were reset on another device. This device was
signed out and is being removed from the account.

Everything this account kept on this device is being deleted:
its chats, contacts, keys and recovery phrase. Your other
devices are not affected.

[progress]  Removing...
```

then, once the deletion is done:

```
Your account keys were reset

The keys of @name were reset on another device. This device was
signed out and removed from the account.

To use this account here again, sign in and get the keys from
the device that reset them.

[Sign in]
```

- **No dismiss, no retry, no chat list behind it**, at launch too. The
  deletion starts by itself; it is the same deletion as "Forget this
  device" (`settings.account.forget_device`), and it sends nothing to the
  server first. The one thing it does over the network is release this
  device's push registration, so the device stops being woken.
- **Sign in** goes to the anonymous sign-in screen. Signing in lands the
  device as a device without keys (section 8), where "Another device" is
  the route back.
- The same screen, with the second line "This device's keys do not match
  the account's published identity." and the **Sign out** button of 3.1
  instead of an automatic deletion, is shown when a device finds at sign-in
  that the account publishes other keys of the **same** generation (an
  anomaly, not a known reset; ADR-0065 4.2 rule 4). It is not expected to
  be seen; it exists so the device never publishes its keys over the
  account's.
- Accessibility: the title and the body are one announcement; the progress
  state is announced once ("Removing"), not on every tick; the single button
  is the only focusable element.

What the signed-out device never does: upload its identity, upload its
device key, send to any chat, show a banner that can be dismissed. The
dismissible "Your published identity was changed on another device" banner
of the earlier build is gone; a changed identity is either this screen or
nothing.

### 15.3 Settings > Devices after a reset

Status: built on web / iOS / Android (`settings.devices.reset_note`,
`settings.passkeys.reset_note`), not walked.

On the device that reset the keys, the Devices screen (`second-device.md`
section 3) lists this device alone, with one dismissible note at the top
for the rest of that session:

```
Your account keys were reset on this device today. The other
devices were signed out and removed. Add them again from
Settings > Devices > Add a device.
```

The other devices do not appear with a "removed" state: they are not
devices of the account any more and the account holds no list of them.
Settings > Security > Passkeys shows its empty state ("No passkeys yet")
with the same note's second sentence adapted: "Your passkeys were deleted
when the account keys were reset. Add a passkey to sign in with Face ID,
Touch ID or your fingerprint again."

### 15.4 Section 9.1, amended: "publish owed" instead of "recovery credential owed"

Status: built on web / iOS / Android (the owed record with the attestation and
per-step marks; `recovery_phrase.fresh.publish_owed`,
`recovery_phrase.fresh.sign_in_to_finish`,
`recovery_phrase.fresh.devices_signed_out`), not walked.

The server step of a reset is one operation, so the new phrase always
signs the person in once that step succeeded. What can still be owed is
telling the directory and the account's device list about the new keys.
The table of 9.1 keeps its rows; the "could not finish" copy becomes:

> Your account keys were reset, but Lakko could not finish publishing
> them. Until that is done, your contacts and your other devices cannot
> find the new keys, so the new words are not shown yet. Try again.

**Try again** repeats only the publishing step and never resets the keys a
second time. If the app is closed in this state, the next time it opens
signed in it retries in the background and the screen returns until the
step is done. If the reset is older than 30 days when the retry runs, the
screen asks for a sign-in first ("Sign in again to finish publishing your
new keys."); signing in changes nothing else.

The Ready state's second sentence is unchanged ("Your contacts will see
that your safety number changed ..."); a third is added: "Your other
devices were signed out. Add them again from Settings > Devices."

### 15.5 Data the UI needs

From the server (all in ADR-0065):

- A reset operation that in one step gives the account new keys, replaces
  the recovery phrase, deletes the passkeys, signs out the other devices,
  and returns the session this device continues with. `429` with a retry
  time; `503` when switched off.
- A way for a device to learn, on any server call, that it was signed out
  by a reset (one code, `key-generation-superseded`), as opposed to a
  session that merely expired (sign in again, nothing deleted).
- A way to tell the directory and the device list about the new keys that
  can be retried after a crash, and that refuses to publish the old keys
  back.
- From the directory: which generation of keys the account publishes, so a
  device can tell its own keys are stale before it uploads anything.

Kept by the client:

- "The new keys are not published yet" (15.4), across restarts, with what
  is needed to retry.
- This device's key generation, so a `401` can be told from a reset
  without a server round trip when the directory answers.
- The one-session Devices note of 15.3.

### 15.6 Cross-platform shape

- The warning, the signed-out screen and the Devices note are the same
  copy and the same order on web, iOS and Android.
- **Web:** the signed-out screen replaces the app in every open tab of the
  account; a tab that was in the background shows it when it next
  refreshes its session. **iOS and Android:** shown at launch when the
  reset happened while the app was closed; the push registration is
  released before the deletion so no further notification arrives for the
  account.
- The typed confirmation uses the platform's plain text field with
  autocorrect and capitalisation suggestions off.

### 15.7 Open in this section

- Whether the password and the authenticator should also stop working
  after a reset (ADR-0065 open question 1). The copy above says they keep
  working; it changes if the maintainer decides otherwise.
- What a contact sees when the person who reset the keys writes to them
  from the new keys, and the eviction of the old devices from shared
  groups: ADR-0065 phase 2, section 16 of this spec.
- Strings: all of the groups proposed here are in `l10n/catalog/en.json`
  since 2026-10-03: `restore_identity.reset.*` (the warning, the
  confirmation word, the failure lines), `devices.signed_out.*` (`title`,
  `body_removing`, `body_removed`, `body_mismatch`, `sign_in`, plus
  `removing` and `this_account`), `settings.devices.reset_note`,
  `settings.passkeys.reset_note`, `recovery_phrase.fresh.publish_owed`,
  `recovery_phrase.fresh.sign_in_to_finish`,
  `recovery_phrase.fresh.devices_signed_out`. All of them are
  security-critical in the ADR-0061 sense. Checked 2026-10-03: 18 of the
  23 keys carry the `security_critical` flag; the five that do not are
  progress and failure lines (`devices.signed_out.removing`,
  `devices.signed_out.this_account`, `restore_identity.reset.resetting`,
  `restore_identity.reset.error.generic`, `restore_identity.reset.error.offline`),
  a choice the builders made and this spec did not ask for; left as built.

## 16. A contact reset their account keys: what you see and do

Added 2026-10-03 for [ADR-0065](../adr/0065-account-key-reset.md) phase 2.
**Status: built 2026-10-04 on web, iOS and Android (unit-tested; the
deviations per client are in the ADR's "Phase 2 as built"); nothing here
has been rendered on a screen or walked on two devices.** The 46 keys of
16.7 exist in the catalog with machine translations.
Section 15 is the person who resets; this section is everyone else: a contact
of theirs, a member of a group they are in, a subscriber of a channel they
own. The ADR's phase-2 text (its "Phase 2" section) is the architecture; this
section is what it looks like.

**Scope decision (maintainer, 2026-10-03):** phase 2 is built **without** the
ADR's proposed owner-slot exception in `lakko-mls`. A group the old identity
**owned** is handled only by **Recreate group** (16.5): a new group, a new
journal, the members re-invited by an admin, the old group closed. The
eviction of 16.3 is **not attempted** in such a group, and nothing in this
section pretends it was. Everything that follows rides existing paths: the
fail-closed identity check ([ADR-0016](../adr/0016-identity-supersession-transport.md)),
the KT log's reset marker ([ADR-0019](../adr/0019-client-authorised-log-leaf.md)
/ [ADR-0036](../adr/0036-auth-attested-genesis.md)), the proposal path
([ADR-0045](../adr/0045-remote-commit-epoch-fork.md),
[`proposal-based-membership.md`](proposal-based-membership.md)), the ended-chat
shapes of that spec's section 6.2, and the contact screen's safety number
([`conversation-info.md`](conversation-info.md) 2.1, `ContactDetailView.tsx`).

Vocabulary the UI uses, so the copy below reads as one thing: a person has
**account keys**; the thing two people compare is the **safety number**; a
contact whose keys were reset and whose new number has not been compared is
**not verified**. The screen never says identity, pin, leaf, key generation
or log.

### 16.1 Detection, and the flagged contact

**Where the client notices.** Two places, both already in the code:

1. Any identity fetch for the contact's account (starting a chat, adding them
   to a group, sending a contact request, the group-call roster resolution)
   runs the fail-closed check. Today a changed key with no proof is an
   **error** at the action (`contacts.identity_changed.*`). After this
   section, when the KT log confirms the new key as a reset (`is_reset`), it
   is a **state** of the contact instead, and the action's error line points
   at that state (16.1 "What is blocked"). A changed key the log does **not**
   confirm as a reset, or cannot confirm, keeps today's error copy and sets
   no state: that is a possible compromise, not a reset, and nothing in this
   section applies to it.
2. The scheduled device-list check every member already runs for every
   account in every chat (ADR-0050's re-admission arm, every ten minutes per
   chat) now also fetches and checks the account's published identity, so a
   plain member who never starts anything with the contact still learns of
   the reset within that interval while a shared chat is open. (The arm's
   add branch is suppressed for a flagged contact; see 16.8.)

The client records, on the contact, `keyState = reset-unverified`, keeps the
old pinned key as `previousKey`, and keeps the new published key as
`pendingKey` (16.8). It does not re-pin. A reset noticed while the app is
closed is noticed at the next open; there is no notification for it, because
there is nothing the person must do now, only before they next message the
contact.

**The contacts list.** The row keeps its position and its name; it gains a
shield-with-exclamation glyph in the trailing column (the slot the muted
glyph uses; both may show) and a muted secondary line:

- `Account keys reset. Verify the new safety number.`

The list is not re-sorted and no section is added: a reset is a fact about
one contact, not a category. Search still finds them.

**The contact screen.** The identifying header is unchanged (name,
`@username`). A **banner** (the component of the design system: sunken
surface, icon, one sentence, one action) sits directly under the header,
above the action row:

```
[shield!]  Anna reset their account keys. Verify the new safety
           number before messaging them again.          [Verify]
```

`Verify` scrolls to and opens the safety-number section (16.2). The banner
has no dismiss: it is the state, and it goes when the state does.

| Control on the contact screen | Normal | `reset-unverified` |
|---|---|---|
| Message | opens or creates the chat | opens the **closed** old chat when one exists (16.4); otherwise disabled with the helper line `Verify the new safety number first.` |
| Call, Video | enabled when a chat exists | disabled, same helper line (the call keys would come from a chat that is closed) |
| Verify safety number | the section of today | the section of 16.2 |
| Rename | enabled | enabled (it is your name for them) |
| Remove contact | enabled | enabled; removing clears the state with the contact |

**The chat header of a shared group.** Nothing. A group's header names the
group; a contact's state is not the group's state. The members panel carries
it (16.3).

**Message bubbles from the new keys in a shared group.** The new identity is
in a shared group only when some admin added them there (16.3 says when that
is allowed). A member who has not verified them sees each of their messages
with the sender's name and, under the name, once per sender group (the
five-minute grouping of `client-redesign.md` 5.2), a muted warning line with
the shield glyph:

- `Not verified since Anna reset their account keys.`

The bubble renders the message; the line is a warning, not a redaction. The
sender name is the contact's name, resolved through the account id: the new
key is not in any profile store yet, and "Member" with a warning under it
would name nobody. Tapping the warning line opens the contact screen. Their
messages in a group never trigger the warning for a member who has verified
them, and never for a member who was never their contact (there is no old
number to have compared; for them the person is a new member like any
other).

**What is blocked until verification**, and what the person sees when they
try:

| Attempt | What happens |
|---|---|
| Start a new one-to-one with them (contact screen, the create menu, `@name` lookup) | refused before anything is sent: `Verify Anna's new safety number first.` with `Open contact` |
| Add them to a group you own or administer (member picker, `@name` in the add field) | the picker row is shown, disabled, with the secondary line `Account keys reset. Verify the new safety number first.`; the add field answers the same sentence |
| Approve their join request (a `/g/` link) in a group you administer | `Approve` disabled with that sentence; `Reject` stays live; the request stays pending |
| Include them in a recreated group (16.5) | not included; the sheet says so |
| Send in the old one-to-one | the chat is closed (16.4) |

Nothing is blocked for anyone else: the groups keep working, the other
members keep messaging, and the contact's **own** actions are not the
observer's to block.

### 16.2 Re-verification

The safety-number section of the contact screen is the one verification
surface (`client-redesign.md` 5.3), and it stays the one. In the
`reset-unverified` state it reads:

```
[shield]  Verify the new safety number
          Anna's account keys were reset, so the safety number
          you may have compared before no longer applies.
          Compare this new one with Anna in person or on a call.
                                                        [Show]

  12345 67890 12345 67890 12345 67890
  12345 67890 12345 67890 12345 67890

  [ ] I compared the new safety number with Anna and it matched.

  [I verified the new safety number]
```

| State | What the person sees |
|---|---|
| Collapsed | The row with the reset explanation and `Show`. |
| Open, computing | `Loading...` in place of the digits; the checkbox and the button are not shown. |
| Open, number shown | The **new** number (computed over `pendingKey`, never `previousKey`), the checkbox unticked, the button disabled. |
| Could not compute | `Could not compute the safety number. Try again.` (`contacts.safety.error`, existing); no checkbox, no button. |
| Checkbox ticked | The button enables. |
| Confirming | The button shows `Verifying...`; the checkbox is disabled. |
| Confirmed | The section returns to its normal shape with the same number; the banner, the list line and every warning of 16.1 are gone; a one-line toast `Anna is verified again.` on the natives, an inline line in the section on web. |
| Could not confirm (store write failed) | `Could not save. Try again.` under the button; nothing else changes. |

**What confirming does.** It re-pins the account id to `pendingKey` with the
monotonic marker (ADR-0016's pin store), sets `keyState = normal`, forgets
`previousKey` for the purpose of the UI (16.8 says what is kept for the
eviction bookkeeping), and from then on the new keys are an ordinary contact:
Message creates a new chat as a first chat, the picker rows enable, a pending
join request's `Approve` enables, the group warning lines disappear on this
device. It changes nothing on any other device of the account (each device
pins for itself) and tells the contact nothing.

**The old number is never shown beside the new one.** A side-by-side invites
comparing the two, which proves nothing; the only comparison that means
anything is with the person, out of band.

**Confirming without comparing.** The checkbox is the deliberate step and its
sentence is the warning; there is no second dialog and no blocking. The
sentence under the checkbox, always visible while the section is open, is
`security_critical`:

- `If you confirm without comparing, someone who took over Anna's account
  would look like Anna to you.`

That is the whole of it: one true sentence where the person acts, no
exclamation mark, no modal.

### 16.3 Eviction in a group you own or administer

**What happens, and that it is silent.** When the observer's account is the
owner or an admin of a group the old identity is in, the client proposes,
through the proposal path and without asking, the removal of every member
device that resolves to `previousKey`. It asks for nothing because there is
no decision: the old devices are not the person's devices any more, and an
admin who has to tap "Remove" on each of them in each group is doing by hand
what the client knows to do. Several admins proposing the same removals
compose into one sweep; the committer is whoever the group's rank rule names
(`proposal-based-membership.md` section 8), which may be the observer.

The removal is proposed per group at the moment the reset is noticed, and
again on open for any group where it has not yet applied (the automatic
re-request discipline of `proposal-based-membership.md` 7.2 applies: a
request refused for a full queue is re-sent when the queue drains, a request
that was dropped by a sweep is re-sent). It is **not** proposed in a group
the old identity owned (16.5), in a one-to-one (16.4), or in a group with no
roles at all (such a group has no proposal path; the contact is flagged there
and nothing else happens; recorded in 16.10).

**The members panel while it is pending.** The old-key member keeps its row.
It is one row even if the person had several devices, because the panel
lists people (`second-device.md` section 4). The row carries the shield
glyph and the secondary line of the pending vocabulary, in the reset's own
words:

- for the owner and the admins, who proposed it: `Account keys reset. Being
  removed.`
- for a plain member of the same group, who proposed nothing and may propose
  nothing: `Account keys reset.`

The panel's existing pending notice counts it (`1 membership change is
waiting to apply.` + the liveness line, `chat.members.pending.*`); nothing is
added above it. The row's `Remove` is disabled while the removal is pending,
as for any pending removal (5.3 of that spec). The member's role marker, if
they were an admin, is still drawn: a removal rewrites no role table, and the
panel does not pretend otherwise (16.10 lists the ghost-admin question).

**When no committer is online.** The row keeps `Being removed.` and the
notice keeps its liveness line, for as long as it takes. There is no timeout,
no "could not remove", and above all no "removed" that did not happen: a
device that is still in the group still reads the group, and the UI must not
say otherwise (invariant #4 as the ADR states it for this case). The panel
notice's liveness sentence is already written to be true at ten seconds and
at ten days.

**When the sweep lands.** The row goes; the group's timeline gains one
system row at the point the commit landed, centred and muted like every
system row:

- on a device that knows the reset (the contact is flagged there, or was
  until verified): `Anna reset their account keys. Their old devices were
  removed from this group.`
- on a device that does not (never their contact, or not yet noticed): the
  existing `Members changed` row.

The group's name and header do not change. Removing the old devices does not
add the new ones: that is a separate add, by an admin, after that admin has
verified the person (16.1), and it produces the ordinary added-member row.

**A plain member** sees the flag on the row, the warning on messages from the
new keys if any arrive, the system row when the removal lands, and the
contact screen's banner. They are offered no control, because the core
offers them none (a member may not propose another account's removal), and a
disabled `Remove` with no way to enable it would be a control promising
nothing. The way for a plain member to make it happen is the one they already
have: ask an admin, out of band.

### 16.4 One-to-one chats

A one-to-one cannot lose a person (ADR-0062), so the chat with the old keys
**ends** rather than being repaired. The shape is the ended-chat shape of
`proposal-based-membership.md` 6.2, with its own words:

| Part | What it shows |
|---|---|
| Chat list row | stays, in place; the secondary line `Chat closed. Anna reset their account keys.`; no unread badge, no typing, no live anything; pinned / muted / archived untouched |
| Timeline | the history as received, read-only; a terminal system row where it stops: line 1 `Anna reset their account keys. This chat is closed.`, line 2 `The messages you already received are still here. No new ones will arrive.` (`chat.ended.consequence`, existing) |
| Composer slot | replaced, not disabled: a static bar `You can no longer send messages in this chat.` and, beside it, the button `Start a new chat`; an unsent draft above it as selectable text under the `Unsent draft` label (6.2.5 of that spec) |
| `Start a new chat` | **disabled** with the helper `Verify Anna's new safety number first.` and a link `Open contact` while `reset-unverified`; **enabled** once verified |
| Header | the contact's name, as before; tapping opens the contact screen (16.1) |
| Contact screen reached from here | `Call` and `Video` gone, `Delete this chat` in their place (the membership-ended layout of `conversation-info.md` section 4) |

**Nothing is sent.** The client stops sending in this chat the moment the
state is recorded, before any eviction: a message sent here would reach the
old devices, which is exactly what a reset exists to end. The old journal is
left to the server's retention; the client does not leave the chat (there is
nothing to leave; the record simply stops).

**Starting the new chat.** Once verified, `Start a new chat` creates a chat
with the new keys exactly as a first chat is created (the contact request /
accept flow is not repeated: they are already a contact). The new chat is a
new row in the list, with no history. The closed row stays until the person
deletes it from its contact screen, for the reason 6.2.4 gives: a transcript
is never deleted as a side effect of something someone else did. The two rows
are told apart by the closed row's secondary line; the new one opens from the
contact screen's `Message` from then on.

**Messages that arrive in the closed chat** (an old device that had not yet
learned of the reset, sending into the old journal): dropped unread and never
shown. The chat is closed; rendering a late message under the "closed" row
would reopen it in the person's eyes, and the message is from keys that are
no longer the contact's.

### 16.5 Groups the old identity owned: Recreate group

Two facts the UI has to live with, both from the ADR: the owner's old devices
cannot be removed from such a group (`owner_survives`), and the group can
never get a new owner (an ownership transfer needs the owner's key, which is
gone). Membership changes among the **other** members still work, because
admins still sweep; what does not work is making the group secure again.
Under the scope decision, the client does not try.

**What every member sees in the owned group.** The old-key member row carries
the flag (`Account keys reset.`), and **no** `Being removed.` on any device.
The members panel shows one notice at the top, above the pending notice,
`security_critical`:

- for an admin: `Anna, the owner of this group, reset their account keys.
  Their old devices cannot be removed from this group and the group cannot
  get a new owner. To keep chatting securely, recreate the group.` with the
  button `Recreate group`
- for a plain member: the same two sentences, then `An admin can recreate
  it.` and no button.

The group keeps working otherwise: the composer stays, messages flow. The
notice is the honest statement that the group now permanently contains
devices nobody can remove; it is not a lock.

**Recreate group** (admin only; needs **no** verification of the new
identity, because it re-invites the people who are still here). It opens a
sheet, the new-group flow of `new-group.md` pre-filled:

```
Recreate group

Name                 [ Weekend plans            ]

Members (4)
  [x] Ivan
  [x] Maria
  [x] Olga
  [ ] Anna                        Account keys reset.
                                  Verify the new safety number
                                  to include them.

The old group stays as it is for everyone until you close it.

[Create and close the old group]        Cancel
```

- Pre-filled with every current member except the old owner; each may be
  unticked. The creating admin becomes the new group's owner (the ordinary
  rule of creation).
- **The new identity** is listed, unticked and disabled, with the two-line
  reason, while `reset-unverified` on this device; ticked by default and
  enabled once this device has verified them. The sheet does not offer to
  verify from here; the contact screen does that.
- Other admins of the old group become plain members of the new one; the
  sheet says nothing about roles (the new owner promotes from the members
  panel as usual).
- `Create and close the old group` is one action: it creates the group,
  sends every ticked member their invitation (the ordinary add path), and
  posts a **closed** notice into the old group (16.8: a group-closed
  envelope naming the new group), then opens the new group.
- Two admins may each recreate; the client does not coordinate. The second
  closed notice to land wins the banner's `Open` target (it is the newer
  stamp); both new groups exist. Recorded in 16.10 rather than solved.

| State of the sheet | What the person sees |
|---|---|
| Idle | as above; the button is disabled when no member is ticked |
| Working | `Creating...`; the sheet cannot be dismissed |
| Created, some invitations failed | the sheet stays with `Could not add {names}. Add them from the members panel.` (`chat.members.add.error.some_failed`, existing) and a `Done` button; the group exists and is open behind it |
| Could not create | one line under the button, sheet stays open: the existing new-group failure copy |
| Offline | `Could not reach Lakko. Check your connection and try again.` |

**The old group, closed.** On every member's device that receives the closed
notice (from an admin of the group, resolved at receipt; a notice from a
plain member is dropped), the old group takes the ended-chat shape of 16.4
with its own words:

- Timeline terminal row: line 1 `Ivan closed this group after the owner's
  account keys were reset.`, line 2 `chat.ended.consequence`.
- Composer slot: the static bar `This group was closed.` and the button `Open
  the new group`, shown only once this device has joined the new group;
  until then the bar alone. A device that was not invited to the new group
  (unticked by the admin) sees the bar alone, permanently; the members panel
  still has `Leave group` as its exit.
- Chat list row: secondary line `Group closed`.
- Members panel: the frozen-roster line is **not** used (the roster is still
  live; the group is closed, not ended); the owned-group notice above is
  replaced by `This group was closed. Continue in {new group name}.`;
  `Leave group` stays.

A closed group is still a group the person is in, so an admin who regrets it
can keep using it by leaving it closed and messaging in the new one; there
is no "reopen". The old owner's new identity, once verified and added to the
new group, sees the old group from the outside only (the old devices were
wiped); nothing is shown to them about it.

### 16.6 Channels

A channel's publishing key derives from the owner's account seed, so a reset
orphans every channel the person owned: no device can post to it, rotate it
or close it any more, including the device that reset the keys.

**On the device that reset the keys** (the person's own Channels list, the
only place that knows): each channel it owned is kept in the list with the
secondary line `You can no longer post here. Your account keys were reset.`
and, on its detail, one notice with the same sentence and one control,
`Remove from this device`; the publish composer, the rotation and the
subscriber management are gone. The person may create a new channel from the
create menu as always; the client does not copy the name, the description or
the subscriber list across, because the subscribers of the old channel have
no way to be told. `security_critical`, since it states what the reset did.

**A subscriber** has no signal: the channel's keys are not the keys the
contact check tracks, and the channel server knows nothing of the reset. The
honest state is the one the client already has, a channel that receives no
new posts, with `Stop following` on its detail. No notice is shown, because
none would be true from what the device knows. A subscriber who learns of it
from the owner out of band unsubscribes; the owner's new channel is found or
shared the way any channel is. Recorded in 16.10 as the one place this
section cannot say anything, and why.

### 16.7 Copy

Catalog-key candidates with English text; `[sc]` marks `security_critical`
in the ADR-0061 sense (shown translated only when reviewed). The groups are
new; none of these keys exist in `l10n/catalog/en.json` on 2026-10-03. Existing
keys the section reuses are named inline above (`contacts.safety.error`,
`chat.ended.consequence`, `chat.ended.unsent_draft`, `chat.members.pending.*`,
`chat.members.add.error.some_failed`, `chat.system.members_changed`,
`common.error.offline`).

`contacts.keys_reset.*` (the flagged contact):

| Key | English |
|---|---|
| `contacts.keys_reset.row_sub` [sc] | Account keys reset. Verify the new safety number. |
| `contacts.keys_reset.banner` [sc] | {name} reset their account keys. Verify the new safety number before messaging them again. |
| `contacts.keys_reset.banner_unnamed` [sc] | This contact reset their account keys. Verify the new safety number before messaging them again. |
| `contacts.keys_reset.banner_action` | Verify |
| `contacts.keys_reset.verify_first` [sc] | Verify {name}'s new safety number first. |
| `contacts.keys_reset.verify_first_short` [sc] | Verify the new safety number first. |
| `contacts.keys_reset.open_contact` | Open contact |
| `contacts.keys_reset.picker_sub` [sc] | Account keys reset. Verify the new safety number first. |

`contacts.safety.reset.*` (the safety-number section after a reset):

| Key | English |
|---|---|
| `contacts.safety.reset.title` [sc] | Verify the new safety number |
| `contacts.safety.reset.body` [sc] | {name}'s account keys were reset, so the safety number you may have compared before no longer applies. Compare this new one with {name} in person or on a call. |
| `contacts.safety.reset.body_unnamed` [sc] | This contact's account keys were reset, so the safety number you may have compared before no longer applies. Compare this new one with them in person or on a call. |
| `contacts.safety.reset.checkbox` [sc] | I compared the new safety number with {name} and it matched. |
| `contacts.safety.reset.checkbox_unnamed` [sc] | I compared the new safety number with this contact and it matched. |
| `contacts.safety.reset.warning` [sc] | If you confirm without comparing, someone who took over {name}'s account would look like {name} to you. |
| `contacts.safety.reset.warning_unnamed` [sc] | If you confirm without comparing, someone who took over this account would look like your contact to you. |
| `contacts.safety.reset.confirm` [sc] | I verified the new safety number |
| `contacts.safety.reset.confirming` | Verifying... |
| `contacts.safety.reset.done` | {name} is verified again. |
| `contacts.safety.reset.done_unnamed` | This contact is verified again. |
| `contacts.safety.reset.save_failed` | Could not save. Try again. |

`chat.keys_reset.*` (shared groups and the one-to-one):

| Key | English |
|---|---|
| `chat.keys_reset.bubble_warning` [sc] | Not verified since {name} reset their account keys. |
| `chat.keys_reset.member_being_removed` [sc] | Account keys reset. Being removed. |
| `chat.keys_reset.member_flag` [sc] | Account keys reset. |
| `chat.keys_reset.system_removed` [sc] | {name} reset their account keys. Their old devices were removed from this group. |
| `chat.keys_reset.one_to_one_closed` [sc] | {name} reset their account keys. This chat is closed. |
| `chat.keys_reset.one_to_one_row` [sc] | Chat closed. {name} reset their account keys. |
| `chat.keys_reset.one_to_one_composer` | You can no longer send messages in this chat. |
| `chat.keys_reset.start_new_chat` | Start a new chat |
| `chat.keys_reset.owned_notice` [sc] | {name}, the owner of this group, reset their account keys. Their old devices cannot be removed from this group and the group cannot get a new owner. To keep chatting securely, recreate the group. |
| `chat.keys_reset.owned_notice_member` [sc] | An admin can recreate it. |

`group.recreate.*`:

| Key | English |
|---|---|
| `group.recreate.action` | Recreate group |
| `group.recreate.title` | Recreate group |
| `group.recreate.members_heading` | Members ({count}) |
| `group.recreate.excluded_reset` [sc] | Account keys reset. Verify the new safety number to include them. |
| `group.recreate.note` | The old group stays as it is for everyone until you close it. |
| `group.recreate.submit` | Create and close the old group |
| `group.recreate.working` | Creating... |
| `group.recreate.done` | Done |
| `group.closed.system` [sc] | {name} closed this group after the owner's account keys were reset. |
| `group.closed.composer` | This group was closed. |
| `group.closed.open_new` | Open the new group |
| `group.closed.row` | Group closed |
| `group.closed.panel` | This group was closed. Continue in {name}. |

`channels.keys_reset.*` (the resetting owner's own list only):

| Key | English |
|---|---|
| `channels.keys_reset.row_sub` [sc] | You can no longer post here. Your account keys were reset. |
| `channels.keys_reset.notice` [sc] | You can no longer post to this channel. Your account keys were reset, and the channel's posts were signed with the old ones. Subscribers are not told; create a new channel and share it with them. |
| `channels.keys_reset.remove` | Remove from this device |

### 16.8 Data the UI needs

From the core and the contact store (client-side; nothing new from any
server, the ADR's phase-2 trigger is the KT log's `is_reset` the clients
already read):

- Per contact: `keyState` (`normal` / `reset-unverified`), `previousKey` (the
  pin before the reset), `pendingKey` (the published key the log confirmed as
  a reset), and the stamp of the observation; persisted per account, across
  restarts; cleared with the contact. **The contact record must be found by
  account id across the key change**: today's accepted-contact row is keyed
  on the signature key, and a re-pin that left the rename, the folders, the
  pins and the mute behind on the old key would make the verified contact a
  stranger. Which store re-keys and which gains an index is the client's;
  the UI needs one contact before and after.
- The confirmation of 16.2: re-pin `pendingKey` under the account id with a
  marker that is monotone past the old one (the log entry's version, or the
  key generation the directory serves, is the core's choice; the UI needs a
  success and a failure).
- The identity check on the scheduled device-list arm (16.1 point 2), and
  the arm's **add branch suppressed** for an account whose contact is
  `reset-unverified` or whose published key is not the pin: without that
  suppression every member's client would propose adding the new identity's
  devices to every shared group within ten minutes of the reset, ahead of
  any verification (a plain member's proposal is refused by the core, an
  admin's is not). The arm's "removed" memory must record the old devices
  once the eviction lands, as it does for any removal it saw.
- Per group, per flagged contact: whether the eviction was proposed, when,
  and whether it applied (the `applied` test of `proposal-based-membership.md`
  section 4, compared on `previousKey`'s device leaves), for the row
  annotation and the automatic re-request; a group's kind (one-to-one /
  group, ADR-0062) and whether its owner key is `previousKey`, for the three
  branches of 16.3, 16.4 and 16.5; and whether the group has a role table at
  all.
- The resolver that names a message's sender by account key must fall
  through to the contact by **account id** when the key is a flagged
  contact's `pendingKey`, so the bubble warning names the person.
- A one-to-one's **closed** state, persisted, with the reason, driving 16.4;
  a late message into a closed one-to-one dropped before it is stored.
- A **group-closed** envelope (`lakko-contacts::group_closed`, the
  group-name template: a bounded, domain-separated application message
  carrying the closing stamp and the new group's id, or no id when the group
  was closed without a successor; accepted at receipt only from an owner or
  admin resolved through the live role table under the per-group lock,
  ordered by a strictly-greater stamp; suppressed by the three background
  filters and the NSE decode arm like every service envelope) and, per
  group, the closed state it sets, with the new group's id for `Open the new
  group`. **Built 2026-10-04 without a reason code:** the only reason a
  client closes a group today is 16.5, so the terminal row's text is the
  receiver's, and a second reason would be a version bump. This is the one
  new wire artefact of this section; it is client-to-client and the server
  never parses it.
- Recreate group needs nothing beyond the new-group flow and the add path.
- Channels: on the owning device, which channels were owned under the old
  keys (the role cache already answers `401` on a post; the UI needs the
  state before the person tries), and a per-channel "remove from this
  device" that drops the publisher state.

### 16.9 Cross-platform shape

One model: a flagged contact, a verification in the contact screen, a silent
eviction by admins, an ended one-to-one, a recreated group, an orphaned
channel. The same copy, the same order of controls, the same states.

- **The contact screen** is the one screen on all three (`conversation-info.md`
  2.1); the banner and the section are added to it, not to a second card.
- **The members panel** keeps its platform idiom (web modal, iOS sheet,
  Android bottom sheet); the owned-group notice sits above the existing
  pending notice on all three.
- **The recreate sheet** is the new-group flow's own surface pre-filled, not
  a new screen: web the modal, iOS the sheet, Android the full-screen dialog
  the new-group flow already uses.
- **The bubble warning** is a line under the sender name, drawn by the same
  grouping rule; a right-click / long press on the bubble does not change
  (the warning line is a tap target of its own, to the contact screen).
- **Web** has no toast; the confirmed line renders inside the section.
- Accessibility: the row glyph is never the only signal (the secondary line
  carries the words); the checkbox and its warning sentence are one
  announcement; the banner is announced on the contact screen's open as the
  first element after the header; the system rows are plain text rows; the
  closed composer bar is a status region announced once when the chat opens.
- The three background readers (web service worker, Android background
  decrypt, iOS NSE) preview a message from the new keys like any message and
  do **not** add the warning: the preview names the sender from what the
  extension can resolve, and a notification is not where a security warning
  belongs. The warning is on the bubble.

### 16.10 Open in this section

- **A group with no role table** (no owner, no proposal path): the contact
  is flagged and nothing is proposed; whether such groups still exist for any
  account was not checked. If they do, the honest state is a notice like
  16.5's without a `Recreate group` button.
- **The ghost admin.** A removal rewrites no role table, so an evicted
  admin's key stays in it. The members panel does not show them any more,
  but the key still ranks as a committer candidate and still counts for the
  quorum of the owner's exit. Whether the eviction proposal should carry a
  role-table change dropping them (one role change per sweep, C-65, so it
  may collide with a pending one) is a core question, not a UI one; until
  decided, the UI shows what the roster shows.
- **Two admins recreating the same group**: both succeed, both post a
  closed notice, the later stamp wins the `Open the new group` target, and
  the members end up invited to two groups. Not solved here; an "already
  recreated by {name}" guard on the sheet, driven by a received closed
  notice, would narrow it and is the obvious first step.
- **Channels have no subscriber-side signal**, so a subscriber sees a dead
  channel and nothing else. A server-side "closed by the owner" flag would
  need the owner to be able to set it after the reset, which is the thing
  the reset removed; a key-reset attestation presented to the channels server
  would be a server-held list of the account's channels (invariant #5). Left
  as stated.
- **Verification is per device.** A person with two devices verifies the
  contact on each; the state does not travel over the DeviceGroup. Carrying
  the re-pin as synced state (the ADR-0058 register shape) is possible and
  was not designed, because a verification one device did on another's
  behalf is a claim the second device did not make.
- **The marker for the re-pin** after a reset (16.8): the core's choice; the
  UI needs only that a later re-verification of a later reset is never
  refused as backwards.
- **Call rosters** (ADR-0021) resolve a tile by account id and would name the
  flagged contact by name with no warning; whether the call grid should carry
  the bubble warning on the tile was not designed.
- Nothing in this section was rendered on any client.
