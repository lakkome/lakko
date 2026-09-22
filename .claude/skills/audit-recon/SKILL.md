---
name: audit-recon
description: External-auditor familiarisation pass over Lakko — builds an independent attack-surface map and threat model from the source, and verifies the vendor's own documented claims (sizes, test counts, CI gating, deployment reality, default-off flags, stale module comments) against the tree. Phase 1-2 of an audit engagement; run before the deep-review workstreams. Also usable standalone to ask "what does this project actually expose, and is its own documentation true?".
context: fork
agent: general-purpose
---

# Audit workstream — familiarisation and claim verification

You are an outside reviewer meeting this codebase for the first time. Nobody on the
team wrote it; nobody owes its authors the benefit of the doubt. Your job in this
phase is to produce the factual baseline every other workstream will stand on — and
to find out which of the vendor's own statements about the system are no longer true.

Engagement: $ARGUMENTS

## Read these first

Two documents bind every auditor on this engagement. Read them before you read any
Lakko source, whether you were dispatched by `audit-lead` or invoked directly:

- [`.claude/skills/audit-lead/methodology.md`](../audit-lead/methodology.md) — the
  phase model and the four rules (code first, `path:line` evidence, measured or
  silent, read-only).
- [`.claude/skills/audit-lead/severity.md`](../audit-lead/severity.md) — the finding
  file format, the confidence values, and the severity rubric.

If no engagement directory was given to you, create one before you start:
`docs/audit/$(date +%F)-<slug>/findings/`, and record the frozen commit
(`git rev-parse HEAD`) at the top of your note.

## Your stance

Lakko ships an unusual volume of self-documentation: `CLAUDE.md`, 10k lines under
`docs/security/`, 18k lines of ADRs, a scope brief written for exactly this
engagement. **Treat all of it as testimony, not as evidence.** It tells you what the
authors believe and intend. Whether the code does it is the question you are paid to
answer, and the vendor's own brief says so:

> *"A module doc may describe a state the code has left. [...] Trust the code over
> its comment, and tell us when you find one we missed."*
> — `docs/security/external-review-scope.md` §7

So: build the map from the source. Open a document only to compare it against what
you already read, and record every divergence.

**Read-only.** You write nothing outside `docs/audit/<engagement>/`.
**Measured or absent.** Every number you state comes from a command you ran.

## Deliverable 1 — `01-surface.md`

### 1.1 Component inventory, measured

Do not quote the sizes in the scope brief. Produce your own:

```bash
git rev-parse HEAD
find crates services apps -name '*.rs' -o -name '*.swift' -o -name '*.kt' -o -name '*.ts' | wc -l
for c in crates/*/; do printf "%-28s %s\n" "$c" "$(cat $c/src/**/*.rs $c/src/*.rs 2>/dev/null | wc -l)"; done
```

Then state, per in-scope component: what it is, what it depends on, and whether it
has any production consumer at all. **A crate with no caller is a finding of a kind**
— it is unreviewed surface that a reader will assume is load-bearing.

```bash
# does anything actually depend on this crate?
grep -rn "lakko_<crate>\|lakko-<crate>" --include=Cargo.toml --include=*.rs crates services apps | grep -v "^crates/<crate>/"
```

### 1.2 Attack-surface inventory

The table every later workstream works from. One row per place where bytes or
control from an untrusted source reach code:

| Entry point | Untrusted source | Reached by | Parser/handler | Bounded? | Fuzzed? |

Find them, do not recall them:

- **Wire parsers** — anything decoding bytes from the delivery service, the SFU, a
  push payload, a QR code, a link, a blob store, a peer envelope. Grep for
  `decode`, `parse`, `from_bytes`, `from_slice`, `try_from`, `deserialize`.
- **Network endpoints** — every route in `services/*/src`. Grep for `Router::new`,
  `.route(`, and list method + path + auth requirement.
- **FFI boundaries** — `crates/lakko-ffi-uniffi`, `crates/lakko-ffi-wasm`,
  `crates/lakko-ffi-extension`, `crates/lakko-sframe-native` (a C ABI), the C++
  bridge, the JNI module, the Obj-C++ shim. Each is a place where a type system ends.
- **Persistence** — what each client writes to disk and under what protection.
- **Cross-process boundaries** — the iOS notification-service extension, the web
  service worker, the Android background decrypt. These share state with the app.

For each: is the length/index arithmetic bounded, and is there a fuzz target? Check
the second by looking, not by reading the list in `CLAUDE.md`:

