// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * ADR-0045 UX §7 — the automatic re-request of a queue-full refusal.
 *
 * `routes/Chat.tsx` cannot be tested at all (`vitest.config.ts` runs a plain
 * `node` environment with no jsdom and no Solid transform), so the rule lives
 * in `membership-retry.ts` and is pinned here — the same split
 * `membership-refusal.ts` / `pending-requests.ts` / `unapplied-requests.ts`
 * use.
 *
 * ## The wasm is REAL, and in three tests it is deliberately NOT
 *
 * `membershipRefusalRetryMaySucceed` is the actual core, loaded through the
 * Node `__wbindgen_init` glue. The inverted-port tests are what discriminate
 * *asking the core* from *keeping a list that happens to agree with it*: a
 * module that hard-coded `pendingSetFull` / `roleChangePending` agrees with
 * the real core on every input and would pass every test that only ever uses
 * the real core.
 */

import { readFileSync } from "node:fs";
import { fileURLToPath } from "node:url";
import { beforeAll, beforeEach, describe, expect, it } from "vitest";

import init, {
  membershipRefusalDisposition,
  membershipRefusalRetryMaySucceed,
} from "../../wasm/lakko_ffi_wasm";
import { externalJoinAffordance } from "./membership-refusal";
import {
  type ArmedRetry,
  type GroupSnapshot,
  type RetryableRequest,
  armedApproveJoinRetry,
  armedRetryFor,
  claimArmedRetry,
  armRetry,
  armedRetries,
  clearArmedRetries,
  retryKey,
  retryScopeKey,
  retryVerdict,
} from "./membership-retry";

beforeAll(async () => {
  const wasmPath = fileURLToPath(
    new URL("../../wasm/lakko_ffi_wasm_bg.wasm", import.meta.url),
  );
  await init({ module_or_path: readFileSync(wasmPath) });
});

/** The real core port — what production passes. */
const REAL = membershipRefusalRetryMaySucceed;

/** A refusal exactly as `lakko-ffi-wasm` throws it. Built here rather than
 *  imported so the test does not depend on the module under test to construct
 *  its own input. */
function thrownRefusal(reason: string): Error {
  const err = new Error("core prose, reworded freely");
  Object.defineProperty(err, "refusalReason", { value: reason, enumerable: true });
  Object.defineProperty(err, "refusalDisposition", {
    value: membershipRefusalDisposition(reason) ?? "doNotRetry",
    enumerable: true,
  });
  return err;
}

/** The one production wrapper shape — `ContactsError.wrap` parks the original
 *  on `variant.cause`. */
function wrapped(inner: Error): Error {
  const outer = new Error("contacts: request failed");
  Object.defineProperty(outer, "variant", {
    value: { cause: inner },
    enumerable: true,
  });
  return outer;
}

const ALICE = "QUxJQ0U=";
const BOB = "Qk9C";
const CAROL = "Q0FST0w=";

/** Carol is NOT in the default roster, so an add of her is genuinely
 *  outstanding — the add fixture has to be somebody the group does not
 *  already contain, or every drain test reads as `already-applied` instead. */
const ADD_CAROL: RetryableRequest = {
  kind: "add",
  subjectKeyB64: CAROL,
  subjectAccountIdB64: "Y2Fyb2wtYWNjdA==",
};
const REMOVE_BOB: RetryableRequest = { kind: "remove", subjectKeyB64: BOB };
const LEAVE: RetryableRequest = { kind: "leave", subjectKeyB64: ALICE };
const MAKE_BOB_ADMIN: RetryableRequest = {
  kind: "set-admin",
  subjectKeyB64: BOB,
  makeAdmin: true,
};
const TRANSFER_TO_BOB: RetryableRequest = {
  kind: "transfer-ownership",
  subjectKeyB64: BOB,
  outgoing: "admin",
};
/** Carol asked to join via a `/g/` link and an admin's approve was refused. */
const APPROVE_CAROL: RetryableRequest = { kind: "approve-join", subjectKeyB64: CAROL };

