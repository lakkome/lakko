// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * Owner-side composer orchestration for link-based call
 * invites (Phase 3c3-composer-helper).
 *
 * Wraps the five-step "create a call link" pipeline into a
 * single async function so the UI doesn't have to know about
 * the call_id RNG, the one-time KeyPackage mint, the FFI
 * boundary, the discovery HTTP round-trip, or the URL builder
 * convention:
 *
 *   1. Take the live call's 16-byte `call_id` from the caller.
 *      A link is a property of a call that is already
 *      happening (`docs/ux/call-links.md` sections 1 and 3),
 *      so it must lead into THAT call and never into a new
 *      one — which is why the id is an argument and not
 *      something this function draws.
 *   2. Mint a one-time MLS `KeyPackage` via
 *      `identity.createKeyPackage()` — the joiner consumes
 *      it to bootstrap the call MLS group.
 *   3. A fresh `linkId` and the link's `link` reach grant
 *      (ADR-0060 section 6), whose key is registered on the
 *      owner's mailbox BEFORE the link exists — a link shared
 *      before its key is registered is a link that does not work.
 *   4. `composeCallInviteCapability(...)` via wasm — signs the
 *      capability under the owner's Ed25519 identity and seals
 *      it under a fresh link secret (`InviteLinkMint`).
 *   5. `DiscoveryClient.uploadCallInvite(...)` — pushes the
 *      sealed blob plus the clear fields to the ledger, which
 *      assigns a random 6-byte id (inline fallback on a
 *      transport failure only).
 *   6. `composeSealedInviteLink(...)` — builds the
 *      `https://<origin>/c/<slug>#<secret>` shape the
 *      owner shares.
 *
 * Sibling of `apps/web/src/contacts/composer.ts` (the
 * contact-invite composer); kept separate because the call-
 * invite path mints a fresh KP per link and embeds the media
 * flags — neither concern exists on the contact-invite side.
 *
 * The four fields a person used to be asked about — media,
 * expiry, use count, label — are now filled from
 * `call-invite-values.ts`, so this function takes wire-level
 * values rather than the form presets that fed it.
 */

import { loadWasm } from "../wasm";
import { DiscoveryClient, DiscoveryNetworkError } from "../discovery/client";
import { CALL_ID_LEN, CALL_INVITE_LABEL_MAX_LEN, CALL_INVITE_URL_PREFIX } from "./invite";
import { composeSealedInviteLink, inviteLinkWidths } from "../contacts/invite-link";
import { reachSession } from "../reach/reach-service";

/** Options bag the UI hands to [`composeAndUploadCallInvite`]. */
export interface ComposeCallInviteOptions {
  /** The owner's 32-byte Ed25519 signing seed — typically
   *  pulled from the device identity store via
   *  `chats.identity().ed25519Seed`. The seed is NOT
   *  persisted by this function; it lives only on the wasm
   *  stack during the one-shot `composeCallInviteCapability`
   *  call. */
  readonly ownerSigningSeed: Uint8Array;
  /** The owner's 16-byte UUIDv7 `account_id`. Embedded in
   *  the capability so the joiner can route the Welcome
   *  back to the right stranger-inbox. */
  readonly ownerAccountId: Uint8Array;
  /** The owner's MLS identity handle. Used to mint the
   *  one-time `KeyPackage` (`identity.createKeyPackage()`).
   *  The identity itself never crosses the wasm-bindgen
   *  boundary in raw form — we drive it through its public
   *  method. */
  readonly identity: { createKeyPackage(): Uint8Array };
  /** Discovery client bound to the owner's session token.
   *  The upload endpoint is session-gated for DoS / rate-
   *  limit; the cryptographic gate is the wire signature on
   *  the capability bytes. */
  readonly discoveryClient: DiscoveryClient;
  /** The live call's 16-byte `call_id`, so the link leads into
   *  the call the inviter is already in. */
  readonly callId: Uint8Array;
  /** Whether the call carries video as well as audio — the
   *  `CallInviteMedia` flag pair. Audio is always set. */
  readonly mediaVideo: boolean;
  /** The wire `expiry_ms`, UNIX millis. `0n` is the
   *  `CALL_NEVER_EXPIRES` sentinel and is never what this
   *  client sends — see `call-invite-values.ts`. */
  readonly expiryMs: bigint;
  /** The wire `max_uses` u32. `0` is the `CALL_SINGLE_USE`
   *  sentinel; `u32::MAX` is unbounded. */
  readonly maxUses: number;
  /** Free-text label the joiner sees on the landing page.
   *  UTF-8; hard cap at [`CALL_INVITE_LABEL_MAX_LEN`] bytes —
   *  the composer pre-validates so the UI sees a typed error
   *  rather than a wasm `JsError` round-trip. Empty since
   *  `docs/ux/call-links.md` section 4. */
  readonly label: string;
  /** Optional URL origin override. Defaults to
   *  `location.origin` (browser) or `https://lakko.me`
   *  (test). Pass a custom value when the owner wants to
   *  share under a CDN / vanity domain. */
  readonly origin?: string;
}

