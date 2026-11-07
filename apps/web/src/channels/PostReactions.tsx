// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * The reaction chips under a channel post (UX `group-and-channel-privacy.md`
 * 13.1, `reactions-and-emoji.md` 7, 8.3, 10, 11): the emoji and the number,
 * always, since nobody's avatar can be known; the person's own highlighted.
 * A tap adds the person's reaction or takes it off; a long press or a right
 * click opens the note that channel reactions are anonymous. Until the counts
 * arrive a post shows no chips.
 *
 * Adding a NEW emoji is the post menu's `React` (the channel view), which
 * calls {@link toggleChannelPostReaction} like a chip tap does.
 */
import { createMemo, createSignal, For, onCleanup, onMount, Show, type JSX } from "solid-js";

import { useAuth } from "../auth/AuthContext";
import { useChats } from "../chat/ChatContext";
import { requestConfirm } from "../ui/confirm-dialog";
import { createLongPress } from "../ui/long-press";
import { t } from "../l10n";
import { wasmSync } from "../wasm";
import { channelAllowedReactions } from "./ban-list";
import { bindChannelFeedback } from "./ChannelFeedbackRuntime";
import { channelSeedFor } from "./channel-access";
import { bytesOfHex } from "./feedback-api";
import { ownReactionsOn } from "./own-feedback-register";
import {
  ChannelReactionCapError,
  drawnCounts,
  postLabelCounts,
  postOverlay,
  reactionCandidates,
  recentReactionsFor,
  requestReactionCounts,
  resolveLabelCounts,
  toggleChannelPostReaction,
} from "./post-feedback";

export { channelAllowedReactions, requestReactionCounts, toggleChannelPostReaction };

/** How long the cap line stays under the chips (reactions-and-emoji 10). */
const CAP_LINE_MS = 3000;

interface ChannelChip {
  readonly emoji: string;
  readonly count: number;
  readonly mine: boolean;
}

/** The anonymity note (reactions-and-emoji 8.3): one sentence, one `OK`. */
export function showChannelReactionNote(): void {
  void requestConfirm({
    title: t("channels.reactions.anonymous_note"),
    body: null,
    confirmLabel: t("common.ok"),
    cancelLabel: t("common.cancel"),
    tone: "neutral",
    notice: true,
  });
}

export function PostReactions(props: {
  channelIdHex: string;
  cursor: number;
  epoch: number;
  postAtMs: number;
  canReact: boolean;
}): JSX.Element {
  const chats = useChats();
  const auth = useAuth();
  bindChannelFeedback(auth, chats);
  const [capLine, setCapLine] = createSignal(false);
  let capTimer: ReturnType<typeof setTimeout> | undefined;
  onCleanup(() => {
    if (capTimer !== undefined) clearTimeout(capTimer);
  });

  // The counts of this post join the view's one batched call.
  onMount(() => requestReactionCounts(props.channelIdHex, [props.cursor]));

  const channelId = createMemo(() => bytesOfHex(props.channelIdHex));
  const own = (): string[] => ownReactionsOn(channelId(), props.cursor);

  /** The service's counts resolved to emoji, or `null` before they came. */
  const resolved = createMemo((): Map<string, number> | null => {
    const labels = postLabelCounts(props.channelIdHex, props.cursor);
    if (labels === undefined) return null;
    const seed = channelSeedFor(props.channelIdHex, props.epoch);
    // A post this device cannot open shows no chips.
    if (seed === undefined) return null;
    if (labels.size === 0) return new Map();
    const s = auth.state();
    const account = s.kind === "session" ? s.account.accountId : null;
    try {
      return resolveLabelCounts(
        wasmSync(),
        seed,
        channelId(),
        props.epoch,
        props.cursor,
        labels,
        reactionCandidates(own(), recentReactionsFor(account)),
      );
    } catch {
      return new Map();
    }
  });

  const chips = createMemo((): ChannelChip[] => {
    const base = resolved();
    if (base === null && own().length === 0) return [];
    const mine = own();
    const counts = drawnCounts(base ?? new Map(), mine, postOverlay(props.channelIdHex, props.cursor));
    const allowed = channelAllowedReactions(props.channelIdHex);
    const entries = [...counts].filter(([e]) =>
      allowed.mode === "all" ? true : allowed.mode === "only" ? allowed.emoji.includes(e) : false,
    );
    // The core's chip order (most first, at most eleven); the order the
    // service's labels resolved in stands in for "first given".
    try {
      const w = wasmSync();
      const sources = entries.map(
        ([emoji, count], i) => new w.ChipSource(emoji, count, BigInt(i), mine.includes(emoji), new Uint8Array()),
      );
      const a =
        allowed.mode === "off"
          ? w.AllowedReactions.off()
          : allowed.mode === "only"
            ? w.AllowedReactions.only([...allowed.emoji])
            : w.AllowedReactions.all();
      try {
        const row = w.reactionChips(sources, "channel", a);
        try {
          return row.chips.map((c) => {
            const chip = { emoji: c.emoji, count: c.count, mine: mine.includes(c.emoji) };
            c.free();
            return chip;
          });
        } finally {
          row.free();
        }
      } finally {
        a.free();
      }
    } catch {
      return entries
        .sort((x, y) => y[1] - x[1])
        .slice(0, 11)
        .map(([emoji, count]) => ({ emoji, count, mine: mine.includes(emoji) }));
    }
  });

  const press = async (emoji: string): Promise<void> => {
    if (!props.canReact) return;
    try {
      await toggleChannelPostReaction(props.channelIdHex, props.cursor, props.epoch, emoji, props.postAtMs);
    } catch (err) {
      if (err instanceof ChannelReactionCapError) {
        setCapLine(true);
        if (capTimer !== undefined) clearTimeout(capTimer);
        capTimer = setTimeout(() => setCapLine(false), CAP_LINE_MS);
      }
    }
  };

  const longPress = createLongPress(() => showChannelReactionNote());

  return (
    <Show when={chips().length > 0 || capLine()}>
      <div class="lk-reactions lk-post-reactions">
        <For each={chips()}>
          {(chip) => {
            const label = (): string =>
              chip.mine
                ? t("chat.reaction.chip_yours_a11y", { emoji: chip.emoji })
                : t("chat.reaction.chip_a11y", { emoji: chip.emoji, count: chip.count });
            return (
              <button
                type="button"
                class="lk-reaction"
                classList={{ mine: chip.mine }}
                aria-label={label()}
                aria-pressed={chip.mine}
                disabled={!props.canReact}
                onContextMenu={(e) => {
                  e.preventDefault();
                  showChannelReactionNote();
                }}
                onPointerDown={longPress.onPointerDown}
                onPointerMove={longPress.onPointerMove}
                onPointerUp={longPress.onPointerUp}
                onPointerCancel={longPress.onPointerCancel}
                onPointerLeave={longPress.onPointerLeave}
                onClick={(e) => {
                  if (longPress.swallowClick(e)) return;
                  e.stopPropagation();
                  void press(chip.emoji);
                }}
              >
                <span class="lk-reaction-emoji">{chip.emoji}</span>
                <span class="lk-reaction-count">{chip.count}</span>
              </button>
            );
          }}
        </For>
      </div>
      <Show when={capLine()}>
        <p class="lk-reaction-cap" role="status">
          {t("chat.reaction.cap_reached")}
        </p>
      </Show>
    </Show>
  );
}
