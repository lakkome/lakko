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

import { loadWasm } from "../wasm";

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
 * Fresh `(seed, signingKeySeed)` tuple — the local
 * publisher state the web UI needs to produce posts
 * before the MLS-per-channel keying lands. Stored
 * in-memory only in this sub-sprint; G4-2-storage
 * persists it to the device-group-keyed sqlite cache.
 */
export interface ChannelPublisherKeys {
  /** 32-byte content-key seed.
   *
   *  **Demo only.** Production seeds come out of the
   *  PublisherGroup's MLS exporter (ADR-0010 Tier 1) —
   *  the `seedHex` you see in `channels-state` is a
   *  single-publisher local-only seed until the MLS-per-
   *  channel sprint lands. */
  readonly seed: Uint8Array;
  /** 32-byte Ed25519 signing-key seed used to derive the
   *  `ChannelSigningKey` for every post. Persisted as a
   *  seed (not a key handle) so the JS side can re-build
   *  the WASM class after a page reload. */
  readonly signingKeySeed: Uint8Array;
}

/** Build a fresh `(seed, signingKeySeed)` pair. */
export async function freshPublisherKeys(): Promise<ChannelPublisherKeys> {
  const wasm = await loadWasm();
  const seed = wasm.newChannelSeedForTest();
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
