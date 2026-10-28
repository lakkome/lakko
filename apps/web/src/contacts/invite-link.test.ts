// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import { describe, expect, it } from "vitest";

import { DiscoveryHttpError, DiscoveryRateLimitedError, isDiscoveryRateLimited } from "../discovery/client";
import {
  InviteLinkUnreachableError,
  isInviteLinkDamaged,
  isInviteLinkUnreachable,
  openInviteLinkFromUrl,
  type InviteLinkWidths,
} from "./invite-link";

const widths: InviteLinkWidths = { blobIdLen: 6, blobIdUrlLen: 8, linkSecretLen: 12, linkSecretUrlLen: 16 };
const slug = "AAAAAAAA";
const hash = "#AAAAAAAAAAAAAAAA";

const open = (fetchPointer: () => Promise<Uint8Array | null>) =>
  openInviteLinkFromUrl({
    slug,
    hash,
    widths,
    what: "invite",
    fetchPointer,
    open: () => "capability",
  });

describe("openInviteLinkFromUrl", () => {
  it("reports a failure to ask the ledger as unreachable, never as a damaged link", async () => {
    const serverError = new DiscoveryHttpError(500, "/discovery/invite (GET)", "boom");
    const err = await open(() => Promise.reject(serverError)).catch((e: unknown) => e);
    expect(err).toBeInstanceOf(InviteLinkUnreachableError);
    expect(isInviteLinkUnreachable(err)).toBe(true);
    expect(isInviteLinkDamaged(err)).toBe(false);
  });

  it("keeps a 429 recognisable inside the unreachable error", async () => {
    const limited = new DiscoveryRateLimitedError("/discovery/invite (GET)", null);
    const err = await open(() => Promise.reject(limited)).catch((e: unknown) => e);
    expect(isDiscoveryRateLimited(err)).toBe(true);
  });

  it("says not found when the ledger has no such link", async () => {
    expect(await open(() => Promise.resolve(null))).toEqual({ kind: "not-found" });
  });

  it("refuses a missing fragment as damaged before asking the ledger", async () => {
    let asked = false;
    const err = await openInviteLinkFromUrl({
      slug,
      hash: null,
      widths,
      what: "invite",
      fetchPointer: () => {
        asked = true;
        return Promise.resolve(null);
      },
      open: () => "capability",
    }).catch((e: unknown) => e);
    expect(isInviteLinkDamaged(err)).toBe(true);
    expect(asked).toBe(false);
  });

  it("throws on a malformed slug without asking the ledger, and that is not unreachable", async () => {
    const err = await openInviteLinkFromUrl({
      slug: "too-short",
      hash,
      widths,
      what: "invite",
      fetchPointer: () => Promise.reject(new Error("must not be asked")),
      open: () => "capability",
    }).catch((e: unknown) => e);
    expect(err).toBeInstanceOf(Error);
    expect(isInviteLinkUnreachable(err)).toBe(false);
  });
});
