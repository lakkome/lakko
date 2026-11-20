// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import { describe, expect, it } from "vitest";

import { base64Encode } from "../auth/account-id";
import { newDraft, withReason, type ReportDraft } from "./report-model";
import { sendReport, type ReportSendPorts, type SealSpec } from "./report-send";

const KEY = new Uint8Array(32).fill(7);
const GROUP_ID = new Uint8Array(32).fill(3);

function ports(over: Partial<ReportSendPorts> = {}): { ports: ReportSendPorts; sealed: SealSpec[]; floors: bigint[] } {
  const sealed: SealSpec[] = [];
  const floors: bigint[] = [];
  const base: ReportSendPorts = {
    rootKey: () => new Uint8Array(32),
    fetchList: async () => new Uint8Array([1]),
    readFloor: () => null,
    writeFloor: (v) => floors.push(v),
    listVersion: () => 2n,
    accountTarget: (k) => new Uint8Array(32).fill(k[0] ?? 0),
    groupLookupKey: async () => new Uint8Array(64).fill(1),
    evidenceFor: async (_g, id) =>
      id === 1
        ? {
            attestation: "verified",
            plaintextB64: base64Encode(new Uint8Array([9, 9])),
            leafKeyB64: base64Encode(new Uint8Array(32).fill(5)),
            bindingB64: null,
          }
        : undefined,
    channelPostFor: (_c, cursor) => (cursor === 10 ? new Uint8Array([4]) : undefined),
    seal: (spec) => {
      sealed.push(spec);
      return new Uint8Array([0xaa]);
    },
    post: async () => ({ kind: "sent", receiptId: "r".repeat(22) }),
  };
  return { ports: { ...base, ...over }, sealed, floors };
}

function personDraft(picked: number[] = [1, 2]): ReportDraft {
  return withReason(
    newDraft({
      target: { kind: "person", accountKeyB64: base64Encode(KEY), name: "A" },
      conversationKind: 1,
      pickFrom: { kind: "chat", groupIdB64: base64Encode(GROUP_ID), authorKeyB64: base64Encode(KEY) },
      picked,
    }),
    1,
  );
}

describe("sending a report", () => {
  it("is unavailable without a pinned root key, and fetches nothing", async () => {
    let fetched = false;
    const { ports: p } = ports({
      rootKey: () => null,
      fetchList: async () => {
        fetched = true;
        return new Uint8Array([1]);
      },
    });
    expect(await sendReport(personDraft(), p)).toEqual({ kind: "unavailable" });
    expect(fetched).toBe(false);
  });

  it("is unavailable when the server has no moderator list", async () => {
    const { ports: p } = ports({ fetchList: async () => null });
    expect(await sendReport(personDraft(), p)).toEqual({ kind: "unavailable" });
  });

  it("is the network failure when the list cannot be fetched", async () => {
    const { ports: p } = ports({
      fetchList: async () => {
        throw new Error("offline");
      },
    });
    expect(await sendReport(personDraft(), p)).toEqual({ kind: "failed" });
  });

  it("refuses a list older than the highest seen", async () => {
    const { ports: p, sealed } = ports({ readFloor: () => 5n, listVersion: () => 4n });
    expect(await sendReport(personDraft(), p)).toEqual({ kind: "unavailable" });
    expect(sealed).toHaveLength(0);
  });

  it("is unavailable for a list that does not verify", async () => {
    const { ports: p } = ports({
      listVersion: () => {
        throw new Error("bad signature");
      },
    });
    expect(await sendReport(personDraft(), p)).toEqual({ kind: "unavailable" });
  });

  it("keeps the new floor and seals a person's target with the messages still here", async () => {
    const { ports: p, sealed, floors } = ports();
    const out = await sendReport(personDraft([1, 2]), p);
    expect(out.kind).toBe("sent");
    expect(floors).toEqual([2n]);
    expect(sealed).toHaveLength(1);
    const spec = sealed[0] as SealSpec;
    expect(spec.targetKind).toBe(1);
    expect(spec.target[0]).toBe(7);
    // Message 2 has no evidence on this device: it is left out.
    expect(spec.messages).toHaveLength(1);
    expect(Array.from(spec.messages[0]?.plaintext ?? [])).toEqual([9, 9]);
    expect(spec.messages[0]?.binding.length).toBe(0);
  });

  it("targets a group by its name's lookup key and fails when it cannot be had", async () => {
    const draft = withReason(
      newDraft({ target: { kind: "group", groupIdB64: "G", handle: "club", name: "Club" }, conversationKind: 2 }),
      2,
    );
    const ok = ports();
    expect((await sendReport(draft, ok.ports)).kind).toBe("sent");
    expect(ok.sealed[0]?.targetKind).toBe(2);
    expect(ok.sealed[0]?.target.length).toBe(64);
    const missing = ports({ groupLookupKey: async () => null });
    expect(await sendReport(draft, missing.ports)).toEqual({ kind: "failed" });
    const broken = ports({
      groupLookupKey: async () => {
        throw new Error("offline");
      },
    });
    expect(await sendReport(draft, broken.ports)).toEqual({ kind: "failed" });
  });

  it("carries a channel's picked posts this page still holds", async () => {
    const hex = "ab".repeat(16);
    const draft = withReason(
      newDraft({
        target: { kind: "channel", channelIdHex: hex, name: "News" },
        conversationKind: 3,
        pickFrom: { kind: "channel", channelIdHex: hex },
        picked: [10, 11],
      }),
      4,
    );
    const { ports: p, sealed } = ports();
    expect((await sendReport(draft, p)).kind).toBe("sent");
    expect(sealed[0]?.targetKind).toBe(3);
    expect(sealed[0]?.fixed).toEqual([{ kind: "channel-post", channelIdHex: hex, post: new Uint8Array([4]) }]);
  });

  it("passes the post's outcome through", async () => {
    const { ports: p } = ports({ post: async () => ({ kind: "limited" }) });
    expect(await sendReport(personDraft(), p)).toEqual({ kind: "limited" });
  });
});
