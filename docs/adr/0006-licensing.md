<!--
SPDX-License-Identifier: AGPL-3.0-or-later
SPDX-FileCopyrightText: 2026 Lakko Contributors
-->

# ADR-0006: AGPLv3 for the entire monorepo + CLA

- **Status:** Accepted
- **Date:** 2026-04-28
- **Context:** Phase 0

## Context

Lakko is a security-critical E2EE messenger. The licence has to:

1. Protect users from a closed-source fork with a backdoor (the "took our code, added a backdoor, sold it as secure" attack)
2. Not get in the way of audit transparency or open-source participation
3. Be compatible with the App Store / Play Store
4. Be simple to manage compliance for in a monorepo (one codebase, one LICENSE)
5. Preserve the option of commercial dual-licensing in the future

The repo is a monorepo (see CLAUDE.md), AI-agent-driven workflow.

## Decision

**AGPLv3 for all code in the monorepo.** A single `LICENSE` at the root (full text of AGPL-3.0). The SPDX header `AGPL-3.0-or-later` is mandatory in every file.

**A CLA (Contributor License Agreement)** is mandatory for every contributor — Apache ICLA-style. Verified by the `cla-assistant` bot on every PR. PRs without a signed CLA do not merge.

The CLA gives the project maintainer the right to re-license a contribution under a different licence in the future — a critical option for:
- Splitting out a separate SDK crate (e.g. `lakko-mls`) under Apache 2.0 in Phase 4-5 if it becomes attractive to the industry
- Commercial dual-licensing for embedded / enterprise use

### Tooling enforcement

- `REUSE.toml` (FSFE REUSE 3.0) — fallback annotations for files without a header (binary fixtures, generated code)
- `reuse lint` in CI — blocks PRs without a header or with the wrong licence
- `cargo deny` — blocks deps with GPL-incompatible licences (e.g. MS-RL, EPL without explicit permission)
- A pre-commit hook adds the SPDX header into new files (template per language)

### Third-party dependencies

`cargo deny` allow list:
- MIT, Apache-2.0, BSD-2/3-Clause, ISC, MPL-2.0, Unicode-DFS-2016, Zlib, CC0-1.0, BSL-1.0
- AGPL-compatible: AGPL-3.0+, GPL-3.0+, LGPL-3.0+

Not allowed: proprietary, GPL-2.0-only (incompatible with AGPL-3.0), CDDL, OSL-without-explicit-permission.

## Alternatives

### MIT / Apache 2.0 for everything

Maximum ecosystem benefit, but **no protection** against a closed fork with a backdoor. Wire was originally Apache and switched to GPLv3 for exactly this reason.

**Rejected:** unacceptable for a security-critical messenger. This is a concrete risk, not a theoretical one.

### Hybrid: Apache 2.0 on crypto / MLS / SDK + AGPLv3 on app / server

Element / Matrix style. Pros: the SDK can be embedded into OSS projects; AGPLv3 protects the product layer. Cons: compliance overhead in a monorepo (per-file REUSE; the dependency direction must be Apache ← AGPL only; cargo-deny gate; constant discipline).

**Rejected for Phase 0-3:** at this stage there is no real SDK consumer; the compliance overhead outweighs the speculative benefit. The CLA preserves the option of splitting the SDK crate out under Apache in Phase 4-5 without re-soliciting contributor signatures.

### Pure AGPL without a CLA (DCO Linux-style only)

Simple. Cons: it is impossible to change the licence in the future without every contributor's consent → a trap.

**Rejected:** the CLA gives us the flexibility we need. DCO is enough for projects with a permanent licence; Lakko may want commercial dual-licensing.

### GPLv3 (without the Affero clause)

Protects against closed binary distribution, but **not** against the SaaS loophole (an adversary hosting a proprietary fork as a service without publishing it). Unacceptable for server-side code.

**Rejected:** AGPLv3 closes the SaaS loophole.

## Consequences

### Positive

- A closed-source backdoored fork cannot legally be distributed
- A SaaS deployment of a proprietary fork must publish its modifications
- One LICENSE = trivial compliance in a monorepo (vs hybrid)
- The CLA preserves flexibility for future re-licensing
- A strong stance for community building — "we are on the user's side"
- App Store / Play Store are compatible with AGPL (FSF confirmation, precedents from Element X, Threema)

### Negative

- Fewer potential commercial SDK consumers (cannot embed without AGPL obligations)
- The CLA is administrative overhead (cla-assistant bot, rejected PRs without CLA)
- Some contributors will refuse to sign a CLA on principle → loss of part of the community

### Neutral

- AGPL-only code cannot be used by academic works under Apache / MIT (but they can accept AGPL for their fork)

## Open questions

- The legal entity of the project (copyright holder, CLA assignee) — Switzerland or Estonia? Decide by Phase 1; for now a temporary "Lakko Contributors"
- Apache ICLA vs other CLA formats — final choice after legal review
- In Phase 4-5: consider splitting `lakko-mls` out under Apache 2.0 into a separate sub-repo if the industry asks for it

## References

- [GNU Affero General Public License v3.0](https://www.gnu.org/licenses/agpl-3.0.html)
- [REUSE Specification 3.0](https://reuse.software/spec/)
- [Apache ICLA template](https://www.apache.org/licenses/contributor-agreements.html)
- [cla-assistant](https://github.com/cla-assistant/cla-assistant)
- ADR-0003 — LiveKit fork (the need for AGPL is explained there)
