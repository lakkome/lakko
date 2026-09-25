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

import { createSignal, Show, type JSX } from "solid-js";

import { TimelineBubble, type BubbleHost } from "../../src/chat/TimelineBubble";
import type { TimelineRow } from "../../src/chat/timeline-row";
import type { AttachmentRefBytes } from "../../src/chat/attachments";
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
  /** How many times a `MediaRecorder` has been started.
   *
   *  The real-browser form of the jsdom test's `engine` array: the ordering
   *  property is "the bar never claims to be recording while
   *  `MediaRecorder.start()` has not been called", and that is the number it is
   *  about. Counted by wrapping the prototype method, which then runs — the
   *  recorder, its encoder and its output are the platform's. */
  readonly recorderStarts: () => number;
  /** Decode `blob` with a real `AudioContext` and report what is in it. */
  decode(blob: Blob): Promise<{ durationMs: number; peakInFirstMs: number }>;
}

const audios: HTMLAudioElement[] = [];
const sent: CaptureResult[] = [];
let gum = 0;
let starts = 0;

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

export const harness: Harness = {
  audios,
  sent,
  gumCalls: () => gum,
  recorderStarts: () => starts,
  decode,
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
async function fileVoiceBytes(hub: MediaHub): Promise<void> {
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
  hub.fileLocal(VOICE_KEY, new Blob([bytes], { type: "audio/mp4" }), "audio/mp4");
}

async function voiceCase(caption: string): Promise<JSX.Element> {
  const hub = createMediaHub({ voiceQueue: () => [] });
  await fileVoiceBytes(hub);
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
// the composer
// ---------------------------------------------------------------------------

function composerCase(): JSX.Element {
  // Remembered per device in a module signal, so a run that switched the mode
  // would otherwise leak into the next page load in the same profile.
  setRecordMode("voice");
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

export const cases: Record<string, () => JSX.Element | Promise<JSX.Element>> = {
  /** A voice message alone in its bubble: items 1, 2, 3, 4 and 6. */
  "voice-only": () => voiceCase(""),
  /** The same with a caption, which keeps the bubble's own meta row — the
   *  control for the "same line" measurement. */
  "voice-captioned": () => voiceCase("listen to this"),
  /** The record button and the recording bar: items 5 and 7. */
  composer: () => composerCase(),
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
    return real(constraints);
  };
}

if (typeof MediaRecorder !== "undefined") {
  const realStart = MediaRecorder.prototype.start;
  MediaRecorder.prototype.start = function start(this: MediaRecorder, timeslice?: number): void {
    starts += 1;
    if (timeslice === undefined) realStart.call(this);
    else realStart.call(this, timeslice);
  };
}
