// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * ADR-0045 UX §6 / §12 item 6 — the typed membership-refusal table.
 *
 * `routes/Chat.tsx` cannot be tested at all (`vitest.config.ts` runs a plain
 * `node` environment with no jsdom and no Solid transform), so the rule lives
 * in `membership-refusal.ts` and is pinned here — the same split
 * `remove-member-confirm.ts` / `ownership-transfer.ts` / `pending-requests.ts`
 * use.
 *
 * ## The wasm is REAL, and in one test it is deliberately NOT
 *
 * `membershipRefusalRetryMaySucceed` is the actual core, loaded through the
 * Node `__wbindgen_init` glue. It is the whole point of requirement 2: the
 * retry affordance is the core's answer, not a list this client keeps.
 *
 * Two tests therefore drive the module with a **wrong** port on purpose. They
 * are not redundant with the real-wasm test — they are what discriminates
 * *asking* from *agreeing by coincidence*. A module that hard-coded the
 * partition would agree with the real core and pass every test that only ever
 * uses the real core.
 *
 * The discrimination evidence for each rule (each probe was RUN, the failure
 * observed, and the probe reverted) is recorded in the module's own header.
 */

import { readFileSync } from "node:fs";
import { fileURLToPath } from "node:url";
import { beforeAll, describe, expect, it } from "vitest";

import init, {
  membershipRefusalDisposition,
  membershipRefusalRetryMaySucceed,
} from "../../wasm/lakko_ffi_wasm";
import { ROLE_CHANGE_ALREADY_PENDING } from "./ownership-transfer";
import {
  ADD_NOT_OWNER_OR_ADMIN,
  COLLAPSED_TO_FALLBACK,
  JOINER_ALREADY_A_MEMBER,
  JOIN_REQUEST_NOT_KEPT,
  NO_SUCH_JOIN_REQUEST,
  NOT_OWNER,
  OWNER_CANNOT_LEAVE,
  OWNER_NOT_A_MEMBER,
  OWNER_UNREMOVABLE,
  PENDING_SET_FULL,
  REASONS_WITH_OWN_COPY,
  REFUSAL_FALLBACK,
  REMOVE_NOT_OWNER_OR_ADMIN,
  RETRY_WHEN_IT_LANDS,
  RETRY_WHEN_THEY_LAND,
  ROLE_CHANGE_PENDING,
  externalJoinAffordance,
  membershipRefusalOf,
  refusalReasonOf,
} from "./membership-refusal";

beforeAll(async () => {
  const wasmPath = fileURLToPath(
    new URL("../../wasm/lakko_ffi_wasm_bg.wasm", import.meta.url),
  );
  await init({ module_or_path: readFileSync(wasmPath) });
});

/** The real core port — what production passes. */
const REAL = membershipRefusalRetryMaySucceed;

/** A refusal exactly as `lakko-ffi-wasm` throws it: a real `Error` whose
 *  `.message` is the core's developer sentence, with two extra own
 *  properties. Built here rather than imported so the test does not depend
 *  on the module under test to construct its own input. */
function thrownRefusal(reason: string, message = "core prose, reworded freely"): Error {
  const err = new Error(message);
  Object.defineProperty(err, "refusalReason", { value: reason, enumerable: true });
  Object.defineProperty(err, "refusalDisposition", {
    value: membershipRefusalDisposition(reason) ?? "doNotRetry",
    enumerable: true,
  });
  return err;
}

/**
 * Every `RefusalReason` the CORE defines, read out of the core itself.
 *
 * This is the drift guard requirement 1 asks for, and a hand-written list
 * cannot be it: a list in this file agrees with itself forever, so a reason
 * added to `lakko-mls` tomorrow would map to nothing and no test would say
 * so. The wasm surface exports no enumeration (`membershipRefusalDisposition`
 * answers per code and returns `undefined` for anything it does not know), so
 * the only enumeration available to a web client is the core's own `ALL`.
 *
 * Parsed rather than guessed: `ALL` gives the variant names, and each
 * variant's `code()` arm gives the wire string the wasm actually throws.
 */
