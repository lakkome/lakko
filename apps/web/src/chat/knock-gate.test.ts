// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * ADR-0053 — the knock gate, against the REAL wasm core.
 *
 * A knock row is an unauthenticated write: anyone who can name the group can
 * put bytes there, including a MEMBER, who can post a real MLS message or
 * proposal. Only the owner and admins read knocks, so if such a row were
 * processed like a journal row it would move those readers alone and fork them
 * from everyone else. `Group.processKnock` refuses anything that is not a
 * stranger's external-join proposal for this group BEFORE processing it; the
 * client routes every knock row through it (`ChatClient.applyKnocks`, the
 * co-publisher poll). This pins the core behaviour the client relies on, with
 * a control proving each refused row WAS processable the ordinary way, so the
 * refusal is the gate and not an unrelated failure.
 */
import { beforeAll, describe, expect, it } from "vitest";
import { readFileSync } from "node:fs";
import { fileURLToPath } from "node:url";

import init, * as wasm from "../../wasm/lakko_ffi_wasm";

beforeAll(async () => {
  const wasmPath = fileURLToPath(
    new URL("../../wasm/lakko_ffi_wasm_bg.wasm", import.meta.url),
  );
  await init({ module_or_path: readFileSync(wasmPath) });
});

/** An owner and a member in one group, both at the same epoch. */
function ownerAndMember(): {
  owner: wasm.Identity;
  member: wasm.Identity;
  ownerGroup: wasm.Group;
  memberGroup: wasm.Group;
} {
  const owner = wasm.Identity.fromSeed(new Uint8Array(64).fill(0x31));
  const member = wasm.Identity.fromSeed(new Uint8Array(64).fill(0x32));
  const ownerGroup = new wasm.Group(owner);
  const added = ownerGroup.addMember(owner, member.createKeyPackage());
  const memberGroup = wasm.Group.joinFromWelcome(added.welcome, member);
  return { owner, member, ownerGroup, memberGroup };
}

describe("ADR-0053 — processKnock refuses anything but a stranger's external-join proposal", () => {
  it("a member's APPLICATION MESSAGE posted as a knock is refused and never applied", () => {
    const { owner, member, ownerGroup, memberGroup } = ownerAndMember();
    const epochBefore = ownerGroup.epoch;
    const message = memberGroup.createApplicationMessage(
      member,
      new TextEncoder().encode("posted to the knock queue"),
    );

    expect(() => ownerGroup.processKnock(owner, message)).toThrow();
    expect(ownerGroup.epoch).toBe(epochBefore);

    // Control: the same bytes are an ordinary, processable journal row — so
    // the refusal above is the knock gate, and the row was not consumed by it
    // (a message the gate had decrypted could not be decrypted again).
    const processed = ownerGroup.processIncomingVerifyingRoles(owner, message);
    expect(processed.kind).toBe("Application");
  });

  it("a member's membership PROPOSAL posted as a knock is refused and never stored", () => {
    const { owner, member, ownerGroup, memberGroup } = ownerAndMember();
    const pendingBefore = ownerGroup.pendingProposals().length;
    // A member proposing its OWN departure needs no role, so the core builds
    // it for a plain member: a real, authorised proposal from a member.
    const proposal = memberGroup.proposeRemoveMember(member, memberGroup.ownLeafIndex);

    expect(() => ownerGroup.processKnock(owner, proposal)).toThrow();
    expect(ownerGroup.pendingProposals().length).toBe(pendingBefore);

    // Control: through the journal path the same proposal IS stored.
    ownerGroup.processIncomingVerifyingRoles(owner, proposal);
    expect(ownerGroup.pendingProposals().length).toBe(pendingBefore + 1);
  });

  it("a stranger's external-join proposal for THIS group passes, like the journal path would", () => {
    const { owner, ownerGroup } = ownerAndMember();
    const stranger = wasm.Identity.fromSeed(new Uint8Array(64).fill(0x34));
    const proposal = stranger.createExternalJoinProposal(ownerGroup.groupId, ownerGroup.epoch);

    const processed = ownerGroup.processKnock(owner, proposal);

    expect(processed.kind).toBe("ExternalJoinProposal");
    expect(Array.from(processed.externalJoinSignatureKey ?? [])).toEqual(
      Array.from(stranger.signaturePublicKey),
    );
  });

  it("a stranger's external-join proposal for ANOTHER group is refused", () => {
    const { owner, ownerGroup } = ownerAndMember();
    const other = new wasm.Group(wasm.Identity.fromSeed(new Uint8Array(64).fill(0x35)));
    const stranger = wasm.Identity.fromSeed(new Uint8Array(64).fill(0x36));
    const proposal = stranger.createExternalJoinProposal(other.groupId, other.epoch);

    expect(() => ownerGroup.processKnock(owner, proposal)).toThrow();
  });
});
