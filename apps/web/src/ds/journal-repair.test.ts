// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * ADR-0057, the second security pass (finding 1 + LOW 7): Append by default,
 * repair from above (`ds/journal-repair.ts`).
 *
 * The rules under test: an Append 409 in a commit path is surfaced (the
 * caller abandons) and never answered with a Replace; the repair sends a
 * Replace only after the delay, only when the group's epoch did not move, and
 * never for a member.
 */
import { describe, expect, it, vi } from "vitest";

import { DsAccessRegistry } from "./access";
import { DsClient } from "./client";
import {
  JOURNAL_REPAIR_DELAY_MS,
  JournalRepair,
  mayRepair,
  type HeadPoller,
  type JournalRepairPorts,
} from "./journal-repair";
import {
  fakeRegistrarAuthority,
  fakeRootAuthority,
  fakeSigner,
  parseFakeStatement,
} from "./access-test-doubles";
import { RANK_ADMIN, RANK_MEMBER, type JournalAuthority, type StatementSender } from "./ownership";

const GID = new Uint8Array(32).fill(0x61);

function recorder(statuses: number[] = []): StatementSender & { puts: Array<{ path: string; body: string }> } {
  const puts: Array<{ path: string; body: string }> = [];
  const script = [...statuses];
  return {
    puts,
    putStatement: vi.fn(async (path: string, body: string) => {
      puts.push({ path, body });
      const status = script.shift() ?? 204;
      if (status >= 400) throw Object.assign(new Error(`HTTP ${status}`), { status });
    }),
  };
}

/** A repair scheduler whose timer is captured, not real. */
function harness(opts: {
  authority?: () => JournalAuthority;
  epoch?: () => bigint | null;
  statuses?: number[];
  poll?: () => Promise<void>;
  owns?: boolean;
}) {
  const sender = recorder(opts.statuses);
  const timers: Array<{ run: () => void; ms: number }> = [];
  const polls = vi.fn(opts.poll ?? (async () => undefined));
  const repair = new JournalRepair();
  const ports: JournalRepairPorts = {
    authority: opts.authority ?? (() => fakeRootAuthority("root")),
    currentEpochKey: () => {
      const e = opts.epoch?.() ?? 7n;
      return e === null ? null : { epoch: e, key: new Uint8Array(32).fill(Number(e)) };
    },
    sender,
    schedule: (run, ms) => {
      timers.push({ run, ms });
      return () => undefined;
    },
  };
  repair.install(ports);
  const poller: HeadPoller = { owns: () => opts.owns ?? true, pollToHead: polls };
  repair.registerHeadPoller(poller);
  return { repair, sender, timers, polls };
}

const info = (): void => undefined;

describe("who repairs", () => {
  it("the root and an admin registrar; a member never", () => {
    expect(mayRepair(fakeRootAuthority())).toBe(true);
    expect(mayRepair(fakeRegistrarAuthority("a", RANK_ADMIN))).toBe(true);
    expect(mayRepair(fakeRegistrarAuthority("m", RANK_MEMBER))).toBe(false);
    expect(mayRepair({ kind: "none" })).toBe(false);
  });

  it("a member's note schedules nothing and sends nothing", () => {
    const h = harness({ authority: () => fakeRegistrarAuthority("m", RANK_MEMBER) });
    expect(h.repair.note(GID, 7n, "append-conflict")).toBe(false);
    expect(h.timers).toHaveLength(0);
    expect(h.sender.puts).toHaveLength(0);
  });

  it("a group no poller holds (DeviceGroup, PublisherGroup: every registrar is the root) is not repaired", () => {
    const h = harness({ owns: false });
    expect(h.repair.note(GID, 7n, "refused-at-head")).toBe(false);
    expect(h.timers).toHaveLength(0);
  });
});

