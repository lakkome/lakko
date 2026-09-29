// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * ADR-0057 obligation 3 — the owner appoints registrars (`ds/appointments.ts`)
 * and a member receives its seed (`ds/registrar-seeds.ts`).
 *
 * The reconcile is driven with fakes for the network and the group; the seed
 * that travels is checked end to end against the REAL core: what the owner
 * composes for a member, that member (and only that member) opens, and the
 * seed is the one the owner registered the public key of.
 */
import { beforeAll, describe, expect, it, vi } from "vitest";
import { readFileSync } from "node:fs";
import { fileURLToPath } from "node:url";

import init, * as wasm from "../../wasm/lakko_ffi_wasm";
import {
  MAX_REGISTRAR_SET,
  loadAppointmentRecord,
  reconcileAppointments,
  saveAppointmentRecord,
  type AppointmentPorts,
  type AppointmentRoster,
  type RecordStorage,
} from "./appointments";
import { RANK_ADMIN, RANK_MEMBER, type JournalAuthority } from "./ownership";
import { fakeRegistrarAuthority, fakeStatementSigner, parseFakeStatement } from "./access-test-doubles";
import { wasmRegistrarDerivation } from "./journal-authority";
import {
  RegistrarSeedStore,
  decodeRegistrarSeedRows,
  encodeRegistrarSeedRows,
  receiveRegistrarSeed,
  registrarSeedVerdict,
  type RegistrarSeedReceivePorts,
} from "./registrar-seeds";

beforeAll(async () => {
  const wasmPath = fileURLToPath(new URL("../../wasm/lakko_ffi_wasm_bg.wasm", import.meta.url));
  await init({ module_or_path: readFileSync(wasmPath) });
});

const GID = new Uint8Array(32).fill(0x24);
const b64 = (b: Uint8Array): string => Buffer.from(b).toString("base64");
const acct = (n: number): Uint8Array => new Uint8Array(32).fill(n);

/** A reconcile harness: the owner "own" with a roster, a fake root, fake
 *  derivations that are readable in assertions, and a recorder. */
function harness(opts: {
  roster: AppointmentRoster | null;
  own?: string;
  authority?: JournalAuthority;
  record?: Map<string, number>;
  x25519?: (key: string) => Uint8Array | null;
  sendFails?: (key: string) => boolean;
  statusScript?: number[];
  sent?: { value: string | null };
}): {
  ports: AppointmentPorts;
  puts: Array<{ path: string; body: string }>;
  envelopes: Array<{ member: string; rank: number; seed: string }>;
  saved: () => Map<string, number> | null;
} {
  const puts: Array<{ path: string; body: string }> = [];
  const envelopes: Array<{ member: string; rank: number; seed: string }> = [];
  let saved: Map<string, number> | null = null;
  const statuses = [...(opts.statusScript ?? [])];
  const ports: AppointmentPorts = {
    groupId: GID,
    roster: () => opts.roster,
    ownAccountKeyB64: () => opts.own ?? b64(acct(1)),
    authority: () =>
      opts.authority ?? { kind: "root", signer: fakeStatementSigner("root"), rootSeed: acct(0xee) },
    derive: {
      // Readable fakes: the seed names the member, the public key names the seed.
      registrarSeed: (_root, member) => Uint8Array.from([0x5e, member[0] ?? 0, ...new Uint8Array(30)]),
      publicKeyOfSeed: (seed) => Uint8Array.from([0x9b, seed[1] ?? 0, ...new Uint8Array(30)]),
    },
    sender: {
      putStatement: vi.fn(async (path: string, body: string) => {
        puts.push({ path, body });
        const status = statuses.shift() ?? 204;
        if (status >= 400) throw Object.assign(new Error(`HTTP ${status}`), { status });
      }),
    },
    loadRecord: () => new Map(opts.record ?? []),
    lastSentSet: () => opts.sent?.value ?? null,
    noteSentSet: (fp) => {
      if (opts.sent !== undefined) opts.sent.value = fp;
    },
    saveRecord: (r) => {
      saved = new Map(r);
    },
    memberX25519: async (key) => (opts.x25519 === undefined ? acct(0x77) : opts.x25519(key)),
    composeEnvelope: (member, _x, rank, seed) => {
      envelopes.push({ member: b64(member), rank, seed: b64(seed) });
      return new Uint8Array([rank]);
    },
    sendEnvelope: async (env) => {
      const last = envelopes[envelopes.length - 1];
      if (last !== undefined && opts.sendFails?.(last.member) === true) throw new Error("offline");
      void env;
    },
  };
  return { ports, puts, envelopes, saved: () => saved };
}

