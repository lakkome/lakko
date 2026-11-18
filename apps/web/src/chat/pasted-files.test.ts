// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import { describe, expect, it } from "vitest";

import { pastedFiles } from "./pasted-files";

const image = { type: "image/png" } as File;
const video = { type: "video/mp4" } as File;
const pdf = { type: "application/pdf" } as File;
const clip = (types: string[], files: File[], items: DataTransferItem[] = []): DataTransfer =>
  ({ types, files, items }) as unknown as DataTransfer;
const fileItem = (f: File): DataTransferItem =>
  ({ kind: "file", getAsFile: () => f }) as unknown as DataTransferItem;
const textItem = { kind: "string", getAsFile: () => null } as unknown as DataTransferItem;

describe("pastedFiles", () => {
  it("attaches a screenshot", () => {
    expect(pastedFiles(clip(["Files"], [image]))).toEqual([image]);
  });
  it("attaches a browser's copied image and a file manager's copy", () => {
    expect(pastedFiles(clip(["text/html", "Files"], [image]))).toEqual([image]);
    expect(pastedFiles(clip(["text/plain", "Files"], [image]))).toEqual([image]);
  });
  it("attaches videos and documents too, as a drop does", () => {
    expect(pastedFiles(clip(["text/plain", "Files"], [video, pdf]))).toEqual([video, pdf]);
  });
  it("leaves a copy from a document as text", () => {
    expect(pastedFiles(clip(["text/plain", "text/html", "Files"], [image]))).toEqual([]);
  });
  it("reads files listed only as items", () => {
    expect(pastedFiles(clip(["Files"], [], [textItem, fileItem(video)]))).toEqual([video]);
  });
  it("attaches nothing from plain text or no clipboard", () => {
    expect(pastedFiles(clip(["text/plain"], [], [textItem]))).toEqual([]);
    expect(pastedFiles(null)).toEqual([]);
  });
});