/** A group of Alice (owner, leaf 0) and Bob (leaf 1), nothing pending, with
 *  Carol's join request retained in the quarantine. */
function snapshot(over: Partial<GroupSnapshot> = {}): GroupSnapshot {
  return {
    roster: [
      { leafIndex: 0, signatureKeyB64: ALICE },
      { leafIndex: 1, signatureKeyB64: BOB },
    ],
    pendingKeys: [],
    pendingCount: 0,
    roleTable: { ownerKeyB64: ALICE, adminKeysB64: [] },
    membershipEnded: false,
    retainedJoinKeys: [CAROL],
    ...over,
  };
}

function armed(request: RetryableRequest, pendingCountAtRefusal = 64): ArmedRetry {
  return { request, reason: "pendingSetFull", pendingCountAtRefusal };
}

const SCOPE = retryScopeKey("acct-1", "group-1");

beforeEach(() => {
  clearArmedRetries(SCOPE);
});

// ---------------------------------------------------------------------------
// 1. Arming — the core decides, and only the core
// ---------------------------------------------------------------------------

describe("what arms a retry", () => {
  it("arms on each of the core's own RetryLater reasons", () => {
    for (const reason of ["pendingSetFull", "roleChangePending"]) {
      expect(membershipRefusalRetryMaySucceed(reason)).toBe(true);
      const out = armedRetryFor(thrownRefusal(reason), ADD_CAROL, 64, REAL);
      expect(out, reason).not.toBeNull();
      expect(out?.reason).toBe(reason);
      expect(out?.pendingCountAtRefusal).toBe(64);
    }
  });

  it("never arms on a DoNotRetry or NotPermitted reason", () => {
    const never = [
      "addNotOwnerOrAdmin",
      "removeNotOwnerOrAdmin",
      "ownerUnremovable",
      "ownerCannotLeave",
      "notOwner",
      "notAMember",
      "notACommitter",
      "leafUpdateRefused",
      "unsupportedProposal",
      "noRoleTable",
      "ownerNotAMember",
      "roleTableWouldVanish",
      "proposalsPending",
      "sweepAlreadyStaged",
      "noSuchJoinRequest",
      "joinerAlreadyAMember",
      "adminNotAMember",
    ];
    for (const reason of never) {
      expect(membershipRefusalRetryMaySucceed(reason), reason).toBe(false);
      expect(armedRetryFor(thrownRefusal(reason), ADD_CAROL, 64, REAL), reason).toBeNull();
    }
  });

  it("never arms on an unknown reason from a newer core", () => {
    expect(armedRetryFor(thrownRefusal("someNineteenthReason"), ADD_CAROL, 64, REAL)).toBeNull();
  });

  it("never arms on an error that is not a refusal at all", () => {
    expect(armedRetryFor(new Error("socket closed"), ADD_CAROL, 64, REAL)).toBeNull();
    expect(armedRetryFor(null, ADD_CAROL, 64, REAL)).toBeNull();
    expect(armedRetryFor("a string", ADD_CAROL, 64, REAL)).toBeNull();
  });

  it("finds the reason through the wrapper the contacts client throws", () => {
    const out = armedRetryFor(wrapped(thrownRefusal("pendingSetFull")), ADD_CAROL, 64, REAL);
    expect(out).not.toBeNull();
  });

  // The probe the brief asks for: prove the module ASKS rather than keeping
  // its own list of which reasons are retryable. A hard-coded module agrees
  // with the real core on every input and passes every test above.
  it("follows the port even when the port is wrong, in both directions", () => {
    const invertedTrue: typeof REAL = (r) => r === "addNotOwnerOrAdmin";
    expect(
      armedRetryFor(thrownRefusal("addNotOwnerOrAdmin"), ADD_CAROL, 64, invertedTrue),
      "a port that says a permission refusal is retryable must be believed",
    ).not.toBeNull();

    const invertedFalse: typeof REAL = () => false;
    expect(
      armedRetryFor(thrownRefusal("pendingSetFull"), ADD_CAROL, 64, invertedFalse),
      "a port that says pendingSetFull is NOT retryable must be believed",
    ).toBeNull();
  });
});

