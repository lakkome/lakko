// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * The account statement as the web client uses it (ADR-0074 section 0.3),
 * through the real wasm core: the `device-revoke` statement a removal sends
 * to auth and, in the header, to discovery.
 */
import { beforeAll, describe, expect, it } from "vitest";
import { readFileSync } from "node:fs";
import { fileURLToPath } from "node:url";

import init, {
  accountStatementHeaderName,
  accountStatementLen,
  signAccountStatement,
  signDeviceRevokeStatement,
  signRecallStartStatement,
  signRecallStatement,
} from "../../wasm/lakko_ffi_wasm";
import { ACCOUNT_STATEMENT_HEADER } from "../discovery/client";

beforeAll(async () => {
  const wasmPath = fileURLToPath(new URL("../../wasm/lakko_ffi_wasm_bg.wasm", import.meta.url));
  await init({ module_or_path: readFileSync(wasmPath) });
});

const SEED = new Uint8Array(32).fill(7);
const ACCOUNT = new Uint8Array(16).fill(3);
const DEVICE = new Uint8Array(32).fill(9);

describe("the device-revoke statement", () => {
  it("is the 138-byte wire form: version 1, kind 3, then the account id", () => {
    const statement = signDeviceRevokeStatement(SEED, ACCOUNT, DEVICE);
    expect(statement).toHaveLength(138);
    expect(accountStatementLen()).toBe(138);
    expect(statement[0]).toBe(1);
    expect(statement[1]).toBe(3);
    expect(Array.from(statement.slice(2, 18))).toEqual(Array.from(ACCOUNT));
  });

  it("is signed anew on every call (a fresh nonce), so it is never stored", () => {
    const a = signDeviceRevokeStatement(SEED, ACCOUNT, DEVICE);
    const b = signDeviceRevokeStatement(SEED, ACCOUNT, DEVICE);
    expect(Array.from(a)).not.toEqual(Array.from(b));
  });

  it("refuses a seed, an account id or a device id of the wrong length", () => {
    expect(() => signDeviceRevokeStatement(new Uint8Array(31), ACCOUNT, DEVICE)).toThrow(/bad-seed-length/);
    expect(() => signDeviceRevokeStatement(SEED, new Uint8Array(15), DEVICE)).toThrow(
      /bad-account-id-length/,
    );
    expect(() => signDeviceRevokeStatement(SEED, ACCOUNT, new Uint8Array(31))).toThrow(
      /bad-device-id-length/,
    );
  });

  it("refuses an unknown kind on the general signer", () => {
    expect(() => signAccountStatement(SEED, 9, ACCOUNT, DEVICE)).toThrow(/unknown-kind/);
  });

  it("travels to discovery under the header the core names", () => {
    expect(accountStatementHeaderName()).toBe(ACCOUNT_STATEMENT_HEADER);
  });
});

// ADR-0074 part 1: the three statements the old device signs with the seed
// kept in its recall record.
describe("the recall statements", () => {
  const LOGIN_REQUEST = btoa("an opaque login request");
  const UPLOAD = btoa("an opaque registration upload");

  it("status is kind 1 over an empty body", () => {
    const statement = signAccountStatement(SEED, 1, ACCOUNT, new Uint8Array(0));
    expect(statement).toHaveLength(accountStatementLen());
    expect(statement[1]).toBe(1);
    expect(Array.from(statement.slice(2, 18))).toEqual(Array.from(ACCOUNT));
  });

  it("start is kind 5 and takes the anchor seq and the login request as sent", () => {
    const statement = signRecallStartStatement(SEED, ACCOUNT, 3n, LOGIN_REQUEST);
    expect(statement).toHaveLength(accountStatementLen());
    expect(statement[1]).toBe(5);
    expect(() => signRecallStartStatement(SEED, ACCOUNT, 3n, "not base64 !")).toThrow(/bad-base64/);
    expect(() => signRecallStartStatement(new Uint8Array(31), ACCOUNT, 3n, LOGIN_REQUEST)).toThrow(
      /bad-seed-length/,
    );
  });

  it("finish is kind 2 over the nonce, the new key and both uploads", () => {
    const nonce = new Uint8Array(32).fill(4);
    const newKey = new Uint8Array(32).fill(5);
    const statement = signRecallStatement(SEED, ACCOUNT, nonce, newKey, UPLOAD, UPLOAD);
    expect(statement).toHaveLength(accountStatementLen());
    expect(statement[1]).toBe(2);
    expect(() => signRecallStatement(SEED, ACCOUNT, new Uint8Array(31), newKey, UPLOAD, UPLOAD)).toThrow(
      /bad-server-nonce-length/,
    );
    expect(() => signRecallStatement(SEED, ACCOUNT, nonce, new Uint8Array(31), UPLOAD, UPLOAD)).toThrow(
      /bad-pubkey-length/,
    );
  });
});
