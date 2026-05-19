<!-- SPDX-License-Identifier: AGPL-3.0-or-later -->
<!-- SPDX-FileCopyrightText: 2026 Lakko Contributors -->

# `services/sfu` — operational playbook

Exact commands for each operation on the LiveKit fork.
The high-level strategy is in [ADR-0008](../../docs/adr/0008-livekit-fork-strategy.md);
this file is the recipe book.

All commands assume `pwd = repo root`.

## 1. Initial vendor pull (one-time)

Run once when 3b1a's vendor step happens. After this commit
lands, the table in [`UPSTREAM.md`](./UPSTREAM.md) is filled
in and the rest of this playbook becomes operative.

```bash
# Pick the upstream pin
LIVEKIT_TAG="v1.7.4"          # update as needed at vendor time
PROTOCOL_TAG="$(curl -s https://raw.githubusercontent.com/livekit/livekit/${LIVEKIT_TAG}/go.mod \
  | awk '/livekit\/protocol/ {print $2}')"

# Create the vendor dirs
mkdir -p services/sfu/upstream

# Clone shallowly at the pinned tag — keeps the repo small.
git clone --depth 1 --branch "${LIVEKIT_TAG}" \
  https://github.com/livekit/livekit.git services/sfu/upstream/livekit
git clone --depth 1 --branch "${PROTOCOL_TAG}" \
  https://github.com/livekit/protocol.git services/sfu/upstream/protocol

# Detach: drop the upstream .git directories so the vendor
# becomes plain tracked files. The pin lives in UPSTREAM.md,
# NOT in nested .git histories.
rm -rf services/sfu/upstream/livekit/.git
rm -rf services/sfu/upstream/protocol/.git

# Fill in UPSTREAM.md with the resolved SHAs:
LIVEKIT_SHA="$(git ls-remote https://github.com/livekit/livekit.git \
  refs/tags/${LIVEKIT_TAG} | cut -f1)"
PROTOCOL_SHA="$(git ls-remote https://github.com/livekit/protocol.git \
  refs/tags/${PROTOCOL_TAG} | cut -f1)"
echo "Pinned at livekit ${LIVEKIT_TAG} (${LIVEKIT_SHA})"
echo "Pinned at protocol ${PROTOCOL_TAG} (${PROTOCOL_SHA})"
# Manually edit services/sfu/UPSTREAM.md to record these.

# Single dedicated commit — DO NOT bundle other changes:
git add services/sfu/upstream services/sfu/UPSTREAM.md
git commit -m "chore(sfu): initial upstream vendor at livekit ${LIVEKIT_TAG}"
```

## 2. Adding a patch (3b1b / 3b1c / 3b1d work)

```bash
# Start from a clean upstream snapshot — apply existing
# patches first so any new patch lands on top of the
# already-modified tree.
just sfu-apply-patches  # see justfile section below

# Make your changes inside services/sfu/upstream/.
# Add `// LAKKO: <reason>` comments wherever you modify
# upstream code (CI greps for these).

