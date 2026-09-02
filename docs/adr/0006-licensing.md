<!--
SPDX-License-Identifier: AGPL-3.0-or-later
SPDX-FileCopyrightText: 2026 Lakko Contributors
-->

# ADR-0006: AGPLv3 for the entire monorepo + CLA

- **Status:** Accepted — **the AGPLv3 half is implemented; the CLA half is
  not built** (see the marked sections below; recorded 2026-09-02).
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

> **Qualified 2026-09-02 (documentation audit): "every file" now has documented
> exceptions, and this ADR never mentioned them.** `crates/vendor/` holds two
> **MPL-2.0** crates — `webauthn-rs-core` (the minimal challenge-injection fork,
> [ADR-0014](./0014-webauthn-action-binding.md)) and `hpke-rs-libcrux`
> (known-gap G-23). Both are correctly mechanised: `REUSE.toml` carries
> `precedence = "override"` blocks for them and `LICENSES/MPL-2.0.txt` is
> present, so `reuse lint` passes honestly rather than by looking the other way.
> MPL-2.0 is file-level copyleft and AGPL-compatible, so nothing about the
> outbound licence changes — but the unqualified sentence above is now false,
> and a reader adding a third vendored crate needs to know the override
> mechanism exists. **Every first-party crate and service is AGPL-3.0-or-later**
> via `license.workspace = true`; that part is exactly true.

~~**A CLA (Contributor License Agreement)** is mandatory for every contributor — Apache ICLA-style. Verified by the `cla-assistant` bot on every PR. PRs without a signed CLA do not merge.~~

> **NOT BUILT (recorded 2026-09-02) — and it is in this ADR's own title.**
> There is **no CLA document** anywhere in the repository and **no
> `cla-assistant`** configuration; `.github/` contains only `workflows/`, none
> of them CLA-related, plus no `CODEOWNERS` and no PR template. The project is
> single-maintainer with no external contributors so far, so nothing has been
> merged un-covered — but the gap is not cost-free, because **the re-licensing
> option described immediately below depends entirely on this**. Without signed
> contributor grants, splitting `lakko-mls` out under Apache-2.0 or
> dual-licensing commercially would require tracking down and getting
> permission from every past contributor. **The cost of the omission grows with
> every contribution**, which is the argument for putting it in place before
> the first external PR rather than after.

The CLA gives the project maintainer the right to re-license a contribution under a different licence in the future — a critical option for:
- Splitting out a separate SDK crate (e.g. `lakko-mls`) under Apache 2.0 in Phase 4-5 if it becomes attractive to the industry
- Commercial dual-licensing for embedded / enterprise use

### Tooling enforcement

- `REUSE.toml` (FSFE REUSE 3.0) — fallback annotations for files without a header (binary fixtures, generated code)
- `reuse lint` in CI — blocks PRs without a header or with the wrong licence
- `cargo deny` — blocks deps with GPL-incompatible licences (e.g. MS-RL, EPL without explicit permission)
- ~~A pre-commit hook adds the SPDX header into new files (template per language)~~
  **Not built** (2026-09-02): there is no `.pre-commit-config.yaml` and no
  installed hook. SPDX enforcement is CI-only (`reuse lint` via
  `fsfe/reuse-action` in `.github/workflows/ci.yml`) plus the manual
  `just reuse`. The gate holds — a missing header fails the build — it just
  fails later than intended, in CI rather than at commit time.

### Third-party dependencies

`cargo deny` allow list:
- MIT, Apache-2.0, BSD-2/3-Clause, ISC, MPL-2.0, Unicode-DFS-2016, Zlib, CC0-1.0, BSL-1.0
- AGPL-compatible: AGPL-3.0+, GPL-3.0+, LGPL-3.0+

Not allowed: proprietary, GPL-2.0-only (incompatible with AGPL-3.0), CDDL, OSL-without-explicit-permission.

> **`deny.toml` is the authority and has moved since (2026-09-02).** The list
> above is the intent; the enforced list lives in
> [`deny.toml`](../../deny.toml) and now additionally allows
> `Apache-2.0 WITH LLVM-exception`, `0BSD`, `Unicode-3.0`, `OpenSSL`,
> `CDLA-Permissive-2.0` and bare `AGPL-3.0` — each with an in-file
> justification comment naming the crate that needed it. It also does **not**
> allow `GPL-3.0` in any form, and allows `LGPL-3.0-only` specifically rather
> than the `LGPL-3.0+` written here. The drift is one-directional: `deny.toml`
> grew as real dependencies arrived and this ADR did not follow. **Read
> `deny.toml` for the list; read this ADR for the policy** — the policy
> (allow-list, not deny-list; nothing AGPL-incompatible) is unchanged and every
> addition is consistent with it.

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
