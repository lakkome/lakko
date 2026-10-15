// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * Joiner-side acceptor orchestration for link-based call
 * invites (Phase 3c3-acceptor-helper).
 *
 * The symmetric sibling of `composer.ts` — collapses the
 * seven-step joiner pipeline into a single discriminated
 * result so the `/c/<slug>` landing page can render the
 * right outcome copy without re-implementing the dispatch:
 *
 *   1. `parseCallInviteSlug(slug)` → pointer | inline | error
 *   2. (pointer branch) `DiscoveryClient.fetchCallInvite(blobId)`
 *      → bytes | null (404 → `notFound`)
 *   3. `CallInviteCapability.openLink(sealed, secret)` — opens
 *      the sealed blob with the fragment's secret and verifies
 *      the capability inside (a missing / malformed fragment, a
 *      substituted or tampered blob → `damaged`)
 *   4. `capability.isValidNow(nowMs)` — expiry filter
 *   5. (pointer branch) `DiscoveryClient.consumeCallInvite(
 *      blobId, capabilityBytes, joinerAccountId)` — atomic
 *      counter bump + per-joiner dedup tag
 *   6. `acceptCallInvite(capability, joinerIdentity)` —
 *      bootstrap a fresh MLS group with joiner as founder +
 *      owner added via the embedded KeyPackage
 *   7. Return `welcomeForOwner` bytes for the platform to
 *      ferry back to the owner (via stranger-inbox routed
 *      by `capability.ownerAccountId` or a future DS
 *      mailbox)
 *
 * The acceptor does NOT deliver the Welcome — that's the
 * platform's transport-layer job, deferred to the
 * 3c3-multi-use sprint. The UI gets the bytes back in the
 * `accepted` result so it can choose how to surface the
 * "share this with the owner" step.
 */

import { callLinkGrantVerifies, type LinkGrantCapability } from "./link-grant";
import { loadWasm } from "../wasm";
import {
  DiscoveryClient,
  DiscoveryHttpError,
  isDiscoveryRateLimited,
} from "../discovery/client";
import {
  inviteLinkWidths,
  isInviteLinkDamaged,
  openInviteLinkFromUrl,
} from "../contacts/invite-link";
import { CALL_INVITE_INLINE_MIN_LEN, parseCallInviteSlug } from "./invite";

// Re-exported by `loadWasm()` — the wasm module surfaces
// `CallInviteCapability` + `acceptCallInvite` + (joiner-side)
// `Identity`. The types below are the runtime shape returned
// from those wasm entry points.

/** Subset of the wasm `Identity` surface the acceptor needs.
 *  Mirror of the equivalent type in `composer.ts`. */
export interface JoinerIdentityHandle {
  /** Required to drive `wasm.acceptCallInvite(capability,
   *  identity)`. The acceptor passes the handle through
   *  unchanged. */
  readonly _identity_marker?: never;
}

/** Options bag the landing page hands to
 *  [`acceptCallInviteFromSlug`]. */
export interface AcceptCallInviteOptions {
  /** The `<slug>` segment from a `/c/<slug>` URL — base64url-
   *  no-pad of either a 6-byte ledger id (pointer form) or
   *  ≥ 400 bytes of inline capability (censored-region
   *  fallback). */
  readonly slug: string;
  /** The URL fragment (`location.hash`): the link secret the capability is
   *  sealed under (sealed-link amendment, 2026-09-28). Missing or malformed
   *  is `damaged`. */
  readonly hash: string | null | undefined;
  /** Joiner's 16-byte UUIDv7 account_id — folded into the
   *  per-joiner dedup tag locally before posting to consume.
   *  Never crosses the wire in plaintext. */
  readonly joinerAccountId: Uint8Array;
  /** Joiner's MLS identity handle — drives the
   *  `acceptCallInvite(capability, identity)` step that
   *  bootstraps the fresh call MLS group with joiner as
   *  founder + owner added via the embedded KP. */
  readonly joinerIdentity: unknown;
  /** Discovery client of the joiner. The consume spends one anonymous
   *  `invite-consume` token drawn with the joiner's session (ADR-0054 S3),
   *  so the joiner is signed in (a guest account on a `/c/` link) before
   *  this runs; the request itself carries no session. */
  readonly discoveryClient: DiscoveryClient;
  /** UNIX millis the acceptor uses for the
   *  `capability.isValidNow(...)` check. Defaults to
   *  `Date.now()`; tests inject a fixed clock. */
  readonly nowMs?: number;
}

