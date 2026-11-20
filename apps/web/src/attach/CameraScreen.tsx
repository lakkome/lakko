// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * `docs/ux/attachments.md` section 5 — Lakko's own capture view
 * (`getUserMedia`). Photo: one frame drawn to a canvas and encoded as JPEG
 * (no metadata). Video: MP4 / H.264 through `MediaRecorder`, at most 60 s; a
 * browser that cannot record MP4 hides the Video switch. The browser's
 * permission prompt is the only system surface, at the first open of this
 * view. Every track is stopped when the view closes.
 *
 * Which camera: the one chosen for calls when there is one, else the default
 * device (a phone-class browser asks for the front camera). The switch button
 * moves through the cameras by device id. A camera that gives no frame within
 * `CAMERA_FIRST_FRAME_MS` counts as failed: the next camera is tried once,
 * then the no-camera state is shown, never a black picture.
 */
import { createSignal, onCleanup, onMount, Show, type JSX } from "solid-js";

import { t } from "../l10n";
import { paintVideoBehind } from "../media/video-overlay";
import { callState } from "../state/call-state";
import { Icon } from "../ui/icons";
import {
  CAMERA_FIRST_FRAME_MS,
  CAMERA_VIDEO_MAX_MS,
  cameraVideoConstraint,
  isPhoneClassBrowser,
  nextCameraId,
  pickVideoMime,
} from "./capture-rules";
import { PHOTO_JPEG_QUALITY } from "./photo-encode";
import "./editors.css";

type Mode = "photo" | "video";
type Phase = "starting" | "live" | "denied" | "failed";

/** Resolves true once the element has a frame, false after `ms` without one. */
function waitForFrame(el: HTMLVideoElement, ms: number): Promise<boolean> {
  return new Promise((resolve) => {
    let done = false;
    const finish = (ok: boolean): void => {
      if (done) return;
      done = true;
      clearTimeout(timer);
      el.removeEventListener("loadeddata", onData);
      el.removeEventListener("resize", onData);
      resolve(ok);
    };
    const onData = (): void => {
      if (el.videoWidth > 0) finish(true);
    };
    const timer = setTimeout(() => finish(el.videoWidth > 0), ms);
    if (typeof el.requestVideoFrameCallback === "function") {
      el.requestVideoFrameCallback(() => finish(true));
    } else {
      el.addEventListener("loadeddata", onData);
      el.addEventListener("resize", onData);
      onData();
    }
  });
}

function stamp(): string {
  const d = new Date();
  const p = (n: number): string => String(n).padStart(2, "0");
  return `${d.getFullYear()}${p(d.getMonth() + 1)}${p(d.getDate())}-${p(d.getHours())}${p(d.getMinutes())}${p(d.getSeconds())}`;
}

