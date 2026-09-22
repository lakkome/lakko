<!-- SPDX-License-Identifier: AGPL-3.0-or-later -->
<!-- SPDX-FileCopyrightText: 2026 Lakko Contributors -->

# LAK-RC-03 — Every key-transparency, witness and attestation-pinning gate is inert in the deployed system, while the documentation describes them as built and in force

- **Severity:** High (finder's proposal; `audit-verify` calibrates)
- **Confidence:** CONFIRMED
- **Workstream:** recon
- **Tracking id:** LAK-RC-03
- **Status:** Open
- **Component:** `infra/systemd/`, `infra/scripts/deploy.sh:34`,
  `infra/nginx/sites-available/lakko.conf`,
  `apps/web/src/transparency/config.ts:12-26`,
  `.github/workflows/deploy-web.yml:58-61, 88-94`
- **Invariants touched:** #6 (identity rotation only via the KT log supersession
  proof), #4, C-56, C-57
- **Vendor claim contradicted:** `CLAUDE.md` ADR table for ADR-0019, 0020, 0036,
  0039, each described as built/landed with no indication the resulting service
  runs nowhere. Partially *conceded* by
  `docs/security/external-review-scope.md` §7.

## Preconditions

None. This is the standing state of the system. **No attacker is required for the
gates to be off; an attacker is only required to benefit from it.**

## Mechanism

**Step 1 — three services are not deployed.** `infra/systemd/` contains units for
`auth`, `channels`, `discovery`, `ds`, `gif-proxy`, `push-gw`, `storage`, `sfu`
(plus coturn and the target). `infra/scripts/deploy.sh:34` reads:

```bash
SERVICES=(auth ds storage discovery push-gw channels sfu gif-proxy)
```

`infra/systemd/lakko.target` `Wants=` exactly those eight.
`infra/nginx/sites-available/lakko.conf` proxies `/api/{auth,ds,discovery,storage,push,channels,gifs}/`
and nothing else — there is no `/transparency/` location in any vhost.
`grep -rn "transparency|witness|obfs" infra/` returns **nothing at all**.

So `services/transparency` (3,756 lines), `services/witness` (826) and
`services/obfs-proxy` (656) have no unit, no deploy entry and no route. They are
compiled by the workspace and run nowhere.

**Step 2 — the client gate is off unless pinned at build time, and fails open.**
`apps/web/src/transparency/config.ts:12-26` says so in its own words:

```
 *   * `VITE_LAKKO_TRANSPARENCY_URL` … **Unset → the whole log check is OFF.**
 *   * `VITE_LAKKO_TRANSPARENCY_OPERATOR_KEY` … **Unset → the log check is OFF**
 *
 * The skip path is deliberately fail-OPEN for availability …
```

`pinnedWitnessKeys()` (`:112-135`) returns `[]` when unset, which the same file
documents as "the witness gate is OFF (dormant, the operator-only transition
posture)".

**Step 3 — the production build supplies no pin.** Twelve distinct
`VITE_LAKKO_*` variables are read by `apps/web/src`. The count of uncommented
assignments in `apps/web/.env.example` is **zero**
(`grep -E '^VITE_LAKKO_[A-Z_]+=' apps/web/.env.example` → no output; every
occurrence is inside a comment). `.github/workflows/deploy-web.yml` declares only
`CARGO_TERM_COLOR`, `RUST_BACKTRACE`, `CARGO_INCREMENTAL` in `env:` (`:58-61`)
and its build step (`:88-94`) is bare `npm ci && npm run build`. `**/.env` is
gitignored (`.gitignore:29`), so `actions/checkout` cannot supply one either.

The CI-deployed web client therefore ships with: the transparency check off, the
witness quorum off, `VITE_LAKKO_DEVICE_ATTEST_KEY` unset — so the ADR-0015 pinned
verifying key falls back to fetching JWKS from the very serving tier the pin
exists to distrust — and the ADR-0038 release notice dormant.

**Step 4 — the server-side append gate that would prevent squatting is also off
by default.** `services/transparency/src/main.rs:287`:

```rust
let require_genesis_attest = std::env::var("LAKKO_TRANSPARENCY_REQUIRE_GENESIS_ATTEST")
    .is_ok_and(|v| matches!(v.trim(), "1" | "true" | "TRUE" | "yes" | "on"));
```

