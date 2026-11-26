// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

// Binds Marian's integer gemm interface to its own wasm fallback at link time.
//
// Upstream, marian-fork/src/tensors/cpu/wasm_intgemm_interface.h declares the
// seven functions below as imports from a "wasm_gemm" module, and
// inference/wasm/import-gemm-module.js supplies them at instantiation: either
// Firefox's privileged mozIntGemm, or JS functions that call the fallback
// exports (int8PrepareAFallback, ...) of the same module.
//
// In a web page mozIntGemm never exists (ADR-0100 section 1), so every gemm
// call went wasm -> JS -> wasm. In Safari (JavaScriptCore, 27.2, measured
// 2026-11-25) that round trip returns wrong results once the trampoline is a
// strict-mode function, which every function in an ES module is: the first
// sentence translates, later ones come back cut short or empty. The same
// trampoline in sloppy mode, and the same wasm in Firefox and Opera, are
// correct.
//
// Defining the imported symbols here makes wasm-ld resolve them inside the
// module: the wasm has no "wasm_gemm" import, no JS sits on the gemm path, and
// the call is a direct wasm call to the same fallback code with the same
// arguments.

#include <cstdint>

namespace {
using Index = std::uint32_t;
}

extern "C" {

// The fallback implementations, marian-fork/src/tensors/cpu/wasm_intgemm_fallback.cpp.
void int8PrepareAFallback(const float* input_A, float scale, float zero_point, Index rows_A, Index width,
                          std::int8_t* output);
void int8PrepareBFallback(const float* input_B, float scale, float zero_point, Index width, Index cols_B,
                          std::int8_t* output);
void int8PrepareBFromTransposedFallback(const float* input_B_transposed, float scale, float zero_point,
                                        Index width, Index cols_B, std::int8_t* output);
void int8PrepareBFromQuantizedTransposedFallback(const std::int8_t* input_B_quant_transposed, Index width,
                                                 Index cols_B, std::int8_t* output);
void int8PrepareBiasFallback(const std::int8_t* input_B_prepared, float scale_A, float zero_point_A,
                             float scale_B, float zero_point_B, Index width, Index cols_B,
                             const float* input_bias, float* output);
void int8MultiplyAndAddBiasFallback(const std::int8_t* input_A_prepared, float scale_A, float zero_point_A,
                                    const std::int8_t* input_B_prepared, float scale_B, float zero_point_B,
                                    const float* input_bias_prepared, float unquant_multiplier, Index rows_A,
                                    Index width, Index cols_B, float* output);
void int8SelectColumnsOfBFallback(const std::int8_t* input_B_prepared, Index width, Index cols_B,
                                  const Index* cols, const Index num_cols, std::int8_t* output);

// The interface, same names and signatures as wasm_intgemm_interface.h.

void int8PrepareA(const float* input_A, float scale, float zero_point, Index rows_A, Index width,
                  std::int8_t* output) {
  int8PrepareAFallback(input_A, scale, zero_point, rows_A, width, output);
}

void int8PrepareB(const float* input_B, float scale, float zero_point, Index width, Index cols_B,
                  std::int8_t* output) {
  int8PrepareBFallback(input_B, scale, zero_point, width, cols_B, output);
}

void int8PrepareBFromTransposed(const float* input_B_transposed, float scale, float zero_point, Index width,
                                Index cols_B, std::int8_t* output) {
  int8PrepareBFromTransposedFallback(input_B_transposed, scale, zero_point, width, cols_B, output);
}

void int8PrepareBFromQuantizedTransposed(const std::int8_t* input_B_quant_transposed, Index width, Index cols_B,
                                         std::int8_t* output) {
  int8PrepareBFromQuantizedTransposedFallback(input_B_quant_transposed, width, cols_B, output);
}

void int8PrepareBias(const std::int8_t* input_B_prepared, float scale_A, float zero_point_A, float scale_B,
                     float zero_point_B, Index width, Index cols_B, const float* input_bias, float* output) {
  int8PrepareBiasFallback(input_B_prepared, scale_A, zero_point_A, scale_B, zero_point_B, width, cols_B,
                          input_bias, output);
}

void int8MultiplyAndAddBias(const std::int8_t* input_A_prepared, float scale_A, float zero_point_A,
                            const std::int8_t* input_B_prepared, float scale_B, float zero_point_B,
                            const float* input_bias_prepared, float unquant_multiplier, Index rows_A,
                            Index width, Index cols_B, float* output) {
  int8MultiplyAndAddBiasFallback(input_A_prepared, scale_A, zero_point_A, input_B_prepared, scale_B,
                                 zero_point_B, input_bias_prepared, unquant_multiplier, rows_A, width, cols_B,
                                 output);
}

void int8SelectColumnsOfB(const std::int8_t* input_B_prepared, Index width, Index cols_B, const Index* cols,
                          const Index num_cols, std::int8_t* output) {
  int8SelectColumnsOfBFallback(input_B_prepared, width, cols_B, cols, num_cols, output);
}

}  // extern "C"
