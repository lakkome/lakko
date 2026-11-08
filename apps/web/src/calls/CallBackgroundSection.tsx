// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * The `Background` section of the call's Settings sheet (UX
 * `docs/ux/call-backgrounds.md` section 2, ADR-0089): a 16:9 preview of the
 * camera as the others see it (not mirrored), the strip of choices, the hint.
 *
 * - With the camera on, the preview plays the track the call sends.
 * - With the camera off, opening the section starts the camera for the
 *   preview only: its own background controller, nothing published, the
 *   camera button stays off, and the line `call.background.preview_camera_off`
 *   shows. Closing the sheet stops that camera.
 * - A tap applies at once (`setBackgroundChoice`); the live call follows.
 * - `+` opens the browser's file picker for images, the same `<input
 *   type=file>` the profile photo uses. Right-click (and a long press on a
 *   touch screen) on an own photo offers `Remove`, with no confirmation.
 * - Arrow keys move along the strip, Enter or Space selects.
 */

import {
  For,
  Show,
  createEffect,
  createMemo,
  createSignal,
  onCleanup,
  onMount,
  untrack,
  type Component,
  type JSX,
} from "solid-js";

import { t, isRtl } from "../l10n";
import { callActions, callState } from "../state/call-state";
import { wasmSync } from "../wasm";
import { Icon } from "../ui/icons";
import {
  backgroundChoice,
  isBackground,
  onBackgroundChoiceChange,
  setBackgroundChoice,
  type BackgroundChoice,
} from "./background-choice";
import { CameraBackgroundController } from "./background-controller";
import {
  backgroundPhotos,
  notifyPhotosChanged,
  onPhotosChanged,
  preparePickedPhoto,
  type PhotoEntry,
} from "./background-photos";
import { cameraBackgroundState, sentCameraTrack, type CameraBackgroundState } from "./background-status";
import { isSelectedItem, nextStripIndex, selectedPhotoId, stripItems, type StripItem } from "./background-strip";
import { backgroundSupported } from "./background-support";

/** Side of a built-in thumbnail as drawn, in pixels. */
const THUMB_PX = 112;
/** A touch held this long opens the photo menu. */
const LONG_PRESS_MS = 500;

const builtinThumbs = new Map<string, string>();

/** A built-in background drawn small by the shared rule, as a data URL. */
function builtinThumb(id: string): string | undefined {
  const cached = builtinThumbs.get(id);
  if (cached !== undefined) return cached;
  try {
    const rgba = wasmSync().backgroundBuiltinRender(id, THUMB_PX, THUMB_PX);
    if (rgba === undefined) return undefined;
    const canvas = document.createElement("canvas");
    canvas.width = THUMB_PX;
    canvas.height = THUMB_PX;
    const ctx = canvas.getContext("2d");
    if (ctx === null) return undefined;
    ctx.putImageData(new ImageData(new Uint8ClampedArray(rgba), THUMB_PX, THUMB_PX), 0, 0);
    const url = canvas.toDataURL("image/png");
    builtinThumbs.set(id, url);
    return url;
  } catch {
    return undefined;
  }
}

function builtinIds(): string[] {
  try {
    return wasmSync().backgroundBuiltinIds();
  } catch {
    return [];
  }
}

function itemName(item: StripItem): string {
  if (item.kind === "add") return t("call.background.add_photo");
  const c = item.choice;
  switch (c.kind) {
    case "none":
      return t("call.background.none");
    case "blur":
      return t(c.strong ? "call.background.strong_blur" : "call.background.blur");
    case "builtin":
      return t("call.background.builtin_label", { name: t(`call.background.name.${c.id}`) });
    case "photo":
      return t("call.background.your_photo");
  }
}

/** The preview's video: plays `track`, not mirrored. */
const PreviewVideo: Component<{ track: MediaStreamTrack | null }> = (props) => {
  let el: HTMLVideoElement | undefined;
  createEffect(() => {
    const track = props.track;
    if (el === undefined) return;
    el.srcObject = track === null ? null : new MediaStream([track]);
    if (track !== null) void el.play().catch(() => undefined);
  });
  onCleanup(() => {
    if (el !== undefined) el.srcObject = null;
  });
  return <video ref={el} class="lk-call-bg-preview-video" muted playsinline autoplay aria-hidden="true" />;
};

export const CallBackgroundSection: Component = () => {
  const supported = backgroundSupported();
  const [choice, setChoice] = createSignal<BackgroundChoice>(backgroundChoice());
  onCleanup(onBackgroundChoiceChange(setChoice));

  // The section is absent where nothing can run, unless a background is
  // already selected: then it carries the failure line and the way out.
  const shown = (): boolean => supported || isBackground(choice());

  const [photos, setPhotos] = createSignal<PhotoEntry[]>([]);
  const [photoUrls, setPhotoUrls] = createSignal<ReadonlyMap<string, string>>(new Map());
  const [unusable, setUnusable] = createSignal(false);
  const [menu, setMenu] = createSignal<{ id: string; x: number; y: number } | null>(null);
  let loadSeq = 0;
  const loadPhotos = async (): Promise<void> => {
    const seq = ++loadSeq;
    const store = backgroundPhotos();
    const list = store === null ? [] : await store.list().catch(() => []);
    const urls = new Map<string, string>();
    for (const p of list) {
      const bytes = await store?.get(p.id).catch(() => null);
      if (bytes !== null && bytes !== undefined) urls.set(p.id, URL.createObjectURL(new Blob([bytes.slice()])));
    }
    if (seq !== loadSeq) {
      for (const url of urls.values()) URL.revokeObjectURL(url);
      return;
    }
    for (const url of photoUrls().values()) URL.revokeObjectURL(url);
    setPhotos(list);
    setPhotoUrls(urls);
  };
  onMount(() => void loadPhotos());
  onCleanup(onPhotosChanged(() => void loadPhotos()));
  onCleanup(() => {
    ++loadSeq;
    for (const url of photoUrls().values()) URL.revokeObjectURL(url);
  });

  const items = createMemo(() => stripItems(builtinIds(), photos()));

  // ----- the preview -----------------------------------------------------
  const cameraOn = (): boolean => callState.participants().find((p) => p.isLocal)?.camOn === true;
  const [previewTrack, setPreviewTrack] = createSignal<MediaStreamTrack | null>(null);
  const [previewState, setPreviewState] = createSignal<CameraBackgroundState>("off");
  let previewController: CameraBackgroundController | null = null;
  let previewSeq = 0;
  const stopPreviewCamera = (): void => {
    ++previewSeq;
    previewController?.dispose();
    previewController = null;
    setPreviewTrack(null);
    setPreviewState("off");
  };
  const startPreviewCamera = async (): Promise<void> => {
    const seq = ++previewSeq;
    const device = untrack(() => callState.settings().selectedVideoInput);
    let stream: MediaStream;
    try {
      stream = await navigator.mediaDevices.getUserMedia({
        video: device === null ? true : { deviceId: { exact: device } },
      });
    } catch {
      return;
    }
    const raw = stream.getVideoTracks()[0];
    if (raw === undefined || seq !== previewSeq) {
      for (const tr of stream.getTracks()) tr.stop();
      return;
    }
    const controller = new CameraBackgroundController({
      label: "preview",
      live: false,
      replaceCameraTrack: (track): Promise<void> => {
        setPreviewTrack(track);
        return Promise.resolve();
      },
      onState: (state, track) => {
        setPreviewState(state);
        setPreviewTrack(track);
      },
    });
    previewController = controller;
    setPreviewTrack(controller.adopt(raw));
  };
  // Camera off and the section shown: the preview runs its own camera.
  createEffect(() => {
    const own = shown() && supported && !cameraOn();
    untrack(() => {
      if (own) {
        if (previewController === null) void startPreviewCamera();
      } else {
        stopPreviewCamera();
      }
    });
  });
  onCleanup(stopPreviewCamera);

  const shownTrack = (): MediaStreamTrack | null => (cameraOn() ? sentCameraTrack() : previewTrack());
  const shownState = (): CameraBackgroundState => {
    if (!supported && isBackground(choice())) return "failed";
    return cameraOn() ? cameraBackgroundState() : previewState();
  };
  const failed = (): boolean => shownState() === "failed";

  const turnOnWithout = (): void => {
    setBackgroundChoice({ kind: "none" });
    callActions.setCamera(true);
  };

  // ----- choosing ---------------------------------------------------------
  let fileInput: HTMLInputElement | undefined;
  const buttons: HTMLButtonElement[] = [];
  const [focusIndex, setFocusIndex] = createSignal(0);
  createEffect(() => {
    const list = items();
    const at = list.findIndex((it) => isSelectedItem(it, choice()));
    setFocusIndex(at < 0 ? 0 : at);
  });

  const pick = (item: StripItem): void => {
    setMenu(null);
    if (item.kind === "add") {
      setUnusable(false);
      fileInput?.click();
      return;
    }
    setUnusable(false);
    setBackgroundChoice(item.choice);
  };

  const onFile = async (file: File): Promise<void> => {
    const store = backgroundPhotos();
    try {
      if (store === null) throw new Error("no account");
      const bytes = await preparePickedPhoto(file);
      const added = await store.add(bytes, selectedPhotoId(choice()));
      setUnusable(false);
      notifyPhotosChanged();
      setBackgroundChoice({ kind: "photo", id: added.id });
    } catch {
      setUnusable(true);
    }
  };

  const removePhoto = async (id: string): Promise<void> => {
    setMenu(null);
    const c = choice();
    await backgroundPhotos()?.remove(id).catch(() => undefined);
    if (c.kind === "photo" && c.id === id) setBackgroundChoice({ kind: "none" });
    notifyPhotosChanged();
  };

  const onStripKey = (event: KeyboardEvent): void => {
    const next = nextStripIndex(focusIndex(), event.key, items().length, isRtl());
    if (next === null) return;
    event.preventDefault();
    setFocusIndex(next);
    buttons[next]?.focus();
  };

  let pressTimer: ReturnType<typeof setTimeout> | undefined;
  const clearPress = (): void => {
    if (pressTimer !== undefined) clearTimeout(pressTimer);
    pressTimer = undefined;
  };
  onCleanup(clearPress);
  let suppressClick = false;

  const openMenu = (id: string, target: HTMLElement): void => {
    const rect = target.getBoundingClientRect();
    setMenu({ id, x: rect.left + rect.width / 2, y: rect.bottom + 4 });
  };

  createEffect(() => {
    if (menu() === null) return;
    const onKey = (e: KeyboardEvent): void => {
      if (e.key === "Escape") {
        e.stopPropagation();
        setMenu(null);
      }
    };
    document.addEventListener("keydown", onKey, true);
    onCleanup(() => document.removeEventListener("keydown", onKey, true));
  });

  const thumbBody = (item: StripItem): JSX.Element => {
    if (item.kind === "add") {
      return (
        <span class="lk-call-bg-thumb-face lk-call-bg-thumb-add" aria-hidden="true">
          <Icon name="plus" size={22} />
        </span>
      );
    }
    const c = item.choice;
    if (c.kind === "none") {
      return (
        <span class="lk-call-bg-thumb-face lk-call-bg-thumb-none" aria-hidden="true">
          <svg width="28" height="28" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.6">
            <circle cx="12" cy="12" r="8.5" />
            <path d="M6 18 18 6" />
          </svg>
        </span>
      );
    }
    if (c.kind === "blur") {
      return (
        <span
          class="lk-call-bg-thumb-face lk-call-bg-thumb-blur"
          classList={{ strong: c.strong }}
          aria-hidden="true"
        >
          <span class="lk-call-bg-thumb-blob a" />
          <span class="lk-call-bg-thumb-blob b" />
        </span>
      );
    }
    const src = c.kind === "builtin" ? builtinThumb(c.id) : photoUrls().get(c.id);
    return (
      <span class="lk-call-bg-thumb-face" aria-hidden="true">
        <Show when={src}>{(s) => <img src={s()} alt="" draggable={false} />}</Show>
      </span>
    );
  };

  const visibleLabel = (item: StripItem): string | null => {
    if (item.kind !== "choice") return null;
    const k = item.choice.kind;
    return k === "none" || k === "blur" ? itemName(item) : null;
  };

  return (
    <Show when={shown()}>
      <section class="lk-call-bg">
        <h3 id="lk-call-settings-background">{t("call.settings.background")}</h3>
        <div
          class="lk-call-bg-preview"
          role="img"
          aria-label={t("call.background.preview_label")}
        >
          <Show when={!failed() && shownTrack() !== null && shownState() !== "starting"}>
            <PreviewVideo track={shownTrack()} />
          </Show>
          <Show when={shownState() === "starting"}>
            <span class="lk-call-bg-preview-line" role="status" aria-live="polite">
              {t("call.background.preparing")}
            </span>
          </Show>
          <Show when={failed()}>
            <div class="lk-call-bg-preview-failed">
              <span role="alert">{t("call.background.failed")}</span>
              <button type="button" class="primary" onClick={turnOnWithout}>
                {t("call.background.failed_action")}
              </button>
            </div>
          </Show>
          <Show when={!cameraOn() && !failed()}>
            <span class="lk-call-bg-preview-note">{t("call.background.preview_camera_off")}</span>
          </Show>
        </div>
        <Show when={supported}>
          <div
            class="lk-call-bg-strip"
            role="group"
            aria-labelledby="lk-call-settings-background"
            onKeyDown={onStripKey}
          >
            <For each={items()}>
              {(item, index) => (
                <div class="lk-call-bg-item">
                  <button
                    type="button"
                    class="lk-call-bg-thumb"
                    classList={{ selected: isSelectedItem(item, choice()) }}
                    ref={(el) => {
                      buttons[index()] = el;
                    }}
                    tabIndex={index() === focusIndex() ? 0 : -1}
                    aria-label={itemName(item)}
                    aria-pressed={item.kind === "choice" ? isSelectedItem(item, choice()) : undefined}
                    onFocus={(): void => {
                      setFocusIndex(index());
                    }}
                    onClick={(): void => {
                      if (suppressClick) {
                        suppressClick = false;
                        return;
                      }
                      pick(item);
                    }}
                    onContextMenu={(e): void => {
                      if (item.kind !== "choice" || item.choice.kind !== "photo") return;
                      e.preventDefault();
                      openMenu(item.choice.id, e.currentTarget);
                    }}
                    onPointerDown={(e): void => {
                      if (e.pointerType !== "touch") return;
                      if (item.kind !== "choice" || item.choice.kind !== "photo") return;
                      const id = item.choice.id;
                      const target = e.currentTarget;
                      clearPress();
                      pressTimer = setTimeout(() => {
                        pressTimer = undefined;
                        suppressClick = true;
                        openMenu(id, target);
                      }, LONG_PRESS_MS);
                    }}
                    onPointerUp={clearPress}
                    onPointerCancel={clearPress}
                    onPointerLeave={clearPress}
                  >
                    {thumbBody(item)}
                  </button>
                  <Show when={visibleLabel(item)}>
                    {(label) => (
                      <span class="lk-call-bg-thumb-name" aria-hidden="true">
                        {label()}
                      </span>
                    )}
                  </Show>
                </div>
              )}
            </For>
          </div>
          <input
            ref={fileInput}
            type="file"
            accept="image/*"
            class="lk-visually-hidden"
            tabIndex={-1}
            aria-hidden="true"
            onChange={(e): void => {
              const input = e.currentTarget;
              const file = input.files?.[0];
              input.value = "";
              if (file !== undefined) void onFile(file);
            }}
          />
          <Show when={unusable()}>
            <span class="lk-call-bg-error" role="alert">
              {t("call.background.photo_unusable")}
            </span>
          </Show>
          <span class="lk-call-settings-hint">{t("call.settings.background_hint")}</span>
        </Show>
        <Show when={menu()}>
          {(m) => (
            <>
              <div class="lk-call-bg-menu-scrim" onClick={(): void => {
                  setMenu(null);
                }}
              />
              <div
                class="lk-call-menu lk-call-bg-menu"
                role="menu"
                style={{ left: `${m().x}px`, top: `${m().y}px` }}
                ref={(el): void => {
                  queueMicrotask(() => el.querySelector("button")?.focus());
                }}
              >
                <button
                  type="button"
                  role="menuitem"
                  class="lk-call-menu-item lk-call-menu-item-danger"
                  onClick={(): void => void removePhoto(m().id)}
                >
                  <Icon name="trash" size={18} />
                  <span>{t("call.background.remove")}</span>
                </button>
              </div>
            </>
          )}
        </Show>
      </section>
    </Show>
  );
};
