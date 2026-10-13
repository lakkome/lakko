<!--
SPDX-License-Identifier: AGPL-3.0-or-later
SPDX-FileCopyrightText: 2026 Lakko Contributors
-->

# Anti-abuse without making the phone pay (ADR-0054 S3 + S4)

What a person sees once per-request proof of work is gone. The rule behind
every state below: a limit is the server's decision, it never signs anyone
out, it never reads as "invalid", and it says when to try again.

## 1. Creating an account (the one remaining cost)

Account creation keeps one deliberately heavy piece of work, done once for the
lifetime of the account. On a phone without a JIT (every iPhone) it takes a
noticeable time; nothing else in the app ever does this again.

**Flow.** The person fills in the sign-up form and taps `Create account`.
The work starts at once, off the main thread; the form stays filled in.

**States.**

- **Working** - the button becomes a disabled row with a spinner and
  `Creating your account...`, and below it one line:
  `This takes a little while, once. You will not wait like this again.`
  No percentage and no time estimate: the work has no progress to report and
  its duration varies by device. `Cancel` returns to the filled-in form.
- **Leaving the screen** while working cancels the work (no half-created
  account; the server holds nothing until the proof is spent).
- **Too many sign-ups from this network** (429 from signup):
  `Too many new accounts from this network right now. Try again later.`
  with `Try again`. The form keeps its contents.
- Every other failure is unchanged from today.

**Guest accounts from a call link** go through the same work, shown in the
call join screen as `Getting you into the call...` with the same one line
below. (Whether guests keep this cost is ADR-0054 open question 2; until it is
answered they pay it.)

## 2. Signing in and recovering

No work, no waiting. A limit reached shows, in the place the error shows today:

- Login, password reset, authenticator code (and the sign-in that follows a
  three-word match; the three words themselves are checked on the device
  and reach no server limit, ADR-0072):
  `Too many attempts. Wait a moment, then try again.` (the existing Android
  copy, now on all three). If the server sends `Retry-After` of more than a
  minute: `Too many attempts. Try again in N minutes.`
- (Removed 2026-10-02, ADR-0072: the recovery code by email or SMS and its
  send limit no longer exist.)
- Signing in on a new device with "Is this you?" on another device, limit
  reached: `Too many requests to your other devices. Try again in a few minutes.`
- Choosing or changing a username, limit reached (`handle-bind-limited`,
  added 2026-09-28): `You have changed your username too often. Try again in
  M minutes.` (M = ceil(retry_after_secs / 60); `Try again later.` when the
  wait is over an hour). The form keeps its contents.

The three-words destruction cap is gone with the server-side word vault
(ADR-0072, 2026-10-02). Three words now have a budget kept by the device: 5
wrong tries, then a 15-minute pause with a countdown
(docs/ux/identity-recovery.md section 6).

## 3. Everyday actions that spend a token (S3)

Looking someone up, opening an invite link, registering a channel name,
knocking on a group link, turning on notifications: each spends one anonymous
token. Tokens are drawn in the background with the session; an ordinary day
never runs out, and sending or reading messages never spends one.

- **No token at hand, refill in progress** - the action waits silently, up to
  a few seconds, as part of its normal loading state. No extra UI.
- **Daily allowance used up** (429 `anon-token-quota-exhausted`):
  - search: `You have looked up a lot of people today. Try again tomorrow.`
  - invite link, group link, channel name:
    `You have used today's allowance for this. Try again tomorrow.`
  - notifications registration: silent retry at the next launch; no message
    (the app keeps working by polling).
  The session is never ended and nothing asks for a password.
- **Tokens switched off on the server** (503) - the action fails with the
  generic network failure copy it has today; no new wording.

## 4. Data the UI needs

- From signup: a distinct 429 ("rate limited") vs other failures, and the
  proof-of-work solver running cancellably off the main thread.
- From auth routes: 429 with an optional `Retry-After` (seconds).
- From the token pool: "no token yet" (wait) vs "quota exhausted" (the 429 body
  code `anon-token-quota-exhausted`, `retry_after_secs`) vs "disabled" (503).

## 5. Cross-platform shape

One mental model, three implementations: the signup working state is the same
row, spinner, sentence and `Cancel` on web, iOS and Android; the limit copy is
the strings above verbatim.
