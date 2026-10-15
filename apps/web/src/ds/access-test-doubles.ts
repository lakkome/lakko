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
import type { JournalAuthority, StatementRequestLike, StatementSignerLike } from "./ownership";

/** A fake signer at `epoch`. */
export function fakeSigner(epoch: bigint, label = "g"): ProofSigner {
  return {
    epoch,
    sign: (method, path, query, body) =>
      `proof:${label}@${epoch}:${method}:${path}:${query}:${body.length}`,
    mailboxClaimBodyJson: () => `{"mailbox_pk_b64":"${label}"}`,
    verifyingKey: () => new Uint8Array([Number(epoch & 0xffn), label.charCodeAt(0)]),
    mailboxAddress: () => new Uint8Array(32).fill(label.charCodeAt(0)),
    free: () => undefined,
  };
}

/** A `dsAccessSigner` method for a fake group whose epoch `epoch()` reads. */
export function fakeAccessSigner(epoch: () => bigint, label = "g"): (identity: unknown) => ProofSigner {
  return () => fakeSigner(epoch(), label);
}

/** One statement a fake signer built, as the fake records it. */
export interface FakeStatement {
  readonly segment: string;
  readonly signer: string;
  readonly journalHex: string;
  readonly epoch?: bigint;
  readonly epochKey?: Uint8Array;
  readonly keys?: Uint8Array;
  readonly ranks?: Uint8Array;
  readonly pushGid?: Uint8Array;
  readonly newRoot?: Uint8Array;
}

function hexOf(b: Uint8Array): string {
  return Array.from(b, (x) => x.toString(16).padStart(2, "0")).join("");
}

/**
 * A fake ADR-0057 statement signer named `label`. Each built request's body
 * is the JSON of a {@link FakeStatement}, so a test's `putStatement` fake can
 * read back exactly what was signed and by whom.
 */
export function fakeStatementSigner(label: string, publicKey?: Uint8Array): StatementSignerLike {
  const req = (st: FakeStatement): StatementRequestLike => ({
    path: `/ds/group/${st.journalHex}/${st.segment}`,
    bodyJson: JSON.stringify(st, (_k, v: unknown) =>
      typeof v === "bigint" ? v.toString() : v instanceof Uint8Array ? hexOf(v) : v,
    ),
    free: () => undefined,
  });
  return {
    claim: (j, epoch, epochKey) =>
      req({ segment: "claim", signer: label, journalHex: hexOf(j), epoch, epochKey }),
    reset: (j, epoch, epochKey) =>
      req({ segment: "reset", signer: label, journalHex: hexOf(j), epoch, epochKey }),
    registerEpoch: (j, epoch, epochKey, revoke) =>
      req({ segment: "access", signer: label, journalHex: hexOf(j), epoch, epochKey, keys: revoke }),
    replaceEpoch: (j, epoch, epochKey, revoke) =>
      req({ segment: "access-replace", signer: label, journalHex: hexOf(j), epoch, epochKey, keys: revoke }),
    registrars: (j, keys, ranks) =>
      req({ segment: "registrars", signer: label, journalHex: hexOf(j), keys, ranks }),
    registrarsSet: (j, keys, ranks) =>
      req({ segment: "registrars-set", signer: label, journalHex: hexOf(j), keys, ranks }),
    rootTransfer: (j, newRoot) =>
      req({ segment: "root", signer: label, journalHex: hexOf(j), newRoot }),
    pushBinding: (j, pushGid) =>
      req({ segment: "push-binding", signer: label, journalHex: hexOf(j), pushGid }),
    publicKey: () => publicKey ?? new Uint8Array(32).fill(label.charCodeAt(0)),
    free: () => undefined,
  };
}

/** A root authority over a fake signer. */
export function fakeRootAuthority(label = "root", rootSeed = new Uint8Array(32).fill(7)): JournalAuthority {
  return { kind: "root", signer: fakeStatementSigner(label), rootSeed };
}

/** A registrar authority over a fake signer. */
export function fakeRegistrarAuthority(label = "reg", rank = 1): JournalAuthority {
  return { kind: "registrar", signer: fakeStatementSigner(label), rank };
}

/** Parse a statement body a fake signer built. */
export function parseFakeStatement(bodyJson: string): Record<string, string> {
  return JSON.parse(bodyJson) as Record<string, string>;
}
