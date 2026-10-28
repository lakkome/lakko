// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * Device cards (ADR-0080): the stateful half on the web. Holds the cards the
 * account's other devices sent, this device's own last card, the city answer
 * and the "Show my city to my other devices" switch; decides on the person's
 * actions whether to send a fresh own card; re-sends it when the DeviceGroup
 * grows. The rules themselves are `device-cards.ts`.
 *
 * Storage: `localStorage`, per account (`wipe-account.ts` deletes the keys).
 * The cards say what the account's own devices are and where; nothing of it
 * leaves this browser except the own card, end-to-end encrypted in the
 * DeviceGroup.
 */

import { createSignal } from "solid-js";
import { loadWasm } from "../wasm";
import { DEFAULT_BASE_URL } from "../discovery/client";
import {
  browserFacts,
  cardGrowthDelayMs,
  cardsStorageKey,
  cityStorageKey,
  showCityStorageKey,
  pruneDeviceCards,
  receiveDeviceCard,
  shouldSendOwnCard,
  type DeviceCardFields,
  type DeviceCardMap,
  type StoredDeviceCard,
} from "./device-cards";
import {
  markServiceEnvelopeDirty,
  setServiceEnvelopeComposer,
} from "../reach/service-envelope-outbox";

/** The city is asked for again at most this often (chosen, not measured). */
export const CITY_REFRESH_MS = 6 * 60 * 60 * 1000;

/** What the web client puts in `app_version`: the row reads "Lakko web". */
export const WEB_APP_VERSION = "web";


interface PersistedCards {
  readonly cards: DeviceCardMap;
  readonly own: StoredDeviceCard | null;
}

interface CityAnswer {
  readonly city: string;
  readonly country: string;
  readonly fetchedAtMs: number;
}

const [cards, setCards] = createSignal<DeviceCardMap>({});
const [ownCard, setOwnCard] = createSignal<StoredDeviceCard | null>(null);
const [showCity, setShowCitySignal] = createSignal(true);

/** Received cards, keyed by leaf key hex. */
export const deviceCards = cards;
/** This device's own last card. */
export const ownDeviceCard = ownCard;
/** The Privacy switch (per device, on by default). */
export const showCityToOwnDevices = showCity;

let account: string | null = null;
let city: CityAnswer | null = null;
let locating: Promise<void> | null = null;
let deciding = false;
let growthTimer: ReturnType<typeof setTimeout> | null = null;

function readJson<T>(key: string): T | null {
  try {
    const raw = localStorage.getItem(key);
    return raw === null ? null : (JSON.parse(raw) as T);
  } catch {
    return null;
  }
}

function writeJson(key: string, value: unknown): void {
  try {
    localStorage.setItem(key, JSON.stringify(value));
  } catch {
    // Storage full or sealed (ADR-0077): the in-memory state stays.
  }
}

function persistCards(): void {
  if (account === null) return;
  const row: PersistedCards = { cards: cards(), own: ownCard() };
  writeJson(cardsStorageKey(account), row);
}

/** Bind the runtime to an account (on sign-in) and register the composer. */
export function bindDeviceCards(accountIdB64: string): void {
  if (account === accountIdB64) return;
  unbindDeviceCards();
  account = accountIdB64;
  const row = readJson<PersistedCards>(cardsStorageKey(accountIdB64));
  setCards(row?.cards ?? {});
  setOwnCard(row?.own ?? null);
  city = readJson<CityAnswer>(cityStorageKey(accountIdB64));
  let shown = true;
  try {
    shown = localStorage.getItem(showCityStorageKey(accountIdB64)) !== "0";
  } catch {
    shown = true;
  }
  setShowCitySignal(shown);
  setServiceEnvelopeComposer("deviceCard", composeOwnCardEnvelope);
}

/** Sign-out: nothing of one account is used for the next. */
export function unbindDeviceCards(): void {
  account = null;
  city = null;
  locating = null;
  if (growthTimer !== null) clearTimeout(growthTimer);
  growthTimer = null;
  setCards({});
  setOwnCard(null);
  setShowCitySignal(true);
  setServiceEnvelopeComposer("deviceCard", null);
}

async function composeOwnCardEnvelope(): Promise<Uint8Array | null> {
  const own = ownCard();
  if (own === null) return null;
  const wasm = await loadWasm();
  const card = new wasm.DeviceCard(
    BigInt(own.updatedAtMs),
    BigInt(own.lastActiveMs),
    own.platform,
    own.model,
    own.appVersion,
    own.os,
    own.city,
    own.country,
  );
  try {
    return wasm.composeDeviceCardEnvelope(card);
  } finally {
    card.free();
  }
}

