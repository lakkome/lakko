// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * One channel post as a card (docs/ux/client-redesign.md section 5.4): the
 * text, or "Not available" with one plain reason, and the time. A channel
 * post carries no author on the wire, so the card shows none.
 */
import { Show, type JSX } from "solid-js";

import type { ChannelPostView } from "./api";
import { utf8Decode, type DecodedPostResult } from "./composer";
import { postTimeLabel } from "./relative-time";

/** The three post-render outcomes (ADR-0031 decrypt-by-epoch):
 *  - `text`       — decrypted plaintext.
 *  - `unreadable` — the device had the material for this post's epoch but
 *                   the open failed (tampered / foreign signer).
 *  - `older-key`  — no material exists for the post's epoch (older than the
 *                   retained window, or a gap). NOT force-decrypted with a
 *                   wrong-epoch seed (invariant #4). */
export type PostDecode =
  | { readonly kind: "text"; readonly text: string }
  | { readonly kind: "unreadable" }
  | { readonly kind: "older-key" };

/** Map the composer's low-level decode result onto a render outcome. */
export function toPostDecode(res: DecodedPostResult): PostDecode {
  if (res.plaintext !== null) {
    return { kind: "text", text: utf8Decode(res.plaintext) };
  }
  return res.hadSeed ? { kind: "unreadable" } : { kind: "older-key" };
}

/** The one-line reason under "Not available", in plain words. */
export function unavailableReason(decoded: Exclude<PostDecode, { kind: "text" }>): string {
  return decoded.kind === "older-key"
    ? "This post is older than what this device can read."
    : "This post could not be opened on this device.";
}

export function PostCard(props: {
  readonly post: ChannelPostView;
  /** `undefined` while the decode has not completed yet. */
  readonly decoded: PostDecode | undefined;
  /** The clock the time label is relative to. */
  readonly now?: Date;
}): JSX.Element {
  const time = (): string => postTimeLabel(props.post.serverTsMs, props.now ?? new Date());
  return (
    <li class="lk-post" classList={{ unavailable: props.decoded !== undefined && props.decoded.kind !== "text" }}>
      <Show
        when={props.decoded}
        fallback={<div class="lk-post-placeholder">Loading...</div>}
      >
        {(d) => (
          <Show
            when={d().kind === "text" ? (d() as { kind: "text"; text: string }) : null}
            fallback={
              <div class="lk-post-unavailable">
                <strong>Not available</strong>
                <span>{unavailableReason(d() as Exclude<PostDecode, { kind: "text" }>)}</span>
              </div>
            }
          >
            {(t) => <div class="lk-post-text">{t().text}</div>}
          </Show>
        )}
      </Show>
      <div class="lk-post-time">
        <time dateTime={new Date(props.post.serverTsMs).toISOString()}>{time()}</time>
      </div>
    </li>
  );
}
