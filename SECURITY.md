<!-- SPDX-License-Identifier: AGPL-3.0-or-later -->
<!-- SPDX-FileCopyrightText: 2026 Lakko Contributors -->

# Security policy

## Status

Lakko is in a **public beta, capped at 1000 accounts. Its cryptography has not been
independently audited yet** ([ADR-0046](docs/adr/0046-external-crypto-review.md)). Do not
rely on it where your safety depends on it. The issues we already know about are listed in
[`docs/security/known-gaps.md`](docs/security/known-gaps.md); the design and its claimed
properties are in [`docs/security/`](docs/security/README.md).

## Reporting a vulnerability

Report privately. **Do not open a public issue, pull request or discussion.**

- **GitHub:** use "Report a vulnerability" on the repository's Security tab
  (private vulnerability reporting).
- **Email:** [security@lakko.me](mailto:security@lakko.me). There is no encryption key
  for this address yet; for details you would not send in plain email, use the GitHub
  report.

Please include:

- what an attacker can do, and what they need first (an account, a member of the group,
  control of a server, a network position);
- the component (a crate, service, client and platform) and the commit or build you
  tested;
- steps or code to reproduce it;
- whether you have told anyone else.

Write in English or Russian.

## What happens next

Lakko is maintained by one person, so these are targets, not guarantees:

- an acknowledgement within 7 days;
- a first assessment (whether it is a vulnerability and how severe) within 30 days;
- a fix, or an agreed plan, before anything is published. We aim to publish within 90 days
  of the report, together with you, and earlier once a fix has shipped.

We record each accepted report and its fix in the repository. If you want, we credit you
by name. There is no bug bounty.

## Scope

In scope:

- the code in this repository: the Rust core (`crates/`), the services (`services/`), the
  clients (`apps/`) and the deployment configuration (`infra/`);
- the claims in `docs/security/`: a way around one of the
  [security invariants](CLAUDE.md#security-invariants--must-not-be-violated), or a new
  path to a known gap, is a report we want;
- the beta servers at `lakko.me`, tested only with accounts you own.

Out of scope:

- anything already listed in `docs/security/known-gaps.md`, unless you found a new way to
  reach it or it is worse than described there;
- volume denial of service, spam, and social engineering of people or providers;
- a vulnerability in a dependency that does not affect Lakko (report it upstream; tell us
  if it does affect Lakko).

## Testing rules

We will not take legal action over research done in good faith within these rules:

- use your own accounts and devices; do not read, change or delete anyone else's data;
- prefer a local deployment (see `CLAUDE.md`, "Local dev") to the beta servers;
- do not degrade the beta servers for other people, and stop as soon as you reach data
  that is not yours;
- give us a reasonable time to fix the issue before you publish.

## Supported versions

Only the current beta build and the `main` branch receive security fixes.
