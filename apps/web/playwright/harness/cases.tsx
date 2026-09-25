// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * One entry per scenario a real-browser spec opens, keyed by `?case=`.
 *
 * Every component below is imported from `src/` and rendered with canned
 * props. That is the whole discipline of this file: a fixture that rebuilt a
 * component's markup would measure the fixture, so nothing here draws a
 * bubble, a waveform, a record button or a menu. What a case IS allowed to do
 * is stand in for a collaborator that is not part of the question — the two
 * name resolvers (`context-stubs.ts`) and the network, which is replaced by
 * filing the media bytes locally through the production hub's own
 * `fileLocal`, exactly as an outgoing take's bytes are filed before upload.
 *
 * No sign-in, no MLS identity, no service. There are no test credentials and
 * there is no server to talk to, so an end-to-end route through the real app
 * is not on the table; a component with known data in a real browser is.
 */

import { createMemo, createSignal, For, Show, type JSX } from "solid-js";

import { mergeConversationEntries } from "../../src/chat/call-timeline";
import { TimelineBubble, type BubbleHost } from "../../src/chat/TimelineBubble";
import { rowCreatedAt, rowIdentity, type TimelineRow } from "../../src/chat/timeline-row";
import type { AttachmentRefBytes } from "../../src/chat/attachments";
import type { MediaUpload } from "../../src/media/MediaBubbles";
import { createMediaHub, mediaKey, type MediaHub } from "../../src/media/media-hub";
import { RecordButton, RecordingBar, RecorderNotices, recorderBusy } from "../../src/media/MediaComposer";
import { createRecorder, type Recorder } from "../../src/media/recording-controller";
import type { CaptureResult } from "../../src/media/media-capture";
import { setRecordMode } from "../../src/media/media-prefs";

/** What a spec can read that is not in the DOM. Deliberately thin: everything
 *  a person could see is asserted through the rendered tree instead. */
export interface Harness {
  /** Every `Audio` the page constructed, newest last.
   *
   *  OBSERVATION, not substitution: `RecordedAudio` extends the platform
   *  class, so the element production code gets is a genuine one and the
   *  engine's autoplay policy applies to it unchanged. It is here because the
   *  voice bubble's element is created with `new Audio()` and never appended
   *  to the document, so `currentTime` — the only direct proof that samples
   *  are being decoded rather than that a class flipped — is otherwise
   *  unreachable from a spec. */
  readonly audios: HTMLAudioElement[];
  /** The live recorder, for the cases that mount one. */
  recorder?: Recorder;
  /** Takes the composer sent, in order. */
  readonly sent: CaptureResult[];
  /** How many times the page has asked for a microphone or camera.
   *
   *  The real-browser form of the jsdom test's `expect(engine).toEqual([])`
   *  after a tap, and the reason that assertion is worth making: "no take was
   *  kept" is much weaker than "no device was ever opened". A tap that opens
   *  the microphone and throws the recording away still lights the operating
   *  system's recording indicator, for switching a glyph.
   *
   *  A counting wrapper that delegates to the platform call, so the browser's
   *  permission flow, device opening and stream are all the real ones. */
  readonly gumCalls: () => number;
  /**
   * How many of those calls have ANSWERED — the device is open and a stream is
   * in hand.
   *
   * The pair, rather than the count alone, is what lets a spec name the moment
   * between "the microphone and camera are open" and "the camera has produced a
   * frame". Sampling that window by waiting a fixed number of milliseconds does
   * not work: `getUserMedia`'s own latency against the fake device varies with
   * how many workers are contending it (measured here from 23ms to 479ms), so
   * the sample lands inside or outside the window depending on the worker
   * count — the trap `playwright/README.md` records for the microphone, in a
   * second place.
   */
  readonly gumAnswers: () => number;
  /** How many times a `MediaRecorder` has been started.
   *
   *  The real-browser form of the jsdom test's `engine` array: the ordering
   *  property is "the bar never claims to be recording while
   *  `MediaRecorder.start()` has not been called", and that is the number it is
   *  about. Counted by wrapping the prototype method, which then runs — the
   *  recorder, its encoder and its output are the platform's. */
  readonly recorderStarts: () => number;
  /**
   * The canvas a video note's recorder was handed, read AT THE INSTANT
   * `MediaRecorder.start()` was called — or `null` when the recorder that
   * started was not recording a canvas (every voice take).
   *
   * The one thing a spec cannot get to any other way. A video note is recorded
   * from a canvas the page draws the camera onto (`media-capture.ts`), the
   * canvas is never in the document, and the question "had anything been drawn
   * on it yet" stops being answerable one frame later — the draw loop fills it
   * in regardless. So it is sampled where the property is: the moment the
   * encoder was started.
   *
   * OBSERVATION, not substitution: `captureStream` and `MediaRecorder.start`
   * both delegate to the platform, and the pixels are read off the production
   * canvas with `getImageData`, which changes nothing on it.
   */
  readonly canvasAtRecorderStart: () => CanvasFacts | null;
  /** Decode `blob` with a real `AudioContext` and report what is in it. */
  decode(blob: Blob): Promise<{ durationMs: number; peakInFirstMs: number }>;
  /** Decode a video take with a real `<video>` and report what is in it. */
  decodeVideo(blob: Blob): Promise<VideoFacts>;
  /** The outgoing-media case: run the retry a person starts from
   *  `Not sent · Retry`, and then land the send. */
  retryUpload?: () => void;
  completeUpload?: () => void;
}