function visible(): boolean {
  return typeof document === "undefined" || document.visibilityState === "visible";
}

/** The own card's fields as they are now. */
export function currentOwnFields(): DeviceCardFields {
  const facts = browserFacts(typeof navigator === "undefined" ? "" : navigator.userAgent);
  const place = showCity() && city !== null ? city : null;
  return {
    platform: "web",
    model: facts.model,
    appVersion: WEB_APP_VERSION,
    os: facts.os,
    city: place?.city ?? "",
    country: place?.country ?? "",
  };
}

async function refreshCity(bound: string, nowMs: number): Promise<void> {
  if (!showCity()) return;
  if (city !== null && nowMs - city.fetchedAtMs >= 0 && nowMs - city.fetchedAtMs < CITY_REFRESH_MS) return;
  if (locating !== null) return locating;
  locating = (async () => {
    try {
      const res = await fetch(`${DEFAULT_BASE_URL}/discovery/locate`, {
        method: "GET",
        credentials: "omit",
        cache: "no-store",
      });
      if (!res.ok) return;
      const body = (await res.json()) as { city?: unknown; country?: unknown };
      if (account !== bound) return;
      city = {
        city: typeof body.city === "string" ? body.city : "",
        country: typeof body.country === "string" ? body.country : "",
        fetchedAtMs: nowMs,
      };
      writeJson(cityStorageKey(bound), city);
    } catch {
      // No city this time; the card goes out without one.
    } finally {
      locating = null;
    }
  })();
  return locating;
}

/**
 * The person did something on this device (a tap, a key press). Sends a fresh
 * own card when the rules say so (ADR-0080 section 3). Never on a timer and
 * never while the page is hidden. Cheap when nothing is due.
 */
export function noteDeviceAction(nowMs: number = Date.now()): void {
  const bound = account;
  if (bound === null || !visible() || deciding) return;
  const own = ownCard();
  const cityDue =
    showCity() && (city === null || nowMs - city.fetchedAtMs < 0 || nowMs - city.fetchedAtMs >= CITY_REFRESH_MS);
  if (!cityDue && !shouldSendOwnCard(own, currentOwnFields(), nowMs)) return;
  deciding = true;
  void (async () => {
    try {
      await refreshCity(bound, nowMs);
      if (account !== bound) return;
      const fields = currentOwnFields();
      if (!shouldSendOwnCard(ownCard(), fields, nowMs)) return;
      setOwnCard({ ...fields, updatedAtMs: nowMs, lastActiveMs: nowMs });
      persistCards();
      markServiceEnvelopeDirty("deviceCard");
    } finally {
      deciding = false;
    }
  })();
}

/** The Privacy switch. Turning it off empties the city and sends at once. */
export function setShowCityToOwnDevices(on: boolean): void {
  setShowCitySignal(on);
  if (account !== null) {
    try {
      localStorage.setItem(showCityStorageKey(account), on ? "1" : "0");
    } catch {
      // The switch holds for this session.
    }
  }
  noteDeviceAction();
}

/** A card arrived from `senderKeyHex`, resolved at receipt; `rosterKeysHex`
 *  is the DeviceGroup roster at that moment. */
export function receiveOwnDeviceCard(
  senderKeyHex: string,
  card: StoredDeviceCard,
  rosterKeysHex: readonly string[],
): void {
  if (account === null) return;
  const next = receiveDeviceCard(cards(), senderKeyHex, card, rosterKeysHex);
  if (next === null) return;
  setCards(next);
  persistCards();
}

/** The roster changed: drop cards of leaves that left. */
export function pruneOwnDeviceCards(rosterKeysHex: readonly string[]): void {
  if (account === null) return;
  const next = pruneDeviceCards(cards(), rosterKeysHex);
  if (next === null) return;
  setCards(next);
  persistCards();
}

/** The DeviceGroup grew: re-send the stored own card, unchanged, after up to
 *  30 s, so the new device learns this one (ADR-0058 discipline). */
export function scheduleOwnCardResend(random: () => number = Math.random): void {
  if (account === null || ownCard() === null || growthTimer !== null) return;
  const bound = account;
  growthTimer = setTimeout(() => {
    growthTimer = null;
    if (account !== bound || !visible()) return;
    markServiceEnvelopeDirty("deviceCard");
  }, cardGrowthDelayMs(random));
}
