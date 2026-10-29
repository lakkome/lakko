// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * Device cards (ADR-0080, docs/ux/second-device.md section 3): the pure
 * rules. What a device says about itself to the account's other devices,
 * how a receiver keeps what it is told, when a device sends its own, and how
 * a row of the Devices screen reads.
 *
 * The same rules are written in Swift (`DeviceCards.swift`) and Kotlin
 * (`DeviceCards.kt`); the wire format and its bounds are Rust's
 * (`lakko-contacts::device_card`).
 *
 * Pure: clocks, locales and translations are parameters.
 */

import { formatListTime } from "../chat/list-time";

/** `localStorage` keys, per account (`wipe-account.ts` removes them). */
export const cardsStorageKey = (account: string): string => `lakko.device-cards.v1.${account}`;
export const cityStorageKey = (account: string): string => `lakko.device-city.v1.${account}`;
export const showCityStorageKey = (account: string): string => `lakko.device-city-shown.v1.${account}`;

/** The closed set of platforms a card may name (ADR-0080 section 1). */
export type DevicePlatform = "ios" | "ipados" | "macos" | "android" | "web" | "desktop";

const PLATFORMS: readonly DevicePlatform[] = ["ios", "ipados", "macos", "android", "web", "desktop"];

/** `true` for a stable platform name. */
export function isDevicePlatform(name: string): name is DevicePlatform {
  return (PLATFORMS as readonly string[]).includes(name);
}

/** A device card as this client keeps it. Times are UNIX milliseconds. */
export interface DeviceCardFields {
  readonly platform: DevicePlatform;
  readonly model: string;
  readonly appVersion: string;
  readonly os: string;
  readonly city: string;
  readonly country: string;
}

export interface StoredDeviceCard extends DeviceCardFields {
  readonly updatedAtMs: number;
  readonly lastActiveMs: number;
}

/** Received cards, keyed by the sender's DeviceGroup leaf signature key
 *  (lower-case hex), resolved at receipt (C-68). */
export type DeviceCardMap = Readonly<Record<string, StoredDeviceCard>>;

/**
 * Keep a received card. Returns the new map, or `null` when nothing changes:
 * the sender is not in the roster read at receipt (a card names nothing about
 * its sender; a leaf that is not there any more has no row), or the card is
 * not strictly newer than the one stored for that key (equal is not newer,
 * so a growth re-send of a stored card changes nothing).
 */
export function receiveDeviceCard(
  cards: DeviceCardMap,
  senderKeyHex: string,
  card: StoredDeviceCard,
  rosterKeysHex: readonly string[],
): DeviceCardMap | null {
  if (!rosterKeysHex.includes(senderKeyHex)) return null;
  const stored = cards[senderKeyHex];
  if (stored !== undefined && !(card.updatedAtMs > stored.updatedAtMs)) return null;
  return { ...cards, [senderKeyHex]: card };
}

/** Drop the card of every leaf that left the DeviceGroup. `null` when none. */
export function pruneDeviceCards(
  cards: DeviceCardMap,
  rosterKeysHex: readonly string[],
): DeviceCardMap | null {
  const keep = Object.entries(cards).filter(([key]) => rosterKeysHex.includes(key));
  if (keep.length === Object.keys(cards).length) return null;
  return Object.fromEntries(keep);
}

/** A device sends a fresh card after an action when its last one is older
 *  than this (ADR-0080 section 3). */
export const CARD_REFRESH_MS = 30 * 60 * 1000;

/** The longest wait before the growth re-send of the own card. */
export const CARD_GROWTH_MAX_DELAY_MS = 300;

/** `true` when two cards differ in a field other than the times. */
export function cardFieldsDiffer(a: DeviceCardFields, b: DeviceCardFields): boolean {
  return (
    a.platform !== b.platform ||
    a.model !== b.model ||
    a.appVersion !== b.appVersion ||
    a.os !== b.os ||
    a.city !== b.city ||
    a.country !== b.country
  );
}

/**
 * Whether the person's action at `nowMs` must send a fresh own card: there is
 * none yet, a field other than the times changed, or the last one is older
 * than [`CARD_REFRESH_MS`]. A clock that went backwards counts as "old", so a
 * device whose clock was wrong does not stay silent for good.
 */
export function shouldSendOwnCard(
  last: StoredDeviceCard | null,
  current: DeviceCardFields,
  nowMs: number,
): boolean {
  if (last === null) return true;
  if (cardFieldsDiffer(last, current)) return true;
  const age = nowMs - last.updatedAtMs;
  return age < 0 || age >= CARD_REFRESH_MS;
}

/** A random delay in `[0, 300 ms)` for the growth re-send. */
export function cardGrowthDelayMs(random: () => number = Math.random): number {
  const r = random();
  const clamped = Number.isFinite(r) ? Math.min(Math.max(r, 0), 0.999_999) : 0;
  return Math.floor(clamped * CARD_GROWTH_MAX_DELAY_MS);
}

