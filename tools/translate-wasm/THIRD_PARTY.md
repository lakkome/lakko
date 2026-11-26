<!-- SPDX-License-Identifier: AGPL-3.0-or-later -->
<!-- SPDX-FileCopyrightText: 2026 Lakko Contributors -->

# Third-party code in `lakko_translate.wasm`

`build.sh` compiles `lakko_translate.{mjs,wasm}` from code Lakko does not own.
This list records what goes into the build and the licence each component's
own licence file states, read from the checkouts `build.sh` uses
(mozilla/translations at `69c16b73f5e907466799d79e14b8df2241479c41`, zstd
1.5.7). Paths are relative to the mozilla/translations checkout unless they
start with `zstd-1.5.7/`.

"Compiled" means `build.ninja` compiles a source file of that component into
an object linked into the target; "headers" means only headers are included.
Whether every compiled object survives the link into the final `.wasm` was
not checked.

## Compiled

| Component | Licence (as its file states) | Licence file |
|---|---|---|
| mozilla/translations `inference/` (bergamot translator, the embind bindings; `wasm/import-gemm-module.js` is not used, see `gemm_direct.cpp`) | MPL-2.0 | `LICENSE` (the repository root; `inference/` has no licence file of its own) |
| marian-fork (Marian NMT) | MIT | `inference/marian-fork/LICENSE.md` |
| intgemm | MIT | `inference/marian-fork/src/3rd_party/intgemm/LICENSE` |
| sentencepiece | Apache-2.0 | `inference/marian-fork/src/3rd_party/sentencepiece/LICENSE` |
| protobuf-lite (inside sentencepiece) | BSD-3-Clause (Google) | `inference/marian-fork/src/3rd_party/sentencepiece/third_party/protobuf-lite/LICENSE` |
| Abseil subset (inside sentencepiece) | Apache-2.0 | `inference/marian-fork/src/3rd_party/sentencepiece/third_party/absl/LICENSE` |
| yaml-cpp | MIT | `inference/marian-fork/src/3rd_party/yaml-cpp/LICENSE` |
| pathie-cpp | BSD (two numbered clauses in the file; read as BSD-2-Clause, not confirmed against the SPDX text) | `inference/marian-fork/src/3rd_party/pathie-cpp/LICENSE` |
| phf | MIT | `inference/marian-fork/src/3rd_party/phf/LICENSE` |
| cnpy | MIT | `inference/marian-fork/src/3rd_party/cnpy/LICENSE` |
| onnxjs wasm-ops | MIT (Microsoft) | `inference/marian-fork/src/3rd_party/onnxjs/LICENSE` |
| onnx protobuf (`marian-fork/src/3rd_party/onnx/protobuf`) | not in the binary: `onnx-ml.pb-wrapper.cpp` and `src/onnx/*.cpp` are wrapped in `#ifdef USE_ONNX`, which this build does not define, so they compile to nothing (the only protobuf messages in the wasm are sentencepiece's) | - |
| zstd decompressor (`zstddeclib.c`, generated from `lib/`) | BSD-3-Clause (zstd is dual BSD / GPLv2; Lakko uses it under BSD) | `zstd-1.5.7/LICENSE` |
| Emscripten 3.1.8 runtime and system libraries (JS glue, embind, musl libc, libc++) | Emscripten MIT OR NCSA; musl MIT; libc++ Apache-2.0 WITH LLVM-exception | in the emsdk checkout: `upstream/emscripten/LICENSE`, `upstream/emscripten/system/lib/libc/musl/COPYRIGHT`, `upstream/emscripten/system/lib/libcxx/LICENSE.TXT` |

## Headers only

| Component | Licence | Licence file |
|---|---|---|
| spdlog | MIT | `inference/marian-fork/src/3rd_party/spdlog/LICENSE` |
| CLI11 | BSD-3-Clause | `inference/marian-fork/src/3rd_party/CLI/LICENSE` |
| mio | MIT | `inference/marian-fork/src/3rd_party/mio/LICENSE` |
| half_float | MIT (stated in `umHalf.h`) | `inference/marian-fork/src/3rd_party/half_float/umHalf.h` |
| sse_mathfun | zlib (stated in the file) | `inference/marian-fork/src/3rd_party/sse_mathfun.h` |
| Eigen (inside onnxjs; `onnxjs/src/wasm-ops/gemm.cpp` includes `Eigen/Core` and `Eigen/Dense`) | MPL-2.0: the three LGPL-2.1 files of this Eigen (`OrderingMethods/Amd.h`, `IterativeLinearSolvers/IncompleteLUT.h`, `SparseCholesky/SimplicialCholesky_impl.h`) are sparse-solver headers `Dense` does not include, and `lakko.cmake` defines `EIGEN_MPL2_ONLY` on `onnx-sgemm`, so including one fails the build | `inference/marian-fork/src/3rd_party/onnxjs/deps/eigen/COPYING.*` |

## Fetched but not compiled into this build

- ruy (Apache-2.0): `USE_RUY` is off for the wasm build.
- ssplit-cpp (Apache-2.0, with LGPL-2.1 data files) and the PCRE2 it uses:
  `inference/3rd_party/CMakeLists.txt` adds ssplit only when
  `COMPILE_WASM` is off.
- fbgemm, nccl, simd_utils, simple-websocket-server: not built for wasm.
- Eigen's own `blas/` and `lapack/` libraries: `build.ninja` defines them, but
  the link line of the target does not use them (only `libonnx-sgemm.a` from
  onnxjs).

## Not in the module

The model files (Mozilla, MPL-2.0 per ADR-0100 open question 5) are data the
worker downloads, not part of this build.