/** Discriminated result. Every variant carries a non-failure
 *  payload OR a stable error tag — the landing page branches
 *  exhaustively without throwing exceptions. */
export type AcceptCallInviteResult =
  | {
      readonly kind: "accepted";
      /** Wire-format bytes the joiner just consumed. UI can
       *  re-render details (label, expiry) from these. */
      readonly capabilityBytes: Uint8Array;
      /** Owner's 16-byte UUIDv7 `account_id` — the Welcome
       *  ferry target. */
      readonly ownerAccountId: Uint8Array;
      /** 16-byte `call_id` embedded in the capability. UI
       *  hands this to the call-admission JWT mint that
       *  routes the joiner into the SFU. */
      readonly callId: Uint8Array;
      /** 71-char Signal-style safety number derived from
       *  the owner's Ed25519 pubkey. The landing page
       *  surfaces it under an "Is this really Alice?" prompt
       *  so the joiner verifies out-of-band (defence-in-
       *  depth against link-substitution attacks; calls.md
       *  C-28). */
      readonly ownerFingerprint: string;
      /** Wire-format MLS Welcome bytes the joiner must
       *  deliver to the owner. The platform's transport
       *  layer (stranger-inbox / future DS mailbox / QR
       *  paste) ferries this; the acceptor returns it
       *  without touching the network. */
      readonly welcomeForOwner: Uint8Array;
      /** 32-byte SFrame call-key seed derived via the MLS
       *  exporter (`mls_exporter("lakko/call/v1", call_id,
       *  32)`). UI hands it to `sframeDeriveParticipantKey`
       *  to seal the joiner's first frame BEFORE the owner
       *  has even joined — the seed is fixed for the call's
       *  initial epoch, so the joiner can pre-encrypt
       *  outbound media. */
      readonly callKeySeed: Uint8Array;
      /** The `AcceptedCallInvite` wasm object — the joiner's
       *  fresh MLS group + the live `GroupCallSession`. UI
       *  passes it to the in-call layer. */
      readonly accepted: unknown;
    }
  | {
      readonly kind: "alreadyJoined";
      /** The `(blob_id, joiner_tag)` PK on the server side
       *  already had a row — counter NOT bumped. */
      readonly capabilityBytes: Uint8Array;
    }
  | { readonly kind: "expired" }
  | { readonly kind: "exhausted" }
  | { readonly kind: "notFound" }
  /** 429 from the ledger on fetch or consume: the link may be fine, try
   *  again later. Never folded into `notFound`. */
  | { readonly kind: "rateLimited" }
  | { readonly kind: "malformedSlug"; readonly reason: string }
  /** The link does not open under its fragment: missing or malformed
   *  fragment, wrong secret, tampered or substituted blob. */
  | { readonly kind: "damaged" }
  | { readonly kind: "signatureFailure"; readonly reason: string }
  | { readonly kind: "failed"; readonly message: string };

/**
 * End-to-end "join call link" pipeline. The seven-step shape
 * documented at the top of the file maps onto a tight switch
 * over [`AcceptCallInviteResult`] variants — every observable
 * outcome is a non-thrown value the UI dispatches on.
 *
 * The only thrown exception path is a `DiscoveryNetworkError`
 * (no network / TLS failure / etc.) — the acceptor folds
 * `DiscoveryHttpError` into discriminated variants but lets
 * lower-level transport failures bubble up so the UI can
 * surface "you're offline" copy.
 *
 * Inline-capability slugs (censored-region fallback) skip
 * the `fetch` + `consume` steps entirely — `consume` on an
 * inline capability has no server-side row to bump. The
 * acceptor surfaces this asymmetry as `accepted` with
 * `capabilityBytes` populated; the `welcomeForOwner` still
 * needs ferrying via whatever transport the user has.
 */
