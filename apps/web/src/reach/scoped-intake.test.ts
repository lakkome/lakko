// SPDX-License-Identifier: AGPL-3.0-or-later
//
// ADR-0060 section 4 - an arriving introduction offer bumps the tick the
// publication runtime re-runs on, so the record is relayed to this account's
// other contacts at once; the same record again does not.

import { beforeEach, describe, expect, it, vi } from "vitest";

const rows = new Map<string, string>();

vi.mock("./reach-service", () => ({
  reachSession: () => ({ accountIdB64: "acct", accountKeyHex: "00".repeat(32) }),
}));
vi.mock("./reach-store", () => ({
  loadReachRow: (account: string, name: string) => Promise.resolve(rows.get(`${account}/${name}`) ?? null),
  saveReachRow: (account: string, name: string, value: string) => {
    rows.set(`${account}/${name}`, value);
    return Promise.resolve();
  },
}));
vi.mock("../wasm", () => ({
  loadWasm: () =>
    Promise.resolve({
      decodeIntroductionOffer: (payload: Uint8Array) => payload,
      decodeIntroductionsBundle: () => ({ updatedAtMs: 1n, free: () => undefined }),
      bundleIsNewer: () => true,
    }),
}));
vi.mock("../contacts/address-resolver", () => ({
  resolveAccountKeyHex: () => "ab".repeat(32),
}));

const { intakeScopedEnvelope, introductionOffersTick, dropIntroductionsFrom, withoutIntroductionsFrom } =
  await import("./scoped-intake");

function offer(bytes: number[]): Promise<void> {
  return intakeScopedEnvelope({
    kind: "introduction-offer",
    payload: new Uint8Array(bytes),
    senderKeyB64: "q83vq83vq83vq83vq83vq83vq83vq83vq83vq83vq80=",
    oneToOne: true,
  });
}

describe("introduction offer intake", () => {
  beforeEach(() => rows.clear());

  it("bumps the relay tick for a new or changed offer only", async () => {
    const start = introductionOffersTick();
    await offer([1, 2, 3]);
    expect(introductionOffersTick()).toBe(start + 1);
    await offer([1, 2, 3]);
    expect(introductionOffersTick()).toBe(start + 1);
    await offer([4, 5, 6]);
    expect(introductionOffersTick()).toBe(start + 2);
  });

  it("keeps both an offer and a bundle that arrive together", async () => {
    // Intake is fire-and-forget per envelope and a contact sends its offer
    // and its bundle back to back; neither write may drop the other.
    const sender = "q83vq83vq83vq83vq83vq83vq83vq83vq83vq83vq80=";
    await Promise.all([
      intakeScopedEnvelope({ kind: "introduction-offer", payload: new Uint8Array([9, 9]), senderKeyB64: sender, oneToOne: true }),
      intakeScopedEnvelope({ kind: "introductions", payload: new Uint8Array([8, 8]), senderKeyB64: sender, oneToOne: true }),
    ]);
    const saved = JSON.parse(rows.get("acct/introductions") ?? "{}") as {
      offers?: Record<string, { recordB64: string }>;
      bundles?: Record<string, { bundleB64: string }>;
    };
    expect(Object.values(saved.offers ?? {}).map((o) => o.recordB64)).toEqual(["CQk="]);
    expect(Object.values(saved.bundles ?? {}).map((b) => b.bundleB64)).toEqual(["CAg="]);
  });

  it("does not bump for an offer outside a one-to-one", async () => {
    const start = introductionOffersTick();
    await intakeScopedEnvelope({
      kind: "introduction-offer",
      payload: new Uint8Array([7]),
      senderKeyB64: "q83vq83vq83vq83vq83vq83vq83vq83vq83vq83vq80=",
      oneToOne: false,
    });
    expect(introductionOffersTick()).toBe(start);
  });
});

describe("removing a contact drops what they offered and relayed", () => {
  beforeEach(() => rows.clear());
  const sender = "q83vq83vq83vq83vq83vq83vq83vq83vq83vq83vq80=";
  const senderHex = "ab".repeat(32);

  it("forgets their offer and bundle and runs the relay again", async () => {
    await intakeScopedEnvelope({ kind: "introduction-offer", payload: new Uint8Array([9]), senderKeyB64: sender, oneToOne: true });
    await intakeScopedEnvelope({ kind: "introductions", payload: new Uint8Array([8]), senderKeyB64: sender, oneToOne: true });
    const start = introductionOffersTick();
    await dropIntroductionsFrom(["cd".repeat(32), senderHex]);
    const saved = JSON.parse(rows.get("acct/introductions") ?? "{}") as {
      offers?: Record<string, unknown>;
      bundles?: Record<string, unknown>;
    };
    expect(saved.offers).toEqual({});
    expect(saved.bundles).toEqual({});
    // The relay re-runs, so the bundle other contacts hold is replaced.
    expect(introductionOffersTick()).toBe(start + 1);
    // Nothing left of theirs: nothing written, no second run.
    await dropIntroductionsFrom([senderHex]);
    expect(introductionOffersTick()).toBe(start + 1);
  });

  it("keeps everyone else's", () => {
    const row = {
      offers: { a: { recordB64: "A", receivedAtMs: 1 }, b: { recordB64: "B", receivedAtMs: 1 } },
      bundles: { a: { bundleB64: "X", updatedAtMs: "1" }, c: { bundleB64: "Y", updatedAtMs: "1" } },
      acks: { a: 1, c: 2 },
    };
    expect(withoutIntroductionsFrom(row, ["a"])).toEqual({
      offers: { b: { recordB64: "B", receivedAtMs: 1 } },
      bundles: { c: { bundleB64: "Y", updatedAtMs: "1" } },
      acks: { c: 2 },
    });
    expect(withoutIntroductionsFrom(row, ["z"])).toBeNull();
  });
});
