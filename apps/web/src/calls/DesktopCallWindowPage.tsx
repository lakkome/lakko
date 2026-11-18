// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * The page of the collapsed call's own window in the desktop app
 * (`docs/ux/calls.md` section 4.5.4a).
 *
 * A separate webview, loaded from `call-window.html`: it does not start the
 * app, holds no keys, reaches no server (the host refuses it everything but
 * its own routes and static files, `apps/desktop/src-tauri/src/proxy.rs`) and
 * knows nothing about the call beyond what the main page relays. It draws
 * section 4.5.1's layout from that: the picture and the own-camera inset as
 * images, the state or the time with the lock, the name and `+N`, the mute
 * button. A press that moves becomes the OS moving the window; one that does
 * not is a click, and brings the call back in the main window; the mute
 * button mutes and does nothing else.
 *
 * The host pushes with `window.__lakkoCallWindow.view(..)` / `.frame(..)` /
 * `.clear()`; the page reads the current state once when it loads.
 */

import { Show, createSignal, onCleanup, onMount, type JSX } from "solid-js";

import { Icon, isIconName, type IconName } from "../ui/icons";
import {
  callWindowFrameUrl,
  fetchCallWindowState,
  postCallWindowAction,
  startCallWindowDrag,
  type CallWindowSlot,
} from "../platform/host";
import {
  osInsetCorner,
  parseCallWindowView,
  pressIsDrag,
  type CallWindowPicture,
  type CallWindowView,
} from "./desktop-call-window-model";

/** An icon name that arrived as text, or `null` for one the set lacks. */
function iconOf(name: string): IconName | null {
  return isIconName(name) ? name : null;
}

/** The avatar colour of an initials picture. */
function toneOf(picture: CallWindowPicture): string | undefined {
  return picture.kind === "initials" ? String(picture.tone) : undefined;
}

/** The letters of an initials picture. */
function initialsOf(picture: CallWindowPicture): string {
  return picture.kind === "initials" ? picture.text : "";
}

/** What the host calls on this page. */
export interface CallWindowPageApi {
  view(v: unknown): void;
  frame(slot: CallWindowSlot, seq: number | null): void;
  clear(): void;
}

declare global {
  interface Window {
    __lakkoCallWindow?: CallWindowPageApi;
  }
}

/**
 * One image slot: the newest frame the host announced, fetched, decoded and
 * then swapped in, so the picture never blanks between two frames. One fetch
 * at a time; a frame announced meanwhile is fetched next, and any older one
 * is skipped.
 */
function frameSlot(slot: CallWindowSlot): {
  readonly url: () => string | null;
  readonly announce: (seq: number | null) => void;
} {
  const [url, setUrl] = createSignal<string | null>(null);
  let wanted: number | null = null;
  let busy = false;
  let shownUrl: string | null = null;
  let epoch = 0;

  const show = (next: string | null): void => {
    const old = shownUrl;
    shownUrl = next;
    setUrl(next);
    if (old !== null) URL.revokeObjectURL(old);
  };

  const pump = async (): Promise<void> => {
    if (busy) return;
    busy = true;
    try {
      while (wanted !== null) {
        const seq = wanted;
        const at = epoch;
        wanted = null;
        try {
          const res = await fetch(callWindowFrameUrl(slot, seq), { cache: "no-store" });
          if (!res.ok) continue;
          const objectUrl = URL.createObjectURL(await res.blob());
          const img = new Image();
          img.src = objectUrl;
          await img.decode().catch(() => undefined);
          // Cleared while it loaded: the old call's picture is not shown.
          if (at !== epoch) {
            URL.revokeObjectURL(objectUrl);
            continue;
          }
          show(objectUrl);
        } catch {
          // The next frame is fetched instead.
        }
      }
    } finally {
      busy = false;
    }
  };

  onCleanup(() => show(null));

  return {
    url,
    announce: (seq) => {
      if (seq === null) {
        epoch += 1;
        wanted = null;
        show(null);
        return;
      }
      wanted = seq;
      void pump();
    },
  };
}

