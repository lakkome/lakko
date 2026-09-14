// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import { beforeAll, describe, expect, it } from "vitest";
import { readFileSync } from "node:fs";
import { fileURLToPath } from "node:url";

import init, {
  decodeHistoryPlaceholder,
  encodeHistoryPlaceholder,
  HistoryAttachmentInput,
  type OpenedHistoryAttachment,
} from "../../wasm/lakko_ffi_wasm";
import { b64UrlEncode } from "../chat/persisted-attachment";
import type { PersistedAttachment } from "../chat/message-log-store";
import { historyMediaFromPersisted, persistedMediaFromOpened } from "./history-media";

beforeAll(async () => {
  const wasmPath = fileURLToPath(new URL("../../wasm/lakko_ffi_wasm_bg.wasm", import.meta.url));
  await init({ module_or_path: readFileSync(wasmPath) });
});

const bytes = (n: number, fill: number) => new Uint8Array(n).fill(fill);

function persisted(extra: Partial<PersistedAttachment>): PersistedAttachment {
  return {
    blobIdB64: "unused-by-mapping",
    keyB64: "unused-by-mapping",
    nonceB64: "unused-by-mapping",
    mime: "audio/mp4",
    sizeStr: "1234",
    name: "voice.m4a",
    ...extra,
  };
}

/** Encode one attachment through the real core and decode it back. */
function throughCore(a: PersistedAttachment): OpenedHistoryAttachment {
  const input = new HistoryAttachmentInput(bytes(32, 1), bytes(32, 2), bytes(12, 3), 1234n, a.mime ?? undefined, a.name ?? undefined);
  const { media, thumb } = historyMediaFromPersisted(a);
  if (media !== null) {
    input.setMedia(media.kind, media.durationMs, media.waveform, media.width, media.height);
  }
  if (thumb !== null) {
    input.setThumb(thumb.blobId, thumb.key, thumb.nonce, thumb.size, thumb.mime, thumb.width, thumb.height);
  }
  const wire = encodeHistoryPlaceholder(9n, 1, "2026-09-14T12:00:00Z", "", [input]);
  const opened = decodeHistoryPlaceholder(wire).attachments as unknown as OpenedHistoryAttachment[];
  expect(opened).toHaveLength(1);
  return opened[0] as OpenedHistoryAttachment;
}

describe("history bundle media metadata (ADR-0047 §5)", () => {
  it("restores a voice message as a voice message", () => {
    const waveformB64 = b64UrlEncode(new Uint8Array([3, 9, 27, 81]));
    const back = persistedMediaFromOpened(
      throughCore(persisted({ kind: "voice", durationMs: 7250, waveformB64 })),
    );
    expect(back).toEqual({ kind: "voice", durationMs: 7250, waveformB64 });
  });

  it("restores a video note with its poster frame", () => {
    const thumb = {
      blobIdB64: b64UrlEncode(bytes(32, 7)),
      keyB64: b64UrlEncode(bytes(32, 8)),
      nonceB64: b64UrlEncode(bytes(12, 9)),
      mime: "image/jpeg",
      sizeStr: "20000",
      width: 240,
      height: 240,
    };
    const back = persistedMediaFromOpened(
      throughCore(
        persisted({ mime: "video/mp4", kind: "video_note", durationMs: 42000, width: 480, height: 480, thumb }),
      ),
    );
    expect(back).toEqual({ kind: "video_note", durationMs: 42000, width: 480, height: 480, thumb });
  });

  it("drops values the envelope would drop", () => {
    const back = persistedMediaFromOpened(
      throughCore(persisted({ mime: "image/gif", kind: "gif", durationMs: 1000, width: 0, height: 5000 })),
    );
    expect(back).toEqual({ kind: "gif" });
  });

  it("restores a plain file with no media fields at all", () => {
    const back = persistedMediaFromOpened(throughCore(persisted({ mime: "application/pdf", name: "a.pdf" })));
    expect(back).toEqual({});
  });

  it("does not transfer the local played marker", () => {
    const { media } = historyMediaFromPersisted(persisted({ kind: "voice", played: true }));
    expect(media).not.toHaveProperty("played");
  });
});
