// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * `docs/ux/conversation-info.md` — what a conversation's name opens, checked
 * across the REAL wasm boundary.
 *
 * ## Why the real core and not a fake
 *
 * The rule itself moved to `crates/lakko-ui-rules/src/info.rs` and is asserted
 * there. Restating those assertions against a hand-written fake would pin the
 * web's GUESS about a rule it no longer owns — the exact shape that let three
 * clients disagree in the first place. So these drive the core through the
 * generated bindings, which makes each one a statement about the BOUNDARY:
 * that the decision the core made is the decision this client receives, under
 * the names this client branches on.
 *
 * What is still the web's own and tested as such: the vocabulary narrowing,
 * the wait-rather-than-fall-back behaviour when the rules have not loaded, the
 * words on the controls, and finding the contact record a one-to-one is about.
 *
 * ## Discrimination (verified by running the broken variants)
 *
 *   - Dropping `membershipEnded` on the way in (handing the core `false`
 *     unconditionally) fails "an ended one-to-one can be closed, and no longer
 *     offers a call" with
 *     `expected [ 'call', 'safety-number', …(2) ] to include 'delete-chat'`.
 *   - Restoring the old `contact-name` spelling in `SECTION_NAMES` fails
 *     "nothing the core emits is dropped on the way in" with
 *     `expected [ Array(3) ] to have a length of 4 but got 3`, and three more
 *     besides — which is the point of that guard: a renamed block is caught
 *     as a count, before anything reasons about what is missing.
 */
import { beforeAll, describe, expect, it } from "vitest";
import { readFileSync } from "node:fs";
import { fileURLToPath } from "node:url";

import initWasm, * as wasm from "../../wasm/lakko_ffi_wasm";
import {
  GROUP_ONLY_SECTIONS,
  channelInfoSections,
  findPersonRecord,
  groupInfoSections,
  infoHeading,
  infoOpenLabel,
  infoOpening,
  infoTargetForKind,
  personInfoSections,
  type GroupInfoCapabilities,
  type InfoSection,
} from "./conversation-info";
import type { UiRules } from "./ui-rules";

let rules: UiRules;

beforeAll(async () => {
  const path = fileURLToPath(new URL("../../wasm/lakko_ffi_wasm_bg.wasm", import.meta.url));
  await initWasm({ module_or_path: readFileSync(path) });
  rules = wasm;
});

/** Whether `sections` holds anything that only makes sense for a group. */
function holdsGroupOnlySection(sections: readonly InfoSection[]): boolean {
  return sections.some((s) => GROUP_ONLY_SECTIONS.includes(s));
}

/** Every combination of the six group capability flags. */
function everyGroupCapability(): GroupInfoCapabilities[] {
  const out: GroupInfoCapabilities[] = [];
  for (let bits = 0; bits < 64; bits += 1) {
    out.push({
      canManageMembers: (bits & 1) !== 0,
      canRename: (bits & 2) !== 0,
      canMintInvite: (bits & 4) !== 0,
      isOwner: (bits & 8) !== 0,
      canLeave: (bits & 16) !== 0,
      membershipEnded: (bits & 32) !== 0,
    });
  }
  return out;
}

