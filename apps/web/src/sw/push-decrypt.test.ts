// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * Unit tests for the read-only throwaway push-decrypt core
 * (`public/sw-push-decrypt.js`). We drive the pure `decryptPushPreview`
 * with fully mocked IDB / wasm / DS deps and assert:
 *
 *   * the happy path decrypts to the REAL newest-message preview,
 *   * every missing input (no account / key / seed / snapshot / DS
 *     failure) degrades to `null` (→ the SW shows generic),
 *   * a frame that fails to decrypt is DROPPED, never guessed
 *     (fail-closed, invariant #4),
 *   * the newest decryptable message wins,
 *   * edit surfaces new text; delete/reaction/typing/read-receipt do not,
 *   * the app's persisted cursor is used as the READ-ONLY fetch start,
 *   * and NO persist / cursor-advance hook is EVER called.
 *
 * The module under test is a `public/*.js` ESM (the one shape reachable
 * from both the verbatim-copied service worker and this Node test).
 */

import { describe, expect, it, vi } from "vitest";
// @ts-expect-error — plain JS ESM in public/, no .d.ts; typed via JSDoc.
import { decryptPushPreview } from "../../public/sw-push-decrypt.js";

// A valid 64-hex-char group id.
const GROUP_HEX = "aa".repeat(32);
const GROUP_BYTES = new Uint8Array(32).fill(0xaa);
const ACCOUNT_ID = "acct-b64";

/** Build a DS message whose payload the mock wasm interprets. */
function msg(id: number, parsed: unknown, opts: { throwOnProcess?: boolean } = {}) {
  const marker = opts.throwOnProcess
    ? "THROW"
    : JSON.stringify(parsed);
  return {
    id,
    payload: new TextEncoder().encode(marker),
    createdAt: new Date(id * 1000).toISOString(),
  };
}

/**
 * Mock wasm core. `processIncomingVerifyingRoles` throws when the
 * payload marker is "THROW", else surfaces it as an Application frame
 * whose `applicationData` is the payload. `parseBodyEnvelope` JSON-
 * decodes that payload into a flat `ParsedBody`-shaped object.
 */
function mockWasm() {
  const freeSpy = vi.fn();
  return {
    freeSpy,
    Identity: {
      fromSeed: (_seed: Uint8Array) => ({
        restoreStorage: (_b: Uint8Array) => undefined,
        free: freeSpy,
      }),
    },
    Group: {
      load: (_identity: unknown, _groupId: Uint8Array) => ({
        processIncomingVerifyingRoles: (_identity: unknown, payload: Uint8Array) => {
          const s = new TextDecoder().decode(payload);
          if (s === "THROW") throw new Error("cannot decrypt (wrong epoch)");
          return { kind: "Application", applicationData: payload, senderLeaf: 1 };
        },
        free: freeSpy,
      }),
    },
    parseBodyEnvelope: (bytes: Uint8Array) => {
      const parsed = JSON.parse(new TextDecoder().decode(bytes)) as Record<string, unknown>;
      return { ...parsed, free: freeSpy };
    },
  };
}

/** A full, working deps object; individual tests override slices. */
function baseDeps(overrides: Record<string, unknown> = {}) {
  const wasm = mockWasm();
  const saveSnapshot = vi.fn();
  const saveMessageLog = vi.fn();
  const advanceCursor = vi.fn();
  const dsFetchGroup = vi.fn(async (_g: Uint8Array, _after: number) => ({
    messages: [msg(42, { kind: "chat", text: "hello world", attachmentCount: 0 })],
  }));
  const deps = {
    resolveAccount: vi.fn(async (_hex: string) => ({
      accountId: ACCOUNT_ID,
      groupIdBytes: GROUP_BYTES,
    })),
    getDeviceKey: vi.fn(async (_a: string) => ({}) as unknown),
    getIdentitySeed: vi.fn(async (_a: string, _k: unknown) => new Uint8Array(64)),
    loadSnapshot: vi.fn(async (_a: string, _k: unknown) => ({
      snapshotBytes: new Uint8Array([1, 2, 3]),
      knownGroupIds: [],
    })),
    wasm,
    dsFetchGroup,
    startCursor: vi.fn(async (_a: string, _h: string) => null),
    // Forbidden — must never be invoked.
    saveSnapshot,
    saveMessageLog,
    advanceCursor,
    ...overrides,
  };
  return { deps, wasm, saveSnapshot, saveMessageLog, advanceCursor, dsFetchGroup };
}

const wake = (over: Record<string, unknown> = {}) => ({
  deviceHex: "dd".repeat(32),
  groupHex: GROUP_HEX,
  pointer: 42,
  ...over,
});

describe("decryptPushPreview — happy path", () => {
  it("decrypts to the real newest-message preview", async () => {
    const { deps, saveSnapshot, saveMessageLog, advanceCursor } = baseDeps();
    const out = await decryptPushPreview(wake(), deps);
    expect(out).toEqual({ title: "Lakko", body: "hello world", tag: GROUP_HEX });
    // Read-only-throwaway: nothing was ever persisted.
    expect(saveSnapshot).not.toHaveBeenCalled();
    expect(saveMessageLog).not.toHaveBeenCalled();
    expect(advanceCursor).not.toHaveBeenCalled();
  });

  it("returns the NEWEST decryptable message when several arrive", async () => {
    const { deps } = baseDeps({
      dsFetchGroup: vi.fn(async () => ({
        messages: [
          msg(40, { kind: "chat", text: "first", attachmentCount: 0 }),
          msg(41, { kind: "chat", text: "second", attachmentCount: 0 }),
          msg(42, { kind: "chat", text: "third", attachmentCount: 0 }),
        ],
      })),
    });
    const out = await decryptPushPreview(wake(), deps);
    expect(out?.body).toBe("third");
  });

  it("surfaces an edit's new text and skips delete/reaction/typing", async () => {
    const { deps } = baseDeps({
      dsFetchGroup: vi.fn(async () => ({
        messages: [
          msg(40, { kind: "chat", text: "original", attachmentCount: 0 }),
          msg(41, { kind: "edit", text: "edited text", target: 40 }),
          msg(42, { kind: "reaction", emoji: "👍", target: 40 }),
          msg(43, { kind: "typing", isTyping: true }),
        ],
      })),
    });
    const out = await decryptPushPreview(wake({ pointer: 43 }), deps);
    // The reaction / typing after the edit carry no preview, so the
    // edit (newest previewable) wins.
    expect(out?.body).toBe("edited text");
  });

  it("renders an attachment hint for an attachment-only message", async () => {
    const { deps } = baseDeps({
      dsFetchGroup: vi.fn(async () => ({
        messages: [msg(42, { kind: "chat", text: "", attachmentCount: 2 })],
      })),
    });
    const out = await decryptPushPreview(wake(), deps);
    expect(out?.body).toBe("Sent 2 attachments");
  });

  it("uses the app's persisted cursor as the READ-ONLY fetch start", async () => {
    const { deps, dsFetchGroup } = baseDeps({
      startCursor: vi.fn(async () => 30),
    });
    await decryptPushPreview(wake({ pointer: 42 }), deps);
    // Fetch started at the persisted cursor (30), not pointer-1 (41),
    // so intervening commits replay and the epoch stays correct.
    expect(dsFetchGroup).toHaveBeenCalledWith(GROUP_BYTES, 30);
  });

  it("falls back to pointer-1 when no persisted cursor is available", async () => {
    const { deps, dsFetchGroup } = baseDeps({
      startCursor: vi.fn(async () => null),
    });
    await decryptPushPreview(wake({ pointer: 42 }), deps);
    expect(dsFetchGroup).toHaveBeenCalledWith(GROUP_BYTES, 41);
  });
});

describe("decryptPushPreview — fail-closed / null fallbacks", () => {
  it("returns null for a Wakeup/Mailbox wake (no groupHex / pointer)", async () => {
    const { deps, dsFetchGroup } = baseDeps();
    expect(await decryptPushPreview(wake({ groupHex: null }), deps)).toBeNull();
    expect(await decryptPushPreview(wake({ pointer: null }), deps)).toBeNull();
    expect(await decryptPushPreview(null, deps)).toBeNull();
    expect(dsFetchGroup).not.toHaveBeenCalled();
  });

  it("returns null for a malformed groupHex", async () => {
    const { deps } = baseDeps();
    expect(await decryptPushPreview(wake({ groupHex: "zz".repeat(32) }), deps)).toBeNull();
    expect(await decryptPushPreview(wake({ groupHex: "aa" }), deps)).toBeNull();
  });

  it("returns null when no local account owns the group", async () => {
    const { deps } = baseDeps({ resolveAccount: vi.fn(async () => null) });
    expect(await decryptPushPreview(wake(), deps)).toBeNull();
    // Short-circuits before touching the device key.
    expect(deps.getDeviceKey).not.toHaveBeenCalled();
  });

  it("returns null when the device key is missing", async () => {
    const { deps } = baseDeps({ getDeviceKey: vi.fn(async () => null) });
    expect(await decryptPushPreview(wake(), deps)).toBeNull();
  });

  it("returns null when the identity seed is missing", async () => {
    const { deps } = baseDeps({ getIdentitySeed: vi.fn(async () => null) });
    expect(await decryptPushPreview(wake(), deps)).toBeNull();
  });

  it("returns null when there is no MLS snapshot", async () => {
    const { deps } = baseDeps({ loadSnapshot: vi.fn(async () => null) });
    expect(await decryptPushPreview(wake(), deps)).toBeNull();
  });

  it("returns null when the DS fetch fails", async () => {
    const { deps } = baseDeps({
      dsFetchGroup: vi.fn(async () => {
        throw new Error("network down");
      }),
    });
    expect(await decryptPushPreview(wake(), deps)).toBeNull();
  });

  it("drops a frame that will not decrypt and returns null if all fail", async () => {
    const { deps } = baseDeps({
      dsFetchGroup: vi.fn(async () => ({
        messages: [msg(42, {}, { throwOnProcess: true })],
      })),
    });
    expect(await decryptPushPreview(wake(), deps)).toBeNull();
  });

  it("skips a non-decryptable frame but still previews a later good one", async () => {
    const { deps } = baseDeps({
      dsFetchGroup: vi.fn(async () => ({
        messages: [
          msg(41, {}, { throwOnProcess: true }),
          msg(42, { kind: "chat", text: "made it", attachmentCount: 0 }),
        ],
      })),
    });
    const out = await decryptPushPreview(wake(), deps);
    expect(out?.body).toBe("made it");
  });

  it("returns null when Group.load yields no group", async () => {
    const { deps, wasm } = baseDeps();
    (wasm.Group as { load: (i: unknown, g: Uint8Array) => unknown }).load = () =>
      undefined;
    expect(await decryptPushPreview(wake(), deps)).toBeNull();
  });

  it("NEVER persists even across the failure branches", async () => {
    const { deps, saveSnapshot, saveMessageLog, advanceCursor } = baseDeps({
      dsFetchGroup: vi.fn(async () => ({
        messages: [msg(42, {}, { throwOnProcess: true })],
      })),
    });
    await decryptPushPreview(wake(), deps);
    expect(saveSnapshot).not.toHaveBeenCalled();
    expect(saveMessageLog).not.toHaveBeenCalled();
    expect(advanceCursor).not.toHaveBeenCalled();
  });
});
