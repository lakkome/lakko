// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * The removal operation and its owed record (`removal.ts`, ADR-0066
 * section 2.3): the order of the steps, what each answer means, what is kept
 * for a retry, and that a device is never left signed out at auth with
 * nobody coming back for the rest.
 */
import { describe, expect, it } from "vitest";

import {
  chatsVerdict,
  clearRemovals,
  directoryVerdict,
  newRemoval,
  nextStep,
  readRemoval,
  readRemovals,
  remainingSteps,
  removalAllowed,
  removalComplete,
  removalRowState,
  resumesByItself,
  revokeVerdict,
  runRemoval,
  usableAttestation,
  withAttestation,
  withStepDone,
  withWaitingOnPerson,
  writeRemoval,
  type DeviceGroupStepResult,
  type RemovalOwed,
  type RemovalPorts,
} from "./removal";
import type { MarkerStorage } from "../auth/phrase-gate";

const NOW = 1_800_000_000_000;
const DEV = "A".repeat(42) + "B";
const DEV2 = "C".repeat(43);

function memory(): MarkerStorage {
  const rows = new Map<string, string>();
  return {
    getItem: (k) => rows.get(k) ?? null,
    setItem: (k, v) => void rows.set(k, v),
    removeItem: (k) => void rows.delete(k),
  };
}

interface Harness {
  readonly ports: RemovalPorts;
  readonly calls: string[];
  readonly saved: (RemovalOwed | null)[];
  revoke: { status: number | null; errorCode: string | null } | "throw";
  directory: number | null | "throw";
  deviceGroup: DeviceGroupStepResult | "throw";
  chats: { failed: number } | null | "throw";
  attestationSeen: (string | null)[];
}

function harness(): Harness {
  const h: Harness = {
    calls: [],
    saved: [],
    revoke: { status: 204, errorCode: null },
    directory: 204,
    deviceGroup: "removed",
    chats: { failed: 0 },
    attestationSeen: [],
    ports: {
      revoke: async (_id, grant) => {
        h.calls.push(`revoke ${grant}`);
        if (h.revoke === "throw") throw new Error("boom");
        return h.revoke;
      },
      deleteDirectoryRow: async () => {
        h.calls.push("directory");
        if (h.directory === "throw") throw new Error("boom");
        return h.directory;
      },
      removeFromDeviceGroup: async (_id, attestation) => {
        h.calls.push("device-group");
        h.attestationSeen.push(attestation);
        if (h.deviceGroup === "throw") throw new Error("boom");
        return h.deviceGroup;
      },
      retireFromChats: async () => {
        h.calls.push("chats");
        if (h.chats === "throw") throw new Error("boom");
        return h.chats;
      },
      save: (_id, record) => {
        h.saved.push(record);
      },
    },
  };
  return h;
}

const opts = (grant: string | null) => ({ grant, now: () => NOW });
const last = (h: Harness): RemovalOwed | null => h.saved[h.saved.length - 1] ?? null;

describe("the record", () => {
  it("starts with nothing done and runs its steps strictly in order", () => {
    let r = newRemoval(DEV, NOW);
    expect(remainingSteps(r)).toEqual(["revoke", "directory", "device-group", "chats"]);
    expect(nextStep(r)).toBe("revoke");
    r = withStepDone(r, "revoke");
    expect(nextStep(r)).toBe("directory");
    r = withStepDone(r, "directory");
    expect(nextStep(r)).toBe("device-group");
    r = withStepDone(r, "device-group");
    expect(nextStep(r)).toBe("chats");
    r = withStepDone(r, "chats");
    expect(nextStep(r)).toBeNull();
    expect(removalComplete(r)).toBe(true);
  });

  it("never skips an earlier step because a later one is marked", () => {
    const r = withStepDone(newRemoval(DEV, NOW), "chats");
    expect(nextStep(r)).toBe("revoke");
  });

  it("continues by itself only once the device is signed out at the server", () => {
    const fresh = newRemoval(DEV, NOW);
    expect(resumesByItself(fresh)).toBe(false);
    const revoked = withStepDone(fresh, "revoke");
    expect(resumesByItself(revoked)).toBe(true);
    let done = revoked;
    for (const s of ["directory", "device-group", "chats"] as const) done = withStepDone(done, s);
    expect(resumesByItself(done)).toBe(false);
  });

  it("shows Removing... on the row only while the rest is finishing", () => {
    expect(removalRowState(null)).toBe("normal");
    expect(removalRowState(newRemoval(DEV, NOW))).toBe("normal");
    expect(removalRowState(withStepDone(newRemoval(DEV, NOW), "revoke"))).toBe("finishing");
  });

  it("reuses a passkey attestation only inside its lifetime", () => {
    const r: RemovalOwed = {
      ...newRemoval(DEV, NOW),
      attestationB64: "att",
      attestationExpiresAtMs: NOW + 1000,
    };
    expect(usableAttestation(r, NOW)).toBe("att");
    expect(usableAttestation(r, NOW + 1000)).toBeNull();
    expect(usableAttestation(newRemoval(DEV, NOW), NOW)).toBeNull();
  });
});

