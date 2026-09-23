// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import { describe, expect, it } from "vitest";

import {
  GROUP_ONLY_SECTIONS,
  channelInfoSections,
  findPersonRecord,
  groupInfoSections,
  infoHeading,
  infoOpenLabel,
  infoTargetForKind,
  personInfoSections,
  type GroupInfoCapabilities,
  type InfoSection,
} from "./conversation-info";

/** Whether `sections` holds anything that only makes sense for a group. */
function holdsGroupOnlySection(sections: readonly InfoSection[]): boolean {
  return sections.some((s) => GROUP_ONLY_SECTIONS.includes(s));
}

/** Every combination of the five group capability flags. */
function everyGroupCapability(): GroupInfoCapabilities[] {
  const out: GroupInfoCapabilities[] = [];
  for (let bits = 0; bits < 32; bits += 1) {
    out.push({
      canManageMembers: (bits & 1) !== 0,
      canRename: (bits & 2) !== 0,
      canMintInvite: (bits & 4) !== 0,
      isOwner: (bits & 8) !== 0,
      membershipEnded: (bits & 16) !== 0,
    });
  }
  return out;
}

describe("infoTargetForKind", () => {
  it("a one-to-one opens the person", () => {
    expect(infoTargetForKind("chat")).toBe("person");
  });

  it("a group opens the group", () => {
    expect(infoTargetForKind("group")).toBe("group");
  });
});

describe("infoOpenLabel", () => {
  it("names what it opens, per target", () => {
    expect(infoOpenLabel("person")).toBe("Contact info");
    expect(infoOpenLabel("group")).toBe("Group info");
    expect(infoOpenLabel("channel")).toBe("Channel info");
  });

  it("the layer's heading is the same words as the button", () => {
    for (const target of ["person", "group", "channel"] as const) {
      expect(infoHeading(target)).toBe(infoOpenLabel(target));
    }
  });

  it("says nothing in protocol terms", () => {
    const banned = ["MLS", "OPRF", "KeyPackage", "pseudonym", "epoch", "capability"];
    for (const target of ["person", "group", "channel"] as const) {
      for (const word of banned) expect(infoOpenLabel(target)).not.toContain(word);
    }
  });
});

describe("personInfoSections", () => {
  it("holds the identity, the actions and the safety number", () => {
    const sections = personInfoSections({ isContact: true });
    expect(sections).toContain("person-identity");
    expect(sections).toContain("person-actions");
    expect(sections).toContain("safety-number");
  });

  it("offers the contact-list blocks only for someone in the contact list", () => {
    expect(personInfoSections({ isContact: true })).toContain("remove-contact");
    expect(personInfoSections({ isContact: true })).toContain("contact-name");
    expect(personInfoSections({ isContact: false })).not.toContain("remove-contact");
    expect(personInfoSections({ isContact: false })).not.toContain("contact-name");
  });

  // The defect of 2026-09-23: tapping a person's name showed `Group name`,
  // `Create invite link`, `Make admin`, `Transfer ownership` and `Leave
  // group`. No capability combination may bring any of them back.
  it("never shows anything that only makes sense for a group", () => {
    for (const isContact of [true, false]) {
      const sections = personInfoSections({ isContact });
      expect(holdsGroupOnlySection(sections)).toBe(false);
      for (const groupOnly of GROUP_ONLY_SECTIONS) {
        expect(sections).not.toContain(groupOnly);
      }
    }
  });
});

