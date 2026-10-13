// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import { describe, expect, it } from "vitest";
import {
  ROTATION_STEPS,
  channelOwnerOutcome,
  directoryRotationOutcome,
  directoryUploadMode,
  newRotation,
  newSavePhraseRotation,
  nextRotationStep,
  pendingFor,
  readRotation,
  remainingRotationSteps,
  rotateCommitVerdict,
  rotationCommitted,
  rotationIntake,
  rotationResumesByItself,
  rotationRevokedDeviceIds,
  rotationSavesPhrase,
  rotationStepLine,
  rotationWithholdsPhrase,
  runRotation,
  supersededStep,
  upgradeVerdict,
  withPendingRotation,
  withRotationStepDone,
  writeRotation,
  type CommitProbe,
  type PendingRotation,
  type RemoveStepResult,
  type RotationOwed,
  type RotationPorts,
  type RotationStep,
} from "./key-rotation";

const DEVICE = "A".repeat(43);

function memoryStorage(): Storage {
  const rows = new Map<string, string>();
  return {
    getItem: (k) => rows.get(k) ?? null,
    setItem: (k, v) => void rows.set(k, v),
    removeItem: (k) => void rows.delete(k),
    clear: () => rows.clear(),
    key: () => null,
    length: 0,
  } as Storage;
}

interface Script {
  remove?: RemoveStepResult[];
  prepareFails?: boolean;
  commit?: Array<{ status: number | null; errorCode: string | null } | "ok">;
  probe?: CommitProbe;
  directory?: Array<{ status: number | null; errorCode: string | null }>;
  later?: RotationStep[];
}

function harness(script: Script = {}) {
  const calls: string[] = [];
  const saved: Array<RotationOwed | null> = [];
  const modes: string[] = [];
  let superseded = 0;
  const ports: RotationPorts = {
    remove: async (_d, grant) => {
      calls.push(`remove:${grant === null ? "no-grant" : "grant"}`);
      return script.remove?.shift() ?? { kind: "removed", preRemovalEpoch: 7n };
    },
    prepare: async () => {
      calls.push("prepare");
      if (script.prepareFails === true) throw new Error("not ready");
      return { newSignaturePubkeyB64: "NEW", preparedAtMs: 1000, envelopeEpoch: 8n };
    },
    commit: async () => {
      calls.push("commit");
      const next = script.commit?.shift() ?? "ok";
      if (next === "ok") {
        return { status: 200 as const, keyGeneration: 3n, attestationB64Url: "ATT", signedAtMs: 2000 };
      }
      return next;
    },
    probeCommit: async () => {
      calls.push("probe");
      return script.probe ?? { kind: "unknown" };
    },
    uploadIdentity: async (_r, mode) => {
      calls.push("directory");
      modes.push(mode);
      return script.directory?.shift() ?? { status: 200, errorCode: null };
    },
    publish: async (step) => {
      calls.push(step);
      return script.later?.includes(step) === true ? "later" : "done";
    },
    superseded: () => {
      superseded += 1;
    },
    save: (r) => void saved.push(r),
  };
  return { ports, calls, saved, modes, superseded: () => superseded };
}

const last = <T>(xs: T[]): T => xs[xs.length - 1] as T;