describe("the check, 30 s later", () => {
  it("nothing is sent before the delay; then poll to head, and Replace at N with the key at N", async () => {
    const spy = vi.spyOn(console, "info").mockImplementation(info);
    const h = harness({ authority: () => fakeRegistrarAuthority("adm", RANK_ADMIN) });
    expect(h.repair.note(GID, 7n, "append-conflict")).toBe(true);
    expect(JOURNAL_REPAIR_DELAY_MS).toBe(30_000);
    expect(h.timers).toEqual([{ run: expect.any(Function), ms: 30_000 }]);
    expect(h.sender.puts).toHaveLength(0);
    expect(h.polls).not.toHaveBeenCalled();
    // A second trigger for the same group while one is pending is folded.
    expect(h.repair.note(GID, 7n, "refused-at-head")).toBe(false);

    expect(await h.repair.check(GID, 7n)).toBe("replaced");
    expect(h.polls).toHaveBeenCalledTimes(1);
    expect(h.sender.puts).toHaveLength(1);
    const st = parseFakeStatement(h.sender.puts[0]?.body ?? "");
    expect(st.segment).toBe("access-replace");
    expect(st.signer).toBe("adm");
    expect(st.epoch).toBe("7");
    expect(st.epochKey).toBe("07".repeat(32));
    expect(st.keys).toBe("");
    spy.mockRestore();
  });

  it("the epoch moved past N during the delay: no Replace", async () => {
    const spy = vi.spyOn(console, "info").mockImplementation(info);
    let epoch = 7n;
    const h = harness({ epoch: () => epoch, poll: async () => void (epoch = 8n) });
    expect(await h.repair.check(GID, 7n)).toBe("moved");
    expect(h.sender.puts).toHaveLength(0);
    spy.mockRestore();
  });

  it("ADR-0091 phase 1: a registered commit held, or a device behind at N, never Replaces", async () => {
    const spy = vi.spyOn(console, "info").mockImplementation(info);
    const known = { value: true };
    const h = harness({ authority: () => fakeRegistrarAuthority("adm", RANK_ADMIN) });
    h.repair.install({
      authority: () => fakeRegistrarAuthority("adm", RANK_ADMIN),
      currentEpochKey: () => ({ epoch: 7n, key: new Uint8Array(32).fill(7) }),
      sender: h.sender,
      registeredCommitKnown: () => known.value,
    });
    expect(await h.repair.check(GID, 7n)).toBe("registered-commit-known");
    expect(h.sender.puts).toHaveLength(0);
    known.value = false;
    expect(await h.repair.check(GID, 7n)).toBe("replaced");
    expect(h.sender.puts).toHaveLength(1);
    spy.mockRestore();
  });

  it("403: an equal or higher rank registered above; stop", async () => {
    const spy = vi.spyOn(console, "info").mockImplementation(info);
    const h = harness({ statuses: [403] });
    expect(await h.repair.check(GID, 7n)).toBe("outranked");
    expect(h.sender.puts).toHaveLength(1);
    spy.mockRestore();
  });

  it("a head poll the journal refuses (401) still decides; an offline one does not Replace", async () => {
    const spy = vi.spyOn(console, "info").mockImplementation(info);
    const refused = harness({
      poll: async () => {
        throw Object.assign(new Error("ChatError"), { cause: Object.assign(new Error("401"), { status: 401 }) });
      },
    });
    expect(await refused.repair.check(GID, 7n)).toBe("replaced");
    const offline = harness({
      poll: async () => {
        throw new TypeError("fetch failed");
      },
    });
    expect(await offline.repair.check(GID, 7n)).toBe("poll-failed");
    expect(offline.sender.puts).toHaveLength(0);
    spy.mockRestore();
  });

  it("an admin demoted to member during the delay does not Replace", async () => {
    const spy = vi.spyOn(console, "info").mockImplementation(info);
    let admin = true;
    const h = harness({
      authority: () => fakeRegistrarAuthority("x", admin ? RANK_ADMIN : RANK_MEMBER),
    });
    expect(h.repair.note(GID, 7n, "append-conflict")).toBe(true);
    admin = false;
    expect(await h.repair.check(GID, 7n)).toBe("not-permitted");
    expect(h.sender.puts).toHaveLength(0);
    spy.mockRestore();
  });
});

describe("the DS client: an Append 409 notes the repair and is never answered with a Replace", () => {
  function client(statuses: number[]) {
    const access = new DsAccessRegistry();
    // An admin registrar: it registers and repairs, but never lazily claims.
    access.setAuthorityFactory({
      root: () => ({ kind: "none" }),
      registrar: () => fakeRegistrarAuthority("adm", RANK_ADMIN),
    });
    access.registerGroupSource(() => fakeSigner(7n));
    const bodies: string[] = [];
    const script = [...statuses];
    const fetchImpl = vi.fn(async (_url: RequestInfo | URL, init?: RequestInit) => {
      if (init?.body !== undefined) bodies.push(new TextDecoder().decode(init.body as ArrayBuffer));
      const status = script.shift() ?? 204;
      const body = status === 204 ? null : status === 201 ? '{"id":1,"created_at":"t"}' : "{}";
      return new Response(body, { status });
    }) as unknown as typeof fetch;
    const note = vi.fn(() => true);
    const ds = new DsClient("/api/ds", { access, fetchImpl, repair: { note } });
    return { ds, note, bodies };
  }

  it("409 (retried once): thrown to the commit path, repair noted at the epoch it is at", async () => {
    const c = client([409, 409]);
    await expect(
      c.ds.commitGroupEpoch(GID, 8n, new Uint8Array(32).fill(8), [], new Uint8Array([1]), fakeSigner(7n)),
    ).rejects.toMatchObject({ status: 409 });
    expect(c.note).toHaveBeenCalledWith(GID, 7n, "append-conflict");
    // Only commits (an Append with its row, ADR-0081) left the client.
    expect(c.bodies.map((b) => parseFakeStatement(b).segment)).toEqual(["commit", "commit"]);
  });

  it("a commit that lands notes nothing", async () => {
    const c = client([201]);
    await c.ds.commitGroupEpoch(GID, 8n, new Uint8Array(32), [], new Uint8Array([1]), fakeSigner(7n));
    expect(c.note).not.toHaveBeenCalled();
  });

  it("a signed read refused at this device's epoch after every retry notes refused-at-head", async () => {
    const c = client([401, 401]);
    await expect(c.ds.fetchGroup(GID, { after: 0 })).rejects.toMatchObject({ status: 401 });
    expect(c.note).toHaveBeenCalledWith(GID, 7n, "refused-at-head");
  });
});
