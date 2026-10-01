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
 *   * an ADR-0042 `push_gid` wake reverse-maps via the injected
 *     `lookupRoute` to the REAL group (miss → legacy raw-group_id path;
 *     lookup error / malformed row → legacy path, never a throw),
 *   * the ADR-0042 (2026-09-30) wake gate: no binding or a `d` that is not
 *     the bound pseudonym decrypts nothing, and the route map + the legacy
 *     scan are asked about the BOUND account only,
 *   * and NO persist / cursor-advance hook is EVER called.
 *
 * The module under test is a `public/*.js` ESM (the one shape reachable
 * from both the verbatim-copied service worker and this Node test).
 */

import { describe, expect, it, vi } from "vitest";
// @ts-expect-error — plain JS ESM in public/, no .d.ts; typed via JSDoc.
import { decryptPushPreview, resolveWakeTarget, wakeBoundAccount } from "../../public/sw-push-decrypt.js";

// A valid 64-hex-char group id.
const GROUP_HEX = "aa".repeat(32);
const GROUP_BYTES = new Uint8Array(32).fill(0xaa);
const ACCOUNT_ID = "acct-b64";
// The ADR-0042 opaque push_gid a post-Sprint-3 wake carries as `g` —
// same 64-lowercase-hex shape as a group id, but NOT a group we know.
const PUSH_GID_HEX = "cc".repeat(32);
const GROUP_B64 = btoa(String.fromCharCode(...GROUP_BYTES));

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

/** Build a DS message carrying RAW (non-JSON) application bytes — the
 *  shape every service envelope has on the wire. */
function rawMsg(id: number, payload: Uint8Array) {
  return { id, payload, createdAt: new Date(id * 1000).toISOString() };
}

// --- Service-envelope fixtures -------------------------------------------
// Both ride the group journal as MLS Application messages, and neither is
// a user-visible message.

/** `lakko_contacts::push_routing::PUSH_ROUTING_SECRET_ENVELOPE_DOMAIN_V1`
 *  — `b"lakko/push-routing-secret/v1\x00"`, 29 bytes. The trailing NUL is
 *  appended programmatically so this source file stays NUL-free. */
const PUSH_ROUTING_DOMAIN = (() => {
  const ascii = new TextEncoder().encode("lakko/push-routing-secret/v1");
  const out = new Uint8Array(ascii.length + 1);
  out.set(ascii, 0);
  out[ascii.length] = 0x00;
  return out;
})();

/** ADR-0044 v2 envelope: domain(29) || version(1)=0x02 || mint_epoch(8 BE)
 *  || secret(32) = 70 bytes. First byte is `l` (0x6c). */
function routingSecretEnvelope(mintEpoch = 7, secretFill = 0xab): Uint8Array {
  const out = new Uint8Array(70);
  out.set(PUSH_ROUTING_DOMAIN, 0);
  out[29] = 0x02;
  // 8-byte big-endian mint_epoch at offset 30.
  new DataView(out.buffer).setBigUint64(30, BigInt(mintEpoch), false);
  out.fill(secretFill, 38);
  return out;
}

/** A prost-encoded `CallSignalingPayload`. The first byte is the `body`
 *  oneof field tag — 0x52 (offer) / 0x5A / 0x62 / 0x6A. */
function callSignalingPayload(tag = 0x52): Uint8Array {
  return new Uint8Array([tag, 0x08, 0x01, 0x12, 0x02, 0x68, 0x69]);
}

/** `lakko_contacts::committer_beacon::COMMITTER_BEACON_ENVELOPE_DOMAIN_V1`
 *  — `b"lakko/committer-beacon/v1\x00"`, 26 bytes. The trailing NUL is
 *  appended programmatically so this source file stays NUL-free. */
const COMMITTER_BEACON_DOMAIN = (() => {
  const ascii = new TextEncoder().encode("lakko/committer-beacon/v1");
  const out = new Uint8Array(ascii.length + 1);
  out.set(ascii, 0);
  out[ascii.length] = 0x00;
  return out;
})();

/** The FFI-reported envelope length: domain(26) || version(1) = 27. */
const COMMITTER_BEACON_LEN = COMMITTER_BEACON_DOMAIN.length + 1;

/** ADR-0045 committer beacon: domain(26) || version(1)=0x01 = 27 bytes.
 *  Field-less, so every beacon on the wire is byte-identical — which is
 *  what lets the filter's peek be a complete test. First byte is `l`. */
function committerBeacon(version = 0x01): Uint8Array {
  const out = new Uint8Array(COMMITTER_BEACON_LEN);
  out.set(COMMITTER_BEACON_DOMAIN, 0);
  out[COMMITTER_BEACON_DOMAIN.length] = version;
  return out;
}

/** `lakko_contacts::profile_request::PROFILE_REQUEST_ENVELOPE_DOMAIN_V1`
 *  — `b"lakko/profile-request/v1\x00"`, 25 bytes; NUL appended
 *  programmatically, as above. */