const roster = (members: number[], owner: number | undefined, admins: number[] = []): AppointmentRoster => ({
  memberAccountKeysB64: members.map((m) => b64(acct(m))),
  ownerAccountKeyB64: owner === undefined ? undefined : b64(acct(owner)),
  adminAccountKeysB64: new Set(admins.map((a) => b64(acct(a)))),
});

describe("obligation 3 — only the owner appoints", () => {
  it("a non-owner, an unreadable roster and a table-less group appoint nothing", async () => {
    for (const r of [roster([1, 2], 2), null, roster([1, 2], undefined)]) {
      const h = harness({ roster: r });
      const out = await reconcileAppointments(h.ports);
      expect(["not-owner", "unavailable"]).toContain(out.kind);
      expect(h.puts).toHaveLength(0);
      expect(h.envelopes).toHaveLength(0);
    }
  });

  it("an owner without its root (not yet installed) appoints nothing", async () => {
    const h = harness({ roster: roster([1, 2], 1), authority: fakeRegistrarAuthority() });
    expect((await reconcileAppointments(h.ports)).kind).toBe("not-owner");
    expect(h.puts).toHaveLength(0);
  });
});

describe("finding 2 — the owner converges the registrar set to the roster", () => {
  it("sends ONE registrars-set with the whole set from the roster, then each member's envelope", async () => {
    const h = harness({ roster: roster([1, 2, 3], 1, [3]) });
    const out = await reconcileAppointments(h.ports);
    expect(out).toMatchObject({ kind: "reconciled", setSent: true, pending: [] });
    // ONE registrars-set statement carrying the registrar PUBLIC keys (never
    // the account keys) with their ranks; the owner itself is never listed.
    expect(h.puts).toHaveLength(1);
    expect(h.puts[0]?.path.endsWith("/registrars-set")).toBe(true);
    const st = parseFakeStatement(h.puts[0]?.body ?? "");
    expect(st.signer).toBe("root");
    expect(st.keys).toBe(
      ["9b02" + "00".repeat(30), "9b03" + "00".repeat(30)].join(""),
    );
    expect(st.ranks).toBe("0102");
    // Then one envelope per member, with the seed the owner derived for it.
    expect(h.envelopes).toEqual([
      { member: b64(acct(2)), rank: RANK_MEMBER, seed: b64(Uint8Array.from([0x5e, 2, ...new Uint8Array(30)])) },
      { member: b64(acct(3)), rank: RANK_ADMIN, seed: b64(Uint8Array.from([0x5e, 3, ...new Uint8Array(30)])) },
    ]);
    expect([...(h.saved() ?? [])]).toEqual([
      [b64(acct(2)), RANK_MEMBER],
      [b64(acct(3)), RANK_ADMIN],
    ]);
  });

  it("does not depend on a local ledger: an empty record still sends the full set, a stale one changes nothing", async () => {
    // A second owner device with no record, and one whose record still
    // names an evicted account 9: both send exactly the roster's set.
    for (const record of [new Map<string, number>(), new Map([[b64(acct(9)), RANK_ADMIN], [b64(acct(2)), RANK_MEMBER]])]) {
      const h = harness({ roster: roster([1, 2], 1), record });
      await reconcileAppointments(h.ports);
      const st = parseFakeStatement(h.puts[0]?.body ?? "");
      expect(h.puts[0]?.path.endsWith("/registrars-set")).toBe(true);
      expect(st.keys).toBe("9b02" + "00".repeat(30));
      expect(st.ranks).toBe("01");
      // The evicted account leaves the envelope record; nobody is sent a
      // removal - the set itself removed it.
      expect(h.saved()?.has(b64(acct(9)))).toBe(false);
    }
  });

  it("is sent once per group per session; an unchanged roster with delivered seeds sends nothing", async () => {
    const sent = { value: null as string | null };
    const record = new Map([[b64(acct(2)), RANK_MEMBER]]);
    const first = harness({ roster: roster([1, 2], 1), record, sent });
    expect(await reconcileAppointments(first.ports)).toMatchObject({ kind: "reconciled", setSent: true, appointed: [] });
    expect(first.puts).toHaveLength(1);
    const again = harness({ roster: roster([1, 2], 1), record, sent });
    expect((await reconcileAppointments(again.ports)).kind).toBe("unchanged");
    expect(again.puts).toHaveLength(0);
  });

  it("a roster or role change sends the new full set; a promotion re-sends the envelope at admin rank", async () => {
    const sent = { value: null as string | null };
    const record = new Map([[b64(acct(2)), RANK_MEMBER]]);
    await reconcileAppointments(harness({ roster: roster([1, 2], 1), record, sent }).ports);
    const h = harness({ roster: roster([1, 2], 1, [2]), record, sent });
    await reconcileAppointments(h.ports);
    expect(parseFakeStatement(h.puts[0]?.body ?? "").ranks).toBe("02");
    expect(h.envelopes.map((e) => e.rank)).toEqual([RANK_ADMIN]);
  });

  it("a departure is dropped from the set and from the envelope record, with no envelope", async () => {
    const sent = { value: null as string | null };
    const h = harness({
      roster: roster([1], 1),
      record: new Map([[b64(acct(2)), RANK_MEMBER]]),
      sent,
    });
    await reconcileAppointments(h.ports);
    const st = parseFakeStatement(h.puts[0]?.body ?? "");
    expect(st.keys).toBe("");
    expect(st.ranks).toBe("");
    expect(h.envelopes).toHaveLength(0);
    expect([...(h.saved() ?? [])]).toEqual([]);
  });

  it("a member whose box key is unknown yet is registered, its envelope left PENDING and not recorded", async () => {
    const h = harness({
      roster: roster([1, 2, 3], 1),
      x25519: (key) => (key === b64(acct(3)) ? null : acct(0x77)),
    });
    const out = await reconcileAppointments(h.ports);
    expect(out).toMatchObject({ kind: "reconciled", pending: [b64(acct(3))] });
    expect(parseFakeStatement(h.puts[0]?.body ?? "").keys).toBe(
      ["9b02" + "00".repeat(30), "9b03" + "00".repeat(30)].join(""),
    );
    expect([...(h.saved() ?? [])]).toEqual([[b64(acct(2)), RANK_MEMBER]]);
  });

  it("a failed envelope leaves only that member pending", async () => {
    const warn = vi.spyOn(console, "warn").mockImplementation(() => {});
    const h = harness({ roster: roster([1, 2, 3], 1), sendFails: (m) => m === b64(acct(2)) });
    const out = await reconcileAppointments(h.ports);
    expect(out).toMatchObject({ kind: "reconciled", pending: [b64(acct(2))], appointed: [b64(acct(3))] });
    expect([...(h.saved() ?? [])]).toEqual([[b64(acct(3)), RANK_MEMBER]]);
    warn.mockRestore();
  });

  it("a refused set records nothing, notes nothing sent, and throws, so the next pass retries", async () => {
    const sent = { value: null as string | null };
    const h = harness({ roster: roster([1, 2], 1), statusScript: [403], sent });
    await expect(reconcileAppointments(h.ports)).rejects.toMatchObject({ status: 403 });
    expect(h.saved()).toBeNull();
    expect(sent.value).toBeNull();
    expect(h.envelopes).toHaveLength(0);
  });

  it("the whole set goes in one statement, well past the old 256-change chunk", async () => {
    const members = Array.from({ length: 300 }, (_, i) => i + 2);
    const big: AppointmentRoster = {
      memberAccountKeysB64: [b64(acct(1)), ...members.map((i) => b64(Uint8Array.from([i & 0xff, i >> 8, ...new Uint8Array(30)])))],
      ownerAccountKeyB64: b64(acct(1)),
      adminAccountKeysB64: new Set(),
    };
    const h = harness({ roster: big });
    await reconcileAppointments(h.ports);
    expect(h.puts).toHaveLength(1);
    expect(MAX_REGISTRAR_SET).toBe(4096);
  });
});

