<!-- SPDX-License-Identifier: AGPL-3.0-or-later -->
<!-- SPDX-FileCopyrightText: 2026 Lakko Contributors -->
<!-- GENERATED SPEC (codespeak) for crates/lakko-anon-cred/src/relations.rs — behavioral contract, reviewer-facing. Keep in lock-step with the module; the spec-drift gate fails CI if relations.rs changes and this file does not. -->

# `relations` — the three proven relations and their transcripts (spec)

**Module:** `crates/lakko-anon-cred/src/relations.rs`
**One line:** The single definition of what each proof proves and what it is bound to, used by both the prover and the verifier.

## Why this module exists

If the client and the service each described a relation, a one-term difference would be a silent soundness hole or a permanent verification failure. Both sides build the statement and the transcript from these functions.

## The construction (CMZ14 MAC_GGM, two scalar attributes)

Generators `G`, `H`. Issuer key per period `(x0, x0t, x1, x2)`, public `C_x0 = x0*G + x0t*H`, `X1 = x1*H`, `X2 = x2*H`. Attributes: `s` (hidden) and `p` (the period, public). MAC: `(U, U')` with `U' = (x0 + x1*s + x2*p) * U`.

## Relations

- **Commitment** (registration and issuance request): witness `[s]`, `C = s*G`.
- **Issuance** (issuer to client): witness `[x0, x0t, x1, x2]`; `C_x0 = x0*G + x0t*H`, `X1 = x1*H`, `X2 = x2*H`, `U' = x0*U + x1*S + x2*P` with `S = s*U` and `P = p*U`. The issuer computes `S = b*C` (it never knows `s`), the client computes `S = s*U`; both equal because `U = b*G`.
- **Presentation** (holder to verifier): witness `[s, z1, -z0]`; `C_s = s*U~ + z1*H`, `V = z1*X1 + (-z0)*G`, `T = s*H_poll`. The verifier computes `V = x0*U~ + x1*C_s + x2*p*U~ - C_U'` from its secret key; for a valid MAC this equals the holder's `z1*X1 - z0*G`, and a proof for any other `V` cannot be made. The first and third equations share `s`, which ties the tag to the credential's attribute.

## Transcripts

Protocol labels `lakko/anon-cred/v1 commitment-registration`, `... issuance-request`, `... issuance-response`, `... presentation`. Bound inputs:
- registration: `C`;
- issuance request: period and `C_x0, X1, X2`, then `C`;
- issuance response: period, issuer key, `C`, `U`, `U'`;
- presentation: period, issuer key, the poll scope (`channel_id || post_cursor_be`), the caller's message, `U~`, `C_s`, `C_U'`, `T`.

The statement (every base and left-hand side) is appended by `proof` after these, so `V`, `S`, `P` and `H_poll` are bound too.

## Tag base

`H_poll = hash_to_group("tag base", "lakko/channel-poll/tag-base/v1" || channel_id || post_cursor_be)` - the ADR-0090 4.3 input under this crate's hash-to-group.

## What this module does NOT do

- It holds no secrets and makes no decisions; `client`, `issuer` and `verifier` call it.

## References

- ADR-0090 4.2, 4.3; CMZ14 sections 4.1-4.2 (MAC_GGM, Issue, Show).
