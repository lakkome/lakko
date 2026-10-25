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

function mount(): { onAdd: ReturnType<typeof vi.fn> } {
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
    />
  ));
  return { onAdd };
}

describe("Contacts list pane: one search field (UX 6.1)", () => {
  it("offers Share invite link and no Add by username form", () => {
    mount();
    one<HTMLButtonElement>(`button[aria-label="${t("contacts.list.add")}"]`).click();
    const items = all<HTMLButtonElement>('[role="menuitem"]').map((b) => b.textContent?.trim());
    expect(items).toEqual([t("contacts.add.share_invite")]);
    expect(items).not.toContain(t("contacts.add.by_username"));
  });

  it("the empty list's Add contact goes to the search field", () => {
    const { onAdd } = mount();
    const button = all<HTMLButtonElement>("button.primary").find(
      (b) => b.textContent?.trim() === t("contacts.list.empty.action"),
    );
    expect(button).toBeDefined();
    button!.click();
    expect(document.activeElement).toBe(one('input[type="search"]'));
    expect(onAdd).not.toHaveBeenCalled();
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
