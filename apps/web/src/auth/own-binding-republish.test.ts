// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * The own bundle's X25519 binding is re-published once when the directory
 * row predates it (security review 2026-11-21).
 */
import { describe, expect, it } from "vitest";
import {
  clearBindingConfirmed,
  ownBindingCheck,
  readBindingConfirmed,
  republishBindingIfMissing,
  writeBindingConfirmed,
  type BindingRepublishDeps,
} from "./own-binding-republish";
import type { BindingFields } from "../discovery/x25519-binding";
import type { MarkerStorage } from "./phrase-gate";

function memory(): MarkerStorage & { readonly map: Map<string, string> } {
  const map = new Map<string, string>();
  return {
    map,
    getItem: (k) => map.get(k) ?? null,
    setItem: (k, v) => {
      map.set(k, v);
    },
    removeItem: (k) => {
      map.delete(k);
    },
  };
}

const b64 = (bytes: Uint8Array): string => btoa(String.fromCharCode(...bytes));

const OWN = {
  signaturePublicKey: new Uint8Array(32).fill(1),
  x25519PublicKey: new Uint8Array(32).fill(2),
};
const OWN_KEY_B64 = b64(OWN.signaturePublicKey);
const GOOD_BINDING = new Uint8Array(64).fill(7);
const ACCT = "QUNDVA==";

/** A verifier that accepts exactly GOOD_BINDING over OWN's keys. */
const verify = (binding: Uint8Array, e: Uint8Array, x: Uint8Array): boolean =>
  binding.every((b) => b === 7) &&
  binding.length === 64 &&
  e.every((b) => b === 1) &&
  x.every((b) => b === 2);

function row(opts: { binding?: Uint8Array | null; sig?: Uint8Array; x?: Uint8Array } = {}): BindingFields {
  const out: BindingFields = {
    signature_pubkey: b64(opts.sig ?? OWN.signaturePublicKey),
    x25519_pubkey: b64(opts.x ?? OWN.x25519PublicKey),
  };
  if (opts.binding !== undefined && opts.binding !== null) {
    return { ...out, x25519_binding: b64(opts.binding) };
  }
  return out;
}

function deps(
  storage: MarkerStorage,
  r: BindingFields | null,
  upload: () => Promise<void>,
  warn: (line: string) => void = () => undefined,
): BindingRepublishDeps {
  return { storage, accountIdB64: ACCT, own: OWN, row: r, verify, upload, warn };
}

describe("ownBindingCheck", () => {
  it("confirms a row of this device's keys with a binding that verifies", () => {
    expect(ownBindingCheck(row({ binding: GOOD_BINDING }), OWN, verify)).toBe("confirmed");
  });

  it("asks for a re-publish when the binding is absent or does not verify", () => {
    expect(ownBindingCheck(row(), OWN, verify)).toBe("republish");
    expect(ownBindingCheck(row({ binding: new Uint8Array(64).fill(9) }), OWN, verify)).toBe(
      "republish",
    );
    expect(ownBindingCheck({ ...row(), x25519_binding: "" }, OWN, verify)).toBe("republish");
  });

  it("leaves a missing row or a row with other keys alone", () => {
    expect(ownBindingCheck(null, OWN, verify)).toBe("not-own");
    expect(ownBindingCheck(row({ sig: new Uint8Array(32).fill(3) }), OWN, verify)).toBe("not-own");
    expect(ownBindingCheck(row({ x: new Uint8Array(32).fill(3) }), OWN, verify)).toBe("not-own");
  });
});

describe("republishBindingIfMissing", () => {
  it("uploads the same keys once when the binding is missing, then marks it", async () => {
    const s = memory();
    let uploads = 0;
    const outcome = await republishBindingIfMissing(
      deps(s, row(), async () => {
        uploads += 1;
      }),
    );
    expect(outcome).toBe("republished");
    expect(uploads).toBe(1);
    expect(readBindingConfirmed(s, ACCT)).toBe(OWN_KEY_B64);
    // The next landing reads the mark and checks nothing.
    const again = await republishBindingIfMissing(
      deps(s, row(), async () => {
        uploads += 1;
      }),
    );
    expect(again).toBe("already-confirmed");
    expect(uploads).toBe(1);
  });

  it("uploads nothing when the row already carries a valid binding, and marks it", async () => {
    const s = memory();
    let uploads = 0;
    const outcome = await republishBindingIfMissing(
      deps(s, row({ binding: GOOD_BINDING }), async () => {
        uploads += 1;
      }),
    );
    expect(outcome).toBe("confirmed");
    expect(uploads).toBe(0);
    expect(readBindingConfirmed(s, ACCT)).toBe(OWN_KEY_B64);
  });

  it("uploads nothing and marks nothing when the row holds other keys or is absent", async () => {
    const s = memory();
    let uploads = 0;
    const up = async (): Promise<void> => {
      uploads += 1;
    };
    expect(
      await republishBindingIfMissing(deps(s, row({ sig: new Uint8Array(32).fill(3) }), up)),
    ).toBe("not-own");
    expect(await republishBindingIfMissing(deps(s, null, up))).toBe("not-own");
    expect(uploads).toBe(0);
    expect(readBindingConfirmed(s, ACCT)).toBeNull();
  });

  it("leaves the mark unset on a failed upload, logs one line, and tries again next time", async () => {
    const s = memory();
    const lines: string[] = [];
    const outcome = await republishBindingIfMissing(
      deps(
        s,
        row(),
        () => Promise.reject(new TypeError("network down for QUNDVA==")),
        (l) => lines.push(l),
      ),
    );
    expect(outcome).toBe("failed");
    expect(readBindingConfirmed(s, ACCT)).toBeNull();
    expect(lines).toEqual(["identity binding re-publish failed (TypeError)"]);
    expect(lines.join("")).not.toContain(ACCT);
    let uploads = 0;
    const retry = await republishBindingIfMissing(
      deps(s, row(), async () => {
        uploads += 1;
      }),
    );
    expect(retry).toBe("republished");
    expect(uploads).toBe(1);
  });

  it("checks again after a key reset (the mark names the old key)", async () => {
    const s = memory();
    writeBindingConfirmed(s, ACCT, b64(new Uint8Array(32).fill(9)));
    let uploads = 0;
    const outcome = await republishBindingIfMissing(
      deps(s, row(), async () => {
        uploads += 1;
      }),
    );
    expect(outcome).toBe("republished");
    expect(uploads).toBe(1);
  });

  it("keeps the mark per account and drops it with the wipe", () => {
    const s = memory();
    writeBindingConfirmed(s, ACCT, OWN_KEY_B64);
    expect(readBindingConfirmed(s, "QkJCQg==")).toBeNull();
    clearBindingConfirmed(s, ACCT);
    expect(readBindingConfirmed(s, ACCT)).toBeNull();
  });
});