/** Output of [`composeAndUploadCallInvite`]. The UI surfaces
 *  `url` in the share sheet, keeps `blobId` + `linkId` for a
 *  revoke (see [`revokeCallInviteLink`]), and keeps `callId`
 *  around for the in-call signaling layer. */
export interface ComposedCallInvite {
  /** The 6-byte ledger id, or `null` for the inline link a transport
   *  failure fell back to. Pass to [`revokeCallInviteLink`] when the
   *  owner cancels the link. */
  readonly blobId: Uint8Array | null;
  /** The link's 16-byte reach scope id: revoking the link
   *  removes exactly this key from the owner's mailbox. */
  readonly linkId: Uint8Array;
  /** The full `https://<origin>/c/<slug>#<secret>` URL the
   *  owner shares. */
  readonly url: string;
  /** The 16-byte `call_id` embedded in the capability — the
   *  caller's live call. Returned so a caller that did not keep
   *  it can bind a call-admission JWT to the same id. */
  readonly callId: Uint8Array;
  /** The sealed blob the server now holds (opaque to it). */
  readonly sealed: Uint8Array;
  /** The 12-byte link secret (the URL fragment), for the device's own
   *  record of the link (`contacts/minted-links.ts`). */
  readonly linkSecret: Uint8Array;
}

/** Programmer-error path: input validation caught before any
 *  wasm / HTTP call. Distinct error class so the UI can
 *  surface form-level copy ("label too long") rather than a
 *  generic toast. */
export class CallInviteComposerArgError extends Error {
  override name = "CallInviteComposerArgError";
}

/**
 * End-to-end "create call link" pipeline. See
 * [`ComposeCallInviteOptions`] for inputs.
 *
 * Errors surface unchanged:
 *   - [`CallInviteComposerArgError`] on local validation
 *     (e.g. label too long, wrong-sized seed).
 *   - `JsError` from wasm on signature failure or wrong-
 *     sized inputs the wasm side catches.
 *   - `DiscoveryHttpError` / `DiscoveryNetworkError` from
 *     the upload step — the UI typically retries on the
 *     latter.
 *
 * The call MUST be driven from inside a user gesture (the
 * network round-trip inside `uploadCallInvite` can take a
 * moment on a slow connection and freezing it under a
 * synchronous click handler degrades UX). That cost is the
 * reason minting happens on the first `Invite` inside a call
 * rather than when the call starts.
 */
