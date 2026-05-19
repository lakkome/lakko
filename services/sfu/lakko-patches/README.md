<!-- SPDX-License-Identifier: AGPL-3.0-or-later -->
<!-- SPDX-FileCopyrightText: 2026 Lakko Contributors -->

# `lakko-patches/`

Patches Lakko applies on top of the pinned LiveKit upstream
(see [`../UPSTREAM.md`](../UPSTREAM.md)). The patches are
the **source of truth** for Lakko's modifications — the
`upstream/` directory itself stays at the patch-free pinned
state in `git`.

This decouples the upstream rebase from the Lakko modifications:
each quarter we re-pin upstream, then re-apply every patch in
this directory. A patch that fails to re-apply is the signal
that upstream changed something we touched; the maintainer
manually rerolls the failing patch.

## Filename convention

```
NNN-<sprint>-<short-description>.patch
```

- `NNN` — three-digit zero-padded sequence number. Patches
  apply in numeric order; later patches may depend on
  earlier ones.
- `<sprint>` — the ROADMAP sprint label that produced the
  patch, e.g. `3b1b` / `3b1c` / `3b1d`. Lets a reviewer
  trace any patch back to a ROADMAP entry.
- `<short-description>` — kebab-case noun phrase.

Examples (none of these exist yet — first patches land in
3b1b):

```
001-3b1b-strip-participant-key-flow.patch
002-3b1b-refuse-plaintext-media-tracks.patch
003-3b1c-validate-lakko-jwt.patch
004-3b1d-replace-room-create-rpc.patch
```

## Patch shape

Patches are unified diffs in the `git apply --directory`
shape — paths are relative to `services/sfu/upstream/livekit/`
(or `services/sfu/upstream/protocol/` if the patch touches
the protocol module).

The first line is a normal `From:` header. Required headers:

```
From: <commit author, name + email>
Date: <commit date>
Subject: <patch title — same as filename description>

<commit body — Lakko sprint label + rationale>

Signed-off-by: <committer>
---
 path/to/file.go | NN ++++++-------
 1 file changed, ...
```

The commit body MUST include a `LAKKO:` marker comment in
the modified Go code itself — CI greps for this so any
file under `upstream/` whose diff is non-trivial against
the pinned snapshot fails the CI gate unless it carries
the marker.

Generate from a tree with the patch already applied:

```bash
git -C services/sfu/upstream/livekit diff > services/sfu/lakko-patches/NNN-3bXX-desc.patch
```

## Re-applying

Manual:

```bash
for p in $(ls services/sfu/lakko-patches/*.patch | sort); do
  git apply --directory=services/sfu/upstream/livekit "${p}"
done
```

Or via the justfile (added in 3b1b):

```bash
just sfu-apply-patches
```

## Verification

CI runs `just sfu-verify-clean` which:

1. Resets `services/sfu/upstream/` to the pinned-vendor state.
2. Applies every patch in order.
3. Builds the SFU (`go build ./...`).
4. Runs the patch-side test suite (`lakko-additions/`).

A patch failing to apply or a post-apply build/test failure
blocks merge.

## Patches that need to die

If a patch's purpose becomes unnecessary (e.g. upstream
adopted our change or we found a cleaner approach in
`lakko-additions/`), delete the file + add a row to the
table below explaining when and why.

| Deleted patch | When | Why |
|---|---|---|
| (none yet) | | |
