// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * ADR-0073 R.6 / R.7 - claim, then build, driven through a fake port
 * (`continuity-runtime.ts`). What these pin is the ORDER: nothing reaches
 * anyone before the claim is confirmed, a lost claim discards, and every
 * step can be re-entered.
 */
import { describe, expect, it } from "vitest";

import {
  continueConversation,
  mayRecheckCompleteness,
  type ContinuityFacts,
  type ContinuityPort,
} from "./continuity-runtime";
import { NO_CLAIMS, type StoredGroupClosed } from "./group-closed-store";
import type { SuccessorClaim } from "./successor-claim";

const ME = "me";
const OLD_ANNA = "anna-old";
const NEW_ANNA = "anna-new";
const BOB = "bob";
const CAROL = "carol";
const ANNA_ACCOUNT = "acct-anna";
const OLD = "G-old";

interface World {
  facts: Map<string, ContinuityFacts>;
  claims: Map<string, StoredGroupClosed>;
  cursor: number | undefined;
  devices: boolean;
  log: string[];
  nextClaimId: number;
  postFails: boolean;
  deliverFails: boolean;
  /** Whether a Welcome can reach the new identity's mailbox. */
  reachable: boolean;
  unreachable: Set<string>;
  recheckDue: boolean;
}

function world(kind: "chat" | "group", over: Partial<ContinuityFacts> = {}): World {
  const facts: ContinuityFacts =
    kind === "chat"
      ? {
          kind,
          rosterPersonKeysB64: [ME, OLD_ANNA],
          ownerKeyB64: ME,
          adminKeysB64: [],
          active: true,
          ...over,
        }
      : {
          kind,
          rosterPersonKeysB64: [OLD_ANNA, ME, BOB, CAROL],
          ownerKeyB64: OLD_ANNA,
          adminKeysB64: [ME, BOB],
          active: true,
          ...over,
        };
  return {
    facts: new Map([[OLD, facts]]),
    claims: new Map(),
    cursor: 0,
    devices: true,
    log: [],
    nextClaimId: 40,
    postFails: false,
    deliverFails: false,
    reachable: true,
    unreachable: new Set(),
    recheckDue: true,
  };
}

function port(w: World): ContinuityPort {
  const state = (g: string): StoredGroupClosed => w.claims.get(g) ?? NO_CLAIMS;
  return {
    ownPersonKeyB64: () => ME,
    supersededKeys: () => new Map([[OLD_ANNA, ANNA_ACCOUNT]]),
    currentKeyOf: (a) => (a === ANNA_ACCOUNT ? NEW_ANNA : null),
    facts: (g) => w.facts.get(g),
    claimState: state,
    cursorOf: () => w.cursor,
    newIdentityHasDevices: async () => w.devices,
    newIdentityReachable: async () => w.reachable,
    prepareSuccessor: async () => {
      w.log.push("prepare");
      return "S-mine";
    },
    postClaim: async (g, successor) => {
      w.claims.set(g, { ...state(g), own: { successorGroupIdB64: successor, messageId: null } });
      if (w.postFails) throw new Error("offline");
      const id = w.nextClaimId;
      w.claims.set(g, { ...state(g), own: { successorGroupIdB64: successor, messageId: id } });
      w.log.push(`claim:${successor}`);
      return id;
    },
    abandonOwnClaim: async (g) => {
      w.claims.set(g, { ...state(g), own: null });
      w.log.push("abandon");
    },
    discardSuccessor: async (s) => {
      w.log.push(`discard:${s}`);
    },
    settle: async (g, winner) => {
      w.claims.set(g, { accepted: winner, pending: [], own: null });
      w.log.push(`settle:${winner.successorGroupIdB64}`);
    },
    recordLineage: async (_g, s) => {
      if (!w.log.includes(`lineage:${s}`)) w.log.push(`lineage:${s}`);
    },
    deliverSuccessor: async (_g, s) => {
      if (w.deliverFails) return "not-ready";
      if (!w.reachable) return "unreachable";
      if (!w.facts.has(s)) {
        w.facts.set(s, {
          kind: w.facts.get(OLD)!.kind,
          rosterPersonKeysB64: [ME, NEW_ANNA],
          ownerKeyB64: ME,
          adminKeysB64: [],
          active: true,
        });
        w.log.push(`deliver:${s}`);
      }
      return "delivered";
    },
    carryName: async () => {
      if (!w.log.includes("name")) w.log.push("name");
    },
    addPerson: async (s, person) => {
      if (w.unreachable.has(person)) {
        w.log.push(`add-failed:${person}`);
        return false;
      }
      const f = w.facts.get(s)!;
      w.facts.set(s, { ...f, rosterPersonKeysB64: [...f.rosterPersonKeysB64, person] });
      w.log.push(`add:${person}`);
      return true;
    },
    recheckDue: () => w.recheckDue,
    setAdmins: async (s, admins) => {
      const f = w.facts.get(s)!;
      w.facts.set(s, { ...f, adminKeysB64: [...admins] });
      w.log.push(`admins:${[...admins].sort().join(",")}`);
    },
    transferOwnership: async (s, newOwner, stayAdmin) => {
      const f = w.facts.get(s)!;
      w.facts.set(s, {
        ...f,
        ownerKeyB64: newOwner,
        adminKeysB64: stayAdmin ? [...new Set([...f.adminKeysB64, ME])] : f.adminKeysB64,
      });
      w.log.push(`transfer:${newOwner}:${stayAdmin ? "admin" : "member"}`);
    },
    resendOwnMessages: async (_g, s) => {
      if (!w.log.includes(`resend:${s}`)) w.log.push(`resend:${s}`);
    },
  };
}

