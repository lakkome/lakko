<!-- SPDX-License-Identifier: AGPL-3.0-or-later -->
<!-- SPDX-FileCopyrightText: 2026 Lakko Contributors -->

# Upstream pin

This file records the exact upstream commit Lakko's
LiveKit fork is pinned to. Every change to the pin is a
commit that touches **this file** + the regenerated
`services/sfu/upstream/` tree.

## Current pin

**STATUS: NOT YET PINNED**

Phase 3b1a sprint shipped the directory scaffolding only.
The actual vendor pull is the next operational step (see
[`PLAYBOOK.md`](./PLAYBOOK.md) §1). On first vendor:

- Pin the **latest stable LiveKit release** at the time of
  3b1a kickoff. As of [ADR-0008](../../docs/adr/0008-livekit-fork-strategy.md),
  [`v1.7.4`](https://github.com/livekit/livekit/releases/tag/v1.7.4)
  is the most recent — re-check at vendor time.
- Pin `livekit/protocol` at the matching `go.mod` version
  the chosen `livekit/livekit` tag depends on.
- Commit the vendor in one large dedicated commit titled
  `chore(sfu): initial upstream vendor at livekit v1.7.4`
  (or whatever the chosen tag is).

After the initial vendor, fill in this table:

| Field | Value |
|---|---|
| `livekit/livekit` commit | `_______________` (40-char SHA) |
| `livekit/livekit` tag | `v_______` (release tag if any) |
| `livekit/protocol` commit | `_______________` |
| `livekit/protocol` tag | `v_______` |
| Pin date (UTC) | `YYYY-MM-DD` |
| Pinned by | `git config user.email` of the committer |
| Lakko sprint | `3b1a` |

## Rebase log

Append-only log of every pin bump. Most-recent at top.

Entry shape:

```text
### YYYY-MM-DD — bumped to <upstream commit> (was <previous>)

- Sprint: <3bXX>
- Rationale: <e.g. security fix in libwebrtc bump>
- Patch impact: <how many patches re-applied cleanly vs needed reroll>
- Drift summary: <new APIs deprecated, removed, etc.>
```

(empty — first pin not yet landed)

## Cadence

Per [ADR-0008](../../docs/adr/0008-livekit-fork-strategy.md):
**re-pin every ~3 months regardless of feature need**.
The longer we drift, the harder the patch reapply.

A calendar reminder lives on the maintainer's ops calendar
+ a `chore(sfu): quarterly rebase reminder` GitHub issue
template fires the first business day of each quarter.

## How to read this file as an auditor

If you're reviewing this fork:

1. Open `UPSTREAM.md` (here) — read the current pin.
2. Compare `services/sfu/upstream/livekit/` HEAD against
   that upstream SHA: `git -C services/sfu/upstream/livekit log -1`.
   If it doesn't match, the pin is stale relative to what's
   committed — that's an issue.
3. Look at `services/sfu/lakko-patches/` — every patch must
   land on the pinned upstream cleanly (CI verifies; see
   `ci/sfu-patches.yml`).
4. Cross-reference each patch against an open / closed
   ROADMAP sprint (3b1b, 3b1c, ...). Every patch ties to a
   sprint.

If steps 2–4 hold, the fork's posture is what this file
claims it is.
