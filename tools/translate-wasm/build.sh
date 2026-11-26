#!/usr/bin/env bash
# SPDX-License-Identifier: AGPL-3.0-or-later
# SPDX-FileCopyrightText: 2026 Lakko Contributors
#
# Builds Lakko's translation engine for the web client (ADR-0100 section 1):
# the inference engine of mozilla/translations at a pinned commit, compiled
# with the emsdk that repository pins, plus zstd's decompress-only decoder and
# the Lakko binding in this directory, into
#
#   apps/web/wasm-translate/lakko_translate.mjs    ES module, default export loadLakkoTranslate
#   apps/web/wasm-translate/lakko_translate.wasm
#   apps/web/wasm-translate/lakko_translate.d.mts  types (copied from this directory)
#
# Everything fetched lives in ${XDG_CACHE_HOME:-$HOME/.cache}/lakko/translate-wasm
# and is checked against the pins below before it is used. Lakko's changes to
# Mozilla's tree are the patch files in this directory, applied by this script.
#
# A rebuild is skipped when the inputs (pins, tool versions, every file in this
# directory) and the three outputs kept in the cache (<cache>/out) are what
# its stamp says; the cached outputs are then copied to apps/web/wasm-translate if absent.
#
# Needs: bash, git, curl, python3 (>= 3.8), shasum or sha256sum, tar.
# Environment: LAKKO_TRANSLATE_JOBS (parallel compile jobs, default min(ncpu, 8));
# LAKKO_TRANSLATE_FORCE=1 rebuilds even when the stamp matches.

set -euo pipefail

# ---------------------------------------------------------------------------
# Pins
# ---------------------------------------------------------------------------

readonly TRANSLATIONS_URL="https://github.com/mozilla/translations.git"
readonly TRANSLATIONS_COMMIT="69c16b73f5e907466799d79e14b8df2241479c41"

# The emsdk commit recorded as the inference/3rd_party/emsdk submodule at
# TRANSLATIONS_COMMIT, and the version inference/scripts/build-wasm.py
# installs from it. The script refuses to run if the two disagree.
readonly EMSDK_URL="https://github.com/emscripten-core/emsdk.git"
readonly EMSDK_COMMIT="2346baa7bb44a4a0571cc75f1986ab9aaa35aa03"
readonly EMSDK_VERSION="3.1.8"

# zstd release tarball; the SHA-256 is the one published beside it
# (zstd-1.5.7.tar.gz.sha256 on the GitHub release v1.5.7), checked on download.
readonly ZSTD_VERSION="1.5.7"
readonly ZSTD_URL="https://github.com/facebook/zstd/releases/download/v${ZSTD_VERSION}/zstd-${ZSTD_VERSION}.tar.gz"
readonly ZSTD_SHA256="eb33e51f49a15e023950cd7825ca74a4a2b43db8354825ac24fc1b7ee09e6fa3"

# Build tools, from PyPI into a venv in the cache. CMake 4 refuses the
# cmake_minimum_required of Mozilla's tree, so CMake 3 is pinned.
readonly CMAKE_PIP_VERSION="3.31.10"
readonly NINJA_PIP_VERSION="1.13.2"

# Submodules the inference build reads (the emsdk submodule is replaced by
# the separate checkout below; the pipeline's top-level 3rd_party is not
# needed).
readonly SUBMODULE_PATHS=(
  "inference/3rd_party/ssplit-cpp"
  "inference/marian-fork/src/3rd_party"
)

# ---------------------------------------------------------------------------
# Paths
# ---------------------------------------------------------------------------

HERE="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd -P)"
REPO="$(cd -- "${HERE}/../.." && pwd -P)"
# Not apps/web/wasm: wasm-pack runs wasm-opt over every .wasm in its own
# output directory, without SIMD enabled, and fails on this one.
OUT_DIR="${REPO}/apps/web/wasm-translate"
CACHE="${XDG_CACHE_HOME:-${HOME}/.cache}/lakko/translate-wasm"
SRC="${CACHE}/translations"
EMSDK="${CACHE}/emsdk"
VENV="${CACHE}/venv"
BUILD="${CACHE}/build"
GEN="${CACHE}/generated"
# The finished outputs and their stamp are kept in the cache too, so a run
# after something emptied apps/web/wasm-translate copies them back instead of
# rebuilding.
CACHE_OUT="${CACHE}/out"
STAMP="${CACHE_OUT}/stamp"

