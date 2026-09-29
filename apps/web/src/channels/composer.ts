// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * TS shim around the `lakko-ffi-wasm` channels surface
 * (G4-2-crypto). Closes the "stub publish" gap from G4-2:
 * the web UI composes real signed encrypted ChannelPosts
 * via `composeChannelPost` and decodes incoming ones via
 * `verifyAndDecryptChannelPost`.
 *
 * The thin wrapper exists so the rest of the web code
 * does not have to remember the lazy-load `loadWasm()`
 * dance for every call site — every helper here returns
 * a `Promise<…>` that resolves after the wasm module is
 * ready.
 */

import { loadWasm, type Wasm } from "../wasm";
import { rethrowAsLegacyChannel } from "./legacy-v1";
import { DsClient } from "../ds/client";
import { claimOrReset } from "../ds/ownership";

/** wasm-bindgen-generated Identity handle. Re-exported
 *  here as a type-only alias so caller signatures stay
 *  readable without a deep import path. */
export type WasmIdentity = Wasm["Identity"]["prototype"];

/** 16-byte channel id parsed from a 32-char hex string. */
export function channelIdFromHex(hex: string): Uint8Array {
  if (hex.length !== 32) {
    throw new Error(
      `channel id must be 32 hex chars (got ${hex.length})`,
    );
  }
  const out = new Uint8Array(16);
  for (let i = 0; i < 16; i += 1) {
    const byte = Number.parseInt(hex.slice(i * 2, i * 2 + 2), 16);
    if (Number.isNaN(byte)) {
      throw new Error(`invalid hex byte at position ${i * 2}`);
    }
    out[i] = byte;
  }
  return out;
}

/** Hex-encode a Uint8Array (lowercase, no separators). */
export function bytesToHex(bytes: Uint8Array): string {
  let out = "";
  for (const b of bytes) {
    out += b.toString(16).padStart(2, "0");
  }
  return out;
}

/** Decode a base64url-no-pad string back to bytes. */
export function base64UrlToBytes(b64: string): Uint8Array {
  // Re-pad + canonicalise to standard base64 so atob accepts it.
  const std = b64
    .replace(/-/g, "+")
    .replace(/_/g, "/")
    .padEnd(b64.length + ((4 - (b64.length % 4)) % 4), "=");
  const binary = atob(std);
  const out = new Uint8Array(binary.length);
  for (let i = 0; i < binary.length; i += 1) {
    out[i] = binary.charCodeAt(i);
  }
  return out;
}

/** Encode bytes to base64url (no padding). Mirror of the
 *  Rust-side `base64::URL_SAFE_NO_PAD` encoder used in
 *  `services/channels`'s envelope-delivery handler. */
