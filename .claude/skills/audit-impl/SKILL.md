---
name: audit-impl
description: External-auditor implementation review of Lakko's Rust code — parser bounds and panic surfaces, integer and slice arithmetic, error paths and whether failures land open or closed, secret lifetime and logging, unsafe in the FFI crates, locking and read-modify-write races, durability ordering, and resource exhaustion. One workstream of an audit engagement; also usable standalone to review a Rust crate for implementation-level defects rather than design ones.
context: fork
agent: general-purpose
---

# Audit workstream — implementation review (Rust)

You are the implementation reviewer. `audit-crypto` asks whether the construction is
right and `audit-protocol` whether the state machine is right; you ask whether **this
code** does what either of them assumes, on every input and every path, including
the paths nobody meant to take.

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

Read the code, not the comment above it. A module doc that says a value is bounded
is a hypothesis; the bound is in the arithmetic or it is not there. The project's own
brief warns that module docs describe states the code has left, and that a sweep for
those found six and did not close the class.

The project's style rules (no `unwrap`, no `panic!`, `#![forbid(unsafe_code)]`,
clippy pedantic + nursery at `-D warnings`) are strong. **Strong rules move defects
rather than removing them** — into the places the rules do not reach: the FFI crates
that are exempt from `forbid(unsafe_code)`, the `?` that turns a distinct failure
into a generic one, the `if let Ok(..)` that swallows, the catch-all on a client's
receive loop. Look there.

**Read-only.** Nothing outside `docs/audit/<engagement>/`.
**`path:line` or it did not happen.**

## 1. Parsers and attacker-controlled bytes

Take the attack-surface inventory from `01-surface.md` — every parser reachable from
the delivery service, the SFU, a push payload, a QR code, a link, a blob store, a
peer envelope. For each:

- **Length before index.** Does every slice, split and array copy check its bound
  first? Find the arithmetic: `[..n]`, `[n..]`, `split_at`, `copy_from_slice`,
  `try_into`, `chunks_exact`, and any `as usize`.
- **Order of gates.** Which is checked first — version, domain, length? A parser that
  indexes a byte to read the version before knowing the payload is long enough is a
  panic on an empty input, and empty inputs arrive.
- **Arithmetic.** Every `+`, `-`, `*` on a value derived from input. Subtraction on
  lengths is the classic underflow; the project builds with overflow checks in debug
  and **wraps in release** unless configured otherwise — check
  `grep -n 'overflow-checks' Cargo.toml` and say which it is, because it changes
  what a bad length does in the shipped binary.
- **Allocation from input.** A length field that reaches `Vec::with_capacity` or a
  loop bound is a memory-exhaustion primitive unless capped before use.
- **Recursion and nesting** in anything that decodes structure.
- **Does it fail as a value or as a panic?** A panic across the FFI boundary is not a
  clean failure — trace what a panic does in the wasm build, in the UniFFI build, and
  in the C ABI of `lakko-sframe-native`.

## 2. Panic surfaces in practice

The rule bans `unwrap`/`expect` in production code. Verify it, and then find what
the rule does not cover:

```bash
grep -rn 'unwrap()\|expect(\|panic!\|unreachable!\|todo!\|unimplemented!' crates/*/src services/*/src --include=*.rs | grep -v '#\[cfg(test)\]' | head -60
grep -rn 'unwrap()' crates/*/src --include=*.rs -B3 | grep -n 'mod tests' | head
```

Then the implicit panics the grep misses: indexing `[i]`, `slice[a..b]`, integer
division, `array::try_into().unwrap()` written as `.into()`, `RwLock`/`Mutex`
poisoning, `assert!` in a non-test path, and `unwrap` inside a closure in a `map`.

## 3. Error paths and the direction of failure

This is the highest-value section. Invariant #4 says no fallback to non-E2EE, and
several places deliberately fail **open** instead, each with an argument.

- Enumerate every deliberate fail-open in scope, with `path:line` and the argued
  reason. Then judge them **as a set**: can a chain of individually-reasonable
  fail-opens compose into a path where something reaches a user unprotected, or an
  unverified artefact is accepted? The vendor's own sixth question asks for exactly
  this and has not done it.