readonly OUTPUTS=(lakko_translate.mjs lakko_translate.wasm lakko_translate.d.mts)

log() { printf '[translate-wasm] %s\n' "$*" >&2; }
die() { printf '[translate-wasm] ERROR: %s\n' "$*" >&2; exit 1; }

need() { command -v "$1" >/dev/null 2>&1 || die "'$1' is required and was not found on PATH"; }

sha256_of() {
  if command -v shasum >/dev/null 2>&1; then
    shasum -a 256 "$1" | cut -d' ' -f1
  else
    sha256sum "$1" | cut -d' ' -f1
  fi
}

sha256_of_stdin() {
  if command -v shasum >/dev/null 2>&1; then
    shasum -a 256 | cut -d' ' -f1
  else
    sha256sum | cut -d' ' -f1
  fi
}

cpu_count() {
  if command -v nproc >/dev/null 2>&1; then nproc
  elif command -v sysctl >/dev/null 2>&1; then sysctl -n hw.ncpu
  else echo 4
  fi
}

need git
need curl
need python3
need tar
command -v shasum >/dev/null 2>&1 || command -v sha256sum >/dev/null 2>&1 || die "'shasum' or 'sha256sum' is required"
python3 -c 'import sys; sys.exit(0 if sys.version_info >= (3, 8) else 1)' || die "python3 >= 3.8 is required"

mkdir -p "${CACHE}" "${OUT_DIR}"

# ---------------------------------------------------------------------------
# Stamp: skip the build when nothing changed
# ---------------------------------------------------------------------------

inputs_digest() {
  {
    printf 'translations %s\n' "${TRANSLATIONS_COMMIT}"
    printf 'emsdk %s %s\n' "${EMSDK_COMMIT}" "${EMSDK_VERSION}"
    printf 'zstd %s %s\n' "${ZSTD_VERSION}" "${ZSTD_SHA256}"
    printf 'cmake %s ninja %s\n' "${CMAKE_PIP_VERSION}" "${NINJA_PIP_VERSION}"
    # Every Lakko file that shapes the output: the patches, the CMake
    # fragment with the link flags, the binding, the JS, the types and this
    # script. THIRD_PARTY.md is documentation and does not.
    local f
    for f in $(cd "${HERE}" && find . -type f ! -name 'THIRD_PARTY.md' | LC_ALL=C sort); do
      printf 'file %s %s\n' "${f}" "$(sha256_of "${HERE}/${f}")"
    done
  } | sha256_of_stdin
}

INPUTS="$(inputs_digest)"

# The stamp lists the inputs digest and the SHA-256 of each output in the
# cache. It matches when the digest is this run's and every cached output has
# its recorded hash.
stamp_matches() {
  [[ -f "${STAMP}" ]] || return 1
  [[ "$(sed -n 's/^inputs //p' "${STAMP}")" == "${INPUTS}" ]] || return 1
  local name recorded
  for name in "${OUTPUTS[@]}"; do
    [[ -f "${CACHE_OUT}/${name}" ]] || return 1
    recorded="$(sed -n "s/^output ${name} //p" "${STAMP}")"
    [[ -n "${recorded}" && "${recorded}" == "$(sha256_of "${CACHE_OUT}/${name}")" ]] || return 1
  done
  return 0
}

# Copies each cached output to apps/web/wasm-translate unless an identical file is there.
publish_outputs() {
  local name
  for name in "${OUTPUTS[@]}"; do
    if [[ ! -f "${OUT_DIR}/${name}" ]] ||
       [[ "$(sha256_of "${OUT_DIR}/${name}")" != "$(sha256_of "${CACHE_OUT}/${name}")" ]]; then
      cp "${CACHE_OUT}/${name}" "${OUT_DIR}/${name}.tmp.$$"
      mv -f "${OUT_DIR}/${name}.tmp.$$" "${OUT_DIR}/${name}"
    fi
  done
}

if [[ "${LAKKO_TRANSLATE_FORCE:-0}" != "1" ]] && stamp_matches; then
  publish_outputs
  log "up to date (inputs ${INPUTS:0:12}); set LAKKO_TRANSLATE_FORCE=1 to rebuild"
  exit 0
fi