Default `false`. Moot while the service does not run, but it means the gate would
still be off on the day it is first deployed.

**Step 5 — nothing asserts any of this at build or boot.** There is no
production-mode check that fails a release build with unset pins, and no startup
refusal. The failure is silent in both directions.

## Reproduction

```
git grep -l transparency infra/                     # → no output
grep -n SERVICES= infra/scripts/deploy.sh           # → 34: SERVICES=(auth ds storage discovery push-gw channels sfu gif-proxy)
grep -E '^VITE_LAKKO_[A-Z_]+=' apps/web/.env.example # → no output
sed -n '58,94p' .github/workflows/deploy-web.yml     # → no VITE_* anywhere
```

## Impact

Invariant #6 states *"Identity rotation — only via a KT log supersession proof.
Old keys do not come back."* In the deployed system the KT log plays no part in
that sentence. What actually enforces identity continuity is the client-side
supersession pin gate (`apps/web/src/contacts/client.ts:1304-1347`), which is
genuinely fail-closed **on a changed key** — and which is plain TOFU on a first
observation, by design.

The consequence, stated precisely: **a malicious or compromised server can serve
Alice a key of its choosing for Bob on Alice's first contact with Bob, and nothing
in the deployed system will ever contradict it.** The mechanism built to
contradict it — an append-only log, an operator signature, a witness consistency
proof — is present in the repository, tested, and running nowhere. The only
remaining check is a user comparing a safety number out of band, and the UI does
not tell the user that this is the only thing standing there.

C-57 (witness co-signing, split-view resistance) and ADR-0039's K-of-N quorum are
inert for the same reason, and ADR-0039 already states the honest version of this
for its own scope: the mechanism is real, the resistance it buys is a *deploy*
property. The same sentence is true of the whole transparency subsystem and is
not said anywhere a reader of `CLAUDE.md` would find it.

The bounded part: this does not by itself let the server read traffic between two
users who have already established pins, and an established pin genuinely does
fail closed on an unproven change.

## Recommendation

**Make the posture legible before making it stronger.** Two changes, in this
order:

1. **Say it in the documents a reader actually reads.** `CLAUDE.md`'s ADR table
   and the security docs should carry a deployment column, or a single
   prominent statement, distinguishing *built* from *running*. The scope brief §7
   already gets this right; the primary documents do not, and they are the ones a
   contributor and a reviewer open first. This costs nothing and removes the
   largest single source of false confidence in the corpus.
2. **Make the client state its own posture to the user.** The client already
   computes `"off"` as a distinct log verdict and records it
   (`client.ts:1617`). Surfacing "this conversation's key has not been checked
   against a transparency log" — once, in the safety-number UI — turns an
   invisible default into an informed one.

**Then close it:** deploy `transparency` and at least one independently operated
`witness`, pin the operator and witness keys into the client build, and add a
build-time assertion so a production build with unset pins fails rather than
silently shipping the fail-open path. Until an independent witness exists,
ADR-0039's own caveat governs what the deployment is worth.

## Notes

- `docs/security/external-review-scope.md` §7 states this accurately and in
  advance. Per severity rule 4, it is filed anyway at the severity the mechanism
  earns, and the report should record it as *conceded by the vendor* rather than
  as *newly discovered*. The disagreement is not about the fact; it is about where
  the fact is written and what a reader of the other documents concludes.
- The manual deploy path `just deploy-web` (`justfile:798`) builds on a developer
  machine where an untracked `.env` could supply the pins. **I cannot observe that
  file and I do not claim to know what the live site was built with.** That
  unobservability is part of the finding: a security-critical build input with no
  tracked source of truth, no assertion and no runtime signal.
- The obfs-proxy half is a different severity question — it is an availability and
  censorship-resistance feature, not a confidentiality one — and I have folded it
  in here only because the root cause (built, documented as landed, deployed
  nowhere) is identical. A verification pass may prefer to split it out.
- I did **not** check whether the iOS and Android clients have the same
  default-off pin behaviour. `CLAUDE.md` describes the same Info.plist /
  `BuildConfig` pin pattern with empty defaults, and the web evidence is
  conclusive on its own, but the native half is asserted by documentation here,
  not measured.
