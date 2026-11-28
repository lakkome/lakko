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
import { afterEach, describe, expect, it, vi } from "vitest";

import { all, one, present, renderUi } from "../test/ui";
import { t } from "../l10n";

/** `docs/ux/navigation.md` 2.4 — the account whose stored names were read. */
let profilesLoadedFor: string | null = "acct";

vi.mock("../profile/ProfileContext", () => ({
  useProfiles: () => ({
    version: () => 0,
    loadedFor: () => profilesLoadedFor,
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

  it("stays listed after a failed Accept, with the line and a Try again that accepts again", () => {
    const onAccept = vi.fn();
    mount({ pending: () => [request], onAccept, acceptFailed: () => true });
    expect(document.body.textContent).toContain(t("contacts.request.accept_failed"));
    const accept = all<HTMLButtonElement>("button").find(
      (b) => b.textContent?.trim() === t("common.try_again"),
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

describe("a request from the new account of a contact who started over (UX 16.11)", () => {
  const request = {
    messageId: 9,
    fromPubkey: new Uint8Array(32).fill(2),
    intro: "It is me again",
    createdAt: "2026-11-27T10:00:00Z",
  } as never;

  it("is an ordinary card with one more line under the name; Accept and Decline stay", () => {
    const onAccept = vi.fn();
    const onDecline = vi.fn();
    mount({ pending: () => [request], onAccept, onDecline, startedOverNote: () => true });
    const note = one<HTMLElement>(".lk-request-note");
    expect(note.textContent?.trim()).toBe(t("contacts.started_over.request_note"));
    expect(note.getAttribute("role")).toBe("note");
    // Under the name, above the intro.
    const body = note.parentElement as HTMLElement;
    const order = Array.from(body.children).map((c) => c.className);
    expect(order.indexOf("lk-row-title")).toBeLessThan(order.indexOf("lk-request-note"));
    expect(order.indexOf("lk-request-note")).toBeLessThan(order.indexOf("lk-request-intro"));
    const labels = all<HTMLButtonElement>("button").map((b) => b.textContent?.trim());
    expect(labels).toContain(t("contacts.request.accept"));
    expect(labels).toContain(t("contacts.request.decline"));
    all<HTMLButtonElement>("button")
      .find((b) => b.textContent?.trim() === t("contacts.request.accept"))!
      .click();
    expect(onAccept).toHaveBeenCalledTimes(1);
  });

  it("every other request carries no such line", () => {
    mount({ pending: () => [request], startedOverNote: () => false });
    expect(present(".lk-request-note")).toBe(false);
    mount({ pending: () => [request] });
    expect(present(".lk-request-note")).toBe(false);
    expect(document.body.textContent).not.toContain(t("contacts.started_over.request_note"));
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

describe("Contacts list pane: one person, one row", () => {
  const invited = {
    accountIdHex: "aa",
    signaturePubkeyB64: "INES",
    displayName: "Ines Vik",
    username: "ines",
  } as never;

  /** Type into the search field. */
  function type(text: string): void {
    const input = one<HTMLInputElement>('input[type="search"]');
    input.value = text;
    input.dispatchEvent(new Event("input", { bubbles: true }));
  }

  it("an invited row writes its username with @", () => {
    mount({ invited: () => [invited] });
    expect(all(".lk-row-sub").map((e) => e.textContent)).toEqual(["@ines"]);
  });

  it("tells the people half who the list shows, an invited row included", () => {
    let listed: ReadonlySet<string> = new Set();
    let any = false;
    mount({
      invited: () => [invited],
      people: (keys, anyRow) => {
        listed = keys();
        any = anyRow();
        return <div />;
      },
    });
    type("ines");
    expect([...listed]).toEqual(["INES"]);
    expect(any).toBe(true);
  });

  it("leaves the line that nothing matches to the people half", () => {
    mount({ invited: () => [invited] });
    type("zzz");
    expect(present(".lk-pane-nomatch")).toBe(false);
    expect(present('[data-testid="people"]')).toBe(true);
  });
});

describe("Contacts list pane: before the stored names are read (navigation.md 2.4)", () => {
  const contact = {
    signaturePubkeyB64: "Anna",
    displayName: "",
    groupIdB64: "g1",
  } as never;

  afterEach(() => {
    profilesLoadedFor = "acct";
  });

  it("draws no contact row and no empty state", () => {
    profilesLoadedFor = null;
    mount({ contacts: () => [contact] });
    expect(document.body.textContent).not.toContain("Anna");
    expect(present(".lk-empty")).toBe(false);
  });

  it("draws the contact once they are read", () => {
    mount({ contacts: () => [contact] });
    expect(document.body.textContent).toContain("Anna");
  });
});
