// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * ADR-0042 — `ChatClient.sendMessage`'s notify wake routes under the opaque
 * `push_gid`, NOT the DS `group_id`.
 *
 * Pins:
 *   - when the `pushGidResolver` yields a `push_gid`, `notifyGroup` is called
 *     with THAT value (never the raw group_id);
 *   - when the resolver yields `null` (this device holds no secret yet), the
 *     wake is SKIPPED — `notifyGroup` is NOT called (invariant #5: never leak
 *     the group_id to push-gw).
 *
 * The group / DS / push-gw are mocked so the test needs no wasm.
 */
import { describe, expect, it, vi } from "vitest";
import { ChatClient } from "./client";
import type { DsClient } from "../ds/client";
import type { PushClient } from "../push/client";
import type {
  Group as WasmGroup,
  Identity as WasmIdentity,
} from "../../wasm/lakko_ffi_wasm";

const GROUP_ID = new Uint8Array([9, 9, 9, 9]);
const PUSH_GID = new Uint8Array(32).fill(0xab);

function mockGroup(): WasmGroup {
  return {
    createApplicationMessage: (_id: unknown, _pt: Uint8Array) =>
      new Uint8Array([1, 2, 3]),
    get groupId(): Uint8Array {
      return GROUP_ID;
    },
  } as unknown as WasmGroup;
}

function mockDs(): { ds: DsClient; publishGroup: ReturnType<typeof vi.fn> } {
  const publishGroup = vi.fn(async () => ({
    id: 7,
    createdAt: "2026-08-06T00:00:00Z",
  }));
  return { ds: { publishGroup } as unknown as DsClient, publishGroup };
}

const IDENTITY_TOKEN = {} as unknown as WasmIdentity;

/** Let the fire-and-forget notify microtasks run. */
async function flush(): Promise<void> {
  await new Promise((r) => setTimeout(r, 0));
}

describe("ChatClient notify — routes under push_gid (ADR-0042)", () => {
  it("notifies under push_gid when the resolver yields one (not group_id)", async () => {
    const { ds } = mockDs();
    const notifyGroup = vi.fn(
      (_group: Uint8Array, _pointer?: number): Promise<{ devicesWoken: number }> =>
        Promise.resolve({ devicesWoken: 1 }),
    );
    const push = { notifyGroup } as unknown as PushClient;

    const client = new ChatClient(
      ds,
      {
        identityProvider: () => IDENTITY_TOKEN,
        onPersist: async () => {},
        pushGidResolver: async () => PUSH_GID,
      },
      push,
    );

    await client.sendMessage(null, mockGroup(), new Uint8Array([7]));
    await flush();

    expect(notifyGroup).toHaveBeenCalledTimes(1);
    const target = notifyGroup.mock.calls[0]![0];
    expect(Array.from(target)).toEqual(Array.from(PUSH_GID));
    // Explicitly NOT the DS group_id.
    expect(Array.from(target)).not.toEqual(Array.from(GROUP_ID));
  });

  it("SKIPS the wake when the resolver yields null (no secret held yet)", async () => {
    const { ds } = mockDs();
    const notifyGroup = vi.fn(
      (_group: Uint8Array, _pointer?: number): Promise<{ devicesWoken: number }> =>
        Promise.resolve({ devicesWoken: 0 }),
    );
    const push = { notifyGroup } as unknown as PushClient;

    const client = new ChatClient(
      ds,
      {
        identityProvider: () => IDENTITY_TOKEN,
        onPersist: async () => {},
        pushGidResolver: async () => null,
      },
      push,
    );

    await client.sendMessage(null, mockGroup(), new Uint8Array([7]));
    await flush();

    // Never notify under the leaky group_id.
    expect(notifyGroup).not.toHaveBeenCalled();
  });
});
