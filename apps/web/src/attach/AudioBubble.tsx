// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * `docs/ux/attachments.md` section 6 — a music or audio file. Playable only
 * when the MIME is on the inline list (the parent decides `playable`);
 * otherwise a file chip with the same title and artist line and `Save`. The
 * bytes are fetched on the first press of play, through `src`.
 */
import { createSignal, onCleanup, Show, type JSX } from "solid-js";

import { t } from "../l10n";
import { Icon } from "../ui/icons";
import { claimPlayback, releasePlayback } from "./audio-exclusive";
import { formatClock } from "./poll-view";
import "./bubbles.css";

export function AudioBubble(props: {
  readonly title: string;
  readonly artist?: string | undefined;
  readonly durationMs?: number | undefined;
  readonly coverUrl?: string | undefined;
  readonly src: () => Promise<string | null>;
  readonly playable: boolean;
  readonly onSave: () => void;
}): JSX.Element {
  const [playing, setPlaying] = createSignal(false);
  const [loading, setLoading] = createSignal(false);
  const [failed, setFailed] = createSignal(false);
  const [position, setPosition] = createSignal(0);
  const [duration, setDuration] = createSignal((props.durationMs ?? 0) / 1000);
  let audio: HTMLAudioElement | undefined;

  onCleanup(() => {
    if (audio !== undefined) {
      audio.pause();
      releasePlayback(audio);
    }
  });

  const ensureAudio = async (): Promise<HTMLAudioElement | null> => {
    if (audio !== undefined) return audio;
    setLoading(true);
    let url: string | null = null;
    try {
      url = await props.src();
    } catch {
      url = null;
    } finally {
      setLoading(false);
    }
    if (url === null) {
      setFailed(true);
      return null;
    }
    const el = new Audio(url);
    el.preload = "auto";
    el.addEventListener("timeupdate", () => setPosition(el.currentTime));
    el.addEventListener("loadedmetadata", () => {
      if (Number.isFinite(el.duration) && el.duration > 0) setDuration(el.duration);
    });
    el.addEventListener("play", () => {
      claimPlayback(el);
      setPlaying(true);
    });
    el.addEventListener("pause", () => setPlaying(false));
    el.addEventListener("ended", () => {
      setPlaying(false);
      setPosition(0);
    });
    el.addEventListener("error", () => {
      setPlaying(false);
      setFailed(true);
    });
    audio = el;
    return el;
  };

  const toggle = async (): Promise<void> => {
    if (playing()) {
      audio?.pause();
      return;
    }
    const el = await ensureAudio();
    if (el === null) return;
    try {
      setFailed(false);
      await el.play();
    } catch {
      setFailed(true);
    }
  };

  const seek = (seconds: number): void => {
    setPosition(seconds);
    if (audio !== undefined) audio.currentTime = seconds;
  };

  const titleLine = (): JSX.Element => (
    <>
      <span class="lk-audio-title lk-ak-ellipsis">{props.title}</span>
      <Show when={(props.artist ?? "").length > 0}>
        <span class="lk-ak-muted lk-ak-ellipsis lk-ak-small">{props.artist}</span>
      </Show>
    </>
  );

  return (
    <Show
      when={props.playable}
      fallback={
        <div class="lk-audio-bubble">
          <span class="lk-location-pin" aria-hidden="true">
            <Icon name="music-note" size={20} />
          </span>
          <div class="lk-audio-main">{titleLine()}</div>
          <button type="button" class="lk-ak-secondary-button" onClick={() => props.onSave()}>
            {t("common.save")}
          </button>
        </div>
      }
    >
      <div class="lk-audio-bubble">
        <button
          type="button"
          class="lk-audio-play"
          aria-label={t(playing() ? "audio.pause" : "audio.play", { title: props.title })}
          aria-busy={loading()}
          onClick={() => void toggle()}
        >
          <Show when={loading()} fallback={<AudioGlyph playing={playing()} />}>
            <span class="lk-ak-spinner" aria-hidden="true" />
          </Show>
        </button>
        <div class="lk-audio-main">
          {titleLine()}
          <input
            class="lk-audio-scrub"
            type="range"
            min="0"
            max={Math.max(1, duration())}
            step="0.1"
            value={position()}
            aria-label={props.title}
            aria-valuetext={`${formatClock(position())} / ${formatClock(duration())}`}
            disabled={audio === undefined && !playing()}
            onInput={(e) => seek(Number(e.currentTarget.value))}
          />
          <span class="lk-audio-time">
            {`${formatClock(position())} / ${formatClock(duration())}`}
          </span>
          <Show when={failed()}>
            <span class="lk-ak-error" role="status">{t("audio.unplayable")}</span>
          </Show>
        </div>
        <Show when={props.coverUrl !== undefined}>
          <img class="lk-audio-cover" src={props.coverUrl} alt="" />
        </Show>
      </div>
    </Show>
  );
}

/** A play triangle or pause bars, drawn inline (the icon set has neither). */
function AudioGlyph(props: { readonly playing: boolean }): JSX.Element {
  return (
    <svg width="18" height="18" viewBox="0 0 24 24" aria-hidden="true" fill="currentColor">
      <Show when={props.playing} fallback={<path d="M7 4.5v15l13-7.5z" />}>
        <path d="M6 4h4v16H6zM14 4h4v16h-4z" />
      </Show>
    </svg>
  );
}
