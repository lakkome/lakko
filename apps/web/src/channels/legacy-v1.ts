// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * Readable surfacing of the retired v1 PublisherGroup derivation.
 *
 * `crates/lakko-mls/src/publisher_group.rs` narrowed the PublisherGroup MLS
 * group-id prefix from `b"lakko/publisher-group/v1/"` (41 bytes total) to
 * `b"lakko/pubgrp/v2/"` (32 bytes) so the id fits the Delivery Service's
 * 32-byte pseudonym width. An MLS `group_id` is bound into `GroupContext` and
 * every epoch secret derived from it, so v1 state **cannot** be re-keyed in
 * place — `PublisherGroup::load` / `::create` refuse loudly instead, and
 * already-minted `/g/` co-publisher links carry a v1 id that no longer resolves
 * to a channel.
 *
 * Both refusals are correct and neither is repairable. This module turns them
 * from raw Rust error strings into copy a channel owner can act on, and — the
 * load-bearing part — does so **narrowly**, so an unrelated failure (network,
 * server rejection, a bad channel-id hex) still reads as itself.
 *
 * ## What each detection keys on
 *
 * - **The dead invite link** keys on the *bytes*: a 41-byte `group_id` carrying
 *   the retired v1 prefix. This is structural and cannot drift — the v1 prefix
 *   is a frozen historical constant (nothing new is ever minted under it), and a
 *   v2 id is a different length. See {@link isLegacyV1PublisherGroupId}.
 *
 * - **The unopenable channel** keys on a substring of the Rust refusal message.
 *   That is genuinely fragile: `lakko_mls::Error::Group(String)` is stringly
 *   typed, and both FFI boundaries (`JsError::new(&format!("{e}"))` on wasm,
 *   `FfiError::Group(m)` on UniFFI) carry the payload through verbatim with no
 *   discriminant, so there is nothing else to match on. An edit to the message
 *   in `refuse_if_legacy_v1_present` would silently fall back to the generic
 *   error text with no compile-time or test-time signal from this side. The
 *   durable fix is a typed error variant in the core; that is a `crates/`
 *   change and deliberately not made here. See {@link isLegacyPublisherGroupError}.
 *
 * The copy is catalog text (`channels.legacy.*`, ADR-0061); a
 * {@link LegacyChannelError} is a {@link KeyedError}, so a screen renders it
 * in the language current when it is shown.
 */
import { t } from "../l10n";
import { KeyedError } from "../l10n/keyed-error";

/**
 * The retired v1 group-id prefix, as ASCII bytes. Frozen: nothing mints under
 * this any more, so this constant is historical and cannot change.
 * Mirrors `PUBLISHER_GROUP_ID_PREFIX_V1` in `crates/lakko-mls/src/publisher_group.rs`.
 */
const V1_PREFIX = "lakko/publisher-group/v1/";

/** Total byte length of a v1 PublisherGroup id: 25-byte prefix + 16-byte channel id. */
const V1_GROUP_ID_LEN = V1_PREFIX.length + 16;

/**
 * Distinctive substring of the Rust-side refusal raised by
 * `refuse_if_legacy_v1_present`. Matched case-sensitively; kept as narrow as
 * possible while staying unique to that one call site.
 */
const RUST_REFUSAL_MARKER = "legacy v1 PublisherGroup state";

/** User-facing copy for a channel whose MLS state predates the v2 derivation. */
export const LEGACY_CHANNEL_MESSAGE = (): string => t("channels.legacy.channel");

/** User-facing copy for a `/g/` co-publisher link minted under the v1 derivation. */
export const LEGACY_COPUBLISHER_LINK_MESSAGE = (): string => t("channels.legacy.copublisher_link");

/** Which of the two retired-v1 surfaces a {@link LegacyChannelError} describes. */
export type LegacyChannelKind = "channel" | "invite-link";

/**
 * A retired-v1 refusal, already carrying its user-facing message.
 *
 * Thrown at the two boundaries where the condition is actually detectable, so
 * every UI call site discriminates on the *type* (`instanceof`) rather than
 * re-doing a string match. There is exactly one place in the web client that
 * inspects an error string for this, and it is {@link isLegacyPublisherGroupError}.
 */
export class LegacyChannelError extends KeyedError {
  /** Which surface refused. */
  readonly kind: LegacyChannelKind;

  constructor(kind: LegacyChannelKind) {
    super(kind === "channel" ? "channels.legacy.channel" : "channels.legacy.copublisher_link");
    this.name = "LegacyChannelError";
    this.kind = kind;
  }
}

/**
 * True when `groupId` carries the retired 41-byte v1 PublisherGroup shape.
 *
 * Structural: length plus the frozen ASCII prefix. A v2 id is 32 bytes, and a
 * chat / DeviceGroup id is 32 bytes of non-matching content, so this never
 * fires on anything current.
 */
export function isLegacyV1PublisherGroupId(groupId: Uint8Array): boolean {
  if (groupId.length !== V1_GROUP_ID_LEN) return false;
  for (let i = 0; i < V1_PREFIX.length; ++i) {
    if (groupId[i] !== V1_PREFIX.charCodeAt(i)) return false;
  }
  return true;
}

/**
 * True when `err` is the Rust-side refusal to open a v1 PublisherGroup.
 *
 * The single string-matching site in the web client — see the module doc for
 * why no better handle exists and why this is fragile.
 */
export function isLegacyPublisherGroupError(err: unknown): boolean {
  if (err instanceof LegacyChannelError) return err.kind === "channel";
  const text =
    err instanceof Error ? err.message : typeof err === "string" ? err : String(err);
  return text.includes(RUST_REFUSAL_MARKER);
}

/**
 * Re-throw `err` as a {@link LegacyChannelError} when it is the retired-v1
 * refusal; otherwise re-throw it untouched so unrelated failures keep their own
 * text.
 *
 * @throws the original error, or a {@link LegacyChannelError}
 */
export function rethrowAsLegacyChannel(err: unknown): never {
  if (isLegacyPublisherGroupError(err)) throw new LegacyChannelError("channel");
  throw err;
}

/**
 * Message to show for `err`: the retired-v1 copy when it applies, otherwise
 * `fallback(err)` so a network / server / codec failure still reads as itself.
 */
export function channelErrorMessage(err: unknown, fallback: (e: unknown) => string): string {
  if (err instanceof LegacyChannelError) return err.text();
  if (isLegacyPublisherGroupError(err)) return LEGACY_CHANNEL_MESSAGE();
  return fallback(err);
}