function coreRefusalCodes(): string[] {
  const src = readFileSync(
    fileURLToPath(new URL("../../../../crates/lakko-mls/src/refusal.rs", import.meta.url)),
    "utf8",
  );
  const all = /pub const ALL: \[Self; \d+\] = \[([\s\S]*?)\];/.exec(src);
  if (all === null) throw new Error("refusal.rs: could not find `RefusalReason::ALL`");
  const variants = [...all[1]!.matchAll(/Self::(\w+)\s*,/g)].map((m) => m[1]!);
  return variants.map((variant) => {
    const arm = new RegExp(`Self::${variant} => "(\\w+)"`).exec(src);
    if (arm === null) throw new Error(`refusal.rs: no code() arm for ${variant}`);
    return arm[1]!;
  });
}

describe("the core's refusal set", () => {
  it("is enumerable, so a NEW reason cannot slip past this suite", () => {
    const codes = coreRefusalCodes();
    // A parse that silently matched nothing would make every test below
    // vacuous, so the shape is asserted before it is used.
    expect(codes.length).toBeGreaterThanOrEqual(18);
    expect(new Set(codes).size).toBe(codes.length);
    expect(codes).toContain("addNotOwnerOrAdmin");
    expect(codes).toContain("pendingSetFull");
    // Every parsed code is one the core itself recognises — which catches a
    // parse that produced plausible-looking rubbish.
    for (const code of codes) {
      expect(membershipRefusalDisposition(code)).toBeDefined();
    }
  });
});

