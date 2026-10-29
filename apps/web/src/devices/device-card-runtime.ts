// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * Device cards (ADR-0080): the stateful half on the web. Holds the cards the
 * account's other devices sent, this device's own last card, the city answer
 * and the "Show my city to my other devices" switch; decides on the person's
 * actions whether to send a fresh own card; re-sends it when the DeviceGroup
 * grows. The rules themselves are `device-cards.ts`.
 *
 * Delivery (live run 2026-10-28, a card that never arrived):
 *   * the own card is stored before it is published, so the row also keeps
 *     `ownUnsent` until the outbox reports it sent; a reload re-marks it,
 *     and a failed publish is retried on every DeviceGroup poll pass
 *     (`retryDirtyServiceEnvelopes`);
 *   * a growth re-send whose timer fires while the page is hidden is kept
 *     owed (`resendOwed`) and sent at the next foreground, never from a
 *     background timer.
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
  adoptPairingCard,
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
  setServiceEnvelopeSentListener,
} from "../reach/service-envelope-outbox";

/** The city is asked for again at most this often (chosen, not measured). */
export const CITY_REFRESH_MS = 6 * 60 * 60 * 1000;

/** What the web client puts in `app_version`: the row reads "Lakko web". */
export const WEB_APP_VERSION = "web";

/** How long building a pairing payload, or the first card after a join,
 *  waits for the city before going without it (chosen, not measured). */
export const PAIRING_CITY_WAIT_MS = 2_000;


interface PersistedCards {
  readonly cards: DeviceCardMap;
  readonly own: StoredDeviceCard | null;
  /** The own card was stored and has not been sent yet. */
  readonly ownUnsent?: boolean;
  /** A growth re-send fell due while the page was hidden. */
  readonly resendOwed?: boolean;
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
let ownUnsent = false;
let resendOwed = false;
/** docs/ux/second-device.md section 3: this device joined the DeviceGroup of
 *  this account before the runtime was bound to it (the restore routes); its
 *  card goes as soon as it is. */
let joinedPendingFor: string | null = null;

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
  const row: PersistedCards = {
    cards: cards(),
    own: ownCard(),
    ...(ownUnsent ? { ownUnsent: true } : {}),
    ...(resendOwed ? { resendOwed: true } : {}),
  };
  writeJson(cardsStorageKey(account), row);
}

/** Hand the own card to the outbox and remember it is not sent yet. */
function markOwnCardDirty(): void {
  if (ownCard() === null) return;
  ownUnsent = true;
  persistCards();
  markServiceEnvelopeDirty("deviceCard");
}

/** The outbox sent the own card. */
function ownCardSent(): void {
  // eslint-disable-next-line no-console
  console.info("[device-card] own card sent");
  if (!ownUnsent) return;
  ownUnsent = false;
  persistCards();
}

/** Bind the runtime to an account (on sign-in) and register the composer. */
export function bindDeviceCards(accountIdB64: string): void {
  if (account === accountIdB64) return;
  const joined = joinedPendingFor;
  joinedPendingFor = null;
  unbindDeviceCards();
  account = accountIdB64;
  const row = readJson<PersistedCards>(cardsStorageKey(accountIdB64));
  setCards(row?.cards ?? {});
  setOwnCard(row?.own ?? null);
  ownUnsent = row?.ownUnsent === true;
  resendOwed = row?.resendOwed === true;
  city = readJson<CityAnswer>(cityStorageKey(accountIdB64));
  let shown = true;
  try {
    shown = localStorage.getItem(showCityStorageKey(accountIdB64)) !== "0";
  } catch {
    shown = true;
  }
  setShowCitySignal(shown);
  setServiceEnvelopeComposer("deviceCard", composeOwnCardEnvelope);
  setServiceEnvelopeSentListener("deviceCard", ownCardSent);
  // A card stored and not sent before the page went (a failed publish, a
  // reload): marked again, the outbox sends it once a sender is there.
  if (ownUnsent && row?.own != null) markServiceEnvelopeDirty("deviceCard");
  // A growth re-send that fell due while hidden waits for a foreground.
  if (resendOwed) noteDeviceForeground();
  // Joined on a restore route before this bind: the card goes now.
  if (joined === accountIdB64) void sendOwnCardNow(accountIdB64, Date.now());
}

/** Sign-out: nothing of one account is used for the next. */
export function unbindDeviceCards(): void {
  account = null;
  city = null;
  locating = null;
  if (growthTimer !== null) clearTimeout(growthTimer);
  growthTimer = null;
  ownUnsent = false;
  resendOwed = false;
  setCards({});
  setOwnCard(null);
  setShowCitySignal(true);
  setServiceEnvelopeComposer("deviceCard", null);
  setServiceEnvelopeSentListener("deviceCard", null);
}

async function composeOwnCardEnvelope(): Promise<Uint8Array | null> {
  const own = ownCard();
  if (own === null) return null;
  return composeCardEnvelope(own);
}