# One build at a time per cache.
LOCK="${CACHE}/lock"
if ! mkdir "${LOCK}" 2>/dev/null; then
  holder="$(cat "${LOCK}/pid" 2>/dev/null || true)"
  if [[ -n "${holder}" ]] && kill -0 "${holder}" 2>/dev/null; then
    die "another build (pid ${holder}) holds ${LOCK}"
  fi
  log "removing a stale lock left by pid ${holder:-unknown}"
  rm -rf "${LOCK}"
  mkdir "${LOCK}" || die "cannot take ${LOCK}"
fi
echo "$$" > "${LOCK}/pid"
trap 'rm -rf "${LOCK}"' EXIT

started=$(date +%s)

# ---------------------------------------------------------------------------
# Build tools: CMake 3 and Ninja
# ---------------------------------------------------------------------------

tools_ok() {
  [[ -x "${VENV}/bin/cmake" && -x "${VENV}/bin/ninja" ]] || return 1
  # Captured first: with pipefail, grep -q closing the pipe early would fail
  # the writer with SIGPIPE.
  local cmake_out ninja_out
  cmake_out="$("${VENV}/bin/cmake" --version 2>/dev/null || true)"
  ninja_out="$("${VENV}/bin/python" -m pip show ninja 2>/dev/null || true)"
  [[ "${cmake_out}" == "cmake version ${CMAKE_PIP_VERSION}"* ]] || return 1
  grep -qx "Version: ${NINJA_PIP_VERSION}" <<<"${ninja_out}" || return 1
}

if ! tools_ok; then
  log "installing cmake ${CMAKE_PIP_VERSION} and ninja ${NINJA_PIP_VERSION} into ${VENV}"
  rm -rf "${VENV}"
  python3 -m venv "${VENV}"
  "${VENV}/bin/python" -m pip install --quiet --disable-pip-version-check \
    "cmake==${CMAKE_PIP_VERSION}" "ninja==${NINJA_PIP_VERSION}"
  tools_ok || die "the build tools in ${VENV} are not the pinned versions"
fi
export PATH="${VENV}/bin:${PATH}"

# ---------------------------------------------------------------------------
# mozilla/translations at the pinned commit
# ---------------------------------------------------------------------------

if [[ ! -d "${SRC}/.git" ]]; then
  log "creating ${SRC}"
  rm -rf "${SRC}"
  git init --quiet "${SRC}"
  git -C "${SRC}" remote add origin "${TRANSLATIONS_URL}"
fi
[[ "$(git -C "${SRC}" remote get-url origin)" == "${TRANSLATIONS_URL}" ]] \
  || die "${SRC} has an unexpected origin; delete it and run again"

if ! git -C "${SRC}" cat-file -e "${TRANSLATIONS_COMMIT}^{commit}" 2>/dev/null; then
  log "fetching mozilla/translations ${TRANSLATIONS_COMMIT}"
  git -C "${SRC}" fetch --quiet --filter=blob:none origin "${TRANSLATIONS_COMMIT}"
fi

# --force restores any file a previous run patched.
git -C "${SRC}" -c advice.detachedHead=false checkout --quiet --force "${TRANSLATIONS_COMMIT}"
[[ "$(git -C "${SRC}" rev-parse HEAD)" == "${TRANSLATIONS_COMMIT}" ]] \
  || die "mozilla/translations is not at ${TRANSLATIONS_COMMIT}"

recorded_emsdk="$(git -C "${SRC}" ls-tree HEAD inference/3rd_party/emsdk | awk '{print $3}')"
[[ "${recorded_emsdk}" == "${EMSDK_COMMIT}" ]] \
  || die "the emsdk submodule at ${TRANSLATIONS_COMMIT} is ${recorded_emsdk}, the pin says ${EMSDK_COMMIT}"
grep -q "^EMSDK_VERSION = \"${EMSDK_VERSION}\"$" "${SRC}/inference/scripts/build-wasm.py" \
  || die "inference/scripts/build-wasm.py no longer installs emsdk ${EMSDK_VERSION}"

log "updating submodules"
git -C "${SRC}" submodule update --quiet --init --recursive --force --filter=blob:none -- "${SUBMODULE_PATHS[@]}"
bad="$(git -C "${SRC}" submodule status --recursive -- "${SUBMODULE_PATHS[@]}" | grep -v '^ ' || true)"
[[ -z "${bad}" ]] || die "submodules are not at their recorded commits: ${bad}"