// ---------------------------------------------------------------------------
// 2. The trigger — the queue drained, and never a clock
// ---------------------------------------------------------------------------

describe("when the retry fires", () => {
  it("waits while the queue has not drained", () => {
    expect(retryVerdict(armed(ADD_CAROL, 64), snapshot({ pendingCount: 64 }))).toEqual({
      kind: "wait",
    });
  });

  it("waits when the queue has GROWN", () => {
    expect(retryVerdict(armed(ADD_CAROL, 64), snapshot({ pendingCount: 70 }))).toEqual({
      kind: "wait",
    });
  });

  it("fires once the queue is strictly smaller than it was at the refusal", () => {
    expect(retryVerdict(armed(ADD_CAROL, 64), snapshot({ pendingCount: 63 }))).toEqual({
      kind: "fire",
    });
    expect(retryVerdict(armed(ADD_CAROL, 64), snapshot({ pendingCount: 0 }))).toEqual({
      kind: "fire",
    });
  });

  it("cannot fire from the passage of time alone — the same snapshot is the same verdict", () => {
    const a = armed(ADD_CAROL, 64);
    const view = snapshot({ pendingCount: 64 });
    for (let i = 0; i < 5; i++) {
      expect(retryVerdict(a, view)).toEqual({ kind: "wait" });
    }
  });

  it("claims nothing on a torn roster read", () => {
    expect(retryVerdict(armed(ADD_CAROL, 64), snapshot({ pendingCount: 0, roster: null }))).toEqual({
      kind: "wait",
    });
  });
});

// ---------------------------------------------------------------------------
// 3. Stopping conditions — every one a state, none a count
// ---------------------------------------------------------------------------

