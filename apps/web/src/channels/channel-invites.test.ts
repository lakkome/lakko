// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * Opening a publisher invitation (ADR-0087 section 15) through the real wasm
 * core, from the message's link (the card) and from the `/g/:slug` route's
 * link. The route builds its link from the router's slug and hash: on an
 * in-app navigation (Accept on the card) `window.location` still names the
 * chat, and that address opens nothing (seen in the desktop app 2026-11-07).
 */
import { beforeAll, describe, expect, it, vi } from "vitest";
import { readFileSync } from "node:fs";
import { fileURLToPath } from "node:url";

import * as wasmNs from "../../wasm/lakko_ffi_wasm";
import init from "../../wasm/lakko_ffi_wasm";

let ledgerRow: Uint8Array | null = null;

vi.mock("../wasm", async () => {
  const ns = await import("../../wasm/lakko_ffi_wasm");
  return { loadWasm: async () => ns, wasmSync: () => ns };
});
vi.mock("../discovery/client", async (importOriginal) => {
  const real = await importOriginal<typeof import("../discovery/client")>();
  class DiscoveryClient {
    async fetchGroupInvite(): Promise<Uint8Array | null> {
      return ledgerRow;
    }
  }
  return { ...real, DiscoveryClient };
});

import { openChannelLink } from "./channel-invites";
import { channelErrorCode } from "./channel-error-copy";
import { composeSealedInviteLink, inviteLinkWidths } from "../contacts/invite-link";
import { groupInviteRouteLink } from "../contacts/group-invite";

beforeAll(async () => {
  const p = fileURLToPath(new URL("../../wasm/lakko_ffi_wasm_bg.wasm", import.meta.url));
  await init({ module_or_path: readFileSync(p) });
});

function mintPublisherLink(): string {
  const w = wasmNs;
  const ownerSeed = new Uint8Array(32).fill(7);
  const groupId = new Uint8Array(32).fill(9);
  const channelId = new Uint8Array(16).fill(3);
  const root = w.dsJournalRootSeed(ownerSeed, groupId);
  const knockSeed = w.groupKnockSeed(root, 0);
  const m = w.composeCoPublisherInviteCapability(
    ownerSeed,
    groupId,
    0n,
    channelId,
    "Pixel Pub Walk",
    w.groupKnockPublicKey(knockSeed),
    0,
    undefined,
    "VG3",
  );
  ledgerRow = m.sealed;
  const widths = inviteLinkWidths(w);
  const url = composeSealedInviteLink({
    origin: "https://test.lakko.me",
    prefix: "/g/",
    blobId: new Uint8Array(widths.blobIdLen).fill(5),
    secret: m.linkSecret,
    widths,
  });
  m.free();
  return url;
}

describe("openChannelLink, publisher invitation", () => {
  it("opens from the message link and from the route's link alike", async () => {
    const url = mintPublisherLink();
    const fromCard = await openChannelLink(url);
    expect(fromCard?.publisherInvite).toBe(true);
    expect(fromCard?.name).toBe("Pixel Pub Walk");
    expect(fromCard?.inviterName).toBe("VG3");

    const u = new URL(url);
    const slug = u.pathname.slice("/g/".length);
    const fromRoute = await openChannelLink(groupInviteRouteLink(slug, u.hash));
    expect(fromRoute?.publisherInvite).toBe(true);
    expect(fromRoute?.channelIdHex).toBe(fromCard?.channelIdHex);
    // The desktop app's own scheme opens the same link.
    const app = await openChannelLink(url.replace("https://test.lakko.me", "lakkoapp://localhost"));
    expect(app?.publisherInvite).toBe(true);
  });

  it("refuses the previous screen's address, which is what window.location held", async () => {
    mintPublisherLink();
    const err = await openChannelLink("lakkoapp://localhost/chat/0011").catch((e: unknown) => e);
    expect(channelErrorCode(err)).toBe("link-invalid");
  });

  it("groupInviteRouteLink keeps or adds the fragment's #", () => {
    expect(groupInviteRouteLink("AbCdEfGh", "#secret")).toBe("/g/AbCdEfGh#secret");
    expect(groupInviteRouteLink("AbCdEfGh", "secret")).toBe("/g/AbCdEfGh#secret");
    expect(groupInviteRouteLink("AbCdEfGh", "")).toBe("/g/AbCdEfGh");
  });
});