export function DesktopCallWindowPage(): JSX.Element {
  const [view, setView] = createSignal<CallWindowView | null>(null);
  const picture = frameSlot("picture");
  const inset = frameSlot("inset");
  const [insetCorner, setInsetCorner] = createSignal<"bottom-left" | "bottom-right">("bottom-left");

  const applyView = (v: unknown): void => {
    const parsed = parseCallWindowView(v);
    setView(parsed);
    if (parsed !== null) {
      document.documentElement.lang = parsed.lang;
      document.documentElement.dir = parsed.dir;
    }
  };

  const api: CallWindowPageApi = {
    view: applyView,
    frame: (slot, seq) => {
      (slot === "picture" ? picture : inset).announce(typeof seq === "number" ? seq : null);
    },
    clear: () => {
      setView(null);
      picture.announce(null);
      inset.announce(null);
    },
  };
  window.__lakkoCallWindow = api;
  onCleanup(() => {
    if (window.__lakkoCallWindow === api) delete window.__lakkoCallWindow;
  });

  onMount(() => {
    void fetchCallWindowState().then((state) => {
      if (state === null) return;
      applyView(state.view);
      picture.announce(state.picture);
      inset.announce(state.inset);
    });
    // The inset sits in the bottom corner away from the screen edge the
    // window is nearer to (§4.5.1); the window can be dragged anywhere, so
    // where that is is looked at again now and then.
    const placeInset = (): void => {
      const s = window.screen as Screen & { availLeft?: number };
      setInsetCorner(osInsetCorner(window.screenX, window.innerWidth, s.availLeft ?? 0, s.availWidth));
    };
    placeInset();
    const timer = window.setInterval(placeInset, 1_000);
    onCleanup(() => window.clearInterval(timer));
  });

  // ----- a press: click or drag --------------------------------------------

  let press: { x: number; y: number; dragging: boolean } | null = null;
  const onPointerDown = (event: PointerEvent): void => {
    if (event.button !== 0) return;
    press = { x: event.screenX, y: event.screenY, dragging: false };
  };
  const onPointerMove = (event: PointerEvent): void => {
    if (press === null || press.dragging) return;
    if ((event.buttons & 1) === 0) {
      press = null;
      return;
    }
    if (pressIsDrag(event.screenX - press.x, event.screenY - press.y)) {
      press.dragging = true;
      // The OS moves the window from here until the button is released.
      void startCallWindowDrag();
    }
  };
  const onPointerUp = (event: PointerEvent): void => {
    const current = press;
    press = null;
    if (current === null || current.dragging || event.button !== 0) return;
    void postCallWindowAction("expand");
  };
  const onPointerCancel = (): void => {
    press = null;
  };

  return (
    <Show when={view()}>
      {(v) => (
        <div
          class="lk-call-window lk-call-window-os"
          role="region"
          aria-label={v().region}
          data-shape="landscape"
        >
          <div
            class="lk-call-tile lk-call-window-picture"
            data-tone={toneOf(v().picture)}
          >
            <Show when={v().picture.kind === "frame" && picture.url()}>
              {(u) => <img class="lk-call-window-frame" src={u()} alt="" draggable={false} />}
            </Show>
            <Show when={v().picture.kind === "blank"}>
              <div class="lk-call-tile-blank" aria-hidden="true">
                <Icon name="video" size={24} />
              </div>
            </Show>
            <Show when={v().picture.kind === "initials"}>
              <div class="lk-call-tile-avatar" aria-hidden="true">
                {initialsOf(v().picture)}
              </div>
            </Show>
          </div>

          <Show when={v().inset && inset.url()}>
            {(u) => (
              <div class="lk-call-window-inset" data-corner={insetCorner()} aria-hidden="true">
                <img class="lk-call-window-frame" src={u()} alt="" draggable={false} />
              </div>
            )}
          </Show>

          <div class="lk-call-window-top" aria-hidden="true">
            <span class="lk-call-window-state">{v().status}</span>
            <Show when={v().encrypted}>
              <span class="lk-call-window-mark">
                <Icon name="lock-fill" size={14} />
              </span>
            </Show>
          </div>

          <div class="lk-call-window-bottom" aria-hidden="true">
            <span class="lk-call-window-name">{v().name}</span>
            <Show when={v().others > 0}>
              <span class="lk-call-window-more">{`+${v().others}`}</span>
            </Show>
          </div>

          {/* The whole window: a click brings the call back, a drag moves it. */}
          <div
            class="lk-call-window-press"
            role="button"
            aria-label={v().spoken}
            onPointerDown={onPointerDown}
            onPointerMove={onPointerMove}
            onPointerUp={onPointerUp}
            onPointerCancel={onPointerCancel}
          />

          <Show when={v().mute}>
            {(m) => (
              <button
                type="button"
                class="lk-call-window-mute"
                data-tone={m().tone}
                aria-label={m().label}
                aria-pressed={m().pressed ?? undefined}
                title={m().label}
                disabled={m().disabled}
                onPointerDown={(event): void => event.stopPropagation()}
                onClick={(event): void => {
                  event.stopPropagation();
                  void postCallWindowAction("mute");
                }}
              >
                <Show when={iconOf(m().icon)}>{(name) => <Icon name={name()} size={20} />}</Show>
              </button>
            )}
          </Show>

          <span class="lk-visually-hidden" role="status" aria-live="polite">
            {v().announced}
          </span>
        </div>
      )}
    </Show>
  );
}
