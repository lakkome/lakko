// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

// @vitest-environment jsdom

/**
 * Base walk 2026-11-04 — the "GIF" label was drawn over the picture's own
 * bottom-left corner and covered the GIF's caption. It sits on a pill of its
 * own below the picture now: outside the image's box, not positioned over it.
 */
import { beforeAll, describe, expect, it } from "vitest";

import { GifBubble } from "./MediaBubbles";
import { createMediaHub } from "./media-hub";
import type { AttachmentRefBytes } from "../chat/attachments";
import { initWasm, installStylesheet, one, renderUi } from "../test/ui";

beforeAll(async () => {
  await initWasm();
});

const gif: AttachmentRefBytes = {
  blobId: new Uint8Array(32).fill(0x47),
  key: new Uint8Array(32),
  nonce: new Uint8Array(12),
  mime: "image/gif",
  size: 2048,
  name: undefined,
  kind: "gif",
  width: 240,
  height: 180,
};

describe("the GIF label", () => {
  it("is outside the picture, on its own pill, never laid over it", () => {
    installStylesheet();
    const hub = createMediaHub({ voiceQueue: () => [] });
    // The bytes are already here, so nothing reaches the network.
    hub.fileLocal("local:gif", new Blob([new Uint8Array(16)], { type: "image/gif" }), "image/gif");
    renderUi(() => (
      <GifBubble attachment={gif} hub={hub} mediaKey="local:gif" incoming={true} upload={undefined} onRetry={() => {}} />
    ));
    const picture = one(".lakko-gif");
    const label = one(".lakko-gif-label");
    expect(label.textContent).toBe("GIF");
    expect(picture.contains(label)).toBe(false);
    expect(label.previousElementSibling).toBe(picture);
    expect(label.getAttribute("aria-hidden")).toBe("true");
    const style = getComputedStyle(label);
    expect(style.position).not.toBe("absolute");
    expect(style.borderRadius).toBe("var(--radius-pill)");
  });
});