describe("the rotation's order (ADR-0074 section 3.2)", () => {
  it("runs every step once, in order, and drops the record", async () => {
    const h = harness();
    const out = await runRotation(newRotation(DEVICE, 1), h.ports, { grant: "g" });
    expect(out).toEqual({ kind: "complete" });
    expect(h.calls).toEqual([
      "remove:grant",
      "prepare",
      "commit",
      "directory",
      "key-package",
      "device-group-root",
      "journals",
      "channels",
      "reach",
      "profile",
      "links",
      "requests",
      "backup",
    ]);
    expect(last(h.saved)).toBeNull();
    expect(h.modes).toEqual(["proof-and-attestation"]);
  });

  it("writes the record before anything is sent", async () => {
    const h = harness();
    const seen: number[] = [];
    const ports = { ...h.ports, remove: async (...a: Parameters<RotationPorts["remove"]>) => {
      seen.push(h.saved.length);
      return h.ports.remove(...a);
    } };
    await runRotation(newRotation(DEVICE, 1), ports, { grant: "g" });
    expect(seen[0]).toBeGreaterThan(0);
  });

  it("tells the sheet each step as it becomes active, three lines in all", async () => {
    const h = harness();
    const lines: string[] = [];
    await runRotation(newRotation(DEVICE, 1), h.ports, {
      grant: "g",
      onStep: (s) => {
        const line = rotationStepLine(s);
        if (last(lines) !== line) lines.push(line);
      },
    });
    expect(lines).toEqual(["remove", "keys", "devices"]);
  });

  it("a refused revoke removes nothing and starts nothing", async () => {
    const h = harness({ remove: [{ kind: "not-removed", failure: "proof-required" }] });
    const out = await runRotation(newRotation(DEVICE, 1), h.ports, { grant: null });
    expect(out).toEqual({ kind: "not-removed", failure: "proof-required" });
    expect(h.calls).toEqual(["remove:no-grant"]);
    expect(last(h.saved)).toBeNull();
  });

  it("never seals the envelope while the removal's DeviceGroup commit is not in", async () => {
    const h = harness({ remove: [{ kind: "finishing" }] });
    const out = await runRotation(newRotation(DEVICE, 1), h.ports, { grant: "g" });
    expect(out).toEqual({ kind: "not-started", reason: "removal-finishing" });
    expect(h.calls).not.toContain("prepare");
    expect(h.calls).not.toContain("commit");
    // The record stays: the same button continues it.
    expect(last(h.saved)).not.toBeNull();
  });

  it("removed but not rotated: a failed prepare leaves the keys unchanged and the record resumable", async () => {
    const h = harness({ prepareFails: true });
    const out = await runRotation(newRotation(DEVICE, 1), h.ports, { grant: "g" });
    expect(out).toEqual({ kind: "not-started", reason: "prepare" });
    const record = last(h.saved) as RotationOwed;
    expect(record.done).toEqual(["remove"]);
    expect(record.preRemovalEpoch).toBe("7");
    expect(rotationCommitted(record)).toBe(false);
    expect(rotationResumesByItself(record)).toBe(false);
    expect(rotationWithholdsPhrase(record)).toBe(false);
    // Trying again does not remove a second time.
    const again = harness();
    expect(await runRotation(record, again.ports, { grant: "g" })).toEqual({ kind: "complete" });
    expect(again.calls[0]).toBe("prepare");
  });

  it("removed but not rotated: every refusal of the commit, in the server's order", async () => {
    const cases: Array<[number, string | null, string]> = [
      [403, "fresh-auth-required", "proof-required"],
      [400, null, "other"],
      [403, "account-key-required", "account-key-required"],
      [429, "too-many-attempts", "rate-limited"],
    ];
    for (const [status, errorCode, reason] of cases) {
      const h = harness({ commit: [{ status, errorCode }] });
      const out = await runRotation(newRotation(DEVICE, 1), h.ports, { grant: "g" });
      expect(out).toEqual({ kind: "not-started", reason });
      const record = last(h.saved) as RotationOwed;
      expect(record.done).toEqual(["remove", "prepare"]);
      // The server chose an answer: nothing is unconfirmed.
      expect(record.commitUnconfirmed).toBeUndefined();
      expect(h.calls).not.toContain("directory");
    }
  });

  it("without a proof the commit is not sent", async () => {
    const start = withRotationStepDone(withRotationStepDone(newRotation(DEVICE, 1), "remove"), "prepare");
    const h = harness();
    expect(await runRotation(start, h.ports, { grant: null })).toEqual({
      kind: "not-started",
      reason: "proof-required",
    });
    expect(h.calls).toEqual([]);
  });

  it("a commit with no answer is ASKED about before anything is sent again", async () => {
    const h = harness({ commit: [{ status: null, errorCode: null }] });
    const out = await runRotation(newRotation(DEVICE, 1), h.ports, { grant: "g" });
    expect(out).toEqual({ kind: "not-started", reason: "unconfirmed" });
    const record = last(h.saved) as RotationOwed;
    expect(record.commitUnconfirmed).toBe(true);

    // It landed: no second commit; the later steps run without an attestation.
    const landed = harness({ probe: { kind: "landed", keyGeneration: 4n } });
    expect(await runRotation(record, landed.ports, { grant: null })).toEqual({ kind: "complete" });
    expect(landed.calls).not.toContain("commit");
    expect(landed.calls.slice(0, 2)).toEqual(["probe", "directory"]);
    expect(landed.modes).toEqual(["proof-only"]);

    // It did not land: the commit is sent again.
    const notLanded = harness({ probe: { kind: "not-landed" } });
    expect(await runRotation(record, notLanded.ports, { grant: "g" })).toEqual({ kind: "complete" });
    expect(notLanded.calls.slice(0, 2)).toEqual(["probe", "commit"]);

    // Unknown: nothing is sent.
    const unknown = harness({ probe: { kind: "unknown" } });
    expect(await runRotation(record, unknown.ports, { grant: "g" })).toEqual({
      kind: "not-started",
      reason: "unconfirmed",
    });
    expect(unknown.calls).toEqual(["probe"]);
  });

  it("after the commit a failure is a retry of exactly the steps that did not land", async () => {
    const h = harness({ later: ["journals"] });
    const out = await runRotation(newRotation(DEVICE, 1), h.ports, { grant: "g" });
    expect(out).toEqual({ kind: "publishing", owed: ["journals", "channels", "reach", "profile", "links", "requests", "backup"] });
    const record = last(h.saved) as RotationOwed;
    expect(record.attestationB64Url).toBe("ATT");
    expect(record.keyGeneration).toBe("3");
    expect(rotationResumesByItself(record)).toBe(true);
    expect(rotationWithholdsPhrase(record)).toBe(true);
    const again = harness();
    expect(await runRotation(record, again.ports, { grant: null })).toEqual({ kind: "complete" });
    expect(again.calls).toEqual(["journals", "channels", "reach", "profile", "links", "requests", "backup"]);
  });

  it("what was granted before is spent: the links and the unanswered requests are owed steps (3.5a rule 5)", async () => {
    const h = harness({ later: ["requests"] });
    const out = await runRotation(newRotation(DEVICE, 1), h.ports, { grant: "g" });
    // After the profile (the reach keys are on the new mailbox by then),
    // links before requests, both before the backup.
    expect(h.calls.indexOf("links")).toBe(h.calls.indexOf("profile") + 1);
    expect(h.calls.indexOf("requests")).toBe(h.calls.indexOf("links") + 1);
    expect(out).toEqual({ kind: "publishing", owed: ["requests", "backup"] });
    const record = last(h.saved) as RotationOwed;
    expect(record.done).toContain("links");
    expect(rotationWithholdsPhrase(record)).toBe(true);
    const again = harness();
    expect(await runRotation(record, again.ports, { grant: null })).toEqual({ kind: "complete" });
    expect(again.calls).toEqual(["requests", "backup"]);
  });

  it("the profile re-send is a step: owed while a conversation has not got it, then retried (3.5a rule 3)", async () => {
    const h = harness({ later: ["profile"] });
    const out = await runRotation(newRotation(DEVICE, 1), h.ports, { grant: "g" });
    // It runs after the reach keys are on the new mailbox and never before.
    expect(h.calls.indexOf("profile")).toBe(h.calls.indexOf("reach") + 1);
    expect(out).toEqual({ kind: "publishing", owed: ["profile", "links", "requests", "backup"] });
    const record = last(h.saved) as RotationOwed;
    expect(record.done).not.toContain("profile");
    // The words stay withheld and the record resumes by itself.
    expect(rotationWithholdsPhrase(record)).toBe(true);
    expect(rotationResumesByItself(record)).toBe(true);
    const again = harness();
    expect(await runRotation(record, again.ports, { grant: null })).toEqual({ kind: "complete" });
    expect(again.calls).toEqual(["profile", "links", "requests", "backup"]);
  });

  it("the directory: a proof the row no longer accepts is sent again WITHOUT the proof", async () => {
    const h = harness({ directory: [{ status: 400, errorCode: "supersession-proof-invalid" }] });
    expect(await runRotation(newRotation(DEVICE, 1), h.ports, { grant: "g" })).toEqual({ kind: "complete" });
    expect(h.modes).toEqual(["proof-and-attestation", "attestation-only"]);
  });

  it("the directory: already landed reads as done; superseded stops and reports", async () => {
    const landed = harness({ directory: [{ status: 409, errorCode: "key-generation-not-newer" }] });
    expect(await runRotation(newRotation(DEVICE, 1), landed.ports, { grant: "g" })).toEqual({
      kind: "complete",
    });
    const over = harness({ directory: [{ status: 401, errorCode: "key-generation-superseded" }] });
    const out = await runRotation(newRotation(DEVICE, 1), over.ports, { grant: "g" });
    expect(out.kind).toBe("publishing");
    expect(over.superseded()).toBe(1);
  });
});