function claim(messageId: number, successor: string, sender = ME): SuccessorClaim {
  return {
    messageId,
    successorGroupIdB64: successor,
    senderKeyB64: sender,
    senderWasAdmin: true,
    adminKeysAtReceiptB64: [ME, BOB],
  };
}

describe("a one-to-one whose other person reset (R.7 case 1)", () => {
  it("claims first, and sends NOTHING until the journal confirms the claim", async () => {
    const w = world("chat");
    expect(await continueConversation(port(w), OLD)).toBe("claimed");
    expect(w.log).toEqual(["prepare", "claim:S-mine"]);
    // The old journal has not been read up to the claim yet.
    w.cursor = 39;
    expect(await continueConversation(port(w), OLD)).toBe("waiting-for-journal");
    expect(w.log).toEqual(["prepare", "claim:S-mine"]);
  });

  it("wins when no earlier eligible claim is in the journal, then builds and re-sends", async () => {
    const w = world("chat");
    await continueConversation(port(w), OLD);
    w.cursor = 40;
    expect(await continueConversation(port(w), OLD)).toBe("built");
    expect(w.log).toEqual([
      "prepare",
      "claim:S-mine",
      "settle:S-mine",
      "deliver:S-mine",
      "lineage:S-mine",
      "resend:S-mine",
    ]);
    // A one-to-one has no name to carry, nobody to add, no roles.
    expect(w.log.some((l) => l.startsWith("add") || l === "name" || l.startsWith("transfer"))).toBe(false);
  });

  it("loses to an earlier claim of the person's other device: discards and adopts", async () => {
    const w = world("chat");
    await continueConversation(port(w), OLD);
    // The other device's claim was earlier in the journal and was accepted
    // by the receive rule while this device read up to its own.
    w.claims.set(OLD, { ...w.claims.get(OLD)!, accepted: claim(37, "S-theirs") });
    w.cursor = 40;
    expect(await continueConversation(port(w), OLD)).toBe("lost");
    expect(w.log).toEqual([
      "prepare",
      "claim:S-mine",
      "discard:S-mine",
      "settle:S-theirs",
      "lineage:S-theirs",
    ]);
    // Nothing of the lost successor ever reached anyone.
    expect(w.log.some((l) => l.startsWith("deliver"))).toBe(false);
    expect(w.claims.get(OLD)?.own).toBeNull();
  });

  it("stands down when a claim is already held, and records the lineage", async () => {
    const w = world("chat");
    w.claims.set(OLD, { accepted: claim(12, "S-theirs", "someone-else"), pending: [], own: null });
    expect(await continueConversation(port(w), OLD)).toBe("adopted");
    expect(w.log).toEqual(["lineage:S-theirs"]);
  });

  it("waits while the new identity publishes no device (R.8)", async () => {
    const w = world("chat");
    w.devices = false;
    expect(await continueConversation(port(w), OLD)).toBe("waiting-for-devices");
    expect(w.log).toEqual([]);
  });

  it("prepares and claims NOTHING while the new identity cannot be reached (2026-11-25)", async () => {
    const w = world("chat");
    w.reachable = false;
    expect(await continueConversation(port(w), OLD)).toBe("waiting-for-reach");
    expect(w.log).toEqual([]);
    expect(w.claims.get(OLD)).toBeUndefined();
    // Once a grant of the new key is held (their contact request, UX 16.6),
    // the next pass continues from the top.
    w.reachable = true;
    expect(await continueConversation(port(w), OLD)).toBe("claimed");
  });

  it("an own claim that cannot be delivered never moves the conversation", async () => {
    const w = world("chat");
    await continueConversation(port(w), OLD);
    w.cursor = 40;
    w.reachable = false;
    expect(await continueConversation(port(w), OLD)).toBe("waiting-for-reach");
    // Settled, but no lineage and no held successor: the row stays the old
    // one-to-one and its sends stay held.
    expect(w.log).toEqual(["prepare", "claim:S-mine", "settle:S-mine"]);
    expect(await continueConversation(port(w), OLD)).toBe("waiting-for-reach");
    expect(w.log.some((l) => l.startsWith("lineage") || l.startsWith("deliver"))).toBe(false);
    w.reachable = true;
    expect(await continueConversation(port(w), OLD)).toBe("built");
    expect(w.log.slice(3)).toEqual(["deliver:S-mine", "lineage:S-mine", "resend:S-mine"]);
  });

  it("a claim of this account's other device records no lineage before this device holds it", async () => {
    const w = world("chat");
    w.claims.set(OLD, { accepted: claim(12, "S-sibling"), pending: [], own: null });
    w.deliverFails = true;
    expect(await continueConversation(port(w), OLD)).toBe("failed");
    expect(w.log).toEqual([]);
  });

  it("a claim whose publish failed leaves nothing behind", async () => {
    const w = world("chat");
    w.postFails = true;
    expect(await continueConversation(port(w), OLD)).toBe("failed");
    expect(w.log).toEqual(["prepare", "abandon", "discard:S-mine"]);
    expect(w.claims.get(OLD)?.own).toBeNull();
    // The next trigger starts over.
    w.postFails = false;
    expect(await continueConversation(port(w), OLD)).toBe("claimed");
  });

  it("a claim interrupted before its id was recorded is posted again, same successor", async () => {
    const w = world("chat");
    w.claims.set(OLD, { accepted: null, pending: [], own: { successorGroupIdB64: "S-mine", messageId: null } });
    expect(await continueConversation(port(w), OLD)).toBe("claimed");
    expect(w.log).toEqual(["claim:S-mine"]);
  });

  it("a failed delivery is resumed by the next pass", async () => {
    const w = world("chat");
    await continueConversation(port(w), OLD);
    w.cursor = 40;
    w.deliverFails = true;
    expect(await continueConversation(port(w), OLD)).toBe("failed");
    w.deliverFails = false;
    expect(await continueConversation(port(w), OLD)).toBe("built");
    expect(w.log.filter((l) => l === "deliver:S-mine")).toHaveLength(1);
    expect(w.log.filter((l) => l === "resend:S-mine")).toHaveLength(1);
  });

  it("does nothing when nothing was superseded in this conversation", async () => {
    const w = world("chat", { rosterPersonKeysB64: [ME, BOB] });
    expect(await continueConversation(port(w), OLD)).toBe("nothing");
  });

  it("an ended membership founds nothing", async () => {
    const w = world("chat", { active: false });
    expect(await continueConversation(port(w), OLD)).toBe("not-eligible");
    expect(w.log).toEqual([]);
  });
});