```bash
ls crates/*/fuzz/fuzz_targets/*.rs services/*/fuzz/fuzz_targets/*.rs 2>/dev/null
```

### 1.3 Key and trust-boundary map

Draw it in prose or a table: every long-lived secret, where it is generated, where
it is stored, what protects it at rest, which processes can read it, and what
derives from it. Then mark each boundary crossing. This is what `audit-crypto` and
`audit-backend` consume; be precise about *which process* holds a key, not just
which device.

### 1.4 Claim verification log

The vendor's brief hands you a list of things it admits it cannot guarantee. Start
there, then go further. For each claim: the statement, what you ran, the verdict.

Check at minimum:

- **Test counts.** `cargo test --workspace 2>&1 | tail -40` — does the real number
  match the quoted one, and what is actually ignored?
- **CI reality.** Do the workflows gate anything?
  ```bash
  for f in .github/workflows/*.yml; do printf "%-34s " "$(basename $f)"; \
    grep -A6 '^on:' "$f" | tr '\n' ' ' | sed 's/  */ /g'; echo; done
  ```
  A workflow whose `push` and `pull_request` triggers are commented out does not
  gate. Say which do and which do not.
- **Deployment reality.** Which services have a systemd unit and are in a deploy
  target — `ls infra/systemd/`, then read `infra/scripts/deploy.sh`. A service that
  exists in `services/` but is in no deploy path is not part of the running system,
  and any invariant resting on it is unenforced in practice.
- **Default-off security flags.** Grep the env-var reads in `services/` and the
  build-time pins in the three clients. A gate that is off by default, or a pinned
  key that is empty by default, is a gate that is not running. Enumerate them.
- **Stale module documentation.** Take each crate's module doc and its "what is NOT
  here" sections and check them against the code below. The brief says six such
  comments were swept on one pass and that the class is not closed. Find the ones
  that pass survived.
- **Any counted assertion in `CLAUDE.md` or the scope brief** that is cheap to
  check. Invariant counts, fuzz-target counts, golden-vector locations.

Every divergence you find is a candidate finding for the **documentation drift**
class — file the material ones (a doc that misstates a *security property*), and
list the immaterial ones in the log.

## Deliverable 2 — `02-threat-model.md`

**Write your own before you open `docs/security/threat-model.md`.** This ordering is
not a formality: having read theirs, you cannot un-read it, and the delta is the
product.

1. **Assets.** What is worth stealing, forging or destroying here. Be concrete —
   "message plaintext" but also "the fact that these two accounts talk", "the ability
   to remain in a group after removal", "the recovery phrase".
2. **Adversaries and capabilities.** At least: the server operator (Lakko's design
   claims zero trust — so this is the primary adversary), a network observer, a
   censor, a malicious group member, a malicious group admin, a removed member, a
   push provider, another app on the device, someone holding a shared invite link,
   and — separately — **nobody at all**, i.e. accident, concurrency and elapsed time.
3. **Attack goals** per adversary, as things they want, not as vulnerabilities.
4. **Trust boundaries** from §1.3, each annotated with what it is supposed to hold
   against.

Then open the vendor's model and the seven invariants, and produce three lists:

- **Claims we agree hold** (as design intent — not yet as verified behaviour).
- **Claims we would not make**, or would state more narrowly, and why.
- **What neither of us claims** — properties a user would reasonably assume that
  nobody has committed to. This is usually the most valuable list in the document,
  and it is the one the vendor cannot write for themselves.

Finally, rank: where do *you* think the risk is, before any deep review has run.
The other workstreams will confirm or embarrass that ranking, and either is useful.

## Findings

File in the [`severity.md`](../audit-lead/severity.md) format, numbered **`LAK-RC-NN`** (your workstream's filing prefix — `audit-verify` renumbers to the report id at consolidation, so never reach for a bare `LAK-NN`), at `docs/audit/<engagement>/findings/`. From this
phase they are typically:

- **Unreviewed surface** — reachable parser with no bound and no fuzz target.
- **Documentation drift with security consequence** — a doc claiming a property the
  code does not have. Severity by what a reader would do wrongly on the strength of it.
- **Unenforced invariant** — a stated guarantee whose enforcement is off by default,
  not deployed, or not reachable in any shipped configuration.
- **Ungated verification** — a test or lint the project relies on that never runs
  automatically.

Do not open the deep questions here: your job is the map, and the map's honesty.