describe("what the answers mean", () => {
  it("revoke: 204 is done", () => {
    expect(revokeVerdict(204, null)).toEqual({ verdict: "done", failure: null });
  });

  it("revoke: every refusal means nothing was removed, each with its own reason", () => {
    expect(revokeVerdict(403, "fresh-auth-required")).toEqual({
      verdict: "refused",
      failure: "proof-required",
    });
    expect(revokeVerdict(403, "account-key-required")).toEqual({
      verdict: "refused",
      failure: "account-key-required",
    });
    expect(revokeVerdict(429, "too-many-attempts")).toEqual({
      verdict: "refused",
      failure: "rate-limited",
    });
    expect(revokeVerdict(400, null)).toEqual({ verdict: "refused", failure: "other" });
    expect(revokeVerdict(403, null)).toEqual({ verdict: "refused", failure: "other" });
    expect(revokeVerdict(401, "device-revoked")).toEqual({ verdict: "refused", failure: "other" });
  });

  it("revoke: no answer is unconfirmed, not a refusal", () => {
    expect(revokeVerdict(null, null)).toEqual({ verdict: "later", failure: "unconfirmed" });
    expect(revokeVerdict(502, null)).toEqual({ verdict: "later", failure: "unconfirmed" });
  });

  it("directory: only an answer that the row is gone is done; nothing drops the record", () => {
    expect(directoryVerdict(204)).toBe("done");
    expect(directoryVerdict(404)).toBe("done");
    for (const status of [null, 401, 403, 429, 500, 503]) {
      expect(directoryVerdict(status)).toBe("later");
    }
  });

  it("chats: done only when every chat answered", () => {
    expect(chatsVerdict({ failed: 0 })).toBe("done");
    expect(chatsVerdict({ failed: 1 })).toBe("later");
    expect(chatsVerdict(null)).toBe("later");
  });

  it("no removal runs while a key-reset publish is owed", () => {
    expect(removalAllowed({ keyResetPublishOwed: true })).toBe(false);
    expect(removalAllowed({ keyResetPublishOwed: false })).toBe(true);
  });
});

