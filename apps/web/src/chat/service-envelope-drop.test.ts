// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * A `lakko/…` service envelope is never rendered, in the FOREGROUND receive
 * path (`toEvent` → `planBackgroundEvents`).
 *
 * The live walk of 2026-09-29: a client on another branch sent a profile
 * envelope v3 into shared groups, and the main clients rendered it as a chat
 * bubble reading `lakko/profile/v1` plus binary. The core now classifies any
 * non-JSON plaintext starting with `lakko/` as `service-envelope` instead of
 * degrading it to lossy-UTF-8 chat text, and the client drops that kind.
 *
 * The wasm is the REAL core (`parseBodyEnvelope`, `decodeProfileEnvelope`, the
 * other codecs `toEvent` sniffs with); only the processed MLS frame is a
 * literal, since the path under test starts after decryption.
 *
 * Discrimination (run): with the `service-envelope` arm removed from
 * `parseBodyEnvelope` in `attachments.ts`, three tests fail — the wrapper
 * returns `'chat'`, "an unknown service envelope is not an application row"
 * reads `expected 'application' to be 'serviceEnvelope'`, and the plan puts an
 * application row into the timeline.
 */
import { beforeAll, describe, expect, it, vi } from "vitest";
import { readFileSync } from "node:fs";
import { fileURLToPath } from "node:url";

import init, * as wasm from "../../wasm/lakko_ffi_wasm";

vi.mock("../wasm", () => ({ loadWasm: async () => wasm, wasmIfLoaded: () => wasm }));

import { parseBodyEnvelope } from "./attachments";
import { planBackgroundEvents } from "./background-events";
import { toEvent } from "./client";

beforeAll(async () => {
  const wasmPath = fileURLToPath(new URL("../../wasm/lakko_ffi_wasm_bg.wasm", import.meta.url));
  await init({ module_or_path: readFileSync(wasmPath) });
});

const enc = new TextEncoder();

/** `"lakko/profile/v1\0" ‖ 0x03 ‖ …` — the shape the other branch sent. */
function v3Profile(): Uint8Array {
  const domain = enc.encode("lakko/profile/v1\0");
  const out = new Uint8Array(domain.length + 1 + 40);
  out.set(domain, 0);
  out[domain.length] = 0x03;
  for (let i = domain.length + 1; i < out.length; i++) out[i] = (i * 37) & 0xff;
  // A readable name in the body, so a rendered bubble would show it.
  out.set(enc.encode("Zed"), domain.length + 12);
  return out;
}

/** A service envelope of a kind this build has never heard of. */
function unknownKind(): Uint8Array {
  const domain = enc.encode("lakko/future-kind/v1\0");
  const out = new Uint8Array(domain.length + 6);
  out.set(domain, 0);
  out.set([0x01, 0x00, 0xff, 0x10, 0x20, 0x30], domain.length);
  return out;
}

function processed(bytes: Uint8Array) {
  return { kind: "Application", applicationData: bytes, senderLeaf: 1 } as never;
}

const at = "2026-09-29T10:00:00.000Z";
const resolve = (): string => "c2VuZGVy";

describe("a service envelope never becomes a chat row", () => {
  it("the core classifies both shapes as a service envelope, not chat", async () => {
    expect((await parseBodyEnvelope(v3Profile())).kind).toBe("service-envelope");
    expect((await parseBodyEnvelope(unknownKind())).kind).toBe("service-envelope");
  });

  it("a v3 profile is not an application row", async () => {
    const e = await toEvent(7, at, processed(v3Profile()), resolve);
    expect(e.kind).not.toBe("application");
    // The profile sniffer claims it first (domain matched, unknown version)
    // and drops it: nothing is filed, nothing is rendered.
    if (e.kind === "profile") expect(e.profile).toBeUndefined();
    else expect(e.kind).toBe("serviceEnvelope");
  });

  it("an unknown service envelope is not an application row", async () => {
    const e = await toEvent(8, at, processed(unknownKind()), resolve);
    expect(e.kind).toBe("serviceEnvelope");
  });

  it("neither reaches the timeline or the chat view", async () => {
    const events = [
      await toEvent(7, at, processed(v3Profile()), resolve),
      await toEvent(8, at, processed(unknownKind()), resolve),
    ];
    const plan = planBackgroundEvents(events);
    expect(plan.timeline).toEqual([]);
    expect(plan.toView).toEqual([]);
    expect(plan.profiles.every((p) => p.profile === undefined)).toBe(true);
  });

  it("an ordinary message starting with `lakko` still renders", async () => {
    const e = await toEvent(9, at, processed(enc.encode("lakko is great")), resolve);
    expect(e.kind).toBe("application");
    if (e.kind === "application") expect(e.text).toBe("lakko is great");
  });
});
