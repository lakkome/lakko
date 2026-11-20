<!-- SPDX-License-Identifier: AGPL-3.0-or-later -->
<!-- SPDX-FileCopyrightText: 2026 Lakko Contributors -->

# UX spec: supporter codes and the supporters list

**Status:** accepted 2026-11-20 (maintainer: blind exchange for the code, the list served
from lakko.me). Mechanism: [ADR-0097](../adr/0097-supporter-codes-and-list.md).
Built 2026-11-20 on the server and on web and desktop, iOS and Android; unit-tested
only, not walked against a server or on a device.
**Surfaces:** web and desktop (one code base), iOS, Android.
**Why:** two rewards of the Kickstarter campaign (`docs/funding/kickstarter-draft.md`): a
place in the beta when all places are taken, and a name in the supporters list inside the
app.

## 1. A supporter code

A backer gets a code by email: 16 letters and digits in four groups, for example
`K7QM-2XRA-9DPF-4WNB`. Typing is forgiving: lower case, spaces and missing dashes are
accepted, and the letters O, I and L are read as 0, 1 and 1.

### 1.1 Where it is entered

Only on the "Lakko is full for now" state of the sign-up screen
([`beta-account-cap.md`](beta-account-cap.md) section 2 item 3). Below the two buttons, a
link:

> `I have a supporter code`

It opens, in place of the full state:

> **Supporter code**
>
> Enter the code from your email.
>
> [ code field ]
>
> `Continue` · `Back`

- `Continue` is enabled once the field holds 16 letters and digits.
- While the code is checked the button reads `Checking...` and is disabled.
- `Back` returns to the full state.

### 1.2 Outcomes

- **The code works:** the ordinary sign-up form, with one line above it:
  > Your supporter place is held for this sign-up.
  The place is held on this device for 30 days; the person signs up as usual.
- **The code is not known:** under the field, "This code does not work. Check it and try
  again." The field keeps what was typed.
- **No connection or a server error:** under the field, "Could not check the code. Try
  again."
- **Too many tries** (the server limits tries per address): "Too many tries. Try again
  later."
- **Sign-up refused because the code was already used for another account** (the server
  can tell only at that moment): the account just made is deleted as in
  `beta-account-cap.md` section 2 item 4, and the full state shows with one line above
  it: "This supporter code has already been used."
- **The beta has a free place anyway:** the held place is not spent; it stays on the
  device until its 30 days end.

Nothing on screen says that the code and the account are kept apart; the privacy page
says it (section 3).

### 1.3 Not in v1

A code typed by a call-link guest after the call, or in Settings by an existing account.
The guest's closed question (`beta-account-cap.md` section 3) is unchanged.

## 2. The supporters list

### 2.1 Where

Settings > About gains a row **Supporters**, after "How Lakko protects you". The row is
shown only when the device holds a checked list with at least one name; before the
campaign there is none, and the row is absent.

The client fetches the list when About opens (and at most once a day otherwise). A list
that fails its signature check is ignored as if absent. A list older than the one held
is ignored.

### 2.2 The screen

> **Supporters**
>
> Lakko is paid for by the people who back it. Thank you.
>
> Name
> Name
> ...

- Names in the order of the file, one per line, plain text, no links, no avatars.
- A long list scrolls; no search, no count.
- **Loading** (opened before the first fetch finished, with nothing held): a spinner.
- **Failed with nothing held:** "Could not load the list. Try again." and `Try again`.

### 2.3 How a name gets on it

A backer chooses the name in the Kickstarter survey (any name, or none). The maintainer
adds it to the source file and signs a new list (ADR-0097 section 2). Nothing in the app
asks for a name.

## 3. Privacy page

`docs/legal/privacy.md` gains: a supporter code is checked by our server; the place it
holds is used through a token our server cannot connect to the code; the names in the
supporters list are the names backers chose to show.

## 4. Strings (`l10n/catalog/en.json`)

| Key | Text |
|---|---|
| `auth.signup.code.link` | I have a supporter code |
| `auth.signup.code.title` | Supporter code |
| `auth.signup.code.body` | Enter the code from your email. |
| `auth.signup.code.continue` | Continue |
| `auth.signup.code.checking` | Checking... |
| `auth.signup.code.back` | Back |
| `auth.signup.code.held` | Your supporter place is held for this sign-up. |
| `auth.signup.code.unknown` | This code does not work. Check it and try again. |
| `auth.signup.code.failed` | Could not check the code. Try again. |
| `auth.signup.code.limited` | Too many tries. Try again later. |
| `auth.signup.code.used` | This supporter code has already been used. |
| `settings.about.supporters` | Supporters |
| `supporters.title` | Supporters |
| `supporters.body` | Lakko is paid for by the people who back it. Thank you. |
| `supporters.failed` | Could not load the list. Try again. |
| `supporters.retry` | Try again |

None is security-critical.