export function CameraScreen(props: {
  readonly onCancel: () => void;
  readonly onCaptured: (files: File[]) => void;
}): JSX.Element {
  const [phase, setPhase] = createSignal<Phase>("starting");
  const [mode, setMode] = createSignal<Mode>("photo");
  const [recording, setRecording] = createSignal(false);
  const [cameraIds, setCameraIds] = createSignal<string[]>([]);
  const phoneClass = typeof navigator !== "undefined" && isPhoneClassBrowser(navigator as Navigator & {
    readonly userAgentData?: { readonly mobile?: boolean };
  });
  /** The camera to open: the one chosen for calls until the switch moves on. */
  let wantedId: string | null = callState.settings().selectedVideoInput;
  /** The camera the open stream comes from. */
  let currentId: string | null = null;
  const videoMime =
    typeof MediaRecorder !== "undefined" && typeof MediaRecorder.isTypeSupported === "function"
      ? pickVideoMime((m) => MediaRecorder.isTypeSupported(m))
      : null;
  let video: HTMLVideoElement | undefined;
  let stream: MediaStream | null = null;
  let recorder: MediaRecorder | null = null;
  let chunks: Blob[] = [];
  let limitTimer: ReturnType<typeof setTimeout> | null = null;
  let closed = false;

  function stopStream(): void {
    stream?.getTracks().forEach((track) => track.stop());
    stream = null;
  }

  function attach(el: HTMLVideoElement): void {
    if (stream === null || el.srcObject === stream) return;
    el.srcObject = stream;
    void el.play().catch(() => undefined);
  }


  async function listCameras(): Promise<string[]> {
    try {
      const devices = await navigator.mediaDevices.enumerateDevices();
      return devices.filter((d) => d.kind === "videoinput" && d.deviceId.length > 0).map((d) => d.deviceId);
    } catch {
      return [];
    }
  }

  /** Open `deviceId` (or the default camera); `retry` is false on the first
   *  attempt, so a camera without a picture hands over to the next one once. */
  async function open(deviceId: string | null = wantedId, retry = false): Promise<void> {
    stopStream();
    setPhase("starting");
    if (typeof navigator === "undefined" || navigator.mediaDevices?.getUserMedia === undefined) {
      setPhase("failed");
      return;
    }
    let s: MediaStream;
    try {
      s = await navigator.mediaDevices.getUserMedia({
        video: cameraVideoConstraint({ deviceId, phoneClass }),
        audio: mode() === "video",
      });
    } catch (err) {
      const name = (err as { name?: string }).name;
      if (name === "NotAllowedError" || name === "SecurityError") {
        setPhase("denied");
        return;
      }
      // A chosen camera that is gone (unplugged, another computer's id):
      // fall back to the default device once.
      if (!retry && deviceId !== null) {
        await open(null, true);
        return;
      }
      setPhase("failed");
      return;
    }
    if (closed) {
      s.getTracks().forEach((track) => track.stop());
      return;
    }
    stream = s;
    currentId = s.getVideoTracks()[0]?.getSettings().deviceId ?? deviceId;
    const el = video;
    if (el === undefined) {
      setPhase("failed");
      stopStream();
      return;
    }
    attach(el);
    const ids = await listCameras();
    setCameraIds(ids);
    const hasFrame = await waitForFrame(el, CAMERA_FIRST_FRAME_MS);
    if (closed || stream !== s) return;
    if (hasFrame) {
      setPhase("live");
      return;
    }
    stopStream();
    const next = retry ? null : nextCameraId(ids, currentId);
    if (next !== null) {
      wantedId = next;
      await open(next, true);
      return;
    }
    setPhase("failed");
  }

  onMount(() => {
    void open();
    const onKey = (e: KeyboardEvent): void => {
      if (e.key !== "Escape" || e.defaultPrevented) return;
      e.preventDefault();
      props.onCancel();
    };
    document.addEventListener("keydown", onKey);
    onCleanup(() => document.removeEventListener("keydown", onKey));
  });
  onCleanup(() => {
    closed = true;
    if (limitTimer !== null) clearTimeout(limitTimer);
    if (recorder !== null && recorder.state !== "inactive") {
      recorder.ondataavailable = null;
      recorder.onstop = null;
      recorder.stop();
    }
    stopStream();
  });

  async function takePhoto(): Promise<void> {
    if (video === undefined || video.videoWidth === 0) return;
    const canvas = document.createElement("canvas");
    canvas.width = video.videoWidth;
    canvas.height = video.videoHeight;
    const ctx = canvas.getContext("2d");
    if (ctx === null) return;
    ctx.drawImage(video, 0, 0, canvas.width, canvas.height);
    const blob = await new Promise<Blob | null>((resolve) =>
      canvas.toBlob(resolve, "image/jpeg", PHOTO_JPEG_QUALITY),
    );
    if (blob === null) return;
    props.onCaptured([new File([blob], `photo-${stamp()}.jpg`, { type: "image/jpeg" })]);
  }

  function startRecording(): void {
    if (stream === null || videoMime === null) return;
    chunks = [];
    try {
      recorder = new MediaRecorder(stream, { mimeType: videoMime });
    } catch {
      return;
    }
    recorder.ondataavailable = (e): void => {
      if (e.data.size > 0) chunks.push(e.data);
    };
    recorder.onstop = (): void => {
      setRecording(false);
      if (limitTimer !== null) clearTimeout(limitTimer);
      limitTimer = null;
      if (chunks.length === 0) return;
      const blob = new Blob(chunks, { type: "video/mp4" });
      props.onCaptured([new File([blob], `video-${stamp()}.mp4`, { type: "video/mp4" })]);
    };
    recorder.start(1000);
    setRecording(true);
    limitTimer = setTimeout(() => stopRecording(), CAMERA_VIDEO_MAX_MS);
  }

  function stopRecording(): void {
    if (recorder !== null && recorder.state !== "inactive") recorder.stop();
  }

  function shutter(): void {
    if (phase() !== "live") return;
    if (mode() === "photo") void takePhoto();
    else if (recording()) stopRecording();
    else startRecording();
  }

  function switchMode(next: Mode): void {
    if (recording() || mode() === next) return;
    setMode(next);
    // Video needs the microphone too; reopen with it, on the same camera.
    void open(currentId ?? wantedId);
  }

  function switchCamera(): void {
    if (recording()) return;
    const next = nextCameraId(cameraIds(), currentId);
    if (next === null) return;
    wantedId = next;
    void open(next);
  }

  return (
    <section class="lk-attach-camera" role="dialog" aria-modal="true" aria-label={t("attach.menu.camera")}>
      <div class="lk-attach-camera-top">
        <button
          type="button"
          class="lk-icon-button"
          aria-label={t("common.close")}
          title={t("common.close")}
          onClick={() => props.onCancel()}
        >
          <Icon name="xmark" />
        </button>
        <Show when={cameraIds().length > 1 && phase() === "live"}>
          <button
            type="button"
            class="lk-icon-button"
            aria-label={t("camera.switch")}
            title={t("camera.switch")}
            disabled={recording()}
            onClick={() => switchCamera()}
          >
            <Icon name="camera" />
          </button>
        </Show>
      </div>
      <Show
        when={phase() === "live" || phase() === "starting"}
        fallback={
          <div class="lk-attach-camera-msg" role="alert">
            <Show when={phase() === "denied"} fallback={<p>{t("attach.menu.no_camera")}</p>}>
              <p>{t("camera.denied")}</p>
              <p>{t("camera.allow_browser")}</p>
            </Show>
          </div>
        }
      >
        <div class="lk-attach-camera-view">
          <video
            ref={(el) => {
              video = el;
              paintVideoBehind(el);
              attach(el);
            }}
            autoplay
            muted
            playsinline
          />
        </div>
      </Show>
      <div class="lk-attach-camera-bottom">
        <Show when={videoMime !== null} fallback={<p class="lk-attach-note">{t("camera.video_unsupported")}</p>}>
          <div class="lk-attach-segments" role="radiogroup" aria-label={t("attach.menu.camera")}>
            <button type="button" role="radio" aria-checked={mode() === "photo"} onClick={() => switchMode("photo")}>
              {t("camera.photo")}
            </button>
            <button type="button" role="radio" aria-checked={mode() === "video"} onClick={() => switchMode("video")}>
              {t("camera.video")}
            </button>
          </div>
        </Show>
        <button
          type="button"
          class="lk-attach-shutter"
          classList={{ recording: recording() }}
          disabled={phase() !== "live"}
          aria-label={
            mode() === "photo"
              ? t("camera.shutter_photo")
              : recording()
                ? t("camera.shutter_stop")
                : t("camera.shutter_record")
          }
          onClick={() => shutter()}
        >
          <span />
        </button>
      </div>
    </section>
  );
}