describe("the answers", () => {
  it("rotate commit", () => {
    expect(rotateCommitVerdict(200, null)).toEqual({ verdict: "done", reason: null });
    expect(rotateCommitVerdict(null, null).verdict).toBe("later");
    expect(rotateCommitVerdict(503, null).verdict).toBe("later");
    expect(rotateCommitVerdict(403, "fresh-auth-required").reason).toBe("proof-required");
  });

  it("directory upload", () => {
    const mode = "proof-and-attestation" as const;
    expect(directoryRotationOutcome({ mode, status: 200, errorCode: null })).toBe("done");
    expect(directoryRotationOutcome({ mode, status: null, errorCode: null })).toBe("later");
    // The fallback is taken once: without the proof the same refusal waits.
    expect(
      directoryRotationOutcome({ mode: "attestation-only", status: 400, errorCode: "supersession-proof-invalid" }),
    ).toBe("later");
    expect(directoryUploadMode(newRotation(DEVICE, 1))).toBe("proof-only");
    expect(directoryUploadMode({ ...newRotation(DEVICE, 1), attestationB64Url: "A" })).toBe(
      "proof-and-attestation",
    );
  });

  it("channel owner handover", () => {
    expect(channelOwnerOutcome({ status: 204, newKeyOwns: null })).toBe("done");
    expect(channelOwnerOutcome({ status: 409, newKeyOwns: true })).toBe("done");
    expect(channelOwnerOutcome({ status: 409, newKeyOwns: false })).toBe("later");
    expect(channelOwnerOutcome({ status: 401, newKeyOwns: null })).toBe("later");
    expect(channelOwnerOutcome({ status: null, newKeyOwns: null })).toBe("later");
  });
});

