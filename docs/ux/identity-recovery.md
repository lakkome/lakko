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
signup, shown after starting over, shown in Settings, and typed in. The slot
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
  | The phrase saved on this device is no longer the account's (the account started over on another device) | "The recovery phrase saved on this device no longer signs in to this account. Choose another way." |
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
- **I have none of these** expands to the start-over panel: a warning ("This
  creates a new identity for your account. Your contacts will need to verify
  you again, messages on your lost devices stay unreadable, and you get a
  new 12-word recovery phrase to save. Only do this if you have lost your
  recovery phrase, every other device and every passkey that restores
  keys."), a typed confirmation, then section 9.1.
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

### 9.1 The new phrase after starting over

Starting over (section 8) gives the account a new phrase. The old phrase
must stop working and the new one must start working before the person is
asked to write it down.

| State | Screen |
|---|---|
| Finishing | "Save your new recovery phrase" and "Finishing...". The words are not shown. |
| Could not finish | "Your new identity was created, but Lakko could not finish setting up your new recovery phrase. Until that is done the new words would not sign you in, so they are not shown yet. Try again." with **Try again** and **Log out**. Try again never creates another identity; it only repeats the last step. |
| Rate limited | The shared `429` line with its countdown; Try again is disabled until it reaches zero. |
| Offline | "Could not finish. Check your connection and try again." |
| Ready | The new phrase in the grid: "You created a new identity for @name. These 12 words are the only way to recover it if you lose access again. Write them down in order and keep them somewhere safe.", "Your contacts will see that your safety number changed and will need to verify you again before messaging you securely.", and the acknowledge gate "I have written down these 12 words and understand that losing them means losing access to this account." |

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
  starting over.
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
  account started over on another device): the three-word route is paused
  on that device (row absent, Settings shows
  `settings.recovery.words.suspended`) until the account is restored there
  again; the phrase itself is kept. The device is not told that its identity
  is stale; open.
- **"Show recovery phrase"** is withheld while 9.1's "could not finish"
  state is pending: the row shows the pending explanation with Try again.
- The catalog items of section 11.
- Nothing in this spec was walked in a browser or on a device against a
  server.
