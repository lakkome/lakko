<!-- SPDX-License-Identifier: AGPL-3.0-or-later -->
<!-- SPDX-FileCopyrightText: 2026 Lakko Contributors -->

# Identity recovery

UX spec for getting an account back: on a device that does not hold its keys
(a new browser, a new phone, a reinstalled app), or on a device that still
holds them when the password is forgotten. Architecture:
[ADR-0072](../adr/0072-recovery-model-simplification.md), which amends
[ADR-0049](../adr/0049-identity-recovery-word-vault.md),
[ADR-0018](../adr/0018-recovery-factor-cascade.md) and
[ADR-0017](../adr/0017-phrase-seeded-identity.md); and
[ADR-0073](../adr/0073-account-key-reset.md) for what resetting the account
keys does to the other devices (section 15, added 2026-10-03) and what a
contact of the person sees afterwards (section 16: rewritten 2026-10-04 as
the silent model, where the answer is "nothing"; designed, not built).

**Rewritten 2026-10-02**, then brought in line the same day with two later
decisions and with what was built on web. The model:

- The recovery phrase is **12 words**.
- **Three words are checked by the device itself**, on a device that already
  holds the account's keys and phrase. They sign the person in there and
  nothing more. There is nothing to set up and nothing about them on a
  server.
- **A passkey restores keys** wherever the platform supports it, with no
  choice to make (amended 2026-11-11, section 10.2).
- **Another device is the first route** everywhere.
- **Every device that has the keys also has the phrase**, so it can show it,
  check three words and hand the keys to a further device.
- Guardian shares and the email / phone code are gone.

Build status is recorded in ADR-0072 and `docs/PRODUCT-READINESS.md`. In
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
             Signs you in. Brings back your chats too when the
             passkey can carry your keys.
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
| Recovery phrase or passkey, another device of the account answered | Fills as that device adds this one. No banner: what happens in the background is not the person's to act on (amended 2026-11-11, maintainer). |
| Recovery phrase or passkey, no other device answered | Fills as contacts open Lakko and their devices add this one. Until the first chat arrives the empty list carries a card: "Your keys are restored. Your chats come back as your contacts open Lakko. Messages from before are not restored." with a button to Contacts. Not a loading state; no spinner. |

