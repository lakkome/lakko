// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * ADR-0074 section 3.5a, the second pass: the pure decisions behind rules 1,
 * 4, 5 and 6 as the web client applies them.
 */
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import {
  PREDECESSOR_MAILBOX_READ_MS,
  decodeNumberByKey,
  predecessorMailboxEvents,
  readPredecessorAdoptedAtMs,
  readablePredecessorKeys,
  resolveBeforeAddress,
  stampPredecessorAdoption,
  keysOfPerson,
} from "./address-resolver";
import { afterProvenRotation, type AccountKeys } from "./predecessor";
import { pendingAfterArrival, createPendingInbox } from "./inbox-store";
import { requestTargetAfterFetch } from "./request-target";
import { isProvenSuccessor } from "./proven-successor";
import {
  linkRevokeOutcome,
  requestsToResend,
  revocationSeedOrder,
  rotatedAtMsOf,
  spentFailureOutcome,
  spentStepVerdict,
} from "./rotation-spent";
import {
  MailboxUnreachableError,
  writeWithAddressRecheck,
  type ResolvedWriteAuth,
  type WriteCandidate,
  type WritePlan,
} from "../reach/mailbox-write";
import {
  EMPTY_RECEIVED_GRANTS,
  dropContactGrant,
  dropScope,
  fileContactGrant,
  fileScopedGrant,
  grantFiling,
  grantsInOrderForPerson,
  withdrawMissingScopedGrants,
} from "../reach/received-grants";
import type { IncomingContactRequest } from "./client";

// ---------------------------------------------------------------------------
// C. Own predecessors are a chain
// ---------------------------------------------------------------------------

describe("own predecessors are a chain", () => {
  const own: AccountKeys = afterProvenRotation(
    afterProvenRotation({ currentKeyB64: "K1", provenPredecessorsB64: [] }, "K2"),
    "K3",
  );

  it("after a second rotation every earlier own key maps to the current one", () => {
    expect(own.provenPredecessorsB64).toEqual(["K1", "K2"]);
    expect(resolveBeforeAddress("K1", [], own)).toBe("K3");
    expect(resolveBeforeAddress("K2", [], own)).toBe("K3");
    expect(resolveBeforeAddress("K3", [], own)).toBe("K3");
    expect(keysOfPerson("K1", [], own)).toEqual(["K3", "K2", "K1"]);
  });

  it("each predecessor mailbox is read inside its OWN 30-day window", () => {
    const t0 = 1_700_000_000_000;
    const left: Record<string, number> = { k1: t0, k2: t0 + 20 * 24 * 60 * 60 * 1000 };
    const at = (k: string): number | null => left[k] ?? null;
    // Day 25: both windows are open.
    expect(readablePredecessorKeys(["k1", "k2"], at, t0 + 25 * 86_400_000)).toEqual(["k1", "k2"]);
    // Day 35: the first is over, the second (left on day 20) is not.
    expect(readablePredecessorKeys(["k1", "k2"], at, t0 + 35 * 86_400_000)).toEqual(["k2"]);
    // Day 51: both are over.
    expect(readablePredecessorKeys(["k1", "k2"], at, left["k2"]! + PREDECESSOR_MAILBOX_READ_MS)).toEqual([]);
    // A key with no time stored is read (the caller stamps it now).
    expect(readablePredecessorKeys(["k9"], at, t0)).toEqual(["k9"]);
  });

  it("a stored row that is not a map of positive numbers reads as empty", () => {
    expect(decodeNumberByKey(null)).toEqual({});
    expect(decodeNumberByKey("7")).toEqual({});
    expect(decodeNumberByKey("[1]")).toEqual({});
    expect(decodeNumberByKey("{")).toEqual({});
    expect(decodeNumberByKey('{"a":5,"b":"x","c":-1,"d":0}')).toEqual({ a: 5 });
  });

  describe("the adoption time is per key and never restarted", () => {
    const store = new Map<string, string>();
    beforeEach(() => {
      store.clear();
      vi.stubGlobal("localStorage", {
        getItem: (k: string) => store.get(k) ?? null,
        setItem: (k: string, v: string) => void store.set(k, v),
        removeItem: (k: string) => void store.delete(k),
      });
    });
    afterEach(() => vi.unstubAllGlobals());

    it("a later rotation does not extend an earlier predecessor's window", () => {
      stampPredecessorAdoption("acct", "k1", 1000);
      // The second rotation: k2 is left now; k1's time stays.
      stampPredecessorAdoption("acct", "k2", 5000);
      stampPredecessorAdoption("acct", "k1", 5000);
      expect(readPredecessorAdoptedAtMs("acct", "k1")).toBe(1000);
      expect(readPredecessorAdoptedAtMs("acct", "k2")).toBe(5000);
      expect(readPredecessorAdoptedAtMs("acct", "k3")).toBeNull();
      expect(readPredecessorAdoptedAtMs("other", "k1")).toBeNull();
    });
  });

  it("a predecessor mailbox delivers Welcomes only (rules 4 and 5)", () => {
    const events = [{ kind: "contact-request" }, { kind: "welcome" }, { kind: "contact-ack" }];
    expect(predecessorMailboxEvents(events)).toEqual([{ kind: "welcome" }]);
  });
});