describe("membership refusal copy", () => {
  it("gives EVERY core reason a sentence, and none of them an empty one", () => {
    for (const code of coreRefusalCodes()) {
      const refusal = membershipRefusalOf(thrownRefusal(code), REAL);
      expect(refusal, `${code} produced no refusal at all`).not.toBeNull();
      expect(refusal!.message.length, `${code} rendered an empty sentence`).toBeGreaterThan(0);
      expect(refusal!.reason).toBe(code);
    }
  });

  it("has a DECIDED answer for every core reason — a new one fails here", () => {
    // Requirement 1, and the assertion above is not it. Falling back is what
    // an unknown reason does, so a reason nobody wrote copy for still renders
    // a correct-looking sentence and the test above passes. What must fail is
    // the DECISION being absent: every core reason is either given its own
    // row or recorded as a deliberate collapse, and a nineteenth reason lands
    // in neither.
    const decided = new Set([...REASONS_WITH_OWN_COPY, ...COLLAPSED_TO_FALLBACK]);
    const core = coreRefusalCodes();
    for (const code of core) {
      expect(
        decided.has(code),
        `${code} is a core refusal with no decision: give it a row in TABLE, or record ` +
          `the collapse in COLLAPSED_TO_FALLBACK`,
      ).toBe(true);
      expect(membershipRefusalOf(thrownRefusal(code), REAL)!.recognised, code).toBe(true);
    }
    // And nothing decided here has fallen out of the core — a stale row is
    // the same drift in the other direction.
    for (const code of decided) {
      expect(core, `${code} is decided here but no longer exists in the core`).toContain(code);
    }
    // The two halves are disjoint: a reason with its own sentence is not a
    // collapse, and vice versa.
    expect(REASONS_WITH_OWN_COPY.filter((c) => COLLAPSED_TO_FALLBACK.has(c))).toEqual([]);
  });

  it("states §6's approved rows verbatim", () => {
    const say = (code: string): string => membershipRefusalOf(thrownRefusal(code), REAL)!.message;

    expect(say("addNotOwnerOrAdmin")).toBe("Only the group owner or an admin can add members.");
    expect(say("removeNotOwnerOrAdmin")).toBe(
      "Only the group owner or an admin can remove members.",
    );
    expect(say("ownerUnremovable")).toBe("The group owner cannot be removed.");
    expect(say("ownerCannotLeave")).toBe("The group owner cannot leave the group.");
    expect(say("roleChangePending")).toBe(
      "A role change is already waiting to apply. Wait for it to land, then try again.",
    );
    // §6 row 7, the fail-closed fallback.
    expect(say("unsupportedProposal")).toBe("This change could not be requested.");

    // Asserted against the exported constants too, so renaming a constant
    // cannot silently change a sentence.
    expect(ADD_NOT_OWNER_OR_ADMIN()).toBe("Only the group owner or an admin can add members.");
    expect(REMOVE_NOT_OWNER_OR_ADMIN()).toBe("Only the group owner or an admin can remove members.");
    expect(OWNER_UNREMOVABLE()).toBe("The group owner cannot be removed.");
    expect(OWNER_CANNOT_LEAVE()).toBe("The group owner cannot leave the group.");
    expect(REFUSAL_FALLBACK()).toBe("This change could not be requested.");
  });

  it("reads §6 row 6 identically to the local pre-check that never reaches the core", () => {
    // `contacts/client.ts` refuses a second role change BEFORE calling the
    // core, throwing `ROLE_CHANGE_ALREADY_PENDING` as a plain Error with no
    // reason attached. The core refuses the same thing with
    // `roleChangePending`. Two entry points, one sentence — §6's "One string
    // table, two entry points, no second vocabulary" — so a user cannot tell
    // which layer answered.
    expect(membershipRefusalOf(thrownRefusal("roleChangePending"), REAL)!.message).toBe(
      ROLE_CHANGE_ALREADY_PENDING(),
    );
  });

  it("says something true for each reason §6 never named", () => {
    const say = (code: string): string => membershipRefusalOf(thrownRefusal(code), REAL)!.message;

    // §7.1 flags this one by name as needing the typed table.
    expect(say("pendingSetFull")).toBe(
      "Too many changes are already waiting to apply. Wait for them to land, then try again.",
    );
    // Reachable from the §6.1 transfer picker when the chosen member left.
    expect(say("ownerNotAMember")).toBe(OWNER_NOT_A_MEMBER());
    expect(OWNER_NOT_A_MEMBER()).toBe("Ownership can only be held by a current member of this group.");
    // Reachable from a stale Approve/Reject tap in the join-requests panel.
    expect(say("noSuchJoinRequest")).toBe(NO_SUCH_JOIN_REQUEST());
    expect(NO_SUCH_JOIN_REQUEST()).toBe("That join request is no longer waiting.");
    expect(say("joinerAlreadyAMember")).toBe(JOINER_ALREADY_A_MEMBER());
    expect(JOINER_ALREADY_A_MEMBER()).toBe("That person has already joined this group.");
    // §6 row 5, widened: the core gives ONE reason to a role change and an
    // ownership transfer, because a transfer IS a role-table rewrite. A
    // string that named only admins would be wrong for half of what the
    // reason covers.
    expect(say("notOwner")).toBe("Only the group owner can change admins or transfer ownership.");
    expect(NOT_OWNER()).toBe("Only the group owner can change admins or transfer ownership.");
  });

  it("collapses to §6's fail-closed row for everything a user cannot act on", () => {
    // Each of these is either a peer-gate refusal on somebody else's commit,
    // a sweep-side refusal on a surface §8 says shows nothing, or one of the
    // three §6 row 7 already names. None has a user-initiated entry point, so
    // a distinct sentence would be copy with no reader — and naming one would
    // leak an internal reason, which §6 row 7 forbids outright.
    expect([...COLLAPSED_TO_FALLBACK].sort()).toEqual([
      "groupStillActive",
      "leafUpdateRefused",
      "noRoleTable",
      "notACommitter",
      "notAMember",
      "proposalsPending",
      "roleTableWouldVanish",
      "sweepAlreadyStaged",
      "unsupportedProposal",
    ]);
    for (const code of COLLAPSED_TO_FALLBACK) {
      const refusal = membershipRefusalOf(thrownRefusal(code), REAL)!;
      expect(refusal.message, code).toBe(REFUSAL_FALLBACK());
      // A recorded collapse, not an oversight — which is the distinction
      // `recognised` exists to carry.
      expect(refusal.recognised, code).toBe(true);
    }
  });

  it("uses no protocol vocabulary", () => {
    // §1: user-facing copy says `request` and `change`; never proposal,
    // commit, epoch, sweep, committer, leaf, or quorum.
    const forbidden = ["proposal", "commit", "epoch", "sweep", "committer", "leaf"];
    for (const code of coreRefusalCodes()) {
      const message = membershipRefusalOf(thrownRefusal(code), REAL)!.message.toLowerCase();
      for (const word of forbidden) {
        expect(message, `${code}: "${word}" leaked into "${message}"`).not.toContain(word);
      }
    }
  });
});

