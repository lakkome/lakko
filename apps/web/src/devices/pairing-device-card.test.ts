// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * docs/ux/second-device.md section 2.1 and section 3 (2026-10-29): the
 * device being added is named on the signed-in device.
 *
 *   - the new device's pairing payload carries the card it would send;
 *   - its place (2026-10-29): one locate request when the code screen
 *     opens, signed in or not; the payload waits for it at most
 *     `PAIRING_CITY_WAIT_MS` and goes without a place after that; the code
 *     route takes an answer that arrived by the time the code is typed; the
 *     city switch holds for a signed-in device, and a fresh cached city is
 *     used with no request;
 *   - the confirm title and the result lines name it, or keep the unnamed
 *     copy when the payload carried no card;
 *   - the signed-in device keeps the payload's card for the new leaf until
 *     the device's own card arrives and replaces it;
 *   - this device's own row is built from its own fields, never "Device";
 *   - a device that has just joined sends its card at once.
 */
import { afterEach, beforeAll, beforeEach, describe, expect, it, vi } from "vitest";
import { readFileSync } from "node:fs";
import { fileURLToPath } from "node:url";

import init, * as wasm from "../../wasm/lakko_ffi_wasm";
import {
  registerServiceEnvelopeSender,
  resetServiceEnvelopeOutbox,
  retryDirtyServiceEnvelopes,
  setServiceEnvelopeComposer,
} from "../reach/service-envelope-outbox";
import { MemoryStorage } from "../test/fake-storage";
import {
  PAIRING_CITY_WAIT_MS,
  adoptPairedDeviceCard,
  bindDeviceCards,
  composePairingCardEnvelope,
  currentOwnFields,
  pairingCardEnvelopeNow,
  startPairingLocate,
  deviceCards,
  ownDeviceCard,
  receiveOwnDeviceCard,
  sendOwnCardAfterJoin,
  setShowCityToOwnDevices,
  unbindDeviceCards,
} from "./device-card-runtime";
import {
  CARD_GROWTH_MAX_DELAY_MS,
  adoptPairingCard,
  cityStorageKey,
  deviceRowView,
  pairingCardName,
  pairingCardView,
  selfRowCard,
  storedCardOf,
  type RowWords,
  type StoredDeviceCard,
} from "./device-cards";
import { LINK_DEVICE_ADDED, linkDeviceAdded } from "./device-link";
import { buildPairingQrPayloadWith, parsePairingQrPayloadWith } from "./pairing";
import { pairingConfirmTitle, pairingScanDone } from "./pairing-copy";

const ACCOUNT = "YWNjb3VudC0wMDAwMDAwMDA=";
const HANDLE = new Uint8Array(32).fill(0x31);
const RENDEZVOUS = new Uint8Array(32).fill(0x52);

const WORDS: RowWords = {
  device: "Device",
  online: "online",
  app: (v) => (v === "" ? "Lakko" : `Lakko ${v}`),
  place: (city, country) => `${city}, ${country}`,
  countryName: (code) => (code === "NL" ? "Netherlands" : code),
};

const CHROME: StoredDeviceCard = {
  platform: "web",
  model: "Chrome on macOS",
  appVersion: "web",
  os: "macOS 26",
  city: "Almere",
  country: "NL",
  updatedAtMs: 1_000,
  lastActiveMs: 1_000,
};

let unregister: (() => void) | null = null;

function keyPackage(): Uint8Array {
  const identity = wasm.Identity.fromSeed(new Uint8Array(32).fill(9));
  try {
    return identity.createKeyPackage();
  } finally {
    identity.free();
  }
}

beforeAll(async () => {
  const wasmPath = fileURLToPath(new URL("../../wasm/lakko_ffi_wasm_bg.wasm", import.meta.url));
  await init({ module_or_path: readFileSync(wasmPath) });
});

beforeEach(() => {
  const g = globalThis as unknown as Record<string, unknown>;
  g["localStorage"] = new MemoryStorage();
  g["document"] = { visibilityState: "visible" };
  // No city: a locate call fails, the card goes without one.
  g["fetch"] = vi.fn(async () => {
    throw new Error("offline");
  });
  vi.spyOn(console, "info").mockImplementation(() => undefined);
  vi.spyOn(console, "warn").mockImplementation(() => undefined);
});

