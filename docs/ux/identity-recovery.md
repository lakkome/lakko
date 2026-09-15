<!-- SPDX-License-Identifier: AGPL-3.0-or-later -->
<!-- SPDX-FileCopyrightText: 2026 Lakko Contributors -->

# Identity recovery

UX spec for getting an account back on a device that does not hold its
identity: a new browser, a new phone, a reinstalled app, or a forgotten
password. Architecture: [ADR-0049](../adr/0049-identity-recovery-word-vault.md).

Before this, every such device ended in a prompt for all 24 recovery words or a
QR code to scan from another device, and a device that did pair got old
messages but no live chats.

## 1. Goal and entry points

| Goal | Entry point |
|---|---|
| Sign in on a device that has never held this account | Login > password accepted > "Restore your identity" |
| Sign in with a passkey on a new device | Login > Use a passkey > "Restore your identity" (until passkey-carried identity lands) |
| Forgot the password | Login > "Forgot password?" > Recover |
| Add a second device from the first one | Settings > Devices > Add a device (unchanged, QR) |

All of them land on one screen, **Restore your identity**, with the same three
routes in the same order.

## 2. The screen

```
Restore your identity

Type three words from your recovery phrase.

  [ 5  ______ ]   [ 14 ______ ]   [ 21 ______ ]

  (o) Continue

Or:
  Confirm on another device  >
  Type all 24 words instead  >
```

- The three numbers are the positions the server returned, shown ascending.
  They are the same every time for this account; a user who has done this
  once recognises them.
- Each field autocompletes from the 2048-word list after two letters and
  shows a red outline for a word that is not in the list. **Continue** is
  disabled until all three fields hold valid words, so a typo never spends
  a guess.
- Paste of a full 24-word phrase into any field fills the three positions
  from it and leaves the rest of the clipboard alone.
- Below the fields, in muted text: "N tries left" appears only after the
  first wrong attempt, never before. The count is the client's own.

## 3. Route A: three words

| Step | What happens |
|---|---|
| Continue | Button reads "Checking…"; the client stretches the words (about a second on a phone) and asks the server. |
| Correct | The screen changes to "Identity restored" for a moment, then the app opens. If the device also needed access (forgot-password route), the password-reset form follows, pre-filled with the username. |
| Wrong | The three fields shake and clear, "Those words did not match. N tries left." Positions stay the same. |
| Locked out | "Too many attempts. Try again in 14:59." with a live countdown; the other two routes stay available. |
| Vault destroyed (after the 10th wrong) | The three-word route disappears from this screen for this account; the copy reads "The three-word shortcut is no longer available for this account. Use another device or your full 24-word phrase." |
| Server unreachable | "Could not reach the server. Check your connection." The fields keep their contents. |
| Words correct but the identity does not match the published one | This is the ADR-0017 fail-closed check. Copy: "This phrase belongs to a different identity than the one published for @name. Nothing was changed." No retry counter is spent (the server accepted the words). |

## 4. Route B: confirm on another device

### On the new device

```
Confirm on another device

Open Lakko on a device that is already signed in
and confirm this sign-in there.

Comparison code
   47 19 03

Make sure the other device shows the same code.

[ Show a QR code instead ]        [ Cancel ]
```

- The code is six digits in three pairs, large.
- "Show a QR code instead" flips to the existing QR pairing screen; the same
  request stays valid in both forms.
- The screen polls. States: **waiting** (spinner under the code), **confirmed
  on another device** (changes to "Restoring…" then the app opens),
  **declined** ("Declined on another device."), **expired** after 10 minutes
  ("This request expired. Start again.").

### On the old device

A modal over whatever screen is open; on native, a notification that opens
it.

```
New sign-in request

Someone is signing in to your account on a new device.

Comparison code
   47 19 03

Does the new device show the same code?

[ It is not me ]            [ Confirm ]
```

- **Confirm** runs the pairing. The modal shows "Adding the device…" and then
  "Adding it to your chats… (12 of 30)" as the group adds land; it can be
  dismissed, the work continues in the background and a failure surfaces as
  a banner ("Some chats could not be added. Open Devices to retry.").
- **It is not me** deletes the request; the modal closes with "Declined. If
  this keeps happening, change your password."
- Only one request is shown at a time; a newer one replaces an older one.
- The modal never shows the new device's name or location; there is none the
  server could vouch for, and a fake one would be worse than none.

## 5. Route C: all 24 words

The existing phrase input (24 numbered slots, autocomplete, paste-to-fill).
Same success and mismatch states as route A minus the counter: the full
phrase is checked locally against the published identity and no server guess
is spent. Copy at the top: "Type your full recovery phrase. You can find it
on the paper you saved at signup."

## 6. After a passkey login on a new device

Until passkey-carried identity lands, a passkey login on a new device goes to
the same screen with a one-line addition at the top: "Signed in with your
passkey. Now restore your identity so your contacts recognise you." Once it
lands, this screen is skipped when the passkey carries the identity, and
shown as today when it does not (an older passkey, or an authenticator
without PRF).

## 7. What the new device sees afterwards

| Route | Chats screen after restore |
|---|---|
| Another device (B, or QR) | The chat list fills as the old device adds this one to each group; a top banner reads "Catching up with your other device…" until it finishes. |
| Three words or 24 words, no other device confirmed | The list is empty, with a card: "Your identity is restored. Your chats live on your other devices. If you have none left, ask your contacts to add you back; they will see you as the same person." plus a button to Contacts. |

The card is the honest state, not a loading state; it must not show a spinner.

## 8. Signup and settings

- Signup shows the 24 words as today, then a short line: "You will normally
  only need three of these, at positions 5, 14 and 21. Keep the whole list
  safe anyway." The three positions are highlighted in the grid.
- Settings > Account gains a row **Recovery**: "Three-word recovery: on ·
  Passkey: 2 · Other devices: 1". It is informational; there is no toggle to
  disable the three-word route (a user who wants no server artefact does not
  yet have a choice; recorded as a follow-up).

## 9. Data the UI needs

From the server:

- `positions[3]` and `salt` for an account (route A start).
- The generic refusal (401) on a miss and 429 on lockout. The server never
  reports a remaining count: a count returned only for real accounts would
  tell an unknown account apart. The client counts its own misses in the
  session and shows "N tries left" from that.
- `request_id` and the pending-request list for route B on both sides.
- Whether the account has a vault at all is never exposed; a destroyed vault
  and a missing account answer alike. The "shortcut no longer available"
  state is derived client-side from the destruction response the client
  itself received on the 10th miss and is remembered locally for that
  username only.

From the client:

- `RecoveryMnemonic.fromEntropy` (exists), the phrase-check path
  `provideRecoveryPhrase` (exists), the pairing orchestrator (exists), and a
  new "add this device to all my groups" fan-out on the old device.

## 10. Cross-platform shape

One screen, three routes, same order, same copy. Divergences:

- Web: the old-device prompt is a modal inside the open tab; there is no
  push, so a user with no Lakko tab open sees nothing until they open one.
  The new device's copy says "open Lakko in a browser or on your phone".
- iOS and Android: the old-device prompt is a notification that opens the
  modal; the app polls on foreground as well.
- The comparison code is always compared, never typed, on every platform.