describe("a group whose owner reset (R.7 case 3)", () => {
  async function winAsAdmin(w: World): Promise<void> {
    expect(await continueConversation(port(w), OLD)).toBe("claimed");
    w.cursor = 40;
  }

  it("ends with the same people, the same roles and the same owner", async () => {
    const w = world("group");
    await winAsAdmin(w);
    // Pass 1: deliver, name, the remaining people, the admin set.
    expect(await continueConversation(port(w), OLD)).toBe("built");
    expect(w.log).toEqual([
      "prepare",
      "claim:S-mine",
      "settle:S-mine",
      "deliver:S-mine",
      "lineage:S-mine",
      "name",
      "add:bob",
      "add:carol",
      "admins:bob",
    ]);
    // Pass 2 (one role change per sweep): the owner is restored, and the
    // founder, an admin before, stays one.
    expect(await continueConversation(port(w), OLD)).toBe("built");
    expect(w.log.at(-1)).toBe("transfer:anna-new:admin");
    const successor = w.facts.get("S-mine")!;
    expect([...successor.rosterPersonKeysB64].sort()).toEqual([BOB, CAROL, ME, NEW_ANNA].sort());
    expect(successor.ownerKeyB64).toBe(NEW_ANNA);
    expect([...successor.adminKeysB64].sort()).toEqual([BOB, ME].sort());
    // Pass 3: nothing left to do; no step repeats.
    const before = w.log.length;
    expect(await continueConversation(port(w), OLD)).toBe("built");
    expect(w.log.length).toBe(before);
  });

  it("ADR-0101 walk D4: a founder that was a plain member holds the hand-back until the history was served", async () => {
    const w = world("group");
    // The founder was a plain member of the old group (the walk: A owned,
    // B was a member); only the owner, who reset, held a role.
    const old = w.facts.get(OLD)!;
    w.facts.set(OLD, { ...old, adminKeysB64: [] });
    let due = false;
    const p = { ...port(w), ownershipTransferDue: () => due };
    expect(await continueConversation(p, OLD)).toBe("claimed");
    w.cursor = 40;
    expect(await continueConversation(p, OLD)).toBe("built");
    expect(w.log).toContain("name");
    expect(w.log.some((l) => l.startsWith("transfer:"))).toBe(false);
    due = true;
    expect(await continueConversation(p, OLD)).toBe("built");
    expect(w.log.at(-1)).toBe("transfer:anna-new:member");
  });

  it("keeps adding a member it could not reach (the completeness re-check)", async () => {
    const w = world("group");
    w.unreachable.add(CAROL);
    await winAsAdmin(w);
    await continueConversation(port(w), OLD);
    expect(w.log).toContain("add-failed:carol");
    expect(w.facts.get("S-mine")!.rosterPersonKeysB64).not.toContain(CAROL);
    w.unreachable.clear();
    await continueConversation(port(w), OLD);
    expect(w.log).toContain("add:carol");
  });

  it("a plain member does not found while another admin exists", async () => {
    const w = world("group", { adminKeysB64: [BOB] });
    expect(await continueConversation(port(w), OLD)).toBe("not-eligible");
    expect(w.log).toEqual([]);
  });

  it("a member that adopted another admin's successor builds nothing", async () => {
    const w = world("group", { adminKeysB64: [BOB] });
    w.claims.set(OLD, { accepted: claim(9, "S-bob", BOB), pending: [], own: null });
    expect(await continueConversation(port(w), OLD)).toBe("adopted");
    expect(w.log).toEqual(["lineage:S-bob"]);
  });

  it("a group where the superseded key is only a member is not continued here", async () => {
    const w = world("group", { ownerKeyB64: ME });
    expect(await continueConversation(port(w), OLD)).toBe("nothing");
  });
});

