// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * Reconciling this device's channel list with the account's channel
 * subscription registers (2026-11-13; contract rules 1-4, UX
 * `docs/ux/second-device.md` section 4a). Pure: the runtime
 * (`ChannelSubscriptionSync.tsx`) gathers the inputs and performs the steps.
 *
 * Who decides what:
 *
 *  - A device that holds a channel's PublisherGroup (it publishes there) is
 *    the authority for the account's role in it: it keeps the register live
 *    with role Owner, or CoPublisher and the group id, and the channel's name
 *    and handle. A leave never removes such a channel from it.
 *  - Any other device follows the register. A live register for a channel it
 *    does not list adds it (as a reader: the subscriber key, the read key, a
 *    co-publisher's group id mark); a leave for a channel it lists drops it,
 *    unless this device just subscribed again (an intent). It writes only
 *    what the account does not hold yet: the register of a channel with no
 *    register at all (channels subscribed before registers existed, and a
 *    fresh subscribe), a read key the register lacks, or the subscriber role
 *    after this device stopped being a publisher (a demotion).
 *  - One subscriber key per account and channel: a live register's key wins
 *    over a different local one, which is unsubscribed at the service.
 */
import type {
  ChannelSubscriptionRoleName,
  ChannelSubscriptionValue,
  ChannelSubscriptionView,
} from "./channel-subscription-register";

/** What this device holds for one channel. */
export interface LocalChannelState {
  /** 32 lower-case hex. */
  readonly idHex: string;
  /** In this device's channel list. */
  readonly listed: boolean;
  /** This device holds the channel's PublisherGroup (it can post). */
  readonly publishing: boolean;
  /** The co-published group id this device has marked, 64 hex, or `null`. */
  readonly coPublisherGroupIdHex: string | null;
  /** This device's subscriber key seed, or `null`. */
  readonly subscriberSecret: Uint8Array | null;
  /** The read key this device holds, or `null`. */
  readonly readKey: Uint8Array | null;
  /** Epoch seeds are held for it. */
  readonly holdsSeeds: boolean;
  /** The name this device shows (the channel record's, else local). */
  readonly name: string;
  /** The `@name` without `@`, or empty. */
  readonly handle: string;
}

/** Why this device writes a register. */
export type ChannelIntent = "subscribed" | "demoted";

export type ChannelSubscriptionStep =
  /** Write this live value. */
  | { readonly kind: "write"; readonly idHex: string; readonly value: ChannelSubscriptionValue }
  /** Add the channel to this device's list, as a reader. */
  | { readonly kind: "add"; readonly idHex: string; readonly register: ChannelSubscriptionView }
  /** Take the register's subscriber key; unsubscribe `oldSecret` (if any)
   *  at the service. */
  | {
      readonly kind: "adopt-key";
      readonly idHex: string;
      readonly secret: Uint8Array;
      readonly oldSecret: Uint8Array | null;
    }
  /** Keep the register's read key. */
  | { readonly kind: "read-key"; readonly idHex: string; readonly readKey: Uint8Array }
  /** Mark the channel as co-published under this group id. */
  | { readonly kind: "co-publisher-mark"; readonly idHex: string; readonly groupIdHex: string }
  /** The account left: drop the channel here as an unsubscribe would,
   *  without telling the service. */
  | { readonly kind: "drop"; readonly idHex: string }
  /** Seeds a pairing carried for a channel the account left. */
  | { readonly kind: "forget-seeds"; readonly idHex: string }
  /** The intent was served (written or no longer needed). */
  | { readonly kind: "intent-done"; readonly idHex: string };

export interface PlanInput {
  readonly local: readonly LocalChannelState[];
  readonly held: ReadonlyMap<string, ChannelSubscriptionView>;
  readonly intents: ReadonlyMap<string, ChannelIntent>;
  /** Channels whose end is running here: never added back. */
  readonly ending: ReadonlySet<string>;
  /** A fresh 32-byte subscriber seed, for a publishing device that holds no
   *  subscriber key and no register yet. */
  readonly freshSecret: () => Uint8Array;
}

const NAME_MAX_BYTES = 255;
const HANDLE_RE = /^[a-z0-9_]{3,32}$/;
// Line breaks, controls and the format characters the core's display-name
// rule refuses: such a name is not carried (the list falls back to the
// channel record).
const NAME_REFUSED_RE = new RegExp("[\\u0000-\\u001f\\u007f-\\u009f\\u00ad\\u061c\\u200b-\\u200f\\u2028-\\u202e\\u2060-\\u206f\\ufeff\\ufff9-\\ufffb]");

/** A name the register accepts, or empty. */
export function registerName(name: string): string {
  const trimmed = name.trim();
  if (trimmed.length === 0 || NAME_REFUSED_RE.test(trimmed)) return "";
  if (new TextEncoder().encode(trimmed).length > NAME_MAX_BYTES) return "";
  return trimmed;
}

/** A handle the register accepts, or empty. */
export function registerHandle(handle: string): string {
  const h = handle.replace(/^@/, "").trim();
  return HANDLE_RE.test(h) ? h : "";
}

export function sameBytes(a: Uint8Array | null, b: Uint8Array | null): boolean {
  if (a === null || b === null) return a === b;
  if (a.length !== b.length) return false;
  for (let i = 0; i < a.length; i += 1) if (a[i] !== b[i]) return false;
  return true;
}

function empty(): Uint8Array {
  return new Uint8Array(0);
}

function sameValue(held: ChannelSubscriptionView, v: ChannelSubscriptionValue): boolean {
  return (
    held.subscribed &&
    held.role === v.role &&
    sameBytes(held.subscriberSecret, v.subscriberSecret) &&
    sameBytes(held.readKey, v.readKey) &&
    held.publisherGroupIdHex === (v.role === "co-publisher" ? v.publisherGroupIdHex : "") &&
    held.name === v.name &&
    held.handle === v.handle
  );
}

function withValue(
  held: ChannelSubscriptionView,
  change: Partial<ChannelSubscriptionValue>,
): ChannelSubscriptionValue {
  return {
    role: held.role,
    subscriberSecret: held.subscriberSecret,
    readKey: held.readKey,
    publisherGroupIdHex: held.publisherGroupIdHex,
    name: held.name,
    handle: held.handle,
    ...change,
  };
}

function nonEmpty(b: Uint8Array | null): Uint8Array | null {
  return b !== null && b.length > 0 ? b : null;
}

/** The steps that bring this device and the registers together. */
export function planChannelSubscriptions(input: PlanInput): ChannelSubscriptionStep[] {
  const steps: ChannelSubscriptionStep[] = [];
  const localById = new Map(input.local.map((c) => [c.idHex, c]));
  for (const c of input.local) {
    if (!c.listed) continue;
    const held = input.held.get(c.idHex);
    const live = held !== undefined && held.subscribed;
    const intent = input.intents.get(c.idHex);
    if (c.publishing) {
      const role: ChannelSubscriptionRoleName = c.coPublisherGroupIdHex === null ? "owner" : "co-publisher";
      const secret = live
        ? held.subscriberSecret
        : (nonEmpty(c.subscriberSecret) ?? input.freshSecret());
      const value: ChannelSubscriptionValue = {
        role,
        subscriberSecret: secret,
        readKey: nonEmpty(c.readKey) ?? (live ? held.readKey : empty()),
        publisherGroupIdHex: role === "co-publisher" ? (c.coPublisherGroupIdHex ?? "") : "",
        name: registerName(c.name) || (live ? held.name : ""),
        handle: registerHandle(c.handle) || (live ? held.handle : ""),
      };
      if (!live || !sameValue(held, value)) steps.push({ kind: "write", idHex: c.idHex, value });
      if (live && c.subscriberSecret !== null && !sameBytes(c.subscriberSecret, held.subscriberSecret)) {
        steps.push({ kind: "adopt-key", idHex: c.idHex, secret: held.subscriberSecret, oldSecret: c.subscriberSecret });
      }
      if (intent !== undefined) steps.push({ kind: "intent-done", idHex: c.idHex });
      continue;
    }
    if (live) {
      if (intent === "demoted") {
        const value = withValue(held, {
          role: "subscriber",
          publisherGroupIdHex: "",
          readKey: nonEmpty(c.readKey) ?? held.readKey,
        });
        if (!sameValue(held, value)) steps.push({ kind: "write", idHex: c.idHex, value });
      } else if (held.readKey.length === 0 && nonEmpty(c.readKey) !== null) {
        steps.push({ kind: "write", idHex: c.idHex, value: withValue(held, { readKey: c.readKey ?? empty() }) });
      }
      if (!sameBytes(c.subscriberSecret, held.subscriberSecret)) {
        steps.push({ kind: "adopt-key", idHex: c.idHex, secret: held.subscriberSecret, oldSecret: c.subscriberSecret });
      }
      if (held.readKey.length === 32 && nonEmpty(c.readKey) === null) {
        steps.push({ kind: "read-key", idHex: c.idHex, readKey: held.readKey });
      }
      if (
        intent !== "demoted" &&
        held.role === "co-publisher" &&
        held.publisherGroupIdHex.length > 0 &&
        c.coPublisherGroupIdHex !== held.publisherGroupIdHex
      ) {
        steps.push({ kind: "co-publisher-mark", idHex: c.idHex, groupIdHex: held.publisherGroupIdHex });
      }
      if (intent !== undefined) steps.push({ kind: "intent-done", idHex: c.idHex });
      continue;
    }
    if (held !== undefined && intent === undefined) {
      // A leave this device did not overrule: the account left.
      steps.push({ kind: "drop", idHex: c.idHex });
      continue;
    }
    // No register, or a leave this device overrules by subscribing again.
    const secret = nonEmpty(c.subscriberSecret);
    if (secret === null) continue;
    steps.push({
      kind: "write",
      idHex: c.idHex,
      value: {
        role: "subscriber",
        subscriberSecret: secret,
        readKey: nonEmpty(c.readKey) ?? empty(),
        publisherGroupIdHex: "",
        name: registerName(c.name),
        handle: registerHandle(c.handle),
      },
    });
    if (intent !== undefined) steps.push({ kind: "intent-done", idHex: c.idHex });
  }
  for (const [idHex, held] of input.held) {
    const here = localById.get(idHex);
    if (here?.listed === true) continue;
    if (held.subscribed) {
      if (!input.ending.has(idHex) && held.subscriberSecret.length === 32) {
        steps.push({ kind: "add", idHex, register: held });
      }
    } else if (here?.holdsSeeds === true && !input.intents.has(idHex)) {
      steps.push({ kind: "forget-seeds", idHex });
    }
  }
  return steps;
}

/** This device's view of a listed channel it does not publish in: a reader
 *  of an owner's or a publisher's channel (UX section 4a), or a subscriber.
 *  `null` when the channel is not listed or this device publishes in it. */
export function readerDeviceRole(input: {
  readonly listed: boolean;
  readonly publishing: boolean;
  readonly registerRole: ChannelSubscriptionRoleName | null;
}): "owner" | "publisher" | null {
  if (!input.listed || input.publishing) return null;
  if (input.registerRole === "owner") return "owner";
  if (input.registerRole === "co-publisher") return "publisher";
  return null;
}

/** The exit block of a channel's info (UX group-and-channel-privacy 12.1). */
const EXIT_SECTIONS: ReadonlySet<string> = new Set(["unsubscribe", "delete-group"]);

/**
 * The info blocks of a channel on a reader device of its owner or a
 * publisher (UX second-device.md section 4a): what a subscriber sees, with
 * the exit block EMPTY - no Unsubscribe, no Delete channel, no Step down: the
 * channel is left from the device that posts in it. Unchanged for anyone
 * else. Pure over the section names.
 */
export function readerDeviceSections<S extends string>(
  sections: readonly S[],
  readerRole: "owner" | "publisher" | null,
): readonly S[] {
  if (readerRole === null) return sections;
  return sections.filter((s) => !EXIT_SECTIONS.has(s));
}