const PROFILE_REQUEST_DOMAIN = (() => {
  const ascii = new TextEncoder().encode("lakko/profile-request/v1");
  const out = new Uint8Array(ascii.length + 1);
  out.set(ascii, 0);
  out[ascii.length] = 0x00;
  return out;
})();

/** The FFI-reported envelope length: domain(25) || version(1) = 26. */
const PROFILE_REQUEST_LEN = PROFILE_REQUEST_DOMAIN.length + 1;

/** ADR-0048 profile request: domain(25) || version(1)=0x01 = 26 bytes.
 *  Field-less like the beacon; first byte is `l`. */
function profileRequest(version = 0x01): Uint8Array {
  const out = new Uint8Array(PROFILE_REQUEST_LEN);
  out.set(PROFILE_REQUEST_DOMAIN, 0);
  out[PROFILE_REQUEST_DOMAIN.length] = version;
  return out;
}

/**
 * Mock wasm core. `processIncomingVerifyingRoles` throws when the
 * payload marker is "THROW", else surfaces it as an Application frame
 * whose `applicationData` is the payload.
 *
 * `parseBodyEnvelope` models the REAL `lakko_attachments::parse_body`
 * (crates/lakko-attachments/src/lib.rs:634-646), including its deliberate
 * pre-1.19 wire-compat fallback: a body that is NOT valid JSON degrades to
 * `Chat { text: String::from_utf8_lossy(bytes) }` rather than erroring.
 * Modelling that faithfully is what makes the service-envelope tests
 * DISCRIMINATE — a mock that simply threw on non-JSON would mask the very
 * defect (a service envelope rendered as a garbage notification body).
 *
 * `decodePushRoutingSecret` models the fail-closed wasm codec: it THROWS
 * unless the bytes are exactly the ADR-0044 v2 envelope.
 *
 * `decodeCommitterBeacon` models its ADR-0045 twin, including the detail
 * that makes the beacon's filter arm different from the routing one: it
 * returns NOTHING on success (the envelope is field-less, so there is no
 * handle to free), and throws on wrong domain / bad length / unknown
 * version. `committerBeaconEnvelopeLen` models the FFI constant the
 * filter reads instead of hardcoding 27.
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
        // ADR-0053 — the throwaway group's access signer; `sign` echoes its
        // inputs so a test can read what was signed.
        dsAccessSigner: (_identity: unknown) => ({
          sign: (method: string, path: string, query: string, body: Uint8Array) =>
            `proof:${method}:${path}:${query}:${body.length}`,
          free: freeSpy,
        }),
        processIncomingVerifyingRoles: (_identity: unknown, payload: Uint8Array) => {
          const s = new TextDecoder().decode(payload);
          if (s === "THROW") throw new Error("cannot decrypt (wrong epoch)");
          return { kind: "Application", applicationData: payload, senderLeaf: 1 };
        },
        free: freeSpy,
      }),
    },
    parseBodyEnvelope: (bytes: Uint8Array) => {
      const text = new TextDecoder().decode(bytes);
      try {
        const parsed = JSON.parse(text) as Record<string, unknown>;
        return { ...parsed, free: freeSpy };
      } catch {
        // The legacy UTF-8 fallback — exactly what surfaces a service
        // envelope as a "chat" body of domain string + binary garbage.
        return { kind: "chat", text, attachmentCount: 0, free: freeSpy };
      }
    },
    decodePushRoutingSecret: (bytes: Uint8Array) => {
      if (bytes.length !== 70) throw new Error("BadLength");
      for (let i = 0; i < PUSH_ROUTING_DOMAIN.length; i++) {
        if (bytes[i] !== PUSH_ROUTING_DOMAIN[i]) throw new Error("WrongDomain");
      }
      if (bytes[29] !== 0x02) throw new Error("UnknownVersion");
      return {
        secret: bytes.slice(38),
        mintEpoch: new DataView(bytes.buffer, bytes.byteOffset).getBigUint64(30, false),
        free: freeSpy,
      };
    },
    committerBeaconEnvelopeLen: () => COMMITTER_BEACON_LEN,
    decodeCommitterBeacon: (bytes: Uint8Array) => {
      if (bytes.length !== COMMITTER_BEACON_LEN) throw new Error("BadLength");
      for (let i = 0; i < COMMITTER_BEACON_DOMAIN.length; i++) {
        if (bytes[i] !== COMMITTER_BEACON_DOMAIN[i]) throw new Error("WrongDomain");
      }
      if (bytes[COMMITTER_BEACON_DOMAIN.length] !== 0x01) throw new Error("UnknownVersion");
      // Field-less: a clean return IS the whole result.
    },
    profileRequestEnvelopeLen: () => PROFILE_REQUEST_LEN,
    decodeProfileRequest: (bytes: Uint8Array) => {
      if (bytes.length !== PROFILE_REQUEST_LEN) throw new Error("BadLength");
      for (let i = 0; i < PROFILE_REQUEST_DOMAIN.length; i++) {
        if (bytes[i] !== PROFILE_REQUEST_DOMAIN[i]) throw new Error("WrongDomain");
      }
      if (bytes[PROFILE_REQUEST_DOMAIN.length] !== 0x01) throw new Error("UnknownVersion");
    },
  };
}

/** A full, working deps object; individual tests override slices. */
function baseDeps(overrides: Record<string, unknown> = {}) {
  const wasm = mockWasm();
  const saveSnapshot = vi.fn();
  const saveMessageLog = vi.fn();
  const advanceCursor = vi.fn();
  const dsFetchGroup = vi.fn(async (_g: Uint8Array, _after: number, _sign?: unknown) => ({
    messages: [msg(42, { kind: "chat", text: "hello world", attachmentCount: 0 })],
  }));
  const deps = {
    readWakeBinding: vi.fn(async () => ({
      devicePseudonymHex: "dd".repeat(32),
      accountId: ACCOUNT_ID,
    })),
    resolveAccount: vi.fn(async (_account: string, _hex: string) => ({
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
    expect(dsFetchGroup).toHaveBeenCalledWith(GROUP_BYTES, 30, expect.any(Function));
  });

  it("falls back to pointer-1 when no persisted cursor is available", async () => {
    const { deps, dsFetchGroup } = baseDeps({
      startCursor: vi.fn(async () => null),
    });
    await decryptPushPreview(wake({ pointer: 42 }), deps);
    expect(dsFetchGroup).toHaveBeenCalledWith(GROUP_BYTES, 41, expect.any(Function));
  });
});

describe("decryptPushPreview — ADR-0053 contract point 9, the signed wake read", () => {
  it("hands the fetch a signer over the throwaway group's access key", async () => {
    const { deps, dsFetchGroup } = baseDeps();
    await decryptPushPreview(wake({ pointer: 42 }), deps);
    const signProof = dsFetchGroup.mock.calls[0]?.[2] as
      | ((m: string, p: string, q: string) => string)
      | undefined;
    // The proof covers the SERVICE path and the query exactly as sent, over
    // an empty body (a GET). The fetch runs while the signer is alive.
    expect(dsFetchGroup).toHaveBeenCalledTimes(1);
    expect(typeof signProof).toBe("function");
  });

  it("the signer the fetch was handed signs what the SW sends", async () => {
    let header: string | null = null;
    const { deps } = baseDeps({
      dsFetchGroup: vi.fn(
        async (_g: Uint8Array, after: number, sign: ((m: string, p: string, q: string) => string) | null) => {
          header = sign === null ? null : sign("GET", `/ds/group/${GROUP_HEX}/messages`, `after=${after}&wait=0`);
          return { messages: [msg(42, { kind: "chat", text: "hi", attachmentCount: 0 })] };
        },
      ),
    });
    const out = await decryptPushPreview(wake({ pointer: 42 }), deps);
    expect(out?.body).toBe("hi");
    expect(header).toBe(`proof:GET:/ds/group/${GROUP_HEX}/messages:after=41&wait=0:0`);
  });

  it("a group that cannot derive a key reads unsigned, and a refusal falls back to generic", async () => {
    const { deps, wasm } = baseDeps({
      dsFetchGroup: vi.fn(async (_g: Uint8Array, _a: number, sign: unknown) => {
        if (sign === null) throw new Error("ds fetch 401");
        return { messages: [] };
      }),
    });
    const load = wasm.Group.load;
    wasm.Group.load = (id: unknown, gid: Uint8Array) => {
      const g = load(id, gid) as Record<string, unknown>;
      g.dsAccessSigner = () => {
        throw new Error("inactive");
      };
      return g as ReturnType<typeof load>;
    };
    expect(await decryptPushPreview(wake({ pointer: 42 }), deps)).toBeNull();
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

describe("decryptPushPreview — service envelopes are never previewed", () => {
  // `parse_body` degrades non-JSON bytes to a `chat` body via
  // `from_utf8_lossy`, so without the filter each of these renders a
  // notification containing its domain string plus raw binary.

  it("returns null for a lone push-routing-secret envelope (ADR-0042/0044)", async () => {
    const { deps } = baseDeps({
      dsFetchGroup: vi.fn(async () => ({
        messages: [rawMsg(42, routingSecretEnvelope())],
      })),
    });
    expect(await decryptPushPreview(wake(), deps)).toBeNull();
  });

  it("returns null for a lone call-signaling payload", async () => {
    for (const tag of [0x52, 0x5a, 0x62, 0x6a]) {
      const { deps } = baseDeps({
        dsFetchGroup: vi.fn(async () => ({
          messages: [rawMsg(42, callSignalingPayload(tag))],
        })),
      });
      expect(await decryptPushPreview(wake(), deps)).toBeNull();
    }
  });

  it("returns null for a lone group-call control body (start / join / leave)", async () => {
    // Fields 14 / 15 / 16 of `CallSignalingPayload`: 0x72, 0x7A and the
    // two-byte tag 0x82 0x01. Before the fix these reached the lossy-UTF-8
    // chat fallback and became a notification of binary garbage.
    const bodies = [
      callSignalingPayload(0x72),
      callSignalingPayload(0x7a),
      new Uint8Array([0x82, 0x01, 0x08, 0x01, 0x12, 0x02, 0x68, 0x69]),
    ];
    for (const body of bodies) {
      const { deps } = baseDeps({
        dsFetchGroup: vi.fn(async () => ({
          messages: [rawMsg(42, body)],
        })),
      });
      expect(await decryptPushPreview(wake(), deps)).toBeNull();
    }
  });

  it("does not take a lone 0x82 without the 0x01 continuation for signaling", async () => {
    // 0x82 alone is not the field-16 tag; the peek must not swallow it.
    const { deps } = baseDeps({
      dsFetchGroup: vi.fn(async () => ({
        messages: [rawMsg(42, new Uint8Array([0x82, 0x02, 0x68, 0x69]))],
      })),
    });
    expect(await decryptPushPreview(wake(), deps)).not.toBeNull();
  });

  it("previews the chat message when a service envelope precedes it", async () => {
    const { deps } = baseDeps({
      dsFetchGroup: vi.fn(async () => ({
        messages: [
          rawMsg(40, routingSecretEnvelope()),
          rawMsg(41, callSignalingPayload()),
          msg(42, { kind: "chat", text: "real message", attachmentCount: 0 }),
        ],
      })),
    });
    const out = await decryptPushPreview(wake({ pointer: 42 }), deps);
    expect(out?.body).toBe("real message");
  });

  it("falls back to the older chat message when a service envelope is NEWEST", async () => {
    // The load-bearing ordering: the envelope is the newest row, so
    // before the fix it OVERWROTE the real preview with its garbage.
    const { deps } = baseDeps({
      dsFetchGroup: vi.fn(async () => ({
        messages: [
          msg(40, { kind: "chat", text: "real message", attachmentCount: 0 }),
          rawMsg(41, callSignalingPayload()),
          rawMsg(42, routingSecretEnvelope()),
        ],
      })),
    });
    const out = await decryptPushPreview(wake({ pointer: 42 }), deps);
    expect(out?.body).toBe("real message");
  });

  it("still previews a 70-byte `l`-prefixed body the codec REFUSES", async () => {
    // The cheap peek matches but the authoritative codec rejects (bad
    // version), so this is a genuine message — same verdict the
    // foreground reaches. Proves the peek alone does not suppress.
    const notAnEnvelope = routingSecretEnvelope();
    notAnEnvelope[29] = 0x01; // retired v1 version byte
    const { deps } = baseDeps({
      dsFetchGroup: vi.fn(async () => ({ messages: [rawMsg(42, notAnEnvelope)] })),
    });
    const out = await decryptPushPreview(wake(), deps);
    expect(out?.body).not.toBeNull();
    expect(out?.body).toContain("lakko/push-routing-secret");
  });

  it("classifies without ever persisting (C-59 read-only discipline)", async () => {
    const { deps, saveSnapshot, saveMessageLog, advanceCursor } = baseDeps({
      dsFetchGroup: vi.fn(async () => ({
        messages: [rawMsg(42, routingSecretEnvelope())],
      })),
    });
    await decryptPushPreview(wake(), deps);
    expect(saveSnapshot).not.toHaveBeenCalled();
    expect(saveMessageLog).not.toHaveBeenCalled();
    expect(advanceCursor).not.toHaveBeenCalled();
  });
});

describe("decryptPushPreview — the ADR-0048 profile request is never previewed", () => {
  // A newcomer asks the group to introduce itself right after joining and
  // on opening a chat with unnamed members, so the newest row of a group
  // somebody just joined is often a request. Unfiltered, the lossy-UTF-8
  // fallback renders it as "lakko/profile-request/v1" plus a control byte.

  it("the unfiltered request really would render as garbage text", () => {
    const wasm = mockWasm();
    const parsed = wasm.parseBodyEnvelope(profileRequest()) as { kind: string; text: string };
    expect(parsed.kind).toBe("chat");
    expect(parsed.text).toContain("lakko/profile-request/v1");
  });

  it("returns null for a lone profile request", async () => {
    const { deps } = baseDeps({
      dsFetchGroup: vi.fn(async () => ({ messages: [rawMsg(42, profileRequest())] })),
    });
    expect(await decryptPushPreview(wake(), deps)).toBeNull();
  });

  it("falls back to the older chat message when a request is NEWEST", async () => {
    const { deps } = baseDeps({
      dsFetchGroup: vi.fn(async () => ({
        messages: [
          msg(41, { kind: "chat", text: "real message", attachmentCount: 0 }),
          rawMsg(42, profileRequest()),
        ],
      })),
    });
    const out = await decryptPushPreview(wake({ pointer: 42 }), deps);
    expect(out?.body).toBe("real message");
  });

  it("still previews a 26-byte `l`-prefixed body the codec REFUSES", async () => {
    // The peek matches but the codec rejects (unknown version): a genuine
    // message, or a future request version this build does not know.
    const notARequest = profileRequest(0xff);
    const { deps } = baseDeps({
      dsFetchGroup: vi.fn(async () => ({ messages: [rawMsg(42, notARequest)] })),
    });
    const out = await decryptPushPreview(wake(), deps);
    expect(out?.body).toContain("lakko/profile-request");
  });

  it("takes the envelope length from the FFI, not a hardcoded 26", async () => {
    const wasm = mockWasm();
    wasm.profileRequestEnvelopeLen = () => 99;
    const { deps } = baseDeps({
      wasm,
      dsFetchGroup: vi.fn(async () => ({ messages: [rawMsg(42, profileRequest())] })),
    });
    const out = await decryptPushPreview(wake(), deps);
    expect(out?.body).toContain("lakko/profile-request");
  });

  it("without the FFI length the arm is off rather than guessing", async () => {
    // A single-byte peek would swallow any 26-byte message starting with
    // `l`; the arm refuses to run on that alone.
    const { profileRequestEnvelopeLen: omitted, ...wasm } = mockWasm();
    void omitted;
    const { deps } = baseDeps({
      wasm,
      dsFetchGroup: vi.fn(async () => ({ messages: [rawMsg(42, profileRequest())] })),
    });
    const out = await decryptPushPreview(wake(), deps);
    expect(out?.body).toContain("lakko/profile-request");
  });

  it("classifies a request without ever persisting (C-59)", async () => {
    const { deps, saveSnapshot, saveMessageLog, advanceCursor } = baseDeps({
      dsFetchGroup: vi.fn(async () => ({ messages: [rawMsg(42, profileRequest())] })),
    });
    await decryptPushPreview(wake(), deps);
    expect(saveSnapshot).not.toHaveBeenCalled();
    expect(saveMessageLog).not.toHaveBeenCalled();
    expect(advanceCursor).not.toHaveBeenCalled();
  });
});

describe("decryptPushPreview — the ADR-0045 committer beacon is never previewed", () => {
  // A beacon is emitted by EVERY owner/admin every 60 s, so in an active
  // group the newest journal row — the one previewed here — is frequently
  // a beacon. Unfiltered, `parse_body`'s lossy-UTF-8 fallback renders it
  // as the literal text "lakko/committer-beacon/v1" plus two control
  // bytes.

  it("the unfiltered beacon really would render as garbage text", () => {
    // Pins the DEFECT, not the fix: the mock's parseBodyEnvelope models
    // the real `parse_body` fallback, so this is the notification body a
    // beacon produces when nothing filters it. If this ever stops being
    // true the tests below stop discriminating and must be re-derived.
    const wasm = mockWasm();
    const parsed = wasm.parseBodyEnvelope(committerBeacon()) as { kind: string; text: string };
    expect(parsed.kind).toBe("chat");
    expect(parsed.text).toContain("lakko/committer-beacon/v1");
  });

  it("returns null for a lone committer beacon", async () => {
    const { deps } = baseDeps({
      dsFetchGroup: vi.fn(async () => ({ messages: [rawMsg(42, committerBeacon())] })),
    });
    expect(await decryptPushPreview(wake(), deps)).toBeNull();
  });

  it("previews the chat message when a beacon precedes it", async () => {
    const { deps } = baseDeps({
      dsFetchGroup: vi.fn(async () => ({
        messages: [
          rawMsg(41, committerBeacon()),
          msg(42, { kind: "chat", text: "real message", attachmentCount: 0 }),
        ],
      })),
    });
    const out = await decryptPushPreview(wake({ pointer: 42 }), deps);
    expect(out?.body).toBe("real message");
  });

  it("falls back to the older chat message when a beacon is NEWEST", async () => {
    // The load-bearing ordering, and the common case in an active group:
    // the beacon is the newest row, so before the fix it OVERWROTE the
    // real preview with its garbage.
    const { deps } = baseDeps({
      dsFetchGroup: vi.fn(async () => ({
        messages: [
          msg(41, { kind: "chat", text: "real message", attachmentCount: 0 }),
          rawMsg(42, committerBeacon()),
        ],
      })),
    });
    const out = await decryptPushPreview(wake({ pointer: 42 }), deps);
    expect(out?.body).toBe("real message");
  });

  it("still previews an unrelated chat message that starts with `l`", async () => {
    // The peek's first byte is `l`, shared with every in-group envelope
    // domain. A real message beginning with `l` must survive — the
    // authoritative codec, not the peek, is what suppresses.
    const { deps } = baseDeps({
      dsFetchGroup: vi.fn(async () => ({
        messages: [msg(42, { kind: "chat", text: "lunch at six?", attachmentCount: 0 })],
      })),
    });
    const out = await decryptPushPreview(wake(), deps);
    expect(out?.body).toBe("lunch at six?");
  });

  it("still previews a 27-byte `l`-prefixed body the codec REFUSES", async () => {
    // Same shape as the routing-secret case: the cheap peek matches but
    // the codec rejects (unknown version), so this is a genuine message.
    // Proves the peek alone does not suppress.
    const notABeacon = committerBeacon(0xff);
    const { deps } = baseDeps({
      dsFetchGroup: vi.fn(async () => ({ messages: [rawMsg(42, notABeacon)] })),
    });
    const out = await decryptPushPreview(wake(), deps);
    expect(out?.body).toContain("lakko/committer-beacon");
  });

  it("takes the envelope length from the FFI, not a hardcoded 27", async () => {
    // Discriminates "reads committerBeaconEnvelopeLen()" from "hardcodes
    // 27": with the FFI reporting a DIFFERENT length, a 27-byte beacon
    // must miss the peek. A filter carrying the literal would suppress it
    // regardless and this would fail — which is the drift ADR-0044
    // documents, caught here rather than at the next version bump.
    const wasm = mockWasm();
    wasm.committerBeaconEnvelopeLen = () => 99;
    const { deps } = baseDeps({
      wasm,
      dsFetchGroup: vi.fn(async () => ({ messages: [rawMsg(42, committerBeacon())] })),
    });
    const out = await decryptPushPreview(wake(), deps);
    expect(out?.body).toContain("lakko/committer-beacon");
  });

  it("classifies a beacon without ever persisting (C-59)", async () => {
    const { deps, saveSnapshot, saveMessageLog, advanceCursor } = baseDeps({
      dsFetchGroup: vi.fn(async () => ({ messages: [rawMsg(42, committerBeacon())] })),
    });
    await decryptPushPreview(wake(), deps);
    expect(saveSnapshot).not.toHaveBeenCalled();
    expect(saveMessageLog).not.toHaveBeenCalled();
    expect(advanceCursor).not.toHaveBeenCalled();
  });
});

describe("decryptPushPreview — ADR-0042 push_gid reverse-map (Sprint 4)", () => {
  it("resolves an opaque push_gid wake via lookupRoute to the REAL group and decrypts", async () => {
    const lookupRoute = vi.fn(async (_account: string, _gid: string) => ({
      accountId: ACCOUNT_ID,
      groupIdB64: GROUP_B64,
    }));
    const { deps, dsFetchGroup, saveSnapshot, saveMessageLog, advanceCursor } =
      baseDeps({ lookupRoute });
    const out = await decryptPushPreview(wake({ groupHex: PUSH_GID_HEX }), deps);
    // Downstream everything (DS fetch, cursor, tag) uses the REAL group.
    expect(out).toEqual({ title: "Lakko", body: "hello world", tag: GROUP_HEX });
    expect(lookupRoute).toHaveBeenCalledWith(ACCOUNT_ID, PUSH_GID_HEX);
    expect(dsFetchGroup).toHaveBeenCalledWith(GROUP_BYTES, 41, expect.any(Function));
    expect(deps.startCursor).toHaveBeenCalledWith(ACCOUNT_ID, GROUP_HEX);
    // The legacy path never ran.
    expect(deps.resolveAccount).not.toHaveBeenCalled();
    // The route lookup is a READ — still nothing persisted, ever.
    expect(saveSnapshot).not.toHaveBeenCalled();
    expect(saveMessageLog).not.toHaveBeenCalled();
    expect(advanceCursor).not.toHaveBeenCalled();
  });

  it("falls back to the legacy raw-group_id path on a route miss", async () => {
    const lookupRoute = vi.fn(async (_account: string, _gid: string) => null);
    const { deps } = baseDeps({ lookupRoute });
    // The wake's `g` IS a real group id this device knows (legacy wake /
    // not-yet-provisioned group) — unchanged pre-Sprint-4 behavior.
    const out = await decryptPushPreview(wake(), deps);
    expect(out).toEqual({ title: "Lakko", body: "hello world", tag: GROUP_HEX });
    expect(lookupRoute).toHaveBeenCalledWith(ACCOUNT_ID, GROUP_HEX);
    expect(deps.resolveAccount).toHaveBeenCalledWith(ACCOUNT_ID, GROUP_HEX);
  });

  it("treats a lookupRoute error as a miss (legacy path still works)", async () => {
    const lookupRoute = vi.fn(async () => {
      throw new Error("idb open blocked");
    });
    const { deps } = baseDeps({ lookupRoute });
    const out = await decryptPushPreview(wake(), deps);
    expect(out?.body).toBe("hello world");
    expect(deps.resolveAccount).toHaveBeenCalledWith(ACCOUNT_ID, GROUP_HEX);
  });

  it("degrades to null (generic) when the lookup errors AND the legacy path misses", async () => {
    const lookupRoute = vi.fn(async () => {
      throw new Error("idb open blocked");
    });
    const { deps, dsFetchGroup } = baseDeps({
      lookupRoute,
      resolveAccount: vi.fn(async () => null),
    });
    // An unknown push_gid + no legacy match → null, never a throw.
    expect(
      await decryptPushPreview(wake({ groupHex: PUSH_GID_HEX }), deps),
    ).toBeNull();
    expect(dsFetchGroup).not.toHaveBeenCalled();
  });

  it("falls back to the legacy path on a malformed route row", async () => {
    for (const badRoute of [
      { accountId: ACCOUNT_ID, groupIdB64: "!!!not-base64!!!" }, // undecodable
      { accountId: ACCOUNT_ID, groupIdB64: btoa("short") }, // not 32 bytes
      { accountId: ACCOUNT_ID }, // missing groupIdB64
      { groupIdB64: GROUP_B64 }, // missing accountId
    ]) {
      const { deps } = baseDeps({
        lookupRoute: vi.fn(async () => badRoute),
      });
      const out = await decryptPushPreview(wake(), deps);
      // The legacy path (g = a known group id) still produced the preview.
      expect(out?.body).toBe("hello world");
      expect(deps.resolveAccount).toHaveBeenCalledWith(ACCOUNT_ID, GROUP_HEX);
    }
  });

  it("never persists on the route-resolved path even when decrypt fails", async () => {
    const { deps, saveSnapshot, saveMessageLog, advanceCursor } = baseDeps({
      lookupRoute: vi.fn(async () => ({
        accountId: ACCOUNT_ID,
        groupIdB64: GROUP_B64,
      })),
      dsFetchGroup: vi.fn(async () => ({
        messages: [msg(42, {}, { throwOnProcess: true })],
      })),
    });
    expect(
      await decryptPushPreview(wake({ groupHex: PUSH_GID_HEX }), deps),
    ).toBeNull();
    expect(saveSnapshot).not.toHaveBeenCalled();
    expect(saveMessageLog).not.toHaveBeenCalled();
    expect(advanceCursor).not.toHaveBeenCalled();
  });
});

describe("ADR-0042 (amended 2026-09-30) — a wake is handled only for the bound account", () => {
  const BOUND = "dd".repeat(32);
  const OTHER_ACCOUNT = "other-acct-b64";

  describe("wakeBoundAccount — the gate", () => {
    const binding = { devicePseudonymHex: BOUND, accountId: ACCOUNT_ID };

    it("no binding: nothing passes", () => {
      expect(wakeBoundAccount(wake(), null)).toBeNull();
      expect(wakeBoundAccount(wake(), undefined)).toBeNull();
    });

    it("a `d` that is not the bound pseudonym does not pass", () => {
      expect(wakeBoundAccount(wake({ deviceHex: "ee".repeat(32) }), binding)).toBeNull();
      expect(wakeBoundAccount(wake({ deviceHex: null }), binding)).toBeNull();
      expect(wakeBoundAccount(wake({ deviceHex: "dd" }), binding)).toBeNull();
      expect(wakeBoundAccount(wake({ deviceHex: "zz".repeat(32) }), binding)).toBeNull();
    });

    it("the bound `d` passes as the bound account, case-insensitively", () => {
      expect(wakeBoundAccount(wake(), binding)).toBe(ACCOUNT_ID);
      expect(wakeBoundAccount(wake({ deviceHex: BOUND.toUpperCase() }), binding)).toBe(
        ACCOUNT_ID,
      );
      expect(
        wakeBoundAccount(wake(), { devicePseudonymHex: BOUND.toUpperCase(), accountId: ACCOUNT_ID }),
      ).toBe(ACCOUNT_ID);
    });

    it("a malformed binding does not pass", () => {
      expect(wakeBoundAccount(wake(), { devicePseudonymHex: "dd", accountId: ACCOUNT_ID })).toBeNull();
      expect(wakeBoundAccount(wake(), { devicePseudonymHex: BOUND, accountId: "" })).toBeNull();
    });
  });

  describe("decryptPushPreview applies the gate before any read", () => {
    it("no binding dep at all: null, and nothing is read", async () => {
      const { deps } = baseDeps({ readWakeBinding: undefined });
      expect(await decryptPushPreview(wake(), deps)).toBeNull();
      expect(deps.resolveAccount).not.toHaveBeenCalled();
      expect(deps.getDeviceKey).not.toHaveBeenCalled();
    });

    it("no binding (signed out): null, and nothing is read", async () => {
      const { deps, dsFetchGroup } = baseDeps({ readWakeBinding: vi.fn(async () => null) });
      expect(await decryptPushPreview(wake(), deps)).toBeNull();
      expect(deps.resolveAccount).not.toHaveBeenCalled();
      expect(deps.getDeviceKey).not.toHaveBeenCalled();
      expect(dsFetchGroup).not.toHaveBeenCalled();
    });

    it("a binding read that throws reads as no binding", async () => {
      const { deps } = baseDeps({
        readWakeBinding: vi.fn(async () => {
          throw new Error("idb open blocked");
        }),
      });
      expect(await decryptPushPreview(wake(), deps)).toBeNull();
      expect(deps.getDeviceKey).not.toHaveBeenCalled();
    });

    it("wrong `d`: null, and nothing is read", async () => {
      const { deps } = baseDeps();
      expect(await decryptPushPreview(wake({ deviceHex: "ee".repeat(32) }), deps)).toBeNull();
      expect(deps.resolveAccount).not.toHaveBeenCalled();
      expect(deps.getDeviceKey).not.toHaveBeenCalled();
    });

    it("right `d`: decrypts, and every per-account read names the bound account", async () => {
      const { deps } = baseDeps();
      const out = await decryptPushPreview(wake(), deps);
      expect(out?.body).toBe("hello world");
      expect(deps.resolveAccount).toHaveBeenCalledWith(ACCOUNT_ID, GROUP_HEX);
      expect(deps.getDeviceKey).toHaveBeenCalledWith(ACCOUNT_ID);
      expect(deps.getIdentitySeed).toHaveBeenCalledWith(ACCOUNT_ID, expect.anything());
      expect(deps.loadSnapshot).toHaveBeenCalledWith(ACCOUNT_ID, expect.anything());
      expect(deps.startCursor).toHaveBeenCalledWith(ACCOUNT_ID, GROUP_HEX);
    });
  });

  describe("account-scoped route lookup (two accounts in one group)", () => {
    // Both accounts are in the group, so both hold a row for the same
    // push_gid. Only the bound account's row may be used.
    const rows = new Map<string, { accountId: string; groupIdB64: string }>([
      [`${OTHER_ACCOUNT}|${PUSH_GID_HEX}`, { accountId: OTHER_ACCOUNT, groupIdB64: GROUP_B64 }],
      [`${ACCOUNT_ID}|${PUSH_GID_HEX}`, { accountId: ACCOUNT_ID, groupIdB64: GROUP_B64 }],
    ]);
    const lookupRoute = async (accountId: string, gid: string) =>
      rows.get(`${accountId}|${gid}`) ?? null;

    it("decrypts as the bound account, never the other row's", async () => {
      const spy = vi.fn(lookupRoute);
      const { deps } = baseDeps({ lookupRoute: spy });
      const out = await decryptPushPreview(wake({ groupHex: PUSH_GID_HEX }), deps);
      expect(out?.tag).toBe(GROUP_HEX);
      expect(spy).toHaveBeenCalledWith(ACCOUNT_ID, PUSH_GID_HEX);
      expect(deps.getDeviceKey).toHaveBeenCalledWith(ACCOUNT_ID);
      expect(deps.getDeviceKey).not.toHaveBeenCalledWith(OTHER_ACCOUNT);
    });

    it("the other account bound: its own row, its own keys", async () => {
      const spy = vi.fn(lookupRoute);
      const { deps } = baseDeps({
        lookupRoute: spy,
        readWakeBinding: vi.fn(async () => ({ devicePseudonymHex: BOUND, accountId: OTHER_ACCOUNT })),
      });
      await decryptPushPreview(wake({ groupHex: PUSH_GID_HEX }), deps);
      expect(spy).toHaveBeenCalledWith(OTHER_ACCOUNT, PUSH_GID_HEX);
      expect(deps.getDeviceKey).toHaveBeenCalledWith(OTHER_ACCOUNT);
      expect(deps.getDeviceKey).not.toHaveBeenCalledWith(ACCOUNT_ID);
    });

    it("a row naming another account is a miss, even if returned for the bound one", async () => {
      const target = await resolveWakeTarget(PUSH_GID_HEX, ACCOUNT_ID, {
        lookupRoute: async () => ({ accountId: OTHER_ACCOUNT, groupIdB64: GROUP_B64 }),
        resolveAccount: async () => null,
      });
      expect(target).toBeNull();
    });
  });

  describe("the legacy fallback is limited to the bound account", () => {
    it("asks only about the bound account's groups", async () => {
      const resolveAccount = vi.fn(async (_a: string, _h: string) => null);
      const target = await resolveWakeTarget(GROUP_HEX, ACCOUNT_ID, {
        lookupRoute: async () => null,
        resolveAccount,
      });
      expect(target).toBeNull();
      expect(resolveAccount).toHaveBeenCalledTimes(1);
      expect(resolveAccount).toHaveBeenCalledWith(ACCOUNT_ID, GROUP_HEX);
    });

    it("an answer naming another account is refused", async () => {
      const { deps, dsFetchGroup } = baseDeps({
        resolveAccount: vi.fn(async () => ({ accountId: OTHER_ACCOUNT, groupIdBytes: GROUP_BYTES })),
      });
      expect(await decryptPushPreview(wake(), deps)).toBeNull();
      expect(deps.getDeviceKey).not.toHaveBeenCalled();
      expect(dsFetchGroup).not.toHaveBeenCalled();
    });

    it("the bound account's own group resolves", async () => {
      const target = await resolveWakeTarget(GROUP_HEX, ACCOUNT_ID, {
        lookupRoute: async () => null,
        resolveAccount: async (a: string) => ({ accountId: a, groupIdBytes: GROUP_BYTES }),
      });
      expect(target).toEqual({ groupIdBytes: GROUP_BYTES, groupHex: GROUP_HEX });
    });
  });
});