describe("the owed record", () => {
  it("round-trips, and a damaged or out-of-order row reads as none", () => {
    const storage = memoryStorage();
    const record = withRotationStepDone(newRotation(DEVICE, 5), "remove");
    writeRotation(storage, "acct", record);
    expect(readRotation(storage, "acct")).toEqual(record);
    storage.setItem("lakko:key-rotation-owed:acct", "{");
    expect(readRotation(storage, "acct")).toBeNull();
    // `commit` done without `prepare`: never a record this client wrote.
    storage.setItem(
      "lakko:key-rotation-owed:acct",
      JSON.stringify({ ...record, done: ["remove", "commit"] }),
    );
    expect(readRotation(storage, "acct")).toBeNull();
    writeRotation(storage, "acct", null);
    expect(readRotation(storage, "acct")).toBeNull();
  });

  it("steps are strictly ordered", () => {
    let r = newRotation(DEVICE, 1);
    for (const step of ROTATION_STEPS) {
      expect(nextRotationStep(r)).toBe(step);
      r = withRotationStepDone(r, step);
    }
    expect(nextRotationStep(r)).toBeNull();
    expect(remainingRotationSteps(r)).toEqual([]);
  });
});

// ---------------------------------------------------------------------------
// A kept device
// ---------------------------------------------------------------------------

