// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * Starting and stopping a share (`docs/ux/screen-share.md` sections 2 and 3),
 * with a fake browser capture and a fake fold: a closed picker says nothing,
 * a refusal says how to allow it (naming System Settings on macOS), a share
 * that could not be sealed is never sent, and the browser's own Stop ends the
 * share like Lakko's.
 */
import { describe, expect, it } from "vitest";

import { t } from "../l10n";
import { ScreenShareBinding, type ScreenCapture } from "./screen-share-binding";
import type { FoldCurrent, ShareFold } from "./screen-share-controller";
import type { OwnSharePicture, ScreenShareControl, ShareOnScreen } from "./screen-share-store";
import type { MirrorFacts } from "./screen-share-mirror";
import { ScreenShareNotSealedError } from "./screen-share-tracks";

class FakeTrack {
  contentHint = "";
  stopped = false;
  private readonly listeners: (() => void)[] = [];
  constructor(readonly kind: "audio" | "video") {}
  stop(): void {
    this.stopped = true;
  }
  addEventListener(_type: "ended", fn: () => void): void {
    this.listeners.push(fn);
  }
  end(): void {
    for (const fn of this.listeners) fn();
  }
}

function fakeStream(withSound: boolean): { stream: MediaStream; video: FakeTrack; audio: FakeTrack | null } {
  const video = new FakeTrack("video");
  const audio = withSound ? new FakeTrack("audio") : null;
  const tracks = audio === null ? [video] : [video, audio];
  const stream = {
    id: "capture",
    getVideoTracks: () => [video],
    getAudioTracks: () => (audio === null ? [] : [audio]),
    getTracks: () => tracks,
  } as unknown as MediaStream;
  return { stream, video, audio };
}

/** A fold that keeps only this device's own share. */
class OwnFold implements ShareFold<never> {
  own: { seq: bigint; peerId: string; withSound: boolean } | null = null;
  nextStartSeq(): bigint {
    return 7n;
  }
  observe(): boolean {
    return false;
  }
  startOwn(seq: bigint, peerId: string, withSound: boolean): boolean {
    this.own = { seq, peerId, withSound };
    return true;
  }
  stopOwn(): bigint | undefined {
    const seq = this.own?.seq;
    this.own = null;
    return seq;
  }
  ownSuperseded(): bigint | undefined {
    return undefined;
  }
  participantLeft(): boolean {
    return false;
  }
  current(): FoldCurrent | undefined {
    const own = this.own;
    if (own === null) return undefined;
    return {
      seq: own.seq,
      signerKey: new Uint8Array(32).fill(1),
      peerId: own.peerId,
      withSound: own.withSound,
      accountKey: new Uint8Array(0),
      isOwn: true,
    };
  }
}

interface Harness {
  binding: ScreenShareBinding<never>;
  sent: { seq: bigint; sharing: boolean; withSound: boolean }[];
  notices: string[];
  attached: MediaStream[];
  detached: number;
  store: {
    current: ShareOnScreen | null;
    control: ScreenShareControl | null;
    busy: boolean;
    own: OwnSharePicture | null;
  };
}

function harness(capture: Partial<ScreenCapture>, attach?: (s: MediaStream) => Promise<void>): Harness {
  const h: Harness = {
    binding: undefined as unknown as ScreenShareBinding<never>,
    sent: [],
    notices: [],
    attached: [],
    detached: 0,
    store: { current: null, control: null, busy: false, own: null },
  };
  const now = { ms: 0 };
  h.binding = new ScreenShareBinding<never>({
    fold: new OwnFold(),
    compose: (_peerId, seq, sharing, withSound) => {
      h.sent.push({ seq, sharing, withSound });
      return new Uint8Array([1]);
    },
    send: () => {},
    media: {
      attach: async (s) => {
        h.attached.push(s);
        if (attach !== undefined) await attach(s);
      },
      detach: () => {
        h.detached += 1;
      },
    },
    ownPeerId: () => "me",
    nameOf: () => "Anna",
    store: {
      setCurrent: (c) => {
        h.store.current = c;
      },
      setControl: (c) => {
        h.store.control = c;
      },
      setBusy: (b) => {
        h.store.busy = b;
      },
      setOwnPicture: (o) => {
        h.store.own = o;
      },
    },
    notice: (text) => h.notices.push(text),
    capture: {
      available: () => true,
      getDisplayMedia: () => Promise.reject(new Error("unset")),
      supportedConstraints: () => ({}),
      macOs: () => false,
      now: () => {
        now.ms += 2_000;
        return now.ms;
      },
      ...capture,
    },
  });
  return h;
}

const refusal = (message = "Permission denied"): Error =>
  Object.assign(new Error(message), { name: "NotAllowedError" });