for patch in $(cd "${HERE}" && find . -maxdepth 1 -name '*.patch' | LC_ALL=C sort); do
  log "applying ${patch#./}"
  git -C "${SRC}" apply --check "${HERE}/${patch}" || die "${patch#./} does not apply to ${TRANSLATIONS_COMMIT}"
  git -C "${SRC}" apply "${HERE}/${patch}"
done

# ---------------------------------------------------------------------------
# emsdk at the pinned commit and version
# ---------------------------------------------------------------------------

if [[ ! -d "${EMSDK}/.git" ]]; then
  log "creating ${EMSDK}"
  rm -rf "${EMSDK}"
  git init --quiet "${EMSDK}"
  git -C "${EMSDK}" remote add origin "${EMSDK_URL}"
fi
[[ "$(git -C "${EMSDK}" remote get-url origin)" == "${EMSDK_URL}" ]] \
  || die "${EMSDK} has an unexpected origin; delete it and run again"
if ! git -C "${EMSDK}" cat-file -e "${EMSDK_COMMIT}^{commit}" 2>/dev/null; then
  log "fetching emsdk ${EMSDK_COMMIT}"
  git -C "${EMSDK}" fetch --quiet origin "${EMSDK_COMMIT}"
fi
git -C "${EMSDK}" -c advice.detachedHead=false checkout --quiet --force "${EMSDK_COMMIT}"
[[ "$(git -C "${EMSDK}" rev-parse HEAD)" == "${EMSDK_COMMIT}" ]] || die "emsdk is not at ${EMSDK_COMMIT}"

log "emsdk ${EMSDK_VERSION}: install (skipped by emsdk when present) and activate"
(cd "${EMSDK}" && ./emsdk install "${EMSDK_VERSION}" >&2 && ./emsdk activate "${EMSDK_VERSION}" >/dev/null)
# shellcheck disable=SC1091
source "${EMSDK}/emsdk_env.sh" >/dev/null 2>&1
command -v emcc >/dev/null 2>&1 || die "emcc is not on PATH after activating emsdk"
emcc_version="$(emcc --version)"
emcc_version="${emcc_version%%$'\n'*}"
[[ "${emcc_version}" == *" ${EMSDK_VERSION} "* ]] || die "emcc reports '${emcc_version}', expected ${EMSDK_VERSION}"
log "${emcc_version}"

# ---------------------------------------------------------------------------
# zstd decompress-only single-file decoder
# ---------------------------------------------------------------------------

ZSTD_TARBALL="${CACHE}/zstd-${ZSTD_VERSION}.tar.gz"
if [[ ! -f "${ZSTD_TARBALL}" ]]; then
  log "downloading zstd ${ZSTD_VERSION}"
  curl --fail --location --silent --show-error --output "${ZSTD_TARBALL}.part" "${ZSTD_URL}"
  mv "${ZSTD_TARBALL}.part" "${ZSTD_TARBALL}"
fi
got="$(sha256_of "${ZSTD_TARBALL}")"
if [[ "${got}" != "${ZSTD_SHA256}" ]]; then
  rm -f "${ZSTD_TARBALL}"
  die "zstd-${ZSTD_VERSION}.tar.gz has SHA-256 ${got}, expected ${ZSTD_SHA256}; the file was deleted"
fi

rm -rf "${GEN}"
mkdir -p "${GEN}/zstd"
tar -xzf "${ZSTD_TARBALL}" -C "${GEN}/zstd"
ZSTD_ROOT="${GEN}/zstd/zstd-${ZSTD_VERSION}"
[[ -d "${ZSTD_ROOT}/lib" ]] || die "zstd tarball does not contain zstd-${ZSTD_VERSION}/lib"
# The command build/single_file_libs/create_single_file_decoder.sh runs.
log "generating zstddeclib.c"
(cd "${ZSTD_ROOT}/build/single_file_libs" &&
  python3 -I combine.py -r ../../lib -x legacy/zstd_legacy.h -o "${GEN}/zstddeclib.c" zstddeclib-in.c)
[[ -s "${GEN}/zstddeclib.c" ]] || die "zstddeclib.c was not generated"

# ---------------------------------------------------------------------------
# Configure and build
# ---------------------------------------------------------------------------

jobs="${LAKKO_TRANSLATE_JOBS:-$(cpu_count)}"
if [[ -z "${LAKKO_TRANSLATE_JOBS:-}" && "${jobs}" -gt 8 ]]; then jobs=8; fi