Amended 2026-11-11 (maintainer). "Answered" is the ADR-0081 third
amendment's answer: after a restore the device knocks on the account's
DeviceGroup, and a running device answers at once (a presence note or a
Welcome). Whether another device is merely listed in the directory says
nothing: a lost device stays listed, which put the banner on exactly the
restores where no other device existed. A device that founded the
DeviceGroup because nobody answered (or because no other device was
listed) shows the card; one that was answered shows the ordinary list (the
banner "Your other devices are adding this one to your chats." is removed
on every client). The card needs a restored device that knew at least one
conversation (the backup's list of
conversations); one that knew none shows the ordinary empty list, since
there is nothing to add it to. Both go away when the first conversation
arrives, as before. The answer is kept per account on the device, so a
restart shows the same state.

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
             A passkey that restores your keys.
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
  section 15.1 (ADR-0073 widened what a reset does; the earlier text here,
  "This resets your account keys. Your contacts will need to verify you
  again, messages on your lost devices stay unreadable, and you get a new
  12-word recovery phrase to save. Only do this if ...", is replaced by it).
- **Log out** leaves the screen without changing anything.
- States per route: the shared table of section 2.

## 8a. Restore your keys: the earlier keys are missing (2026-11-15)

ADR-0074, "Joining after a rotation". After the account's keys were changed,
its chats keep using the keys from before the change. A device that signs in
or is restored after that, and does not yet hold those earlier keys, does
not open the app: it would join chats under the wrong keys and never reach
the account's existing ones. It shows a variant of the section-8 screen
instead, on every route (sign-in, the phrase, an email code, a passkey,
another device).

```
Restore your keys

You are logged in as @name, but this device does not have the
keys your chats use yet. Bring them from another device that is
still logged in.

  [devices]  Another device                 >
             Open Lakko on a device that is still logged in.

  I have none of these  v

Log out
```

- **Another device** is the section-3 flow without the username field, as
  on the section-8 screen. The keys come with it; when it succeeds the app
  opens.
- **Passkey and Recovery phrase are not on this screen.** Both bring the
  keys this device already has.
- **No progress, no counter, no time limit.** The device also looks for the
  earlier keys by itself (its saved copy, at start and every few minutes
  while the screen is open); when it finds them the screen gives way to the
  app with no message. A failed look is not shown.
- **I have none of these** expands to the same **Reset account keys** panel
  as section 8 (warning of section 15.1, typed confirmation, then 9.1).
- **Log out** as in section 8.
- Relaunching the app on this device returns to this screen until the keys
  arrive.

Copy (new catalog keys; `security_critical` like the section-8 lede):

| Key | English |
|---|---|
| `restore_identity.earlier_keys.lede` | "You are logged in as {username}, but this device does not have the keys your chats use yet. Bring them from another device that is still logged in." |
| `restore_identity.earlier_keys.another_device.sub` | "Open Lakko on a device that is still logged in." |

The title, the row title, "I have none of these" and "Log out" reuse the
section-8 keys.

Data the UI needs: the parked reason (earlier keys missing, as opposed to no
keys at all), the username. Nothing about the keys themselves is shown.

Cross-platform shape: one parked state on all three, rendered by the existing
restore-prompt screen (web `auth/RestoreIdentityPrompt.tsx`, iOS
`UI/Auth/RestoreIdentityPromptScreen.swift`, Android
`ui/auth/RestoreIdentityPrompt.kt`) with the rows of this section.

## 8b. The username after a passkey sign-in (2026-11-15)

A passkey sign-in on a new device needs no name typed, so the session lands
with no username. Without it the device has no login handle: it cannot ask
the account's other devices to let it in (ADR-0081, it waits and retries)
and Settings shows no name. Seen live on an iPhone the same day: it waited
forever. The device learns the name like this, the same on every client.

1. **From the backup first.** The username the backup's own profile names,
   then the account's username as the restored synced state holds it. Each
   is checked (step 3); the first the account holds is taken with no screen.
2. **Else ask**, once the session has landed. The prompt is a sheet over the
   app, which stays usable behind it; it is never a step of the sign-in.

```
What is your username?

This device does not know your account's username yet. Enter it
to sync with your other devices.

  Username
  [                    ]

  [ Continue ]   I don't remember
```

3. **Continue** first applies the username rule (a name it refuses shows the
   rule's usual line and sends nothing), then asks auth whether this account
   holds that name. Auth compares with this account only and never says
   whether the name belongs to someone else.
   - **It is this account's name:** the name is kept as after a sign-in by
     name (shown in Settings, used to sync); the sheet closes; the device
     asks its other devices to let it in at once, not at its next retry.
   - **Another name:** "That is not the username of this account." The
     field keeps what was typed.
   - **The account has no username:** the sheet closes; nothing else.
   - **Too many tries:** "Too many tries. Try again later."
   - **No answer:** "Couldn't check the username. Check your connection and
     try again."
4. **I don't remember** shows "You can see it in Settings on your other
   device. Until then this device does not sync with them." with OK, then
   closes. The next start of the app asks again while the name is still
   unknown. Closing the sheet (Esc, the close button, the backdrop) is the
   same, without the note.

States: hidden; asking (idle, checking with the button disabled, each error
line); the forgot note. A guest account (ADR-0032) has no username by
design and is never asked. Neither the name nor its handle is logged.

Copy (catalog keys, not security-critical):

| Key | English |
|---|---|
| `auth.username_prompt.title` | "What is your username?" |
| `auth.username_prompt.lede` | "This device does not know your account's username yet. Enter it to sync with your other devices." |
| `auth.username_prompt.field` | "Username" |
| `auth.username_prompt.continue` | "Continue" |
| `auth.username_prompt.forgot` | "I don't remember" |
| `auth.username_prompt.forgot_note` | "You can see it in Settings on your other device. Until then this device does not sync with them." |
| `auth.username_prompt.error.not_this` | "That is not the username of this account." |
| `auth.username_prompt.error.too_many` | "Too many tries. Try again later." |
| `auth.username_prompt.error.network` | "Couldn't check the username. Check your connection and try again." |

Data the UI needs: the session's username; the username the restored own
profile names; the stored own-username register (ADR-0076); whether the
landing's backup step has finished; the answer of `POST
/auth/login-handle/check` (`same` / `other` / `none`, `429`).

Cross-platform shape: one rule and one sheet on all three. Web: the rules in
`auth/username-prompt.ts`, the sheet `auth/UsernamePrompt.tsx` (a
`ModalLayer`, mounted in `App.tsx`), the rejoin wait cut short in
`DeviceGroupSyncContext`. iOS and Android: the platform's existing sheet
style.

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

**Step 3 is retired** (2026-11-25, removed from web, which still showed it after a key change; the phones never did): section 19 shows no phrase at signup and offers a passkey as its own row of "Protect your account" (19.1a), so no phrase screen ends with a passkey offer. What it said: **Step 3, offer a passkey** (only where passkeys are supported and the
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
| Ready | The new phrase in the grid: "The account keys for @name were reset. These 12 words bring the account back on a new device if you lose access again. Write them down in order and keep them somewhere safe.", and the acknowledge gate "I have written down these 12 words and understand that losing them means losing access to this account." |

Contacts are told nothing about the reset (section 16, silent continuity); the
line that once said they would see a changed safety number was removed from this
table on 2026-11-25 (no client showed it).

If the app is closed in the "could not finish" state, the next time it opens
signed in it retries in the background, and the screen returns until the
step is done.

## 10. Settings > Security

### 10.1 Recovery

```
Recovery

Brings back your chats on a new device
  Recovery phrase     Your 12-word recovery phrase restores your
                      keys and contacts on a new device. Saved
                      on this device.
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

### 10.2 Passkeys restore keys

**Amended 2026-11-11 (maintainer):** there is no choice any more between a
passkey that only signs in and one that also restores keys. Every passkey
restores keys wherever the device and the passkey provider can carry them;
the choice screen, its warning and **Stop restoring keys** are gone. The
cost is stated in ADR-0072 (decision 6, amended): whoever can use the
person's synced passkeys can read their future messages and act as them.

The list shows each passkey with its name, when it was added and last used,
and one of two labels, which are a state and not a setting: **Logs in and
restores keys** or **Logs in only**.

Adding a passkey:

1. **Add passkey** > name it > the system sheet creates it.
2. The keys are then stored for it, with nothing to choose: from what the
   creation returned, else from one more system sheet for that passkey (some
   platforms give what is needed only when a passkey is used). Result:
   "Passkey added. It logs you in and restores your keys." and the row is
   **Logs in and restores keys**.
3. When the platform cannot protect keys with this passkey: "Passkey added.
   It logs you in. On this device and passkey provider it cannot restore your
   keys, so keep your recovery phrase." and the row is **Logs in only**.
4. Storing the keys needs the recovery phrase on this device (every device
   with the keys holds it, ADR-0072). If it is missing, the passkey is added
   as **Logs in only** and its row offers **Restore keys with this passkey**
   (below), which asks for the phrase.

When the system sheet refuses because this device's passkey provider
already holds a passkey for this account (WebAuthn `InvalidStateError`, the
excluded-credential case), the line is "This device already has a passkey
for this account." and nothing changes; it is never shown as the browser or
device being unable to create passkeys (amended 2026-11-11).

States while storing: working; cancelled at the second sheet ("The passkey
was added. It logs you in only; restore keys with it from its row."); a
different passkey answered ("A different passkey answered. Try again and
choose the passkey you just added."); `503` ("Restoring keys with a passkey
is not available on this server."); offline or another failure after the
passkey was created ("The passkey was added. It logs you in only for now;
restore keys with it from its row."). The second sheet is skipped only when
the platform said at creation that the passkey cannot carry keys; when it
said nothing, the sheet runs. A passkey this device found cannot carry keys
is remembered on the device, so its row does not offer the repair again.

A passkey that can carry keys but holds none (created before this change,
on another client, or after the account keys changed) shows **Logs in only**
with one action on its row, **Restore keys with this passkey**: one system
sheet, the twelve-slot input first when this device does not hold the phrase
("Type your recovery phrase to restore keys with this passkey."; the phrase
must be this account's: "That recovery phrase does not belong to this
account. Nothing was changed."), then "This passkey now restores your keys."
A different passkey answering here reads "A different passkey answered. Try
again and choose this passkey."; dismissing the sheet changes nothing and
shows nothing.
This is a repair, not a choice: there is nothing to turn off.

Removing a passkey also removes what it stored for restoring keys; the
existing passkey-confirmation step for removal is unchanged.

A passkey added from "Protect your account" (19.1a) restores keys when it
can, with no choice.

Every passkey is registered as discoverable (the server asks for a resident
key), so a device that holds no account yet finds it with **Use passkey** and
no username.

### 10.3 Password

A Password card in Settings > Security opens the form of 7.1.

## 11. Copy

The source of truth is `l10n/catalog/en.json`; this spec quotes it and does
not edit it. The groups that carry this spec: `recovery.*`,
`recovery.words.*`, `recovery.passkey.*`, `recovery.new_password.*`,
`recovery_phrase.*` (including `recovery_phrase.fresh.*`),
`restore_identity.*`, `settings.recovery.*`, `settings.passkeys.*` (including
`keys.*`, `keys_choice.*`, `label.*`), `settings.password.*`,
`auth.signup.passkey.*`, `device_link.keys_not_sent`.

Every passkey add and repair line, the three-word lines, the
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
- ~~`settings.account.forget_device.body_device` and `.sub` leave out a
  passkey that restores keys.~~ Done 2026-11-28: the four forget-device lines
  (web, desktop, phones) name another device, a passkey that restores keys,
  the recovery phrase if one was saved, and otherwise the password with a
  second way in, which resets the account keys; `settings.recovery.passkeys.sub`
  no longer speaks of turning restoring on.

## 12. Data the UI needs

From the server (all in ADR-0072):

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

Added 2026-10-03 with [ADR-0073](../adr/0073-account-key-reset.md).
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
  - Your safety number changes.
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
  changes nothing about the decision), so those two bullets do not count
  them.
- **Amended 2026-11-28 (seen on the test stack: the list stood above the
  "cannot reset" panel of an account with no second factor and named an
  authenticator app the account did not have).** The warning (the lede,
  `What it does:` and the bullets) describes a reset this account can do,
  so it is drawn only above the section of 15.1b (its rows, or a confirmed
  factor), once the account's factors are known. It is **not drawn** above
  any "cannot reset" panel (an account with none of the three, `I cannot
  use any of these`, the password unknown of 17.2), nor while the factors
  are still being read; then the panel stands alone. On iOS the inline
  heading `Reset account keys` goes with it; Android keeps it as the
  screen's title.
- The last bullet names what keeps working, only what the account has: the
  password always, then the authenticator app and the recovery email when
  present ("Your password keeps working." / "Your password and your
  authenticator app keep working." / "Your password and your recovery email
  keep working." / "Your password, your authenticator app and your recovery
  email keep working.", `restore_identity.reset.keeps.password`,
  `.password_totp`, `.password_email`, `.password_totp_email`). The
  passkeys are never in it: the reset deletes them.
- The contacts bullet says only "Your safety number changes."
  (`restore_identity.reset.effect_safety_number`). Whether the chats come
  back is said once, by the line of 15.1c, which is the only place that
  knows whether the reset is sealed; the earlier "Your contacts keep their
  chats with you" (`effect_contacts`) and the "Chats, groups and channels
  come back..." bullet are gone, because an unsealed reset closes the old
  one-to-ones. The new-phrase screen after a reset
  (`recovery_phrase.fresh.safety_note`) says the same short sentence for
  the same reason.
- After the tap: a full-screen "Resetting..." with no cancel (the server
  step is one operation and cannot be half done), then section 9.1 in its
  new form (15.4).
- Failures before anything changed, shown inline under the button and
  leaving the panel open: the shared `429` line with its countdown; "Could
  not reset. Check your connection and try again." (offline); "Could not
  reset. Try again later." (any other server refusal). A failure **after**
  the server step is 15.4, never this panel.

### 15.1a The password, proven now (added 2026-10-04)

A reset needs the account password proven **just now**, not merely a
signed-in session: the server refuses `POST /account/keys/reset/*` without a
reset grant, and only a password sign-in mints one (ten minutes, one use).
A stolen session, or an unlocked phone in someone else's hands, cannot reset
the keys. The authenticator code is **not** asked for: the authenticator
usually lives on the phone the person lost.

What the person sees on the warning screen of 15.1:

| How they got here | The screen |
|---|---|
| Signed in with the password less than ten minutes ago (the ordinary lost-everything path) | Exactly 15.1. No extra field. |
| Signed in another way (passkey, authenticator, another device), or more than ten minutes ago | One `Password` field above the confirmation word, with the helper `Enter your password to reset the account keys.` The button stays disabled until both the password and the word are typed. |
| As above, and this device does not know the account's username (a passkey sign-in that named no account) | The sign-in screen's `Username` field above the password, with its own label; the button needs it too. A name that belongs to a different account is answered as a wrong password. |

| State | What the person sees |
|---|---|
| Wrong password | Under the field: `Wrong password. Check it and try again.` Nothing was reset. |
| Too many attempts | The existing rate-limit line of the sign-in screen, with the wait. |
| The grant expired between opening the screen and pressing the button | The `Password` field appears with the helper; nothing else changes; no error styling (nothing went wrong). |
| Offline / other | `restore_identity.reset.error.offline` / `.generic`, as in 15.1. |

The password field is the platform's secure text field with the show / hide
control the sign-in screen uses; it is never pre-filled and never kept after
the screen closes. Submitting runs the password sign-in first and the reset
second; the person sees one `Resetting...` state for both.

**Built 2026-10-04** on `services/auth` (DB-tested) and on web, iOS and
Android (unit tests only; nobody has seen the field on a screen or run the
round trip against a server).

**Amended 2026-10-06 (maintainer's decision; designed, not built).** Section
17 generalises this subsection into one rule shared with deleting the account
and adding a device. Two things change here:

- **A passkey counts.** A sign-in by passkey in the last ten minutes
  qualifies like a password sign-in: exactly 15.1, no field. When no proof is
  live and the account has a passkey this device can use, `Use a passkey
  instead` (`proof.passkey.use`) sits under the `Password` field; the gesture
  alone satisfies the screen and the field may stay empty. Cancelling the
  system sheet returns to the idle form with `Cancelled. Nothing was reset.`
  (`proof.reset.cancelled`). The first row of the table above therefore reads
  "signed in with the password **or a passkey** less than ten minutes ago",
  and the second row's "passkey" moves out of "another way".
- **Authenticator only, password unknown.** A person who got in with the
  authenticator code and does not know the password cannot reset. The screen
  says so instead of showing a button that can never work; see 17.2.

**Amended 2026-11-26 (maintainer's decision; designed, not built): the
password is not enough any more.** Section 15.1b adds a second factor after
the password. Two lines of the 2026-10-06 amendment above no longer hold for
the reset: a passkey sign-in no longer stands in for the password (a passkey
is now one of the second factors, 15.1b), and `Use a passkey instead` is not
drawn in the reset's proof block. The sentence of 15.1a "The authenticator
code is **not** asked for" is reversed: it is one of the three factors.
Deleting the account and adding a device keep the 17.1 rule unchanged.

### 15.1b The second factor (added 2026-11-26)

Status: designed, not built. Maintainer's decision of 2026-11-26
([ADR-0073](../adr/0073-account-key-reset.md), amendment of that date):
since ADR-0101 a reset brings back chats, groups, channels and recent
history, so whoever resets must prove more than the password. A reset needs
the password, proven as in 15.1a (a password sign-in in the last ten
minutes, or the field), **and** one of these, whichever the account has:

- a code from the authenticator app;
- a code sent to the recovery email;
- a passkey of the account. A passkey that does not restore keys counts;
  the reset deletes it afterwards, as the warning says.

The recovery phrase is not one of them: whoever has it restores the keys
(section 8) and does not reset.

The reset panel, top to bottom: the warning of 15.1 with its bullets (only
while this section is drawn, 15.1 as amended 2026-11-28), the section
below, the typed word, the buttons.

```
Confirm it is you

Password
[ ............        (show) ]
Enter your password to reset the account keys.

Then confirm with one of these:
  [authenticator] Authenticator app                  >
  [mail]          Code by email                      >
  [passkey]       Passkey                            >

I cannot use any of these

Type RESET to confirm
[            ]

[Reset account keys]      Cancel
```

- The `Password` block is the 17.1 component without `Use a passkey
  instead`. It is not drawn while a password proof is live (a password
  sign-in less than ten minutes ago); then the section starts at "Then
  confirm with one of these:" and its first line reads "Confirm with one of
  these:" (`proof.reset.factor.title_no_password`).
- The rows are the factors **this account has**; the device is signed in
  and asks the server. `Passkey` is also hidden where this device or
  browser cannot use passkeys. The order is fixed: authenticator, email,
  passkey. A row the account does not have is absent, never disabled.
- Choosing a row expands it in place; the other rows stay visible and can be
  chosen instead at any moment, and the expanded one closes.
- **The password is checked first**, at the moment the person confirms a
  factor (the sixth digit of an authenticator code, `Send code`, `Use
  passkey`). With no live password proof the device signs in with the typed
  password, then checks the factor. A wrong password stops there: the
  factor is not tried and an authenticator code is not used up.
- Once a factor is confirmed the section collapses to one line with a check
  mark: "Confirmed with your authenticator app." / "Confirmed with a code by
  email." / "Confirmed with a passkey." (`proof.reset.factor.done.totp`,
  `.email`, `.passkey`). The typed word and the button below become usable.
- A confirmation lasts ten minutes (chosen, not measured; the same as the
  password proof). If it runs out before `Reset account keys` is pressed,
  the section opens again as it was, with no error styling (nothing went
  wrong).
- `Reset account keys` is enabled only when a factor is confirmed and the
  word is typed.

**Authenticator app**, expanded:

```
Enter the 6-digit code from your authenticator app.
[ _ _ _ _ _ _ ]
```

The platform's one-time-code field; the code is checked on the sixth
digit, with no separate button.

**Code by email**, expanded:

```
Recovery email   [ name@example.com ]
[ Send code ]
```

then, in place:

```
If this is the recovery email of @name, a code is on its way.
It works for 10 minutes.
[ _ _ _ _ _ _ ]
Send again (in 0:45)        Use a different address
```

- The address is pre-filled when this device kept it (ADR-0075, amended
  2026-10-13), otherwise typed. The answer to `Send code` is the same
  whether or not the address is the account's, as in 19.3.
- ~~On a **mail-protected account** (no phrase saved, 19.1), the email row
  carries one more line under it ... `Use my email instead` ...~~ Removed
  2026-11-26 with the email vault: no mailed code brings keys back.

**Passkey**, expanded: one button `Use passkey`. It opens the platform's
own passkey sheet (the one system surface allowed here, 17.1) with
`recovery.passkey.waiting` behind it.

States of the section:

| State | What the person sees |
|---|---|
| Wrong password | `restore_identity.reset.error.wrong_password` under the field; no factor was tried. |
| Wrong authenticator code | "That code did not work. Wait for the next code in the app and try again." (`settings.recovery.totp.error.wrong`). The digits clear and keep focus. |
| Too many authenticator codes | `recovery.error.rate_limited` with the countdown, under the code field, which stays disabled until zero. The email and passkey rows stay usable. |
| Email: wrong code | "That code is not right." (19.2) The digits clear. Five tries per code. |
| Email: too many wrong codes | "Too many attempts. Send a new code." The row returns to its `Send code` state. |
| Email: too many letters | `recovery.error.rate_limited` with the countdown under `Send code`. |
| Email: the letter could not be sent | "We could not send the letter. Try again later." |
| Email: code expired | "That code has expired." with `Send again`. |
| Passkey sheet up | `recovery.passkey.waiting`. |
| Passkey cancelled | `proof.reset.cancelled` ("Cancelled. Nothing was reset."), back to the row. |
| Passkey of another account | `proof.passkey.other_account`. |
| Passkey failed otherwise | `proof.passkey.failed`. |
| Offline | `common.error.offline`. Typed values are kept. |

The numbers: an authenticator code is limited to 5 tries per 15 minutes per
account (chosen, not measured; the same budget as the authenticator
sign-in); email codes keep the letter and code limits of 19.2.

**I cannot use any of these**, and an account that has none of the three,
show the "cannot reset" panel in place of the section, the typed word and
the button; `Cancel` stays. The warning of 15.1 is not drawn above it
(amended 2026-11-28: it described a reset the account cannot do):

```
The account keys cannot be reset

Resetting needs your password and one more way to confirm it is
you: your authenticator app, your recovery email or a passkey.

What you can do:
  - Sign in on a device that still has your keys, then add this
    device from it.
  - Enter your 12-word recovery phrase.
  - Use a passkey that restores keys.

Without one of these, the keys of this account cannot be restored
or reset.

Back                                                      Cancel
```

- Keys: `proof.reset.no_factor.title`, `.body`, and the existing
  `proof.reset.cannot.what_you_can_do`, `.option_device`, `.option_phrase`,
  `.option_passkey`, `.otherwise`. `Back` (present only when the panel came
  from `I cannot use any of these`) returns to the section.
- For an account that has none of the three the body reads "This account
  has no second way to confirm it is you, so its keys cannot be reset."
  (`proof.reset.no_factor.body_none`) and there is no `Back`. Such accounts
  were made before 2026-11-26; on the test stack they are recreated, and no
  production account exists. The panel has **no control to add a factor**:
  adding one needs only the password, so a person who knows nothing but the
  password could add a factor and reset at once.
- No disabled `Reset account keys` button anywhere in this state, and no
  sentence suggesting support or a later way.

After the reset:

- The passkeys are gone (15.1). An account whose only factor was a passkey
  has none left, so after the new-phrase or "Your account has new keys"
  state of 15.4 the device shows the protect step of 19.1a before the app,
  with no way past it (19.1c).
- ~~When the factor was a code by email, the device ... stores the new
  phrase's email vault~~ Removed 2026-11-26 with the email vault.
- The channels the account owns come back by themselves after the reset
  (ADR-0101 section 7, amended 2026-11-26): the password and the confirmed
  factor together open the list of them; neither does alone. Nothing is
  shown while that runs; each channel appears in the list when it is back.
  When the password was changed by a mailed code on a device without the
  keys, with no device holding the keys since, the owned channels come back
  only if a passkey that keeps the chats sealed the reset (its own lock
  opens the list, ADR-0101 section 7 item 3a); otherwise none does (19.10),
  and the reset is unsealed (15.1c).
- The new keys publish the account's protection again (ADR-0101 section
  10) with the current password in it; the passkeys were deleted and the
  new phrase is not saved, so the notice of 19.1d applies until a phrase or
  a passkey that keeps the chats is added.

### 15.1c Will this reset bring things back? (added 2026-11-26)

Status: designed, not built. Maintainer's decision of 2026-11-26
(ADR-0101 section 10). Contacts move the chats, return the history, the
profile and the channel subscriptions only for a **sealed** reset: one
signed by something the account set up before the reset and published under
its old keys. The seals an account can have: its current password (every
account), each passkey that keeps the chats (19.1d), and the recovery phrase
once saved. A reset without a seal is **unsealed**: it still works and takes
the username, but the account starts over and the person reaches contacts
again by contact requests.

The device knows which it is before the person types the word: once the
factor of 15.1b is confirmed it checks whether the password just typed (or
the passkey just used) matches a seal the account published. It never asks
the server whether the reset will be sealed.

The line stands between the confirmed factor and `Type RESET to confirm`,
and replaces the warning bullet "Chats, groups and channels come back from
your contacts' devices..." of 15.1. It is the only place on the screen that
says whether the chats come back: the warning's contacts bullet says only
that the safety number changes (15.1, amended 2026-11-28).

**Sealed:**

> Your chats, groups and channels will come back from your contacts'
> devices, with the messages they still have, newest first.

(`restore_identity.reset.sealed`)

**Unsealed:**

> This reset starts your account over. Your chats, groups and the channels
> you own will not come back. You keep your username, and you can send your
> contacts new requests.

(`restore_identity.reset.unsealed`, in the `--warning` colour.) When the
account has a passkey that keeps the chats and it was not the factor used, a
line follows: "Use a passkey that keeps your chats to bring them back." with
`Use passkey` (`restore_identity.reset.unsealed_use_passkey`), which runs the
passkey factor of 15.1b again; a seal from it makes the line the sealed one.

Why a reset is unsealed, in practice: the password was changed by a mailed
code on a device without the keys and no device holding the keys has been
opened with it since (19.3); no passkey keeps the chats; no phrase was saved.
The notice of 19.1d tells the person this in advance.

What the person sees after an unsealed reset: 15.4 as for any reset, then
the app with no conversations and no owned channels. Contacts see what 16.11
says.


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
  anomaly, not a known reset; ADR-0073 4.2 rule 4). It is not expected to
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

**Waiting for the directory (amended 2026-11-12, seen on an iPhone).** The
directory accepts a reset's new keys only once an hour after the
account's keys last changed (the cool-down of ADR-0073 section 4.1); a
second reset inside that hour was answered "Try again", which could not
succeed for up to an hour and did not say so. The refusal now carries how
long is left, and the screen says it instead of the "could not finish"
copy:

> Your account keys were reset less than an hour after the last change,
> so they can be published in 40 minutes. Lakko will finish by itself and
> then show the new words.

For an account with no saved phrase (ADR-0075) the line names no words:
"Your account has new keys. The change finishes in 40 minutes, by
itself." There is no Try again button in this state. The app retries by itself
when the time is up (while the screen is open) and whenever it opens
signed in; the minutes count down while the screen is open. Any other
refusal or a network failure keeps the "could not finish" copy and Try
again.

### 15.5 Data the UI needs

From the server (all in ADR-0073):

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
- Since 2026-11-26 (15.1b): which of the three second factors the account
  has (a confirmed authenticator, a recovery email, passkeys); one check per
  factor that, after a password sign-in, answers "confirmed" with a
  ten-minute expiry or a refusal with a retry time; the email check sends a
  letter only for the account's address and answers alike either way; the
  reset itself refuses without a confirmed factor.

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
  after a reset (ADR-0073 open question 1). The copy above says they keep
  working; it changes if the maintainer decides otherwise.
- What a contact sees when the person who reset the keys writes to them
  from the new keys, and the eviction of the old devices from shared
  groups: ADR-0073 phase 2, section 16 of this spec (rewritten 2026-10-04).
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

## 16. A contact reset their account keys: nothing to see, nothing to do

**Rewritten 2026-10-04 (maintainer's decision).** The first version of this
section (2026-10-03, built on all three clients on 2026-10-04) made a
contact's reset a visible event: a flagged contact, a banner, a mandatory
safety-number comparison with a checkbox, blocked actions, a warning under
message bubbles, a closed one-to-one with `Start a new chat`, an owned-group
notice with `Recreate group`, a closed-group shape. The maintainer rejected
all of it ("what do I care what Anna does with her keys; what matters to me
is that Anna is Anna and not someone else") and, offered a single quiet
system row, declined that too: no line of any kind.

**Status: designed, not built.** What is in the three clients today is the
visible flow this section replaces; 16.9 lists what has to be removed. The
architecture is [ADR-0073](../adr/0073-account-key-reset.md), section "Phase
2 revised (2026-10-04): silent continuity".

Section 15 is the person who resets. This section is everyone else: a
contact of theirs, a member of a group they are in.

### 16.1 The model

When a contact resets their account keys, the other person **sees nothing
and does nothing**. The same contact, the same conversation row in the same
place, the history where it was, the composer and the call buttons live.
There is no flag, banner, checkbox, blocked action, warning line, system
row, closed chat, `Start a new chat`, `Recreate group`, toast or
notification, on any client, for a contact or for a plain co-member.

The client does the work in the background: it checks that the change is a
reset the server attested for that account (16.2), takes the new keys as
the contact's keys, and moves each conversation it shares with them onto
keys the old devices do not hold (16.4 to 16.6).

The one surface that remains is the one that existed before any of this: the
safety-number section of the contact screen. It shows the current number,
computed over the current keys, with its ordinary copy
(`contacts.safety.*`). It does not say the number changed. A person who
compared the number before and opens the section again sees a different
number; that is the whole of what a reset looks like, and only to someone
who looks.

**The cost, accepted by the maintainer.** Someone who takes over Anna's
account (her password and her second factor) and resets its keys looks like
Anna to everyone until someone compares the safety number. Since 2026-11-26
the server cannot do this on its own: a reset moves
conversations only when it is sealed by Anna's own password, passkey or
phrase (16.2 rule 5); an unsealed one is 16.11, the one case in this
section where the other person sees something. This is what the
mainstream messengers do. What Lakko has on top: the reset is signed by the
server for that account, it is recorded in the public key log, and it is
loud for the real Anna, whose devices are signed out with "your account keys
were reset on another device" (15.2).

**What still fails closed, exactly as before phase 2.** A key change that is
not an attested reset is a possible substitution and is refused at the
action that met it, with the existing copy and no state:

| What the client found | Copy at the action |
|---|---|
| The key log shows no reset for the new key | `contacts.identity_changed.unexplained` (`_unnamed`) |
| The key log shows a reset, but the server's signed statement for it is missing or does not check out, or the device has no key to check it with | `contacts.identity_changed.unconfirmed` (`_unnamed`) |
| The key log cannot be read to the end (it answered, and what it answered does not verify) | `contacts.identity_changed.unconfirmed` (`_unnamed`) |

`contacts.identity_changed.reset` and `reset_unnamed` are **deleted**: their
text ends in "re-verify their safety number out of band before continuing",
and there is no longer a step that continues. The `unconfirmed` text
("could not be confirmed against the transparency log") is reused for the
second row as written; a wording that also covers "the server's statement
did not check out" is a copy change for the build, security-critical, and is
listed in 16.10.

A refusal is shown only where the person acted (sending a contact request,
adding to a group, opening a `@name`), as today. It sets nothing on the
contact and changes no row.

### 16.2 What the client checks before it accepts a reset

Not UI, but it decides whether the person sees nothing (accepted) or a
refusal (16.1). The rule is one function, the same on the three clients,
specified in the ADR. In the words of this spec: the published keys for the
account differ from the ones this device holds for it, and **all** of these
are true:

1. the directory serves a reset statement for the account, signed by the
   Lakko server, and it verifies under the server key this device trusts;
2. the statement is for **this** account and for exactly the published keys;
3. it is newer than any reset this device already accepted for the account;
4. the public key log's latest entry for the account is those same keys,
   marked as a reset, and the entry verifies the way every log entry is
   verified. A build with no key log configured skips this step, which is
   every build today: the log is not deployed, and until it is the reset
   rests on the server's signed statement alone (ADR, R.2 and decision D3).
5. **(added 2026-11-26)** the reset is **sealed**: it carries a signature,
   by a seal key the account published under the old keys before the reset
   (its password, a passkey that keeps the chats, or its phrase), over the
   new keys. Rules 1 to 4 are the server's word; this one is the account's,
   and the server cannot produce it (ADR-0101 section 10). When 1 to 4 hold
   and 5 does not, the reset is not refused: it is **unsealed**, and 16.11
   applies instead of 16.4 to 16.7.

Then the device takes the new keys as the contact's, keeps the old ones only
as "this is who wrote the messages already on screen" (so old bubbles still
say Anna), and starts 16.4 to 16.6.

Not yet known is not a refusal. When the directory or the key log cannot be
reached, the device has no verdict: it changes nothing, tells nobody, and
asks again at the next occasion. Only a negative answer produces the copy of
16.1.

**When it runs.** At every place the client already fetches a person's keys
(a contact request, an add, a `@name`, opening the contact screen, a call
roster), at the scheduled per-chat device check every member already runs
(about every ten minutes while a chat is polled), when a one-to-one is
opened, and before a send in a one-to-one whose last check is older than
that interval. A reset noticed while the app is closed is noticed at the
next of these. Nothing is announced when it is.

### 16.3 What each surface shows

| Surface | What the other person sees |
|---|---|
| Contacts list | The same row: same name, same position, no glyph, no second line. |
| Contact screen | Unchanged. `Message`, `Call`, `Video`, `Rename`, `Remove contact` as always. The safety-number section shows the current number with its ordinary copy. |
| Chat list | The same row for the one-to-one and for every shared group: same place, same pin / mute / archive / folder, same unread count, same draft. No "closed" line. |
| One-to-one timeline | The history as it was, then new messages below it. No boundary row where the keys changed. Old messages keep Anna's name. |
| Group timeline | The same. The members panel lists Anna once, as before. No `Members changed` row for the swap of her devices (16.5). |
| Members panel | Anna's row, with the role she had. No annotation, no pending line for the swap. |
| Calls | `Call` and `Video` work as soon as the conversation has moved (16.4). A group call names her tile as before (the tile is resolved through the account, which did not change). |
| Notifications | None for the reset. A message from her new devices notifies like any message, under her name. |
| Search, folders, pins | Unchanged. A pinned message from before the reset stays pinned and opens where it was. |
| Channels | Unchanged for subscribers (16.7). |

**The safety-number "verified" state.** The recon of the three clients
found no stored "compared" mark outside the flow this section removes: the
section shows a number and nothing records that it was compared. If a client
has or gains such a mark, an accepted reset must clear it
silently, because a mark that survives a key change would claim a comparison
that was never made for these keys.

### 16.4 One-to-one

A one-to-one cannot swap a person, and Anna's new devices are a different
set of keys, so the conversation continues in a **new** secure session
behind the **same** row. The person is not told there are two.

What the row is made of, after the move: the old session's messages
(read-only, kept on the device), then the new session's. One timeline, one
scroll, one draft, one read position, one set of pins. Reply-jumps and
pinned-message jumps into the old part still work.

**Who creates the new session.** The other person's device, not Anna's:
Anna's new device knows nobody (a reset is the lost-everything path). The
first of the person's devices to accept the reset creates it, adds Anna's
new devices and the person's own other devices in one step, and from then on
sends there. Two devices of the same person doing it at once still end with
one session (the ADR's "claim, then build").

**States the composer has to handle without announcing anything:**

| Moment | What the person sees |
|---|---|
| Reset accepted, new session ready | Nothing. They type and send as always. |
| Reset accepted, new session not ready (Anna's new device has not published what is needed to add it, or this device is offline) | The composer stays live. A sent message shows the ordinary **sending** state (`chat.message.sending`) and goes out when the session exists. It is never sent into the old session, where only the removed devices would get it. |
| It stays not ready for a long time | The message stays in the sending state, and after the client's ordinary give-up it shows the ordinary failed state (`chat.send.failed`) with its ordinary retry. No special copy. |
| Reset not noticed yet, the person sends | The message goes into the old session and shows as sent. Anna cannot read it. When the reset is accepted, the device **sends again**, into the new session, every message of this account that it sent in this conversation after the moment the reset was signed. The bubbles do not move or duplicate on the sender's side. |
| A call is started before the new session is ready | The call rings out and is not answered, as when the other side is unreachable. No special copy. |
| A call was live in the old session when the reset is accepted | It ends, the way a call ends when the other side drops (the media was going to a removed device). No special copy. The next call uses the new session. |

**Messages arriving in the old session** after the move (a removed device
still sending): dropped unread, never shown, never notified. They are from
keys that are no longer Anna's.

**On Anna's side** (her new device, which holds nothing): the conversation
arrives as a chat with the person, with no history, when her device is
added. That is section 15's subject, and it is what a first chat looks like.
Her device accepts it without asking, as it accepts any chat it is added to
today. (The alternative, a request she must accept, is recorded in 16.10.)

### 16.5 Groups

**A group Anna is a member or an admin of.** Her old devices are removed
and her new devices are added by the group's owner or admins, through the
ordinary membership requests, without a prompt. Until that has applied she
is simply not reading the group from her new device; the others notice
nothing. The timeline shows **no** row for it: the `Members changed` row is
suppressed for a change that only swaps the devices of a person who is still
a member. If she was an admin, the owner's device restores that silently
too.

When no owner or admin of the group is online, nothing happens until one is;
there is nothing to show while it waits.

**A group Anna owns.** Her old devices cannot be removed from it and it can
never get another owner, so the group continues in a **new** group behind
the **same** row, the way a one-to-one does: one of the remaining members
creates it, everyone who is still a member is added, Anna's new devices are
added, and she is made its owner again. Same name, same row, same history
above, same pins, same folder. Nobody is asked and nothing is announced.

States, all without copy:

| Moment | What a member sees |
|---|---|
| New group exists and this device has joined it | Nothing. Sends go to the new group. |
| New group exists, this device has not been added yet | Nothing. The composer stays live and sends go to the old group, where the members who already moved still read them. |
| Messages from members who have not moved yet | They appear in the timeline as usual. |
| Messages from Anna's old devices in the old group | Dropped unread. |
| A member the creator could not add | For that member the group keeps working in the old group; anyone already moved keeps adding them until it succeeds. Stated as a risk in 16.10, because until it succeeds their view and the others' differ. |

**A group with no roles at all.** Such groups are not created by any current
client. If one exists, nothing is done in it; recorded in 16.10.

### 16.6 A contact request from the same person

A person who reset has lost their contacts and may find the other person
again and send a contact request before the other person's device noticed
anything. The device does not show a request for someone who is already a
contact: it runs the check of 16.2 on the sender; when the reset is
accepted, it answers the request itself (the new session of 16.4 is the
answer), and no row appears under requests. When the check refuses, the
request is not shown either, and nothing is sent to the keys that sent it.
When there is no verdict yet, the request waits, unshown, for the next
check.

A `/i/` link from the same person is the same case reached from a link: the
link opens the existing contact.

### 16.7 Channels

Unchanged from the first version of this section, because nothing here was
ever shown to another person. A channel's publishing key derives from the
owner's account seed, so a reset orphans every channel the person owned. On
the device that reset the keys, each such channel shows `You can no longer
post here. Your account keys were reset.` with `Remove from this device`
(`channels.keys_reset.*`, built, kept). A subscriber has no signal and sees
a channel that receives no new posts. A channel is not moved to a successor:
its subscribers are not members of anything the device could move.

**Superseded for the owner (ADR-0101 section 7, amended 2026-11-26):** the
device that reset the keys takes each channel it owned back by itself,
from a list that the account's password and a second factor open together
(and that any device holding the account keys opens), and posts again; its
subscribers keep the channel and see new posts, with nothing shown to them
and nothing asked of them. `channels.keys_reset.*` remains only for a
channel that could not be taken back (not in the list, or its recovery key
was replaced). Subscriptions and publisher seats come back from contacts'
devices as ADR-0101 section 7 says.

### 16.8 Data the UI needs

The architecture is in the ADR; this is the list the screens depend on.

- **One contact before and after.** The contact record is found by account
  across the key change; rename, folders, pins, mute and the chat link stay.
  The old keys are kept on the contact only to name the author of messages
  already stored.
- **One conversation over several sessions.** A conversation keeps the id
  it had (the first session's), and everything keyed on a conversation
  (draft, pin / mute / archive, folder membership, read position, personal
  pins, call history, the name) stays keyed on it. The message log and the
  things that belong to one session (shared pins by message, delivery and
  read marks, push routing) are per session, and the timeline is the
  sessions in order. A device of the same person added later learns which
  session continues which conversation from the state its other devices
  hand it.
- **A held send.** A message typed while a one-to-one's new session does
  not exist yet is kept and sent when it does, in the ordinary sending
  state. The catalog has a sending and a failed state
  (`chat.message.sending`, `chat.send.failed`); whether each client has a
  queue that survives a restart was not checked (16.10).
- **The reset's moment** (from the server's signed statement), for the
  re-send of 16.4.
- **No per-contact state for the reset.** There is no `keyState`, no
  pending key, no observation stamp shown anywhere. What the device keeps
  is the last accepted generation per account, so an older statement is
  never accepted again.
- **The `Members changed` suppression** needs, at the moment a membership
  change lands, whether the set of **people** changed or only the devices of
  people who stay.

### 16.9 What is removed

Everything the first version of this section added to the screens, on all
three clients: the flagged contact state and its store; the contacts-list
glyph and line; the contact-screen banner; the disabled `Message` / `Call` /
`Video` and their helper line; the safety-number section's reset shape, the
checkbox, the warning sentence and the confirm button; the `Open contact`
links beside refusals; the disabled picker rows; the blocked `Approve`; the
bubble warning; the members-panel reset lines and the eviction system row;
the closed one-to-one (list line, terminal row, composer bar, `Start a new
chat`); the owned-group notice; `Recreate group` and its sheet; the
closed-group shape and `Open the new group`. The per-file inventory is in
the ADR.

Catalog keys to delete (43 of the 46 keys of the first version's 16.7, plus
two older ones), in `l10n/catalog/en.json`, every translation, and the three
embedded copies:

- `contacts.keys_reset.*` (8): `row_sub`, `banner`, `banner_unnamed`,
  `banner_action`, `verify_first`, `verify_first_short`, `open_contact`,
  `picker_sub`
- `contacts.safety.reset.*` (12): `title`, `body`, `body_unnamed`,
  `checkbox`, `checkbox_unnamed`, `warning`, `warning_unnamed`, `confirm`,
  `confirming`, `done`, `done_unnamed`, `save_failed`
- `chat.keys_reset.*` (10): `bubble_warning`, `member_being_removed`,
  `member_flag`, `system_removed`, `one_to_one_closed`, `one_to_one_row`,
  `one_to_one_composer`, `start_new_chat`, `owned_notice`,
  `owned_notice_member`
- `group.recreate.*` (8): `action`, `title`, `members_heading`,
  `excluded_reset`, `note`, `submit`, `working`, `done`
- `group.closed.*` (5): `system`, `composer`, `open_new`, `row`, `panel`
- `contacts.identity_changed.reset`, `contacts.identity_changed.reset_unnamed`

Kept: `channels.keys_reset.*` (3; the person's own reset, 16.7) and
`contacts.identity_changed.unexplained*` / `unconfirmed*` (16.1).

**Section 15 copy that is no longer true** and must change with the build
(both security-critical): `restore_identity.reset.effect_contacts` ("Your
contacts will see that your safety number changed and must verify you again
before messaging you securely.") and the same sentence on the new-phrase
screen (9.1 / 15.4). Proposed: "Your contacts keep their chats with you. Your
safety number changes." The other bullets of 15.1 stand. The messages
bullet changed with ADR-0101 (walk 2026-11-26): "Messages sent before today
cannot be read with the new keys" was no longer true once conversations and
their history come back from contacts' devices; it reads "Chats, groups and
channels come back from your contacts' devices, with the messages they
still have, newest first." **Superseded 2026-11-28** (15.1): with sealed and
unsealed resets (15.1c) both proposals overclaim for an unsealed reset; the
contacts bullet and the new-phrase line read only "Your safety number
changes." (`restore_identity.reset.effect_safety_number`,
`recovery_phrase.fresh.safety_note`), and the line of 15.1c alone says
whether the chats come back.

### 16.10 Cross-platform shape, and what is open

One model on the three clients: no surface, one acceptance rule, one
conversation over several sessions. There is no platform-specific UI in this
section because there is no UI in it. The three background readers (web
service worker, Android background decrypt, iOS notification extension)
treat a message in a conversation's new session as a message in that
conversation: same title, same grouping, same mute.

Open, stated plainly:

- **Nothing of this model is built or walked.** The visible flow is what is
  in the clients.
- **Anna's side accepts a re-established chat without asking** (16.4). The
  alternative is that it lands under her contact requests and she accepts
  each; then the other person's messages wait, unseen, until she does. The
  silent choice follows the maintainer's rule for the other person; whether
  it is also right for the person who reset is the maintainer's to confirm
  (ADR, decision D1).
- **A member who cannot be moved** to an owned group's successor (16.5)
  keeps writing in the old group while replies to them go to the new one,
  which they do not see. The retry closes it when it succeeds; nothing tells
  either side while it has not.
- **A held message that never goes out** (Anna never comes back) ends in the
  ordinary failed state. The person is not told why. That is the model.
- **Whether each client's send queue survives a restart** was not checked;
  the re-send of 16.4 and the held send depend on it.
- **The `unconfirmed` wording** (16.1) for "the server's statement did not
  check out": reused as written, to be reviewed as copy.
- **A group with no role table**: nothing is done.
- **Channels have no subscriber-side signal**, as before.
- **Per device.** Each device of the other person accepts the reset for
  itself; nothing about it travels between their devices except which
  session continues which conversation.

### 16.11 A contact started over: an unsealed reset (added 2026-11-26)

Status: designed, not built. Maintainer's decision of 2026-11-26 (ADR-0101
section 10). Rule 5 of 16.2 fails while rules 1 to 4 hold: the reset is
real at the server but carries no seal the account published before it.
The server alone can produce such a reset, and so can whoever holds the
password and the mailbox. The device therefore treats it as **a new account
under the same username**, and nothing moves:

- **The one-to-one** keeps its history and its place. Its composer is
  replaced by one line, "@name started over with a new account. Messages
  here are no longer delivered." (`contacts.started_over.closed`), because
  the account's old devices are gone and nothing sent there would arrive.
  The call buttons are hidden. No banner, no row in the chat list changes.
- **Groups** are not moved, nobody is evicted on this account's behalf, and
  no successor is founded. The old leaves stay until an owner or admin
  removes them by hand; messages they would have read go to devices that no
  longer exist.
- **Channels** the person subscribed to or published in are not returned.
- **No history, no profile, no channel seeds** are sent.
- **A contact request** from the new keys is shown as an ordinary request
  card (16.6's silent answer does not apply), with one more line under the
  name: "Lakko could not confirm this is the same person as before. Accept
  only if you are sure it is them." (`contacts.started_over.request_note`). Accepting it
  starts a new one-to-one with the new keys; the old one stays as above.
  Declining changes nothing.
- The safety number section of the contact (16.1) shows the old number for
  the old conversation and the new one for the new conversation.

**Decided 2026-11-26 (maintainer).** The closed line (in place of the
composer, which is removed) and the request note are shown. They are the
**only exception** to 16.1's rule that a contact's reset shows nothing: here
the device could not confirm it is the same person, which is the one thing
the rest of section 16 relies on. A sealed reset stays silent.

## 17. Proving it is you: reset, delete, add a device, and the new-device notice

Added 2026-10-06 (maintainer's decisions of that day). **Status: designed,
not built.** Nothing in this section exists in `services/auth` or in any
client except what 15.1a already built (the password field on the reset
screen and the password-minted grant). The catalog keys named here were added
to `l10n/catalog` the same day with machine translations; a key existing is
not a screen existing.

### 17.1 The one rule

A signed-in session says "this device was let in at some point". Three
actions hand over or destroy the whole account, so they need more: **the
person proves who they are now**, by typing the account password or by a
passkey gesture.

- The three actions: **Reset account keys** (15.1), **Delete account**
  (Settings > Account), and **Add a device** on the device that lets the new
  one in (section 3 "On the signed-in device", and the scan step of
  `second-device.md`).
- A proof lasts **ten minutes**. A password or passkey sign-in counts as a
  proof, so someone who signed in a moment ago is not asked again; so does a
  proof given for one of the three actions when the person moves on to
  another. Nothing on screen shows the ten minutes or counts them down. When
  the proof ran out while a screen was open, the field simply appears, with
  no error styling (nothing went wrong).
- What does **not** count: the authenticator code, three words of the phrase,
  another device's confirmation, a session however old or new, the phone's
  own screen lock.
- One exception to the ten minutes, kept from ADR-0014: deleting an account
  **that has a passkey** always asks for the passkey gesture for that
  deletion, whatever was proven before.
- **Amended 2026-11-26:** the reset is stricter than this rule. It needs the
  password (a passkey does not stand in for it) and then a second factor:
  the authenticator code, a code by email or a passkey (15.1b). Delete
  account and Add a device keep the rule as written. Setting up an
  authenticator app now needs this proof too (19.1b), because the
  authenticator is one of the reset's factors.

The proof block is one component, drawn the same in all three places:

```
Password
[ ............        (show) ]
Enter your password to <do the thing>.

Use a passkey instead
```

- `Password` is `auth.password.label`, the sign-in screen's secure field with
  its show / hide control (`auth.password.show` / `auth.password.hide`). Never
  pre-filled, never offered to the system's "save password" prompt, cleared
  when the surface closes.
- The helper names the action: `restore_identity.reset.password_help`,
  `proof.delete.help`, `proof.add_device.help`.
- `Use a passkey instead` (`proof.passkey.use`, a ghost button) is shown only
  when the account has at least one passkey **and** this device or browser
  can use passkeys. Otherwise it is absent, not disabled. It opens the
  platform's own passkey sheet, the one system surface allowed here; Lakko
  draws nothing over it and shows `recovery.passkey.waiting` behind it.
- When a proof is live the block is not drawn at all and the surface looks
  exactly as it did before this section.
- The block lives **inside the surface that already exists** (the reset
  panel, the delete dialog, the "Is this you?" modal). It is never a second
  screen, a second dialog or an operating-system prompt.

States the block shares, wherever it is drawn:

| State | What the person sees |
|---|---|
| Idle, proof live | No block. |
| Idle, no proof | The block. The surface's main button is disabled while the field is empty (unless a passkey route is offered, 17.4). |
| Checking | The main button shows its working word, the field and both buttons are disabled. |
| Wrong password | One line under the field in the danger colour, the field keeps focus and is selected. The line says what did **not** happen (per surface below). |
| Rate limited | `recovery.error.rate_limited` ("Too many attempts. Try again in {time}.") with the live countdown; the main button and the field stay disabled until zero; `Use a passkey instead` and every cancel / decline control stay usable. Without a retry time from the server: `common.error.too_many_attempts`. |
| Passkey sheet up | `recovery.passkey.waiting`. |
| Gesture cancelled | Back to idle, with one line saying nothing happened (per surface). No error styling. |
| Passkey of another account | `proof.passkey.other_account`. |
| Passkey check failed otherwise | `proof.passkey.failed`. |
| Offline | `common.error.offline`. The field keeps its contents. |
| Success | The surface continues with what it was doing; the person never sees a separate "verified" step. |

### 17.2 Reset account keys

**Amended 2026-11-26:** for the reset, the proof is the password **and** a
second factor (15.1b). In the table below, "Password or passkey sign-in"
reads "password sign-in", the passkey rows lose `Use a passkey instead` (a
passkey is one of the second factors of 15.1b instead), and the "cannot
reset" panel of an account with no usable factor is the one of 15.1b. The
rest of this subsection stands.

The screen of 15.1, with 15.1a as amended. In order, top to bottom: the
warning and its bullets, the proof block (when no proof is live), the typed
word, the buttons.

| How they got here | The screen |
|---|---|
| Password or passkey sign-in, or a proof, in the last ten minutes | 15.1, no block. |
| Anything else, account has a usable passkey | The block with `Use a passkey instead`. `Reset account keys` needs the typed word and either a non-empty password or a completed gesture. |
| Anything else, no usable passkey | The block without the passkey button, and under it the link `I do not know my password` (`proof.no_password_link`). |
| Signed in with the authenticator code in this session, no usable passkey, no proof | The "cannot reset" panel below is shown **first**, in place of the block, the word and the button; `I know my password` (`proof.know_password_link`) switches to the row above. |

The "cannot reset" panel replaces the block, the typed word and the `Reset
account keys` button; the warning above it stays, `Cancel` stays:

```
The account keys cannot be reset without your password or a passkey

An authenticator code signs you in, but it is not enough to reset
the account keys.

What you can do:
  - Sign in on a device that still has your keys, then add this
    device from it.
  - Enter your 12-word recovery phrase.
  - Use a passkey that restores keys.

Without one of these, the keys of this account cannot be restored
or reset.

I know my password                                         Cancel
```

Keys: `proof.reset.cannot.title`, `.body`, `.what_you_can_do`,
`.option_device`, `.option_phrase`, `.option_passkey`, `.otherwise`. The
three options are text, not buttons: each of them is a row of the screen this
panel sits on (section 8), one tap back through `Cancel`. There is no
disabled `Reset account keys` button anywhere in this state, and no sentence
suggesting support or a later way: there is none.

Per-surface lines: wrong password `restore_identity.reset.error.wrong_password`;
gesture cancelled `proof.reset.cancelled`; other failures as 15.1. A passkey
used here is one of the passkeys the reset then deletes; the warning's bullet
already says so.

### 17.3 Delete account

The existing `Delete your account?` dialog (`settings.account.delete.title`,
`.body`, danger `Delete account`, `Cancel`), drawn by the app. The block goes
between the body and the actions.

| Account | The dialog |
|---|---|
| Has a passkey | As today (ADR-0014): no field; under the body `You will be asked for your passkey.` (`proof.passkey.notice`); `Delete account` opens the passkey sheet for this deletion. Cancelling it: `settings.account.delete.error.cancelled`. A live proof does not skip this. |
| No passkey, proof live | As today: body and buttons. |
| No passkey, no proof | The block with `proof.delete.help`, no passkey button; `Delete account` disabled while the field is empty; under the field `I do not know my password`. |
| No passkey, chose `I do not know my password` | The field and the `Delete account` button are replaced by `proof.delete.cannot` and `I know my password`; `Cancel` stays. |

Wrong password: `proof.delete.wrong_password`, the dialog stays open. Rate
limit, offline: the shared rows. Any other refusal:
`settings.account.delete.error.generic`. After success the device is wiped
and lands on the sign-in screen, as today.

The design system's "typed confirmation" for Delete account is not added on
top: typing the password is the deliberate act, and a second typed word would
be ceremony. On a passkey account the gesture plays that part.

### 17.4 Add a device (the device that lets the new one in)

Two entry points, one rule.

**"Is this you?" (the code route).** The modal of section 3, step 1. The
block sits in the same step, under the question and above the buttons; there
is still one decision on one screen:

```
New sign-in request

Someone is signing in to your account on a new device.
Is this you?

Password
[ ............        (show) ]
Enter your password to add the device.

Use a passkey instead

[Yes, it's me]        It is not me
```

| Case | Step 1 |
|---|---|
| Proof live | As built: the question and the two buttons. |
| No proof, no usable passkey | The block, no passkey button. `Yes, it's me` is disabled while the field is empty. |
| No proof, account has a usable passkey | No field. `You will be asked for your passkey.` (`proof.passkey.notice`) under the question; `Yes, it's me` opens the passkey sheet; `Use your password instead` (`proof.password.use`) swaps the notice for the field (the passkey may live on another device). |

Order of events on **Yes, it's me**: the proof is checked first; only when it
passes does this device choose the code, answer the request and show step 2.
So the rule of section 3 holds unchanged: **nothing is sent for the request
until the person both said yes and proved it.** `It is not me` never needs a
proof and is enabled in every state, including rate-limited and offline.

| State | What the person sees | The request |
|---|---|---|
| Wrong password | `proof.add_device.request.wrong_password` under the field; step 1 stays. | Still pending, unanswered. |
| Gesture cancelled | `proof.add_device.request.cancelled`; step 1 stays. | Still pending. |
| Rate limited | The shared line; `Yes, it's me` disabled; `It is not me` works. | Still pending until it expires. |
| Offline | `common.error.offline`. | Still pending. |
| The request expired while the person was typing | The modal closes with the toast `device_link.request.expired`. | Gone; the new device shows its own "expired". |
| Closed without choosing (backdrop, Escape, back) | Nothing; as built. The typed password is discarded. | Left to expire. |
| Success | Step 2 (the code), then as section 3. | Answered. |

A wrong password here spends one attempt of the account's password budget
and nothing of the request: the one-try-per-request rule of section 3 is
about the six-digit code, and no code exists yet.

**Scanning the new device's code** (`Settings > Devices > Add a device >
Scan a code`). The existing `Link this device?` step
(`devices.scan.confirm.*`) gets the same block between its body and `Link
device`, with the same three cases. Lines: `proof.add_device.help`,
`proof.add_device.scan.wrong_password`, `proof.add_device.scan.cancelled`.
A failure leaves the step open with the scanned code kept, so the person
does not scan again. The decision named the confirmation prompt; the scan
route hands over exactly the same thing, so leaving it out would leave the
door the rule closes.

The new device's side (section 3 "On the new device") does not change: it
keeps showing "Waiting for your other device..." through all of the above.

### 17.5 A device was added to your account

Shown on **every other device of the same account** after a device joins it,
by any route (code, scan, a restore that re-adds a device). This is the
person's own account security; the rule against surfacing key events
(section 16) is about other people's keys and does not apply.

Amended 2026-10-29 (maintainer): the warning is a system notification, a
badge on Settings and a highlighted row in Settings > Devices, and seeing it
on one device clears it on all of them. The banner card at the top of the
Chats list is removed.

**Pending.** A device is *pending* on this device when it is in the account's
DeviceGroup roster, this device has not treated it as known (the rules
below), and no device of the account has acknowledged it yet (the synced
register of 17.6). While at least one device is pending, every surface below
is up; when none is, every surface is gone.

**Surfaces while pending.**

1. **System notification.** Posted once per pending device, at the moment
   this device learns of it: when Lakko is running, in front or not, and at
   the next open when it learns of the device then. Title
   `devices.added_notice.title` (`.title_many` when two or more are pending),
   body `devices.added_notice.notification_body`. It names no device and no
   place. Tapping it opens Settings > Devices. It uses a fixed identifier per
   account, so a second device replaces the first notification instead of
   stacking. Withdrawn from the notification centre / shade / browser as soon
   as the pending set becomes empty, whatever device the acknowledgement came
   from. Web: only when the person already allowed notifications (never asked
   for here); iOS and Android: through the app's existing notification
   permission (never asked for here either). A closed app is not woken
   (17.8): the notification is posted at the next open.
2. **Badge on Settings.** The Settings destination of the navigation (tab bar
   on phones, sidebar / rail on wider layouts) carries an exclamation-mark
   badge, and inside Settings the `Devices` row carries the same badge;
   where Settings shows its section list beside the open section (web on a
   wide screen), the section that holds `Devices` carries it too
   (`settings.security.attention_a11y`). The
   badge is drawn in the design system's warning colour (yellow; the
   maintainer, 2026-10-29: it asks to look, it reports no failure), the
   text on it in the colour used on the accent, and it is never colour
   alone: its accessible name is `nav.settings.attention_a11y` on the
   destination and `settings.devices.attention_a11y` on the row.
3. **Highlighted row.** In Settings > Devices each pending device's row has an
   accent-tinted background and the `New` label
   (`settings.devices.new_badge`, text, so not colour alone), and the first
   such row is scrolled into view. Above the list, the explanation:

```
(device icon)  A device was added to your account
               Added 6 Oct 2026, 14:05.
               If this was not you, remove the device, then change
               your password.
```

   Keys `devices.added_notice.title` (or `.title_many`), `.when` (the most
   recent join time), `.not_you`. No button: the screen it would open is this
   one, and opening it is the acknowledgement.

**What counts as seen.** Opening Settings > Devices while a device is pending
(directly, from the badge, or by tapping the notification). At that moment
this device writes an acknowledgement for **every device pending here** and
sends it to the account's other devices. On this device the highlight and
the explanation stay for the rest of this visit to the screen, so the person
actually sees which row is new; leaving the screen clears them. The badge
and the notification go at once. A device that becomes pending while Settings > Devices is
open and in front is acknowledged at once and joins the highlight. If the
synced state is not ready yet (the DeviceGroup has not opened), opening the
screen consumes nothing: the rows stay pending and highlighted and the badge
stays, and the acknowledgement is written the moment the state becomes ready
if the screen is still open. Removing a pending device also ends its
warning everywhere (it left the roster). There is no `Dismiss`: looking at
the list is the decision the person can take, and the list is where the
`Remove` button is.

**On every other device.** When an acknowledgement arrives, the
acknowledged devices stop being pending there: the notification is
withdrawn, the badge goes, and if Settings > Devices is open the highlight
goes with it. A device that learns of a new device only after it was already
acknowledged elsewhere never shows anything about it.

**Not on the device that was just added.** A device never shows the warning
about itself, and it treats every device already in the account when it
joined as known.

**Not on the device that let it in.** That device showed "Is this you?" or
`Link this device?`, took the proof, and showed `Device added.`; a second
alert about the same act a moment later teaches people to dismiss without
reading. It does not acknowledge the device for the account either: the
other devices still warn (the cost is stated in 17.8).

**Removed with change of password.** Removing a row that carries `New` shows,
above the list: `Device removed. If you did not add it, change your password
now.` (`settings.devices.removed_change_password`) with a `Change password`
button (`settings.password.change`), unchanged.

**Accessibility.** The badge's accessible name is announced as part of the
destination's or row's name; it is not a live region. The explanation above
the list is read as the first element of the screen, title and both lines as
one announcement; it never takes focus by itself. The highlighted rows
expose the `New` label in their accessible name.

### 17.6 Data the UI needs

From the server (none of it exists except the password-minted grant of
15.1a):

- A proof that a **passkey sign-in** and a **passkey gesture made while
  signed in** mint as well as a password sign-in, accepted for ten minutes by
  the three actions, and **reusable within the ten minutes across them**
  (15.1a's grant is one use: a reset spends it by changing the keys). Whether
  this is one credential or one per action is the ADR's call; the UI needs
  only "is a proof live, and until when".
- The account deletion refuses without a proof when the account has no
  passkey; the passkey case stays as ADR-0014 built it.
- Both add-device routes refuse without a proof on the letting-in device:
  answering a request, and starting the hand-over after a scan.
- For each, a refusal the client can tell from every other failure ("proof
  required"), so a proof that expired mid-screen shows the field and not an
  error; `429` with a retry time shared with the sign-in budget.
- How this session was landed (password, passkey, authenticator code, other),
  kept by the client for the session: it decides which row of 17.2 opens
  first.
- Whether the account has a passkey, known before the block is drawn (the
  passkey list the settings screen already loads).

Kept by the client:

- The proof and its expiry, in memory only; gone on sign-out and on restart
  (a restart costs at most one more password entry).
- Per device: the devices this device already treats as known, the devices
  this device itself let in, and the pending devices with the time this
  device learned of each (the DeviceGroup carries no join date).
- Per account, synced between the account's devices: the set of devices
  whose warning was seen (the `lakko-sync` device-acknowledgement register,
  one register per device signature key, write-once, carried as a
  DeviceGroup service envelope `lakko/sync/device-ack/v1` and persisted with
  the rest of the synced state, so the backup carries it). A device pending
  locally whose key is in this set is not pending.
- The join time per device (the device list has no added-on date on web
  today; `Devices.tsx` says so).

### 17.7 Cross-platform shape

- The block is the same fields in the same order on web, iOS and Android,
  inside the surface each platform already draws: the reset panel, the app's
  own confirm dialog (web `ConfirmDialog`, the app-drawn sheets on iOS and
  Android), the "Is this you?" modal / sheet. No `alert`, no
  `.confirmationDialog`, no system `AlertDialog` gains a text field for this;
  where a client still uses a stock dialog for Delete account, this change
  is where it moves to the app's own.
- The passkey sheet is the platform's and differs; everything around it does
  not.
- Web: the proof is per tab (memory only). iOS / Android: the proof does not
  survive the process. In every case a missing proof costs one field.
- iOS and Android: when "Is this you?" arrives by notification, tapping it
  opens the same sheet with the block; the password is never typed into a
  notification action.
- The secure field uses the platform's password content type so a password
  manager can fill it; the passkey button is a separate control, never the
  field's accessory.
- Right-to-left: the block mirrors; the password field's contents stay LTR.

Accessibility, beyond 17.5: the helper is the field's description, the error
line is announced when it appears (`common.error.a11y` wraps it) and focus
stays in the field; the main button's disabled state is explained by the
helper, not by colour; the countdown is announced once, not every second;
`It is not me` is reachable in every state without passing through a
disabled control.

### 17.8 Deliberately not built, and open

- **Changing the password on a device that holds the keys asks for nothing
  today** (10.3: "you only type the new password"). Whoever holds the
  unlocked phone can set a password and then pass every check in this
  section. The rule is hollow until that screen asks for a proof too (the old
  password, a passkey, or the local three-word check of section 6). Not
  designed here; **needs a maintainer decision before this section is
  built.**
- **Adding a passkey asks for nothing today.** A session could add a passkey
  and then use it as the proof. Same shape, same need for a decision (ask for
  a proof to add one, or do not accept a passkey younger than the session).
- A passkey account whose passkeys are all out of reach cannot delete the
  account with the password alone (17.3 keeps ADR-0014 as is). Open.
- The device that let the new one in shows no banner. If the ten-minute
  proof was still live from a sign-in, someone holding that unlocked device
  could add a device with no prompt and no trace on it; the other devices
  still show the banner. Narrowing the ten minutes for Add a device, or
  showing the banner there when no proof was typed in the step itself, is
  the fix if this residual is not accepted.
- No wake of a closed app for the new-device notice (it would need the
  server to learn "this account's device list changed" as a push reason).
- No device name, model or location in the notice or the prompt: the account
  holds none.
- No list of past proofs, no "trusted for 30 days", no way to lengthen or
  switch off the ten minutes.
- No way through for a person with only the authenticator code. That is the
  decision, and the screen says so.
- Strings: 32 keys added 2026-10-06 (`proof.*`, `devices.added_notice.*`,
  `device_link.request.expired`, `settings.devices.new_badge`,
  `settings.devices.removed_change_password`), machine-translated into the
  ten languages; 22 are security-critical and render in English in a
  translation until reviewed (ADR-0061).
- Nothing here was built, rendered or walked.

## 18. A device that should not have the account: remove it, change the keys, take the account back

Added 2026-10-07 (maintainer's decisions of that day). **Status: designed,
not built.** Nothing in this section exists in any service or client.
Architecture: [ADR-0074](../adr/0074-account-takeover-resistance.md). The
catalog was **not** edited; the new keys are listed in 18.6 with their
English text.

Three things a person may need, and the one screen each starts from:

| The person's situation | Where | What it does |
|---|---|---|
| "I do not use that device any more" / "I lost it" | Settings > Devices, on a device that has the keys | Removes the device: signs it out at once, deletes the account's data on it at its next contact, keeps it out. |
| "That device was stolen" | The same row, second choice | Removes it **and** changes the account keys. The other devices keep everything. |
| "My account keys were reset and it was not me" | The old device, by itself | Takes the account back with the password from before. |

Contacts see nothing for any of these: no row, no banner, no prompt, no
changed name. The safety-number screen shows the current number, as always.

### 18.1 Settings > Devices: remove a device

The Devices list of `second-device.md` section 3. Each other device's row
has **Remove**. It opens the app's own sheet (never a system dialog):

```
Remove this device?

It is signed out now. The next time it connects, everything this
account kept on it is deleted: chats, contacts, keys and the
recovery phrase.

( ) I do not use it any more
( ) It was lost or stolen

[proof block, when no proof is live: section 17.1]

[Cancel]                                   [Remove device]
```

- The two choices are a radio pair, neither selected at first; the main
  button is disabled until one is.
- **I do not use it any more** keeps the button `Remove device`.
- **It was lost or stolen** changes the sheet in place:

```
Remove this device and change your account keys

A lost device still holds your account keys. Changing them makes
what it holds useless.

- Your other devices keep their chats and get the new keys by
  themselves.
- You get a new recovery phrase. The old one stops working.
- Your passkeys still sign you in. To restore keys with one again,
  choose "Restore keys with this passkey" on its row.
- The people you talk to do not have to do anything.

[proof block]

[Cancel]                          [Remove and change keys]
```

- The proof block is section 17.1's, with helper
  `proof.remove_device.help`. A device that holds no keys never reaches
  this sheet: the Devices list is not shown on it (it shows "Restore your
  keys", section 8).

States of the sheet:

| State | What the person sees |
|---|---|
| Idle | As drawn. |
| Working (remove) | Button reads `Removing...`; everything disabled. |
| Working (remove and change keys) | A step list replaces the bullets, one line active at a time: `Removing the device` / `Changing your account keys` / `Updating your other devices`. No percentage. Cancel is disabled from the first step. |
| Done (remove) | The sheet closes; the row is gone; one line at the top of the list for that session: `devices.remove.done`. |
| Done (remove and change keys) | The new recovery phrase screen of section 9.1, unchanged, then back to Devices with `devices.rotate.done`. |
| Sign-out could not be confirmed | `devices.remove.error.not_revoked`: nothing was removed, the row stays. Retry is the same button. |
| Signed out, removal still finishing | The row stays with a `Removing...` label and no Remove control; it finishes by itself the next time this device is online. No error. |
| Keys could not be changed (before the change took effect) | `devices.rotate.error.not_started`: the device **was** removed, the keys are unchanged; the sheet offers `Try again` and `Not now`. |
| Keys changed, publishing still owed | The existing "publish owed" state of 15.4: the words are withheld with 15.4's copy, retried by itself. |
| Wrong password / rate limited / offline / passkey states | Section 17.1's table. Wrong-password line: `proof.remove_device.wrong_password`. |
| This device has no account key recorded yet | `devices.remove.error.try_later` (a state of an account created before this was built; not expected after the flag day). |

What changed from today's sheet: `settings.devices.remove.body` ("You may
be asked for your passkey") is replaced by the text above; the passkey
gesture, when the account has one, is the proof block's.

### 18.2 The removed device

Replaces `second-device.md` 3.1's screen. Same full-screen shape, no
dismiss, no chat list behind it, at launch too. The deletion **starts by
itself**; there is no Sign out button to tap first.

```
This device was removed

It was removed from the account @name on another device and
signed out.

Everything this account kept on this device is being deleted:
its chats, contacts, keys and recovery phrase.

[progress]  Removing...
```

then:

```
This device was removed

It was removed from the account @name on another device and
signed out. Everything the account kept here was deleted.

[Sign in]
```

- Shown when the server refuses the device as removed, or when the device
  reads its own removal from its devices' shared state. Never on a failed
  request alone.
- No "Was it you?" here and no way back: a removal changes no keys, so
  there is nothing to take back. If the person did not remove it, they sign
  in with their password and are a device without keys (section 8).
- Accessibility as 15.2: one announcement for title and body, `Removing`
  announced once, the single button the only focusable element.

**Log out** (Settings > Account) keeps its sheet and copy. What changes is
not visible: the device is signed out at the server too, so a copy of what
it held cannot keep using the account. Offline, the device logs out locally
and tells the server the next time the app opens; nothing is shown for
that.

### 18.3 The other devices after "remove and change keys"

A kept device changes its keys by itself the next time it is online. The
person sees the app as it was, with every chat, and one dismissible line at
the top of the chat list:

```
Your account keys were changed on another device. This device has
the new ones.                                  [Show new phrase]
```

- `Show new phrase` opens Settings > Security > Recovery, which asks for
  the password as it always does.
- While the device is fetching the new keys (it was told its session is
  out of date and has not yet read them): the chat list stays, the header
  shows `Updating keys...` where "Synchronizing..." would be, sends wait.
  No full-screen state.
- If the new keys never reach it (it was offline for longer than they are
  kept, about a month): it shows 18.4, whose **Yes** leads to signing in
  and getting the keys from another device.

### 18.4 The old device after a reset: "Was it you?"

Replaces 15.2's first screen. A device that learns the account keys were
reset elsewhere deletes the account's chats, contacts and conversation
data **at once**, as before, and then asks:

```
Your account keys were reset

The keys of @name were reset on another device on 7 October.
This device was signed out, and the chats and contacts it kept
were deleted.

Was it you?

[Yes, it was me]
[No, take the account back]

You can take the account back from this device until 6 November.
```

- Both buttons are full-width, stacked, neither styled as destructive;
  **No, take the account back** is the primary.
- The date line is drawn only when the device has confirmed with the server
  that taking back is possible. While it is checking: the buttons are shown
  with **No** disabled and `devices.recall.checking` under them. Offline:
  both shown, **No** disabled, `common.error.offline`, re-checked when the
  connection returns.
- When taking back is not possible (the time has passed, or this device was
  never able to): the screen is 15.2 as built (the deletion finishes by
  itself, then `Sign in`). Nothing says why.
- **Yes, it was me**: one confirmation inside the same screen, not a
  dialog: `devices.recall.yes.confirm` with `Delete and sign in` /
  `Go back`. Then 15.2's "Removing..." and `Sign in`.

**No, take the account back** opens the form on the same screen:

```
Take the account back

Enter the password this account had before the reset, and choose a
new one. Whoever reset the keys will be signed out.

Password before the reset
[ ............            (show) ]

New password
[ ............            (show) ]
Repeat new password
[ ............            (show) ]

- The account gets new keys and a new recovery phrase.
- Passkeys and the authenticator code are removed. Add them again
  afterwards.
- Messages sent to the account since the reset were not sent to
  you and cannot be recovered.

[Back]                              [Take the account back]
```

| State | What the person sees |
|---|---|
| Idle | Main button disabled until the three fields are filled, the new password passes the signup rule and the two match. |
| Working | `Taking the account back...`; fields and buttons disabled. |
| Wrong password | `devices.recall.error.wrong_password` under the first field, which keeps focus and is selected. If the server said how many tries are left below five: `devices.recall.error.tries_left`. |
| Changed your own password recently | Not detectable; the wrong-password line carries the hint (`...If you changed your password in the last month, enter the one you had before that.`). |
| Rate limited | `recovery.error.rate_limited` with the countdown; Back stays usable. |
| No more tries | `devices.recall.error.exhausted`; the screen becomes 15.2 (deletion finishes, `Sign in`). |
| Not possible any more (the time passed while the form was open) | `devices.recall.error.closed`; then 15.2. |
| Offline | `common.error.offline`; the fields keep their contents. |
| Done | The new recovery phrase screen of section 9.1 (acknowledge-gated, withheld while the publish is owed as in 15.4), then the app, signed in, with no chats yet. One line at the top of the chat list for that session: `devices.recall.done`. |

- The three password fields are secure fields with show / hide; none is
  pre-filled; the first uses the platform's "current password" content
  type so a password manager offers the saved one, the other two "new
  password".
- No passkey route here: the passkeys on the account may be the other
  party's, and the rule is the password from before.
- After the account was taken back, the device that had reset the keys is
  an old device in its turn. It has nothing older to take the account back
  with, so it shows 15.2 as built: the deletion finishes by itself, then
  `Sign in`.

### 18.5 What nobody else sees

- A contact: nothing, for a removal, a key change with the devices kept, a
  reset, or an account taken back. Conversations stay in their rows.
- The person's own other devices after a plain removal: the row disappears
  from Devices. No banner.
- After the account was taken back, the device that had reset it shows
  15.2 / 18.4 like any other old device.

### 18.6 Copy (new keys; not in the catalog yet)

`sc` marks a security-critical candidate (ADR-0061: it renders in English
in a translation until a native reviewer has passed it).

| Key | English | sc |
|---|---|---|
| `settings.devices.remove.title` | Remove this device? | |
| `settings.devices.remove.body` (replaces the existing text) | It is signed out now. The next time it connects, everything this account kept on it is deleted: chats, contacts, keys and the recovery phrase. | sc |
| `settings.devices.remove.reason_unused` | I do not use it any more | |
| `settings.devices.remove.reason_stolen` | It was lost or stolen | |
| `settings.devices.remove.confirm` | Remove device | |
| `settings.devices.remove.working` | Removing... | |
| `devices.rotate.title` | Remove this device and change your account keys | sc |
| `devices.rotate.lede` | A lost device still holds your account keys. Changing them makes what it holds useless. | sc |
| `devices.rotate.effect_devices` | Your other devices keep their chats and get the new keys by themselves. | sc |
| `devices.rotate.effect_phrase` | You get a new recovery phrase. The old one stops working. | sc |
| `devices.rotate.effect_passkeys` | Your passkeys still sign you in. To restore keys with one again, choose "Restore keys with this passkey" on its row. | sc |
| `devices.rotate.effect_contacts` | The people you talk to do not have to do anything. | sc |
| `devices.rotate.confirm` | Remove and change keys | |
| `devices.rotate.step_remove` | Removing the device | |
| `devices.rotate.step_keys` | Changing your account keys | |
| `devices.rotate.step_devices` | Updating your other devices | |
| `devices.rotate.done` | Your account keys were changed. Your other devices get the new ones when they next connect. | sc |
| `devices.rotate.error.not_started` | The device was removed, but your account keys were not changed. | sc |
| `devices.rotate.retry` | Try again | |
| `devices.rotate.not_now` | Not now | |
| `devices.remove.done` | Device removed and signed out. | sc |
| `devices.remove.pending` | Removing... | |
| `devices.remove.error.not_revoked` | The device could not be signed out, so nothing was removed. Try again. | sc |
| `devices.remove.error.try_later` | This device is still setting up the account. Try again in a minute. | |
| `proof.remove_device.help` | Enter your password to remove this device. | sc |
| `proof.remove_device.wrong_password` | That password is not right. The device was not removed. | sc |
| `devices.removed.title` (existing) | This device was removed | sc |
| `devices.removed.body_removing` | It was removed from the account {name} on another device and signed out. Everything this account kept on this device is being deleted: its chats, contacts, keys and recovery phrase. | sc |
| `devices.removed.body_done` | It was removed from the account {name} on another device and signed out. Everything the account kept here was deleted. | sc |
| `devices.rotated.notice` | Your account keys were changed on another device. This device has the new ones. | sc |
| `devices.rotated.show_phrase` | Show new phrase | |
| `devices.rotated.updating` | Updating keys... | |
| `devices.recall.body` | The keys of {name} were reset on another device on {date}. This device was signed out, and the chats and contacts it kept were deleted. | sc |
| `devices.recall.question` | Was it you? | sc |
| `devices.recall.yes` | Yes, it was me | sc |
| `devices.recall.no` | No, take the account back | sc |
| `devices.recall.until` | You can take the account back from this device until {date}. | sc |
| `devices.recall.checking` | Checking whether the account can be taken back... | |
| `devices.recall.yes.confirm` | The rest of what this account kept on this device will be deleted. To use the account here again, sign in and get the keys from the device that reset them. | sc |
| `devices.recall.yes.delete` | Delete and sign in | |
| `devices.recall.yes.back` | Go back | |
| `devices.recall.form.title` | Take the account back | sc |
| `devices.recall.form.lede` | Enter the password this account had before the reset, and choose a new one. Whoever reset the keys will be signed out. | sc |
| `devices.recall.form.old_password` | Password before the reset | sc |
| `devices.recall.form.new_password` | New password | |
| `devices.recall.form.repeat_password` | Repeat new password | |
| `devices.recall.form.effect_keys` | The account gets new keys and a new recovery phrase. | sc |
| `devices.recall.form.effect_factors` | Passkeys and the authenticator code are removed. Add them again afterwards. | sc |
| `devices.recall.form.effect_messages` | Messages sent to the account since the reset were not sent to you and cannot be recovered. | sc |
| `devices.recall.form.submit` | Take the account back | sc |
| `devices.recall.form.working` | Taking the account back... | |
| `devices.recall.error.wrong_password` | That is not the password this account had before the reset. If you changed your password in the last month, enter the one you had before that. | sc |
| `devices.recall.error.tries_left` | {count, plural, one {# try left} other {# tries left}} | sc |
| `devices.recall.error.exhausted` | Too many wrong passwords. The account can no longer be taken back from this device. | sc |
| `devices.recall.error.closed` | The account can no longer be taken back from this device. | sc |
| `devices.recall.done` | You took your account back. Add your passkeys and your other devices again. | sc |
| `settings.account.delete.blocked` | The account keys were reset recently. The account cannot be deleted until {date}. | sc |

Reused unchanged: `devices.signed_out.*` (15.2), `proof.*` and
`recovery.error.rate_limited` (17), `common.error.offline`,
`settings.account.log_out.*`, the section 9.1 phrase screen.

Retired when this is built: `devices.removed.body`, `devices.removed.sign_out`
(the deletion no longer waits for a tap).

### 18.7 Data the UI needs

From the server (none of it exists; routes in ADR-0074):

- Removing: an answer that tells "signed out at the server" from every
  failure, before anything else is attempted; a refusal that means "proof
  required" (17.6); a refusal that means "this device cannot remove
  devices" (it holds no keys).
- On the removed device: a refusal that means **removed**, distinct from
  "keys were reset" and from "session expired". Each of the three leads to
  a different screen and a different deletion (all / staged / none).
- For "Was it you?": whether this device can take the account back, until
  when, and when the keys were reset; the answer must be unobtainable
  without this device.
- For the form: wrong password as its own answer, with the number of tries
  left when it is below five; "no longer possible"; the retry time when
  rate limited.
- For a kept device: that its session is out of date **because the keys
  were changed with the devices kept** is not something the server tells
  it; it learns it from its own devices' shared state. Until it has, it
  shows `Updating keys...`, never a full-screen state.
- Account deletion: a refusal carrying the date until which it is blocked.

Kept by the client:

- On the old device, for the time taking back is possible: the account
  name, the date it learned of the reset, the date until which it can act,
  and what it needs to prove it is a device of the account. Nothing else of
  the account.
- On the removing device: which step of a removal or of a key change is
  still owed, so a closed app finishes it at the next launch.
- On a kept device: whether the "keys were changed" line was dismissed.

### 18.8 Cross-platform shape

- One model on web, iOS and Android: the same sheet, the same two
  full-screen states, the same form, the same order of fields.
- Every surface is the app's own: the remove sheet replaces any stock
  confirm (web `ConfirmDialog`, the app-drawn sheets on iOS and Android);
  the "Yes" confirmation is drawn inside the screen.
- The removed and "Was it you?" screens replace the whole app on every
  platform, including on launch and when opened from a notification; a
  notification that arrives for an account in either state is not shown.
- Web: a second tab of the same account shows the same state without a
  reload. iOS: the notification extension stops producing previews for the
  account as soon as either state is entered. Android: the same for the
  background decrypt.
- Dates use the platform's medium date in the interface language. `{name}`
  is the `@username`.
- Right-to-left: the sheet and the form mirror; password fields stay LTR.
- Accessibility: the radio pair is one group with a label; the step list
  announces each step once when it becomes active; on "Was it you?" the
  title, body and question are one announcement and focus lands on **No,
  take the account back**; error lines are announced when they appear and
  focus stays in the field; the tries-left line is announced once per
  change.

### 18.9 Deliberately not built, and open

- **A device without keys cannot remove a device.** It can reset the keys
  (15.1), which the old devices can answer with 18.4.
- **Taking back needs an old device that is opened within about a month.**
  With no device left, a reset by someone who learned the password stands.
- **What was sent to the account between the reset and the take-back is
  gone for the owner and was read by the other party.** The form says so.
- **A removal alone does not change the account keys.** The sheet's second
  choice exists to say this at the moment it matters; there is no warning
  on the first.
- **After "remove and change keys", conversations keep working unchanged,
  and a device added later joins them as before.** Moving every
  conversation onto the new keys is a later build (ADR-0074 section 3.6)
  with nothing to show.
- **Account deletion is blocked for about a month after a reset**, with one
  line and a date. Open: the maintainer may prefer to allow it (ADR-0074
  D3).
- **The password asked for is the one from before the first change**, which
  for a person who changed their own password in the last month is the
  older one. The error line says it. Open (ADR-0074 D2).
- No list of "devices that were removed", no undo of a removal, no email or
  push about any of this to the person or to anyone else.
- Whether the take-back form should also offer the old device's own screen
  lock as an extra step was not designed.
- Strings: 57 keys listed in 18.6 (55 new, one existing text replaced, one
  existing key reused as is), 37 marked security-critical, none added to
  the catalog. Nothing here was built, rendered or walked.

## 19. An account without a saved phrase, a recovery email, and taking the account back with the phrase

Added 2026-10-12 (maintainer's decisions of that day). **Status: designed,
not built.** Nothing in this section exists in `services/auth` or in any
client. Architecture: ADR-0075. Where this section and sections 1 to 10
disagree about sign-up or the phrase, this section is the newer decision;
the older sections are reconciled when it is built.

What was decided:

- The 12 words are **not shown at sign-up**. A person may save them later.
- A person may add one email address. The server keeps only a one-way value
  made from the **username and the address together**, never the address.
- ~~**While no phrase is saved**, a code mailed to that address brings the
  person all the way back: a new password and, on any device, their keys,
  contacts and chats. The app says plainly that this is only as safe as the
  mailbox.~~ **Withdrawn 2026-11-26 (maintainer):** the email vault is
  removed (ADR-0075, amended that day). A mailed code never brings keys.
- **A mailed code gives a new password only**, for every account, and serves
  as a second way to confirm it is you for a key reset (15.1b). (Until
  2026-11-26 this held only once a phrase was saved.)
- A person who holds the phrase can **take the account back** for about a
  month after someone reset its keys.
- Adding an email can be skipped, with a warning. **Superseded 2026-11-26:**
  one of an email, an authenticator app or a passkey is required at
  sign-up; the phrase is an optional extra and does not count (19.1a).
- When the account has an authenticator, its code is asked for as well.

### 19.1 One kind of account (rewritten 2026-11-26)

Until 2026-11-26 this table set an account without a saved phrase (whose
mailed code brought the keys back) against one with a saved phrase. With the
email vault removed (ADR-0075, amended 2026-11-26) there is one kind:

| | Every account |
|---|---|
| A mailed code gives | A new password, and confirms a key reset (15.1b). Never the keys. |
| On a new device after the code | **Restore your keys** (section 8) |
| What brings the keys back | Another device, a passkey that restores keys, the recovery phrase |
| What brings the chats back after a reset | A reset the account sealed in advance (15.1c): with the current password, a passkey that keeps the chats, or the phrase |
| Settings > Account status line | 19.1c |
| Three-word row (section 2) | Present on a device that holds the account, once the phrase was saved |

Past messages come back from another device, or from contacts after a sealed
reset (ADR-0101). No screen promises more.

### 19.1a Sign-up

**Rewritten 2026-11-26 (maintainer's decision; designed, not built).** A
second way to confirm it is you is **required** at sign-up: a recovery
email, an authenticator app or a passkey. It is what a key reset asks for
after the password (15.1b), and since ADR-0101 a reset brings the chats,
groups, channels and recent history back. The recovery phrase stays on the
step as an optional extra; it is **not** one of the three, because a person
who later loses it is left with nothing. There is no `Not now`, no "Continue
without protection?" and no banner. The earlier text of this subsection
(2026-10-12) is replaced by what follows.

Username and password, then one step:

```
Protect your account
If you forget your password or lose this device, this is how
you get back in. Choose at least one.

  [mail]  Add an email                 Recommended     >
          We send a code to it. Anyone who can read that
          mailbox can get into your account.
  [authenticator] Use an authenticator app            >
          A 6-digit code from an app on your phone. If
          the app is only on this device, you lose it
          with the device.
  [passkey] Create a passkey                           >
          Sign in with your fingerprint, face or screen
          lock. No password to remember.

Optional
  [key]   Also save a recovery phrase                  >
          12 words only you keep. The strongest way back.

[ Continue ]
```

- **Rows.** `Add an email` is shown when mail is set up on this server and
  the account has a username (as before). `Use an authenticator app` is
  **always** shown: the server refuses to start without the authenticator
  configured (ADR-0073, amended 2026-11-26), so every sign-up has at least
  this row. `Create a passkey` is shown where this device or browser can use
  passkeys and the account has a username. Order fixed: email,
  authenticator, passkey, then the optional phrase under the caption
  `Optional` (`protect.optional`).
- **`Continue`** is the only way off the step. It is disabled until at least
  one of the first three rows is marked `Done`; a saved phrase alone does not
  enable it. Disabled means drawn in the disabled style with the line under
  it "Choose one of the first three to continue." (`protect.continue_needs`),
  never hidden. Once enabled it goes to the app.
- **The authenticator alone** (maintainer, 2026-11-26) is allowed, with a
  warning. When the authenticator is the only one of the three marked
  `Done`, a note in the `--warning` colour token (not the danger colour),
  with the warning icon, stands between the rows and
  `Continue`:

  > If you lose your authenticator app, you will not be able to get this
  > account back. Add an email or a passkey as well.

  (`protect.totp_only.warning`). When a phrase was also saved on the step,
  it reads instead: "If you lose both your authenticator app and your
  recovery phrase, you will not be able to get this account back."
  (`protect.totp_only.warning_phrase`). `Continue` stays enabled; the note
  goes as soon as an email or a passkey is added. It is announced once by
  screen readers when it appears.
- **Rows done.** Each of the three runs its flow (email 19.2, authenticator
  19.1b, passkey the existing creation) and returns here with the row marked
  `Done`; the person may add another before `Continue`. A row marked `Done`
  can be opened again only from Settings.
- **The phrase.** `Also save a recovery phrase` runs 19.5 and returns here
  with the row marked `Done`; it no longer ends the step by itself (it did
  before 2026-11-26). Since the email vault was removed (2026-11-26) the
  order no longer matters: an email never holds the keys.
- **The honest notice** of 19.1d stands under the rows, below the
  authenticator-only note when both apply, as soon as a factor is `Done`
  and neither a phrase nor a passkey that keeps the chats is.
- **Copy changes.** `protect.body` gains "Choose at least one."; the
  passkey row's body is unchanged; the phrase row's body drops "Stronger,
  and nothing to do with email." for "The strongest way back." (the
  existing `protect.phrase.body`); its title becomes `Also save a recovery
  phrase` (`protect.phrase.title_optional`).
- **Removed:** `protect.skip`, `protect.skip.confirm.*`,
  `protect.banner.none`, `protect.banner.action`, `protect.status.none`, and
  the line "An account with a passkey does not show the 'no way back in'
  banner."

What happens when a flow fails or is left:

| Case | What the person sees |
|---|---|
| Email: any state of the 19.2 table that is not `Done` | That table's line on the email surface; `Back` returns to the step with the row not done. |
| Email: mail could not be sent | "We could not send the letter. Try again later." on the email surface; the other rows stay available. |
| Authenticator: wrong code, offline, leaving | 19.1b; the row stays not done. |
| Passkey: the system sheet cancelled | Back on the step, one line under the passkey row: "No passkey was added." (`protect.passkey.cancelled`), no error styling. |
| Passkey: creation failed | "Could not add the passkey. Try again, or choose another way." (the existing key, text changed: it named `Not now`). |
| The app is closed on the step | At the next start the step is shown again before the app (19.1c). |

Accessibility: `Continue` announces its disabled state together with the
line under it; each row announces `Done` with its title.

### 19.1b The authenticator app (added 2026-11-26)

Status: designed, not built. Entry points: `Use an authenticator app` on the
step of 19.1a, and Settings > Security > Recovery > Authenticator app
(10.1), which is the same screen. Until now the setup existed only in
Settings (`settings.recovery.totp.*`); this subsection specifies it for both.

```
Use an authenticator app

Scan this code with your authenticator app, or type the key in
by hand, then enter the six-digit code the app shows.

        [ QR code ]

Key: ABCD EFGH IJKL MNOP QRST UVWX YZ23 4567     Copy

[ Open in authenticator app ]        (phones only)

Code  [ _ _ _ _ _ _ ]

Back
```

- The QR code is the shared `BrandedQr` component; the key is the same
  secret in groups of four, in the code font, with `Copy`
  (`settings.recovery.totp.key`, `common.copy`).
- `Open in authenticator app` (`settings.recovery.totp.open_app`) is shown
  on iOS and Android only and hands the setup link to whichever
  authenticator app the phone has. It is there because a phone cannot scan
  its own screen; it is a link to another app, not a share sheet.
- The code is checked on the sixth digit, with no separate button.
- **Proof.** Setting up an authenticator needs the 17.1 proof (it is one of
  the reset's factors, so a bare session must not be able to add one). The
  sign-up counts as that proof for ten minutes, so the step asks nothing
  more. In Settings, when no proof is live, the 17.1 block is drawn above
  the QR code with the helper "Enter your password to set up an
  authenticator app." (`proof.add_totp.help`).

| State | What the person sees |
|---|---|
| Starting | The QR area shows a spinner; nothing else is usable. |
| Ready | As drawn. |
| Checking | The code field is disabled with a spinner beside it. |
| Wrong or expired code | `settings.recovery.totp.error.wrong` ("That code did not work. Wait for the next code in the app and try again."); the digits clear and keep focus. |
| Too many tries | `recovery.error.rate_limited` with the countdown; the field stays disabled until zero. |
| Already set up | `settings.recovery.totp.already_enabled`; at sign-up this cannot happen, and if it does the step marks the row `Done`. |
| Offline | `common.error.offline`; the QR and key stay. |
| Any other failure | `settings.recovery.totp.error.generic` with `Try again`, which starts over with a new key. |
| Done | At sign-up, back to the step with the row `Done`. In Settings, `settings.recovery.totp.done` and the card shows "On". |
| `Back` before Done | Back to where it came from; nothing is set up. The key shown is never used: the next setup makes a new one. |

`settings.recovery.totp.done` loses its second sentence's claim ("A code
from the app can now get you back into your account.") for "A code from the
app now confirms it is you." (`settings.recovery.totp.done`, text changed):
the code signs in (section 6) and confirms a reset, and it does not bring
keys.

### 19.1c Every account keeps a second way in (added 2026-11-26)

- **At every session landing**, an account with a username and none of the
  three (no confirmed authenticator, no recovery email, no passkey) is shown
  the step of 19.1a before the app, with the heading "Add a second way to
  get back in" (`protect.title_required`) and the same rows and rule. This
  covers an account whose only factor was a passkey after a reset (15.1b),
  after a recall (18.4, which clears the authenticator and the passkeys),
  and accounts made before this rule.
- **The last one cannot be removed.** In Settings, `Remove recovery email`
  and deleting the last passkey are refused when the account would be left
  with none of the three: the app's own dialog says "Add another way to get
  back in first. This is the last one this account has."
  (`protect.last_factor`) with `OK`. The server refuses it too.
- **Changing the username** turns the recovery email off (19.2). When the
  email is the last of the three, the device carries the address it kept
  into the change (ADR-0076 section 2), so the email moves to the new name;
  when it holds no address, the username screen asks for it before the
  change, with the line "Your recovery email moves to your new username.
  Type it once." (`protect.username_email`).
- **The authenticator as the only one left.** When the authenticator
  becomes the account's only second way in (an email or a passkey was
  removed, or a reset deleted the passkeys), Settings > Security > Recovery
  (10.1) shows the note of 19.1a at its top, in the same `--warning` style, for
  as long as it stays the only one; it cannot be dismissed. The
  confirmation of a removal that would leave only the authenticator adds,
  before its buttons: "Your authenticator app will be the only other way to
  confirm it is you. If you lose it, you will not be able to get this
  account back." (`protect.totp_only.remove_notice`; with a saved phrase,
  "...If you lose it and your recovery phrase, ...",
  `protect.totp_only.remove_notice_phrase`). The note is not shown anywhere
  else: not on the chat list, not as a banner.
- **Settings > Account** status line (amended 2026-11-26, after the email
  vault was removed): "Your chats are kept by your recovery phrase" when a
  phrase is saved, else "Your chats are kept by a passkey" when a passkey
  keeps them (19.1d), else "Your chats are kept by your password only"
  (`protect.status.phrase`, `protect.status.passkey_seal`,
  `protect.status.password_only`), the last one in the `--warning` colour
  with the 19.1d actions. `protect.status.email` and `protect.status.totp`
  are not used.
- **Guest accounts** (ADR-0032) have no username and see none of this;
  `Create account` after a call ends on the step of 19.1a like any sign-up.

### 19.1d When the chats hang on the password alone (added 2026-11-26)

Status: designed, not built. Maintainer's decision of 2026-11-26.

**What decides it.** After a key reset, contacts give the chats back only
when the reset is sealed by something the account set up before
(ADR-0101 section 10, UX 15.1c): the password as it is now, a passkey that
keeps the chats, or the recovery phrase. The password is the one that is
easy to lose. An account is **kept by the password only** when it has a
second way in (19.1a) but no saved phrase and no passkey that keeps the
chats.

**A passkey that keeps the chats** is one whose authenticator gave Lakko a
secret of its own when the passkey was added (the WebAuthn `prf`
extension). Lakko learns this when the passkey is created, not before.
Where it is expected, from what the platforms document and nothing measured
here: Apple platform passkeys from iOS 18 / macOS 15; Android passkeys on API
34 and above (below it the request is dropped at creation, ADR-0072);
browsers depending on the browser and the authenticator. A passkey that did
not give the secret still signs in and still confirms a reset (15.1b); its
row in Settings > Security > Passkeys says "Does not keep your chats."
(`settings.passkeys.no_seal`).

**The notice.** One copy, in the `--warning` colour with the warning icon:

> **Your chats depend on your password**
> If you forget your password and lose your devices, your chats will not
> come back. Save a recovery phrase or add a passkey to keep them.
>
> [ Save a recovery phrase ]   [ Add a passkey ]

(`protect.password_only.title`, `.body`, `.save_phrase`, `.add_passkey`.)
`Add a passkey` is shown only where this device or browser can create
passkeys. If the passkey it creates does not keep the chats, the notice
stays and gains one line, "This passkey signs you in but does not keep your
chats." (`protect.password_only.passkey_no_seal`).

**Where it shows:**

1. **The sign-up step** (19.1a), under the rows, once a second way in is
   `Done` and while neither a phrase nor a passkey that keeps the chats is.
   It does not block `Continue`.
2. **Settings > Security > Recovery** (10.1), at the top, for as long as
   the account is kept by the password only. It cannot be dismissed. The
   status line of Settings > Account (19.1c) says the same in one line.
3. **Once after a sign-in on a device**, the first time the app opens
   there: the app's own dialog (`LakkoDialog`) with the same title and body,
   `Save a recovery phrase` as the main action and `Not now` (plain), which
   closes it for good on this device. Not after a key reset: the reset's
   own screens already said what was kept (15.1c).

Nowhere else: not on the chat list, not as a banner, not in a notification.

**When it goes.** As soon as the account's published protection (ADR-0101
section 10) lists a saved phrase or a passkey that keeps the chats, on every
device, at its next look at the account's own protection. Removing that
phrase is not possible; deleting that passkey brings the notice back.

Accessibility: the notice is announced once when it appears; its two
buttons are ordinary buttons in reading order.

### 19.2 Adding the address

Entry points: Settings > Account > **Recovery email**, and 19.1a.

```
Recovery email
If you forget your password, we send a code to this address
so you can choose a new one.

Email   [ name@example.com ]

Lakko does not keep your address. You will type it again,
together with your username, when you need a code.

Anyone who can read this mailbox can get into your account.

[ Send code ]
```

Then, on the same surface:

```
Enter the code
We sent a 6-digit code to name@example.com.

[ _ _ _ _ _ _ ]

Send again (in 0:45)        Use a different address
```

- Adding, replacing and removing the address each need the proof of 17.1
  (password or passkey within ten minutes); the block of 17.1 is drawn above
  the email field when no proof is live. Sign-up counts as a proof.
- The account must have a username. A guest account sees one line, "Choose a
  username first.", and a button to the username screen.
- The address is shown back only on this surface and only until it closes.
  Afterwards Settings shows **Recovery email: On** with `Replace` and
  `Remove`, never the address (the device does not keep it either).
- Replacing is the same two steps for the new address; the old one stops
  working when the new code is accepted, not before.
- Removing is one confirmation in the app's own dialog: "Remove recovery
  email? You will not be able to get a new password by email." When the
  email is the account's last second way in, removing is refused instead
  (19.1c, since 2026-11-26).
- Changing the username turns the recovery email off (the stored value was
  made from the old name). The username screen says so before the change and
  offers to add it again after. When the email is the last second way in,
  it moves with the name instead (19.1c).

States:

| State | What the person sees |
|---|---|
| Not a valid address | One line under the field; `Send code` disabled until it parses. |
| Sending | `Send code` shows its working word. |
| Sent | The code step. |
| Wrong code | "That code is not right." The fields clear and keep focus. |
| Code expired (10 minutes) | "That code has expired." with `Send again`. |
| Too many wrong codes | "Too many attempts. Send a new code." The code step returns to the address step. |
| Too many letters | `recovery.error.rate_limited` with the countdown. |
| Mail could not be sent | "We could not send the letter. Try again later." |
| Mail is not set up on this server | The row in Settings and the sign-up step are absent. |
| Offline | `common.error.offline`. |
| Done | Back to Settings with **Recovery email: On**; at sign-up, on to the app. |

Every other signed-in device of the account shows the notice of 17.5's kind
once: "A recovery email was added to your account." / "...was replaced" /
"...was removed", with `OK` and `This was not me` (which opens Settings >
Account > Recovery email on that device).


The line "Anyone who can read this mailbox..." is shown on every account
(amended 2026-11-26: "Save a recovery phrase to change that." is removed;
with the email vault gone a phrase no longer changes what the mailbox
reaches, which is a new password, sign-in and confirming a reset, never the
keys; a reset confirmed only by the mailbox and a new password is unsealed,
15.1c, so it does not move the chats). When the account has an authenticator the form adds: "You will also need a
code from your authenticator app. If it is on the phone you lose, the email
will not be enough."

### 19.3 Getting back in with the code

Entry point: **Get back in** (section 2) gains one row, first in the list
above both groups, since what it brings depends on the account:

```
  [mail]     Email code                     >
             A code sent to your recovery email.
```

The row is always shown (no row depends on server state).

Step 1: username and address. Step 2, always, whatever was typed:

```
Check your email
If that username and address belong together, a code is on
its way. It works for 10 minutes.

[ _ _ _ _ _ _ ]

Send again (in 0:45)        Back
```

Step 3, only when the account has an authenticator: its 6-digit code, with
"Lost your authenticator? Use another way to get back in."

Step 4, "Choose a new password", then:

- Signed in; on a device without keys, **Restore your keys** (section 8).
  (Amended 2026-11-26: the "No phrase saved" branch that brought the keys
  back from the email vault is removed with the vault.) A password changed
  here, on a device without the keys, is not yet the password that seals a
  reset (15.1c) or locks the owned channels: a device that holds the keys
  takes it over at its next password sign-in. Until then, a reset is
  unsealed unless a passkey keeps the chats.

Rules:

- Step 2 looks and behaves the same for a matching pair, a wrong pair, an
  unknown username and an account with no email. A letter leaves only for a
  match.
- A wrong code and a code that was never sent get the same line: "That code
  is not right." Five tries per code.
- Step 3 appears only after the mailed code was accepted. There is no way
  past it without the authenticator.
- Nothing before step 4 tells which kind of account this is.
- The username is pre-filled per section 2; the address never is.

States: as the table of 19.2 for codes, letters, limits and offline, plus
"Email codes are not available right now." when mail is not set up.

### 19.4 The letter

Subject: "Your Lakko code". Body: the code, "It works for 10 minutes.", and
"If you did not ask for this, ignore this letter. Your password has not
changed." No username, no link, no device or location, no image. Sent in the
language of the requesting device.

### 19.5 Saving a recovery phrase

Settings > Account > **Recovery phrase**, and 19.1a.

```
Save a recovery phrase
12 words that only you keep. After this, a code by email can
change your password but cannot open your account.

This gives your account new keys. Your contacts and chats
stay as they are. Your other devices stay signed in.

[ Show my 12 words ]
```

**Amended 2026-11-26:** the paragraph "This gives your account new keys..."
and the key change behind it are removed with the email vault (no copy of the
phrase ever left the device), and the first paragraph loses its second
sentence: "12 words that only you keep. They bring back your keys on a new
device, and your chats after a reset." (`protect.phrase.save.body`, text
changed). Saving a phrase changes no key: the screen shows the words the
device already holds. When the three-word check passes, the device
publishes the account's protection again with the phrase in it (ADR-0101
section 10), which ends the notice of 19.1d. The bullets below that speak of
"the new keys" no longer apply.

- Needs the proof of 17.1. Then the existing phrase screens: the words in
  the grid, the confirmation of three of them, "I wrote them down".
- With the new keys: the words are shown only when the change has gone
  through; until then a progress line, and on failure "We could not finish.
  Nothing changed." with `Try again`.
- Without them: the words appear at once. The account counts as protected
  only after the three-word check, when the server has been told; on failure
  of that last call the three-word screen stays, with "We could not finish.
  Nothing changed." and `Try again`. A person who leaves before then has saved
  nothing, and the row in Settings stays as it was.
- Afterwards Settings shows "Your chats are kept by your recovery phrase"
  (19.1c) and `Show phrase` (behind 17.1). There is no way back to an account without one.
- Other devices of the account show once: "A recovery phrase was saved for
  your account." with `OK` / `This was not me`.

### 19.6 What the account's devices see afterwards

Every signed-in device of the account shows once, the next time it is
opened, one of:

```
Your password was changed
Someone used your recovery email to choose a new password
on <date>.
[ It was me ]     [ It was not me ]
```

```
Your account was opened with your recovery email
On <date>, on another device.
[ It was me ]     [ It was not me ]
```

(Amended 2026-11-26: the second notice is removed with the email vault; a
mailed code no longer opens the account with its keys, so only the first
one remains. The "A recovery phrase was saved" notice of 19.5 is shown from
the account's published protection, not from a server notice.)

`It was not me` opens one screen with, in order: choose a new password (this
device holds the keys, so it asks nothing else), `Remove recovery email`,
and, when no phrase is saved, `Save a recovery phrase` (amended
2026-11-26: without the line about new keys; with the vault gone, whoever
came in by email got no keys to lose). When the keys
were reset after the change, the device shows "Was it you?" of 18.4 instead.

Contacts see nothing.

### 19.7 Taking the account back with the phrase

For a person whose account has a saved phrase and whose keys were reset by
someone else. On an old device this is 18.4, unchanged, with one more
choice under the password field: `Use my recovery phrase instead`. Without
an old device, **Get back in** > Recovery phrase: when the words belong to
the account as it was before a reset in the last month, the form continues
with

```
This account's keys were changed on <date>.
Was that you?

[ It was me ]          [ Take the account back ]
```

`Take the account back` asks for a new password and does what 18.4 does:
the account returns to this person with new keys, every other device is
signed out, the authenticator and passkeys are cleared. The same costs as
18.9 apply and the form says them. After the month the words are simply
"not this account's phrase".

~~Not offered for an account without a saved phrase: there the person who came
in by email holds the same words.~~ Amended 2026-11-26: with the email vault
removed nobody gets the words by email, so the choice is offered to every
account; in practice it serves only a person who saved the phrase, since
nobody else has seen the words.

### 19.8 Data the UI needs

- Whether the server can send mail.
- For the signed-in account: has an email (yes/no, never the address), has a
  saved phrase, has an authenticator.
- Enrol start / finish, replace, remove, each under a 17.1 proof.
- Recover: start with one uniform answer; code check answering "accepted"
  and "authenticator needed"; authenticator check; then either the sealed
  keys or the password-only path; seconds until another letter.
- Save a phrase: progress and outcome of the key change, the new words only
  on success.
- Notices for the account's own devices: password changed by email, account
  opened by email, email added / replaced / removed, phrase saved.
- For 19.7: whether the typed phrase is the account's phrase from before a
  reset within the window, and the reset's date.
- Since 2026-11-26 (19.1a to 19.1c): authenticator setup start / finish
  under a 17.1 proof; whether mail is available (the authenticator always
  is); the three factors of the signed-in account; a refusal, with its own
  code, of removing the email or a passkey that is the last of the three.

### 19.9 Cross-platform shape

The same steps and rows on web, iOS and Android. Code fields use each
platform's one-time-code hint. No system dialog, no deep link (the letter
has no link).

### 19.10 Deliberately not built, and open

- ~~**An account without a saved phrase is as safe as its mailbox, and the
  operator of the server could open it.**~~ ~~When someone else opens such
  an account by email, both hold the same keys.~~ Both withdrawn 2026-11-26
  with the email vault: no mailed code returns keys, for any account.
- The mailbox still reaches the password and, with it and the mailbox as the
  second way in, a key reset. Such a reset is unsealed (15.1c): it takes the
  username and starts the account over, and contacts keep their chats away
  from it. 19.7 and 18.4 are the answer for the owner.
- ~~No email and no phrase: no way back. The app says so twice (19.1a).~~
  Since 2026-11-26 every account has an email, an authenticator or a
  passkey (19.1a, 19.1c), and with the password any one of them resets the
  keys (15.1b), after which conversations come back from contacts.
- **What the second factor costs (2026-11-26).** A person whose only factor
  is an authenticator app on the phone they lost, with no recovery email,
  no passkey elsewhere and no phrase, can no longer reset: the account is
  lost.
- **What the seal costs (2026-11-26).** A person who forgot the password,
  has no saved phrase and no passkey that keeps the chats, gets a new
  password by mail and resets: the reset is unsealed, the account starts
  over, and the chats, groups and owned channels do not come back. 19.1d
  says so in advance. The maintainer allowed the authenticator as the only factor with a
  warning (19.1a, 19.1c); nothing else can be done about it without making
  the password enough again.
- **Owned channels** (ADR-0101 section 7, settled 2026-11-26) are under two
  locks: the password with a second factor, and the account keys. A device
  with the keys always opens them. A reset with the password and a factor
  opens them when the password is the one the list was locked with, or
  when a passkey that keeps the chats sealed it (each such passkey has its
  own lock, decided 2026-11-26). The
  route through a mailed code that brought the keys back is gone with the
  email vault (2026-11-26). An account that lost the phrase, every device
  and the password loses its owned channels, and its reset is unsealed, so
  its chats and groups do not come back either.
- An authenticator on the lost phone blocks the email route.
- Changing the username turns the recovery email off. A forgotten address or
  username cannot be looked up.
- No reminder or security letters. One address per account. No phone number.
- Strings: none added to the catalog. Nothing here was built or walked.
