// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * `docs/ux/attachments.md` section 5 — Lakko's own capture view
 * (`getUserMedia`). Photo: one frame drawn to a canvas and encoded as JPEG
 * (no metadata). Video: MP4 / H.264 through `MediaRecorder`, at most 60 s; a
 * browser that cannot record MP4 hides the Video switch. The browser's
 * permission prompt is the only system surface, at the first open of this
 * view. Every track is stopped when the view closes.
 */
import { createSignal, onCleanup, onMount, Show, type JSX } from "solid-js";

import { t } from "../l10n";
import { Icon } from "../ui/icons";
import { CAMERA_VIDEO_MAX_MS, pickVideoMime } from "./capture-rules";
import { PHOTO_JPEG_QUALITY } from "./photo-encode";
import "./editors.css";

type Mode = "photo" | "video";
type Phase = "starting" | "live" | "denied" | "failed";

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
  const [cameraCount, setCameraCount] = createSignal(0);
  const [facing, setFacing] = createSignal<"user" | "environment">("user");
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

  async function open(): Promise<void> {
    stopStream();
    setPhase("starting");
    if (typeof navigator === "undefined" || navigator.mediaDevices?.getUserMedia === undefined) {
      setPhase("failed");
      return;
    }
    try {
      const s = await navigator.mediaDevices.getUserMedia({
        video: { facingMode: facing(), width: { ideal: 1920 }, height: { ideal: 1080 } },
        audio: mode() === "video",
      });
      if (closed) {
        s.getTracks().forEach((track) => track.stop());
        return;
      }
      stream = s;
      if (video !== undefined) {
        video.srcObject = s;
        void video.play().catch(() => undefined);
      }
      setPhase("live");
      try {
        const devices = await navigator.mediaDevices.enumerateDevices();
        setCameraCount(devices.filter((d) => d.kind === "videoinput").length);
      } catch {
        setCameraCount(1);
      }
    } catch (err) {
      const name = (err as { name?: string }).name;
      setPhase(name === "NotAllowedError" || name === "SecurityError" ? "denied" : "failed");
    }
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
    // Video needs the microphone too; reopen with it.
    void open();
  }

  function switchCamera(): void {
    if (recording()) return;
    setFacing(facing() === "user" ? "environment" : "user");
    void open();
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
          <Icon name="close" />
        </button>
        <Show when={cameraCount() > 1 && phase() === "live"}>
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
        <video ref={video} autoplay muted playsinline />
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
