// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * The one avatar circle. Initials, tinted by one of four colours chosen by a
 * hash of the id so the same person always gets the same colour on every
 * device; and, for a person (`personKey`), their profile photo when this
 * device holds it (ADR-0084, docs/ux/profile-screen.md section 7). A profile
 * video plays muted and looped only where the caller asks (`playVideo`: the
 * large views); everywhere else its still is shown.
 */
import { createSignal, Match, Switch, type JSX } from "solid-js";

import { avatarMediaFor, conversationAvatarMediaFor, type AvatarMedia } from "../profile/avatar-runtime";
import { wasmSync } from "../wasm";

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
  /** Play a profile video (large views only). */
  playVideo?: boolean;
}): JSX.Element {
  const [broken, setBroken] = createSignal<string | null>(null);
  const media = (): AvatarMedia | null => {
    if (props.media !== undefined) return props.media;
    if (props.conversationKey !== undefined) {
      return conversationAvatarMediaFor(props.conversationKey, props.playVideo === true);
    }
    return avatarMediaFor(props.personKey, props.playVideo === true);
  };
  const video = (): string | null => {
    const m = media();
    return props.playVideo === true && m?.videoUrl !== null && m?.videoUrl !== undefined && broken() !== m.videoUrl
      ? m.videoUrl
      : null;
  };
  const still = (): string | null => {
    const m = media();
    return m?.stillUrl !== null && m?.stillUrl !== undefined && broken() !== m.stillUrl ? m.stillUrl : null;
  };
  return (
    <span
      class={`lk-avatar ${props.size ?? ""} ${props.class ?? ""}`}
      data-tone={toneOf(props.id ?? props.label)}
      data-photo={video() !== null || still() !== null ? "true" : undefined}
      aria-hidden="true"
    >
      <Switch fallback={initialsOf(props.label)}>
        <Match when={video()}>
          {(url) => (
            <video
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
      </Switch>
    </span>
  );
}