- Find the *accidental* fail-opens: a `let _ =`, a `.ok()`, a `if let Ok(x)` with no
  `else`, a catch-all `catch`/`rescue` on a receive loop, a `?` that maps a
  security-relevant error into a generic one that a caller treats as benign.
- **Where a security decision is made, what is the default arm?** Read each `match`
  and `if` in a verification path and ask what happens on the branch nobody wrote.
- Error *messages*: does a refusal leak which check failed in a way that helps an
  attacker enumerate, or leak a secret into a log or a returned string?

## 4. Secret lifetime

- Every secret: where it is created, every copy made of it, and whether each copy is
  zeroised. Check `Zeroize`/`ZeroizeOnDrop` usage and find the types that hold key
  material without it.
- Secrets in `String`, `Vec<u8>`, `format!`, `Debug`, `Serialize`, or a JSON body.
  Does any type derive `Debug` over a field holding key material?
  ```bash
  grep -rn 'derive(.*Debug' crates/lakko-crypto/src crates/lakko-mls/src crates/lakko-attest/src --include=*.rs | head -30
  ```
- **Logging.** The project logs via `tracing`. Check every `debug!`/`info!`/`warn!`/
  `error!` in a crypto or auth path for a secret, a token, a pseudonym, a username,
  or a full envelope in an argument or a `?field`.
- Secrets that cross the FFI boundary: once handed to Swift, Kotlin or JS they leave
  Rust's control entirely. List what crosses and what the receiving side does with it.

## 5. `unsafe`, and the crates exempt from the ban

`lakko-ffi-uniffi`, `lakko-ffi-wasm`, `lakko-sframe-native` (a C ABI with raw
pointers and manual ownership transfer), plus the C++ bridge, the JNI module and the
Obj-C++ shim.

```bash
grep -rn 'unsafe' crates/*/src --include=*.rs | head -40
```

For each `unsafe` block: is there a `// SAFETY:` comment, does it state a condition,
and **is that condition actually enforced by the caller**? A SAFETY comment that
restates the operation is not a justification.

For the C ABI specifically: every returned pointer's ownership, every `_free`
counterpart, double-free and use-after-free on the error paths, null handling on
every parameter, and what a panic unwinding into C does.

## 6. Concurrency, locking and durability ordering

Lakko has a documented class of defects here (a read-modify-write that was not
serialised, a lock that coalesced where it had to queue, a lock held across network
I/O). Look for more of the same shape:

- Every read-modify-write on shared or persisted state: is the whole span under one
  lock, and is the lock **serialising** rather than coalescing or de-duplicating?
- A decision taken against a snapshot read before a wait. A value captured as a
  parameter is evaluated *before* the await, not after — find the ones that matter.
- Locks held across network or disk I/O, and what a timeout does to the lock.
- Cross-process boundaries a lock cannot span: the iOS extension, the web service
  worker, the Android background decrypt, two browser tabs on one origin.
- **Durability ordering.** Where a property depends on A being persisted before B is
  published (or the reverse), find what enforces it. Then find the crash window
  between them and say what state it leaves. Two stores with no shared transaction
  is a torn write waiting to happen; ask what reconciles it at restart.

## 7. Resource exhaustion and bounds

Every queue, cache, retry, buffer, and per-peer map: what bounds it, who can grow
it, and what happens at the bound — reject, evict, or grow. An eviction policy on a
security-relevant set (a replay cache, a dedup set, a pending set) is an attack
surface: causing an eviction is causing a bypass.

## 8. Dependency and build hygiene

Observational only — the measured advisory run belongs to `audit-dynamic`:

- Vendored forks under `crates/vendor/`: what is the delta from upstream, is it
  frozen by a test, and does the fork still track a maintained upstream?
- Build-time configuration that changes a security property: feature flags, `cfg`
  gates, `debug_assertions`-only behaviour, test-only hooks that could arm in a
  release build. Grep for `debug_assertions` and for fixed-seed or override env vars.

## Findings

[`severity.md`](../audit-lead/severity.md) format, numbered **`LAK-IM-NN`** (your workstream's filing prefix — `audit-verify` renumbers to the report id at consolidation, so never reach for a bare `LAK-NN`), under `docs/audit/<engagement>/findings/`. Then write
`12-implementation.md`: the parser table with per-parser verdicts, the complete
fail-open inventory with your judgement on the set, the `unsafe` review, and what
you did not reach.
