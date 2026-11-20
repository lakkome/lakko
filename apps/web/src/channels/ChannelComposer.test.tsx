// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * The channel composer's keys (ADR-0087 section 12, the chat's composer
 * behaviour): Enter publishes, Shift+Enter does not. Seen live 2026-11-05:
 * Enter did nothing and only the send button published.
 */
import { afterEach, describe, expect, it, vi } from "vitest";

import { one, renderUi } from "../test/ui";

vi.mock("../auth/AuthContext", () => ({
  useAuth: () => ({ identitySecrets: () => null, state: () => ({ kind: "anonymous" }) }),
}));
vi.mock("../calls/CallHostContext", () => ({ useCallHost: () => ({}) }));
vi.mock("../calls/capture-rule", () => ({ callOwnsCaptureNow: () => false }));
vi.mock("../chat/ui-rules", () => ({ uiRules: () => null }));
vi.mock("../chat/attachments", async (orig) => ({
  ...(await orig<typeof import("../chat/attachments")>()),
  composeEnvelope: async (text: string) => new TextEncoder().encode(`env:${text}`),
  composeEditEnvelope: async (cursor: number, text: string) => new TextEncoder().encode(`edit:${cursor}:${text}`),
}));

import { ChannelComposer } from "./ChannelComposer";
import type { MediaHub } from "../media/media-hub";

const hub = { registry: { set: () => null, entries: () => [], revoke: () => undefined } } as unknown as MediaHub;

afterEach(() => {
  vi.restoreAllMocks();
});

function type(field: HTMLTextAreaElement, value: string): void {
  field.value = value;
  field.dispatchEvent(new Event("input", { bubbles: true }));
}

function key(field: HTMLTextAreaElement, init: KeyboardEventInit): void {
  field.dispatchEvent(new KeyboardEvent("keydown", { bubbles: true, cancelable: true, ...init }));
}

describe("ChannelComposer keys", () => {
  it("Enter publishes what the field holds", async () => {
    const published: string[] = [];
    renderUi(() => (
      <ChannelComposer trayKey="test"
        hub={hub}
        editing={null}
        onCancelEdit={() => undefined}
        publish={async (p) => {
          published.push(new TextDecoder().decode(p));
        }}
      />
    ));
    const field = one<HTMLTextAreaElement>("textarea");
    type(field, "hello");
    key(field, { key: "Enter" });
    await vi.waitFor(() => expect(published).toEqual(["env:hello"]));
  });

  it("Shift+Enter does not publish", async () => {
    const publish = vi.fn(async () => undefined);
    renderUi(() => <ChannelComposer trayKey="test" hub={hub} editing={null} onCancelEdit={() => undefined} publish={publish} />);
    const field = one<HTMLTextAreaElement>("textarea");
    type(field, "hello");
    key(field, { key: "Enter", shiftKey: true });
    await new Promise((r) => setTimeout(r, 20));
    expect(publish).not.toHaveBeenCalled();
  });

  it("Enter on an edited post publishes the edit", async () => {
    const published: string[] = [];
    renderUi(() => (
      <ChannelComposer trayKey="test"
        hub={hub}
        editing={{ cursor: 7, text: "old" }}
        onCancelEdit={() => undefined}
        publish={async (p) => {
          published.push(new TextDecoder().decode(p));
        }}
      />
    ));
    const field = one<HTMLTextAreaElement>("textarea");
    await new Promise((r) => setTimeout(r, 0));
    type(field, "new");
    key(field, { key: "Enter" });
    await vi.waitFor(() => expect(published).toEqual(["edit:7:new"]));
  });
});
