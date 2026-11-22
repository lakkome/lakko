// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * The one avatar circle. Initials, tinted by one of four colours chosen by a
 * hash of the id so the same person always gets the same colour on every
 * device; and, for a person (`personKey`), their profile photo when this
 * device holds it (ADR-0084, docs/ux/profile-screen.md section 7). A profile
 * video never has sound; it plays looped when the caller's `playVideo` mode
 * says so (`ui/avatar-video.ts`): always in the large views, while the
 * pointer is over the row in Chats and Contacts, in the chat header while
 * hovered (always on a touch-only device). Everywhere else its still.
 */
import { createSignal, Match, onCleanup, onMount, Switch, type JSX } from "solid-js";

import { avatarMediaFor, conversationAvatarMediaFor, type AvatarMedia } from "../profile/avatar-runtime";
import { wasmSync } from "../wasm";
import { isDeletedAccount } from "../people/person-status";
import { Icon } from "./icons";
import { paintVideoBehind } from "../media/video-overlay";
import { avatarVideoPlays, deviceCanHover, pointerHovers, type AvatarVideoMode } from "./avatar-video";

/** What a hover over counts for: the circle's row or header button, else the
 *  circle itself. */
const HOVER_TARGET = ".lk-row, .lk-chat-title";

/**
 * The initials of a display label: the shared rule in the core
 * (`avatarInitials`, design-system.md Avatar bullet), the same on every
 * client. No initials logic lives here; an empty answer, or a core not loaded
 * yet, shows `placeholder`. Reactive: re-runs once the core has loaded.
 */
export function initialsOf(label: string, placeholder = "?"): string {
  try {
    const initials = wasmSync().avatarInitials(label);
    return initials.length > 0 ? initials : placeholder;
  } catch {
    return placeholder;
  }
}

/** The person glyph's box for an avatar size (about half the circle). */
function glyphSize(size: "sm" | "md" | "lg" | "xl" | "profile" | undefined): number {
  switch (size) {
    case "sm":
      return 16;
    case "lg":
      return 28;
    case "xl":
      return 48;
    case "profile":
      return 56;
    default:
      return 22;
  }
}

/** One of the four avatar tints, stable per id. */
export function toneOf(id: string): 1 | 2 | 3 | 4 {
  let h = 0;
  for (let i = 0; i < id.length; i += 1) h = (h * 31 + id.charCodeAt(i)) >>> 0;
  return ((h % 4) + 1) as 1 | 2 | 3 | 4;
}

export function Avatar(props: {
  /** What the initials come from. */
  label: string;
  /** What the tint comes from (defaults to the label). */
  id?: string;
  size?: "sm" | "md" | "lg" | "xl" | "profile";
  class?: string;
  /** The person's key (the one their profile is filed under), or `self` for
   *  the signed-in account: the photo is looked up for it. */
  personKey?: string | undefined;
  /** A group or channel (its id, base64): its photo is looked up instead of
   *  a person's (ADR-0087 section 18, UX 4.5.5). */
  conversationKey?: string | undefined;
  /** Media to show instead of the looked-up photo (a pending change on the
   *  profile screen); `null` forces the initials. */
  media?: AvatarMedia | null | undefined;
  /** When a profile video plays (`ui/avatar-video.ts`): `true` always (large
   *  views), `"hover"` while the row is hovered, `"header"` the chat header. */
  playVideo?: AvatarVideoMode;
  /** Draw the group glyph instead of initials when there is no photo (a new
   *  group or channel before its name is typed), as iOS and Android do. */
  groupGlyph?: boolean;
}): JSX.Element {
  const [broken, setBroken] = createSignal<string | null>(null);
  const [hovering, setHovering] = createSignal(false);
  const canHover = deviceCanHover();
  let circle: HTMLSpanElement | undefined;
  onMount(() => {
    const mode = props.playVideo;
    if ((mode !== "hover" && mode !== "header") || !canHover || circle === undefined) return;
    const target: Element = circle.closest(HOVER_TARGET) ?? circle;
    const enter = (e: Event): void => {
      if (pointerHovers((e as PointerEvent).pointerType)) setHovering(true);
    };
    const leave = (): void => {
      setHovering(false);
    };
    target.addEventListener("pointerenter", enter);
    target.addEventListener("pointerleave", leave);
    onCleanup(() => {
      target.removeEventListener("pointerenter", enter);
      target.removeEventListener("pointerleave", leave);
    });
  });
  // Asking for the video is what downloads it, so it is asked for only
  // while it is to play.
  const plays = (): boolean => avatarVideoPlays(props.playVideo, hovering(), canHover);
  // ADR-0098: a deleted account is the neutral person glyph: no photo, no
  // initials, the neutral surface colour.
  const deleted = (): boolean =>
    props.personKey !== undefined && props.personKey !== "self" && isDeletedAccount(props.personKey);
  const media = (): AvatarMedia | null => {
    if (deleted()) return null;
    if (props.media !== undefined) return props.media;
    if (props.conversationKey !== undefined) {
      return conversationAvatarMediaFor(props.conversationKey, plays());
    }
    return avatarMediaFor(props.personKey, plays());
  };
  const video = (): string | null => {
    const m = media();
    return plays() && m?.videoUrl !== null && m?.videoUrl !== undefined && broken() !== m.videoUrl
      ? m.videoUrl
      : null;
  };
  const still = (): string | null => {
    const m = media();
    return m?.stillUrl !== null && m?.stillUrl !== undefined && broken() !== m.stillUrl ? m.stillUrl : null;
  };
  return (
    <span
      ref={circle}
      class={`lk-avatar ${props.size ?? ""} ${props.class ?? ""}`}
      data-tone={toneOf(props.personKey ?? props.id ?? props.label)}
      data-photo={video() !== null || still() !== null ? "true" : undefined}
      data-deleted={deleted() ? "true" : undefined}
      aria-hidden="true"
    >
      <Switch fallback={initialsOf(props.label)}>
        <Match when={deleted()}>
          <Icon name="person-fill" size={glyphSize(props.size)} />
        </Match>
        <Match when={video()}>
          {(url) => (
            <video
              ref={paintVideoBehind}
              class="lk-avatar-media"
              src={url()}
              poster={still() ?? undefined}
              muted
              loop
              autoplay
              playsinline
              onError={() => setBroken(url())}
            />
          )}
        </Match>
        <Match when={still()}>
          {(url) => <img class="lk-avatar-media" src={url()} alt="" onError={() => setBroken(url())} />}
        </Match>
        <Match when={props.groupGlyph === true}>
          <Icon name="person-3-fill" size={glyphSize(props.size)} />
        </Match>
      </Switch>
    </span>
  );
}
