// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * A media bubble's cleanup runs while its parent is being torn down, where the
 * parent's getters can throw. One such throw stopped the route change that was
 * disposing the bubble: after a failed send, chats and sections would not
 * switch until the app restarted. Reached the cleanup is, read the key it is
 * not.
 *
 * A photo or video of a message is drawn in its mosaic and played in the
 * viewer (`docs/ux/media-messages.md` section 11); the viewer hides what it
 * shows when it is disposed, without reading its parent either.
 */

// @vitest-environment jsdom

import { render } from "solid-js/web";
import { beforeAll, describe, expect, it, vi } from "vitest";

import { VideoNoteBubble } from "./MediaBubbles";
import { createMediaHub } from "./media-hub";
import { MediaGallery } from "./MediaGallery";
import type { AttachmentRefBytes } from "../chat/attachments";
import { initWasm } from "../test/ui";

beforeAll(async () => {
  await initWasm();
  HTMLMediaElement.prototype.pause = (): void => {};
});

function ref(kind: "video_note" | "other", mime: string): AttachmentRefBytes {
  return {
    blobId: new Uint8Array(0),
    key: new Uint8Array(0),
    nonce: new Uint8Array(0),
    mime,
    size: 2048,
    name: undefined,
    kind,
    durationMs: 3000,
  };
}

const bubbles = [
  ["a video note", VideoNoteBubble, ref("video_note", "video/mp4")],
] as const;

describe("disposing a media bubble", () => {
  it.each(bubbles)("%s releases the last key without reading its parent", (_, Bubble, attachment) => {
    const hub = createMediaHub({ voiceQueue: () => [] });
    const release = vi.spyOn(hub, "release");
    let tornDown = false;
    const host = document.createElement("div");
    const props = {
      attachment,
      hub,
      get mediaKey(): string {
        if (tornDown) throw new Error("stale read");
        return "local:1";
      },
      incoming: false,
      upload: undefined,
      onRetry: (): void => {},
    };
    const dispose = render(() => Bubble(props), host);
    tornDown = true;
    expect(dispose).not.toThrow();
    expect(release).toHaveBeenCalledWith("local:1");
  });
});

describe("disposing the media viewer", () => {
  it("hides the item it shows without reading its parent", () => {
    const onHide = vi.fn();
    let tornDown = false;
    const item = {
      kind: "video" as const,
      url: (): string | null => null,
      saveName: "video.mp4",
      onHide,
    };
    const host = document.createElement("div");
    const props = {
      get items() {
        if (tornDown) throw new Error("stale read");
        return [item];
      },
      start: 0,
      onClose: (): void => {},
    };
    const dispose = render(() => MediaGallery(props), host);
    tornDown = true;
    expect(dispose).not.toThrow();
    expect(onHide).toHaveBeenCalledTimes(1);
  });
});