/** Pixel facts about a canvas. An undrawn canvas is transparent black, so it
 *  reads `mean: 0, max: 0` — and the fake camera's pattern never does. */
export interface CanvasFacts {
  readonly width: number;
  readonly height: number;
  /** Mean luma over every pixel, 0..255. */
  readonly mean: number;
  readonly max: number;
}

/** What a decoded video take says about itself. */
export interface VideoFacts {
  readonly durationMs: number;
  /** The presentation time of the first frame the engine presented, and of the
   *  last one — the window the PICTURE covers inside a file whose length is
   *  `durationMs`. */
  readonly firstFrameMs: number;
  readonly lastFrameMs: number;
  /** The first presented frame's pixels. */
  readonly firstFrame: CanvasFacts;
}

const audios: HTMLAudioElement[] = [];
const sent: CaptureResult[] = [];
let gum = 0;
let gumAnswered = 0;
let starts = 0;
/** Which canvas each capture track came from, so the recorder's input can be
 *  traced back to the surface the page draws on. */
const capturedFrom = new WeakMap<MediaStreamTrack, HTMLCanvasElement>();
let canvasAtStart: CanvasFacts | null = null;

class RecordedAudio extends Audio {
  constructor(src?: string) {
    super(src);
    audios.push(this);
  }
}

/**
 * Peak amplitude over the first `windowMs`, plus the decoded length.
 *
 * The first number is what "the recording did not lose the start of the
 * phrase" reduces to once a browser is involved: the fake microphone plays a
 * loud tone on every sample, so a window that decodes near-silent is a window
 * the recorder was not capturing in.
 */
async function decode(blob: Blob): Promise<{ durationMs: number; peakInFirstMs: number }> {
  const bytes = await blob.arrayBuffer();
  const ctx = new AudioContext();
  try {
    const buf = await ctx.decodeAudioData(bytes);
    const ch = buf.getChannelData(0);
    const upto = Math.min(ch.length, Math.round(buf.sampleRate * 0.15));
    let peak = 0;
    for (let i = 0; i < upto; i += 1) peak = Math.max(peak, Math.abs(ch[i] ?? 0));
    return { durationMs: Math.round(buf.duration * 1000), peakInFirstMs: peak };
  } finally {
    void ctx.close();
  }
}