// ---------------------------------------------------------------------------
// A / E. The proven successor, and where a contact request goes
// ---------------------------------------------------------------------------

describe("the proven successor of a key", () => {
  const base = { oldKeyB64: "OLD", newKeyB64: "NEW", bundleKeyB64: null, proofAuthorisesKeyB64: null };

  it("the pin store's proven chain suffices", () => {
    expect(isProvenSuccessor({ ...base, resolvedOldKeyB64: "NEW" })).toBe(true);
  });

  it("the published bundle's proof, verified under the old key, suffices", () => {
    expect(
      isProvenSuccessor({ ...base, resolvedOldKeyB64: "OLD", bundleKeyB64: "NEW", proofAuthorisesKeyB64: "NEW" }),
    ).toBe(true);
  });

  it("an unproven mismatch is not a successor", () => {
    expect(isProvenSuccessor({ ...base, resolvedOldKeyB64: "OLD" })).toBe(false);
    // A bundle with no (valid) proof.
    expect(isProvenSuccessor({ ...base, resolvedOldKeyB64: "OLD", bundleKeyB64: "NEW" })).toBe(false);
    // A proof that authorises a third key, or a bundle that publishes one.
    expect(
      isProvenSuccessor({ ...base, resolvedOldKeyB64: "OLD", bundleKeyB64: "NEW", proofAuthorisesKeyB64: "X" }),
    ).toBe(false);
    expect(
      isProvenSuccessor({ ...base, resolvedOldKeyB64: "OLD", bundleKeyB64: "X", proofAuthorisesKeyB64: "NEW" }),
    ).toBe(false);
    // The pin store resolving the old key to someone ELSE proves nothing.
    expect(isProvenSuccessor({ ...base, resolvedOldKeyB64: "OTHER" })).toBe(false);
  });

  it("a key is never its own successor", () => {
    expect(
      isProvenSuccessor({
        oldKeyB64: "K",
        newKeyB64: "K",
        resolvedOldKeyB64: "K",
        bundleKeyB64: "K",
        proofAuthorisesKeyB64: "K",
      }),
    ).toBe(false);
  });
});

describe("a contact request to a key in hand, after the gate (rule 1)", () => {
  it("the published key: as supplied", () => {
    expect(
      requestTargetAfterFetch({ suppliedKeyB64: "K", publishedKeyB64: "K", publishedIsProvenSuccessor: false }),
    ).toBe("as-supplied");
  });
  it("a proven predecessor: the CURRENT keys, never the old one", () => {
    expect(
      requestTargetAfterFetch({ suppliedKeyB64: "OLD", publishedKeyB64: "NEW", publishedIsProvenSuccessor: true }),
    ).toBe("current");
  });
  it("any other key: nothing is sent", () => {
    expect(
      requestTargetAfterFetch({ suppliedKeyB64: "OLD", publishedKeyB64: "NEW", publishedIsProvenSuccessor: false }),
    ).toBe("refuse");
  });
});