describe("the completeness re-check of a moved admin that did not found (R.7 case 3)", () => {
  /** Bob founded `S-bob`; this device (ME) adopted it and has joined it. */
  function adopted(ownRoleInSuccessor: "admin" | "owner" | "member", successorRoster: string[]): World {
    const w = world("group", { adminKeysB64: [ME, BOB] });
    w.claims.set(OLD, { accepted: claim(9, "S-bob", BOB), pending: [], own: null });
    w.facts.set("S-bob", {
      kind: "group",
      rosterPersonKeysB64: successorRoster,
      ownerKeyB64: ownRoleInSuccessor === "owner" ? ME : BOB,
      adminKeysB64: ownRoleInSuccessor === "admin" ? [ME] : [],
      active: true,
    });
    return w;
  }

  it("a moved admin adds whoever of the old roster the successor lacks", async () => {
    const w = adopted("admin", [BOB, ME, NEW_ANNA]);
    expect(await continueConversation(port(w), OLD)).toBe("rechecked");
    expect(w.log).toContain("add:carol");
    // Never the superseded key, never itself, nobody already there.
    expect(w.log.filter((l) => l.startsWith("add:"))).toEqual(["add:carol"]);
  });

  it("a moved owner does the same", async () => {
    const w = adopted("owner", [BOB, ME]);
    expect(await continueConversation(port(w), OLD)).toBe("rechecked");
    expect(w.log).toContain("add:carol");
  });

  it("a moved plain member adds nobody", async () => {
    const w = adopted("member", [BOB, ME]);
    expect(await continueConversation(port(w), OLD)).toBe("adopted");
    expect(w.log.some((l) => l.startsWith("add"))).toBe(false);
  });

  it("a device that has not joined the successor yet adds nobody", async () => {
    const w = adopted("admin", [BOB]);
    expect(await continueConversation(port(w), OLD)).toBe("adopted");
    expect(w.log.some((l) => l.startsWith("add"))).toBe(false);
  });

  it("runs at the arm's cadence, and not at all when nobody is missing", async () => {
    const w = adopted("admin", [BOB, ME]);
    w.recheckDue = false;
    expect(await continueConversation(port(w), OLD)).toBe("adopted");
    expect(w.log.some((l) => l.startsWith("add"))).toBe(false);
    const full = adopted("admin", [BOB, ME, CAROL, NEW_ANNA]);
    expect(await continueConversation(port(full), OLD)).toBe("adopted");
  });

  it("mayRecheckCompleteness: the table", () => {
    const facts = (over: Partial<ContinuityFacts>): ContinuityFacts => ({
      kind: "group",
      rosterPersonKeysB64: [ME, BOB],
      ownerKeyB64: BOB,
      adminKeysB64: [],
      active: true,
      ...over,
    });
    expect(mayRecheckCompleteness(undefined, ME)).toBe(false);
    expect(mayRecheckCompleteness(facts({}), ME)).toBe(false);
    expect(mayRecheckCompleteness(facts({ adminKeysB64: [ME] }), ME)).toBe(true);
    expect(mayRecheckCompleteness(facts({ ownerKeyB64: ME }), ME)).toBe(true);
    expect(mayRecheckCompleteness(facts({ adminKeysB64: [ME], active: false }), ME)).toBe(false);
    expect(mayRecheckCompleteness(facts({ adminKeysB64: [ME], rosterPersonKeysB64: [BOB] }), ME)).toBe(false);
  });
});

