// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * Opening kept introduction records, against the REAL core: the records and
 * the bundle are composed by `composeIntroduction` /
 * `composeIntroductionsBundle`, the tags come from `introductionTags` and the
 * open from `openIntroduction`. The OPRF outputs are stand-in bytes (the
 * server's evaluation is not needed to exercise the sealing).
 */
import { beforeAll, describe, expect, it } from "vitest";
import { readFileSync } from "node:fs";
import { fileURLToPath } from "node:url";

import initWasm, * as core from "../../wasm/lakko_ffi_wasm";
import { base64Encode } from "../auth/account-id";
import { openMatchingIntroductions, recordsOf, type KeptIntroductions } from "./introductions";

beforeAll(async () => {
  const path = fileURLToPath(new URL("../../wasm/lakko_ffi_wasm_bg.wasm", import.meta.url));
  await initWasm({ module_or_path: readFileSync(path) });
});

const oprf = (fill: number): Uint8Array => new Uint8Array(64).fill(fill);
const IDENTITY = new Uint8Array(32).fill(0x21);
const ACCOUNT = new Uint8Array(16).fill(0x31);
const FOF_SEED = new Uint8Array(32).fill(0x41);

function record(name: string, termOprfs: Uint8Array[]): Uint8Array {
  const grant = new core.ReachGrant(core.ReachClass.Fof, 3, FOF_SEED);
  const payload = new core.IntroductionPayload(IDENTITY, ACCOUNT, name, undefined, undefined, grant);
  try {
    return core.composeIntroduction(payload, termOprfs, 1_700_000_000_000n);
  } finally {
    payload.free();
    grant.free();
  }
}

function rowOf(records: Uint8Array[], offer?: Uint8Array): KeptIntroductions {
  const bundle = core.composeIntroductionsBundle(records, 1_700_000_000_000n);
  return {
    bundles: { cc: { bundleB64: base64Encode(bundle) } },
    offers: offer === undefined ? {} : { dd: { recordB64: base64Encode(offer) } },
  };
}

describe("introductions", () => {
  it("opens the record a probe's tag matches, and reveals the claim and the grant", () => {
    const records = recordsOf(rowOf([record("Anna Weber", [oprf(1), oprf(2)])]), core);
    expect(records.length).toBeGreaterThanOrEqual(1);
    const opened = openMatchingIntroductions(records, [{ oprf: oprf(2), via: "term" }], core);
    expect(opened).toHaveLength(1);
    expect(opened[0]).toMatchObject({ displayName: "Anna Weber", username: null, via: "term" });
    expect(opened[0]!.identityPk).toEqual(IDENTITY);
    expect(opened[0]!.accountId).toEqual(ACCOUNT);
    const grant = core.decodeReachGrant(opened[0]!.fofWire);
    try {
      expect(grant.class).toBe(core.ReachClass.Fof);
      expect(grant.seed).toEqual(FOF_SEED);
    } finally {
      grant.free();
    }
  });

  it("opens nothing for a term the record was not sealed under", () => {
    const records = recordsOf(rowOf([record("Anna Weber", [oprf(1)])]), core);
    expect(openMatchingIntroductions(records, [{ oprf: oprf(9), via: "term" }], core)).toEqual([]);
  });

  it("reads a contact's own offer as well as the relayed bundles", () => {
    const offer = record("Bo Lind", [oprf(7)]);
    const records = recordsOf(rowOf([record("Anna Weber", [oprf(1)])], offer), core);
    const opened = openMatchingIntroductions(records, [{ oprf: oprf(7), via: "username" }], core);
    expect(opened.map((o) => o.displayName)).toEqual(["Bo Lind"]);
    expect(opened[0]!.via).toBe("username");
  });

  it("skips a bundle that does not decode, and a record that does not parse", () => {
    const row: KeptIntroductions = {
      bundles: { cc: { bundleB64: base64Encode(new Uint8Array([1, 2, 3])) } },
      offers: {},
    };
    expect(recordsOf(row, core)).toEqual([]);
    expect(openMatchingIntroductions([new Uint8Array([9, 9])], [{ oprf: oprf(1), via: "term" }], core)).toEqual([]);
  });

  it("drops a record whose tag matched but which does not authenticate: a forgery, never shown", () => {
    const good = record("Anna Weber", [oprf(1)]);
    const tampered = good.slice();
    tampered[tampered.length - 1] = (tampered[tampered.length - 1] ?? 0) ^ 0xff;
    expect(openMatchingIntroductions([tampered], [{ oprf: oprf(1), via: "term" }], core)).toEqual([]);
  });
});