const pending = (key: string, preparedAtMs = 1000): PendingRotation => ({
  newSignaturePubkeyB64: key,
  envelopeB64: `env-${key}`,
  preparedAtMs,
});
const FOURTEEN_DAYS = 14 * 24 * 60 * 60 * 1000;
const expired = (preparedAtMs: number, nowMs: number): boolean => nowMs - preparedAtMs > FOURTEEN_DAYS;

describe("a kept device: the envelope (ADR-0074 section 3.3)", () => {
  it("is stored only from the account's own DeviceGroup, verified, and for another key", () => {
    expect(rotationIntake({ fromOwnDeviceGroup: true, verified: true, newKeyIsHeldKey: false })).toBe("store");
    expect(rotationIntake({ fromOwnDeviceGroup: false, verified: true, newKeyIsHeldKey: false })).toBe("drop");
    expect(rotationIntake({ fromOwnDeviceGroup: true, verified: false, newKeyIsHeldKey: false })).toBe("drop");
    expect(rotationIntake({ fromOwnDeviceGroup: true, verified: true, newKeyIsHeldKey: true })).toBe("drop");
  });

  it("is kept per new key, replaced by a re-send, and dropped after 14 days", () => {
    let set = withPendingRotation([], pending("K1", 1000), 2000, expired);
    set = withPendingRotation(set, pending("K2", 1500), 2000, expired);
    set = withPendingRotation(set, pending("K1", 1800), 2000, expired);
    expect(set.map((p) => [p.newSignaturePubkeyB64, p.preparedAtMs])).toEqual([
      ["K2", 1500],
      ["K1", 1800],
    ]);
    // Past the bound: dropped; an envelope that is already past it is not stored.
    expect(withPendingRotation(set, null, 1500 + FOURTEEN_DAYS + 1, expired).map((p) => p.newSignaturePubkeyB64)).toEqual(["K1"]);
    expect(withPendingRotation([], pending("OLD", 0), FOURTEEN_DAYS + 1, expired)).toEqual([]);
  });

  it("which one is adopted is decided by the key the server side shows, never by order or stamp", () => {
    const set = [pending("K1", 9000), pending("K2", 1)];
    expect(pendingFor(set, "K2")?.envelopeB64).toBe("env-K2");
    expect(pendingFor(set, "K3")).toBeNull();
    expect(pendingFor(set, null)).toBeNull();
  });
});

describe("a kept device: superseded (ADR-0074 section 3.3)", () => {
  it("tries the held key first, then each pending key", () => {
    expect(
      supersededStep({ heldKeyB64: "H", pending: [pending("K1"), pending("K2")], tried: [], journal: "not-read" }),
    ).toEqual({ kind: "try-keys", keysB64: ["H", "K1", "K2"] });
  });

  it("with no pending envelope it reads the journal ONCE before it is a reset", () => {
    expect(supersededStep({ heldKeyB64: "H", pending: [], tried: ["H"], journal: "not-read" })).toEqual({
      kind: "read-journal",
    });
    // The journal brought an envelope: it is tried.
    expect(supersededStep({ heldKeyB64: "H", pending: [pending("K1")], tried: ["H"], journal: "read" })).toEqual({
      kind: "try-keys",
      keysB64: ["K1"],
    });
    // It brought none, or could not be read: a reset.
    expect(supersededStep({ heldKeyB64: "H", pending: [], tried: ["H"], journal: "read" })).toEqual({ kind: "reset" });
    expect(supersededStep({ heldKeyB64: "H", pending: [], tried: ["H"], journal: "unreadable" })).toEqual({
      kind: "reset",
    });
  });

  it("an envelope whose key the server did not accept is not adopted, and the journal is still read once", () => {
    expect(
      supersededStep({ heldKeyB64: "H", pending: [pending("K1")], tried: ["H", "K1"], journal: "not-read" }),
    ).toEqual({ kind: "read-journal" });
    expect(
      supersededStep({ heldKeyB64: "H", pending: [pending("K1")], tried: ["H", "K1"], journal: "read" }),
    ).toEqual({ kind: "reset" });
  });

  it("the session upgrade's answers", () => {
    expect(upgradeVerdict(200, null)).toBe("accepted");
    expect(upgradeVerdict(401, "key-generation-superseded")).toBe("not-accepted");
    expect(upgradeVerdict(401, "device-revoked")).toBe("device-revoked");
    expect(upgradeVerdict(401, "refresh-invalid")).toBe("sign-in");
    expect(upgradeVerdict(401, "statement-stale")).toBe("stale");
    // No answer, a limit, a server error or an unreadable 401: never a reset.
    expect(upgradeVerdict(null, null)).toBe("later");
    expect(upgradeVerdict(429, "too-many-attempts")).toBe("later");
    expect(upgradeVerdict(503, null)).toBe("later");
    expect(upgradeVerdict(401, null)).toBe("later");
  });
});

