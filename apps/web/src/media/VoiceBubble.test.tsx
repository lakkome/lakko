// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * The voice bubble, rendered. Two of the defects a person met on it.
 *
 * ## Defect 2 — pressing play did nothing, in silence
 *
 * `play()` was `async` and awaited the fetch and AEAD open before reaching
 * `audio.play()`, so the call landed after the click handler's task had ended,
 * outside the user activation, and the engine refused it with
 * `NotAllowedError` — which `playback-outcome.ts` deliberately classifies as
 * "go back to the triangle and say nothing" for a voice message, because sound
 * is the whole message. It was the one path through the bubble that reported
 * nothing at all.
 *
 * **What is and is not provable here.** jsdom implements no media engine and no
 * activation policy: it will never refuse a `play()`, so the *rejection* cannot
 * be reproduced. What can — and what the activation rule turns the defect into
 * — is an ORDERING question: did production code reach `play()` on the
 * handler's own stack, or after an await? That is decidable, and it is decided
 * below by asserting the call count immediately after `dispatchEvent` returns,
 * with nothing awaited in between. The stub records; it decides nothing.
 * `voice-press.ts` pins WHICH branch a press takes; only this can see whether
 * the branch was acted on in time.
 *
 * ## Defect 3 — the bubble grew by nearly 30px the moment playback started
 *
 * The speed control is a `<button>`, and the global `button` rule floors every
 * button at `--control-h` (40px) in a 12px line. jsdom computes no layout, so
 * the 28px is not observable here and this file does not pretend otherwise.
 * What IS observable is the cascade that caused it — which declaration wins for
 * that element, read out of the real `src/styles.css`. A control button is
 * asserted alongside it, so "jsdom parsed no rules" cannot pass as "the right
 * rule won".
 */

// @vitest-environment jsdom

import { beforeAll, beforeEach, describe, expect, it } from "vitest";

import { VoiceBubble } from "./MediaBubbles";
import { createMediaHub, mediaKey, type MediaHub } from "./media-hub";
import type { AttachmentRefBytes } from "../chat/attachments";
import { all, initWasm, installStylesheet, mouse, one, renderUi, settle } from "../test/ui";

/** Every call the component made into the media engine jsdom does not have,
 *  in order. The engine's behaviour is stubbed; the assertions are all about
 *  what our own code did to it and when. */
let calls: string[] = [];

beforeAll(async () => {
  await initWasm();
  HTMLMediaElement.prototype.load = function load(): void {
    calls.push("load");
  };
  HTMLMediaElement.prototype.play = function play(): Promise<void> {
    calls.push("play");
    // A real engine fires `play` on a successful start, and the component's
    // `playing` signal — which is what turns the triangle into a pause glyph —
    // is driven by that event rather than by the call returning.
    this.dispatchEvent(new Event("play"));
    return Promise.resolve();
  };
  HTMLMediaElement.prototype.pause = function pause(): void {
    calls.push("pause");
    this.dispatchEvent(new Event("pause"));
  };
});

const BLOB_ID = new Uint8Array(32).fill(0x7e);
const KEY = mediaKey(BLOB_ID);

function ref(): AttachmentRefBytes {
  return {
    blobId: BLOB_ID,
    key: new Uint8Array(32),
    nonce: new Uint8Array(12),
    mime: "audio/mp4",
    size: 2048,
    name: undefined,
    kind: "voice",
    durationMs: 7000,
    waveform: new Uint8Array([8, 120, 200, 90, 40, 170, 60, 230]),
  };
}

let hub: MediaHub;

beforeEach(() => {
  calls = [];
  hub = createMediaHub({ voiceQueue: () => [] });
});

/** The bytes already decrypted and filed, which is the state the
 *  scroll-into-view warm-up leaves a bubble in by the time a person can press
 *  it (`near-viewport.ts`; jsdom has no `IntersectionObserver`, so the warm-up
 *  runs on mount exactly as it does for a bubble already on screen). */
function fileBytes(): void {
  hub.fileLocal(KEY, new Blob([new Uint8Array(32)], { type: "audio/mp4" }), "audio/mp4");
}

function mount(): HTMLElement {
  renderUi(() => (
    <VoiceBubble
      attachment={ref()}
      hub={hub}
      mediaKey={KEY}
      incoming={true}
      upload={undefined}
      onRetry={() => {}}
    />
  ));
  return one("button.lakko-voice-play");
}

// ---------------------------------------------------------------------------
// Defect 2 — the press reaches the engine on its own stack.
// ---------------------------------------------------------------------------