// ---------------------------------------------------------------------------
// D. A refused write re-checks the address once (rule 6)
// ---------------------------------------------------------------------------

const refused = (): Error => Object.assign(new Error("HTTP 401"), { status: 401 });

const box = (n: number): Uint8Array => new Uint8Array(32).fill(n);

/** A candidate whose verified binding names the mailbox `mailbox` (ADR-0078:
 *  each candidate carries its own target). */
function candidate(name: string, released: string[], mailbox: Uint8Array = box(1)): WriteCandidate {
  return {
    kind: "kept",
    resolve: async (): Promise<ResolvedWriteAuth> => ({
      kind: "kept",
      target: { address: mailbox, mailboxPk: mailbox },
      auth: { signer: { name } as never, delegation: null },
      release: () => released.push(name),
    }),
  };
}

/** A candidate whose binding did not verify: resolves to `null`. */
function refusedBinding(): WriteCandidate {
  return { kind: "kept", resolve: async () => null };
}
const nameOf = (auth: { signer?: unknown }): string => (auth.signer as { name: string }).name;

describe("a refused write re-checks the address once", () => {
  it("the first refusal re-checks, re-resolves and the retry lands in the new mailbox", async () => {
    const released: string[] = [];
    let known = false;
    const plan = vi.fn(async (): Promise<WritePlan> =>
      known
        ? { candidates: [candidate("new", released, box(2))] }
        : { candidates: [candidate("new", released), candidate("old", released)] },
    );
    const recheck = vi.fn(async () => {
      known = true;
    });
    const seen: string[] = [];
    const out = await writeWithAddressRecheck(
      plan,
      async (target, auth) => {
        seen.push(`${target.address[0]}:${nameOf(auth)}`);
        // The new grant opens only the new mailbox.
        if (target.address[0] === 1 && nameOf(auth) === "new") throw refused();
        return 7;
      },
      { recheck },
    );
    expect(out).toBe(7);
    expect(recheck).toHaveBeenCalledTimes(1);
    expect(plan).toHaveBeenCalledTimes(2);
    expect(seen).toEqual(["1:new", "2:new"]);
    expect(released).toEqual(["new", "new"]);
  });

  it("when the re-check learns nothing, the grant kept from before the rotation still opens the old mailbox", async () => {
    const released: string[] = [];
    const plan = async (): Promise<WritePlan> => ({
      candidates: [candidate("new", released), candidate("old", released)],
    });
    const recheck = vi.fn(async () => {
      throw new Error("offline");
    });
    const seen: string[] = [];
    const out = await writeWithAddressRecheck(
      plan,
      async (target, auth) => {
        seen.push(`${target.address[0]}:${nameOf(auth)}`);
        if (nameOf(auth) === "new") throw refused();
        return "stored";
      },
      { recheck },
    );
    expect(out).toBe("stored");
    expect(recheck).toHaveBeenCalledTimes(1);
    // new (refused) -> re-check -> the plan again: new (refused), then old.
    expect(seen).toEqual(["1:new", "1:new", "1:old"]);
  });

  it("re-checks at most once, then gives the refusal back", async () => {
    const recheck = vi.fn(async () => undefined);
    const err = refused();
    const write = vi.fn(async () => {
      throw err;
    });
    await expect(
      writeWithAddressRecheck(
        async () => ({ candidates: [candidate("a", []), candidate("b", [])] }),
        write,
        { recheck },
      ),
    ).rejects.toBe(err);
    expect(recheck).toHaveBeenCalledTimes(1);
    // a (refused), re-check, then a and b of the second plan.
    expect(write).toHaveBeenCalledTimes(3);
  });

  it("a Welcome that nothing opens is kept owed, not thrown as a refusal", async () => {
    await expect(
      writeWithAddressRecheck(
        async () => ({ candidates: [candidate("a", [])] }),
        async () => {
          throw refused();
        },
        { recheck: async () => undefined, unreachable: true },
      ),
    ).rejects.toBeInstanceOf(MailboxUnreachableError);
    // No grant at all: nothing is written and nothing is re-checked.
    const recheck = vi.fn(async () => undefined);
    const write = vi.fn(async () => 1);
    await expect(
      writeWithAddressRecheck(async () => ({ candidates: [] }), write, {
        recheck,
        unreachable: true,
      }),
    ).rejects.toBeInstanceOf(MailboxUnreachableError);
    expect(write).not.toHaveBeenCalled();
    expect(recheck).not.toHaveBeenCalled();
  });

  it("a failure that is not a refusal is thrown at once, with no re-check", async () => {
    const offline = Object.assign(new Error("offline"), { status: 503 });
    const recheck = vi.fn(async () => undefined);
    await expect(
      writeWithAddressRecheck(
        async () => ({ candidates: [candidate("a", []), candidate("b", [])] }),
        async () => {
          throw offline;
        },
        { recheck },
      ),
    ).rejects.toBe(offline);
    expect(recheck).not.toHaveBeenCalled();
  });

  it("without a re-check the candidates are simply tried in order", async () => {
    const seen: string[] = [];
    const out = await writeWithAddressRecheck(
      async () => ({ candidates: [candidate("a", []), candidate("b", [])] }),
      async (_t, auth) => {
        seen.push(nameOf(auth));
        if (nameOf(auth) === "a") throw refused();
        return 1;
      },
      { recheck: null },
    );
    expect(out).toBe(1);
    expect(seen).toEqual(["a", "b"]);
  });

  it("ADR-0078: a candidate whose binding does not verify is skipped, never written", async () => {
    const seen: string[] = [];
    const out = await writeWithAddressRecheck(
      async () => ({ candidates: [refusedBinding(), candidate("b", [])] }),
      async (_t, auth) => {
        seen.push(nameOf(auth));
        return 2;
      },
      { recheck: null },
    );
    expect(out).toBe(2);
    expect(seen).toEqual(["b"]);
    // Nothing verifies: nothing is written, and the write is unreachable.
    const write = vi.fn(async () => 1);
    await expect(
      writeWithAddressRecheck(async () => ({ candidates: [refusedBinding()] }), write, { recheck: null }),
    ).rejects.toBeInstanceOf(MailboxUnreachableError);
    expect(write).not.toHaveBeenCalled();
  });
});

