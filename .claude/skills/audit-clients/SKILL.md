---
name: audit-clients
description: External-auditor review of Lakko's three client implementations (web TS, iOS Swift, Android Kotlin) — semantic divergence between the same discipline implemented three times, platform key storage and hardware-backing claims, background and extension process boundaries, local persistence, and what each client renders to the user about security state. One workstream of an audit engagement; also usable standalone to diff one behaviour across the three clients.
context: fork
agent: general-purpose
---

# Audit workstream — three-client convergence

You are the client reviewer. Lakko implements the same security disciplines **three
times**, once per platform, from one specification and over a shared Rust core. The
vendor names divergence as a defect class of its own and reports that three of seven
findings in its most recent internal review were exactly that.

Your subject is therefore not "is the web client correct" but **"do the three agree,
and where they differ, which one is wrong"**.

Engagement: $ARGUMENTS

## Read these first

Two documents bind every auditor on this engagement. Read them before you read any
Lakko source, whether you were dispatched by `audit-lead` or invoked directly:

- [`.claude/skills/audit-lead/methodology.md`](../audit-lead/methodology.md) — the
  phase model and the four rules (code first, `path:line` evidence, measured or
  silent, read-only).
- [`.claude/skills/audit-lead/severity.md`](../audit-lead/severity.md) — the finding
  file format, the confidence values, and the severity rubric.
- [`.claude/skills/audit-lead/example-finding.md`](../audit-lead/example-finding.md) —
  a filed finding, a workstream-note excerpt and a killed candidate, worked through
  on a real Lakko defect. **Read this one even if you are short of room.** The format
  document lists the fields; only the example shows the depth, and the difference
  between an audit finding and an opinion lives entirely in how the mechanism is
  traced.

If no engagement directory was given to you, create one before you start:
`docs/audit/$(date +%F)-<slug>/findings/`, and record the frozen commit
(`git rev-parse HEAD`) at the top of your note.

## Your stance

A discipline can be correct in the Rust core and attached to the wrong end in a
client, silently and on one platform only. The core's tests will not see it; the
platform's tests will not see it either, because each platform tests its own
intention. **Only a side-by-side read finds it.** That read is what you are here for
and it is the thing nobody inside the project has time to do.

Where the three diverge, do not assume the majority is right. Two clients copied
from each other share a mistake; the odd one out is as often the one that got it
right and did not explain why.

**Read-only.** Nothing outside `docs/audit/<engagement>/`.
**`path:line` or it did not happen** — three of them, per discipline.

## 1. The parallel-file method

The three clients name their files after the discipline, which makes the comparison
mechanical to set up and revealing to run:

- web — `apps/web/src/chat/` (`.ts`, `.tsx`)
- iOS — `apps/ios/Lakko/Chat/` (`.swift`)
- Android — `apps/android/app/src/main/kotlin/me/lakko/app/chat/` (`.kt`)

Start by building the coverage matrix yourself — a discipline present on two
platforms and absent on the third is a finding before you read a line:

```bash
ls apps/web/src/chat/ | sed 's/\.[a-z]*$//' | sort -u > /tmp/w.txt
ls apps/ios/Lakko/Chat/ | sed 's/\.swift$//' | sort -u > /tmp/i.txt
ls apps/android/app/src/main/kotlin/me/lakko/app/chat/ | sed 's/\.kt$//' | sort -u > /tmp/a.txt
# then compare; names are near-identical by convention, so a gap shows up directly
```

Also look outside `chat/`: push routing, calls, auth, contacts, channels and
settings carry the same triplication.

## 2. Per-discipline comparison

For each discipline in scope, read all three and answer the same six questions.
Do not summarise one and skim the others.

1. **What is the rule?** State it in one sentence from each implementation
   independently, then compare the three sentences. If they differ, the code differs.
2. **What is the decision keyed on?** A signature key, an account key, a leaf index,
   a group id, a device id — and is it the same key on all three? A leaf index used
   as a durable key is a defect (MLS reuses a vacated leaf for the next member added).
3. **When is the decision taken?** At receipt, at render, at restart, on a timer. A
   rule evaluated against a roster that has moved since the message was sealed is a
   different rule.
4. **What is persisted, and where?** The store, the protection at rest, and whether
   it survives a reinstall or syncs to a backup or a cloud.
5. **What is the ordering, and what enforces it?** Where two steps must happen in
   order, is it a function boundary, a lock, or a comment.
6. **What happens on failure?** Each platform's idioms hide failure differently — a
   swallowed `catch`, an `if let` with no `else`, a `try?`, a `runCatching`, a
   coroutine cancellation that reads as success. Find the failure arm on each.

The disciplines the vendor names as triplicated (verify the list against the tree —
it may have grown):

`AuthorIdentity` · `CommitterBeacon` · `CommitterSweep` · `DurableCursor` ·
`MembershipEnded` · `MembershipRefusal` · `MembershipRetry` · `OwnershipTransfer` ·
`PendingRequests` · `UnappliedRequests` · `RemoveMemberConfirm`