export async function composeAndUploadCallInvite(
  opts: ComposeCallInviteOptions,
): Promise<ComposedCallInvite> {
  // ---- 0. Local validation ---------------------------------
  if (opts.ownerSigningSeed.length !== 32) {
    throw new CallInviteComposerArgError(
      `ownerSigningSeed must be 32 bytes, got ${opts.ownerSigningSeed.length}`,
    );
  }
  if (opts.ownerAccountId.length !== 16) {
    throw new CallInviteComposerArgError(
      `ownerAccountId must be 16 bytes, got ${opts.ownerAccountId.length}`,
    );
  }
  const labelBytes = new TextEncoder().encode(opts.label);
  if (labelBytes.length > CALL_INVITE_LABEL_MAX_LEN) {
    throw new CallInviteComposerArgError(
      `label is ${labelBytes.length} bytes (UTF-8), max is ${CALL_INVITE_LABEL_MAX_LEN}`,
    );
  }

  // ---- 1. call_id: the live call's -------------------------
  if (opts.callId.length !== CALL_ID_LEN) {
    throw new CallInviteComposerArgError(
      `callId must be ${CALL_ID_LEN} bytes, got ${opts.callId.length}`,
    );
  }
  const callId = opts.callId;

  // ---- 2. One-time KeyPackage mint -------------------------
  const oneTimeKeyPackage = opts.identity.createKeyPackage();
  if (oneTimeKeyPackage.length === 0) {
    throw new CallInviteComposerArgError(
      "identity.createKeyPackage() returned 0 bytes",
    );
  }

  // ---- 3. Wire-level values --------------------------------
  const mediaAudio = true; // every call carries audio

  // ---- 4. The link's reach key, registered first ----------
  // ADR-0060 section 6: the joiner's first write to the owner's
  // closed mailbox is signed with this link's key, so it must be
  // registered before the link can be shared. No session, or a
  // registration that failed, fails the mint.
  const session = reachSession();
  if (session === null) {
    throw new CallInviteComposerArgError("no reach session: sign in to create a call link");
  }
  const wasm = await loadWasm();
  const linkId = wasm.generateLinkId();
  const expiry = Number(opts.expiryMs);
  const linkGrant = await session.newLink(linkId, expiry === 0 ? null : expiry);

  // ---- 5. composeCallInviteCapability via wasm -------------
  // Sealed-link amendment of ADR-0009 / ADR-0028 (2026-09-28): the compose
  // returns the sealed blob, the 12-byte link secret for the fragment, and
  // the issuer key the upload carries in clear for the revoke.
  let mint: import("../../wasm/lakko_ffi_wasm").InviteLinkMint;
  try {
    mint = wasm.composeCallInviteCapability(
      opts.ownerSigningSeed,
      opts.ownerAccountId,
      callId,
      opts.expiryMs,
      opts.maxUses,
      mediaAudio,
      opts.mediaVideo,
      oneTimeKeyPackage,
      opts.label,
      linkId,
      linkGrant,
    );
  } catch (err) {
    await session.revokeLink(linkId).catch(() => undefined);
    throw err;
  } finally {
    linkGrant.free();
  }
  const sealed = mint.sealed;
  const linkSecret = mint.linkSecret;
  const issuerPubkey = mint.issuerPubkey;
  mint.free();

  // ---- 6. uploadCallInvite via HTTP ------------------------
  // The inline sealed form is the fallback for a TRANSPORT failure only; a
  // server answer surfaces as an error (and the key of a link that was
  // never handed out is removed).
  let blobId: Uint8Array | null;
  try {
    blobId = await opts.discoveryClient.uploadCallInvite({
      sealed,
      issuerPubkey,
      expiryMs: opts.expiryMs,
      maxUses: opts.maxUses,
    });
  } catch (err) {
    if (!(err instanceof DiscoveryNetworkError)) {
      await session.revokeLink(linkId).catch(() => undefined);
      throw err;
    }
    // eslint-disable-next-line no-console
    console.warn("call-invite: ledger unreachable, using the inline link");
    blobId = null;
  }

  // ---- 7. URL build ----------------------------------------
  // The fragment carries the secret; a link without it opens nothing.
  const url = composeSealedInviteLink({
    origin: opts.origin ?? defaultOrigin(),
    prefix: CALL_INVITE_URL_PREFIX,
    blobId,
    sealed,
    secret: linkSecret,
    widths: inviteLinkWidths(wasm),
  });

  return {
    blobId,
    linkId,
    url,
    callId,
    sealed,
    linkSecret,
  };
}

/**
 * Revoke a call link the owner minted: the ledger row (owner-signed
 * delete) and the link's reach key (ADR-0060 section 6), so a joiner
 * who already holds the link can neither fetch it nor write to the
 * owner's mailbox under it. The key goes even when the ledger answers
 * "not found" (the row may already be gone), and an inline link (no
 * ledger row) only loses its key.
 */
export async function revokeCallInviteLink(opts: {
  readonly discoveryClient: DiscoveryClient;
  readonly blobId: Uint8Array | null;
  readonly linkId: Uint8Array;
  readonly ownerSigningSeed: Uint8Array;
}): Promise<void> {
  try {
    if (opts.blobId !== null) {
      await opts.discoveryClient.revokeCallInvite(opts.blobId, opts.ownerSigningSeed);
    }
  } finally {
    await reachSession()?.revokeLink(opts.linkId);
  }
}

function defaultOrigin(): string {
  return typeof location !== "undefined" ? location.origin : "https://lakko.me";
}