describe("a grant of before a rotation is kept beside the newer one", () => {
  const g = (wire: string, version: number) => ({ wireB64: wire, version, receivedAtMs: 0 });

  it("the filing decision", () => {
    expect(grantFiling(g("a", 1), undefined, undefined)).toBe("replace");
    expect(grantFiling(g("b", 2), g("a", 1), undefined)).toBe("replace");
    expect(grantFiling(g("a", 1), g("a", 1), undefined)).toBe("ignore");
    expect(grantFiling(g("a", 1), g("b", 2), undefined)).toBe("ignore");
    // The same version under another key: the account keys were rotated.
    expect(grantFiling(g("b", 1), g("a", 1), undefined)).toBe("replace-keep-previous");
    // The replaced grant replayed never puts the old key back in front.
    expect(grantFiling(g("a", 1), g("b", 1), { ...g("a", 1), addressHex: "old" })).toBe("ignore");
  });

  it("the old grant is offered last, and only while the old address stands", () => {
    let store = fileContactGrant(EMPTY_RECEIVED_GRANTS, "old", g("G-old", 3));
    store = fileScopedGrant(store, "old", "s1", "group", g("S-old", 1));
    // The re-sent profile arrives before the proven change is observed.
    store = fileContactGrant(store, "old", g("G-new", 3));
    store = fileScopedGrant(store, "old", "s1", "group", g("S-new", 1));
    expect(store.people["old"]?.contact?.wireB64).toBe("G-new");
    expect(store.people["old"]?.previous?.contact).toMatchObject({ wireB64: "G-old", addressHex: "old" });
    // Still addressed by the old key: the new grants first, the old ones after.
    expect(grantsInOrderForPerson(store, ["old"], { scopeHex: "s1" }).map((x) => x.wireB64)).toEqual([
      "G-new",
      "S-new",
      "G-old",
      "S-old",
    ]);
    // The proven change is observed: the address moved, the old ones are gone.
    expect(grantsInOrderForPerson(store, ["new", "old"], { scopeHex: "s1" }).map((x) => x.wireB64)).toEqual([
      "G-new",
      "S-new",
    ]);
  });

  it("a plain re-key (a higher version) keeps nothing of before", () => {
    let store = fileContactGrant(EMPTY_RECEIVED_GRANTS, "p", g("a", 1));
    store = fileContactGrant(store, "p", g("b", 1));
    store = fileContactGrant(store, "p", g("c", 2));
    expect(store.people["p"]?.previous).toBeUndefined();
    expect(grantsInOrderForPerson(store, ["p"]).map((x) => x.wireB64)).toEqual(["c"]);
  });

  it("a withdrawn class, a left scope and a removed contact take the kept grant with them", () => {
    let store = fileContactGrant(EMPTY_RECEIVED_GRANTS, "p", g("c1", 1));
    store = fileContactGrant(store, "p", g("c2", 1));
    store = fileScopedGrant(store, "p", "s", "group", g("g1", 1));
    store = fileScopedGrant(store, "p", "s", "group", g("g2", 1));
    store = fileScopedGrant(store, "p", "s", "member", g("m1", 1));
    store = fileScopedGrant(store, "p", "s", "member", g("m2", 1));
    const withdrawn = withdrawMissingScopedGrants(store, "p", "s", new Set(["member"] as const));
    expect(withdrawn.people["p"]?.previous?.scopes?.["s"]).toEqual({
      member: { ...g("m1", 1), addressHex: "p" },
    });
    expect(dropScope(store, "s").people["p"]?.previous?.scopes).toBeUndefined();
    expect(dropContactGrant(store, "p").people["p"]?.previous?.contact).toBeUndefined();
  });
});