async function composeCardEnvelope(own: StoredDeviceCard): Promise<Uint8Array> {
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
  if (bound === null || !visible()) return;
  // An action is a foreground: a growth re-send that waited goes now.
  noteDeviceForeground();
  if (deciding) return;
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
      markOwnCardDirty();
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
): boolean {
  if (account === null) return false;
  const next = receiveDeviceCard(cards(), senderKeyHex, card, rosterKeysHex);
  if (next === null) return false;
  setCards(next);
  persistCards();
  return true;
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
 *  300 ms, so the new device learns this one (ADR-0058 discipline). */
export function scheduleOwnCardResend(random: () => number = Math.random): void {
  if (account === null || ownCard() === null || growthTimer !== null) return;
  const bound = account;
  growthTimer = setTimeout(() => {
    growthTimer = null;
    if (account !== bound) return;
    if (!visible()) {
      // Hidden: kept owed and sent at the next foreground, never from here.
      resendOwed = true;
      persistCards();
      return;
    }
    markOwnCardDirty();
  }, cardGrowthDelayMs(random));
}

/** The page came to the foreground (or the person acted on it): send a
 *  growth re-send that fell due while it was hidden. */
export function noteDeviceForeground(): void {
  if (account === null || !resendOwed || !visible()) return;
  resendOwed = false;
  markOwnCardDirty();
}

/** `p`, or `undefined` once `ms` passed. */
async function within<T>(p: Promise<T>, ms: number): Promise<T | undefined> {
  let timer: ReturnType<typeof setTimeout> | null = null;
  try {
    return await Promise.race([
      p,
      new Promise<undefined>((resolve) => {
        timer = setTimeout(() => resolve(undefined), ms);
      }),
    ]);
  } finally {
    if (timer !== null) clearTimeout(timer);
  }
}

/**
 * docs/ux/second-device.md section 2.1: the fields of the card this device
 * puts in its pairing payload, the same fields it puts in its own card. A
 * device bound to its account asks for its city as its card does (the city
 * switch holds), waiting at most `waitMs`, past which the card goes without
 * one. A device that is not signed in yet (the restore routes) asks for
 * nothing: no request leaves a fresh device before its session (ADR-0056),
 * so its payload card carries no place.
 */
export async function pairingCardFields(
  waitMs: number = PAIRING_CITY_WAIT_MS,
  nowMs: number = Date.now(),
): Promise<DeviceCardFields> {
  const bound = account;
  if (bound !== null) await within(refreshCity(bound, nowMs), waitMs);
  return currentOwnFields();
}

/**
 * The device-card envelope for this device's pairing payload, or `null` when
 * it cannot be composed: a payload without a card is a pairing like any
 * other, so this never throws.
 */
export async function composePairingCardEnvelope(
  waitMs: number = PAIRING_CITY_WAIT_MS,
  nowMs: number = Date.now(),
): Promise<Uint8Array | null> {
  try {
    const fields = await pairingCardFields(waitMs, nowMs);
    return await composeCardEnvelope({ ...fields, updatedAtMs: nowMs, lastActiveMs: nowMs });
  } catch {
    return null;
  }
}

/**
 * The device that let a new one in keeps the card from its pairing payload
 * for the new leaf (`adoptPairingCard`: only when none is stored, stamped
 * lowest so the device's own card replaces it). `false` when nothing changed.
 */
export function adoptPairedDeviceCard(keyHex: string, card: StoredDeviceCard): boolean {
  if (account === null) return false;
  const next = adoptPairingCard(cards(), keyHex, card);
  if (next === null) return false;
  setCards(next);
  persistCards();
  return true;
}

/** A fresh own card, sent now (the city wait is bounded). */
async function sendOwnCardNow(bound: string, nowMs: number): Promise<void> {
  await within(refreshCity(bound, nowMs), PAIRING_CITY_WAIT_MS);
  if (account !== bound) return;
  const last = ownCard();
  // Strictly newer than any card this device sent before, whatever the clock.
  const stamp = Math.max(nowMs, (last?.updatedAtMs ?? 0) + 1);
  setOwnCard({ ...currentOwnFields(), updatedAtMs: stamp, lastActiveMs: nowMs });
  markOwnCardDirty();
}

/**
 * docs/ux/second-device.md section 3: this device has just joined the
 * account's DeviceGroup from a pairing Welcome. Its card goes at once, not
 * after the growth delay (that is for the devices that were already there)
 * and not after the next action: the person is looking at it. When the
 * runtime is not bound to the account yet (a restore route lands the session
 * after the join), the card goes when it is.
 */
export function sendOwnCardAfterJoin(accountIdB64: string, nowMs: number = Date.now()): Promise<void> {
  if (account !== accountIdB64) {
    joinedPendingFor = accountIdB64;
    return Promise.resolve();
  }
  return sendOwnCardNow(accountIdB64, nowMs);
}