describe("the retry affordance", () => {
  it("is EXACTLY the core's answer, and the core's answer is the two full queues", () => {
    // Requirement 2, first half. The maintainer's rule: retry when a request
    // was refused because a queue was full, never when it was refused because
    // of its own state. Written as a literal expectation rather than derived
    // from the port, so a reason that changes disposition in the core has to
    // be moved here too.
    const retryable = coreRefusalCodes().filter(
      (code) => membershipRefusalOf(thrownRefusal(code), REAL)!.retryMaySucceed,
    );
    expect(retryable).toEqual(["roleChangePending", "pendingSetFull"]);
  });

  it("follows the port even when the port is WRONG", () => {
    // Requirement 2, second half — the test that catches a client which
    // hard-coded the partition. A module that decided retryability itself
    // would agree with the real core on every input and no amount of
    // real-core testing would ever notice. So the port is inverted.
    const inverted = (reason: string): boolean => !REAL(reason);

    // The two the core calls retryable now read as not, and the invitation
    // to try again goes with them.
    for (const code of ["roleChangePending", "pendingSetFull"]) {
      const refusal = membershipRefusalOf(thrownRefusal(code), inverted)!;
      expect(refusal.retryMaySucceed, code).toBe(false);
      expect(refusal.message, code).not.toContain("try again");
    }
    // And one the core calls permanent reads as retryable.
    expect(membershipRefusalOf(thrownRefusal("ownerUnremovable"), inverted)!.retryMaySucceed).toBe(
      true,
    );
  });

  it("adds the try-again invitation only when the port says so", () => {
    // The invitation is not baked into the sentence: it is appended because
    // the CORE said waiting helps. With the port forced false the fact
    // survives and the invitation does not, which is what proves the
    // sentence is assembled from the core's answer rather than stored whole.
    const never = (): boolean => false;
    expect(membershipRefusalOf(thrownRefusal("roleChangePending"), never)!.message).toBe(
      ROLE_CHANGE_PENDING(),
    );
    expect(membershipRefusalOf(thrownRefusal("pendingSetFull"), never)!.message).toBe(
      PENDING_SET_FULL(),
    );
    // Concatenation, not two unrelated strings: joined they are §6 row 6.
    expect(`${ROLE_CHANGE_PENDING()} ${RETRY_WHEN_IT_LANDS()}`).toBe(ROLE_CHANGE_ALREADY_PENDING());
    expect(`${PENDING_SET_FULL()} ${RETRY_WHEN_THEY_LAND()}`).toBe(
      "Too many changes are already waiting to apply. Wait for them to land, then try again.",
    );
  });

  it("never invites a retry for a reason it does not recognise", () => {
    // Fail-closed both ways: an unknown reason gets the fallback sentence AND
    // no invitation, because nothing is known about whether waiting helps.
    const refusal = membershipRefusalOf(thrownRefusal("someReasonFromAFutureCore"), REAL)!;
    expect(refusal.retryMaySucceed).toBe(false);
    expect(refusal.message).toBe(REFUSAL_FALLBACK());
    // The one case `recognised` is false for: nobody decided about it.
    expect(refusal.recognised).toBe(false);
    // The code still survives for a log, so the gap is diagnosable.
    expect(refusal.reason).toBe("someReasonFromAFutureCore");
  });
});

