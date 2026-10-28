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
import { CommitConflictError } from "./ownership";

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
  /** ADR-0081 - the row a `commit` request carries. */
  readonly row?: Uint8Array;
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
    replaceEpoch: (j, epoch, epochKey, revoke) =>
      req({ segment: "access-replace", signer: label, journalHex: hexOf(j), epoch, epochKey, keys: revoke }),
    registrars: (j, keys, ranks) =>
      req({ segment: "registrars", signer: label, journalHex: hexOf(j), keys, ranks }),
    registrarsSet: (j, keys, ranks) =>
      req({ segment: "registrars-set", signer: label, journalHex: hexOf(j), keys, ranks }),
    rootTransfer: (j, newRoot) =>
      req({ segment: "root", signer: label, journalHex: hexOf(j), newRoot }),
    commit: (j, epoch, epochKey, revoke, row) =>
      req({ segment: "commit", signer: label, journalHex: hexOf(j), epoch, epochKey, keys: revoke, row }),
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

/** The DS operations a commit path's fake implemented before ADR-0081. */
interface PublishingFake {
  publishGroup?(groupId: Uint8Array, row: Uint8Array, ...rest: never[]): Promise<{ id: number; createdAt: string }>;
  registerGroupEpoch?(groupId: Uint8Array, epoch: bigint, key: Uint8Array, revoke?: readonly Uint8Array[]): Promise<void>;
}

/**
 * ADR-0081 - give a fake DS client `commitGroupEpoch` as the one request it
 * stands for, with the DS's semantics:
 *
 * - the registration (`registerGroupEpoch`, when the fake records it) and the
 *   row's append (`publishGroup`) happen together: a refusal of either leaves
 *   neither, and nothing is remembered for the epoch;
 * - an identical re-send (same journal, epoch and row) appends nothing and
 *   answers the original id with `resent: true`;
 * - any other commit at an epoch already registered with a row is refused
 *   with {@link CommitConflictError} (`409`), appending nothing.
 *
 * So a suite written against the two separate calls keeps asserting the same
 * order and refusals, and a journal a test replays holds exactly the rows the
 * DS would have appended.
 */
export function withCommitRoute<T extends PublishingFake>(fake: T): T & {
  commitGroupEpoch(
    groupId: Uint8Array,
    epoch: bigint,
    key: Uint8Array,
    revoke: readonly Uint8Array[],
    row: Uint8Array,
  ): Promise<{ id: number; createdAt: string; resent: boolean }>;
} {
  const registered = new Map<string, { row: string; id: number; createdAt: string }>();
  return Object.assign(fake, {
    async commitGroupEpoch(
      groupId: Uint8Array,
      epoch: bigint,
      key: Uint8Array,
      revoke: readonly Uint8Array[],
      row: Uint8Array,
    ) {
      const slot = `${hexOf(groupId)}:${epoch}`;
      const held = registered.get(slot);
      if (held !== undefined) {
        if (held.row === hexOf(row)) return { id: held.id, createdAt: held.createdAt, resent: true };
        throw new CommitConflictError(Object.assign(new Error("409"), { status: 409 }));
      }
      await fake.registerGroupEpoch?.(groupId, epoch, key, revoke);
      const p =
        fake.publishGroup === undefined
          ? { id: 1, createdAt: "t" }
          : await fake.publishGroup(groupId, row);
      registered.set(slot, { row: hexOf(row), id: p.id, createdAt: p.createdAt });
      return { id: p.id, createdAt: p.createdAt, resent: false };
    },
  });
}

/** The eager direct-commit methods a fake group may carry, each moving the
 *  fake's epoch by one as the real core's eager merge does. */
const STAGED_TWINS: Readonly<Record<string, string>> = {
  stageAddMember: "addMember",
  stageAddMembers: "addMembers",
  stageRemoveMember: "removeMember",
  stageSetGroupRoles: "setGroupRoles",
};

/**
 * ADR-0081 - give a fake group the staged twins of its eager direct commits
 * (`stageAddMember`, `stageRemoveMember`, `stageSetGroupRoles`) and the
 * pending-commit lifecycle (`hasStagedSweep`, `dsStagedAccessSigner`,
 * `mergeStagedSweep`, `abandonStagedSweep`), as the real core has them.
 *
 * A staged twin runs the fake's eager method (so its spies and recorded
 * order see the create) and holds the epoch it moved back until a merge:
 * the group reads N while the commit is staged, N+1 after a merge, and N
 * again after an abandon. `dsAccessSigner` follows the epoch the group
 * reads. A second stage while one is pending throws, as the core refuses it.
 * A fake that already defines the lifecycle (a sweep double) keeps its own.
 *
 * No client calls the staged twins since the ADR-0081 amendment of 2026-10-19
 * deleted the direct-commit paths; what the suites still use here is the
 * pending-commit lifecycle the ADR-0045 sweep reaches.
 */
export function withStagedTwins<T extends object>(group: T): T {
  let offset = 0n;
  let staged = false;
  const raw = group as unknown as Record<string, unknown> & { readonly epoch?: bigint };
  const readEpoch = (): bigint => (raw.epoch ?? 0n) + offset;
  return new Proxy(group, {
    get(target, prop, receiver) {
      if (typeof prop === "string" && prop in STAGED_TWINS) {
        const eager = Reflect.get(target, STAGED_TWINS[prop] as string, receiver) as
          | ((...args: unknown[]) => unknown)
          | undefined;
        if (eager === undefined) return undefined;
        return (...args: unknown[]) => {
          if (staged) throw new Error("sweepAlreadyStaged: a staged commit is already awaiting a verdict");
          const before = raw.epoch ?? 0n;
          const out = eager.apply(target, args);
          offset -= (raw.epoch ?? 0n) - before;
          staged = true;
          return out;
        };
      }
      if (prop === "epoch" && "epoch" in target) return readEpoch();
      if (prop === "dsAccessSigner" && "dsAccessSigner" in target) {
        return () => fakeSigner(readEpoch());
      }
      if (prop in target) return Reflect.get(target, prop, receiver);
      switch (prop) {
        case "hasStagedSweep":
          return () => staged;
        case "dsStagedAccessSigner":
          return () => (staged ? fakeSigner(readEpoch() + 1n) : undefined);
        case "mergeStagedSweep":
          return () => {
            if (staged) {
              offset += 1n;
              staged = false;
            }
          };
        case "abandonStagedSweep":
          return () => {
            staged = false;
          };
        default:
          return undefined;
      }
    },
  });
}
