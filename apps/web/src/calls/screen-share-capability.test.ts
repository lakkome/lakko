// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/** Who may send a share, what is asked for, and how a failed start reads
 *  (`docs/ux/screen-share.md` sections 1 and 2). */
import { describe, expect, it } from "vitest";

import {
  canSendScreenShare,
  classifyShareStartError,
  displayMediaOptions,
  isMacOs,
  SHARE_REFUSED_WITHIN_MS,
} from "./screen-share-capability";

const named = (name: string, message = ""): { name: string; message: string } => ({ name, message });

describe("canSendScreenShare", () => {
  it("needs screen capture and a fine pointer", () => {
    expect(canSendScreenShare({ hasGetDisplayMedia: true, finePointer: true })).toBe(true);
    // A phone or tablet browser.
    expect(canSendScreenShare({ hasGetDisplayMedia: true, finePointer: false })).toBe(false);
    // A browser with no screen capture.
    expect(canSendScreenShare({ hasGetDisplayMedia: false, finePointer: true })).toBe(false);
  });
});

describe("displayMediaOptions", () => {
  it("always asks for the picture and for sound", () => {
    const options = displayMediaOptions({});
    expect(options.video).toBe(true);
    expect(options.audio).not.toBe(false);
  });

  it("leaves the page's own sound out where the browser can", () => {
    const options = displayMediaOptions({ restrictOwnAudio: true });
    expect(options.audio).toEqual({ restrictOwnAudio: true });
    expect(options["systemAudio"]).toBe("include");
  });

  it("asks for a tab's sound only where it cannot", () => {
    const options = displayMediaOptions({ suppressLocalAudioPlayback: true });
    expect(options.audio).toBe(true);
    expect(options["systemAudio"]).toBe("exclude");
  });

  it("keeps the call's own tab out of the picker", () => {
    expect(displayMediaOptions({})["selfBrowserSurface"]).toBe("exclude");
  });
});

describe("classifyShareStartError", () => {
  it("reads a closed picker as a cancel", () => {
    expect(classifyShareStartError(named("NotAllowedError", "Permission denied"), 4_000)).toBe("cancelled");
    expect(classifyShareStartError(named("AbortError"), 10)).toBe("cancelled");
  });

  it("reads a refusal by the system as a refusal", () => {
    expect(classifyShareStartError(named("NotAllowedError", "Permission denied by system"), 4_000)).toBe(
      "refused",
    );
  });

  it("reads a rejection too quick for a person as a refusal", () => {
    expect(classifyShareStartError(named("NotAllowedError"), SHARE_REFUSED_WITHIN_MS - 1)).toBe("refused");
    expect(classifyShareStartError(named("NotAllowedError"), SHARE_REFUSED_WITHIN_MS)).toBe("cancelled");
  });

  it("reads anything else as a refusal", () => {
    expect(classifyShareStartError(named("NotReadableError"), 4_000)).toBe("refused");
    expect(classifyShareStartError(new Error("boom"), 4_000)).toBe("refused");
    expect(classifyShareStartError("weird", 4_000)).toBe("refused");
  });
});

describe("isMacOs", () => {
  it("reads the platform, then the user agent", () => {
    expect(isMacOs("", "macOS")).toBe(true);
    expect(isMacOs("Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7)", undefined)).toBe(true);
    expect(isMacOs("Mozilla/5.0 (Windows NT 10.0; Win64; x64)", "Windows")).toBe(false);
    expect(isMacOs("Mozilla/5.0 (iPad; CPU OS 17_0 like Mac OS X)", undefined)).toBe(false);
  });
});
