<!-- SPDX-License-Identifier: AGPL-3.0-or-later -->
<!-- SPDX-FileCopyrightText: 2026 Lakko Contributors -->

# LAK-RC-09 — A transparency-log key mismatch is reported only to the developer console, and does not block the operation

- **Severity:** Medium (finder's proposal; `audit-verify` calibrates)
- **Confidence:** CONFIRMED
- **Workstream:** recon
- **Tracking id:** LAK-RC-09
- **Status:** Open
- **Component:** `apps/web/src/contacts/client.ts:1608-1647`,
  `apps/web/src/contacts/log-posture.test.ts:10-13`
- **Invariants touched:** #6, #4, C-56
- **Vendor claim contradicted:** none — the posture is deliberate and pinned by a
  test. This is filed as a disagreement with an accepted design decision, per
  severity rule 4.

## Preconditions

For the code path: a transparency log must be configured and reachable, which
today it is not (**LAK-RC-03**). This finding therefore describes what happens
*when the log is switched on* — it is about the value of the control the vendor
is planning to enable, not about today's deployed state.

For the attack it fails to stop: a malicious or compromised discovery tier
serving a key that the log does not attest.

## Mechanism

`apps/web/src/contacts/client.ts:1607-1610` — one verdict is fatal:

```ts
    // Rollback is the one hard failure (unambiguous attack).
    if (status === "rollback") {
      throw new LogRollbackDetected(accountIdB64);
    }
```

`:1628-1637` — the verdict that means *the served key is not the logged key* is
not:

```ts
    if (status === "mismatch") {
      // Surfaced-but-not-blocked for S3c. The natural fail-closed
      // refinement (hard-block on mismatch) is deferred to a
      // lakko-security ruling — see the method doc + the S3c report.
      // eslint-disable-next-line no-console
      console.warn(
        `ContactsClient: transparency-log MISMATCH for account ` +
          `${accountIdB64} — the log's latest key differs from what ` +
          `discovery served (or the inclusion proof failed). Proceeding ` +
          `for S3c (supersession/pin gate is primary); flag for review.`,
      );
```

`console.warn` is the entire user-facing consequence. The verdict is persisted
next to the pin (`recordLogStatus(accountIdB64, status)` at `:1616`), so the
information exists in the client's own store — it simply never reaches a person.
Grepping the web tree for a consumer of that recorded status outside the
contacts client and its tests returns nothing that renders it.

The posture is intentional and pinned. `log-posture.test.ts:10-13`:

```
 *   * `"rollback"`  → HARD FAIL (`LogRollbackDetected`).
 *   * `"mismatch"`  → recorded + surfaced, does NOT block.
 *   * `"absent"` / `"unreachable"` / `"off"` → recorded, does NOT block.
```

"Surfaced" in that test comment means surfaced *to the console*.

**Why the fallback argument does not fully hold.** The stated justification is
that the supersession pin gate is primary and fail-closed, so the log is additive.
That is true for a *changed* key: gate 1 runs first (`:1316-1332`) and throws
`SupersessionRefused` before the log check is reached. It is **not** true for a
**first** observation, which is TOFU — gate 1 pins whatever it is served and does
not refuse. So in the one case where an attacker's substituted key would actually
be accepted, the log's mismatch verdict is the only contradicting signal in the
system, and it goes to `console.warn`.

## Reproduction

Requires a configured transparency log (absent today) and a discovery response
whose `signature_pubkey` differs from the logged leaf for that account, on a first
contact. Not reproducible in this engagement — no deployed transparency service
and a read-only mandate. The control flow above is settled by the code.

## Impact

The key-transparency subsystem — `crates/lakko-transparency` (2,777 lines), two
services, an operator key, witness co-signing, a K-of-N quorum, and eight fuzz
targets — exists to produce exactly one signal: *the key you were served is not
the key the log commits to*. When that signal fires on the case that matters
most, it is written to a console no user opens.

Bounded honestly:

- This is not a *missing detection*. The detection works; the routing does not.
- On a *changed* key the primary gate already fails closed, so the exposure is
  confined to first observation and to a proof-verification failure.
- The safety-number comparison remains available as an out-of-band check, and is
  what the threat model ultimately relies on.

But the composition is what earns the severity: with the log off (LAK-RC-03) there
is no signal; with the log on, the signal exists and is discarded. There is no
configuration in which a user learns that their correspondent's key is disputed.

## Recommendation

**Route the verdict to the safety-number UI.** The client already persists
`logStatus` on the pin record; rendering it where the user verifies a contact is a
presentation change, not a protocol change, and it converts a dead signal into the
thing the subsystem was built for. A `mismatch` should read as an explicit warning
with the out-of-band-verification prompt, not as a neutral state.

**Then decide the blocking question, which the comment defers.** Two defensible
positions, and the trade-off is real:

- *Block on mismatch at first observation.* This is the fail-closed reading and it
  matches invariant #4's posture elsewhere. Its cost is that a log that is stale,
  partitioned or mid-reconcile would refuse legitimate first contacts — and
  `services/discovery/src/transparency_log.rs:27-38` describes the append as
  best-effort with a reconciliation sweep, so a legitimately-lagging log is a
  normal state, not an exceptional one. Blocking without fixing that would make
  first contact unreliable.
- *Warn loudly and proceed.* Keeps availability, and is defensible **only if the
  warning reaches the user**, which today it does not.

Either is better than the current state, which has the availability cost of
neither and the security benefit of neither.

## Notes

- Filed per severity rule 4: the vendor has explicitly deferred this
  ("deferred to a `lakko-security` ruling") rather than overlooked it, and the
  report should say so. The disagreement is narrow — not with the decision to
  proceed, but with the decision to proceed *silently*.
- `"absent"` and `"unreachable"` are treated the same way and I do not dispute
  that: an eventually-consistent log is legitimately often absent, and warning on
  it would train users to ignore the warning. `"mismatch"` is categorically
  different, because no benign process produces it.
- I did **not** check the iOS and Android equivalents of this path. The web client
  is the reference implementation for this arc, so the natural assumption is that
  they mirror it, but that is an assumption and not a measurement.
- I did not review the `verifyRotationChain` / `verifyIdentityInLog`
  implementations in `apps/web/src/transparency/verify.ts`, so I make no claim
  about whether the `"mismatch"` verdict is correctly *computed* — only about what
  is done with it.
