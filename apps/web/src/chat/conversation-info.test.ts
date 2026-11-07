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
  conversationInfoSectionsFor,
  findPersonRecord,
  infoHeading,
  infoOpenLabel,
  infoOpening,
  infoTargetForKind,
  personInfoSections,
  type ChannelInfoCapabilities,
  type ConversationInfoInput,
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

/** Every input of the ADR-0087 info rule, for a group. */
function everyGroupInput(): ConversationInfoInput[] {
  const out: ConversationInfoInput[] = [];
  for (const role of ["owner", "admin", "member"] as const) {
    for (let bits = 0; bits < 16; bits += 1) {
      out.push({
        kind: "group",
        role,
        alone: (bits & 1) !== 0,
        canTransfer: (bits & 2) !== 0,
        canLeave: (bits & 4) !== 0,
        hasHandle: (bits & 8) !== 0,
      });
    }
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

  it("the screen's heading is the same words as the button", () => {
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
    for (const membershipEnded of [true, false]) {
      for (const isContact of [true, false]) {
        const caps = { membershipEnded, isContact };
        const raw = wasm.personInfoSections(new wasm.PersonInfoCapabilities(membershipEnded, isContact));
        expect(personInfoSections(rules, caps)).toHaveLength(raw.length);
      }
    }
    for (const input of everyGroupInput()) {
      const raw = wasm.conversationInfoSections(
        input.kind,
        input.role,
        input.alone,
        input.canTransfer,
        input.canLeave,
        input.hasHandle,
      );
      expect(conversationInfoSectionsFor(rules, input)).toHaveLength(raw.length);
    }
    for (const caps of everyChannelInput()) {
      const role = caps.role === "owner" ? 0 : caps.role === "publisher" ? 1 : 2;
      const Caps = wasm.ChannelInfoCapabilities as unknown as new (...a: unknown[]) => never;
      const raw = wasm.channelInfoSections(
        new Caps(role, caps.hasHandle, caps.hasOtherPublishers, caps.membersOfGroupsAndChannelsOn),
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
    for (const membershipEnded of [true, false]) {
      for (const isContact of [true, false]) {
        for (const s of personInfoSections(rules, { membershipEnded, isContact })) produced.add(s);
      }
    }
    for (const input of everyGroupInput()) {
      for (const s of conversationInfoSectionsFor(rules, input)) produced.add(s);
    }
    for (const caps of everyChannelInput()) {
      for (const s of channelInfoSections(rules, caps)) produced.add(s);
    }
    const branchedOn: readonly InfoSection[] = [
      // contacts/ContactDetailView.tsx
      "message",
      "call",
      "safety-number",
      "rename-contact",
      "remove-contact",
      "add-contact",
      "delete-chat",
      // routes/Chat.tsx and chat/GroupInfoExtras.tsx
      "rename-group",
      "add-member",
      "invite-link",
      "transfer-ownership",
      "delete-group",
      "handle",
      "group-type",
      "reactions",
      // channels/ChannelInfo.tsx
      "channel-description",
      "invite-people",
      "subscribers",
      "admins",
      "members-can-find-me",
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
    for (const input of everyGroupInput()) {
      for (const s of conversationInfoSectionsFor(rules, input)) inSomeGroup.add(s);
    }
    for (const name of GROUP_ONLY_SECTIONS) expect([...inSomeGroup]).toContain(name);
  });
});

describe("Their contacts can find me (who-can-find-me 5.1)", () => {
  it("is on a contact's card only while Contacts of my contacts is on", () => {
    const on = { membershipEnded: false, isContact: true, contactsOfContactsOn: true };
    expect(personInfoSections(rules, on)).toContain("their-contacts-can-find-me");
    expect(personInfoSections(rules, { ...on, contactsOfContactsOn: false })).not.toContain(
      "their-contacts-can-find-me",
    );
    expect(personInfoSections(rules, { membershipEnded: false, isContact: true })).not.toContain(
      "their-contacts-can-find-me",
    );
  });

  it("is not on the card of a person the viewer removed", () => {
    const removed = { membershipEnded: false, isContact: false, contactsOfContactsOn: true };
    expect(personInfoSections(rules, removed)).not.toContain("their-contacts-can-find-me");
  });
});

describe("personInfoSections", () => {
  it("renders nothing until the shared rules have loaded", () => {
    expect(personInfoSections(null, { membershipEnded: false, isContact: true })).toEqual([]);
  });

  it("offers a contact the contact-list blocks", () => {
    const contact = personInfoSections(rules, { membershipEnded: false, isContact: true });
    expect(contact).toContain("safety-number");
    expect(contact).toContain("rename-contact");
    expect(contact).toContain("remove-contact");
    expect(contact).toContain("call");
    expect(contact).not.toContain("add-contact");
  });

  // Amended 2026-10-24: a person the viewer removed keeps the conversation,
  // and its card puts them back.
  it("offers a removed person Add to contacts instead of rename and remove", () => {
    const removed = personInfoSections(rules, { membershipEnded: false, isContact: false });
    expect(removed).toContain("add-contact");
    expect(removed).toContain("safety-number");
    expect(removed).toContain("call");
    expect(removed).not.toContain("rename-contact");
    expect(removed).not.toContain("remove-contact");
  });

  // Spec section 2.1 (2026-09-26): Message, wherever the card was opened
  // from. It used to be withheld when the card was reached from the chat's
  // own header, which needed an "origin" threaded through the shared rule and
  // gave one person two cards; the origin is gone and there is no argument
  // left to withhold it on.
  it("offers Message whoever is looking, unless the conversation is over", () => {
    expect(personInfoSections(rules, { membershipEnded: false, isContact: true })).toContain("message");
  });

  // The defect of 2026-09-23: tapping a person's name showed `Group name`,
  // `Create invite link`, `Make admin`, `Transfer ownership` and `Leave
  // group`. No capability combination may bring any of them back — asserted
  // exhaustively on this side of the boundary as well as in the core, because
  // what the core decides and what this client receives are two claims.
  it("never shows anything that only makes sense for a group", () => {
    for (const membershipEnded of [true, false]) {
      const sections = personInfoSections(rules, { membershipEnded, isContact: membershipEnded });
      expect(holdsGroupOnlySection(sections)).toBe(false);
      for (const groupOnly of GROUP_ONLY_SECTIONS) {
        expect(sections).not.toContain(groupOnly);
      }
    }
  });

  // The defect this change closes: `Delete this chat` lived only in the GROUP
  // panel, and a one-to-one stopped opening that panel when the header began
  // routing by what the conversation IS — so ending a one-to-one left a
  // conversation that could never be closed on this client at all.
  it("an ended one-to-one can be closed, and no longer offers a call", () => {
    const sections = personInfoSections(rules, { membershipEnded: true, isContact: true });
    expect(sections).toContain("delete-chat");
    expect(sections).not.toContain("call");
  });

  it("an ended one-to-one still says everything about the person", () => {
    // The conversation ended; they did not.
    const sections = personInfoSections(rules, { membershipEnded: true, isContact: true });
    expect(sections).toContain("safety-number");
    expect(sections).toContain("rename-contact");
    expect(sections).toContain("remove-contact");
  });
});

describe("conversationInfoSectionsFor (ADR-0087 section 18)", () => {
  const owner: ConversationInfoInput = {
    kind: "group",
    role: "owner",
    alone: false,
    canTransfer: true,
    canLeave: true,
    hasHandle: true,
  };

  it("renders nothing until the shared rules have loaded", () => {
    expect(conversationInfoSectionsFor(null, owner)).toEqual([]);
  });

  it("carries the blocks this client gates a control on", () => {
    const sections = conversationInfoSectionsFor(rules, owner);
    for (const gated of ["rename-group", "add-member", "invite-link", "group-type", "handle"] as const) {
      expect(sections).toContain(gated);
    }
  });

  it("an owner alone deletes the group instead of handing it over", () => {
    const sections = conversationInfoSectionsFor(rules, { ...owner, alone: true });
    expect(sections).toContain("delete-group");
    expect(sections).not.toContain("transfer-ownership");
  });

  it("a group with no proposal path is offered no way out", () => {
    const sections = conversationInfoSectionsFor(rules, { ...owner, role: "member", canLeave: false });
    expect(sections).not.toContain("leave-group");
    expect(sections).toContain("group-name");
    expect(sections).toContain("members");
  });

  it("the owner and admins choose the reactions, a member does not", () => {
    expect(conversationInfoSectionsFor(rules, owner)).toContain("reactions");
    expect(conversationInfoSectionsFor(rules, { ...owner, role: "admin" })).toContain("reactions");
    expect(conversationInfoSectionsFor(rules, { ...owner, role: "member" })).not.toContain("reactions");
  });

  it("only the owner chooses who can join", () => {
    for (const role of ["admin", "member"] as const) {
      expect(conversationInfoSectionsFor(rules, { ...owner, role })).not.toContain("group-type");
    }
  });
});

function everyChannelInput(): ChannelInfoCapabilities[] {
  const out: ChannelInfoCapabilities[] = [];
  for (const role of ["owner", "publisher", "subscriber"] as const) {
    for (let bits = 0; bits < 8; bits += 1) {
      out.push({
        role,
        hasHandle: (bits & 1) !== 0,
        hasOtherPublishers: (bits & 2) !== 0,
        membersOfGroupsAndChannelsOn: (bits & 4) !== 0,
      });
    }
  }
  return out;
}

describe("channelInfoSections", () => {
  const caps = (
    role: "owner" | "publisher" | "subscriber",
    extra: Partial<{ hasHandle: boolean; hasOtherPublishers: boolean; membersOfGroupsAndChannelsOn: boolean }> = {},
  ): ChannelInfoCapabilities => ({
    role,
    hasHandle: true,
    hasOtherPublishers: false,
    membersOfGroupsAndChannelsOn: true,
    ...extra,
  });

  it("renders nothing until the shared rules have loaded", () => {
    expect(channelInfoSections(null, caps("owner"))).toEqual([]);
  });

  it("always identifies the channel and lists its subscribers", () => {
    for (const role of ["owner", "publisher", "subscriber"] as const) {
      const s = channelInfoSections(rules, caps(role));
      expect(s).toContain("channel-description");
      expect(s).toContain("subscribers");
    }
  });

  it("the owner and publishers invite; only the owner changes the type", () => {
    expect(channelInfoSections(rules, caps("publisher"))).toContain("invite-people");
    expect(channelInfoSections(rules, caps("publisher"))).not.toContain("group-type");
    expect(channelInfoSections(rules, caps("owner"))).toContain("group-type");
    expect(channelInfoSections(rules, caps("subscriber"))).not.toContain("invite-people");
  });

  it("a subscriber gets the way out, a publisher none", () => {
    expect(channelInfoSections(rules, caps("subscriber"))).toContain("unsubscribe");
    expect(channelInfoSections(rules, caps("publisher"))).not.toContain("unsubscribe");
  });

  it("never shows anything that only makes sense for a group", () => {
    for (const c of everyChannelInput()) {
      const s = channelInfoSections(rules, c);
      for (const g of ["group-name", "members", "add-member", "roles", "transfer-ownership", "leave-group"] as const) {
        expect(s).not.toContain(g);
      }
    }
  });
});

describe("findPersonRecord", () => {
  const linked = { signaturePubkeyB64: "keyA", groupIdB64: "g1" };
  const byKey = { signaturePubkeyB64: "keyB" };

  it("the peer's key decides; a record linked to this chat for another person does not count", () => {
    expect(
      findPersonRecord([byKey, linked], { groupIdB64: "g1", peerKeyB64: "keyB" }),
    ).toBe(byKey);
    expect(
      findPersonRecord([byKey, linked], { groupIdB64: "g1", peerKeyB64: "keyA" }),
    ).toBe(linked);
  });

  it("uses the chat's link only while the roster cannot name the peer", () => {
    expect(findPersonRecord([byKey, linked], { groupIdB64: "g1", peerKeyB64: null })).toBe(linked);
  });

  it("falls back to the peer's key when no link was ever written", () => {
    expect(
      findPersonRecord([byKey, linked], { groupIdB64: "g9", peerKeyB64: "keyB" }),
    ).toBe(byKey);
  });

  it("is null when no record can be found", () => {
    expect(
      findPersonRecord([byKey], { groupIdB64: "g9", peerKeyB64: "keyZ" }),
    ).toBeNull();
    expect(findPersonRecord([byKey], { groupIdB64: "g9", peerKeyB64: null })).toBeNull();
  });
});

describe("infoOpening", () => {
  // `docs/ux/conversation-info.md` section 3, "Except a person": a person's
  // info IS the contact screen and there is one of it, so from the
  // conversation the header goes to the contact's own page rather than
  // rendering a second card as a layer. The layer is the group's idiom and
  // nobody else's.
  const record = { signaturePubkeyB64: "keyA", groupIdB64: "g1" };

  it("a contact opens their own page, never a layer", () => {
    expect(infoOpening("person", record)).toBe("contact-page");
  });

  it("a person without a contact record opens nothing", () => {
    // A roster that cannot be read. Every one-to-one peer is a contact by
    // construction, so this is not a state with a card of its own: not the
    // group panel, and no longer the retired "not a contact" layer either.
    expect(infoOpening("person", null)).toBe("nothing");
  });

  it("a group opens its screen whatever record is at hand", () => {
    expect(infoOpening("group", null)).toBe("group-screen");
    // A record cannot belong to a group, but a caller handing one over by
    // mistake must not be sent to a contact page for a group.
    expect(infoOpening("group", record)).toBe("group-screen");
  });

  it("opens nothing until the destination is known", () => {
    // A layer opened before the rules load would show whichever branch they
    // resolve to a moment later - for a contact, the second card again.
    expect(infoOpening(null, record)).toBe("nothing");
    expect(infoOpening(null, null)).toBe("nothing");
  });
});