describe("ADR-0101 section 8: the profile comes back with the conversation", () => {
  it("is carried after the delivery and the lineage, never before", async () => {
    const w = world("chat");
    w.cursor = 100;
    w.claims.set(OLD, { ...NO_CLAIMS, own: { successorGroupIdB64: "S-mine", messageId: 40 } });
    const p = port(w);
    const withProfile: ContinuityPort = {
      ...p,
      carryProfile: async (_g, s, acct) => {
        w.log.push(`profile:${s}:${acct}`);
      },
    };
    expect(await continueConversation(withProfile, OLD)).toBe("built");
    const i = w.log.indexOf("deliver:S-mine");
    const j = w.log.indexOf("lineage:S-mine");
    const k = w.log.indexOf(`profile:S-mine:${ANNA_ACCOUNT}`);
    expect(i).toBeGreaterThanOrEqual(0);
    expect(j).toBeGreaterThan(i);
    expect(k).toBeGreaterThan(j);
  });
  it("is not carried while the new identity is unreachable", async () => {
    const w = world("chat");
    w.cursor = 100;
    w.reachable = false;
    w.claims.set(OLD, { ...NO_CLAIMS, own: { successorGroupIdB64: "S-mine", messageId: 40 } });
    const carried: string[] = [];
    const p: ContinuityPort = { ...port(w), carryProfile: async (_g, s) => void carried.push(s) };
    expect(await continueConversation(p, OLD)).toBe("waiting-for-reach");
    expect(carried).toEqual([]);
  });
});