describe("when the retry stops", () => {
  it("drops an add once the person is in the roster", () => {
    const view = snapshot({
      pendingCount: 0,
      roster: [
        { leafIndex: 0, signatureKeyB64: ALICE },
        { leafIndex: 1, signatureKeyB64: BOB },
        { leafIndex: 2, signatureKeyB64: CAROL },
      ],
    });
    expect(retryVerdict(armed(ADD_CAROL, 64), view)).toEqual({
      kind: "drop",
      why: "already-applied",
    });
  });

  it("drops a remove once the person has left the roster", () => {
    const view = snapshot({
      pendingCount: 0,
      roster: [{ leafIndex: 0, signatureKeyB64: ALICE }],
    });
    expect(retryVerdict(armed(REMOVE_BOB, 64), view)).toEqual({
      kind: "drop",
      why: "already-applied",
    });
  });

  it("drops a leave once our own leaf has gone", () => {
    const view = snapshot({
      pendingCount: 0,
      roster: [{ leafIndex: 1, signatureKeyB64: BOB }],
    });
    expect(retryVerdict(armed(LEAVE, 64), view)).toEqual({
      kind: "drop",
      why: "already-applied",
    });
  });

  it("drops a role change once the live table already says what was asked", () => {
    const view = snapshot({
      pendingCount: 0,
      roleTable: { ownerKeyB64: ALICE, adminKeysB64: [BOB] },
    });
    expect(retryVerdict(armed(MAKE_BOB_ADMIN, 64), view)).toEqual({
      kind: "drop",
      why: "already-applied",
    });
  });

  it("drops a DEMOTION once the member is no longer an admin", () => {
    const demote: RetryableRequest = {
      kind: "set-admin",
      subjectKeyB64: BOB,
      makeAdmin: false,
    };
    const view = snapshot({
      pendingCount: 0,
      roleTable: { ownerKeyB64: ALICE, adminKeysB64: [] },
    });
    expect(retryVerdict(armed(demote, 64), view)).toEqual({
      kind: "drop",
      why: "already-applied",
    });
    // ...and keeps waiting while they still are one.
    expect(
      retryVerdict(
        armed(demote, 64),
        snapshot({ pendingCount: 64, roleTable: { ownerKeyB64: ALICE, adminKeysB64: [BOB] } }),
      ),
    ).toEqual({ kind: "wait" });
  });

  it("drops a transfer once the successor holds the group", () => {
    const view = snapshot({
      pendingCount: 0,
      roleTable: { ownerKeyB64: BOB, adminKeysB64: [ALICE] },
    });
    expect(retryVerdict(armed(TRANSFER_TO_BOB, 64), view)).toEqual({
      kind: "drop",
      why: "already-applied",
    });
  });

  it("keeps waiting on a transfer the live table has not yet made", () => {
    expect(
      retryVerdict(armed(TRANSFER_TO_BOB, 64), snapshot({ pendingCount: 64 })),
    ).toEqual({ kind: "wait" });
  });

  it("shares ONE armed slot between the two role-change shapes, newest wins", () => {
    expect(retryKey(MAKE_BOB_ADMIN)).toBe(retryKey(TRANSFER_TO_BOB));
    armRetry(SCOPE, armed(MAKE_BOB_ADMIN));
    armRetry(SCOPE, armed(TRANSFER_TO_BOB));
    expect(armedRetries(SCOPE)).toHaveLength(1);
    expect(armedRetries(SCOPE)[0]?.request.kind).toBe("transfer-ownership");
  });

  it("keeps waiting on a role change the live table does NOT yet match", () => {
    const view = snapshot({
      pendingCount: 64,
      roleTable: { ownerKeyB64: ALICE, adminKeysB64: [] },
    });
    expect(retryVerdict(armed(MAKE_BOB_ADMIN, 64), view)).toEqual({ kind: "wait" });
  });

  it("drops once the request is itself waiting in the pending set", () => {
    expect(
      retryVerdict(armed(ADD_CAROL, 64), snapshot({ pendingCount: 1, pendingKeys: [`add:${CAROL}`] })),
    ).toEqual({ kind: "drop", why: "already-waiting" });
  });

  it("resolves a pending REMOVE through the roster rather than a stored leaf", () => {
    // Bob is at leaf 1 now. `proposalKey` writes `remove:1`.
    expect(
      retryVerdict(armed(REMOVE_BOB, 64), snapshot({ pendingCount: 1, pendingKeys: ["remove:1"] })),
    ).toEqual({ kind: "drop", why: "already-waiting" });
    // A pending remove of somebody ELSE's leaf is not ours.
    expect(
      retryVerdict(armed(REMOVE_BOB, 64), snapshot({ pendingCount: 1, pendingKeys: ["remove:7"] })),
    ).toEqual({ kind: "fire" });
  });

  it("drops a role change already waiting, whatever it says", () => {
    expect(
      retryVerdict(
        armed(MAKE_BOB_ADMIN, 64),
        snapshot({ pendingCount: 1, pendingKeys: ["role-change"] }),
      ),
    ).toEqual({ kind: "drop", why: "already-waiting" });
  });

  it("drops everything once our own membership has ended", () => {
    expect(
      retryVerdict(armed(ADD_CAROL, 64), snapshot({ pendingCount: 0, membershipEnded: true })),
    ).toEqual({ kind: "drop", why: "membership-ended" });
  });

  it("drops a role change in a group that has lost its role table", () => {
    expect(
      retryVerdict(armed(MAKE_BOB_ADMIN, 64), snapshot({ pendingCount: 0, roleTable: null })),
    ).toEqual({ kind: "drop", why: "no-role-table" });
  });
});

// ---------------------------------------------------------------------------
// 3b. Approving an external join — the arm the core made possible
// ---------------------------------------------------------------------------

