// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * One channel post as a card (docs/ux/client-redesign.md section 5.4): the
 * text, or "Not available" with one plain reason, and the time. A channel
 * post carries no author on the wire, so the card shows none.
 */
import { Show, type JSX } from "solid-js";

import type { ChannelPostView } from "./api";
import { MessageText } from "../chat/MessageText";
import { t } from "../l10n";
import { unavailableReason, type PostDecode } from "./post-decode";
import { postTimeLabel } from "./relative-time";

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
        fallback={<div class="lk-post-placeholder">{t("common.loading")}</div>}
      >
        {(d) => (
          <Show
            when={d().kind === "text" ? (d() as { kind: "text"; text: string }) : null}
            fallback={
              <div class="lk-post-unavailable">
                <strong>{t("channels.post.not_available")}</strong>
                <span>{unavailableReason(d())}</span>
              </div>
            }
          >
            {(post) => <div class="lk-post-text"><MessageText text={post().text} /></div>}
          </Show>
        )}
      </Show>
      <div class="lk-post-time">
        <time dateTime={new Date(props.post.serverTsMs).toISOString()}>{time()}</time>
      </div>
    </li>
  );
}