// ---------------------------------------------------------------------------
// F. What was granted before the rotation is spent (rule 5)
// ---------------------------------------------------------------------------

describe("one pending contact request per person", () => {
  const succ = (pairs: string[]) => (o: string, n: string) => pairs.includes(`${o}>${n}`);

  it("a request from the proven successor of a pending request's key replaces it", () => {
    expect(
      pendingAfterArrival(
        [
          { messageId: 1, keyB64: "OLD" },
          { messageId: 2, keyB64: "X" },
        ],
        "NEW",
        succ(["OLD>NEW"]),
      ),
    ).toEqual({ replaces: [1], dropIncoming: false });
  });

  it("without a proof nothing is replaced (two rows, as before)", () => {
    expect(pendingAfterArrival([{ messageId: 1, keyB64: "OLD" }], "NEW", succ([]))).toEqual({
      replaces: [],
      dropIncoming: false,
    });
  });

  it("a stale request from a superseded key is dropped and replaces nothing", () => {
    expect(pendingAfterArrival([{ messageId: 5, keyB64: "NEW" }], "OLD", succ(["OLD>NEW"]))).toEqual({
      replaces: [],
      dropIncoming: true,
    });
  });

  it("the replacement and the removal land in ONE write, with no second row", () => {
    const store = new Map<string, string>();
    const writes: string[] = [];
    const storage = {
      getItem: (k: string) => store.get(k) ?? null,
      setItem: (k: string, v: string) => {
        writes.push(v);
        store.set(k, v);
      },
      removeItem: (k: string) => void store.delete(k),
      key: () => null,
      length: 0,
    };
    const inbox = createPendingInbox({
      accountIdB64: "acct",
      isAccepted: () => false,
      onChange: () => undefined,
      storage,
    });
    const req = (id: number, key: number): IncomingContactRequest =>
      ({ messageId: id, fromPubkey: new Uint8Array(32).fill(key) }) as IncomingContactRequest;
    const row = (id: number) => ({ messageId: id, payload: new Uint8Array([id]), createdAt: "t" });
    inbox.addFetched([{ request: req(1, 1), row: row(1) }]);
    writes.length = 0;
    inbox.addFetched([{ request: req(2, 2), row: row(2), replaces: [1] }]);
    expect(writes).toHaveLength(1);
    expect(inbox.pending().map((r) => r.messageId)).toEqual([2]);
    expect(JSON.parse(writes[0]!).pending.map((r: { messageId: number }) => r.messageId)).toEqual([2]);
  });
});

