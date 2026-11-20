// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * Every control here is named by the text on its row (base walk 2026-11-13:
 * Channel info's rows had no name, a contact's checkbox and the Who can find
 * me radios read as "on"). jsdom computes no accessible name, so this checks
 * the reference: `aria-labelledby` resolves to elements whose text is what
 * the row shows.
 */
import { describe, expect, it } from "vitest";
import { all, one, renderUi } from "../test/ui";
import { ChoiceRow, InfoRowButton, PickRow } from "./LabelledRows";

/** The name `aria-labelledby` gives `el`: the referenced texts, in order. */
function nameOf(el: Element): string {
  const ids = (el.getAttribute("aria-labelledby") ?? "").split(/\s+/).filter((s) => s.length > 0);
  return ids.map((id) => document.getElementById(id)?.textContent?.trim() ?? "<missing>").join(" ");
}

describe("LabelledRows", () => {
  it("names an info row by its title and value", () => {
    renderUi(() => (
      <>
        <InfoRowButton icon="person-plus" title="Invite people" onOpen={() => undefined} />
        <InfoRowButton icon="person-2" title="Subscribers" value="12" onOpen={() => undefined} />
      </>
    ));
    const [invite, subs] = all("button");
    expect(nameOf(invite!)).toBe("Invite people");
    expect(nameOf(subs!)).toBe("Subscribers 12");
  });

  it("names a contact's checkbox by the contact's name", () => {
    renderUi(() => <PickRow label="Anna" checked={false} onToggle={() => undefined} />);
    expect(nameOf(one("input[type=checkbox]"))).toBe("Anna");
  });

  it("names a choice by its title and describes it by its line", () => {
    renderUi(() => (
      <>
        <ChoiceRow type="radio" name="m" title="Nobody" sub="No one finds you." checked onChange={() => undefined} />
        <ChoiceRow type="checkbox" title="Contacts of contacts" checked={false} onChange={() => undefined} />
      </>
    ));
    const [radio, box] = all("input");
    expect(nameOf(radio!)).toBe("Nobody");
    expect(document.getElementById(radio!.getAttribute("aria-describedby") ?? "")?.textContent).toBe("No one finds you.");
    expect(nameOf(box!)).toBe("Contacts of contacts");
    expect(box!.hasAttribute("aria-describedby")).toBe(false);
  });

  it("gives every row its own ids", () => {
    renderUi(() => (
      <>
        <PickRow label="A" checked={false} onToggle={() => undefined} />
        <PickRow label="B" checked={false} onToggle={() => undefined} />
      </>
    ));
    const names = all("input").map(nameOf);
    expect(names).toEqual(["A", "B"]);
  });
});