# Source paths reach the binary through __FILE__ (assertion and exception
# messages). They are rewritten so the output names no local directory (no
# user name) and is the same wherever the cache and the repository live.
case "${CACHE}${HERE}" in
  *" "*) die "the cache path and the repository path must not contain spaces (${CACHE}, ${HERE})" ;;
esac
# Marian's CMakeLists replaces CMAKE_CXX_FLAGS, so the maps go through
# EMCC_CFLAGS, which emcc adds to every compile it runs.
export EMCC_CFLAGS="-ffile-prefix-map=${CACHE}=/lakko-translate-cache -ffile-prefix-map=${HERE}=/lakko-translate-wasm"

rm -rf "${BUILD}"
log "configuring"
emcmake cmake -G Ninja -S "${SRC}/inference" -B "${BUILD}" \
  -DCMAKE_BUILD_TYPE=Release \
  -DCOMPILE_WASM=on \
  -DWORMHOLE=off \
  -DGIT_SUBMODULE=OFF \
  -DLAKKO_TRANSLATE_CMAKE="${HERE}/lakko.cmake" \
  -DLAKKO_ZSTD_DECLIB="${GEN}/zstddeclib.c" \
  -DLAKKO_ZSTD_LIB_DIR="${ZSTD_ROOT}/lib" >&2

log "building with ${jobs} jobs"
cmake --build "${BUILD}" --target bergamot-translator -j "${jobs}" >&2

BUILT_MJS="${BUILD}/lakko_translate.mjs"
BUILT_WASM="${BUILD}/lakko_translate.wasm"
[[ -s "${BUILT_MJS}" && -s "${BUILT_WASM}" ]] || die "the link did not produce lakko_translate.mjs and .wasm in ${BUILD}"

# gemm_direct.cpp resolves Marian's gemm imports inside the module; upstream's
# JS gemm module (inference/wasm/import-gemm-module.js and
# patch-artifacts-import-gemm-module.sh) is not used. If the link went back to
# importing them, the module could not be instantiated, so it is checked here.
if LC_ALL=C grep -aqF -e 'wasm_gemm' -e 'int8_prepare_a' "${BUILT_WASM}"; then
  die "lakko_translate.wasm still imports the gemm functions; gemm_direct.cpp was not linked in"
fi

# The web client's CSP has no 'unsafe-eval': the glue must not need it.
if grep -nE 'new Function|[^A-Za-z_$.]eval\(' "${BUILT_MJS}" >&2; then
  die "lakko_translate.mjs contains new Function or eval(; it would not run under the CSP"
fi
grep -q 'export default loadLakkoTranslate' "${BUILT_MJS}" || die "lakko_translate.mjs has no default export loadLakkoTranslate"
grep -q 'zstdDecompressToAligned' "${BUILT_MJS}" || die "lakko_translate.mjs does not contain the zstd wrapper"
for built in "${BUILT_MJS}" "${BUILT_WASM}"; do
  if LC_ALL=C grep -aqF -e "${CACHE}" -e "${HERE}" -e "${HOME}" "${built}"; then
    die "$(basename "${built}") contains a local path; the prefix maps did not cover it"
  fi
done
if grep -q 'SharedArrayBuffer' "${BUILT_MJS}"; then
  log "note: the glue mentions SharedArrayBuffer (feature detection only is expected)"
fi

# ---------------------------------------------------------------------------
# Install the outputs and write the stamp
# ---------------------------------------------------------------------------

rm -rf "${CACHE_OUT}"
mkdir -p "${CACHE_OUT}"
cp "${BUILT_WASM}" "${CACHE_OUT}/lakko_translate.wasm"
cp "${BUILT_MJS}" "${CACHE_OUT}/lakko_translate.mjs"
cp "${HERE}/lakko_translate.d.mts" "${CACHE_OUT}/lakko_translate.d.mts"

{
  printf 'inputs %s\n' "${INPUTS}"
  for name in "${OUTPUTS[@]}"; do
    printf 'output %s %s\n' "${name}" "$(sha256_of "${CACHE_OUT}/${name}")"
  done
} > "${STAMP}.tmp.$$"
mv -f "${STAMP}.tmp.$$" "${STAMP}"

publish_outputs

elapsed=$(( $(date +%s) - started ))
log "built in ${elapsed} s:"
for name in "${OUTPUTS[@]}"; do
  log "  ${OUT_DIR}/${name} ($(wc -c < "${OUT_DIR}/${name}" | tr -d ' ') bytes)"
done
