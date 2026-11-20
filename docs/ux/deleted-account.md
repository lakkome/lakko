<!-- SPDX-License-Identifier: AGPL-3.0-or-later -->
<!-- SPDX-FileCopyrightText: 2026 Lakko Contributors -->

# UX spec — a deleted account

**Status:** design, the input to [ADR-0098](../adr/0098-deleted-account.md) and the client
builds (web, desktop, iOS, Android).
**Decision:** the maintainer's (2026-11-20). When a person deletes their Lakko account, the
people who talked to them see **Deleted account**, as in Telegram: no `@username`, no photo,
no name. What they wrote stays readable. Nothing can be sent to them and they cannot be
called.

## 1. Why this exists

Seen on the test stack: after W7 Droid deleted their account, the other side still showed
`W7 Droid @w7droid1422`; a message read `Sent` and arrived nowhere; and `@w7droid1422` had
already been taken by a different, new account. A name that points at another person is the
impersonation this product must never show.

## 2. The rule in one line

A person is shown as **Deleted account** from the moment this device learns their account is
gone, everywhere they appear, until this device forgets them. It is final: it never turns back
into the old name.

A `@username` is shown beside any person only while this device has recently confirmed that
person's account still exists (ADR-0098 section 4). Otherwise the line shows the name alone.

## 3. States per surface

| Surface | What the person sees |
|---|---|
| Chat list row | Title `Deleted account`, the neutral person avatar (no photo, no initials), the last message preview unchanged, unread count unchanged. |
| Contacts list | Row `Deleted account`, neutral avatar, no `@username`, sorted by that title. Not matched by the old name or username in the contacts search. |
| One-to-one chat | Header `Deleted account`, no subtitle, no call buttons, no video button. The whole history stays readable; images and files still open while their files exist. The composer is replaced by one line: `This account was deleted.` No button. Long-press on a message keeps Copy, Save, Forward, Pin for me; Reply and React are gone. Typing and read markers stop. |
| A message sent before this device knew | Stays `Sent` (it never arrives). No error, no retry. |
| Group timeline | Each of their messages is labelled `Deleted account` with the neutral avatar. Their reactions still count; in a reaction's list of people they are `Deleted account`. Quotes of their messages (`reply`) show `Deleted account` as the author. A forward that names them keeps the name it carried (it is a snapshot the forwarder chose). |
| Group members list | Row `Deleted account`, neutral avatar, no `@username`, role mark kept (`Owner`, `Admin`). The row goes away by itself when an owner's or admin's device removes their devices from the group. A group they owned keeps working; nobody can change roles in it. |
| Person info (opened from a chat, a member row, a contact) | Title `Deleted account`, neutral avatar, no `@username`, no safety number, one line `This account was deleted.` Actions: `Open chat` (only when a one-to-one exists), `Delete chat` (same), `Remove contact` (only when on the contact list). No Call, no Add contact, no Rename, no Add to group, no Share, and no Block or Report (ADR-0099): nothing more can arrive from them, and there is no account left for a report to act on. The block and suspension lines of ADR-0099 are not shown. |
| Their messages, message menu | `Report` stays on their messages (`docs/ux/reporting-and-moderation.md`): what they sent before deleting can still need a look. |
| Call roster, call tile | Cannot occur after the deletion is known (they cannot join). An ended call in the history shows `Deleted account`. |
| Notifications | The deletion itself never produces a notification. A notification already shown stays. Background previews of their earlier messages use `Deleted account` as the sender once this device knows. |
| People search | Their username is not found (the directory no longer has it, or it now belongs to someone else, who is shown as that someone else). Local results never show them by the old name. |
| Incoming contact request from them, not yet answered | Removed silently when this device learns the account is gone; accepting such a request that is still on screen shows `This account was deleted.` and removes the card. |
| A pending contact I added through their link, waiting for them | Turns into `Deleted account`; the messages held for them are discarded, never sent; no composer. `Delete chat` removes it. |
| Their invite link (`/i/`) I open later | The ordinary dead-link outcome (Contacts screen); nothing about deletion. |
| My own group invite link they had used | Nothing changes. |

## 4. What the person never sees

No banner, no system row in the timeline ("X deleted their account"), no notification, no
toast, no question. The state is shown, not announced. No word about keys, the directory,
a server or a check. No retry button anywhere.

## 5. Copy (catalog keys)

| Key | Text | Where |
|---|---|---|
| `person.deleted.name` | `Deleted account` | every name, title and author label |
| `person.deleted.body` | `This account was deleted.` | composer replacement, person info, a request accept that fails |

Both are short, sentence case, no full stop on the name. Translators: the name is a label for
a person who is no longer on Lakko, not an instruction.

## 6. The person who deletes

Unchanged screens (Settings > Account > Delete account). The device tells every
conversation it is in, then wipes itself; this takes no visible step and no extra wait beyond
the existing cleanup budget. If it cannot reach a conversation, the others still learn it the
next time they look at this person (ADR-0098 section 3).

## 7. Cross-platform shape

One state per person, `deleted` or not, decided by the shared rule in Rust
(`accountDeletionStatus`), and one display rule for usernames (`mayShowUsername`), so web,
desktop, iOS and Android cannot disagree. The person info actions come from
`deletedPersonInfoSections`. The neutral avatar is the design system's person glyph on the
neutral surface colour, the same on every client. Platform idioms (long-press, right-click,
hover) are the existing message-menu idioms; only the item list shrinks.

## 8. Data the UI needs (fed to ADR-0098)

Per person this device knows (contact, one-to-one peer, group member): the account key the
conversation resolves them to, the account id when known, whether a verified deletion
statement arrived, the last directory answer and when, when the account was last confirmed
to exist, and when a directory check was last attempted.
