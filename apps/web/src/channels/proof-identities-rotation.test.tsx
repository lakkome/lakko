// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * 2026-11-20 (ADR-0074) — after TWO rotations the identities the session
 * installs for channel proofs are the current one and the OLDEST own
 * predecessor (the lineage), as on iOS and Android: an owner proof by the
 * current key, a co-publisher proof by the lineage key.
 */
import { describe, expect, it, vi } from "vitest";

vi.mock("../ds/session-access", () => ({
  clearDsAccess: vi.fn(),
  installMailboxAccess: vi.fn(),
  installPredecessorChainAccess: vi.fn(),
}));

import { ChatProvider, useChats, type ChatApi } from "../chat/ChatContext";
import { renderUi } from "../test/ui";
import {
  coPublisherProofIdentity,
  installAccountIdentities,
  ownerProofIdentity,
  proofIdentitiesOf,
} from "./api";
import type { Wasm } from "../wasm";

type Identity = InstanceType<Wasm["Identity"]>;

function identity(n: number): Identity {
  return { signaturePublicKey: new Uint8Array(32).fill(n), free: () => undefined } as unknown as Identity;
}

describe("channel proof identities after two rotations", () => {
  it("the lineage is the oldest own predecessor, the owner key the current one", () => {
    let chats: ChatApi | null = null;
    renderUi(() => (
      <ChatProvider>
        {(() => {
          chats = useChats();
          return null;
        })()}
      </ChatProvider>
    ));
    const api = chats as unknown as ChatApi;
    const first = identity(1);
    const second = identity(2);
    const third = identity(3);
    api.setAccountIdentity(first as never);
    api.rotateAccountIdentity(second as never);
    api.rotateAccountIdentity(third as never);

    const ids = proofIdentitiesOf(api as never);
    expect(ids?.current).toBe(third);
    expect(ids?.lineage).toBe(first);

    installAccountIdentities(() => proofIdentitiesOf(api as never));
    try {
      for (const passed of [first, third]) {
        expect(ownerProofIdentity(passed)).toBe(third);
        expect(coPublisherProofIdentity(passed)).toBe(first);
      }
    } finally {
      installAccountIdentities(null);
    }
  });
});
