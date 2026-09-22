<!-- SPDX-License-Identifier: AGPL-3.0-or-later -->
<!-- SPDX-FileCopyrightText: 2026 Lakko Contributors -->

# LAK-RC-08 — Database-backed security tests report success without executing when no test database is configured

- **Severity:** Medium (finder's proposal; `audit-verify` calibrates)
- **Confidence:** CONFIRMED
- **Workstream:** recon
- **Tracking id:** LAK-RC-08
- **Status:** Open
- **Component:** `services/auth/src/device_authorize.rs:481-486` (representative),
  and 22 further sites across `services/{auth,transparency,discovery,witness}`
- **Invariants touched:** none directly; this is a control-integrity defect
- **Vendor claim contradicted:** none explicitly — but every "verified firsthand"
  and "N tests green" claim in `CLAUDE.md` for the ADR-0014, 0018, 0019, 0020,
  0036, 0037, 0039 and 0049 arcs is a claim about tests in this set

## Preconditions

None. It is the default on any machine without the per-service test database
environment variables set — which includes every GitHub Actions runner, and
includes `ci.yml` itself.

## Mechanism

The pattern, at `services/auth/src/device_authorize.rs:478-486`:

```rust
#[tokio::test]
#[allow(clippy::too_many_lines)]
async fn device_authorize_grant_round_trip() {
    let Ok(url) = std::env::var("LAKKO_AUTH_TEST_DATABASE_URL") else {
        eprintln!(
            "skipping device_authorize_grant_round_trip: LAKKO_AUTH_TEST_DATABASE_URL unset"
        );
        return;
    };
```

The test **returns `Ok`**. It is not `#[ignore]`d, so it is not reported as
ignored; it is counted as a pass. `cargo test` prints the `eprintln!` only with
`--nocapture`, so in ordinary output the line is invisible and the summary reads
identically to a real run.

23 test bodies use this shape, distributed
`services/auth` 8, `services/transparency` 8, `services/discovery` 2,
`services/witness` 1, plus parameterised helpers in `auth/backup_head.rs:539` and
`auth/throttle.rs:422`. Named:

```
action_grant_enforces_passkey_delete          append_assigns_monotonic_version_per_username
append_is_idempotent_for_same_pubkey          append_verifies_genesis_attestation
append_verifies_leaf_authorisation            cosign_accepts_extension_refuses_fork
device_authorize_grant_round_trip             device_request_round_trip
genesis_attest_stored_preserved_and_replaced  latest_endpoint_returns_highest_version_and_404s_unseen
latest_returns_highest_version_entry          reconciler_scan_selects_unlogged_and_stale_only
recovery_contact_full_flow                    reload_from_db_rebuilds_identical_root
totp_enroll_and_state_machine                 vault_state_machine
witness_high_water_persist_load_round_trips
```

Read that list against what it covers: the WebAuthn action-binding grant that
stops a stolen token deleting a passkey (ADR-0014), the device-authorisation grant
redemption (ADR-0018 factor a), the TOTP state machine and its lockout
(ADR-0018 c), the recovery-contact flow (ADR-0018 d), the word vault and its
guess budget (ADR-0049), the KT-log leaf-authorisation and genesis-attestation
append gates (ADR-0019, ADR-0036), the witness fork refusal (ADR-0020), and the
Merkle root reload. These are the highest-value server-side security tests in the
repository, and they are precisely the set that silently no-ops.

The pattern is deliberate and documented — `device_authorize.rs:369-372` says
*"Gated on `LAKKO_AUTH_TEST_DATABASE_URL` — skips (does not fail) when no …"* —
so this is a design choice, not an oversight. The defect is what the choice
produces downstream.

## Reproduction

```
# with no LAKKO_*_TEST_DATABASE_URL set:
cargo test -p lakko-svc-auth 2>&1 | grep -E '^test result:'
#   reports passes, including device_authorize_grant_round_trip

cargo test -p lakko-svc-auth -- --nocapture 2>&1 | grep '^skipping'
#   the same run, now showing the tests that did nothing
```

Measured this session on the full workspace: **2,771 passed, 0 failed, 14
ignored** — with no test database configured on this machine, so up to 23 of those
2,771 "passes" executed no assertion.

## Impact

Two distinct harms.

**A false signal to anyone reading the number.** "2,771 tests pass" is the
sentence a maintainer, a contributor and an auditor use to decide how much to
trust a change. It overstates coverage by exactly the set above, and the
overstatement is concentrated in the security-critical server paths rather than
spread evenly. The 14 genuinely-`#[ignore]`d tests, by contrast, are honest: they
are reported as ignored and each carries a reason string. The contrast shows the
project knows how to express "did not run" and did not use it here.

**A regression can land unnoticed.** Combined with **LAK-RC-07** — `ci.yml` is
`workflow_dispatch`-only, and when dispatched runs on a runner with no MariaDB —
there is no configuration in which these tests currently execute except a
developer's local box with the env var deliberately set. A change that breaks the
single-use grant consumption, the TOTP replay window or the append gate would
produce a green run everywhere.

**What this is not.** It is not evidence that any of these tests currently fails.
I did not stand up a test database and run them, so I make no claim either way
about their present state.

## Recommendation

**Make the skip visible in the summary.** Two standard options:

1. Mark them `#[ignore = "requires LAKKO_AUTH_TEST_DATABASE_URL"]`, matching what
   the project already does correctly for its 14 docker/network-gated tests. They
   then appear in the `ignored` count, and `--ignored` runs them. This is the
   smallest change and it makes the number honest immediately.
2. Better: **fail when the variable is unset unless an explicit opt-out is set**
   (e.g. `LAKKO_ALLOW_SKIP_DB_TESTS=1`). A test that cannot run should be loud by
   default and quiet only when someone has said so. This inverts the current
   default, which is quiet by default and loud never.

**And run them somewhere.** A MariaDB service container in a GitHub Actions job is
a handful of lines and would move the entire set from never-executed to
executed-per-run. Paired with re-enabling automatic triggers (LAK-RC-07), it
closes both halves.

## Notes

- The 14 `#[ignore]`d tests are a separate and well-handled category — docker
  compose stacks, live DoH, a live SFU — each with an explicit reason. Nothing in
  this finding applies to them.
- `services/push-gw`, `services/storage`, `services/channels` and `services/ds`
  have no tests of this shape.
- I counted 23 by grepping for the `"skipping "` string in test bodies and
  deduplicating by test name to 17 distinct named tests plus parameterised
  helpers. The exact figure depends on how the two helper functions in
  `backup_head.rs` and `throttle.rs` are counted; the set of *named* tests
  affected is the 17 listed above, and that list is what matters.