afterEach(() => {
  unregister?.();
  unregister = null;
  unbindDeviceCards();
  resetServiceEnvelopeOutbox();
  vi.useRealTimers();
  vi.restoreAllMocks();
  const g = globalThis as unknown as Record<string, unknown>;
  delete g["localStorage"];
  delete g["document"];
  delete g["fetch"];
});

describe("the new device's pairing payload", () => {
  it("carries the card the device would send", async () => {
    bindDeviceCards(ACCOUNT);
    setShowCityToOwnDevices(false);
    const envelope = await composePairingCardEnvelope(startPairingLocate(), 5_000);
    expect(envelope).not.toBeNull();
    const payload = buildPairingQrPayloadWith(wasm, HANDLE, keyPackage(), RENDEZVOUS, envelope);
    const decoded = parsePairingQrPayloadWith(wasm, payload, HANDLE);
    const fields = currentOwnFields();
    expect(decoded.deviceCard).toEqual({ ...fields, updatedAtMs: 5_000, lastActiveMs: 5_000 });
  });

  it("decodes with no card when the payload carried none", () => {
    const payload = buildPairingQrPayloadWith(wasm, HANDLE, keyPackage(), RENDEZVOUS);
    expect(parsePairingQrPayloadWith(wasm, payload, HANDLE).deviceCard).toBeNull();
  });

  it("passes no card argument to the core when there is none", () => {
    const calls: number[] = [];
    const core = {
      encodePairingQr: (...args: unknown[]): Uint8Array => {
        calls.push(args.length);
        return new Uint8Array([1]);
      },
      decodePairingQr: () => {
        throw new Error("unused");
      },
    };
    buildPairingQrPayloadWith(core, HANDLE, new Uint8Array(1), RENDEZVOUS);
    buildPairingQrPayloadWith(core, HANDLE, new Uint8Array(1), RENDEZVOUS, new Uint8Array([7]));
    expect(calls).toEqual([3, 4]);
  });
});


/** A locate route that answers only when told to. */
function delayedLocate(): {
  readonly urls: string[];
  answer(city: string, country: string): void;
} {
  const urls: string[] = [];
  const waiting: Array<(r: Response) => void> = [];
  (globalThis as unknown as Record<string, unknown>)["fetch"] = vi.fn(async (input: RequestInfo | URL) => {
    urls.push(String(input));
    return new Promise<Response>((resolve) => waiting.push(resolve));
  });
  return {
    urls,
    answer(city, country) {
      for (const resolve of waiting.splice(0)) {
        resolve(new Response(JSON.stringify({ city, country }), { status: 200 }));
      }
    },
  };
}

function cardOf(envelope: Uint8Array | null): StoredDeviceCard | null {
  const payload = buildPairingQrPayloadWith(wasm, HANDLE, keyPackage(), RENDEZVOUS, envelope);
  return parsePairingQrPayloadWith(wasm, payload, HANDLE).deviceCard;
}

const LOCATE_URL = "/api/discovery/discovery/locate";

