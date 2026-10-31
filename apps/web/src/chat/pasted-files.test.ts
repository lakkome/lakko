// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import { describe, expect, it } from "vitest";

import { pastedImages } from "./pasted-files";

const image = { type: "image/png" } as File;
const pdf = { type: "application/pdf" } as File;
const clip = (types: string[], files: File[]): DataTransfer =>
  ({ types, files }) as unknown as DataTransfer;

describe("pastedImages", () => {
  it("attaches a screenshot", () => {
    expect(pastedImages(clip(["Files"], [image]))).toEqual([image]);
  });

  it("attaches a picture copied from a browser or a file manager", () => {
    expect(pastedImages(clip(["text/html", "Files"], [image]))).toEqual([image]);
    expect(pastedImages(clip(["text/plain", "Files"], [image]))).toEqual([image]);
  });

  it("leaves a copy from a document to the text field", () => {
    expect(pastedImages(clip(["text/plain", "text/html", "Files"], [image]))).toEqual([]);
  });

  it("attaches no other file and nothing from an empty clipboard", () => {
    expect(pastedImages(clip(["Files"], [pdf]))).toEqual([]);
    expect(pastedImages(null)).toEqual([]);
  });
});
