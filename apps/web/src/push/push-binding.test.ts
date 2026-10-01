// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * ADR-0057 obligation 8 — when the owner or an admin mints or rotates the
 * push routing secret, the group's journal is bound to its `push_gid`
 * (`bindJournalToPushGid`) BEFORE the secret is broadcast (the second
 * security pass, finding 5; `mintAndBindSecret` in `group-routing.ts`). The
 * routing id is derived by the REAL core; the DS is a recorder.
 */
import { afterEach, beforeAll, describe, expect, it, vi } from "vitest";
import { readFileSync } from "node:fs";
import { fileURLToPath } from "node:url";

import init, * as wasm from "../../wasm/lakko_ffi_wasm";
import { bindJournalToPushGid, mintAndBindSecret } from "./group-routing";
import { dsAccess } from "../ds/access";
import {
  fakeRegistrarAuthority,
  fakeRootAuthority,
  parseFakeStatement,
} from "../ds/access-test-doubles";
import type { StatementSender } from "../ds/ownership";

beforeAll(async () => {
  const wasmPath = fileURLToPath(new URL("../../wasm/lakko_ffi_wasm_bg.wasm", import.meta.url));
  await init({ module_or_path: readFileSync(wasmPath) });
});

afterEach(() => {
  dsAccess.setAuthorityFactory(null);
});

const GID = new Uint8Array(32).fill(0x51);
const GID_B64 = Buffer.from(GID).toString("base64");
const SECRET = new Uint8Array(32).fill(0x07);

function recorder(status = 204): StatementSender & { puts: Array<{ path: string; body: string }> } {
  const puts: Array<{ path: string; body: string }> = [];
  return {
    puts,
    putStatement: vi.fn(async (path: string, body: string) => {
      puts.push({ path, body });
      if (status >= 400) throw Object.assign(new Error(`HTTP ${status}`), { status });
    }),
  };
}

describe("ADR-0057 obligation 8 — push binding", () => {
  it("the owner binds the journal to derivePushGid(secret) with its root", async () => {
    dsAccess.setAuthorityFactory({ root: () => fakeRootAuthority("root"), registrar: () => ({ kind: "none" }) });
    const off = dsAccess.registerOwnerSource(() => true);
    const s = recorder();
    expect(await bindJournalToPushGid(GID_B64, SECRET, s)).toBe("bound");
    off();
    expect(s.puts[0]?.path).toBe(`/ds/group/${"51".repeat(32)}/push-binding`);
    const st = parseFakeStatement(s.puts[0]?.body ?? "");
    expect(st.signer).toBe("root");
    const expected = Buffer.from(wasm.derivePushGid(SECRET)).toString("hex");
    expect(st.pushGid).toBe(expected);
  });

  it("an admin registrar binds too", async () => {
    dsAccess.setAuthorityFactory({ root: () => ({ kind: "none" }), registrar: () => fakeRegistrarAuthority("adm", 2) });
    const s = recorder();
    expect(await bindJournalToPushGid(GID_B64, SECRET, s)).toBe("bound");
    expect(parseFakeStatement(s.puts[0]?.body ?? "").signer).toBe("adm");
  });

  it("a member does not bind, and sends nothing", async () => {
    dsAccess.setAuthorityFactory({ root: () => ({ kind: "none" }), registrar: () => fakeRegistrarAuthority("m", 1) });
    const s = recorder();
    expect(await bindJournalToPushGid(GID_B64, SECRET, s)).toBe("not-permitted");
    expect(s.puts).toHaveLength(0);
  });

  it("409 (bound to another journal) and 503 (disabled) are logged and the caller continues", async () => {
    const warn = vi.spyOn(console, "warn").mockImplementation(() => {});
    dsAccess.setAuthorityFactory({ root: () => fakeRootAuthority(), registrar: () => ({ kind: "none" }) });
    const off = dsAccess.registerOwnerSource(() => true);
    expect(await bindJournalToPushGid(GID_B64, SECRET, recorder(409))).toBe("conflict");
    expect(await bindJournalToPushGid(GID_B64, SECRET, recorder(503))).toBe("disabled");
    off();
    warn.mockRestore();
  });
});

describe("finding 5 — mint, then bind, and only then may the caller broadcast", () => {
  it("204: the first secret is kept", async () => {
    let n = 0;
    const mint = (): Uint8Array => new Uint8Array(32).fill(++n);
    const bind = vi.fn(async () => "bound" as const);
    const secret = await mintAndBindSecret(GID_B64, { mint, bind });
    expect(secret).toEqual(new Uint8Array(32).fill(1));
    expect(bind).toHaveBeenCalledTimes(1);
  });

  it("409: exactly one fresh secret is minted and bound; a second 409 is logged and that secret kept", async () => {
    const warn = vi.spyOn(console, "warn").mockImplementation(() => {});
    let n = 0;
    const mint = (): Uint8Array => new Uint8Array(32).fill(++n);
    const bound: number[] = [];
    const bind = vi.fn(async (_g: string, secret: Uint8Array) => {
      bound.push(secret[0] ?? 0);
      return "conflict" as const;
    });
    const secret = await mintAndBindSecret(GID_B64, { mint, bind });
    expect(bound).toEqual([1, 2]);
    expect(secret).toEqual(new Uint8Array(32).fill(2));
    warn.mockRestore();
  });

  it("503 or a failure: the secret is kept (broadcast anyway), no re-mint", async () => {
    for (const outcome of ["disabled", "failed", "not-permitted"] as const) {
      let n = 0;
      const bind = vi.fn(async () => outcome);
      const secret = await mintAndBindSecret(GID_B64, { mint: () => new Uint8Array(32).fill(++n), bind });
      expect(secret[0]).toBe(1);
      expect(bind).toHaveBeenCalledTimes(1);
    }
  });
});
