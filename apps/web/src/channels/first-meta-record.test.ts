// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * Seen live 2026-11-13: a public channel's first metadata record never
 * posted, and its re-issue said "not public".
 *
 * - `composeChannelMetaRecord` against the real wasm core: a record with a
 *   signed `@name`, a photo and reactions composes (the composer used to
 *   free the objects the core had taken by value, which throws).
 * - `planRecordReissue`: a re-issue with no held record states the channel
 *   from the owner's access record, and a held record that contradicts the
 *   owner's setting is corrected under a fresh stamp.
 */
import { beforeAll, describe, expect, it } from "vitest";
import { readFileSync } from "node:fs";
import { fileURLToPath } from "node:url";

import * as wasm from "../../wasm/lakko_ffi_wasm";
import init from "../../wasm/lakko_ffi_wasm";
import {
  composeChannelMetaRecord,
  oweRecordIfContradicted,
  planRecordReissue,
  type RecordReissueSources,
} from "./channel-meta";
import { channelsActions } from "../state/channels-state";
import type { ChannelSharedMeta } from "./channel-log-codec";
import { bytesToB64, type AvatarData } from "../profile/avatar-model";

beforeAll(async () => {
  const wasmPath = fileURLToPath(new URL("../../wasm/lakko_ffi_wasm_bg.wasm", import.meta.url));
  await init({ module_or_path: readFileSync(wasmPath) });
});

function signedHandle(isPublic: boolean): NonNullable<ChannelSharedMeta["handle"]> {
  const h = wasm.ConversationHandle.sign(
    new Uint8Array(32).fill(1),
    new Uint8Array(32).fill(2),
    5n,
    isPublic,
    "walk_28_channel",
    new Uint8Array(169).fill(3),
  );
  try {
    return { handle: "walk_28_channel", isPublic, envelopeB64: bytesToB64(wasm.composeConversationHandleEnvelope(h)) };
  } finally {
    h.free();
  }
}

const ref = (fill: number): AvatarData["still"] => ({
  blobIdB64: bytesToB64(new Uint8Array(32).fill(fill)),
  keyB64: bytesToB64(new Uint8Array(32).fill(fill + 1)),
  nonceB64: bytesToB64(new Uint8Array(12).fill(fill + 2)),
  size: 1000,
  side: 256,
});

describe("composeChannelMetaRecord through the core", () => {
  it("composes a public record with a signed name, a photo and reactions", () => {
    const out = composeChannelMetaRecord(wasm, {
      name: "Walk 28 channel",
      description: "",
      stampMs: 10n,
      topic: 3,
      language: "en",
      listed: true,
      handle: signedHandle(true),
      avatar: { still: ref(4), video: null },
      reactions: { mode: "only", emoji: ["👍"] },
    });
    expect(out.handleKept).toBe(true);
    const c = wasm.classifyChannelPostPlaintext(out.plaintext);
    try {
      expect(c.kind).toBe("meta");
      expect(c.listed).toBe(true);
      const h = c.handle;
      expect(h?.handle).toBe("walk_28_channel");
      expect(h?.public).toBe(true);
      h?.free();
    } finally {
      c.free();
    }
  });

  it("leaves out a signed name whose flag is not the record's, instead of failing", () => {
    const out = composeChannelMetaRecord(wasm, {
      name: "Walk 28 channel",
      description: "",
      stampMs: 10n,
      topic: 0,
      language: "",
      listed: false,
      handle: signedHandle(true),
      avatar: undefined,
      reactions: undefined,
    });
    expect(out.handleKept).toBe(false);
  });
});

const publicAccess = {
  visibility: "public" as const,
  handle: "walk_28_channel",
  handleEnvelopeB64: "ZW52",
  topic: 3,
  language: "en",
  avatar: null,
};

const base: RecordReissueSources = {
  held: undefined,
  access: publicAccess,
  localName: "Walk 28 channel",
  localDescription: "About",
  isOwner: true,
  nowMs: 1_000,
};

describe("planRecordReissue", () => {
  it("with no held record, states a public channel as public with its signed name", () => {
    const plan = planRecordReissue(base);
    expect(plan.kind).toBe("fresh");
    if (plan.kind !== "fresh") return;
    expect(plan.input).toMatchObject({
      name: "Walk 28 channel",
      description: "About",
      listed: true,
      topic: 3,
      language: "en",
      handle: { handle: "walk_28_channel", isPublic: true, envelopeB64: "ZW52" },
    });
    expect(plan.stampMs).toBe(1_000n);
  });

  it("posts nothing when the flag is not known here", () => {
    expect(planRecordReissue({ ...base, access: { ...publicAccess, visibility: null } }).kind).toBe("none");
    expect(planRecordReissue({ ...base, localName: "  " }).kind).toBe("none");
  });

  it("re-issues an agreeing held record verbatim with its original stamp", () => {
    const held: ChannelSharedMeta = {
      name: "Walk 28 channel",
      description: "",
      updatedAtMs: 500n,
      listed: true,
      handle: { handle: "walk_28_channel", isPublic: true, envelopeB64: "ZW52" },
    };
    const plan = planRecordReissue({ ...base, held });
    expect(plan).toEqual({ kind: "verbatim", input: held, stampMs: 500n });
  });

  it("corrects a held record that says a public channel is not public, under a newer stamp", () => {
    const held: ChannelSharedMeta = { name: "Walk 28 channel", description: "", updatedAtMs: 5_000n, listed: false };
    const plan = planRecordReissue({ ...base, held });
    expect(plan.kind).toBe("fresh");
    if (plan.kind !== "fresh") return;
    expect(plan.input.listed).toBe(true);
    expect(plan.input.handle).toMatchObject({ handle: "walk_28_channel", isPublic: true });
    // The clock is behind the held stamp: still strictly newer.
    expect(plan.stampMs).toBe(5_001n);
  });

  it("a co-publisher re-issues what it holds verbatim", () => {
    const held: ChannelSharedMeta = { name: "N", description: "", updatedAtMs: 7n, listed: false };
    expect(planRecordReissue({ ...base, held, isOwner: false }).kind).toBe("verbatim");
  });
});

describe("oweRecordIfContradicted", () => {
  const OWNED = "44".repeat(16);
  const KEYS = { seed: new Uint8Array(32).fill(1), signingKeySeed: new Uint8Array(32).fill(2), seedEpoch: 0 };

  it("owes a corrected record when the owner holds one saying its public channel is not public", () => {
    channelsActions.setKeys(OWNED, KEYS);
    channelsActions.updateAccess(OWNED, (r) => ({ ...r, visibility: "public" }));
    channelsActions.setSharedMeta(OWNED, { name: "N", description: "", updatedAtMs: 5n, listed: false });
    expect(oweRecordIfContradicted(OWNED)).toBe(true);
    expect(channelsActions.accessOf(OWNED).recordOwed).toBe(true);
    // Owed once: nothing more to mark while it is owed.
    expect(oweRecordIfContradicted(OWNED)).toBe(false);
  });

  it("leaves an agreeing record alone", () => {
    const id = "55".repeat(16);
    channelsActions.setKeys(id, KEYS);
    channelsActions.updateAccess(id, (r) => ({ ...r, visibility: "public" }));
    channelsActions.setSharedMeta(id, { name: "N", description: "", updatedAtMs: 5n, listed: true });
    expect(oweRecordIfContradicted(id)).toBe(false);
    expect(channelsActions.accessOf(id).recordOwed).toBeUndefined();
  });
});