describe("ScreenShareBinding", () => {
  it("registers the button only where the browser can send", () => {
    const yes = harness({});
    yes.binding.enable();
    expect(yes.store.control).not.toBeNull();
    const no = harness({ available: () => false });
    no.binding.enable();
    expect(no.store.control).toBeNull();
  });

  it("says nothing when the person closes the picker", async () => {
    const h = harness({ getDisplayMedia: () => Promise.reject(refusal()) });
    await h.binding.start();
    expect(h.notices).toEqual([]);
    expect(h.sent).toEqual([]);
    expect(h.store.busy).toBe(false);
  });

  it("says how to allow it when the browser refuses", async () => {
    const h = harness({ getDisplayMedia: () => Promise.reject(refusal("Permission denied by system")) });
    await h.binding.start();
    expect(h.notices).toEqual([t("call.screen_share.refused")]);
    const mac = harness({
      getDisplayMedia: () => Promise.reject(refusal("Permission denied by system")),
      macOs: () => true,
    });
    await mac.binding.start();
    expect(mac.notices).toEqual([t("call.screen_share.refused_macos")]);
  });

  it("sends nothing of a share that could not be sealed", async () => {
    const capture = fakeStream(true);
    const h = harness({ getDisplayMedia: () => Promise.resolve(capture.stream) }, () =>
      Promise.reject(new ScreenShareNotSealedError()),
    );
    await h.binding.start();
    expect(h.sent).toEqual([]);
    expect(h.notices).toEqual([t("call.screen_share.not_sealed")]);
    expect(capture.video.stopped).toBe(true);
    expect(capture.audio?.stopped).toBe(true);
    expect(h.binding.sharing()).toBe(false);
    expect(h.store.current).toBeNull();
  });

  it("shares, with the picture hinted as detail and the sound said", async () => {
    const capture = fakeStream(true);
    const h = harness({ getDisplayMedia: () => Promise.resolve(capture.stream) });
    await h.binding.start();
    expect(capture.video.contentHint).toBe("detail");
    expect(h.attached).toEqual([capture.stream]);
    expect(h.sent).toEqual([{ seq: 7n, sharing: true, withSound: true }]);
    expect(h.store.current).toMatchObject({ isOwn: true, withSound: true, peerId: "me" });
    expect(h.binding.sharing()).toBe(true);
  });

  it("treats the browser's own Stop like Lakko's", async () => {
    const capture = fakeStream(false);
    const h = harness({ getDisplayMedia: () => Promise.resolve(capture.stream) });
    await h.binding.start();
    capture.video.end();
    expect(h.sent.at(-1)).toEqual({ seq: 7n, sharing: false, withSound: false });
    expect(h.detached).toBeGreaterThan(0);
    expect(h.store.current).toBeNull();
    expect(h.binding.sharing()).toBe(false);
  });

  it("shows the own picture, and no picture for a share of this very screen", async () => {
    const facts: MirrorFacts = {
      displaySurface: "window",
      captured: { width: 1920, height: 1080 },
      captureHandleIsOwn: undefined,
          ownWindow: { width: 1440, height: 900 },
      ownScreen: { width: 1512, height: 982 },
      devicePixelRatio: 2,
      screenExtended: undefined,
    };
    let current = facts;
    let resized: (() => void) | null = null;
    const capture = fakeStream(false);
    const h = harness({
      getDisplayMedia: () => Promise.resolve(capture.stream),
      mirrorFacts: () => Promise.resolve(current),
      onWindowChange: (l) => {
        resized = l;
        return () => {
          resized = null;
        };
      },
    });
    await h.binding.start();
    await Promise.resolve();
    expect(h.store.own?.stream).toBe(capture.stream);
    expect(h.store.own?.view).toEqual({ kind: "picture" });

    // The person switches the share to the screen the call is on.
    current = { ...facts, displaySurface: "monitor", captured: { width: 3024, height: 1964 } };
    (resized as (() => void) | null)?.();
    await Promise.resolve();
    expect(h.store.own?.view).toEqual({ kind: "mirror", surface: "screen" });

    h.binding.stop();
    expect(h.store.own).toBeNull();
    expect(resized).toBeNull();
  });

  it("stops the capture and says so when the call ends", async () => {
    const capture = fakeStream(false);
    const h = harness({ getDisplayMedia: () => Promise.resolve(capture.stream) });
    h.binding.enable();
    await h.binding.start();
    h.binding.dispose();
    expect(capture.video.stopped).toBe(true);
    expect(h.sent.at(-1)?.sharing).toBe(false);
    expect(h.store.control).toBeNull();
    expect(h.store.current).toBeNull();
  });
});