describe("the record", () => {
  it("round-trips per (account, group) and ignores junk", () => {
    const data = new Map<string, string>();
    const storage: RecordStorage = {
      getItem: (k) => data.get(k) ?? null,
      setItem: (k, v) => void data.set(k, v),
    };
    saveAppointmentRecord("acc", "g1", new Map([["k", RANK_ADMIN]]), storage);
    expect([...loadAppointmentRecord("acc", "g1", storage)]).toEqual([["k", RANK_ADMIN]]);
    expect(loadAppointmentRecord("acc", "g2", storage).size).toBe(0);
    data.set("lakko/ds-appointments/v1/acc/g3", "{\"k\":7,\"j\":1}");
    expect([...loadAppointmentRecord("acc", "g3", storage)]).toEqual([["j", RANK_MEMBER]]);
  });
});

describe("obligation 4 — the receive gate", () => {
  const header = { journalId: GID, memberAccountKey: acct(2), rank: 1 };
  const base = {
    header,
    groupId: GID,
    ownAccountKey: acct(2),
    senderAccountKey: acct(1),
    ownerAccountKey: acct(1),
  };

  it("keeps an envelope addressed to this account from the group's owner", () => {
    expect(registrarSeedVerdict(base)).toBe("accept");
  });

  it("drops one addressed to another account", () => {
    expect(registrarSeedVerdict({ ...base, ownAccountKey: acct(3) })).toBe("not-addressed");
  });

  it("drops one sent by anyone but the owner, a table-less group included", () => {
    expect(registrarSeedVerdict({ ...base, senderAccountKey: acct(3) })).toBe("sender-not-owner");
    expect(registrarSeedVerdict({ ...base, senderAccountKey: undefined })).toBe("sender-not-owner");
    expect(registrarSeedVerdict({ ...base, ownerAccountKey: undefined })).toBe("sender-not-owner");
  });

  it("drops one for another journal, and a malformed one", () => {
    expect(registrarSeedVerdict({ ...base, groupId: acct(9) })).toBe("wrong-journal");
    expect(registrarSeedVerdict({ ...base, header: null })).toBe("malformed");
    expect(registrarSeedVerdict({ ...base, header: { ...header, rank: 0 } })).toBe("malformed");
  });
});