describe("recognising a refusal at all", () => {
  it("reads the reason the wasm attaches, never the message", () => {
    expect(refusalReasonOf(thrownRefusal("notOwner"))).toBe("notOwner");
  });

  it("finds the reason through the wrapper the contacts client throws", () => {
    // Every membership call in `contacts/client.ts` goes out through
    // `ContactsError.wrap`, which builds a NEW Error carrying only the
    // message and parks the original on `variant.cause`. Without this the
    // whole table would be dead code on the one path that reaches the UI.
    const inner = thrownRefusal("addNotOwnerOrAdmin");
    const wrapped = Object.assign(new Error(inner.message), {
      variant: { kind: "wrapped", cause: inner },
    });
    expect(refusalReasonOf(wrapped)).toBe("addNotOwnerOrAdmin");
    expect(membershipRefusalOf(wrapped, REAL)!.message).toBe(ADD_NOT_OWNER_OR_ADMIN());

    // And through a plain `cause` chain, which is what a rethrow produces.
    const chained = new Error("outer", { cause: inner });
    expect(refusalReasonOf(chained)).toBe("addNotOwnerOrAdmin");
  });

  it("leaves a NON-refusal alone", () => {
    // An upload failure, a network error, a codec error: these keep their own
    // message. Turning one into "This change could not be requested." would
    // be a worse lie than the raw text, and §6 row 7 is about a refusal
    // whose reason must not be leaked — not about every error in the app.
    expect(refusalReasonOf(new Error("upload failed"))).toBeUndefined();
    expect(membershipRefusalOf(new Error("upload failed"), REAL)).toBeNull();
    expect(membershipRefusalOf("a bare string", REAL)).toBeNull();
    expect(membershipRefusalOf(null, REAL)).toBeNull();
    expect(membershipRefusalOf(undefined, REAL)).toBeNull();
  });

  it("ignores a `refusalReason` that is not a string", () => {
    // The property is attacker-adjacent only in the sense that it arrives
    // from a boundary; the point is that a non-string must not be coerced
    // into a lookup key that then misses and renders the fallback for what
    // is actually a plain error.
    const odd = Object.assign(new Error("plain"), { refusalReason: 7 });
    expect(refusalReasonOf(odd)).toBeUndefined();
  });
});

describe("an external-join request that was not kept", () => {
  it("is never actionable — the core would refuse the approve", () => {
    // Requirement 3. The core fires `ExternalJoinProposal` even when the
    // quarantine was full and the request was NOT retained; approving it
    // then refuses with `noSuchJoinRequest`. An Approve button for it is an
    // affordance that cannot work.
    const affordance = externalJoinAffordance({ retained: false });
    expect(affordance.kind).toBe("informational");
    expect(affordance.kind === "informational" && affordance.notice).toBe(JOIN_REQUEST_NOT_KEPT());
  });

  it("is actionable when the core kept it", () => {
    expect(externalJoinAffordance({ retained: true }).kind).toBe("actionable");
  });

  it("says what happened instead of showing nothing", () => {
    // §7.1 recorded this as unsurfaceable: "at 64 retained external joins
    // `retain_external_join` silently stops retaining, and nothing
    // distinguishes that from a request that never arrived". `retained` is
    // exactly that distinction, so the state is now statable and silence is
    // no longer the honest option.
    expect(JOIN_REQUEST_NOT_KEPT()).toBe(
      "A join request arrived while too many were already waiting, so it was not kept. " +
        "Ask the person to send it again later.",
    );
  });
});
