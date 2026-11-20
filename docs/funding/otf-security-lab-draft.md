<!-- SPDX-License-Identifier: AGPL-3.0-or-later -->
<!-- SPDX-FileCopyrightText: 2026 Lakko Contributors -->

# DRAFT — application to the OTF Red Team Lab / Security Lab

**Status: draft, not submitted.** Written 2026-11-18 without the current OTF form in
hand. Before submitting: open the live application form, map each section below to its
fields, check the current eligibility rules, and fill every `TODO`.

---

## Project name

Lakko

## Applicant

VVG (Dutch sole proprietorship, KvK `TODO`), owner and sole developer Valentin Goncharov,
the Netherlands. Contact: info@lakko.me.

## One-sentence summary

Lakko is an open-source (AGPLv3) messenger that encrypts every conversation type — one-to-one
chats, groups, channels and calls — with MLS (RFC 9420), runs against a server designed to
learn neither message content nor the social graph, and carries censorship-resistant
transports for people in countries that block messengers.

## The problem

People in countries that block or surveil messengers need a tool that is (1) end-to-end
encrypted for groups and calls, not only one-to-one, (2) usable without a phone number,
(3) able to reach its server when ordinary connections are blocked, and (4) open so its
claims can be checked. Existing tools each meet some of these. Lakko is built to meet all
four from one codebase.

Lakko is for anyone who wants to talk privately and decide for themselves who can reach
them. Every conversation is end-to-end encrypted. A person chooses who can find them by
username or by name (nobody, contacts of contacts, members of their groups and channels,
or everyone); someone outside that circle can reach them only through an invitation link
they shared. A conversation with a new person starts only through such a link or when
they accept that person's contact request.
The same design matters most to people for whom an ordinary messenger is risky or
blocked: they get the same protection, without a phone number, and with transports
that reach the server where messengers are blocked.

## What exists today

- One shared Rust core used by native iOS and Android clients, a web client, and a desktop
  client (Tauri) in progress.
- MLS (RFC 9420, via OpenMLS) for one-to-one chats, groups, channels and multi-device;
  SFrame (RFC 9605) for call media under keys from an MLS exporter; a Rust SFU that relays
  sealed media it cannot read.
- A server side that stores no phone numbers or email addresses by default: OPAQUE
  password login, OPRF-blinded username lookup, sealed-sender mailboxes, anonymous
  rate-limit tokens, call admission without an account identifier, a key transparency log
  with witness co-signing.
- Censorship-resistant transports (REALITY, Shadowsocks-2022, WebTransport) in the native
  clients, reached since 2026-10-30 through a relay on our test server.
- Engineering practice: architecture decision records for every design choice
  (`docs/adr/`), an indexed list of security invariants and known gaps
  (`docs/security/`), property tests, fuzz targets on every parser that reads
  untrusted input, an internal security review whose findings are recorded with
  their fixes.
- Status: public beta, capped at 1000 users, marked as unaudited. `TODO: number of users,
  countries, date of launch at the time of submission.`

## What we ask the Lab for

An independent security review. The scope is already written:
`docs/security/external-review-scope.md` in the repository — boundaries, measured code
sizes, a reading order, what has already been verified, the questions we most want
answered, and what a reviewer must not assume.

We ask for it in parts, so the most important part can start first:

1. **Part 1 (requested now): the MLS group-membership state machine and the cryptographic
   core.** The proposal / commit authorisation rule and its three enforcement layers, the
   staged commit and its rollback, committer election, role tables, external join,
   multi-device leaves, and the constructions they rest on: call media keys
   (MLS exporter to SFrame), attestations, key transparency proofs, encrypted backup.
   Sections 2.1 and 2.3 of the scope document.
2. **Part 2 (later): the three client implementations** of the same ordering and
   rollback rules (web, iOS, Android) and their key storage. Section 2.2.
3. **Part 3 (later, separately): the censorship-resistant transports**, ideally with a
   censorship-lab component.

The sizes in the scope document were measured on 2026-09-02 and will be re-measured at
the commit we freeze for the review. `TODO: re-measure before submitting.`

## Why now

Lakko's own rule is that it does not leave the beta, lift the 1000-user cap or list in an
app store for production until this review is done (ADR-0046). The review is the one step
between the beta and a release people at risk can rely on. Until then the beta says
plainly that it has not been audited and is not promoted to people at risk.

## What we commit to after the review

- Publish the report, after the fixes, in the repository.
- Fix every finding rated medium or above before the beta leaves its cap, and record each
  fix against the finding.
- Ask the reviewer for a retest of the fixes.

## Risks and how we handle them

- **One developer.** The design, its reasons and its open problems are written down in
  the repository (ADRs, invariant index, known gaps), so the work is reviewable and
  transferable. There is no second maintainer today; an external review is the
  independent check the project does not otherwise have.
- **Scope.** The project is large. The review is asked for in parts, starting with the
  part everything else depends on.

## Links

- Website: https://lakko.me
- Repository: https://github.com/lakkome/lakko (public)
- Scope document: https://github.com/lakkome/lakko/blob/main/docs/security/external-review-scope.md
- Known gaps: `docs/security/known-gaps.md`
- Funding manifest: `funding.json`

## Facts to collect before submitting

- [ ] Current OTF Lab form, eligibility and whether a sole proprietor in the EU may apply
- [ ] KvK number (public contact email: info@lakko.me)
- [ ] Beta user count, launch date, countries (only what you can show)
- [x] Repository public (2026-11-20)
- [ ] `SECURITY.md` on the public `main` branch
- [ ] Re-measured code sizes at a frozen commit
- [ ] Any partner organisation willing to write a support letter
