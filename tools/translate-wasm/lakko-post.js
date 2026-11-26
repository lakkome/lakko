// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

// Linked with --post-js into lakko_translate.mjs (inside the module factory,
// where `Module` is the module object). Wraps the bindings of
// zstd_bindings.cpp, which report errors as null / -1, into the public API,
// which throws. The bindings are looked up at call time: embind registers
// them when the runtime starts, after this code has run.

// Largest zstd frame header (ZSTD_FRAMEHEADERSIZE_MAX).
var LAKKO_ZSTD_FRAME_HEADER_MAX = 18;

function lakkoRequireBytes(src, name) {
  if (!(src instanceof Uint8Array)) {
    throw new TypeError(name + ": src must be a Uint8Array");
  }
}

/**
 * Decompresses one zstd frame into a new AlignedMemory of exactly the
 * decompressed size and returns it; the caller owns it (`delete()` it, or
 * hand it to the engine, which takes its contents).
 *
 * `expectedSize` is the decompressed size from the model manifest. Mozilla's
 * model files do not state their size in the frame, so for them it is
 * required. When the frame does state a size it must equal `expectedSize`.
 * Throws when neither gives a size, on a size over 512 MiB, data after the
 * frame, output longer or shorter than the size, or any decode error; never
 * returns partial output.
 */
Module["zstdDecompressToAligned"] = function (src, alignment, expectedSize) {
  lakkoRequireBytes(src, "zstdDecompressToAligned");
  if (!Number.isInteger(alignment) || alignment <= 0) {
    throw new TypeError("zstdDecompressToAligned: alignment must be a positive integer");
  }
  var expected = -1;
  if (expectedSize !== undefined) {
    if (!Number.isSafeInteger(expectedSize) || expectedSize <= 0) {
      throw new TypeError("zstdDecompressToAligned: expectedSize must be a positive integer");
    }
    expected = expectedSize;
  }
  var memory = Module["_lakkoZstdDecompress"](src, alignment, expected);
  if (memory === null || memory === undefined) {
    throw new Error("zstdDecompressToAligned: " + Module["_lakkoZstdLastError"]());
  }
  return memory;
};

/**
 * The content size a zstd frame header states, or -1 when the header is
 * invalid, states no size, or states one above 2^53 - 1.
 */
Module["zstdFrameContentSize"] = function (src) {
  lakkoRequireBytes(src, "zstdFrameContentSize");
  return Module["_lakkoZstdFrameContentSize"](src.subarray(0, LAKKO_ZSTD_FRAME_HEADER_MAX));
};
