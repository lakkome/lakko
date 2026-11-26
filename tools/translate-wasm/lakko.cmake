# SPDX-License-Identifier: AGPL-3.0-or-later
# SPDX-FileCopyrightText: 2026 Lakko Contributors
#
# Included at the end of inference/wasm/CMakeLists.txt of the pinned
# mozilla/translations checkout (hook added by
# 0001-wasm-cmake-lakko-hook.patch). Adds the zstd decoder and the Lakko
# binding to the `bergamot-translator` target and turns the output into the
# ES module the Lakko web worker imports (ADR-0100 section 1).
#
# Inputs (set by build.sh on the cmake command line):
#   LAKKO_ZSTD_DECLIB   absolute path of the generated zstddeclib.c
#   LAKKO_ZSTD_LIB_DIR  absolute path of zstd's lib/ (for zstd.h)

foreach(_lakko_var LAKKO_ZSTD_DECLIB LAKKO_ZSTD_LIB_DIR)
  if(NOT DEFINED ${_lakko_var} OR NOT EXISTS "${${_lakko_var}}")
    message(FATAL_ERROR "lakko.cmake: ${_lakko_var} is not set or does not exist")
  endif()
endforeach()

set(_lakko_dir "${CMAKE_CURRENT_LIST_DIR}")

target_sources(bergamot-translator PRIVATE
  "${LAKKO_ZSTD_DECLIB}"
  "${_lakko_dir}/zstd_bindings.cpp"
  # Resolves Marian's "wasm_gemm" imports to its own fallback at link time
  # (see the file: the JS trampoline upstream uses is wrong in Safari).
  "${_lakko_dir}/gemm_direct.cpp"
)

# The decoder is decompression only: no legacy formats, no x86 assembly
# (there is none for wasm), no debug output.
set_source_files_properties("${LAKKO_ZSTD_DECLIB}" PROPERTIES
  LANGUAGE C
  COMPILE_DEFINITIONS "ZSTD_DISABLE_ASM=1;ZSTD_LEGACY_SUPPORT=0;DEBUGLEVEL=0"
)
set_source_files_properties("${_lakko_dir}/zstd_bindings.cpp" PROPERTIES
  INCLUDE_DIRECTORIES "${LAKKO_ZSTD_LIB_DIR};${CMAKE_SOURCE_DIR}/src/translator;${CMAKE_SOURCE_DIR}"
)

# Eigen (pulled in by onnxjs's sgemm through `Eigen/Dense`) is MPL-2.0
# except three sparse-solver files under LGPL-2.1, which Dense does not
# include. EIGEN_MPL2_ONLY makes Eigen refuse to compile if one of them is
# ever included (ADR-0100 section 2, licences).
# Deferred to the end of the configure: onnx-sgemm is defined in a
# directory processed after this one.
function(lakko_eigen_mpl2_only)
  if(NOT TARGET onnx-sgemm)
    message(FATAL_ERROR "lakko.cmake: target onnx-sgemm not found; check where Eigen is compiled")
  endif()
  target_compile_definitions(onnx-sgemm PRIVATE EIGEN_MPL2_ONLY)
endfunction()
cmake_language(DEFER DIRECTORY "${CMAKE_SOURCE_DIR}" CALL lakko_eigen_mpl2_only)

set_target_properties(bergamot-translator PROPERTIES
  OUTPUT_NAME "lakko_translate"
  SUFFIX ".mjs"
)

# These come after the options of inference/CMakeLists.txt on the link line,
# and emcc takes the last value of a setting, so -sENVIRONMENT here replaces
# the upstream "web,worker".
target_link_options(bergamot-translator PRIVATE
  -sMODULARIZE=1
  -sEXPORT_ES6=1
  -sEXPORT_NAME=loadLakkoTranslate
  # Upstream sets 0, which emcc 3.1.8 refuses together with MODULARIZE.
  # Import and export names are then minified; nothing outside the generated
  # glue depends on them, since gemm_direct.cpp removed the only imports the
  # upstream JS provided by name.
  -sDECLARE_ASM_MODULE_EXPORTS=1
  -sENVIRONMENT=worker
  # Firefox's starting size (40 MiB); the memory still grows.
  -sINITIAL_MEMORY=41943040
  -sALLOW_MEMORY_GROWTH=1
  # Repeated from upstream so the CSP guarantee does not depend on it:
  # no eval and no new Function anywhere in the glue.
  -sDYNAMIC_EXECUTION=0
  -sFILESYSTEM=0
  "SHELL:--post-js ${_lakko_dir}/lakko-post.js"
)