/** What the browser tells about itself, for the card. */
export interface BrowserFacts {
  /** e.g. "Chrome on macOS". */
  readonly model: string;
  /** e.g. "macOS 26"; the version is left out where the browser hides it. */
  readonly os: string;
}

/**
 * Read the browser and system from a user agent string. Only names this
 * client recognises are kept; anything else becomes "Browser" / an empty
 * system rather than echoing the raw string.
 */
export function browserFacts(userAgent: string): BrowserFacts {
  const ua = userAgent;
  let browser = "Browser";
  if (/Edg\//.test(ua)) browser = "Edge";
  else if (/OPR\//.test(ua)) browser = "Opera";
  else if (/Firefox\//.test(ua) || /FxiOS\//.test(ua)) browser = "Firefox";
  else if (/Chrome\//.test(ua) || /CriOS\//.test(ua)) browser = "Chrome";
  else if (/Safari\//.test(ua) && /Version\//.test(ua)) browser = "Safari";

  let system = "";
  let version = "";
  const ios = /(?:iPhone|iPad|iPod).*? OS (\d+)(?:_(\d+))?/.exec(ua);
  const android = /Android (\d+(?:\.\d+)?)/.exec(ua);
  const mac = /Mac OS X (\d+)[._](\d+)/.exec(ua);
  const windows = /Windows NT (\d+\.\d+)/.exec(ua);
  if (ios !== null) {
    system = /iPad/.test(ua) ? "iPadOS" : "iOS";
    version = ios[2] === undefined ? (ios[1] ?? "") : `${ios[1] ?? ""}.${ios[2]}`;
  } else if (android !== null) {
    system = "Android";
    version = android[1] ?? "";
  } else if (mac !== null) {
    // Browsers freeze the reported macOS version at 10.15; it says nothing.
    system = "macOS";
    version = mac[1] === "10" && mac[2] === "15" ? "" : `${mac[1]}.${mac[2]}`;
  } else if (windows !== null) {
    system = "Windows";
    // Windows 11 also reports NT 10.0.
    version = windows[1] === "10.0" ? "" : (windows[1] ?? "");
  } else if (/CrOS/.test(ua)) {
    system = "ChromeOS";
  } else if (/Linux/.test(ua)) {
    system = "Linux";
  }
  const model = system === "" ? browser : `${browser} on ${system}`;
  const os = system === "" ? "" : version === "" ? system : `${system} ${version}`;
  return { model, os };
}

/** Which icon a row shows. */
export type DeviceIconKind = "phone" | "tablet" | "computer" | "browser" | "unknown";

export function deviceIconKind(platform: DevicePlatform | null): DeviceIconKind {
  switch (platform) {
    case "ios":
    case "android":
      return "phone";
    case "ipados":
      return "tablet";
    case "macos":
    case "desktop":
      return "computer";
    case "web":
      return "browser";
    default:
      return "unknown";
  }
}

/** A device of the DeviceGroup roster, with its card when one arrived. */
export interface DeviceRowInput {
  readonly keyHex: string;
  readonly leafIndex: number;
  readonly isSelf: boolean;
  readonly card: StoredDeviceCard | null;
}

/**
 * The two sections of the screen: this device, then the others, newest
 * activity first; devices with no card yet come last, in roster order.
 */
export function orderDeviceRows(rows: readonly DeviceRowInput[]): {
  self: DeviceRowInput | null;
  others: DeviceRowInput[];
} {
  const self = rows.find((r) => r.isSelf) ?? null;
  const others = rows
    .filter((r) => !r.isSelf)
    .slice()
    .sort((a, b) => {
      if (a.card !== null && b.card !== null) {
        if (b.card.lastActiveMs !== a.card.lastActiveMs) return b.card.lastActiveMs - a.card.lastActiveMs;
        return a.leafIndex - b.leafIndex;
      }
      if (a.card !== null) return -1;
      if (b.card !== null) return 1;
      return a.leafIndex - b.leafIndex;
    });
  return { self, others };
}

/** The words a row needs, translated by the caller. */
export interface RowWords {
  /** "Device", for a row with no card or no model. */
  readonly device: string;
  /** "online", this device's activity. */
  readonly online: string;
  /** `Lakko {version}` with the version filled in; "Lakko" when unknown. */
  readonly app: (version: string) => string;
  /** `{city}, {country}`. */
  readonly place: (city: string, country: string) => string;
  /** The country's name in the viewer's language, from its code. */
  readonly countryName: (code: string) => string;
}

/** What a row of the Devices screen shows. */
export interface DeviceRowView {
  readonly title: string;
  /** App and system, `Lakko 1.4 · iOS 27.0.1`; empty for a row with no card. */
  readonly detail: string;
  /** City and country, or empty (left out). */
  readonly place: string;
  /** "online", a time, a weekday or a date; empty when unknown. */
  readonly activity: string;
  readonly icon: DeviceIconKind;
}

export function deviceRowView(
  row: DeviceRowInput,
  words: RowWords,
  nowMs: number,
  locale?: string,
): DeviceRowView {
  const card = row.card;
  const activity = row.isSelf
    ? words.online
    : card === null || card.lastActiveMs <= 0
      ? ""
      : formatListTime(new Date(card.lastActiveMs).toISOString(), new Date(nowMs), locale);
  if (card === null) {
    return { title: words.device, detail: "", place: "", activity, icon: "unknown" };
  }
  const app = words.app(card.appVersion);
  const detail = card.os === "" ? app : `${app} · ${card.os}`;
  const country = card.country === "" ? "" : words.countryName(card.country);
  const place =
    card.city !== "" && country !== ""
      ? words.place(card.city, country)
      : card.city !== ""
        ? card.city
        : country;
  return {
    title: card.model === "" ? words.device : card.model,
    detail,
    place,
    activity,
    icon: deviceIconKind(card.platform),
  };
}

/** The country's name in `locale`, from its two-letter code; the code when
 *  the runtime has no name for it. */
export function countryNameOf(code: string, locale?: string): string {
  try {
    const names = new Intl.DisplayNames(locale === undefined ? undefined : [locale], { type: "region" });
    return names.of(code) ?? code;
  } catch {
    return code;
  }
}

/** A decoded card as the wasm `DeviceCard` class exposes it (structural, so
 *  the rules stay free of wasm types). */
export interface DeviceCardLike {
  readonly platform: string;
  readonly model: string;
  readonly appVersion: string;
  readonly os: string;
  readonly city: string;
  readonly country: string;
  readonly updatedAtMs: bigint;
  readonly lastActiveMs: bigint;
}

/** The card as this client keeps it, or `null` for a platform it does not
 *  know (such a card is dropped, as a DeviceGroup card would be). */
export function storedCardOf(card: DeviceCardLike): StoredDeviceCard | null {
  const platform = card.platform;
  if (!isDevicePlatform(platform)) return null;
  return {
    platform,
    model: card.model,
    appVersion: card.appVersion,
    os: card.os,
    city: card.city,
    country: card.country,
    updatedAtMs: Number(card.updatedAtMs),
    lastActiveMs: Number(card.lastActiveMs),
  };
}

/**
 * docs/ux/second-device.md section 2.1: the card a new device put in its
 * pairing payload, kept for its leaf on the device that let it in, so its
 * row has a name at once. Kept only when no card is stored for that leaf,
 * and stamped 0, the lowest stamp: any card the device itself sends over the
 * DeviceGroup is strictly newer and replaces it (`receiveDeviceCard`).
 * Returns the new map, or `null` when nothing changes.
 */
export function adoptPairingCard(
  cards: DeviceCardMap,
  keyHex: string,
  card: StoredDeviceCard,
): DeviceCardMap | null {
  if (cards[keyHex] !== undefined) return null;
  return { ...cards, [keyHex]: { ...card, updatedAtMs: 0 } };
}

/**
 * docs/ux/second-device.md section 3, "This device never waits for a card":
 * this device's own row is built from what it knows about itself now, never
 * looked up among the cards the other devices sent.
 */
export function selfRowCard(
  own: StoredDeviceCard | null,
  current: DeviceCardFields,
  nowMs: number,
): StoredDeviceCard {
  return { ...current, updatedAtMs: own?.updatedAtMs ?? 0, lastActiveMs: nowMs };
}

/** What the confirm step and the result line say about a device being
 *  added: its name (the card's model or browser name), and the same two
 *  lines its Devices row will show. All empty with no card. */
export interface PairingCardView {
  /** `null` with no card, or a card with no name. */
  readonly name: string | null;
  readonly detail: string;
  readonly place: string;
}

export function pairingCardView(card: StoredDeviceCard | null, words: RowWords): PairingCardView {
  if (card === null) return { name: null, detail: "", place: "" };
  // The row formatter, so the confirm shows exactly what the row will.
  const view = deviceRowView({ keyHex: "", leafIndex: 0, isSelf: false, card: { ...card, lastActiveMs: 0 } }, words, 0);
  return { name: pairingCardName(card), detail: view.detail, place: view.place };
}

/** The name a device being added goes by: its card's model or browser name,
 *  `null` with no card or an empty name (the unnamed copy is used then). */
export function pairingCardName(card: StoredDeviceCard | null): string | null {
  const name = card?.model.trim() ?? "";
  return name === "" ? null : name;
}