# Generate the patch with a sequenced filename + descriptive name.
NEXT=$(printf "%03d" $(($(ls services/sfu/lakko-patches/*.patch 2>/dev/null | wc -l) + 1)))
git -C services/sfu/upstream/livekit diff > /tmp/lakko.patch
mv /tmp/lakko.patch services/sfu/lakko-patches/${NEXT}-<short-description>.patch

# Revert the changes in upstream/ (patches are the source
# of truth; upstream/ stays at the pinned-clean state).
git checkout services/sfu/upstream/livekit

# Commit the patch file only.
git add services/sfu/lakko-patches/${NEXT}-<short-description>.patch
git commit -m "fix(sfu): <patch description> [3bXY]"
```

## 3. Quarterly upstream rebase

```bash
# 3a. Stash anything in upstream/ (there shouldn't be
#     anything — patches live in lakko-patches/).
git status services/sfu/upstream

# 3b. Pull the new upstream pin.
NEW_LIVEKIT_TAG="v1.X.Y"  # latest stable
rm -rf services/sfu/upstream/livekit
git clone --depth 1 --branch "${NEW_LIVEKIT_TAG}" \
  https://github.com/livekit/livekit.git services/sfu/upstream/livekit
rm -rf services/sfu/upstream/livekit/.git

# 3c. Try to re-apply every patch in order.
PATCH_DIR=services/sfu/lakko-patches
for p in $(ls ${PATCH_DIR}/*.patch | sort); do
  echo "applying ${p}"
  if ! git apply --directory=services/sfu/upstream/livekit "${p}"; then
    echo "FAILED: ${p}"
    echo "Resolve manually:"
    echo "  1. cd services/sfu/upstream/livekit"
    echo "  2. apply ${p} with --3way or by hand"
    echo "  3. regenerate ${p} via:"
    echo "     git -C services/sfu/upstream/livekit diff > ${p}"
    echo "  4. revert services/sfu/upstream/livekit"
    echo "  5. re-run this script"
    exit 1
  fi
done

# 3d. Verify build cleanliness from upstream's own test
#     suite. We don't ship LiveKit's tests — but they need
#     to pass on the rebased tree to confirm no regression
#     introduced by our patches.
(cd services/sfu/upstream/livekit && go test ./... -short)

# 3e. Revert upstream/ back to the patch-free state so
#     git only records the new vendor + the UPSTREAM.md
#     bump.
git checkout services/sfu/upstream

# 3f. Edit UPSTREAM.md — bump the pin + append a rebase-log
#     entry.

# 3g. Commit.
git add services/sfu/upstream services/sfu/UPSTREAM.md
git commit -m "chore(sfu): quarterly rebase to livekit ${NEW_LIVEKIT_TAG}"
```

## 4. Adding a Lakko-only Go file (lakko-additions/)

For code that doesn't belong inline in upstream — auth-JWT
verifier middleware, Lakko-specific integration tests,
Prometheus metric labels, etc.

```bash
# Just write the file under services/sfu/lakko-additions/.
# No patch needed — these files survive the rebase
# untouched.
mkdir -p services/sfu/lakko-additions/auth
$EDITOR services/sfu/lakko-additions/auth/jwt_verifier.go

git add services/sfu/lakko-additions/auth/jwt_verifier.go
git commit -m "feat(sfu): add Lakko JWT verifier [3b1c]"
```

The build system (Go modules + a `services/sfu/lakko-additions/go.mod`
once we have ≥ 1 Go file) treats these as a separate
module that depends on `./upstream/livekit/...`. That
boundary keeps the patch surface and the addition surface
clean from each other.

## 5. `justfile` recipes

The root `justfile` exposes the common operations. The
recipes themselves are added in 3b1b once we have actual
patches to test against:

```text
just sfu-apply-patches   # apply every lakko-patches/*.patch
                         # to upstream/livekit/. Used in dev
                         # before building.
just sfu-rebuild-patches # regenerate every patch from the
                         # current upstream/livekit state.
                         # Used when a manual edit needs
                         # capturing.
just sfu-rebase          # quarterly rebase (§3 above).
just sfu-verify-clean    # assert upstream/ matches the pin
                         # + every patch applies cleanly.
                         # CI gate.
```

These are stubs until 3b1b lands the first patch.

## 6. CI gates

The CI side enforces:

1. Every commit that touches `services/sfu/upstream/` must
   also touch `services/sfu/UPSTREAM.md` (the pin must
   move). Exception: the initial vendor commit (§1) — a
   `.gitattributes`-level marker exempts the first commit
   that creates the directory.
2. `just sfu-verify-clean` passes — every patch applies on
   top of the pinned upstream cleanly.
3. Every file under `services/sfu/upstream/` whose Lakko
   diff exists carries a `LAKKO:` marker comment.
4. The `cargo deny` / `reuse lint` / `clippy` gates from
   the root `justfile` are Rust-side only and do not run
   against the SFU. Go-side linting (`go vet`, `golangci-lint`)
   is added in 3b1b once there's Go code to lint.

## 7. Troubleshooting

### "I edited a file under `upstream/` directly and now I'm confused"

```bash
# Save your change as a patch, then revert.
git -C services/sfu/upstream/livekit diff > /tmp/save.patch
git checkout services/sfu/upstream/livekit

# Manually move /tmp/save.patch into services/sfu/lakko-patches/
# with a sequenced filename + descriptive suffix per §2.
```

### "The rebase script failed mid-way"

The patches are independent — apply them one by one, fix
the failing one by hand (use `git apply --3way` for a
3-way merge), regenerate that patch, then resume.

### "I want to run the SFU locally"

That's a 3b1b+ task — once the patches land, build with
`go build ./...` from `services/sfu/upstream/livekit/cmd/server`.
Until then there's no operational SFU binary to run.