describe("the place in the new device's payload card", () => {
  it("waits at most 1.5 s", () => {
    expect(PAIRING_CITY_WAIT_MS).toBe(1_500);
  });

  it("a device not signed in asks once and carries an answer that arrives within the bound", async () => {
    const locate = delayedLocate();
    const place = startPairingLocate({ waitMs: 60_000 });
    const composed = composePairingCardEnvelope(place, 5_000);
    locate.answer("Almere", "NL");
    const card = cardOf(await composed);
    expect(card?.city).toBe("Almere");
    expect(card?.country).toBe("NL");
    expect(locate.urls).toEqual([LOCATE_URL]);
    // Held for this pairing only: nothing of it is stored.
    expect((globalThis as unknown as { localStorage: MemoryStorage }).localStorage.length).toBe(0);
  });

  it("an answer after the bound leaves the payload card without a place; the code route takes it", async () => {
    const locate = delayedLocate();
    const place = startPairingLocate({ waitMs: 20 });
    const card = cardOf(await composePairingCardEnvelope(place, 5_000));
    expect(card?.city).toBe("");
    expect(card?.country).toBe("");
    locate.answer("Almere", "NL");
    await vi.waitFor(() => expect(place.current()).not.toBeNull());
    // The code is typed after this: the payload sealed then carries it.
    const sealed = cardOf(await pairingCardEnvelopeNow(place, 6_000));
    expect(sealed?.city).toBe("Almere");
    expect(locate.urls).toEqual([LOCATE_URL]);
  });

  it("a failed request leaves the card without a place", async () => {
    const fetchMock = vi.fn(async () => {
      throw new Error("offline");
    });
    (globalThis as unknown as Record<string, unknown>)["fetch"] = fetchMock;
    const card = cardOf(await composePairingCardEnvelope(startPairingLocate({ waitMs: 60_000 }), 5_000));
    expect(card?.platform).toBe("web");
    expect(card?.city).toBe("");
    expect(fetchMock).toHaveBeenCalledTimes(1);
  });

  it("the city switch off on a signed-in device: no request and no place", async () => {
    bindDeviceCards(ACCOUNT);
    setShowCityToOwnDevices(false);
    const locate = delayedLocate();
    const card = cardOf(await composePairingCardEnvelope(startPairingLocate({ waitMs: 60_000 }), 5_000));
    expect(card?.city).toBe("");
    expect(locate.urls).toEqual([]);
  });

  it("a signed-in device with a fresh cached city uses it and asks nothing", async () => {
    localStorage.setItem(
      cityStorageKey(ACCOUNT),
      JSON.stringify({ city: "Utrecht", country: "NL", fetchedAtMs: Date.now() }),
    );
    bindDeviceCards(ACCOUNT);
    const locate = delayedLocate();
    const card = cardOf(await composePairingCardEnvelope(startPairingLocate({ waitMs: 60_000 }), 5_000));
    expect(card?.city).toBe("Utrecht");
    expect(locate.urls).toEqual([]);
  });

  it("a signed-in device whose cached city is stale and gets no answer in time carries the cached city", async () => {
    localStorage.setItem(
      cityStorageKey(ACCOUNT),
      JSON.stringify({ city: "Utrecht", country: "NL", fetchedAtMs: Date.now() - 7 * 60 * 60 * 1000 }),
    );
    bindDeviceCards(ACCOUNT);
    const locate = delayedLocate();
    const card = cardOf(await composePairingCardEnvelope(startPairingLocate({ waitMs: 20 }), 5_000));
    // What this device's own card already states, not no place.
    expect(card?.city).toBe("Utrecht");
    expect(card?.country).toBe("NL");
    expect(locate.urls).toEqual([LOCATE_URL]);
  });

  it("a signed-in device with no fresh city asks once and keeps the answer in its city cache", async () => {
    bindDeviceCards(ACCOUNT);
    const locate = delayedLocate();
    const place = startPairingLocate({ waitMs: 60_000 });
    const composed = composePairingCardEnvelope(place, 5_000);
    locate.answer("Almere", "NL");
    expect(cardOf(await composed)?.city).toBe("Almere");
    expect(locate.urls).toEqual([LOCATE_URL]);
    const cached = JSON.parse(localStorage.getItem(cityStorageKey(ACCOUNT)) ?? "null") as {
      city: string;
    } | null;
    expect(cached?.city).toBe("Almere");
  });
});

describe("the confirm and result lines", () => {
  it("name the device from its card, with the row's other lines", () => {
    const view = pairingCardView(CHROME, WORDS);
    expect(view).toEqual({
      name: "Chrome on macOS",
      detail: "Lakko web · macOS 26",
      place: "Almere, Netherlands",
    });
    expect(pairingConfirmTitle(view.name)).toBe("Link “Chrome on macOS”?");
    expect(pairingScanDone(view.name)).toBe(
      "“Chrome on macOS” added. It will appear in the list once it has finished joining.",
    );
    expect(linkDeviceAdded(view.name)()).toBe("“Chrome on macOS” added.");
  });

  it("keep the unnamed copy with no card or a card with no name", () => {
    expect(pairingCardView(null, WORDS)).toEqual({ name: null, detail: "", place: "" });
    expect(pairingCardName({ ...CHROME, model: "  " })).toBeNull();
    expect(pairingConfirmTitle(null)).toBe("Link this device?");
    expect(pairingScanDone(null)).toBe(
      "Device added. It will appear in the list once it has finished joining.",
    );
    expect(linkDeviceAdded(null)).toBe(LINK_DEVICE_ADDED);
    expect(linkDeviceAdded(null)()).toBe("Device added.");
  });

  it("reads a decoded card only for a platform this client knows", () => {
    const like = { ...CHROME, updatedAtMs: 2n, lastActiveMs: 3n };
    expect(storedCardOf(like)).toEqual({ ...CHROME, updatedAtMs: 2, lastActiveMs: 3 });
    expect(storedCardOf({ ...like, platform: "toaster" })).toBeNull();
  });
});