describe("pressing play", () => {
  it("calls play() synchronously, inside the gesture, once the bytes are here", async () => {
    fileBytes();
    const play = mount();
    // Let the mount-time warm-up attach the source, as it has by the time a
    // person can aim at the button.
    await settle();
    calls = [];

    mouse(play, "click");

    // NOTHING is awaited between the dispatch above and this line. If the
    // component awaits anything before `play()` — as it did — the engine is
    // reached in a later task, which is precisely what loses the activation.
    expect(calls).toEqual(["play"]);
  });

  it("unlocks the element inside the gesture when the bytes are not here yet", async () => {
    // The cold path: no bytes filed, so the press cannot reach `play()` on its
    // own stack. It must still TOUCH the element synchronously — bringing it
    // into existence and loading it under the gesture is what tells the engine
    // the person asked for this element's sound.
    let release: ((v: Uint8Array) => void) | undefined;
    const original = hub.load;
    hub.load = () =>
      new Promise<string>((resolve) => {
        release = () => resolve(hub.fileLocal(KEY, new Blob([]), "audio/mp4") ?? "");
      });
    const play = mount();
    await settle();
    calls = [];

    mouse(play, "click");

    expect(calls).toEqual(["load"]);
    release?.(new Uint8Array());
    await settle();
    await settle();
    expect(calls).toEqual(["load", "play"]);
    hub.load = original;
  });

  it("a second press pauses rather than starting again", async () => {
    fileBytes();
    const play = mount();
    await settle();
    calls = [];

    mouse(play, "click");
    mouse(play, "click");

    expect(calls).toEqual(["play", "pause"]);
  });

  it("an upload still in flight is not playable", async () => {
    fileBytes();
    renderUi(() => (
      <VoiceBubble
        attachment={ref()}
        hub={hub}
        mediaKey={KEY}
        incoming={false}
        upload={0.4}
        onRetry={() => {}}
      />
    ));
    await settle();
    calls = [];
    const play = one("button.lakko-voice-play");
    expect((play as HTMLButtonElement).disabled).toBe(true);
    mouse(play, "click");
    expect(calls).toEqual([]);
  });
});

// ---------------------------------------------------------------------------
// Defect 3 — the global button floor does not reach into the quiet line.
// ---------------------------------------------------------------------------

describe("the quiet line's own geometry rules", () => {
  beforeEach(() => {
    installStylesheet();
  });

  it("the speed control is not floored at the global control height", async () => {
    fileBytes();
    mount();
    await settle();

    const rate = one("button.lakko-voice-rate");
    const control = document.createElement("button");
    document.body.appendChild(control);

    // The control button is the proof that jsdom parsed the sheet at all: if it
    // dropped the rules, both of these read `auto` and this assertion fails
    // rather than passing vacuously.
    expect(getComputedStyle(control).minHeight).toBe("var(--control-h)");
    // 40px in a 12px line. The chip has to say so itself, because the global
    // `button` rule reaches every button inside a bubble.
    expect(getComputedStyle(rate).minHeight).toBe("0px");
  });

  it("the play disc opts out of the same floor", async () => {
    fileBytes();
    const play = mount();
    await settle();
    // Same rule, one control over: `min-height` beats `height`, so the 38px
    // disc came out a 38x40 ellipse.
    expect(getComputedStyle(play).minHeight).toBe("0px");
  });

  it("the speed control holds its space while nothing is playing", async () => {
    fileBytes();
    mount();
    await settle();

    const rate = one("button.lakko-voice-rate");
    // Reserved, not absent: the row's geometry must not depend on WHICH of its
    // children are present, and `visibility: hidden` is what makes "absent to
    // the eye" and "absent to a screen reader and the tab order" the same
    // thing while holding exactly the space the control will take.
    expect(rate.getAttribute("data-reserved")).toBe("true");
    expect(rate.getAttribute("tabindex")).toBe("-1");
    expect(getComputedStyle(rate).visibility).toBe("hidden");
  });

  it("and is shown once this message is the one playing", async () => {
    fileBytes();
    const play = mount();
    await settle();

    mouse(play, "click");

    const rate = one("button.lakko-voice-rate");
    expect(rate.getAttribute("data-reserved")).toBeNull();
    expect(getComputedStyle(rate).visibility).toBe("visible");
    // Still not floored, which is the whole point: this is the moment the
    // bubble used to jump, with a person's eye on it.
    expect(getComputedStyle(rate).minHeight).toBe("0px");
  });

  it("every child of the quiet line is a child of the quiet line", async () => {
    fileBytes();
    mount();
    await settle();
    const line = one(".lakko-voice-line");
    // The arrangement the spec fixes as the rule: one row, the waveform above
    // one quiet line, and the speed control at its trailing end — not stacked
    // in a third column, which is what web had.
    expect(all("button.lakko-voice-rate", line).length).toBe(1);
    expect(all(".lakko-voice-time", line).length).toBe(1);
    expect(one(".lakko-voice-row").contains(line)).toBe(true);
  });
});