describe("approving an external join", () => {
  it("arms on a queue-full approve refusal, which the core now carries a reason for", () => {
    const out = armedApproveJoinRetry(
      thrownRefusal("pendingSetFull"),
      CAROL,
      64,
      true,
      REAL,
    );
    expect(out).not.toBeNull();
    expect(out?.request).toEqual(APPROVE_CAROL);
    expect(out?.pendingCountAtRefusal).toBe(64);
  });

  it("never arms on any approve refusal the core does not call retryable", () => {
    // Every reason `approve_external_join` can produce besides the queue-full
    // one. Each is asserted against the CORE's own answer rather than a list
    // written here, so a core that reclassified one would fail this test
    // rather than silently change what the client does.
    for (const reason of [
      "noRoleTable",
      "noSuchJoinRequest",
      "joinerAlreadyAMember",
      "addNotOwnerOrAdmin",
      "notAMember",
    ]) {
      expect(membershipRefusalRetryMaySucceed(reason), reason).toBe(false);
      expect(
        armedApproveJoinRetry(thrownRefusal(reason), CAROL, 64, true, REAL),
        reason,
      ).toBeNull();
    }
  });

  it("never arms on an approve that failed with NO reason at all", () => {
    // The state before the core fix: `approve_external_join` flattened the
    // typed error into prose, so this is what every approve refusal looked
    // like. An absent reason arms nothing, then and now.
    expect(
      armedApproveJoinRetry(
        new Error("external join approve refused: proposal refused"),
        CAROL,
        64,
        true,
        REAL,
      ),
    ).toBeNull();
  });

  it("finds the reason through the wrapper the contacts client throws", () => {
    expect(
      armedApproveJoinRetry(
        wrapped(thrownRefusal("pendingSetFull")),
        CAROL,
        64,
        true,
        REAL,
      ),
    ).not.toBeNull();
  });

  it("follows the port even when the port is wrong, in both directions", () => {
    const invertedTrue: typeof REAL = (r) => r === "noSuchJoinRequest";
    expect(
      armedApproveJoinRetry(
        thrownRefusal("noSuchJoinRequest"),
        CAROL,
        64,
        true,
        invertedTrue,
      ),
      "a port that says a missing-request refusal is retryable must be believed",
    ).not.toBeNull();

    const invertedFalse: typeof REAL = () => false;
    expect(
      armedApproveJoinRetry(
        thrownRefusal("pendingSetFull"),
        CAROL,
        64,
        true,
        invertedFalse,
      ),
      "a port that says pendingSetFull is NOT retryable must be believed",
    ).toBeNull();
  });

  // The rule this asserts is `externalJoinAffordance`'s, not one restated
  // here: an unretained request has nothing for `approveExternalJoin` to look
  // up, so a retry for it could only ever be refused. Written against the
  // affordance rather than against the literal `false` so that a future change
  // to what "actionable" means carries the retry path with it.
  it("arms exactly when the affordance says the request is actionable", () => {
    for (const retained of [true, false]) {
      const actionable = externalJoinAffordance({ retained }).kind === "actionable";
      const out = armedApproveJoinRetry(
        thrownRefusal("pendingSetFull"),
        CAROL,
        64,
        retained,
        REAL,
      );
      expect(out !== null, `retained=${retained}`).toBe(actionable);
    }
  });

  it("keeps the approve slot apart from an add of the same person", () => {
    expect(retryKey(APPROVE_CAROL)).not.toBe(retryKey(ADD_CAROL));
    expect(retryKey(APPROVE_CAROL)).toContain(CAROL);
    armRetry(SCOPE, armed(APPROVE_CAROL));
    armRetry(SCOPE, armed(ADD_CAROL));
    expect(armedRetries(SCOPE)).toHaveLength(2);
    // A manual approve tap disarms the approve record and leaves the add one.
    expect(claimArmedRetry(SCOPE, retryKey(APPROVE_CAROL))).not.toBeUndefined();
    expect(claimArmedRetry(SCOPE, retryKey(APPROVE_CAROL))).toBeUndefined();
    expect(armedRetries(SCOPE)).toHaveLength(1);
    expect(armedRetries(SCOPE)[0]?.request.kind).toBe("add");
  });

  it("fires once the queue drained, and not before", () => {
    expect(retryVerdict(armed(APPROVE_CAROL, 64), snapshot({ pendingCount: 64 }))).toEqual({
      kind: "wait",
    });
    expect(retryVerdict(armed(APPROVE_CAROL, 64), snapshot({ pendingCount: 63 }))).toEqual({
      kind: "fire",
    });
  });

  it("drops once the joiner is a member — whoever's approve landed", () => {
    const view = snapshot({
      pendingCount: 0,
      roster: [
        { leafIndex: 0, signatureKeyB64: ALICE },
        { leafIndex: 1, signatureKeyB64: BOB },
        { leafIndex: 2, signatureKeyB64: CAROL },
      ],
      // The core reaps a joined request out of the quarantine, so this is
      // empty in reality; supplied non-empty here so the test can only pass
      // by reading the ROSTER.
      retainedJoinKeys: [CAROL],
    });
    expect(retryVerdict(armed(APPROVE_CAROL, 64), view)).toEqual({
      kind: "drop",
      why: "already-applied",
    });
  });

  it("drops once another admin's Add for the joiner is already waiting", () => {
    // An approved external join IS an ordinary admin-authored `Add` keyed on
    // the joiner's signature key, so it lands in the pending set under exactly
    // the key an add of the same person would.
    expect(
      retryVerdict(
        armed(APPROVE_CAROL, 64),
        snapshot({ pendingCount: 1, pendingKeys: [`add:${CAROL}`] }),
      ),
    ).toEqual({ kind: "drop", why: "already-waiting" });
  });

  it("drops once the request has left the retained quarantine", () => {
    // Nothing else in the group changed: Carol is not a member, no Add is
    // pending, the queue drained. The core simply no longer holds the request
    // — a reload of the group empties the in-memory quarantine while this
    // record survives — and re-issuing would be refused `noSuchJoinRequest`.
    expect(
      retryVerdict(armed(APPROVE_CAROL, 64), snapshot({ pendingCount: 0, retainedJoinKeys: [] })),
    ).toEqual({ kind: "drop", why: "join-request-gone" });
    // Somebody ELSE's retained request is not ours.
    expect(
      retryVerdict(
        armed(APPROVE_CAROL, 64),
        snapshot({ pendingCount: 0, retainedJoinKeys: [BOB] }),
      ),
    ).toEqual({ kind: "drop", why: "join-request-gone" });
  });

  it("claims nothing on a torn read of the retained set", () => {
    expect(
      retryVerdict(
        armed(APPROVE_CAROL, 64),
        snapshot({ pendingCount: 0, retainedJoinKeys: null }),
      ),
    ).toEqual({ kind: "wait" });
  });

  it("does not read the retained set for any other kind of request", () => {
    // A torn or empty quarantine says nothing about an add, a removal or a
    // role change, so it must not stop one.
    for (const request of [ADD_CAROL, REMOVE_BOB, MAKE_BOB_ADMIN]) {
      expect(
        retryVerdict(armed(request, 64), snapshot({ pendingCount: 0, retainedJoinKeys: null })),
        request.kind,
      ).toEqual({ kind: "fire" });
      expect(
        retryVerdict(armed(request, 64), snapshot({ pendingCount: 0, retainedJoinKeys: [] })),
        request.kind,
      ).toEqual({ kind: "fire" });
    }
  });

  it("drops an approve in a group that has lost its role table", () => {
    expect(
      retryVerdict(armed(APPROVE_CAROL, 64), snapshot({ pendingCount: 0, roleTable: null })),
    ).toEqual({ kind: "drop", why: "no-role-table" });
  });

  it("drops an approve once our own membership has ended", () => {
    expect(
      retryVerdict(armed(APPROVE_CAROL, 64), snapshot({ pendingCount: 0, membershipEnded: true })),
    ).toEqual({ kind: "drop", why: "membership-ended" });
  });

  it("hands the approve record to exactly one of two concurrent claims", () => {
    armRetry(SCOPE, armed(APPROVE_CAROL));
    expect(claimArmedRetry(SCOPE, retryKey(APPROVE_CAROL))).not.toBeUndefined();
    expect(claimArmedRetry(SCOPE, retryKey(APPROVE_CAROL))).toBeUndefined();
  });
});