// ADR-0075 section 3 (UX 19.5): "Save a recovery phrase" is this rotation
// with no device to remove.
describe("the rotation that saves a recovery phrase", () => {
  it("removes no device: the remove step is done from the start and never runs", async () => {
    const h = harness();
    const start = newSavePhraseRotation(1);
    expect(rotationSavesPhrase(start)).toBe(true);
    expect(nextRotationStep(start)).toBe("prepare");
    const out = await runRotation(start, h.ports, { grant: "g" });
    expect(out).toEqual({ kind: "complete" });
    expect(h.calls.some((c) => c.startsWith("remove"))).toBe(false);
    expect(h.calls.slice(0, 3)).toEqual(["prepare", "commit", "directory"]);
    // Every later step of the ordinary rotation still runs, in order.
    expect(h.calls.slice(3)).toEqual(ROTATION_STEPS.slice(4));
  });

  it("revokes no device with its commit; the ordinary rotation revokes the removed one", () => {
    expect(rotationRevokedDeviceIds(newSavePhraseRotation(1))).toEqual([]);
    expect(rotationRevokedDeviceIds(newRotation(DEVICE, 1))).toEqual([DEVICE]);
    expect(rotationSavesPhrase(newRotation(DEVICE, 1))).toBe(false);
  });

  it("without a proof nothing is sent and nothing changed", async () => {
    const h = harness();
    const out = await runRotation(newSavePhraseRotation(1), h.ports, { grant: null });
    expect(out).toEqual({ kind: "not-started", reason: "proof-required" });
    expect(h.calls).toEqual(["prepare"]);
    expect(rotationCommitted(last(h.saved) as RotationOwed)).toBe(false);
  });

  it("a refused commit changed nothing; the words stay withheld until every step landed", async () => {
    const refused = harness({ commit: [{ status: 403, errorCode: "fresh-auth-required" }] });
    expect(await runRotation(newSavePhraseRotation(1), refused.ports, { grant: "g" })).toEqual({
      kind: "not-started",
      reason: "proof-required",
    });
    const owed = harness({ later: ["journals"] });
    const out = await runRotation(newSavePhraseRotation(1), owed.ports, { grant: "g" });
    expect(out.kind).toBe("publishing");
    const record = last(owed.saved) as RotationOwed;
    expect(rotationSavesPhrase(record)).toBe(true);
    expect(rotationWithholdsPhrase(record)).toBe(true);
    expect(rotationResumesByItself(record)).toBe(true);
  });

  it("its record survives a reload, and a damaged one cannot drive a retry", () => {
    const storage = memoryStorage();
    const record = newSavePhraseRotation(5);
    writeRotation(storage, "acct", record);
    expect(readRotation(storage, "acct")).toEqual(record);
    // A save-phrase record that claims a device, or lost its done step.
    writeRotation(storage, "acct", { ...record, deviceIdB64: DEVICE });
    expect(readRotation(storage, "acct")).toBeNull();
    writeRotation(storage, "acct", { ...record, done: [] });
    expect(readRotation(storage, "acct")).toBeNull();
    // An ordinary record with no device id is still refused.
    writeRotation(storage, "acct", { ...newRotation(DEVICE, 1), deviceIdB64: "" });
    expect(readRotation(storage, "acct")).toBeNull();
    storage.setItem("lakko:key-rotation-owed:acct", JSON.stringify({ ...record, purpose: "other" }));
    expect(readRotation(storage, "acct")).toBeNull();
  });
});
