// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * ADR-0065 "Phase 2 revised: silent continuity", R.2 - the acceptance rule
 * for a contact's account-key RESET, as one pure decision.
 *
 * A reset is a key change with NO supersession proof (with one, ADR-0016's
 * path runs and this module is never asked: a proven rotation is not a
 * reset). It is accepted, silently, when the directory serves an
 * auth-signed key-reset attestation for exactly this account and this key,
 * at a generation strictly above the one this device last accepted, and the
 * key log (when one is configured) agrees. Anything negative is the
 * existing fail-closed refusal; anything not yet known is no verdict at
 * all.
 *
 * Free of wasm, storage and the network so vitest can run the whole table;
 * `contacts/client.ts` gathers the inputs and acts on the outcome. iOS and
 * Android mirror this table exactly (R.12).
 */

/** What the caller learned by running `verifyKeyResetAttestation` over the
 *  bundle's `key_reset_attest` under the trusted attest key. */
export type AttestCheck =
  /** The bundle carries no attestation. */
  | { readonly kind: "absent" }
  /** It carries one and the verifier refused it (signature, account
   *  binding, key binding), or it is not decodable. */
  | { readonly kind: "invalid" }
  /** No attest key could be obtained (no build-time pin and the JWKS fetch
   *  failed): nothing can be said about the attestation yet. */
  | { readonly kind: "no-key" }
  /** It verified for this account id and the fetched key. */
  | { readonly kind: "verified"; readonly keyGeneration: number; readonly signedAtMs: number };

/** The verified key-log answer for the fetched key (`verifyIdentityInLog`). */
export interface LogCheck {
  readonly status: "confirmed" | "absent" | "rollback" | "mismatch" | "unreachable" | "off";
  /** The log's `is_reset` marker when a latest entry was read. */
  readonly isReset: boolean | null;
}

/** Which refusal copy the action shows (`contacts.identity_changed.*`). */
export type ResetRefusal =
  /** The key log shows the new key and does NOT record a reset. */
  | "unexplained"
  /** Everything else that is negative. */
  | "unconfirmed";

export type ResetVerdict =
  | { readonly kind: "accepted"; readonly keyGeneration: number; readonly signedAtMs: number }
  | { readonly kind: "refused"; readonly why: ResetRefusal }
  | { readonly kind: "no-verdict" };

/** The facts of one fetched bundle the rule reads. */
export interface ResetVerdictInput {
  /** The key this device HOLDS for the account (the pin, or a roster's
   *  account key for a member it never pinned), standard base64. */
  readonly heldKeyB64: string;
  /** The generation this device last accepted for the account (the
   *  bundle's generation at the first pin); 0 when none was recorded. */
  readonly acceptedGeneration: number;
  /** The fetched bundle's key and generation. */
  readonly fetchedKeyB64: string;
  readonly bundleGeneration: number;
  readonly attest: AttestCheck;
  readonly log: LogCheck;
}

/** The refusal copy for a negative verdict: `unexplained` only when the log
 *  itself, read to the end, shows the key without a reset. */
function refusalFor(log: LogCheck): ResetVerdict {
  return {
    kind: "refused",
    why: log.status === "confirmed" && log.isReset === false ? "unexplained" : "unconfirmed",
  };
}

/**
 * R.2, in the order the checks decide:
 *
 * | # | attestation | generation                         | key log                         | verdict                 |
 * |---|-------------|------------------------------------|---------------------------------|-------------------------|
 * | 1 | absent      | -                                  | any                             | refused                 |
 * | 2 | invalid     | -                                  | any                             | refused                 |
 * | 3 | no-key      | -                                  | any                             | no-verdict              |
 * | 4 | verified    | != bundle's, or <= accepted        | any                             | refused                 |
 * | 5 | verified    | ok                                 | confirmed, `is_reset` false     | refused (`unexplained`) |
 * | 6 | verified    | ok                                 | mismatch / rollback             | refused                 |
 * | 7 | verified    | ok                                 | unreachable / absent            | no-verdict              |
 * | 8 | verified    | ok                                 | confirmed + `is_reset`, or off  | accepted                |
 *
 * A refusal's copy is `unexplained` exactly when the log is `confirmed`
 * without `is_reset`, `unconfirmed` otherwise. Row 8's `off` arm is decision
 * D3 ({@link logOffAcceptsReset}).
 *
 * Callers must not ask for an unchanged key; one that does is answered
 * `no-verdict` (nothing to accept, nothing to refuse).
 */
export function resetVerdict(input: ResetVerdictInput): ResetVerdict {
  if (input.fetchedKeyB64 === input.heldKeyB64) return { kind: "no-verdict" };
  const attest = input.attest;
  if (attest.kind === "absent" || attest.kind === "invalid") return refusalFor(input.log);
  if (attest.kind === "no-key") return { kind: "no-verdict" };
  if (
    !Number.isSafeInteger(attest.keyGeneration) ||
    attest.keyGeneration !== input.bundleGeneration ||
    attest.keyGeneration <= input.acceptedGeneration
  ) {
    return refusalFor(input.log);
  }
  const accepted: ResetVerdict = {
    kind: "accepted",
    keyGeneration: attest.keyGeneration,
    signedAtMs: attest.signedAtMs,
  };
  switch (input.log.status) {
    case "confirmed":
      return input.log.isReset === true ? accepted : refusalFor(input.log);
    case "off":
      return logOffAcceptsReset() ? accepted : { kind: "refused", why: "unconfirmed" };
    case "mismatch":
    case "rollback":
      return refusalFor(input.log);
    case "unreachable":
    case "absent":
      return { kind: "no-verdict" };
  }
}

/**
 * Decision D3 (R.10): with no key log configured in the build, a reset is
 * accepted on the attestation alone. `false` would refuse every reset until
 * the log is deployed.
 */
export function logOffAcceptsReset(): boolean {
  return true;
}

/**
 * The rotation marker of the re-pin (R.2 "The re-pin"): never backwards, in
 * ADR-0016's marker space (seconds).
 */
export function resetRepinMarker(heldMarker: number, nowMs: number): number {
  return Math.max(heldMarker, Math.floor(nowMs / 1000));
}