export function bytesToBase64Url(bytes: Uint8Array): string {
  let binary = "";
  for (const b of bytes) {
    binary += String.fromCharCode(b);
  }
  return btoa(binary)
    .replace(/\+/g, "-")
    .replace(/\//g, "_")
    .replace(/=+$/, "");
}

/**
 * Local publisher state the web UI needs to produce
 * posts on a channel. The `seed` is derived from an
 * MLS PublisherGroup exporter (ADR-0010 Tier 1) keyed
 * by the publisher's identity and the channel id —
 * the same byte-identical seed any device of the same
 * publisher would derive on the same channel.
 */
export interface ChannelPublisherKeys {
  /** 32-byte content-key seed.
   *
   *  Derived from an MLS PublisherGroup via
   *  `Group.exportSecret(identity, channelExporterLabel(),
   *  channelId, 32)` and then folded through
   *  `channelSeedFromMlsExporterOutput`. Two devices of
   *  the same publisher on the same channel produce the
   *  byte-identical seed. */
  readonly seed: Uint8Array;
  /** 32-byte Ed25519 signing-key seed used to derive the
   *  `ChannelSigningKey` for every post. Persisted as a
   *  seed (not a key handle) so the JS side can re-build
   *  the WASM class after a page reload. */
  readonly signingKeySeed: Uint8Array;
}

/**
 * Build a fresh `(seed, signingKeySeed)` pair for a new
 * channel. `identity` is the publisher's MLS identity
 * (typically built from the OPAQUE export key via
 * `wasm.Identity.fromSeed`); `channelIdHex` is the
 * 16-byte channel id as a 32-char hex string.
 *
 * G4-2-mls-persistence step 3 — the PublisherGroup is now
 * **persisted** in the OpenMLS provider keyed by
 * `channelIdHex` (deterministic group id derived from the
 * 16-byte channel id). The first publish on this browser
 * profile mints + persists; every subsequent publish
 * reuses the same MLS state, so the exporter output is
 * stable across page reloads. Subscribers who received a
 * sealed seed envelope in a prior epoch continue to
 * decrypt new posts.
 *
 * The previous throwaway-group implementation lived here
 * until step 3 (flagged in the G-27 closeout). See
 * `crates/lakko-mls/src/publisher_group.rs` for the
 * Rust-core invariant + the load-or-create dance.
 *
 * "Persisted in the provider" means the provider of THIS
 * `identity`: pass the session-long account identity
 * (`useChats().accountIdentity()`), whose provider the channel
 * log snapshots, not a throwaway `Identity.fromSeed` — a group
 * created in a throwaway provider dies with it.
 *
 * `existingSigningSeed` — the per-channel Ed25519 signing seed
 * this device persisted on a prior open (mirror of the iOS /
 * Android `existingSigningSeed`). When supplied and well-formed
 * the publisher keeps signing under the same key across a
 * reload; a wrong-length seed falls back to a fresh one rather
 * than failing the open (the caller re-persists what it gets).
 */
export async function freshPublisherKeys(
  identity: WasmIdentity,
  channelIdHex: string,
  existingSigningSeed?: Uint8Array,
): Promise<ChannelPublisherKeys> {
  const wasm = await loadWasm();
  const channelId = channelIdFromHex(channelIdHex);

  // Persistent PublisherGroup → MLS exporter → channel
  // seed. The wasm-bindgen surface delegates to the
  // load-or-create dance in `crates/lakko-mls`; the seed
  // is stable across page reloads for the same identity +
  // channel id.
  const publisher = await openOrFoundPublisherGroup(wasm, identity, channelId);
  let seed: Uint8Array;
  try {
    seed = publisher.channelSeed(identity);
  } finally {
    publisher.free();
  }

  if (existingSigningSeed !== undefined && existingSigningSeed.length === 32) {
    // Reconstruct rather than trust: a seed the constructor refuses is
    // treated like an absent one.
    try {
      const signer = new wasm.ChannelSigningKey(existingSigningSeed);
      signer.free();
      return { seed, signingKeySeed: existingSigningSeed.slice() };
    } catch {
      // fall through to a fresh seed
    }
  }
  const signer = wasm.ChannelSigningKey.generate();
  const signingKeySeed = signer.toSeed();
  signer.free();
  return { seed, signingKeySeed };
}

/**
 * Compose a signed encrypted ChannelPost. Returns the
 * wire bytes ready to `POST /v1/channels/:id/posts`.
 *
 * The signing-key handle is built fresh from
 * `signingKeySeed` on every call so the public surface
 * does not hold a live WASM object reference across
 * await boundaries (which would complicate cleanup +
 * make stale-state bugs hard to spot). The construction
 * cost is dominated by the AEAD seal — the
 * `ChannelSigningKey::new` step is two Ristretto255
 * scalar ops.
 */
export async function composePost(
  keys: ChannelPublisherKeys,
  channelIdHex: string,
  epochId: number,
  messageCounter: bigint,
  plaintext: Uint8Array,
): Promise<Uint8Array> {
  const wasm = await loadWasm();
  const channelId = channelIdFromHex(channelIdHex);
  const signer = new wasm.ChannelSigningKey(keys.signingKeySeed);
  try {
    return wasm.composeChannelPost(
      keys.seed,
      channelId,
      epochId,
      signer,
      messageCounter,
      plaintext,
    );
  } finally {
    signer.free();
  }
}

/**
 * Verify + decrypt a base64url-encoded ChannelPost
 * (the form `services/channels` returns via the fetch
 * endpoint). Returns the recovered plaintext bytes.
 */
export async function verifyAndDecryptPost(
  seed: Uint8Array,
  channelIdHex: string,
  postWireB64Url: string,
): Promise<Uint8Array> {
  const wasm = await loadWasm();
  const channelId = channelIdFromHex(channelIdHex);
  const wire = base64UrlToBytes(postWireB64Url);
  const post = wasm.decodeChannelPost(wire);
  try {
    return wasm.verifyAndDecryptChannelPost(seed, channelId, post);
  } finally {
    post.free();
  }
}

/** Result of {@link decodeAndDecryptPost}. */
export interface DecodedPostResult {
  /** The post's `epoch_id`, read from its clear wire header. */
  readonly epochId: number;
  /** Recovered plaintext, or `null` when no seed was available
   *  for `epochId` OR the AEAD open failed. */
  readonly plaintext: Uint8Array | null;
  /** `true` when a seed for `epochId` was supplied — so a `null`
   *  `plaintext` means a genuine decrypt failure (wrong seed,
   *  tampered ciphertext, or a foreign signer). `false` when no
   *  seed existed for this epoch (older-than-retention window, or
   *  a gap): the post is not decryptable on this device and was
   *  NOT force-decrypted with a wrong-epoch seed (invariant #4). */
  readonly hadSeed: boolean;
}

/**
 * Decode a base64url ChannelPost once, read its `epoch_id`, pick
 * the matching seed via `seedForEpoch`, and — only when a seed
 * exists — verify + AEAD-open under it (ADR-0031 multi-epoch
 * retention). A subscriber keeps an `epoch -> seed` map and each
 * post is opened under the seed for ITS epoch. A post whose epoch
 * has no seed comes back `{ plaintext: null, hadSeed: false }` and
 * is never decrypted with a different epoch's seed.
 *
 * The single decode (vs. `verifyAndDecryptPost`, which re-decodes
 * internally) lets the caller branch on the epoch before spending
 * an AEAD open — and surfaces the older-key placeholder cleanly.
 */
export async function decodeAndDecryptPost(
  channelIdHex: string,
  postWireB64Url: string,
  seedForEpoch: (epoch: number) => Uint8Array | undefined,
): Promise<DecodedPostResult> {
  const wasm = await loadWasm();
  const channelId = channelIdFromHex(channelIdHex);
  const wire = base64UrlToBytes(postWireB64Url);
  const post = wasm.decodeChannelPost(wire);
  try {
    const epochId = post.epochId;
    const seed = seedForEpoch(epochId);
    if (seed === undefined) {
      return { epochId, plaintext: null, hadSeed: false };
    }
    try {
      const plaintext = wasm.verifyAndDecryptChannelPost(seed, channelId, post);
      return { epochId, plaintext, hadSeed: true };
    } catch {
      return { epochId, plaintext: null, hadSeed: true };
    }
  } finally {
    post.free();
  }
}

// ---------------------------------------------------------
// Subscriber-side surface (G4-3c-web)
// ---------------------------------------------------------

/**
 * Subscriber-side X25519 keypair. The public half is what
 * gets registered on `POST /v1/channels/:id/subscribers`;
 * the secret half stays on the device + unwraps the per-
 * subscriber HPKE seed envelopes the publisher fans out
 * on every rotation.
 *
 * Both halves are 32 bytes — the storage layer
 * (channels-state.ts) persists them in localStorage as
 * base64url for now; a follow-up sprint moves the secret
 * into IndexedDB encrypted under the device-group key.
 */
export interface ChannelSubscriberKeys {
  /** 32-byte X25519 secret. */
  readonly secret: Uint8Array;
  /** 32-byte X25519 public. */
  readonly public: Uint8Array;
}

/** Build a fresh X25519 keypair for joining a channel as
 *  a subscriber. Backed by `crypto.getRandomValues` via
 *  the `getrandom + js` feature on the wasm side. */
export async function freshSubscriberKeys(): Promise<ChannelSubscriberKeys> {
  const wasm = await loadWasm();
  const kp = wasm.ChannelSubscriberKeypair.generate();
  try {
    return {
      secret: kp.secret(),
      public: kp.public(),
    };
  } finally {
    kp.free();
  }
}

/**
 * Seal a 32-byte content-key seed against a subscriber's
 * X25519 public key. Returns the 80-byte wire bytes ready
 * to base64url-encode and `POST
 * /v1/channels/:id/subscribers/:pk/envelope`.
 *
 * Used by the publisher side of the UI when delivering
 * a fresh seed to each registered subscriber on rotation
 * or initial fan-out.
 */
export async function sealSubscriberSeedEnvelope(
  subscriberPub: Uint8Array,
  channelIdHex: string,
  epochId: number,
  seed: Uint8Array,
): Promise<Uint8Array> {
  const wasm = await loadWasm();
  const channelId = channelIdFromHex(channelIdHex);
  return wasm.sealSubscriberSeedEnvelope(
    subscriberPub,
    channelId,
    epochId,
    seed,
  );
}

/**
 * Open a seed envelope with the subscriber's keypair.
 * Returns the recovered 32-byte content-key seed — hand
 * it to `verifyAndDecryptPost` to decode posts on this
 * channel for this epoch.
 *
 * The keypair is rebuilt fresh from `secret` on every
 * call so the public surface does not hold a live WASM
 * handle across await boundaries (same pattern as
 * `composePost` above).
 */
export async function openSubscriberSeedEnvelope(
  secret: Uint8Array,
  channelIdHex: string,
  epochId: number,
  wire: Uint8Array,
): Promise<Uint8Array> {
  const wasm = await loadWasm();
  const channelId = channelIdFromHex(channelIdHex);
  const kp = new wasm.ChannelSubscriberKeypair(secret);
  try {
    return wasm.openSubscriberSeedEnvelope(kp, channelId, epochId, wire);
  } finally {
    kp.free();
  }
}

/** Wire byte length of a seed envelope —
 *  `eph_pub(32) + sealed_seed(32) + tag(16) = 80`. Exposed
 *  for UI sanity checks. */
export async function seedEnvelopeLen(): Promise<number> {
  const wasm = await loadWasm();
  return wasm.channelSeedEnvelopeLen();
}

// ---------------------------------------------------------
// UTF-8 helpers
// ---------------------------------------------------------

/** UTF-8 encode a string for the publish form. */
export function utf8Encode(s: string): Uint8Array {
  return new TextEncoder().encode(s);
}

/** UTF-8 decode bytes back to a string. Replacement-on-
 *  invalid so a future ChannelPost wire-shape that
 *  carries non-text payload does not throw the whole
 *  list rendering loop. */
export function utf8Decode(bytes: Uint8Array): string {
  return new TextDecoder("utf-8", { fatal: false }).decode(bytes);
}

/**
 * ADR-0057 obligations 1 + 7 — open the channel's PublisherGroup, and when
 * this call FOUNDS it (no persisted group for the channel: a first open, or a
 * re-found after a lost-state recovery), claim its journal with the channel
 * owner's ROOT; a 409 (the deterministic id already carries an older group's
 * chain) becomes a root RESET. A loaded group claims nothing here: a reset of
 * a live chain would drop every epoch this device has not seen, and an
 * unclaimed journal is claimed by the owner's first refused request.
 *
 * FAIL-CLOSED (the second security pass, finding 3): the claim is awaited and
 * must land before the handle is returned, so no channel is announced (the
 * handle registered, the channel created on the service, a post published)
 * for a journal somebody else could claim first. A failed claim frees the
 * handle and throws. The founded group stays in the provider (it cannot be
 * deleted), so an "unconfirmed founding" marker for its id is kept until a
 * claim lands: the next open of that channel claims again instead of taking
 * the loaded group as claimed. Throws what `openOrCreate` throws too (a
 * retired v1 group, surfaced as the legacy-channel copy). The caller owns
 * the handle.
 */
export async function openOrFoundPublisherGroup(
  wasm: Wasm,
  identity: WasmIdentity,
  channelId: Uint8Array,
  storage: UnconfirmedFoundingStorage | null = defaultFoundingStorage(),
): Promise<InstanceType<Wasm["PublisherGroup"]>> {
  let existed = false;
  try {
    const loaded = wasm.PublisherGroup.load(identity, channelId);
    existed = loaded !== undefined;
    loaded?.free();
  } catch {
    existed = false;
  }
  let publisher: InstanceType<Wasm["PublisherGroup"]>;
  try {
    publisher = wasm.PublisherGroup.openOrCreate(identity, channelId);
  } catch (err) {
    // Retired v1 state refuses here and cannot be repaired.
    rethrowAsLegacyChannel(err);
  }
  const groupId = publisher.groupId();
  const key = foundingKey(groupId);
  if (!existed || isUnconfirmedFounding(storage, key)) {
    markUnconfirmedFounding(storage, key);
    try {
      const epochSigner = publisher.dsAccessSigner(identity);
      const root = wasm.dsJournalRootSigner(identity, groupId);
      try {
        await claimOrReset(new DsClient(), groupId, root, epochSigner.epoch, epochSigner.verifyingKey());
      } finally {
        epochSigner.free();
        root.free();
      }
      clearUnconfirmedFounding(storage, key);
    } catch (err) {
      // eslint-disable-next-line no-console
      console.warn("ds: PublisherGroup claim failed; the channel is not opened", err);
      try {
        publisher.free();
      } catch {
        /* already freed */
      }
      throw err;
    }
  }
  return publisher;
}

/** Where the unconfirmed-founding marker lives (group ids only). */
export interface UnconfirmedFoundingStorage {
  getItem(key: string): string | null;
  setItem(key: string, value: string): void;
  removeItem(key: string): void;
}

const FOUNDING_PREFIX = "lakko/publisher-group-unclaimed/v1/";

function defaultFoundingStorage(): UnconfirmedFoundingStorage | null {
  try {
    return typeof localStorage === "undefined" ? null : localStorage;
  } catch {
    return null;
  }
}

function foundingKey(groupId: Uint8Array): string {
  let hex = "";
  for (let i = 0; i < groupId.length; i++) hex += (groupId[i] ?? 0).toString(16).padStart(2, "0");
  return FOUNDING_PREFIX + hex;
}

function isUnconfirmedFounding(storage: UnconfirmedFoundingStorage | null, key: string): boolean {
  try {
    return storage?.getItem(key) !== null && storage?.getItem(key) !== undefined;
  } catch {
    return false;
  }
}

function markUnconfirmedFounding(storage: UnconfirmedFoundingStorage | null, key: string): void {
  try {
    storage?.setItem(key, "1");
  } catch {
    /* best effort */
  }
}

function clearUnconfirmedFounding(storage: UnconfirmedFoundingStorage | null, key: string): void {
  try {
    storage?.removeItem(key);
  } catch {
    /* best effort */
  }
}