describe("groupInfoSections", () => {
  const full: GroupInfoCapabilities = {
    canManageMembers: true,
    canRename: true,
    canMintInvite: true,
    isOwner: true,
    membershipEnded: false,
  };

  it("keeps every capability the old panel had", () => {
    const sections = groupInfoSections(full);
    const kept: InfoSection[] = [
      "group-name",
      "rename-group",
      "members",
      "add-member",
      "invite-link",
      "transfer-ownership",
      "leave-group",
    ];
    for (const section of kept) expect(sections).toContain(section);
  });

  it("a plain member reads the name and the roster and acts on neither", () => {
    const sections = groupInfoSections({
      canManageMembers: false,
      canRename: false,
      canMintInvite: false,
      isOwner: false,
      membershipEnded: false,
    });
    expect(sections).toContain("group-name");
    expect(sections).not.toContain("rename-group");
    expect(sections).toContain("members");
    expect(sections).not.toContain("add-member");
    expect(sections).not.toContain("invite-link");
    expect(sections).not.toContain("transfer-ownership");
    // A member who may do nothing else may still leave.
    expect(sections).toContain("leave-group");
  });

  it("only the owner is offered the transfer", () => {
    expect(groupInfoSections({ ...full, isOwner: false })).not.toContain(
      "transfer-ownership",
    );
  });

  // Spec section 4: the conversation is over, the record of who was in it
  // is not. What is left is the one thing the reader still can do - close
  // the door behind them (ADR-0045 UX 6.2).
  it("an ended membership is readable and offers nothing but the door", () => {
    for (const caps of everyGroupCapability()) {
      if (!caps.membershipEnded) continue;
      const sections = groupInfoSections(caps);
      expect(sections).toEqual(["group-name", "members", "delete-chat"]);
    }
  });

  it("an ended membership can never be talked into renaming or inviting", () => {
    for (const caps of everyGroupCapability()) {
      if (!caps.membershipEnded) continue;
      const sections = groupInfoSections(caps);
      for (const gone of ["rename-group", "add-member", "invite-link", "leave-group"] as const) {
        expect(sections).not.toContain(gone);
      }
    }
  });

  it("always lists the members", () => {
    for (const caps of everyGroupCapability()) {
      expect(groupInfoSections(caps)).toContain("members");
    }
  });
});

describe("channelInfoSections", () => {
  it("always identifies the channel", () => {
    expect(channelInfoSections({ isPublisher: false, isSubscriber: false })).toEqual([
      "channel-identity",
    ]);
  });

  it("a publisher reaches the screen it already has", () => {
    expect(channelInfoSections({ isPublisher: true, isSubscriber: false })).toContain(
      "manage-channel",
    );
  });

  it("a subscriber can stop following", () => {
    expect(channelInfoSections({ isPublisher: false, isSubscriber: true })).toContain(
      "unsubscribe",
    );
  });

  it("a publisher is never offered an unsubscribe that would end nothing", () => {
    expect(channelInfoSections({ isPublisher: true, isSubscriber: true })).not.toContain(
      "unsubscribe",
    );
  });

  it("never shows anything that only makes sense for a group", () => {
    for (const isPublisher of [true, false]) {
      for (const isSubscriber of [true, false]) {
        expect(
          holdsGroupOnlySection(channelInfoSections({ isPublisher, isSubscriber })),
        ).toBe(false);
      }
    }
  });
});

describe("findPersonRecord", () => {
  const linked = { signaturePubkeyB64: "keyA", groupIdB64: "g1" };
  const byKey = { signaturePubkeyB64: "keyB" };

  it("prefers the chat's own contact link", () => {
    expect(
      findPersonRecord([byKey, linked], { groupIdB64: "g1", peerKeyB64: "keyB" }),
    ).toBe(linked);
  });

  it("falls back to the peer's key when no link was ever written", () => {
    expect(
      findPersonRecord([byKey, linked], { groupIdB64: "g9", peerKeyB64: "keyB" }),
    ).toBe(byKey);
  });

  it("is null when the other person is not in the contact list", () => {
    expect(
      findPersonRecord([byKey], { groupIdB64: "g9", peerKeyB64: "keyZ" }),
    ).toBeNull();
    expect(findPersonRecord([byKey], { groupIdB64: "g9", peerKeyB64: null })).toBeNull();
  });

  it("an unknown person still opens a person, so the sections stay a person's", () => {
    const record = findPersonRecord([], { groupIdB64: "g9", peerKeyB64: "keyZ" });
    const sections = personInfoSections({ isContact: record !== null });
    expect(holdsGroupOnlySection(sections)).toBe(false);
    expect(sections).toContain("safety-number");
  });
});