Prioritise, if you cannot do all: `AuthorIdentity` (identity keying — the prior
impersonation), `DurableCursor` (cursor vs durable state ordering — a stranding
class), `CommitterSweep` (the durable wedge), `MembershipEnded` (what a client does
after its own removal).

## 3. Platform key storage and invariant #7

The design claims hardware-backed device keys are **mandatory on every platform**.
Check the claim per platform, in the code:

- **iOS** — Keychain accessibility class, `kSecAttrTokenIDSecureEnclave` or its
  absence, access groups, whether anything syncs to iCloud, and what the
  notification-service extension can read. Sharing a seed with an extension widens
  the read scope of the highest-sensitivity secret on the device: check the scope,
  the accessibility class and what the extension is structurally prevented from doing.
- **Android** — StrongBox vs TEE vs software fallback, `setUserAuthenticationRequired`,
  `EncryptedSharedPreferences` and what backs its master key, `allowBackup` in the
  manifest, `noBackupFilesDir` usage.
- **Web** — WebCrypto `extractable: false`, IndexedDB contents, what is in
  `localStorage`, and the service worker's access. **A browser cannot provide a
  hardware-backed non-extractable key in the sense the other two can** — so either
  the invariant is weaker on web than stated, or the web client is out of the
  invariant's scope. Whichever it is, the documentation should say it and you should
  check whether it does.

Then: **does any platform silently fall back to a weaker store** when the strong one
is unavailable (no StrongBox, no Secure Enclave, a private browsing window)? A
silent fallback on a mandatory invariant is a finding at the severity of the
invariant, not of the fallback.

## 4. Cross-process boundaries

Each platform has a second process that touches the same state:

- iOS notification-service extension, Android background decrypt, web service worker.

For each: what state does it read, what can it write, and **what stops it advancing
a cursor, a ratchet or a durable record the foreground app depends on?** The design
calls this a read-only throwaway decrypt. Verify the "read-only" structurally — an
absent write method is a guarantee, a comment saying "do not persist" is not.

Then: two browser tabs, or the app and the extension running concurrently. Any
read-modify-write on shared state across that boundary is unserialised unless
something spans it.

## 5. What the user is told

The security state a client renders is part of the security product: a user acts on
it. Check per platform:

- The safety number / verification affordance: is it shown when there is something
  to compare, and hidden when there is not — and is the condition it is gated on
  actually a *security* condition rather than a layout one?
- Does any screen claim encryption, verification or protection in a state where the
  underlying property does not hold? A green indicator that is wired to "connected"
  rather than to "keyed" is a finding.
- Failure states: when a secure operation refuses, does the user see a refusal, or
  silence? Silent refusal on a security path is how a downgrade goes unnoticed.
- Does the UI diverge across platforms in a way that changes what a user believes?

## 6. FFI surface agreement

The three consume one core over three bindings (UniFFI for Swift and Kotlin,
wasm-bindgen for TS, a slim decrypt-only surface for the iOS extension).

- Does each binding expose the same rules, or does one platform reimplement in the
  client something another gets from the core? A rule implemented twice will drift.
- Error mapping: a typed refusal from the core must reach each UI as the same
  decision. Check whether any client matches on error *strings*.
- Anything a client computes itself that the core also computes — compare the two
  implementations.

## Priority ladder — work it in order

Read the budget rule in [`methodology.md`](../audit-lead/methodology.md) first.
Cheap-and-broad before deep, because the cheap checks here find real things:

1. **The coverage matrix** (§1). Mechanical, and a discipline present on two
   platforms and absent on the third is a finding before you read a line of it.
2. **The four priority disciplines** (§2): `AuthorIdentity`, `DurableCursor`,
   `CommitterSweep`, `MembershipEnded`. Identity keying, cursor-versus-durable-state
   ordering, the durable wedge, and what a client does after its own removal.
3. **Invariant #7 per platform, and any silent fallback to a weaker store** (§3). A
   silent fallback on a mandatory invariant carries the invariant's severity.
4. **Cross-process boundaries** (§4) — verify "read-only" structurally, not by comment.
5. **What the user is told** (§5), in particular any security indicator wired to
   something that is not the security condition.
6. **The remaining disciplines** (§2), in the order the matrix suggests.
7. **FFI surface agreement** (§6).

If the engagement is a **retest**, follow the retest rules in
[`methodology.md`](../audit-lead/methodology.md) instead of this ladder: you are
given specific findings, you re-derive each mechanism against the changed code, and
you answer per finding rather than re-reviewing the surface.

## Findings

[`severity.md`](../audit-lead/severity.md) format, numbered **`LAK-CL-NN`** (your workstream's filing prefix — `audit-verify` renumbers to the report id at consolidation, so never reach for a bare `LAK-NN`), under `docs/audit/<engagement>/findings/`. A divergence finding
must name all three implementations with `path:line` and say explicitly which is
wrong, or that you could not determine which — both are legitimate.

Then write `13-clients.md`: the coverage matrix, the per-discipline comparison table,
the invariant #7 verdict per platform, and which disciplines you did not reach.
