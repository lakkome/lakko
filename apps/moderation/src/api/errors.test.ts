// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import { describe, expect, it } from "vitest";

import { ApiError, blobErrorMessage, errorMessage } from "./errors";

describe("errorMessage", () => {
  it("words a statement refused for its age or a clock ahead, never as a bare code", () => {
    const stale = errorMessage(new ApiError(400, "stale-statement", null));
    expect(stale).toContain("more than 24 hours ago");
    expect(stale).not.toContain("stale-statement");
    const future = errorMessage(new ApiError(400, "future-statement", null));
    expect(future).toContain("clock");
    expect(future).not.toContain("future-statement");
  });
});

describe("blobErrorMessage", () => {
  it("has its own sentence for a busy file fetch, and falls back for anything else", () => {
    expect(blobErrorMessage(new ApiError(503, "busy", null))).toBe(
      "Too many files are being fetched right now. Wait a moment and try again.",
    );
    expect(blobErrorMessage(new ApiError(404, "blob-not-found", null))).toBe(
      errorMessage(new ApiError(404, "blob-not-found", null)),
    );
  });
});
