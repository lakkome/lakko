// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/** ADR-0054 S3 — the token copy (docs/ux/anti-abuse.md section 3). */
import { describe, expect, it } from "vitest";

import {
  AnonTokenQuotaExhaustedError,
  AnonTokensDisabledError,
  AnonTokenUnavailableError,
} from "./anon-token-pool";
import { ACTION_ALLOWANCE_USED, LOOKUP_ALLOWANCE_USED, anonTokenMessage } from "./token-copy";
import { findByName, findByNameMessage } from "../chat/find-by-name";
import { joinFailureNotice } from "../routes/join-group-failure";
import { formatContactsError } from "../contacts/errors";
import { ContactsError } from "../contacts/client";
import type { UiRules } from "../chat/ui-rules";

const NET = "network copy";

describe("anonTokenMessage", () => {
  it("search: the lookup copy", () => {
    expect(anonTokenMessage(new AnonTokenQuotaExhaustedError("discovery-lookup", 10), NET)).toBe(
      "You have looked up a lot of people today. Try again tomorrow.",
    );
  });

  // The storage contexts (ADR-0064 / ADR-0065) have no copy of their own:
  // they take the generic allowance copy.
  it.each([
    "invite-consume",
    "discovery-register",
    "ds-knock",
    "vault-claim",
    "blob-upload",
    "blob-read",
    "mailbox-write",
    "call-join",
  ] as const)(
    "%s: the allowance copy",
    (context) => {
      expect(anonTokenMessage(new AnonTokenQuotaExhaustedError(context, 10), NET)).toBe(
        "You have used today's allowance for this. Try again tomorrow.",
      );
    },
  );

  it("push registration is silent", () => {
    expect(anonTokenMessage(new AnonTokenQuotaExhaustedError("push-register", 10), NET)).toBeNull();
  });

  it("disabled or unavailable is the action's own network copy", () => {
    expect(anonTokenMessage(new AnonTokensDisabledError("ds-knock"), NET)).toBe(NET);
    expect(anonTokenMessage(new AnonTokenUnavailableError("ds-knock", "x"), NET)).toBe(NET);
  });

  it("finds a wrapped refusal and ignores everything else", () => {
    const wrapped = ContactsError.wrap(new AnonTokenQuotaExhaustedError("discovery-lookup", 1));
    expect(anonTokenMessage(wrapped, NET)).toBe(LOOKUP_ALLOWANCE_USED());
    expect(anonTokenMessage(new Error("boom"), NET)).toBeNull();
  });
});

describe("the surfaces", () => {
  const rules = {
    bareName: (s: string) => s.replace(/^@/, ""),
    looksLikeAName: () => true,
  } as unknown as UiRules;

  it("search: an allowance used up is neither 'nothing found' nor 'unreachable'", async () => {
    let personAsked = false;
    const found = await findByName(
      "alice",
      {
        channel: async () => {
          throw new AnonTokenQuotaExhaustedError("discovery-lookup", 100);
        },
        person: async () => {
          personAsked = true;
          return null;
        },
      },
      rules,
    );
    expect(found).toEqual({ kind: "allowance-used" });
    expect(personAsked).toBe(false);
    expect(findByNameMessage(found, "alice", rules)).toBe(LOOKUP_ALLOWANCE_USED());
  });

  it("a contact request whose lookup ran out says so", () => {
    const err = ContactsError.wrap(new AnonTokenQuotaExhaustedError("discovery-lookup", 1));
    expect(formatContactsError(err, "contacts.error.send")).toBe(LOOKUP_ALLOWANCE_USED());
  });

  it("a group link that ran out of tokens says so, without a retry", () => {
    const err = ContactsError.wrap(new AnonTokenQuotaExhaustedError("invite-consume", 1));
    const notice = joinFailureNotice(err);
    expect(notice.render()).toBe(ACTION_ALLOWANCE_USED());
    expect(notice.offerRetry).toBe(false);
    const knock = ContactsError.wrap(new AnonTokenQuotaExhaustedError("ds-knock", 1));
    const knockNotice = joinFailureNotice(knock);
    expect(knockNotice.render()).toBe(ACTION_ALLOWANCE_USED());
    expect(knockNotice.offerRetry).toBe(false);
  });
});