/** Mean and peak luma over every pixel of `bitmap`, drawn 1:1. */
function lumaOf(bitmap: HTMLCanvasElement | HTMLVideoElement, width: number, height: number): CanvasFacts {
  const probe = document.createElement("canvas");
  probe.width = width;
  probe.height = height;
  const g = probe.getContext("2d", { willReadFrequently: true });
  if (g === null) return { width, height, mean: 0, max: 0 };
  // Onto a canvas of the harness's own, so nothing is read back off the
  // production surface's context and the production canvas is never touched.
  g.drawImage(bitmap, 0, 0, width, height);
  const d = g.getImageData(0, 0, width, height).data;
  let sum = 0;
  let max = 0;
  for (let i = 0; i < d.length; i += 4) {
    const lum = ((d[i] ?? 0) * 299 + (d[i + 1] ?? 0) * 587 + (d[i + 2] ?? 0) * 114) / 1000;
    sum += lum;
    if (lum > max) max = lum;
  }
  return { width, height, mean: sum / (d.length / 4), max };
}

/** One frame presented by `video`, as its presentation time — `kick` is what
 *  makes the engine present one (a play, or a seek). */
function nextPresentedFrame(video: HTMLVideoElement, kick: () => void, timeoutMs = 5000): Promise<number> {
  return new Promise((resolve, reject) => {
    let settled = false;
    const timer = setTimeout(() => {
      if (!settled) {
        settled = true;
        reject(new Error("no frame was presented"));
      }
    }, timeoutMs);
    video.requestVideoFrameCallback((_now, meta) => {
      if (settled) return;
      settled = true;
      clearTimeout(timer);
      resolve(meta.mediaTime);
    });
    kick();
  });
}

/**
 * What is actually in a video take.
 *
 * `requestVideoFrameCallback` rather than "seek and draw", and the difference
 * is not cosmetic: drawing a freshly loaded element reads back pure BLACK
 * whatever the file holds, because no frame has been painted yet. Measured in
 * this session, that artefact reported every take's opening frame as blank,
 * including takes that were demonstrably fine. This asks the engine which
 * frame it presented and when.
 */
async function decodeVideo(blob: Blob): Promise<VideoFacts> {
  const url = URL.createObjectURL(blob);
  const v = document.createElement("video");
  v.muted = true;
  v.playsInline = true;
  v.preload = "auto";
  v.src = url;
  try {
    await new Promise<void>((resolve, reject) => {
      v.addEventListener("loadeddata", () => resolve(), { once: true });
      v.addEventListener("error", () => reject(new Error(`decode failed (${v.error?.code ?? "none"})`)), { once: true });
      setTimeout(() => reject(new Error("decode timed out")), 5000);
    });
    const firstFrameMs = (await nextPresentedFrame(v, () => void v.play().catch(() => undefined))) * 1000;
    const firstFrame = lumaOf(v, v.videoWidth, v.videoHeight);
    v.pause();
    // The last frame the picture reaches. A seek past the final video sample
    // still presents that sample, so its own timestamp is the answer.
    const lastFrameMs =
      (await nextPresentedFrame(v, () => {
        v.currentTime = Math.max(0, v.duration - 0.02);
      })) * 1000;
    return { durationMs: v.duration * 1000, firstFrameMs, lastFrameMs, firstFrame };
  } finally {
    v.removeAttribute("src");
    URL.revokeObjectURL(url);
  }
}

export const harness: Harness = {
  audios,
  sent,
  gumCalls: () => gum,
  gumAnswers: () => gumAnswered,
  recorderStarts: () => starts,
  canvasAtRecorderStart: () => canvasAtStart,
  decode,
  decodeVideo,
};

// ---------------------------------------------------------------------------
// the voice message
// ---------------------------------------------------------------------------

const BLOB_ID = new Uint8Array(32).fill(0x7e);
const VOICE_KEY = mediaKey(BLOB_ID);
/** Fixed, so the rendered clock is the same on every run and in every zone. */
const SENT_AT = "2026-09-24T15:21:00Z";

function voiceRef(): AttachmentRefBytes {
  return {
    blobId: BLOB_ID,
    key: new Uint8Array(32),
    nonce: new Uint8Array(12),
    // The one format ADR-0047 allows for a voice message, and the type the
    // asset really is: the render decision and the blob the element loads
    // agree, as they do for a message a phone sent.
    mime: "audio/mp4",
    size: 58603,
    name: undefined,
    kind: "voice",
    durationMs: 7000,
    waveform: new Uint8Array([8, 120, 200, 90, 40, 170, 60, 230, 150, 70, 210, 30]),
  };
}

