// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * ADR-0080 device cards: the receiver's store rules, the send rule, the
 * Devices screen's order and row text, the browser facts, and the intake's
 * sender resolution, with the real wasm codec for the round trip.
 */
import { beforeAll, describe, expect, it } from "vitest";
import { readFileSync } from "node:fs";
import { fileURLToPath } from "node:url";

import init, * as wasm from "../../wasm/lakko_ffi_wasm";
import {
  CARD_REFRESH_MS,
  browserFacts,
  cardGrowthDelayMs,
  deviceRowView,
  orderDeviceRows,
  pruneDeviceCards,
  receiveDeviceCard,
  shouldSendOwnCard,
  type DeviceRowInput,
  type RowWords,
  type StoredDeviceCard,
} from "./device-cards";
import { decodeDeviceCardEnvelope, senderKeyAtReceipt } from "./device-card-intake";

beforeAll(async () => {
  const wasmPath = fileURLToPath(new URL("../../wasm/lakko_ffi_wasm_bg.wasm", import.meta.url));
  await init({ module_or_path: readFileSync(wasmPath) });
});

const NOW = new Date(2026, 9, 14, 15, 0, 0).getTime(); // a Wednesday afternoon, local time

function card(over: Partial<StoredDeviceCard> = {}): StoredDeviceCard {
  return {
    platform: "ios",
    model: "iPhone 16 Pro",
    appVersion: "1.4",
    os: "iOS 27.0.1",
    city: "Almere",
    country: "NL",
    updatedAtMs: NOW - 1000,
    lastActiveMs: NOW - 1000,
    ...over,
  };
}

describe("receiving a card", () => {
  it("keeps a card from a leaf in the roster and refuses one that is not", () => {
    const next = receiveDeviceCard({}, "aa", card(), ["aa", "bb"]);
    expect(next).toEqual({ aa: card() });
    expect(receiveDeviceCard({}, "cc", card(), ["aa", "bb"])).toBeNull();
  });

  it("replaces only on a strictly newer stamp", () => {
    const stored = { aa: card({ updatedAtMs: 10 }) };
    expect(receiveDeviceCard(stored, "aa", card({ updatedAtMs: 10, model: "X" }), ["aa"])).toBeNull();
    expect(receiveDeviceCard(stored, "aa", card({ updatedAtMs: 9 }), ["aa"])).toBeNull();
    expect(receiveDeviceCard(stored, "aa", card({ updatedAtMs: 11, model: "X" }), ["aa"])?.aa?.model).toBe("X");
  });

  it("drops the card of a leaf that left", () => {
    const stored = { aa: card(), bb: card() };
    expect(pruneDeviceCards(stored, ["aa"])).toEqual({ aa: card() });
    expect(pruneDeviceCards(stored, ["aa", "bb"])).toBeNull();
  });
});

describe("sending the own card", () => {
  const fields = card();
  it("sends with no card yet, on a field change, and when older than 30 minutes", () => {
    expect(shouldSendOwnCard(null, fields, NOW)).toBe(true);
    const last = card({ updatedAtMs: NOW - 60_000 });
    expect(shouldSendOwnCard(last, fields, NOW)).toBe(false);
    expect(shouldSendOwnCard(last, { ...fields, city: "Utrecht" }, NOW)).toBe(true);
    expect(shouldSendOwnCard(last, { ...fields, appVersion: "1.5" }, NOW)).toBe(true);
    expect(shouldSendOwnCard(card({ updatedAtMs: NOW - CARD_REFRESH_MS + 1 }), fields, NOW)).toBe(false);
    expect(shouldSendOwnCard(card({ updatedAtMs: NOW - CARD_REFRESH_MS }), fields, NOW)).toBe(true);
  });

  it("treats a stamp from the future as old, so a wrong clock never silences it", () => {
    expect(shouldSendOwnCard(card({ updatedAtMs: NOW + 5_000 }), fields, NOW)).toBe(true);
  });

  it("waits up to 300 ms before the growth re-send", () => {
    expect(cardGrowthDelayMs(() => 0)).toBe(0);
    expect(cardGrowthDelayMs(() => 0.5)).toBe(150);
    expect(cardGrowthDelayMs(() => 1)).toBeLessThan(300);
    expect(cardGrowthDelayMs(() => Number.NaN)).toBe(0);
  });
});

const words: RowWords = {
  device: "Device",
  online: "online",
  app: (v) => (v === "" ? "Lakko" : `Lakko ${v}`),
  place: (c, k) => `${c}, ${k}`,
  countryName: (code) => (code === "NL" ? "Netherlands" : code),
};

function row(over: Partial<DeviceRowInput> = {}): DeviceRowInput {
  return { keyHex: "aa", leafIndex: 0, isSelf: false, card: card(), ...over };
}