describe("runRemoval", () => {
  it("runs the four steps in order and leaves no record", async () => {
    const h = harness();
    const out = await runRemoval(newRemoval(DEV, NOW), h.ports, opts("grant"));
    expect(out).toEqual({ kind: "complete" });
    expect(h.calls).toEqual(["revoke grant", "directory", "device-group", "chats"]);
    expect(last(h)).toBeNull();
  });

  it("writes the record before the revoke is sent", async () => {
    const h = harness();
    let savedBeforeRevoke = -1;
    const ports: RemovalPorts = {
      ...h.ports,
      revoke: async (id, grant) => {
        savedBeforeRevoke = h.saved.length;
        return h.ports.revoke(id, grant);
      },
    };
    await runRemoval(newRemoval(DEV, NOW), ports, opts("grant"));
    expect(savedBeforeRevoke).toBe(1);
    expect(h.saved[0]?.done.revoke).toBe(false);
  });

  it("stops at a refused revoke: nothing else happens and no record stays", async () => {
    const h = harness();
    h.revoke = { status: 403, errorCode: "account-key-required" };
    const out = await runRemoval(newRemoval(DEV, NOW), h.ports, opts("grant"));
    expect(out).toEqual({ kind: "not-revoked", failure: "account-key-required" });
    expect(h.calls).toEqual(["revoke grant"]);
    expect(last(h)).toBeNull();
  });

  it("keeps the record when the revoke got no answer, and runs nothing else", async () => {
    for (const answer of [{ status: null, errorCode: null }, "throw"] as const) {
      const h = harness();
      h.revoke = answer;
      const out = await runRemoval(newRemoval(DEV, NOW), h.ports, opts("grant"));
      expect(out).toEqual({ kind: "not-revoked", failure: "unconfirmed" });
      expect(h.calls).toEqual(["revoke grant"]);
      const kept = last(h);
      expect(kept?.done.revoke).toBe(false);
      // Not continued without the person: the revoke needs a proof.
      expect(kept !== null && resumesByItself(kept)).toBe(false);
    }
  });

  it("sends nothing and writes nothing without a proof", async () => {
    const h = harness();
    const out = await runRemoval(newRemoval(DEV, NOW), h.ports, opts(null));
    expect(out).toEqual({ kind: "not-revoked", failure: "proof-required" });
    expect(h.calls).toEqual([]);
    expect(h.saved).toEqual([]);
  });

  it("keeps a revoked device owed when the directory step has no answer", async () => {
    for (const answer of [null, 403, 503, "throw"] as const) {
      const h = harness();
      h.directory = answer;
      const out = await runRemoval(newRemoval(DEV, NOW), h.ports, opts("grant"));
      expect(out).toEqual({
        kind: "finishing",
        owed: ["directory", "device-group", "chats"],
        needsGesture: false,
      });
      expect(h.calls).toEqual(["revoke grant", "directory"]);
      const kept = last(h);
      expect(kept?.done).toEqual({ revoke: true, directory: false, deviceGroup: false, chats: false });
      expect(kept !== null && resumesByItself(kept)).toBe(true);
    }
  });

  it("resumes from the record without a proof and without revoking again", async () => {
    const h = harness();
    h.directory = null;
    await runRemoval(newRemoval(DEV, NOW), h.ports, opts("grant"));
    const kept = last(h);
    expect(kept).not.toBeNull();
    if (kept === null) return;

    const again = harness();
    const out = await runRemoval(kept, again.ports, opts(null));
    expect(out).toEqual({ kind: "complete" });
    expect(again.calls).toEqual(["directory", "device-group", "chats"]);
    expect(last(again)).toBeNull();
  });

  it("stays owed while a chat has not answered, and finishes on a later run", async () => {
    const h = harness();
    h.chats = { failed: 2 };
    const out = await runRemoval(newRemoval(DEV, NOW), h.ports, opts("grant"));
    expect(out).toEqual({ kind: "finishing", owed: ["chats"], needsGesture: false });
    const kept = last(h);
    expect(kept?.done).toEqual({ revoke: true, directory: true, deviceGroup: true, chats: false });
    if (kept === null) return;

    const again = harness();
    expect(await runRemoval(kept, again.ports, opts(null))).toEqual({ kind: "complete" });
    expect(again.calls).toEqual(["chats"]);
  });

  it("says when the DeviceGroup step is waiting for the passkey gesture", async () => {
    const h = harness();
    h.deviceGroup = "needs-gesture";
    const out = await runRemoval(newRemoval(DEV, NOW), h.ports, opts("grant"));
    expect(out).toEqual({ kind: "finishing", owed: ["device-group", "chats"], needsGesture: true });
    expect(h.calls).toEqual(["revoke grant", "directory", "device-group"]);
  });

  it("hands the DeviceGroup step a stored attestation only while it is usable", async () => {
    const fresh = harness();
    await runRemoval(
      { ...newRemoval(DEV, NOW), attestationB64: "att", attestationExpiresAtMs: NOW + 1 },
      fresh.ports,
      opts("grant"),
    );
    expect(fresh.attestationSeen).toEqual(["att"]);

    const stale = harness();
    await runRemoval(
      { ...newRemoval(DEV, NOW), attestationB64: "att", attestationExpiresAtMs: NOW },
      stale.ports,
      opts("grant"),
    );
    expect(stale.attestationSeen).toEqual([null]);
  });

  // The T2 tail: a removal that needs the passkey again must be finishable.
  it("stores that the owed step waits on the person, so the row offers Finish removing after a relaunch", async () => {
    const h = harness();
    h.deviceGroup = "needs-gesture";
    await runRemoval(newRemoval(DEV, NOW), h.ports, opts("grant"));
    const stored = last(h);
    expect(stored?.waitsOnPerson).toBe(true);
    expect(stored?.done).toEqual({ revoke: true, directory: true, deviceGroup: false, chats: false });
    // What a relaunch reads back: signed out at auth, still in the
    // DeviceGroup, and a control to finish it.
    const storage = memory();
    writeRemoval(storage, "acct", DEV, stored);
    expect(removalRowState(readRemoval(storage, "acct", DEV))).toBe("needs-person");
  });

  it("an aged-out attestation leads to the same waiting state, never to a row with no control", async () => {
    const h = harness();
    h.deviceGroup = "needs-gesture";
    const revoked = withStepDone(withStepDone(newRemoval(DEV, NOW), "revoke"), "directory");
    const aged = { ...revoked, attestationB64: "att", attestationExpiresAtMs: NOW };
    const out = await runRemoval(aged, h.ports, opts(null));
    expect(out).toEqual({ kind: "finishing", owed: ["device-group", "chats"], needsGesture: true });
    expect(h.attestationSeen).toEqual([null]);
    expect(removalRowState(last(h))).toBe("needs-person");
  });

  it("Finish removing: a new attestation ends the wait and completes the owed steps", async () => {
    const waiting = withWaitingOnPerson(
      withStepDone(withStepDone(newRemoval(DEV, NOW), "revoke"), "directory"),
    );
    expect(removalRowState(waiting)).toBe("needs-person");
    const attached = withAttestation(waiting, "att2", NOW + 60_000);
    expect(attached.waitsOnPerson).toBeUndefined();
    expect(removalRowState(attached)).toBe("finishing");
    const h = harness();
    // No grant: the revoke already has its answer.
    expect(await runRemoval(attached, h.ports, opts(null))).toEqual({ kind: "complete" });
    expect(h.calls).toEqual(["device-group", "chats"]);
    expect(h.attestationSeen).toEqual(["att2"]);
    expect(last(h)).toBeNull();
  });

  it("the waiting mark goes when the DeviceGroup step lands, and a row waits on the person only for that step", () => {
    const waiting = withWaitingOnPerson(withStepDone(newRemoval(DEV, NOW), "revoke"));
    const landed = withStepDone(withStepDone(waiting, "directory"), "device-group");
    expect(landed.waitsOnPerson).toBeUndefined();
    expect(removalRowState(landed)).toBe("finishing");
    // Not yet signed out at the server: the normal row, whatever is marked.
    expect(removalRowState(withWaitingOnPerson(newRemoval(DEV, NOW)))).toBe("normal");
    // A step that merely could not run now is not the person's to finish.
    expect(removalRowState(withStepDone(newRemoval(DEV, NOW), "revoke"))).toBe("finishing");
  });

  it("treats a step that throws as no answer, never as done", async () => {
    const h = harness();
    h.deviceGroup = "throw";
    const out = await runRemoval(newRemoval(DEV, NOW), h.ports, opts("grant"));
    expect(out.kind).toBe("finishing");
    expect(last(h)?.done.deviceGroup).toBe(false);
  });
});