function host(hub: MediaHub, over: Partial<BubbleHost> = {}): BubbleHost {
  return {
    onDownload: () => {},
    onPreview: () => {},
    onHidePreview: () => {},
    getCachedUrl: () => null,
    ownIdentity: "own-key",
    opState: () => undefined,
    readByOthers: () => false,
    readersOf: () => [],
    editingId: null,
    editText: "",
    setEditText: () => {},
    onStartEdit: () => {},
    onCancelEdit: () => {},
    onSaveEdit: () => {},
    onDeleteMessage: () => {},
    onReact: () => {},
    actionsAvailable: true,
    hub,
    mediaUpload: undefined,
    onRetrySend: () => {},
    fullReactPickerId: null,
    onOpenFullReactPicker: () => {},
    onCloseFullReactPicker: () => {},
    ...over,
  };
}

function incomingRow(text: string, attachments: AttachmentRefBytes[]): TimelineRow {
  return {
    kind: "incoming",
    event: {
      kind: "application",
      messageId: 42,
      text,
      plaintext: text,
      attachments,
      senderLeaf: 1,
      senderKey: "peer-key",
      createdAt: SENT_AT,
    },
  };
}

/** AAC in MP4, written by `playwright/fixtures/make-assets.mjs`. */
const VOICE_ASSET = "/playwright/fixtures/voice.m4a";

/**
 * The bytes, fetched from the harness server and filed under the blob's own key
 * through the production hub — the state a bubble is in once its
 * scroll-into-view warm-up has run, with no network and no AEAD in the way.
 *
 * A missing asset throws HERE, by name. The generator needs `ffmpeg` for this
 * one, and on a machine without it the fetch returns Vite's 404 page: filing
 * that as `audio/mp4` would give a bubble that never reaches `playing`, and
 * every playback measurement would fail as "the clock never advanced" — a
 * missing tool reported as a product defect.
 */
async function fileVoiceBytes(hub: MediaHub, key: string): Promise<Blob> {
  const res = await fetch(VOICE_ASSET);
  if (!res.ok) {
    throw new Error(
      `voice fixture missing (${res.status} for ${VOICE_ASSET}) — run node playwright/fixtures/make-assets.mjs; it needs ffmpeg`,
    );
  }
  const bytes = await res.arrayBuffer();
  // An MP4's first box is `ftyp` at offset 4, which an HTML error page served
  // with a 200 would not have.
  const tag = new TextDecoder().decode(new Uint8Array(bytes, 4, 4));
  if (tag !== "ftyp") throw new Error(`voice fixture is not an MP4 (box tag ${JSON.stringify(tag)})`);
  const blob = new Blob([bytes], { type: "audio/mp4" });
  hub.fileLocal(key, blob, "audio/mp4");
  return blob;
}

async function voiceCase(caption: string): Promise<JSX.Element> {
  const hub = createMediaHub({ voiceQueue: () => [] });
  await fileVoiceBytes(hub, VOICE_KEY);
  return (
    <ol class="lk-timeline">
      <TimelineBubble
        row={incomingRow(caption, [voiceRef()])}
        isGroup={false}
        startsRun={true}
        endsRun={true}
        {...host(hub)}
      />
    </ol>
  );
}

// ---------------------------------------------------------------------------
// an outgoing voice message, across the moment its send lands
// ---------------------------------------------------------------------------

/** `local:<tempId>` — the key `routes/Chat.tsx` files an outgoing take's own
 *  bytes under until the upload comes back, and the blob id it comes back
 *  with. */
const LOCAL_KEY = "local:1";
const UPLOADED_BLOB_ID = new Uint8Array(32).fill(0x5a);

/** `placeholderRef` in `routes/Chat.tsx`: the take's media metadata with no
 *  blob id yet — which is what makes the bubble read its bytes from the local
 *  key instead of the network. */