describe("the new device's row on the device that let it in", () => {
  const KEY = "ab".repeat(32);

  it("keeps the payload's card, stamped lowest, only when none is stored", () => {
    const adopted = adoptPairingCard({}, KEY, CHROME);
    expect(adopted?.[KEY]).toEqual({ ...CHROME, updatedAtMs: 0 });
    expect(adoptPairingCard({ [KEY]: CHROME }, KEY, { ...CHROME, model: "Other" })).toBeNull();
  });

  it("is replaced by the device's own card when it arrives", () => {
    bindDeviceCards(ACCOUNT);
    expect(adoptPairedDeviceCard(KEY, CHROME)).toBe(true);
    expect(deviceCards()[KEY]?.model).toBe("Chrome on macOS");
    const own: StoredDeviceCard = { ...CHROME, model: "Chrome on Linux", updatedAtMs: 1 };
    expect(receiveOwnDeviceCard(KEY, own, [KEY])).toBe(true);
    expect(deviceCards()[KEY]).toEqual(own);
    // A later payload card never overwrites a card the device sent.
    expect(adoptPairedDeviceCard(KEY, CHROME)).toBe(false);
    expect(deviceCards()[KEY]).toEqual(own);
  });
});

describe("this device's own row", () => {
  it("is built from its own fields, never 'Device'", () => {
    const fields = { ...CHROME };
    const card = selfRowCard(null, fields, 9_000);
    const view = deviceRowView({ keyHex: "x", leafIndex: 0, isSelf: true, card }, WORDS, 9_000);
    expect(view.title).toBe("Chrome on macOS");
    expect(view.detail).toBe("Lakko web · macOS 26");
    expect(view.place).toBe("Almere, Netherlands");
    expect(view.activity).toBe("online");
  });

  it("follows the current fields over a stale own card", () => {
    const stale: StoredDeviceCard = { ...CHROME, os: "macOS 25", updatedAtMs: 7 };
    const card = selfRowCard(stale, CHROME, 9_000);
    expect(card.os).toBe("macOS 26");
    expect(card.updatedAtMs).toBe(7);
  });
});

describe("a device that has just joined", () => {
  it("sends its own card at once, not after the growth delay", async () => {
    bindDeviceCards(ACCOUNT);
    setServiceEnvelopeComposer("deviceCard", async () => (ownDeviceCard() === null ? null : new Uint8Array([1])));
    const sent: Uint8Array[] = [];
    unregister = registerServiceEnvelopeSender(async (envelope) => {
      sent.push(envelope);
    });
    vi.useFakeTimers();
    await sendOwnCardAfterJoin(ACCOUNT, 50_000);
    await retryDirtyServiceEnvelopes();
    // No timer was advanced: nothing waited for the growth delay.
    expect(CARD_GROWTH_MAX_DELAY_MS).toBeGreaterThan(0);
    expect(sent).toHaveLength(1);
    expect(ownDeviceCard()?.updatedAtMs).toBe(50_000);
  });

  it("sends a card strictly newer than one it sent before", async () => {
    bindDeviceCards(ACCOUNT);
    await sendOwnCardAfterJoin(ACCOUNT, 50_000);
    await sendOwnCardAfterJoin(ACCOUNT, 40_000);
    expect(ownDeviceCard()?.updatedAtMs).toBe(50_001);
  });

  it("joined before the runtime was bound: sends when it binds", async () => {
    await sendOwnCardAfterJoin(ACCOUNT, 60_000);
    expect(ownDeviceCard()).toBeNull();
    const sent: Uint8Array[] = [];
    unregister = registerServiceEnvelopeSender(async (envelope) => {
      sent.push(envelope);
    });
    bindDeviceCards(ACCOUNT);
    setServiceEnvelopeComposer("deviceCard", async () => (ownDeviceCard() === null ? null : new Uint8Array([2])));
    await vi.waitFor(() => expect(ownDeviceCard()).not.toBeNull());
    await retryDirtyServiceEnvelopes();
    expect(sent.length).toBeGreaterThanOrEqual(1);
  });

  it("a join pending for another account is dropped on bind", async () => {
    await sendOwnCardAfterJoin("b3RoZXI=", 60_000);
    bindDeviceCards(ACCOUNT);
    await Promise.resolve();
    expect(ownDeviceCard()).toBeNull();
  });
});