describe("storage", () => {
  it("keeps one record per device, per account", () => {
    const s = memory();
    writeRemoval(s, "acct", DEV, newRemoval(DEV, NOW));
    writeRemoval(s, "acct", DEV2, withStepDone(newRemoval(DEV2, NOW), "revoke"));
    writeRemoval(s, "other", DEV, newRemoval(DEV, NOW + 1));
    expect(readRemovals(s, "acct").map((r) => r.deviceIdB64)).toEqual([DEV, DEV2]);
    expect(readRemoval(s, "acct", DEV2)?.done.revoke).toBe(true);
    expect(readRemoval(s, "other", DEV)?.startedAtMs).toBe(NOW + 1);
  });

  it("replaces a device's record and drops it with null", () => {
    const s = memory();
    writeRemoval(s, "acct", DEV, newRemoval(DEV, NOW));
    writeRemoval(s, "acct", DEV, withStepDone(newRemoval(DEV, NOW), "revoke"));
    expect(readRemovals(s, "acct")).toHaveLength(1);
    expect(readRemoval(s, "acct", DEV)?.done.revoke).toBe(true);
    writeRemoval(s, "acct", DEV, null);
    expect(readRemovals(s, "acct")).toEqual([]);
    expect(s.getItem("lakko:device-removal-owed:acct")).toBeNull();
  });

  it("reads a damaged row as nothing owed", () => {
    const s = memory();
    s.setItem("lakko:device-removal-owed:acct", "not json");
    expect(readRemovals(s, "acct")).toEqual([]);
    s.setItem("lakko:device-removal-owed:acct", JSON.stringify([{ v: 2 }, { v: 1, deviceIdB64: "x" }]));
    expect(readRemovals(s, "acct")).toEqual([]);
    expect(readRemovals(undefined, "acct")).toEqual([]);
  });

  it("is cleared with the account", () => {
    const s = memory();
    writeRemoval(s, "acct", DEV, newRemoval(DEV, NOW));
    clearRemovals(s, "acct");
    expect(readRemovals(s, "acct")).toEqual([]);
  });

  it("stores no statement and no grant", () => {
    const s = memory();
    writeRemoval(s, "acct", DEV, {
      ...newRemoval(DEV, NOW),
      attestationB64: "att",
      attestationExpiresAtMs: NOW + 1,
    });
    const raw = s.getItem("lakko:device-removal-owed:acct") ?? "";
    expect(Object.keys(JSON.parse(raw)[0] as object).sort()).toEqual([
      "attestationB64",
      "attestationExpiresAtMs",
      "deviceIdB64",
      "done",
      "startedAtMs",
      "v",
    ]);
  });
});