export async function acceptCallInviteFromSlug(
  opts: AcceptCallInviteOptions,
): Promise<AcceptCallInviteResult> {
  // ---- 1-3. Parse, fetch, open under the fragment ----------
  // A malformed slug or fragment is refused before any request; the sealed
  // blob opens only under the fragment's secret, which also verifies the
  // capability inside (catches a substituted or tampered blob).
  const wasm = await loadWasm();
  let widths: ReturnType<typeof inviteLinkWidths>;
  try {
    widths = inviteLinkWidths(wasm);
    parseCallInviteSlug(opts.slug, widths);
  } catch (err) {
    return {
      kind: "malformedSlug",
      reason: err instanceof Error ? err.message : String(err),
    };
  }
  let capability: {
    readonly callId: Uint8Array;
    readonly ownerAccountId: Uint8Array;
    readonly expiryMs: bigint;
    readonly ownerFingerprint: string;
    readonly wire: Uint8Array;
    isValidNow(nowMs: bigint): boolean;
  };
  let blobId: Uint8Array | null;
  try {
    const opened = await openInviteLinkFromUrl({
      slug: opts.slug,
      hash: opts.hash,
      widths,
      inlineMinLen: CALL_INVITE_INLINE_MIN_LEN,
      what: "call-invite",
      fetchPointer: (id) => opts.discoveryClient.fetchCallInvite(id),
      open: (sealed, secret) => wasm.CallInviteCapability.openLink(sealed, secret),
    });
    if (opened.kind === "not-found") return { kind: "notFound" };
    capability = opened.capability;
    blobId = opened.blobId;
  } catch (err) {
    if (isInviteLinkDamaged(err)) return { kind: "damaged" };
    if (err instanceof DiscoveryHttpError && err.status === 404) {
      return { kind: "notFound" };
    }
    if (isDiscoveryRateLimited(err)) return { kind: "rateLimited" };
    throw err; // bubble up network / non-404 HTTP errors
  }
  // ADR-0078 - the link grant's mailbox binding must verify against the
  // owner's account key; a capability whose grant names another mailbox is
  // refused as damaged.
  if (!callLinkGrantVerifies(wasm, capability as unknown as LinkGrantCapability)) {
    return { kind: "damaged" };
  }
  // The PLAINTEXT wire the joiner tag is computed over, never the sealed blob.
  const capabilityBytes = capability.wire;

  // ---- 4. Expiry check ------------------------------------
  const nowMs = opts.nowMs ?? Date.now();
  if (!capability.isValidNow(BigInt(nowMs))) {
    return { kind: "expired" };
  }

  // ---- 5. Pointer branch: consume (counter bump + dedup) --
  if (blobId !== null) {
    let consumeResult: Awaited<
      ReturnType<DiscoveryClient["consumeCallInvite"]>
    >;
    try {
      consumeResult = await opts.discoveryClient.consumeCallInvite(
        blobId,
        capabilityBytes,
        opts.joinerAccountId,
      );
    } catch (err) {
      if (isDiscoveryRateLimited(err)) return { kind: "rateLimited" };
      return {
        kind: "failed",
        message: err instanceof Error ? err.message : String(err),
      };
    }
    if (consumeResult.kind === "not-found") {
      return { kind: "notFound" };
    }
    if (consumeResult.kind === "exhausted") {
      return { kind: "exhausted" };
    }
    if (consumeResult.kind === "already-consumed") {
      // The joiner has already accepted this exact invite
      // before. Server doesn't bump the counter; UI typically
      // surfaces "you're already in this call" + opens the
      // existing call group rather than re-bootstrapping.
      return { kind: "alreadyJoined", capabilityBytes };
    }
    // `consumed` — fall through to the accept step.
  }

  // ---- 6. acceptCallInvite via wasm — bootstrap the MLS
  //          call group + return the Welcome ----------------
  let accepted: {
    readonly welcomeForOwner: Uint8Array;
    readonly callKeySeed: Uint8Array;
  };
  try {
    // eslint-disable-next-line @typescript-eslint/no-explicit-any
    accepted = (wasm as any).acceptCallInvite(
      capability,
      opts.joinerIdentity,
    );
  } catch (err) {
    return {
      kind: "failed",
      message: err instanceof Error
        ? `accept_call_invite: ${err.message}`
        : `accept_call_invite: ${String(err)}`,
    };
  }

  // ---- 7. Done -------------------------------------------
  return {
    kind: "accepted",
    capabilityBytes,
    ownerAccountId: capability.ownerAccountId,
    callId: capability.callId,
    ownerFingerprint: capability.ownerFingerprint,
    welcomeForOwner: accepted.welcomeForOwner,
    callKeySeed: accepted.callKeySeed,
    accepted,
  };
}
