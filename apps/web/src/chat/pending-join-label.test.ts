// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * The pending join request's label resolves through the joiner's ACCOUNT key
 * (ADR-0050 device binding, `Group.pendingExternalJoinAccountKey`), because a
 * request is keyed on the device leaf and a contact on the account. Found in
 * the ADR-0053 live walk: a contact showed as "Member".
 */
import { describe, expect, it } from "vitest";

import { pendingJoinLabel } from "./pending-join-label";
import type { ResolvedName } from "../profile/resolver";

const LEAF = new Uint8Array(32).fill(0x11);
const ACCOUNT = new Uint8Array(32).fill(0x22);
const b64 = (b: Uint8Array): string => `k${b[0]}`;

function named(name: string): ResolvedName {
  return { name, source: "rename", username: null, listLabel: name, theirName: null, fingerprint: "a1b2 c3d4", usernameNotVerified: false } as unknown as ResolvedName;
}
const unnamed: ResolvedName = {
  name: "a1b2 c3d4",
  source: "fingerprint",
  username: null,
  listLabel: "a1b2 c3d4",
  theirName: null,
} as unknown as ResolvedName;

describe("pendingJoinLabel", () => {
  it("names a contact through the account key behind the request", () => {
    const label = pendingJoinLabel(
      { pendingExternalJoinAccountKey: () => ACCOUNT },
      LEAF,
      b64,
      (k) => (k === "k34" ? named("Anna") : unnamed),
    );
    expect(label).toBe("Anna");
  });

  it("a non-contact keeps the existing label", () => {
    const label = pendingJoinLabel(
      { pendingExternalJoinAccountKey: () => ACCOUNT },
      LEAF,
      b64,
      () => unnamed,
    );
    expect(label).toBe("Member");
  });

  it("no account key (or a failed read) falls back to the leaf key", () => {
    const resolve = (k: string): ResolvedName => (k === "k17" ? named("Leaf name") : unnamed);
    expect(pendingJoinLabel({ pendingExternalJoinAccountKey: () => undefined }, LEAF, b64, resolve)).toBe(
      "Leaf name",
    );
    expect(
      pendingJoinLabel(
        {
          pendingExternalJoinAccountKey: () => {
            throw new Error("torn");
          },
        },
        LEAF,
        b64,
        resolve,
      ),
    ).toBe("Leaf name");
  });
});
