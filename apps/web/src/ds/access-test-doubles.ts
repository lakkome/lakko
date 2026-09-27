// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * Host-test doubles for ADR-0053's DS access signer, shared by the suites whose
 * fake groups drive a commit path. Test-only: nothing in the app imports this.
 *
 * A fake signer's verifying key is its epoch, so two signers for the same
 * epoch compare equal and a signer for a new epoch does not, which is all the
 * registry and the heal rule read. `sign` returns a string naming what it
 * signed, so a test can assert the exact method, path, query and body length.
 */
import type { ProofSigner } from "./access";

/** A fake signer at `epoch`. */
export function fakeSigner(epoch: bigint, label = "g"): ProofSigner {
  return {
    epoch,
    sign: (method, path, query, body) =>
      `proof:${label}@${epoch}:${method}:${path}:${query}:${body.length}`,
    groupClaimBodyJson: () => `{"epoch":${epoch},"access_pk_b64":"${label}${epoch}"}`,
    mailboxClaimBodyJson: () => `{"identity_pk_b64":"${label}"}`,
    verifyingKey: () => new Uint8Array([Number(epoch & 0xffn), label.charCodeAt(0)]),
    mailboxPseudonym: () => new Uint8Array(32).fill(label.charCodeAt(0)),
    free: () => undefined,
  };
}

/** A `dsAccessSigner` method for a fake group whose epoch `epoch()` reads. */
export function fakeAccessSigner(epoch: () => bigint, label = "g"): (identity: unknown) => ProofSigner {
  return () => fakeSigner(epoch(), label);
}
