<!-- SPDX-License-Identifier: AGPL-3.0-or-later -->
<!-- SPDX-FileCopyrightText: 2026 Lakko Contributors -->

# Store beta text (draft)

**Draft, 2026-11-19.** For TestFlight ("Beta App Description", external testing) and the
Google Play testing tracks (the testing description and the opt-in page). It opens with
the two sentences of the beta notice ([`ux/beta-notice.md`](ux/beta-notice.md),
`beta.notice.body`), unchanged. Before it is used: check what the beta actually contains
against [`beta-launch-checklist.md`](beta-launch-checklist.md) section 3, and remove any
line below that is not true of the build being shipped.

---

Lakko is in beta. An independent security audit is not finished yet; we are working on
it. We believe Lakko is safe to use, but we cannot prove it yet. By continuing, you
accept this risk.

Lakko is an open-source messenger that encrypts one-to-one chats, groups, channels and
calls end to end. You sign up with a username; no phone number or email address is
needed.

The beta has room for a limited number of people. Report security problems privately to
security@lakko.me. `TODO: where testers send ordinary feedback (TestFlight and Play have
their own feedback channels).`

What to test in this build:
- `TODO: fill per build (sign-up, chats, groups, calls, ...)`