describe("the rotating device's owed steps", () => {
  it("only an unreachable server keeps a step owed", () => {
    expect(spentStepVerdict([])).toBe("done");
    expect(spentStepVerdict(["done", "skipped"])).toBe("done");
    expect(spentStepVerdict(["done", "unreachable"])).toBe("later");
  });

  it("a failure is `unreachable` only for the network, a 5xx, 408 or 429", () => {
    expect(spentFailureOutcome(Object.assign(new Error("x"), { status: 503 }))).toBe("unreachable");
    expect(spentFailureOutcome(Object.assign(new Error("x"), { status: 429 }))).toBe("unreachable");
    expect(spentFailureOutcome(Object.assign(new Error("x"), { status: 401 }))).toBe("skipped");
    expect(spentFailureOutcome(Object.assign(new Error("x"), { status: 404 }))).toBe("skipped");
    expect(spentFailureOutcome(new TypeError("Failed to fetch"))).toBe("unreachable");
    const network = Object.assign(new Error("n"), { name: "DsNetworkError" });
    // Found through a wrapper's cause chain.
    expect(spentFailureOutcome({ variant: { kind: "wrapped", cause: network } })).toBe("unreachable");
    expect(spentFailureOutcome(new Error("keys reset"))).toBe("skipped");
    expect(spentFailureOutcome("text")).toBe("skipped");
  });

  it("a link is revoked under a predecessor seed, the newest first, never the current one", () => {
    expect(revocationSeedOrder(["k1", "k2"])).toEqual(["k2", "k1"]);
    expect(linkRevokeOutcome(["not-owner", "revoked"])).toBe("done");
    expect(linkRevokeOutcome(["unreachable"])).toBe("unreachable");
    // No predecessor signed it: a link minted after the rotation. Left alone.
    expect(linkRevokeOutcome(["not-owner", "not-owner"])).toBe("skipped");
    expect(linkRevokeOutcome([])).toBe("skipped");
  });

  it("only requests sent before the keys were changed are sent again, once", () => {
    const r = (id: string, sentAtMs: number) => ({
      recipientUsername: id,
      recipientAccountIdHex: id,
      sentAtMs,
    });
    const waiting = [r("b", 200), r("a", 100), r("c", 900)];
    expect(requestsToResend(waiting, 500).map((x) => x.recipientAccountIdHex)).toEqual(["a", "b"]);
    // After the re-send the entry carries a later time: not sent twice.
    expect(requestsToResend([r("a", 600), r("b", 700)], 500)).toEqual([]);
    expect(rotatedAtMsOf({ startedAtMs: 10, preparedAtMs: 20, signedAtMs: 30 })).toBe(30);
    expect(rotatedAtMsOf({ startedAtMs: 10 })).toBe(10);
  });
});
