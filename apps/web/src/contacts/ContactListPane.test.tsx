// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * The Contacts list pane under `docs/ux/who-can-find-me.md` section 6.1: one
 * search field and nothing else. The Add menu keeps only Share invite link
 * (web has no code scanning), the empty list's Add contact takes the person
 * to the field, and typing shows the people half of the field under the
 * contacts - on an empty list too.
 */
import { createSignal } from "solid-js";
import { describe, expect, it, vi } from "vitest";

import { all, one, present, renderUi } from "../test/ui";
import { t } from "../l10n";

vi.mock("../profile/ProfileContext", () => ({
  useProfiles: () => ({
    version: () => 0,
    resolve: (k: Uint8Array | string) => ({ name: typeof k === "string" ? k : "Member", username: null }),
  }),
}));

import { ContactListPane, type AddMode } from "./ContactListPane";

function mount(over: Partial<Parameters<typeof ContactListPane>[0]> = {}): { onAdd: ReturnType<typeof vi.fn> } {
  const onAdd = vi.fn<(mode: AddMode) => void>();
  const [filter, setFilter] = createSignal("");
  renderUi(() => (
    <ContactListPane
      filter={filter}
      onFilter={setFilter}
      people={() => <div data-testid="people">people</div>}
      onAdd={onAdd}
      pending={() => []}
      pendingBusy={() => false}
      error={() => null}
      onAccept={() => {}}
      onDecline={() => {}}
      sent={() => []}
      onDismissSent={() => {}}
      invited={() => []}
      onOpenInvited={() => {}}
      contacts={() => []}
      selectedKey={() => null}
      onSelect={() => {}}
      {...over}
    />
  ));
  return { onAdd };
}

describe("an incoming request", () => {
  const request = {
    messageId: 7,
    fromPubkey: new Uint8Array(32).fill(1),
    intro: null,
    createdAt: "2026-10-30T10:00:00Z",
  } as never;

  it("stays listed after a failed Accept, with the line, and Accept works again", () => {
    const onAccept = vi.fn();
    mount({ pending: () => [request], onAccept, acceptFailed: () => true });
    expect(document.body.textContent).toContain(t("contacts.request.accept_failed"));
    const accept = all<HTMLButtonElement>("button").find(
      (b) => b.textContent?.trim() === t("contacts.request.accept"),
    );
    expect(accept?.disabled).toBe(false);
    accept!.click();
    expect(onAccept).toHaveBeenCalledTimes(1);
  });

  it("has no line until an Accept failed, and no second Accept while one runs", () => {
    mount({ pending: () => [request], accepting: () => true });
    expect(document.body.textContent).not.toContain(t("contacts.request.accept_failed"));
    const accept = all<HTMLButtonElement>("button").find(
      (b) => b.textContent?.trim() === t("contacts.request.accept"),
    );
    expect(accept?.disabled).toBe(true);
  });
});

describe("Contacts list pane: one search field (UX 6.1)", () => {
  it("offers Share invite link and no Add by username form", () => {
    mount();
    one<HTMLButtonElement>(`button[aria-label="${t("contacts.list.add")}"]`).click();
    const items = all<HTMLButtonElement>('[role="menuitem"]').map((b) => b.textContent?.trim());
    expect(items).toEqual([t("contacts.add.share_invite")]);
    expect(items).toHaveLength(1);
  });

  it("the empty list's button opens the invite links", () => {
    const { onAdd } = mount();
    const button = all<HTMLButtonElement>("button.primary").find(
      (b) => b.textContent?.trim() === t("contacts.add.share_invite"),
    );
    expect(button).toBeDefined();
    button!.click();
    expect(onAdd).toHaveBeenCalledWith("invite");
  });

  it("typing shows the people half of the field, on an empty list too", () => {
    mount();
    expect(present('[data-testid="people"]')).toBe(false);
    const input = one<HTMLInputElement>('input[type="search"]');
    input.value = "anna";
    input.dispatchEvent(new Event("input", { bubbles: true }));
    expect(present('[data-testid="people"]')).toBe(true);
    expect(present(".lk-empty")).toBe(false);
  });
});
