<!-- SPDX-License-Identifier: AGPL-3.0-or-later -->
<!-- SPDX-FileCopyrightText: 2026 Lakko Contributors -->

# UX spec: sign-up when the beta is full

**Status:** accepted 2026-11-19 (maintainer). Server built the same day
(`services/auth/src/account_cap.rs`, DB-tested); web and desktop, iOS and Android built
the same day, unit-tested only, not walked on a device or against a server.
**Surfaces:** web and desktop (one code base), iOS, Android.
**Why:** the public beta has room for a fixed number of accounts with a username
([ADR-0046 amendment](../adr/0046-external-crypto-review.md), 1000 at launch).

## 1. What the server says

- `GET /signup/open` -> `{"open": true | false}`. Anonymous, cheap, rate limited per
  address. A client asks it before the sign-up proof of work.
- `PUT /auth/login-handle` -> `403 {"error":"registration-closed"}` when an account
  binds its **first** username and the beta is full. A rename or a re-bind is never
  refused. This answer is final; `/signup/open` can be stale by the time it is given.
- Accounts without a username (call-link guests) take no place. A guest can always join
  a call.
- An account that never got a username is deleted by the server a day after it was made.

## 2. Sign-up screen

1. When the sign-up screen opens, and again when the person submits it, the client asks
   `/signup/open`.
2. **Open, or the question failed** (no connection, an error, an older server answering
   `404`): the ordinary form. The bind decides later.
3. **Closed:** the form is replaced by the full state:

   > **Lakko is full for now**
   >
   > The beta has room for a limited number of people, and all places are taken.
   > Places open up when people leave. Try again later.
   >
   > `Try again` · `Log in`

   - `Try again` asks `/signup/open` again; while it runs the button reads `Checking...`
     and is disabled; open -> the form, with nothing typed lost; still closed -> stays,
     no extra message.
   - `Log in` goes to the sign-in screen, as the "Already have an account?" link does.
   - Nothing is shown about how many places there are or how many are taken.
4. **Refused at the bind** (the beta filled between the question and the bind): the
   account the sign-up just made has no username. The client deletes it at once
   (`DELETE /account`, which needs no fresh-auth proof for an account with no username and
   no passkey), wipes it from the device as a declined guest is wiped, and shows the full
   state of item 3. If the delete fails, the device still wipes it and the server deletes
   it within a day; nothing extra is shown. No username is left in the directory: the
   bind at auth comes before the directory registration (ADR-0054 S5 claim order); a
   client that registers in the directory first must delete that entry too.

## 3. A guest after a call (`docs/ux/call-links.md` section 9)

1. When the question "Create an account..." is about to show, the client asks
   `/signup/open`.
2. **Closed**, and the guest holds no username yet (a guest whose earlier `Create account`
   already took one keeps its place and always gets the ordinary question), the question
   reads, with no heading and no decline note:

   > Lakko is full for now, so this guest account cannot be kept. Places open up when
   > people leave.
   >
   > `OK`

   `OK` does what `No, thanks` does (deletes the guest account at once, same success and
   failure lines). The 23-hour rule of section 9 item 8 is unchanged.
3. **Open, or the question failed:** the question as today.
4. **`Create account` refused at the bind:** the form gives way to the closed question of
   item 2. The guest account is not deleted by the refusal; `OK` deletes it.

## 4. Everything else

- Signing in, a second device, recovery, renaming: unaffected; none of them binds a first
  username.
- Joining a call from a link: unaffected.
- No banner, count, waitlist or notification. A waitlist is not part of the beta.
- Logs carry the outcome only (`registration closed`), never a username.

## 5. Strings (`l10n/catalog/en.json`)

| Key | Text |
|---|---|
| `auth.signup.closed.title` | Lakko is full for now |
| `auth.signup.closed.body` | The beta has room for a limited number of people, and all places are taken. Places open up when people leave. Try again later. |
| `auth.signup.closed.retry` | Try again |
| `auth.signup.closed.checking` | Checking... |
| `auth.signup.closed.log_in` | Log in |
| `call.guest.closed.body` | Lakko is full for now, so this guest account cannot be kept. Places open up when people leave. |
| `call.guest.closed.ok` | OK |

None of them is security-critical. Machine translations into the other ten languages
follow the catalog's usual `machine` status.
