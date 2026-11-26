// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

// zstd decoding inside lakko_translate.wasm (ADR-0100 section 3, open
// question 6): the model files arrive as Mozilla's zstd bytes and are
// decompressed straight into the AlignedMemory the engine loads, so no
// decompressed copy passes through JavaScript.
//
// Mozilla's model files are zstd frames that do not state their content size
// (they were compressed as a stream), so the caller passes the decompressed
// size from the model manifest. When a frame states a size, it must equal the
// size passed; when it states none, the decoder must produce exactly that
// many bytes. Either way the output buffer is allocated once, at its final
// size, and a frame that would decode to more or fewer bytes is refused.
//
// The functions bound here never throw. With exception catching disabled a
// C++ throw aborts the whole module, and a JS exception thrown through wasm
// frames would skip the stack restore. Errors are returned as null / -1 and
// the reason is read from `_lakkoZstdLastError`; lakko-post.js wraps both
// into the public `zstdDecompressToAligned` / `zstdFrameContentSize`, which
// throw in JavaScript.

#include <emscripten/bind.h>

#include <cstdint>
#include <cstdlib>
#include <string>
#include <vector>

#include "definitions.h"
#include "zstd.h"
#include "zstd_errors.h"

namespace {

using AlignedMemory = marian::bergamot::AlignedMemory;
using emscripten::val;

// Largest decompressed model file accepted. The largest file in Mozilla's
// collection is far below it; it bounds what a hostile frame header can make
// the module allocate.
constexpr unsigned long long kMaxContentSize = 512ULL * 1024ULL * 1024ULL;

// Largest alignment accepted (the engine asks for 256 at most).
constexpr std::size_t kMaxAlignment = 65536;

// Integers above this are not exact in a JS number.
constexpr unsigned long long kMaxSafeInteger = (1ULL << 53) - 1;

std::string g_lastError;

AlignedMemory* fail(const char* reason) {
  g_lastError = reason;
  return nullptr;
}

// zstddeclib.c is built with ZSTD_STRIP_ERROR_STRINGS, so ZSTD_getErrorName
// returns no reason; the reasons a model file can hit are named here.
void setDecodeError(std::size_t code) {
  switch (ZSTD_getErrorCode(code)) {
    case ZSTD_error_dstSize_tooSmall:
      g_lastError = "frame decodes to more bytes than the expected size";
      break;
    case ZSTD_error_corruption_detected:
      g_lastError = "frame is corrupt";
      break;
    case ZSTD_error_checksum_wrong:
      g_lastError = "frame checksum does not match";
      break;
    case ZSTD_error_srcSize_wrong:
      g_lastError = "frame is truncated";
      break;
    default:
      g_lastError = "zstd error " + std::to_string(static_cast<int>(ZSTD_getErrorCode(code)));
      break;
  }
}

bool isValidAlignment(std::size_t alignment) {
  return alignment >= sizeof(void*) && alignment <= kMaxAlignment && (alignment & (alignment - 1)) == 0;
}

// Copies a Uint8Array into the module's memory with one TypedArray.set.
std::vector<std::uint8_t> copyIn(const val& src) {
  return emscripten::convertJSArrayToNumberVector<std::uint8_t>(src);
}

// `expectedSize` is the decompressed size from the manifest, or a negative
// value when the caller has none (then the frame must state its size).
AlignedMemory* lakkoZstdDecompress(const val& src, std::size_t alignment, double expectedSize) {
  g_lastError.clear();
  if (!isValidAlignment(alignment)) {
    return fail("alignment must be a power of two between the pointer size and 65536");
  }
  const bool haveExpected = expectedSize >= 0;
  if (haveExpected && (expectedSize < 1 || expectedSize > static_cast<double>(kMaxContentSize) ||
                       expectedSize != static_cast<double>(static_cast<unsigned long long>(expectedSize)))) {
    return fail("expected size must be an integer between 1 and 512 MiB");
  }

  const std::vector<std::uint8_t> input = copyIn(src);
  if (input.empty()) {
    return fail("empty input");
  }

  const unsigned long long stated = ZSTD_getFrameContentSize(input.data(), input.size());
  if (stated == ZSTD_CONTENTSIZE_ERROR) {
    return fail("not a zstd frame");
  }
  unsigned long long contentSize = 0;
  if (stated == ZSTD_CONTENTSIZE_UNKNOWN) {
    if (!haveExpected) {
      return fail("frame does not state its content size and no expected size was given");
    }
    contentSize = static_cast<unsigned long long>(expectedSize);
  } else {
    if (stated == 0) {
      return fail("frame is empty");
    }
    if (stated > kMaxContentSize) {
      return fail("frame content size is over 512 MiB");
    }
    if (haveExpected && stated != static_cast<unsigned long long>(expectedSize)) {
      return fail("frame content size differs from the expected size");
    }
    contentSize = stated;
  }

  // One frame and nothing after it.
  const std::size_t frameSize = ZSTD_findFrameCompressedSize(input.data(), input.size());
  if (ZSTD_isError(frameSize)) {
    return fail("frame is truncated or malformed");
  }
  if (frameSize != input.size()) {
    return fail("input carries data after the first frame");
  }

  const std::size_t outSize = static_cast<std::size_t>(contentSize);

  // AlignedMemory aborts the module when posix_memalign fails, so the
  // allocation is tried first and a failure is reported instead.
  void* probe = nullptr;
  if (posix_memalign(&probe, alignment, outSize) != 0 || probe == nullptr) {
    return fail("out of memory");
  }
  std::free(probe);

  auto* out = new AlignedMemory(outSize, alignment);
  ZSTD_DCtx* dctx = ZSTD_createDCtx();
  if (dctx == nullptr) {
    delete out;
    return fail("out of memory");
  }
  const std::size_t written = ZSTD_decompressDCtx(dctx, out->begin(), outSize, input.data(), input.size());
  ZSTD_freeDCtx(dctx);

  if (ZSTD_isError(written)) {
    setDecodeError(written);
    delete out;
    return nullptr;
  }
  if (written != outSize) {
    delete out;
    return fail("decompressed size differs from the expected size");
  }
  return out;
}

double lakkoZstdFrameContentSize(const val& header) {
  const std::vector<std::uint8_t> input = copyIn(header);
  if (input.empty()) {
    return -1.0;
  }
  const unsigned long long contentSize = ZSTD_getFrameContentSize(input.data(), input.size());
  if (contentSize == ZSTD_CONTENTSIZE_ERROR || contentSize == ZSTD_CONTENTSIZE_UNKNOWN ||
      contentSize > kMaxSafeInteger) {
    return -1.0;
  }
  return static_cast<double>(contentSize);
}

std::string lakkoZstdLastError() { return g_lastError; }

}  // namespace

EMSCRIPTEN_BINDINGS(lakko_zstd) {
  emscripten::function("_lakkoZstdDecompress", &lakkoZstdDecompress, emscripten::allow_raw_pointers());
  emscripten::function("_lakkoZstdFrameContentSize", &lakkoZstdFrameContentSize);
  emscripten::function("_lakkoZstdLastError", &lakkoZstdLastError);
  emscripten::constant("_lakkoZstdMaxContentSize", static_cast<double>(kMaxContentSize));
}