// ---------------------------------------------------------------------------
// 4. Exactly-once
// ---------------------------------------------------------------------------

describe("exactly-once", () => {
  it("hands the record to exactly one of two concurrent claims", () => {
    armRetry(SCOPE, armed(ADD_CAROL));
    const first = claimArmedRetry(SCOPE, retryKey(ADD_CAROL));
    const second = claimArmedRetry(SCOPE, retryKey(ADD_CAROL));
    expect(first).not.toBeUndefined();
    expect(second).toBeUndefined();
  });

  it("holds at most one record per request, so a second refusal replaces rather than duplicates", () => {
    armRetry(SCOPE, armed(ADD_CAROL, 64));
    armRetry(SCOPE, armed(ADD_CAROL, 12));
    expect(armedRetries(SCOPE)).toHaveLength(1);
    expect(armedRetries(SCOPE)[0]?.pendingCountAtRefusal).toBe(12);
  });

  it("a manual tap disarms the record, so an in-flight retry cannot double-publish", () => {
    armRetry(SCOPE, armed(ADD_CAROL));
    // The manual handler claims the same slot before it starts.
    expect(claimArmedRetry(SCOPE, retryKey(ADD_CAROL))).not.toBeUndefined();
    // The retry driver then finds nothing to fire.
    expect(claimArmedRetry(SCOPE, retryKey(ADD_CAROL))).toBeUndefined();
    expect(armedRetries(SCOPE)).toHaveLength(0);
  });

  it("keeps different requests apart", () => {
    armRetry(SCOPE, armed(ADD_CAROL));
    armRetry(SCOPE, armed(REMOVE_BOB));
    expect(armedRetries(SCOPE)).toHaveLength(2);
    claimArmedRetry(SCOPE, retryKey(ADD_CAROL));
    expect(armedRetries(SCOPE)).toHaveLength(1);
  });

  it("scopes records per group", () => {
    armRetry(SCOPE, armed(ADD_CAROL));
    expect(armedRetries(retryScopeKey("acct-1", "group-2"))).toHaveLength(0);
    clearArmedRetries(retryScopeKey("acct-1", "group-2"));
    expect(armedRetries(SCOPE)).toHaveLength(1);
  });

  // This repo has been bitten twice by a raw NUL committed into a source file
  // (ADR-0042 MEDIUM-3, ADR-0043 F5): git then treats the file as binary and
  // `grep -r` skips it silently. A scope key is a plain Map key, so a control
  // character in one is always a mistake — asserted here rather than left to a
  // manual byte scan, and it is why the scope constants above are BUILT by the
  // module rather than written as literals.
  it("builds a scope key with no control characters", () => {
    const key = retryScopeKey("acct-1", "group-1");
    // eslint-disable-next-line no-control-regex
    expect(/[\u0000-\u001f]/.test(key), key).toBe(false);
    expect(key).toBe("acct-1 group-1");
    // Both halves are load-bearing and ordered.
    expect(retryScopeKey("a", "b")).not.toBe(retryScopeKey("b", "a"));
  });

  it("keys a remove on the signature key, never on a leaf that OpenMLS reuses", () => {
    expect(retryKey(REMOVE_BOB)).toContain(BOB);
    expect(retryKey(REMOVE_BOB)).not.toBe("remove:1");
  });
});