function pendingVoiceRef(): AttachmentRefBytes {
  return { ...voiceRef(), blobId: new Uint8Array(0), key: new Uint8Array(0), nonce: new Uint8Array(0) };
}

function uploadedVoiceRef(): AttachmentRefBytes {
  return { ...voiceRef(), blobId: UPLOADED_BLOB_ID };
}

/**
 * The one state in which a person can play an outgoing take, and what happens
 * to it when the send finally lands.
 *
 * A row whose ack is `pending` has an upload fraction, and the voice bubble's
 * play button is DISABLED while it does (`mediaUploadFor` / `uploading()`), so
 * the reachable story is the failed one: the take did not reach the server, it
 * sits in the timeline as `Not sent · Retry`, the person plays it back, and
 * then retries. That retry replaces the row twice — once to mark it pending,
 * once to make it sent — under an element that is making sound.
 *
 * The list wiring below MIRRORS `routes/Chat.tsx` (the merged entries, the
 * reference-keyed `<For>`, the non-keyed `<Show>` that hands the row down).
 * That is the one place a fixture here reproduces a route's own shape, and it
 * is unavoidable: the timeline list is not a component, and the defect IS the
 * reconciliation. Everything the wiring drives — the merge, the bubble, the
 * hub, the element — is production. If the route's list shape changes, this
 * has to change with it or it stops covering anything.
 */
async function outgoingVoiceSendCase(): Promise<JSX.Element> {
  const hub = createMediaHub({ voiceQueue: () => [] });
  // The take's own bytes, filed exactly as the composer files them on release.
  await fileVoiceBytes(hub, LOCAL_KEY);

  const failed: TimelineRow = {
    kind: "outgoing",
    tempId: 1,
    plaintext: "",
    attachments: [pendingVoiceRef()],
    ack: "failed",
    mediaLocalKey: LOCAL_KEY,
    authorKey: "own-key",
  };
  const [rows, setRows] = createSignal<TimelineRow[]>([failed]);
  // What `mediaUploadFor` answers for the row in each of the three states.
  const [upload, setUpload] = createSignal<MediaUpload>("failed");

  harness.retryUpload = () => {
    // `setMediaRowAck(tempId, "pending")` + `setMediaUpload(tempId, 0)`.
    setRows(rows().map((r) => (r.kind === "outgoing" ? { ...r, ack: "pending" as const } : r)));
    setUpload(0);
  };
  harness.completeUpload = () => {
    // `runMediaJob`'s tail, in its order: the take's bytes move to the real
    // blob id — `hub.adopt` is the production call — and the row is replaced by
    // the sent one.
    hub.adopt(LOCAL_KEY, mediaKey(UPLOADED_BLOB_ID));
    setRows([
      {
        kind: "outgoing",
        tempId: 1,
        plaintext: "",
        attachments: [uploadedVoiceRef()],
        ack: "sent",
        messageId: 77,
        createdAt: SENT_AT,
        authorKey: "own-key",
      },
    ]);
    setUpload(undefined);
  };

  const entries = createMemo(() => mergeConversationEntries(rows, [], rowCreatedAt, rowIdentity));
  return (
    <ol class="lk-timeline">
      <For each={entries()}>
        {(entry) => (
          <Show when={entry.kind === "message" ? entry.row : undefined}>
            {(row) => (
              <TimelineBubble
                row={row()}
                isGroup={false}
                startsRun={true}
                endsRun={true}
                {...host(hub)}
                mediaUpload={upload()}
              />
            )}
          </Show>
        )}
      </For>
    </ol>
  );
}

// ---------------------------------------------------------------------------
// the composer
// ---------------------------------------------------------------------------