describe("end to end with the real core: the owner seals, the member opens", () => {
  it("the member opens the seed the owner derived; the store keeps it; another member cannot", async () => {
    const owner = wasm.Identity.fromSeed(new Uint8Array(64).fill(0x01));
    const member = wasm.Identity.fromSeed(new Uint8Array(64).fill(0x02));
    const other = wasm.Identity.fromSeed(new Uint8Array(64).fill(0x03));
    const rootSeed = wasm.dsJournalRootSeed(owner.ed25519Seed, GID);
    const seed = wasm.dsJournalRegistrarSeed(rootSeed, member.signaturePublicKey);
    const envelope = wasm.composeRegistrarSeedEnvelope(
      GID,
      member.signaturePublicKey,
      member.x25519PublicKey,
      RANK_ADMIN,
      seed,
    );
    expect(envelope.length).toBe(wasm.registrarSeedEnvelopeLen());

    const store = new RegistrarSeedStore();
    await store.open("acc", { load: async () => null, save: async () => {} });
    const portsFor = (who: typeof member): RegistrarSeedReceivePorts => ({
      decode: (pt) => {
        const d = wasm.decodeRegistrarSeedEnvelope(pt);
        const h = { journalId: d.journalId, memberAccountKey: d.memberAccountKey, rank: d.rank };
        d.free();
        return h;
      },
      open: (pt) => wasm.openRegistrarSeedEnvelope(pt, who),
      ownAccountKey: () => who.signaturePublicKey,
      keep: (g, v) => store.set(g, v),
    });
    const arrival = {
      groupId: GID,
      groupIdB64: b64(GID),
      plaintext: envelope,
      senderAccountKey: owner.signaturePublicKey,
      ownerAccountKey: owner.signaturePublicKey,
    };
    expect(await receiveRegistrarSeed(portsFor(other), arrival)).toBe("not-addressed");
    expect(store.get(b64(GID))).toBeUndefined();
    // A member relaying the owner's envelope is not the owner.
    expect(
      await receiveRegistrarSeed(portsFor(member), { ...arrival, senderAccountKey: other.signaturePublicKey }),
    ).toBe("sender-not-owner");
    expect(await receiveRegistrarSeed(portsFor(member), arrival)).toBe("accept");
    expect(store.get(b64(GID))).toEqual({ seed, rank: RANK_ADMIN });

    // The key the member now signs with is the one the owner registered.
    const derive = wasmRegistrarDerivation(wasm);
    const signer = wasm.dsStatementSigner(store.get(b64(GID))?.seed ?? new Uint8Array(32));
    expect(signer.publicKey()).toEqual(derive.publicKeyOfSeed(derive.registrarSeed(rootSeed, member.signaturePublicKey)));
    signer.free();
    owner.free();
    member.free();
    other.free();
  });

  it("a newer envelope replaces the older one", async () => {
    const store = new RegistrarSeedStore();
    await store.open("acc", { load: async () => null, save: async () => {} });
    await store.set("g", { seed: acct(1), rank: RANK_MEMBER });
    await store.set("g", { seed: acct(2), rank: RANK_ADMIN });
    expect(store.get("g")).toEqual({ seed: acct(2), rank: RANK_ADMIN });
  });

  it("the encrypted row's plaintext round-trips and skips junk", () => {
    const rows = new Map([["g", { seed: acct(5), rank: RANK_MEMBER }]]);
    expect(decodeRegistrarSeedRows(encodeRegistrarSeedRows(rows))).toEqual(rows);
    expect(decodeRegistrarSeedRows('{"g":{"s":"AA==","r":1},"h":{"s":1,"r":2}}').size).toBe(0);
    expect(decodeRegistrarSeedRows("not json").size).toBe(0);
  });
});