describe("infoTargetForKind", () => {
  it("a one-to-one opens the person", () => {
    expect(infoTargetForKind(rules, "chat")).toBe("person");
  });

  it("a group opens the group", () => {
    expect(infoTargetForKind(rules, "group")).toBe("group");
  });

  it("answers nothing at all until the shared rules have loaded", () => {
    // Not a fallback: a destination guessed here is a second copy of the
    // rule, and a wrong guess shows a one-to-one the group panel — the
    // defect the routing was changed to fix.
    expect(infoTargetForKind(null, "chat")).toBeNull();
    expect(infoTargetForKind(null, "group")).toBeNull();
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

describe("the vocabulary that crosses the boundary", () => {
  // The narrowing back from the core's `string[]` drops a name this client
  // has never heard of. Dropping is not a guard — a renamed block would
  // vanish from the screen either way — so THIS is the guard: nothing is
  // ever dropped, on any target, under any capabilities. A rename or an
  // addition in Rust fails here rather than silently un-rendering a control.
  it("nothing the core emits is dropped on the way in", () => {
    for (const isContact of [true, false]) {
      for (const membershipEnded of [true, false]) {
        const caps = { isContact, membershipEnded };
        const raw = wasm.personInfoSections(
          new wasm.PersonInfoCapabilities(isContact, membershipEnded),
        );
        expect(personInfoSections(rules, caps)).toHaveLength(raw.length);
      }
    }
    for (const caps of everyGroupCapability()) {
      const raw = wasm.groupInfoSections(
        new wasm.GroupInfoCapabilities(
          caps.canManageMembers,
          caps.canRename,
          caps.canMintInvite,
          caps.isOwner,
          caps.canLeave,
          caps.membershipEnded,
        ),
      );
      expect(groupInfoSections(rules, caps)).toHaveLength(raw.length);
    }
    for (let bits = 0; bits < 8; bits += 1) {
      const caps = {
        isPublisher: (bits & 1) !== 0,
        isSubscriber: (bits & 2) !== 0,
        subscriberCountKnown: (bits & 4) !== 0,
      };
      const raw = wasm.channelInfoSections(
        new wasm.ChannelInfoCapabilities(
          caps.isPublisher,
          caps.isSubscriber,
          caps.subscriberCountKnown,
        ),
      );
      expect(channelInfoSections(rules, caps)).toHaveLength(raw.length);
    }
  });

  // Every name a `.tsx` gates a control on. A view cannot be reached from a
  // test here (no jsdom, no Solid transform), so the list is written down and
  // checked instead: a name no capability ever produces is a control that
  // renders for nobody, which is how `Unsubscribe` and `Remove contact` went
  // missing on the other two clients.
  it("every name a view branches on is one the core actually produces", () => {
    const produced = new Set<InfoSection>();
    for (const isContact of [true, false]) {
      for (const membershipEnded of [true, false]) {
        for (const s of personInfoSections(rules, { isContact, membershipEnded })) {
          produced.add(s);
        }
      }
    }
    for (const caps of everyGroupCapability()) {
      for (const s of groupInfoSections(rules, caps)) produced.add(s);
    }
    for (let bits = 0; bits < 8; bits += 1) {
      const caps = {
        isPublisher: (bits & 1) !== 0,
        isSubscriber: (bits & 2) !== 0,
        subscriberCountKnown: (bits & 4) !== 0,
      };
      for (const s of channelInfoSections(rules, caps)) produced.add(s);
    }
    const branchedOn: readonly InfoSection[] = [
      // contacts/ContactDetailView.tsx
      "message",
      "call",
      "safety-number",
      "rename-contact",
      "remove-contact",
      "delete-chat",
      // routes/Chat.tsx
      "rename-group",
      "add-member",
      "invite-link",
      // routes/Channels.tsx
      "subscriber-count",
      "manage-channel",
      "unsubscribe",
    ];
    for (const name of branchedOn) expect([...produced]).toContain(name);
  });

  // The oracle below is the web's own list of what a group block is, because
  // `InfoSection::group_only` is not on the wasm surface. Its drift is caught
  // here: an entry the core never puts in a group's info is an entry that has
  // been renamed out from under it, and the person property would then be
  // passing over a name that no longer exists.
  it("every group-only name is one a group's info really holds", () => {
    const inSomeGroup = new Set<InfoSection>();
    for (const caps of everyGroupCapability()) {
      for (const s of groupInfoSections(rules, caps)) inSomeGroup.add(s);
    }
    for (const name of GROUP_ONLY_SECTIONS) expect([...inSomeGroup]).toContain(name);
  });
});

describe("personInfoSections", () => {
  it("renders nothing until the shared rules have loaded", () => {
    expect(personInfoSections(null, { isContact: true, membershipEnded: false })).toEqual([]);
  });

  it("offers the contact-list blocks only for someone in the contact list", () => {
    const contact = personInfoSections(rules, {
      isContact: true,
      membershipEnded: false,
    });
    expect(contact).toContain("safety-number");
    expect(contact).toContain("rename-contact");
    expect(contact).toContain("remove-contact");

    const stranger = personInfoSections(rules, {
      isContact: false,
      membershipEnded: false,
    });
    expect(stranger).not.toContain("safety-number");
    expect(stranger).not.toContain("rename-contact");
    expect(stranger).not.toContain("remove-contact");
    // Still a PERSON, not a group: not being a contact is a real answer.
    expect(stranger).toContain("call");
  });

  // Spec section 2.1 (2026-09-26): Message, wherever the card was opened
  // from. It used to be withheld when the card was reached from the chat's
  // own header, which needed an "origin" threaded through the shared rule and
  // gave one person two cards; the origin is gone and there is no argument
  // left to withhold it on.
  it("offers Message whoever is looking, unless the conversation is over", () => {
    for (const isContact of [true, false]) {
      expect(personInfoSections(rules, { isContact, membershipEnded: false })).toContain(
        "message",
      );
    }
  });

  // The defect of 2026-09-23: tapping a person's name showed `Group name`,
  // `Create invite link`, `Make admin`, `Transfer ownership` and `Leave
  // group`. No capability combination may bring any of them back — asserted
  // exhaustively on this side of the boundary as well as in the core, because
  // what the core decides and what this client receives are two claims.
  it("never shows anything that only makes sense for a group", () => {
    for (const isContact of [true, false]) {
      for (const membershipEnded of [true, false]) {
        const sections = personInfoSections(rules, { isContact, membershipEnded });
        expect(holdsGroupOnlySection(sections)).toBe(false);
        for (const groupOnly of GROUP_ONLY_SECTIONS) {
          expect(sections).not.toContain(groupOnly);
        }
      }
    }
  });

  // The defect this change closes: `Delete this chat` lived only in the GROUP
  // panel, and a one-to-one stopped opening that panel when the header began
  // routing by what the conversation IS — so ending a one-to-one left a
  // conversation that could never be closed on this client at all.
  it("an ended one-to-one can be closed, and no longer offers a call", () => {
    for (const isContact of [true, false]) {
      const sections = personInfoSections(rules, {
        isContact,
        membershipEnded: true,
      });
      expect(sections).toContain("delete-chat");
      expect(sections).not.toContain("call");
    }
  });

  it("an ended one-to-one still says everything about the person", () => {
    // The conversation ended; they did not.
    const sections = personInfoSections(rules, {
      isContact: true,
      membershipEnded: true,
    });
    expect(sections).toContain("safety-number");
    expect(sections).toContain("rename-contact");
    expect(sections).toContain("remove-contact");
  });
});

describe("groupInfoSections", () => {
  const full: GroupInfoCapabilities = {
    canManageMembers: true,
    canRename: true,
    canMintInvite: true,
    isOwner: true,
    canLeave: true,
    membershipEnded: false,
  };

  it("renders nothing until the shared rules have loaded", () => {
    expect(groupInfoSections(null, full)).toEqual([]);
  });

  it("carries the three blocks this client gates a control on", () => {
    const sections = groupInfoSections(rules, full);
    for (const gated of ["rename-group", "add-member", "invite-link"] as const) {
      expect(sections).toContain(gated);
    }
  });

  // The rule used to offer `leave-group` unconditionally, and this client
  // hid it anyway through `exitBlockState`, which is a rule nobody was
  // following. The condition is now a field, so the two agree.
  it("a group with no proposal path is offered no way out", () => {
    const sections = groupInfoSections(rules, { ...full, isOwner: false, canLeave: false });
    expect(sections).not.toContain("leave-group");
    expect(sections).toContain("group-name");
    expect(sections).toContain("members");
  });

  // The other half of the same state as the one-to-one above, and the reason
  // the two now agree: one rule, asked twice.
  it("an ended membership is readable and offers nothing but the door", () => {
    for (const caps of everyGroupCapability()) {
      if (!caps.membershipEnded) continue;
      expect(groupInfoSections(rules, caps)).toEqual(["group-name", "members", "delete-chat"]);
    }
  });
});

describe("channelInfoSections", () => {
  it("renders nothing until the shared rules have loaded", () => {
    expect(
      channelInfoSections(null, {
        isPublisher: true,
        isSubscriber: false,
        subscriberCountKnown: true,
      }),
    ).toEqual([]);
  });

  it("always identifies the channel", () => {
    expect(
      channelInfoSections(rules, {
        isPublisher: false,
        isSubscriber: false,
        subscriberCountKnown: false,
      }),
    ).toEqual(["channel-handle", "channel-description"]);
  });

  it("a publisher reaches the screen it already has, a subscriber the way out", () => {
    expect(
      channelInfoSections(rules, {
        isPublisher: true,
        isSubscriber: false,
        subscriberCountKnown: false,
      }),
    ).toContain("manage-channel");
    expect(
      channelInfoSections(rules, {
        isPublisher: false,
        isSubscriber: true,
        subscriberCountKnown: false,
      }),
    ).toContain("unsubscribe");
  });

  it("a publisher is never offered an unsubscribe that would end nothing", () => {
    expect(
      channelInfoSections(rules, {
        isPublisher: true,
        isSubscriber: true,
        subscriberCountKnown: true,
      }),
    ).not.toContain("unsubscribe");
  });

  it("offers a subscriber count only where this device has one", () => {
    expect(
      channelInfoSections(rules, {
        isPublisher: true,
        isSubscriber: false,
        subscriberCountKnown: true,
      }),
    ).toContain("subscriber-count");
    expect(
      channelInfoSections(rules, {
        isPublisher: true,
        isSubscriber: false,
        subscriberCountKnown: false,
      }),
    ).not.toContain("subscriber-count");
  });

  it("never shows anything that only makes sense for a group", () => {
    for (let bits = 0; bits < 8; bits += 1) {
      const sections = channelInfoSections(rules, {
        isPublisher: (bits & 1) !== 0,
        isSubscriber: (bits & 2) !== 0,
        subscriberCountKnown: (bits & 4) !== 0,
      });
      expect(holdsGroupOnlySection(sections)).toBe(false);
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
    const sections = personInfoSections(rules, {
      isContact: record !== null,
      membershipEnded: false,
    });
    expect(holdsGroupOnlySection(sections)).toBe(false);
    expect(sections).toContain("call");
  });
});

describe("infoOpening", () => {
  // `docs/ux/conversation-info.md` section 3, "Except a person": a person's
  // info IS the contact screen and there is one of it, so from the
  // conversation the header goes to the contact's own page rather than
  // rendering a second card as a layer. The layer is the group's idiom, and
  // the fallback for a one-to-one whose contact record cannot be found.
  const record = { signaturePubkeyB64: "keyA", groupIdB64: "g1" };

  it("a contact opens their own page, never a layer", () => {
    expect(infoOpening("person", record)).toBe("contact-page");
  });

  it("a person without a contact record falls back to the layer", () => {
    // A roster that cannot be read: still a PERSON, still not the group panel
    // - the layer renders the person screen with the contact blocks absent.
    expect(infoOpening("person", null)).toBe("layer");
  });

  it("a group opens the layer whatever record is at hand", () => {
    expect(infoOpening("group", null)).toBe("layer");
    // A record cannot belong to a group, but a caller handing one over by
    // mistake must not be sent to a contact page for a group.
    expect(infoOpening("group", record)).toBe("layer");
  });

  it("opens nothing until the destination is known", () => {
    // A layer opened before the rules load would show whichever branch they
    // resolve to a moment later - for a contact, the second card again.
    expect(infoOpening(null, record)).toBe("nothing");
    expect(infoOpening(null, null)).toBe("nothing");
  });
});
