// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * `docs/ux/who-can-find-me.md` sections 4.1 and 8: what a tap on the contacts
 * part does to the keys. The rules are the REAL wasm; the store and the reach
 * session are stood in for, so what is under test is which key each tap moves.
 */
import { readFileSync } from "node:fs";
import { fileURLToPath } from "node:url";

import { beforeAll, beforeEach, describe, expect, it, vi } from "vitest";

import init, * as wasm from "../../wasm/lakko_ffi_wasm";

const { session, setting } = vi.hoisted(() => ({
  session: {
    rotateOnContactsNarrowed: vi.fn(async () => undefined),
    rotateOnSettingChange: vi.fn(async (_narrowed: boolean) => undefined),
  },
  setting: { current: null as unknown },
}));

vi.mock("../wasm", () => ({ loadWasm: async () => wasm }));
vi.mock("../reach/reach-service", () => ({ reachSession: () => session }));
vi.mock("../reach/visibility-store", () => ({
  visibilitySetting: () => setting.current,
  updateVisibility: async (transition: (s: unknown) => unknown) => {
    setting.current = transition(setting.current);
    return setting.current;
  },
}));

import { accountIdsHexOf } from "./contact-visibility";
import * as actions from "./visibility-actions";

const ANNA = "01".repeat(16);
const BOB = "02".repeat(16);
const held = (): wasm.VisibilitySetting => setting.current as wasm.VisibilitySetting;
const allows = (hex: string): boolean => actions.allowsContact(wasm as never, hex);

beforeAll(async () => {
  const wasmPath = fileURLToPath(new URL("../../wasm/lakko_ffi_wasm_bg.wasm", import.meta.url));
  await init({ module_or_path: readFileSync(wasmPath) });
});

beforeEach(() => {
  setting.current = wasm.defaultVisibilitySetting();
  session.rotateOnContactsNarrowed.mockClear();
  session.rotateOnSettingChange.mockClear();
});

describe("the contacts part moves the fof key, and nothing else", () => {
  it("moving from All to Only the ones I choose moves it at once", async () => {
    await actions.setContactsForm("chosen");
    expect(session.rotateOnContactsNarrowed).toHaveBeenCalledTimes(1);
    expect(session.rotateOnSettingChange).not.toHaveBeenCalled();
  });

  it("excluding one contact under All keeps the others on and moves it", async () => {
    await actions.excludeContactUnderAll([ANNA, BOB], BOB);
    expect(allows(ANNA)).toBe(true);
    expect(allows(BOB)).toBe(false);
    expect(session.rotateOnContactsNarrowed).toHaveBeenCalledTimes(1);
  });

  it("switching a contact off moves it, switching one on does not", async () => {
    await actions.setContactsForm("chosen");
    await actions.setContactChosen(ANNA, true);
    session.rotateOnContactsNarrowed.mockClear();
    expect(allows(ANNA)).toBe(true);
    await actions.setContactChosen(BOB, true);
    expect(session.rotateOnContactsNarrowed).not.toHaveBeenCalled();
    await actions.setContactChosen(BOB, false);
    expect(allows(BOB)).toBe(false);
    expect(session.rotateOnContactsNarrowed).toHaveBeenCalledTimes(1);
  });

  it("widening back to All needs no new key", async () => {
    await actions.setContactsForm("chosen");
    session.rotateOnContactsNarrowed.mockClear();
    await actions.setContactsForm("all");
    expect(session.rotateOnContactsNarrowed).not.toHaveBeenCalled();
  });
});

describe("a change of the whole setting", () => {
  it("rotates the group and channel keys, and fof only when it narrowed contacts", async () => {
    await actions.setContactsForm("chosen");
    await actions.chooseMode("everyone");
    expect(session.rotateOnSettingChange).toHaveBeenLastCalledWith(false);
    await actions.chooseMode("some");
    expect(session.rotateOnSettingChange).toHaveBeenLastCalledWith(true);
  });
});

describe("a removed contact (UX section 4.1)", () => {
  it("leaves the chosen list, and the removal itself moved the keys", async () => {
    await actions.setContactsForm("chosen");
    await actions.setContactChosen(ANNA, true);
    session.rotateOnContactsNarrowed.mockClear();
    await actions.forgetChosenContact(ANNA);
    expect(accountIdsHexOf(held().chosenContacts)).toEqual([]);
    expect(session.rotateOnContactsNarrowed).not.toHaveBeenCalled();
  });

  it("changes nothing for someone who was not on the list", async () => {
    const before = held();
    await actions.forgetChosenContact(BOB);
    expect(held()).toBe(before);
  });
});