function composerCase(mode: "voice" | "video_note"): JSX.Element {
  // Remembered per device in a module signal, so a run that switched the mode
  // would otherwise leak into the next page load in the same profile.
  setRecordMode(mode);
  const [inCall] = createSignal(false);
  const urls: string[] = [];
  const recorder = createRecorder({
    onSend: (result) => sent.push(result),
    inCall,
    createUrl: (blob) => {
      const u = URL.createObjectURL(blob);
      urls.push(u);
      return u;
    },
    revokeUrl: (u) => URL.revokeObjectURL(u),
  });
  harness.recorder = recorder;
  return (
    <form class="lk-composer" onSubmit={(e) => e.preventDefault()}>
      {/* The composer's real shape (`routes/Chat.tsx`): the bar replaces the
          field while a take is in progress, and the record button STAYS
          MOUNTED through a held take because it holds the pointer capture the
          gesture runs on. */}
      <Show when={recorderBusy(recorder)} fallback={<input class="lk-composer-input" placeholder="Message" />}>
        <RecordingBar recorder={recorder} />
      </Show>
      <RecordButton recorder={recorder} />
      <RecorderNotices recorder={recorder} />
    </form>
  );
}

// ---------------------------------------------------------------------------
// the composer's message field
// ---------------------------------------------------------------------------

/**
 * The composer's field, alone, in the production cascade.
 *
 * `docs/ux` section 5.2 says the field grows with the draft to five lines and
 * then scrolls. The GROWTH is `growComposer` in `routes/Chat.tsx` — two lines
 * that write `scrollHeight` onto the element's own `style.height` — and the CAP
 * is entirely the stylesheet's: `max-height`, `line-height` and
 * `box-sizing: border-box` on `.lk-composer-input`.
 *
 * This case renders the field the route declares and NOTHING else, so a spec
 * can vary the one input the route varies (the inline height) and measure what
 * the cascade does with it. It deliberately does not reproduce `growComposer`:
 * a fixture that computed the height would be measuring its own arithmetic
 * instead of the cap that ships.
 *
 * Inside a `.lk-composer` because the form is where the field's `flex: 1` and
 * the row's `align-items: flex-end` come from, and a field measured outside it
 * is a field in a different layout from the shipped one.
 */
function composerFieldCase(): JSX.Element {
  return (
    <form class="lk-composer" onSubmit={(e) => e.preventDefault()}>
      <textarea class="lk-composer-input" rows={1} placeholder="Message" />
    </form>
  );
}

// ---------------------------------------------------------------------------

export const cases: Record<string, () => JSX.Element | Promise<JSX.Element>> = {
  /** A voice message alone in its bubble: items 1, 2, 3, 4 and 6. */
  "voice-only": () => voiceCase(""),
  /** The same with a caption, which keeps the bubble's own meta row — the
   *  control for the "same line" measurement. */
  "voice-captioned": () => voiceCase("listen to this"),
  /** The record button and the recording bar: items 5 and 7. */
  composer: () => composerCase("voice"),
  /** The same composer with the mode switched, so a hold records a video note
   *  through the canvas pipeline. */
  "composer-video": () => composerCase("video_note"),
  /** An outgoing voice take that did not send, played back, and then retried. */
  "outgoing-voice-send": () => outgoingVoiceSendCase(),
  /** The composer's message field, for the five-line cap. */
  "composer-field": () => composerFieldCase(),
};

declare global {
  interface Window {
    __lakkoHarness: Harness;
  }
}

window.__lakkoHarness = harness;
window.Audio = RecordedAudio;

/**
 * Counted — and, on request, made to take as long as a real device does.
 *
 * `?gumDelay=<ms>` waits before delegating. This is NOT a stub: the platform
 * call still runs, opens a real (fake-backed) device and returns a real stream,
 * and the browser's own permission flow is untouched. What it changes is the
 * ENVIRONMENT, and it changes it towards reality: Chromium's fake device opens
 * in a few tens of milliseconds, while a real microphone takes a few hundred.
 *
 * That gap is the whole of one defect. "The bar claimed to be recording while
 * the device was still opening" cannot be exhibited by a browser whose device
 * opens instantly — the window in which audio was lost is shorter than the
 * encoder's own frame quantisation — so a measurement taken against the fake
 * device's timing is a measurement of nothing. Measured in this session, the
 * defect restored: with the default timing the deficit was 11-54ms, inside the
 * noise; with the suite running in parallel, so the fake device was contended,
 * it was 330-463ms. A test whose discrimination depends on how many workers
 * happen to be running is not a test, so the delay is named instead.
 */