describe("the Devices screen", () => {
  it("puts this device on top and the others newest activity first, cardless last", () => {
    const rows = [
      row({ keyHex: "old", leafIndex: 1, card: card({ lastActiveMs: 100 }) }),
      row({ keyHex: "none", leafIndex: 2, card: null }),
      row({ keyHex: "self", leafIndex: 3, isSelf: true }),
      row({ keyHex: "new", leafIndex: 4, card: card({ lastActiveMs: 200 }) }),
      row({ keyHex: "none0", leafIndex: 0, card: null }),
    ];
    const { self, others } = orderDeviceRows(rows);
    expect(self?.keyHex).toBe("self");
    expect(others.map((r) => r.keyHex)).toEqual(["new", "old", "none0", "none"]);
  });

  it("reads a row like other messengers' active sessions", () => {
    const v = deviceRowView(row(), words, NOW, "en-GB");
    expect(v.title).toBe("iPhone 16 Pro");
    expect(v.detail).toBe("Lakko 1.4 · iOS 27.0.1");
    expect(v.place).toBe("Almere, Netherlands");
    expect(v.icon).toBe("phone");
    expect(v.activity).toBe("14:59");
  });

  it("says online for this device", () => {
    expect(deviceRowView(row({ isSelf: true }), words, NOW, "en-GB").activity).toBe("online");
  });

  it("shows a weekday within the week and a date beyond it", () => {
    const threeDays = deviceRowView(row({ card: card({ lastActiveMs: NOW - 3 * 86_400_000 }) }), words, NOW, "en-GB");
    expect(threeDays.activity).toBe("Sun");
    const old = deviceRowView(row({ card: card({ lastActiveMs: NOW - 30 * 86_400_000 }) }), words, NOW, "en-GB");
    expect(old.activity).toBe("14 Sept");
  });

  it("leaves out what is not known", () => {
    const noPlace = deviceRowView(row({ card: card({ city: "", country: "" }) }), words, NOW, "en-GB");
    expect(noPlace.place).toBe("");
    const countryOnly = deviceRowView(row({ card: card({ city: "" }) }), words, NOW, "en-GB");
    expect(countryOnly.place).toBe("Netherlands");
    const web = deviceRowView(
      row({ card: card({ platform: "web", model: "Chrome on macOS", appVersion: "web", os: "" }) }),
      words,
      NOW,
      "en-GB",
    );
    expect(web.detail).toBe("Lakko web");
    expect(web.icon).toBe("browser");
  });

  it("shows Device for a device whose card has not arrived", () => {
    const v = deviceRowView(row({ card: null }), words, NOW, "en-GB");
    expect(v).toEqual({ title: "Device", detail: "", place: "", activity: "", icon: "unknown" });
  });
});

describe("browser facts", () => {
  it("names the browser and the system", () => {
    const mac =
      "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/130.0.0.0 Safari/537.36";
    expect(browserFacts(mac)).toEqual({ model: "Chrome on macOS", os: "macOS" });
    const firefox = "Mozilla/5.0 (X11; Linux x86_64; rv:131.0) Gecko/20100101 Firefox/131.0";
    expect(browserFacts(firefox)).toEqual({ model: "Firefox on Linux", os: "Linux" });
    const safari =
      "Mozilla/5.0 (iPhone; CPU iPhone OS 18_1 like Mac OS X) AppleWebKit/605.1.15 (KHTML, like Gecko) Version/18.1 Mobile/15E148 Safari/604.1";
    expect(browserFacts(safari)).toEqual({ model: "Safari on iOS", os: "iOS 18.1" });
    const android =
      "Mozilla/5.0 (Linux; Android 15; Pixel 7a) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/130.0.0.0 Mobile Safari/537.36";
    expect(browserFacts(android)).toEqual({ model: "Chrome on Android", os: "Android 15" });
    const edge =
      "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/130.0.0.0 Safari/537.36 Edg/130.0.0.0";
    expect(browserFacts(edge)).toEqual({ model: "Edge on Windows", os: "Windows" });
    expect(browserFacts("")).toEqual({ model: "Browser", os: "" });
  });
});

describe("intake", () => {
  it("resolves the sender at receipt and never from a leaf the roster does not hold", () => {
    const roster = [
      { leaf: 0, key: "aa" },
      { leaf: 2, key: "cc" },
    ];
    expect(senderKeyAtReceipt(2, roster)).toBe("cc");
    expect(senderKeyAtReceipt(1, roster)).toBeNull();
    expect(senderKeyAtReceipt(undefined, roster)).toBeNull();
    expect(senderKeyAtReceipt(0, null)).toBeNull();
  });

  it("round-trips a card through the core codec and ignores other envelopes", () => {
    const built = new wasm.DeviceCard(10n, 9n, "web", " Chrome on macOS\n", "web", "macOS", "Almere", "nl");
    const bytes = wasm.composeDeviceCardEnvelope(built);
    built.free();
    expect(decodeDeviceCardEnvelope(wasm, bytes)).toEqual({
      platform: "web",
      model: "Chrome on macOS",
      appVersion: "web",
      os: "macOS",
      city: "Almere",
      country: "NL",
      updatedAtMs: 10,
      lastActiveMs: 9,
    });
    expect(decodeDeviceCardEnvelope(wasm, wasm.composeCommitterBeacon())).toBeNull();
    const env = wasm.dispatchIncomingEnvelope(bytes);
    expect(env.kind).toBe(wasm.IncomingEnvelopeKind.DeviceCard);
    env.free();
  });
});
