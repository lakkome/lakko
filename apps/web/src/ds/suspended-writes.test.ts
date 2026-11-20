// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * ADR-0098 step 3: while this device is in suspended mode the DS client
 * sends nothing (the Delivery Service does not know accounts, so the client
 * honours the suspension). A write is refused before it leaves, as a
 * transport failure the loops back off from; a read goes out as before.
 */
import { afterEach, describe, expect, it, vi } from "vitest";

import { DsAccessRegistry } from "./access";
import { fakeSigner } from "./access-test-doubles";
import { DsClient, DsNetworkError } from "./client";
import { setAccountSuspendedFlag } from "../auth/suspension-state";

const GID = new Uint8Array(32).fill(0xab);

function harness(): { ds: DsClient; fetchImpl: ReturnType<typeof vi.fn> } {
  const reg = new DsAccessRegistry();
  reg.registerGroupSource(() => fakeSigner(4n));
  const fetchImpl = vi.fn(
    async () =>
      new Response(JSON.stringify({ id: 9, created_at: "t", messages: [], has_more: false }), {
        status: 200,
      }),
  );
  return { ds: new DsClient("/api/ds", { access: reg, fetchImpl: fetchImpl as unknown as typeof fetch }), fetchImpl };
}

afterEach(() => setAccountSuspendedFlag(false));

describe("the DS client in suspended mode", () => {
  it("refuses a publish without sending it", async () => {
    setAccountSuspendedFlag(true);
    const { ds, fetchImpl } = harness();
    await expect(ds.publishGroup(GID, new Uint8Array([1, 2, 3]))).rejects.toBeInstanceOf(DsNetworkError);
    expect(fetchImpl).not.toHaveBeenCalled();
  });

  it("still reads the journal", async () => {
    setAccountSuspendedFlag(true);
    const { ds, fetchImpl } = harness();
    await ds.fetchGroup(GID, { after: 0 });
    expect(fetchImpl).toHaveBeenCalledTimes(1);
  });

  it("publishes again once suspended mode ended", async () => {
    setAccountSuspendedFlag(false);
    const { ds, fetchImpl } = harness();
    await ds.publishGroup(GID, new Uint8Array([1, 2, 3]));
    expect(fetchImpl).toHaveBeenCalledTimes(1);
  });
});