if (navigator.mediaDevices?.getUserMedia !== undefined) {
  const real = navigator.mediaDevices.getUserMedia.bind(navigator.mediaDevices);
  const delayMs = Number(new URL(window.location.href).searchParams.get("gumDelay") ?? "0");
  navigator.mediaDevices.getUserMedia = async (constraints?: MediaStreamConstraints) => {
    gum += 1;
    if (delayMs > 0) await new Promise((r) => setTimeout(r, delayMs));
    const stream = await real(constraints);
    gumAnswered += 1;
    return stream;
  };
}

/** Remember which canvas a capture track draws from; the platform call runs
 *  and its stream is returned untouched. */
if (typeof HTMLCanvasElement !== "undefined") {
  const realCapture = HTMLCanvasElement.prototype.captureStream;
  HTMLCanvasElement.prototype.captureStream = function captureStream(
    this: HTMLCanvasElement,
    frameRequestRate?: number,
  ): MediaStream {
    const stream =
      frameRequestRate === undefined ? realCapture.call(this) : realCapture.call(this, frameRequestRate);
    for (const track of stream.getVideoTracks()) capturedFrom.set(track, this);
    return stream;
  };
}

if (typeof MediaRecorder !== "undefined") {
  const realStart = MediaRecorder.prototype.start;
  MediaRecorder.prototype.start = function start(this: MediaRecorder, timeslice?: number): void {
    starts += 1;
    // BEFORE the encoder runs, which is the instant the question is about: is
    // there a camera frame on the canvas this recorder is about to record?
    canvasAtStart = null;
    for (const track of this.stream.getVideoTracks()) {
      const canvas = capturedFrom.get(track);
      if (canvas !== undefined) {
        canvasAtStart = lumaOf(canvas, canvas.width, canvas.height);
        break;
      }
    }
    if (timeslice === undefined) realStart.call(this);
    else realStart.call(this, timeslice);
  };
}

/**
 * A camera that takes as long to produce its first frame as a real one does.
 *
 * `?camWarmup=<ms>` makes a `<video>` fed by a `MediaStream` report no frame
 * size for that long after it is given one. Same argument as `?gumDelay=`, one
 * step further in: `getUserMedia` resolving means the DEVICE is open, not that
 * a frame has arrived, and `videoWidth === 0` is the platform's own way of
 * saying so — which is exactly what the video-note draw loop tests. Chromium's
 * fake camera crosses that line in a few tens of milliseconds; a real camera
 * takes hundreds, and the gap is where one of the defects lives.
 *
 * It changes the ENVIRONMENT and not the product: frames really are flowing,
 * the element really is playing them, and only the moment the page can first
 * read a frame's size moves.
 */
const camWarmupMs = Number(new URL(window.location.href).searchParams.get("camWarmup") ?? "0");
if (camWarmupMs > 0 && typeof HTMLVideoElement !== "undefined") {
  const fedAt = new WeakMap<HTMLMediaElement, number>();
  const srcObject = Object.getOwnPropertyDescriptor(HTMLMediaElement.prototype, "srcObject");
  const set = srcObject?.set;
  if (srcObject !== undefined && set !== undefined) {
    Object.defineProperty(HTMLMediaElement.prototype, "srcObject", {
      ...srcObject,
      set(this: HTMLMediaElement, value: MediaStream | null) {
        if (value !== null) fedAt.set(this, performance.now());
        set.call(this, value);
      },
    });
  }
  const warming = (el: HTMLVideoElement): boolean => {
    const at = fedAt.get(el);
    return at !== undefined && performance.now() - at < camWarmupMs;
  };
  for (const name of ["videoWidth", "videoHeight"] as const) {
    const real = Object.getOwnPropertyDescriptor(HTMLVideoElement.prototype, name);
    const get = real?.get;
    if (real === undefined || get === undefined) continue;
    Object.defineProperty(HTMLVideoElement.prototype, name, {
      ...real,
      get(this: HTMLVideoElement) {
        return warming(this) ? 0 : (get.call(this) as number);
      },
    });
  }
}
