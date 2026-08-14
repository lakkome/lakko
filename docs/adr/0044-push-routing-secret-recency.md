<!--
SPDX-License-Identifier: AGPL-3.0-or-later
SPDX-FileCopyrightText: 2026 Lakko Contributors
-->

# ADR-0044: Mint-epoch recency ordering for the push routing secret — close the stale-replay rotation undo (G-36 residual 9)

- **Status:** **Proposed.** Built end-to-end — **`936d849`** (core + FFI + web
  reference) and **`014e84e`** (iOS + Android) — and the flag day is over, with no
  client left on the v1 gate. **But it is NOT accepted, because it does not yet
  achieve what it was written to achieve.** The `lakko-security` review of the
  landed arc returned **REQUEST CHANGES** on a blocking **HIGH** and **reproduced
  the defect by execution** on web: two envelopes driven concurrently the way
  `Chat.tsx` actually drives them yielded verdicts
  `['accept-admin-rotate', 'accept-identical']` and a final stored secret of
  `0xEE` **at epoch 10** — the **stale** one. **F-A does not close as shipped.**
  The ordering primitive is correct and the gate is armed, but the receive path is
  an **unserialized read-modify-write dispatched fire-and-forget per envelope**,
  so the gate is handed a stale snapshot: it is **bypassed, not defeated** (see
  § The blocking defect, and **G-36 residual 10**). A per-`(account, group)`
  serializing lock spanning the read and the write is in flight on all three
  clients; this ADR flips to **Accepted** only once that lands and is verified.
  S1-S6 of the sprint decomposition landed; S7 is this docs reconcile. This ADR
  **extends**
  [ADR-0042](0042-push-opaque-group-routing-id.md) **and**
  [ADR-0043](0043-push-routing-secret-rotation-on-removal.md) — it supersedes
  neither. The `push_gid` derivation and its golden, the 29-byte domain
  separator, the E2EE MLS application-message carrier, the defer-never-downgrade
  posture, the owner-only mint rule, the authority gate's rank
  (owner > admin > member), and the byte-identical `services/push-gw` all carry
  through verbatim. It revisits exactly one property both ADRs ratified
  **implicitly, by omission**: that the receive gate has no recency notion at
  all.
- **Date:** 2026-08-12 (proposed) · 2026-08-14 (arc built; review returned
  REQUEST CHANGES, status held at Proposed)
- **Context:** Phase 3 — the push subsystem. **Intends** to close **G-36 residual
  9** (`lakko-security` **F-A**, raised in the ADR-0043 pre-merge re-review) and
  the concurrent-rotator core of **G-36 residual 7** (`lakko-security` **F2**).
  **Neither closes as shipped** — both are blocked on the same unserialized
  receive path (**residual 10**). What the arc *did* close outright is **G-36
  residual 6**: C-61 now has a regression test on all three clients.

**Anchor convention.** Line numbers throughout the § Context recon and the
§ References are as of **`820230d`** — the commit this ADR was written against,
*before* the arc landed — except for
`crates/lakko-contacts/src/push_routing.rs`, whose anchors are the S1 working
tree that became `936d849`. **They are deliberately not re-based to HEAD**: they
document the state the decision was taken against. Symbol names are the durable
reference — ADR-0043's own corrections showed these files' lines drift by
hundreds within one arc, and this arc moved every one of them again.

## Context

ADR-0042 gave each group a 32-byte `push_routing_secret`, distributed in-group
over an MLS application message, and routed every push key-space site under
`push_gid = keyed-BLAKE3(secret, "lakko/push/group-routing-id/v1")`. ADR-0043
made that secret **rotate on a member removal**, so an evicted member's routing
id goes inert. Both installed an **authority** gate on the receive path: a pure
decision function per client that resolves the MLS sender leaf against the
ADR-0022 role table and decides whether that sender may write the group's
secret.

Neither ever installed an **ordering** gate. That omission is this ADR's subject.

### The finding — a stale re-broadcast silently and permanently undoes a rotation

`lakko-security` **F-A**, recorded as **G-36 residual 9**. It is the one residual
on that list whose consequence is **confidentiality rather than wake delivery**,
and it needs **no malice and no crash** — only a failed broadcast followed by
somebody else's rotation:

1. An owner's or admin's post-add `redistributeSecretToGroup` **broadcast
   fails**. The failure path marks the row pending (web
   `group-routing.ts:870` `markGroupRoutingPending`; iOS `broadcastAndSettle`
   `PushGroupRouting.swift:733`; Android `redistribute`
   `GroupPushRouting.kt:695`), so that device now holds the **pre-rotation**
   secret `S_old` in an un-settled row.
2. Meanwhile another admin removes a member and rotates the group to `S1`
   (ADR-0043, ratified option (b) — any owner *or* admin may rotate).
3. The first device runs its provisioning pass **before** it applies the `S1`
   envelope. The `pending` branch — the **any-holder** re-broadcast added in
   `7e7d143` (web `ensureGroupPushRouting` `:766-804`; iOS `ensureProvisioned`
   `:813`; Android `ensure` `:553`) — re-sends **`S_old`**.
4. Every member's gate sees an authorised sender and accepts (`accept-owner` /
   `accept-admin-rotate`), persists `S_old`, **and retires `gid(S1)`** via the
   receive-side teardown (web `:632-634`, iOS `:470-475`, Android `:513-515`).
   The group migrates **back** onto the routing id the evicted member still
   derives and is still subscribed to.
5. No further membership shrink occurs, so **no further rotation fires**. The
   state is **permanent, not transient**.

### The root cause, stated plainly

**The receive gate has no recency or monotonicity notion at all.** Every input to
every verdict is an authority question. Read the three decision cores side by
side and the omission is structural, not an oversight in one client:

| Client | Decision core | Inputs |
|---|---|---|
| Web | `routingSecretVerdict` (`apps/web/src/push/group-routing.ts:468-519`) | `ownerKey`, `adminKeys`, `senderKey`, `storedSecret`, `storedUnreadable`, `incomingSecret`, `verifyFailed` |
| iOS | `classifyIncomingSecret` (`apps/ios/Lakko/Push/PushGroupRouting.swift:575-618`) | `incoming`, `stored`, `storedUnreadable`, `senderKey`, `authority` |
| Android | `decideAcceptReceivedSecret` (`apps/android/.../push/GroupPushRouting.kt:350-381`) | `senderSignatureKey`, `owner`, `incomingSecret`, `storedSecret` |

Not one of them can ask *is this newer than what I hold*. The stored and incoming
secrets are compared **only for byte-equality**, on the fast path that short-
circuits authority (`group-routing.ts:494-499`, `PushGroupRouting.swift:585`,
`GroupPushRouting.kt:359-361`). So a **retry** of the current secret and a
**replay** of a superseded one are byte-indistinguishable to the gate. The
any-holder re-broadcast branch and rotation-on-removal are each individually
sound; they interact badly precisely because no ordering predicate exists to
separate the two.

### The same omission produces residual 7

**G-36 residual 7** (`lakko-security` **F2**, MEDIUM): concurrent rotators can
strand a device on a superseded secret. Under ADR-0043 option (b) one removal
produces **N+1 rotations** in a group with an owner and N admins — the committer
rotates from its own remove path while every other owner/admin observes the same
shrink through its poll diff and rotates too. All N+1 secrets are equally
authorised and mutually incomparable, so which one a device ends on depends on
arrival order, and devices partition.

That is the same missing predicate wearing a different hat. A fix for F-A that
does not also order the N+1 same-epoch mints would be half a fix.

### What exists today — recon, verified firsthand

- **`push_gid` never depends on anything but the secret bytes.**
  `derive_push_gid` (`push_routing.rs:345`) is keyed-BLAKE3 over the 32-byte
  secret with a fixed domain message, golden-pinned (`tests::derive_push_gid_golden`).
  Adding a field to the envelope therefore cannot disturb the routing id.
- **The 29-byte domain separator is a dispatch prefix, and is frozen.**
  `crate::incoming_envelope` classifies by byte-prefix match
  (`incoming_envelope.rs:172`), so changing those bytes would make a peer fall
  through to the chat parser and render the envelope as garbage text. The
  in-payload version byte is the knob that moves.
- **A v1 → v2 bump has an in-crate precedent.**
  `group_join_attribution.rs:62-64` — `const VERSION: u8 = 2;`, "Bumped to 2 when
  the joiner self-signature landed" — refused its unsigned predecessor outright
  rather than accepting it alongside.
- **Every re-broadcast path recomposes the envelope from the stored secret at
  send time.** There are six such sites, two per client, and all six take only
  the secret: web `broadcastAndMarkDistributed` (`:657`, compose at `:664`) and
  `redistributeSecretToGroup` (`:856`, compose at `:867`); iOS
  `broadcastAndSettle` (`:721`, compose at `:729`) and `redistribute`; Android
  `broadcastAndMark` (`:624`, compose at `:630`) and `redistribute` (`:687`,
  compose at `:693`).
- **All three secret stores already carry exactly one extra field beside the
  secret**, so the encoding precedent for a second one is in place: web
  `SecretRow { key, ciphertext, distributed }`
  (`group-routing-store.ts:119`, `DB_VERSION = 1` at `:103`, both stores written
  in one transaction at `:263-264`); iOS `PushRoutingRecord { secret, distributed }`
  (`PushRoutingStore.swift:91-100`) encoded **inside** the sealed plaintext with
  a flag byte (`:109-111`) and a legacy-length fallback; Android
  `Entry(secret, distributed)` (`PushRoutingSecretStore.kt:123`) encoded as
  `base64(secret) ‖ separator ‖ flag` (`formatValue`, `:301-302`).
- **The group epoch is already on both FFI surfaces.** `Group::epoch()`
  (`crates/lakko-mls/src/group.rs:807`) is exposed as
  `crates/lakko-ffi-uniffi/src/lib.rs:813` and
  `crates/lakko-ffi-wasm/src/mls.rs:674`. No new core surface is needed to source
  a stamp.
- **But the rotation entry points do not hold a group.** Only web's
  `rotatePushRoutingSecret` takes one (`group: WasmGroup`,
  `group-routing.ts:1116`). iOS `rotateSecret(accountIdB64:groupIdHex:ownRole:broadcast:)`
  (`:1064`) and Android `rotate(accountIdB64, groupIdHex, ownRole, ports)`
  (`:953`) take pure values. Android's `receiveGated` (`:490`) and `ensure`
  (`:553`) likewise take a pre-resolved `OwnerLookup` / `isOwner` rather than a
  `Group`. Threading an epoch is therefore a **signature change on the native
  clients**, following their existing pure-value shape, not a re-architecture.

## The load-bearing design decision

Four points carry this ADR. Each is stated because each is a place where an
obvious-looking simplification is wrong.

### 1. The tiebreak is not a corner case — it is the common path

Under ADR-0043 option (b), one member removal produces **N+1 mints at the same
epoch**: the committing device rotates directly from its remove path, and every
other owner/admin rotates on observing the same shrink in its poll diff. Absent
an intervening commit, all N+1 stamps are identical.

So an epoch alone yields N+1 **incomparable** values, and the gate would be back
to arrival-order-dependent — which is exactly residual 7. **Epoch alone would not
close F-A either**, since the stale `S_old` and a same-epoch rotation would tie.
The tiebreak is load-bearing machinery, not a defensive afterthought.

### 2. Why the *secret bytes* are the tiebreak, and why nothing sender-derived works

The tiebreak must be a function of the **value**, not of the messenger:

- **On a re-broadcast the sender is the re-broadcaster, not the minter.** A
  tiebreak on leaf index or role rank would give the *same bytes* a *different*
  position in the order depending on who relayed them. That breaks the
  `accept-identical` fast path (the same value would compare differently on
  successive arrivals), breaks convergence, and would make the any-holder retry
  branch — the whole repair mechanism of `7e7d143` — a source of churn.
- **A minter identity carried inside the envelope is self-asserted.** The
  envelope body is attacker-chosen; the only MLS-authenticated attribution
  available is the sender leaf of the **current** message, which is the relayer.
  And even given an honest minter id, a receiver cannot re-derive that minter's
  role *at the mint epoch* — the role table it can read is the current one.

The secret bytes have none of these problems. They are available to every
receiver by construction (they *are* the value being ordered), stable across any
number of relays, tie-free (32 CSPRNG bytes), and convergent in either arrival
order. Grinding them buys at most one same-epoch tie, and only an owner or admin
can play at all, since a plain member never clears the authority gate.

### 3. The mint epoch must be PERSISTED and re-emitted verbatim — a store change, not only a wire change

This is the point most likely to be lost in implementation, and losing it voids
the entire fix.

All six re-broadcast sites (§ Context) recompose the envelope from the stored
secret at send time. If any one of them stamps the **current** epoch at that
moment instead of replaying the epoch the secret was **minted** under, then the
stale envelope arrives carrying a *newer* stamp than the live rotation, wins the
comparison, and F-A is reproduced with the ordering gate fully in place and
working as specified.

So the epoch is not a transient wire field. It is durable per-`(account, group)`
state that travels with the secret for that secret's whole life, and every store
must carry it — which is why this is a schema change on all three clients, not
just a codec change. The core states the rule at the composer
(`push_routing.rs:358-362`): "`mint_epoch` is stamped at mint time and then
travels with the secret forever — a re-broadcast MUST replay the epoch the secret
was minted under, never a fresh one."

**As built, Android makes this rule impossible to break BY ACCIDENT** — a
deliberate strengthening past the web reference, recorded here because it is the
shape the other two clients should converge on if this rule is ever revisited.
`composeEnvelope` does not take a `ULong`; it takes a sealed `MintStamp`
(`GroupPushRouting.kt`):

```kotlin
public sealed class MintStamp(public val epoch: ULong) {
    public class Fresh internal constructor(epoch: ULong) : MintStamp(epoch)
    public class Replayed internal constructor(epoch: ULong) : MintStamp(epoch)
}
```

Both constructors are `internal`, and each subtype has exactly one producer in
`src/main`: `Fresh` only from `PushRoutingPorts.freshMintStamp()` (the port that
reads the live `Group::epoch()`), `Replayed` only from
`StoredSecret.replayStamp`.

**The strength of this must be stated precisely, because the obvious reading
overstates it.** Kotlin's `internal` is **module**-scoped, and
`GroupPushRouting.kt`, `ChatRepository.kt` and `ChatClient.kt` all live in the
same `:app` module — so `MintStamp.Fresh(someEpoch)` **is** constructible from
any of them, and `StoredSecret`'s own constructor is fully **public**, so
`StoredSecret(secret, freshEpoch).replayStamp` yields a `Replayed` carrying an
arbitrary epoch. The sealed type therefore does **not** make a wrong stamp
unrepresentable, and this ADR should not be read as claiming it does.

What it genuinely buys — real, and the reason it is worth the ceremony — is that
it **eliminates the silent failure mode**. A bare `ULong` can no longer be passed
where a stamp is expected: handing `composeEnvelope` the result of
`group.epoch()` at a re-broadcast site is a **compile error**, not a subtly wrong
program. Getting the rule wrong now requires writing a differently-named call
(`MintStamp.Fresh(...)`, or a hand-built `StoredSecret` with a fresh epoch) — a
visible, deliberate act that a reviewer will see, rather than the accidental slip
this section warns about. **"Cannot be done by accident", not "cannot be done".**

The same commit turned `getSecret` into `getRecord` so the secret and its stamp
are read through one store call and cannot tear against a concurrent receive; web
reads the pair through a single `GroupRoutingRecord` get and iOS through
`getRecord` for the same reason.

### 4. Clamp the future, and clamp it before authority

An ordering rule with no upper bound hands an authorised writer a new and
strictly worse capability: stamp `u64::MAX` and nothing can ever exceed it, so
the group's routing id is **frozen permanently** — no later owner rotation can
win. That would escalate **G-36 residual 3** from "a malicious admin can churn
the routing id" (which self-repairs at the owner's next rotation) to "a malicious
admin can freeze it forever" (which does not). Trading a recoverable denial for
an unrecoverable one is not an acceptable price for closing F-A.

The clamp is sound because an honest stamp can never exceed the receiver's own
current epoch:

```
stamp  <=  seal epoch  <=  receiver's epoch at dispatch
```

The minter stamps `group.epoch()` at mint time and the group's epoch only
advances afterwards, so `stamp <= seal`. The receiver applies commits up to and
including the one carrying the message before dispatching its plaintext, so
`seal <= receiver's epoch`. A re-broadcast widens the gap (an old stamp on a
freshly sealed envelope) but never inverts it. So the clamp **never rejects an
honest envelope**, and it is evaluated **before** authority precisely so that not
even the owner can freeze the group.

### 5. `accept-identical` must not move the stored stamp — a real defect, found during implementation

This point was **not** in the ADR as first written. It was uncovered by the web
verdict tests while S4 was being built, and it is recorded here as a discovered
defect rather than a designed property because that is what it was.

`accept-identical` is the fast path for a benign retry: byte-identical incoming
secret, accepted from **anyone**, before any authority is consulted and — since it
sits at rule 1 — before the § 4 clamp too. Both short-circuits are correct in
themselves. The first cut then did the obvious thing on the persist step and
re-filed the row with the **incoming** epoch.

That combination is an **epoch-inflation channel that reaches the exact freeze the
clamp exists to prevent, by going around it**. A **plain member** — who can never
clear the authority gate, and whose envelope would be clamped if it carried a
future stamp — relays the group's **current** secret bytes stamped `u64::MAX`.
Rule 1 matches on the bytes alone, so neither gate runs; the stored stamp becomes
`u64::MAX`; and every subsequent rotation, **the owner's included**, now loses the
recency comparison forever. Residual 3 escalates from admin-can-churn all the way
past admin-can-freeze to *any-member*-can-freeze.

The fix is one line per client and the reasoning is what makes it non-obvious: on
the identical path the stored `(secret, mint_epoch)` pair is left **untouched**,
because the bytes are by definition unchanged and the stamp is the one property a
retry has no authority to move.

- web — `verdict === "accept-identical" ? storedMintEpoch : mintEpoch`
- iOS — `verdict == .acceptIdentical ? (record?.mintEpoch ?? 0) : mintEpoch`
- Android — `when (verdict) { ACCEPT_IDENTICAL -> stored?.mintEpoch ?: mintEpoch; else -> mintEpoch }`

Each carries a named regression test (web
`accept-identical cannot INFLATE the stored epoch (a plain member freeze)`, iOS
`testAcceptIdenticalCannotInflateTheStoredEpoch` driving a real three-member
group, Android
`` `accept-identical cannot INFLATE the stored epoch (a plain-member freeze)` ``),
each of which relays identical bytes at the `u64` maximum from a plain member,
asserts the stored stamp is unmoved, **and** then proves a later owner rotation
still lands — the second half being the part that actually distinguishes a fix
from a mask.

The general lesson generalises past this ADR: **a rule that short-circuits a gate
must not write any state that gate protects.** Rule 1 skips two gates, so rule 1
may write nothing but what it matched on — which is nothing.

## Decision

**Carry a `mint_epoch` in the distribution envelope and order envelopes by the
pair `(mint_epoch, secret_bytes)`, refusing any envelope that is not strictly
greater than what the device holds.**

### Wire format — v2, a hard flag day

```text
  plaintext_payload =
      PUSH_ROUTING_SECRET_ENVELOPE_DOMAIN_V1 || version || mint_epoch || secret
     └────────── 29 bytes, ASCII ───────────┘ └─ 0x02 ─┘ └─ 8 B, BE ─┘ └─ 32 B ─┘
```

70 bytes total (was 62). The **domain separator is byte-identical** — those 29
bytes are the dispatch prefix and are frozen forever
(`push_routing.rs:889` `domain_separator_bytes_survived_the_v2_bump` pins it).
The `_V1` in the constant's name denotes the envelope **family**, not the schema
version; the in-payload version byte is the knob that moves.

**No dual-accept.** `0x02` is the only accepted value; a retired v1 envelope is
refused with `UnknownVersion` (`push_routing.rs:434`,
`tests::decode_rejects_v1_envelope_as_unknown_version`). Accepting v1 alongside
v2 would admit envelopes that carry no `mint_epoch`, hence have no position in
the order — re-opening F-A for exactly the envelopes a stale device would send.
The decoder deliberately checks **version before length** (`:434` before `:437`)
so a v1 envelope reports the actual problem rather than a
`BadLength { expected: 41, got: 33 }` that reads like truncation. The mirror case
is asymmetric and does not need to be otherwise: an already-shipped v1 decoder
meeting a v2 envelope checks length first and reports `BadLength`. Both are a
fail-closed drop.

### The order lives in the core, once

`push_routing_secret_is_newer(incoming_epoch, incoming_secret, stored_epoch,
stored_secret) -> bool` (`push_routing.rs:313-331`), with the
`PushRoutingSecretEnvelope::supersedes` convenience wrapper (`:263`). Epoch
first; on an exact tie the 32 secret bytes lexicographically; **strictly greater
wins, equal is not newer**.

Three clients each re-deriving a comparison is precisely the drift class that
produced F-A, so the order is defined here and every platform routes its decision
through it via the FFI rather than reimplementing it. Two properties are pinned
deliberately:

- **Equal is not newer**, which is what keeps a benign retry from looking like an
  update and makes the `accept-identical` fast path safe.
- **Wrong-length secrets are fail-closed to `false`** (`:319-323`) — a malformed
  incoming secret must never displace a stored one, and a corrupt stored secret
  is better left in place than overwritten on a comparison against a value that
  was never a secret. Never panics.

### The core owns the ORDER; the client owns the CLAMP

A deliberate split, and worth stating because the core's own doc makes the
generic choice: `mint_epoch` is **opaque to `lakko-contacts`**
(`push_routing.rs:241-250`). The crate never reads it as a wall-clock time, never
bounds it, and never feeds it to `derive_push_gid`. It requires only that one
group's mints be non-decreasing over time, and names that as the caller's
obligation.

The **clients** discharge that obligation by sourcing the stamp from the MLS
group epoch (`Group::epoch()`), which is what makes the § 4 clamp checkable at
all: an MLS epoch is shared state every receiver can independently observe, so a
receiver can bound an incoming stamp against its own. A free-running counter
could not be bounded by anything.

**Stated as the load-bearing split it is:** the core deliberately does **not
interpret** `mint_epoch` — it owns the **ORDER**, and nothing else. The clamp is
not missing from `lakko-contacts` by oversight; it is impossible there, because a
crate that never learns what the number *means* has nothing to bound it against.
Sourcing the stamp from `Group::epoch()` is therefore a **client-side binding**,
and it is the whole reason the reject-future rule is checkable at all. Two
consequences fall out of that split and both are visible in the landed code: the
core's test matrix can only ever pin order properties (strict-total-order,
lexicographic-wire agreement, wrong-length fail-closed), while every clamp test
lives on a client; and a client that got the binding wrong — stamping a
free-running counter, say — would still typecheck against the FFI and still order
correctly, failing only the clamp. The obligation is named at the field's doc
comment on `PushRoutingSecretEnvelope::mint_epoch` for exactly that reason.

As built, every client reads the epoch **after** its serializing lock, never
before: web `readGroupEpoch(group)` inside the `run` IIFE past the `await
previous` queue wait; iOS a `@MainActor () -> UInt64?` **provider closure**
(`rotateSecret(accountIdB64:groupIdHex:ownRole:mintEpoch:broadcast:)`) invoked
inside `performRotation` past `await predecessor?.value`; Android a **suspending
port** (`PushRoutingPorts.freshMintStamp()`) called inside `mutex.withLock`.
Neither native holds a `Group`, and on both a plain value parameter would
necessarily have been evaluated *before* the wait — so a rotation queued behind a
predecessor would stamp the epoch it was *triggered* at rather than the epoch it
actually *mints* at, which is a stale stamp by construction. Android pins this
directly (`a QUEUED rotation stamps the epoch it MINTS at, not the one it was
triggered at`).

### The accept rule, in this order

1. **Incoming secret bytes == stored** → **accept-identical**. No authority, no
   ordering. **Unchanged**, and this is what preserves retry semantics: the
   any-holder re-broadcast, the post-add redistribute, and any relay stay
   idempotent. It matches on the **secret bytes** and leaves the stored
   `(secret, mint_epoch)` **untouched** — deliberately, since updating the stored
   epoch here would be an epoch-inflation channel reachable without clearing the
   authority gate **or the clamp**. See § 5: this was a live defect in the first
   cut, not a precaution.
2. **`mint_epoch` > the receiver's own current group epoch** → **reject-future**,
   fail-closed. Evaluated **before** authority (§ 4).
3. **The existing ADR-0042 / ADR-0043 authority gate** — owner / admin /
   no-role-table / unverifiable — **unchanged**, including the rank, the
   owner-evaluated-first branch, the fail-closed unreadable-role-table and
   unresolvable-sender refusals, and the legacy no-role-table acceptance.
4. **No stored secret** → accept (the seed path).
5. **`(incoming_epoch, incoming_secret) > (stored_epoch, stored_secret)`** →
   accept, persist, and retire the old gid; otherwise **reject-stale** — no
   persist, no subscribe, no teardown.

**As built — the verdict surfaces are not identically shaped, and that is worth
stating rather than glossing.** Both natives widened their gate from a binary
accept/reject to *named* verdicts, and the reason is structural rather than
diagnostic: the persist step must file the **stored** epoch on the identical path
and the **incoming** epoch everywhere else (§ 5), so a binary verdict would force
the orchestration to re-derive "was it identical?" by comparing bytes a second
time — a duplicate of the gate's own rule 1, free to drift from it, which is
precisely the drift class this ADR exists to close. The persist decision now falls
out of the gate's answer instead of being recomputed beside it. Web
(`RoutingSecretVerdict`, 9 members) and Android (`ReceiveVerdict`, 9 members) name
every authority outcome individually; **iOS deliberately does not** — `SecretVerdict`
has 5 cases (`accept`, `acceptIdentical`, `reject`, `rejectFuture`, `rejectStale`),
collapsing the authority accepts into one `accept` and the authority refusals into
one `reject`, with the distinctions surviving as `diagnosticTag` strings. One
further divergence: an **unreadable own epoch** maps to `reject-unverifiable` on
web and Android but to `.rejectFuture` on iOS. Both are fail-closed refusals with
identical effect (drop, no persist, no subscribe); only the label differs, so no
behavioural gap follows — but a reviewer comparing the three cores side by side
should expect the asymmetry rather than read it as a bug.

### The semantic change, stated plainly

**A total order and unconditional owner-priority cannot both hold.**

Today the gate promises the owner wins regardless of arrival order — its branch
is evaluated before anything is read about what is stored
(`group-routing.ts:507-508`, `PushGroupRouting.swift:605-606`,
`GroupPushRouting.kt:368-370`). Under the total order the owner wins whenever it
mints at a later epoch, which is the normal case; but **at an exact epoch tie the
bytes decide and the owner can lose to an admin**.

That is deliberate. The property owner-priority existed to provide —
arrival-order independence — is preserved by a **strictly stronger** mechanism: a
total order is arrival-order independent by construction, for every participant,
not merely for the owner's envelope. What is lost is only that the owner's
*particular bytes* win a tie, which was never the point; both tied values are
authorised post-removal mints, equally unknown to the evicted member.

The clean framing: **authority decides WHO may write; the order decides WHICH
write survives.** The gate still runs first and still decides authorisation
alone; the order arbitrates only between envelopes that already cleared it.

Recorded here as a **deliberate narrowing of a previously stated property**, in
the same way ADR-0043 recorded Android's removed strip-admins-on-unreadable-store
rule rather than letting it pass as an implementation detail.

## Alternatives

- **A plain monotone counter, no epoch anchor.** Rejected on two counts, the
  second decisive. First, it does not avoid the tiebreak: two rotators after one
  removal both read `stored` and both pick `stored + 1`, so it ties in exactly
  the same place the epoch does and needs the same secret-bytes tiebreak anyway.
  Second, and fatally, **it has no independent anchor**. A counter is whatever
  the sender says it is, so § 4's clamp has nothing to check against and the
  permanent-freeze capability stands. The MLS epoch is shared state every
  receiver observes for itself.

- **Sender leaf, or owner-beats-admin, as the tiebreak.** Rejected — breaks under
  relay (§ 2). The sender of a re-broadcast is not the minter, so the value's
  position in the order would change every time it is forwarded.

- **Dual-accept: keep taking v1 alongside v2.** Rejected. A v1 envelope carries no
  `mint_epoch` and therefore has no position in the order at all. Whatever
  default were assigned to it, an attacker or a stale device would simply send
  v1 — so the fix would be optional for exactly the party it constrains. The
  `group_join_attribution` v1 → v2 bump set the precedent of refusing the
  predecessor.

- **Use the MLS seal epoch instead of a carried mint epoch.** Rejected, and this
  is the alternative that looks free, so the reason is worth being concrete
  about. With OpenMLS `max_past_epochs` defaulting to **0**
  (`openmls-0.8.1/src/group/mls_group/config.rs:50-52`, "The default is 0"; never
  overridden anywhere in `crates/lakko-mls/src/`), a receiver's epoch at decrypt
  time **is** the seal epoch — so it appears the wire already carries a free,
  unforgeable recency stamp. It does not work, because the seal epoch measures
  when an envelope was **sent**, not when its secret was **minted**, and F-A is
  precisely a case where those diverge: the stale re-broadcast is composed
  **fresh**, at the current post-rotation epoch, so its seal epoch is **newer**
  than the rotation's. Ordering by seal epoch would rank the stale envelope above
  the live one — worse than no ordering at all. Only a **persisted** mint epoch
  discriminates.

- **Do nothing.** Rejected. F-A is permanent on trigger, needs neither malice nor
  a crash, and silently revokes the ADR-0043 property for the affected group with
  no user-visible signal.

## Consequences

- **Positive — subject to the blocking defect below, which currently voids the
  first two claims in practice.** The *intended* closure is: the stale `S_old` was
  minted at some epoch `E_old` no later than the pre-removal epoch; the rotation
  to `S1` is broadcast strictly after the Remove commit (C-61), so it is stamped
  at `E_rot >= E_remove > E_old`, and the re-broadcast of `S_old` is refused as
  stale by every member holding `S1`. **F2 / residual 7** was to follow: the N+1
  same-epoch rotations from one removal are totally ordered, so every device
  converges on the single maximum instead of partitioning by arrival order.
  **Neither holds yet** — see § The blocking defect. What **does** hold today,
  independently of it: the order is defined **once**, in the audited core, rather
  than three times; the v2 codec is bounded, fail-closed and fuzzed; the future
  clamp exists; and **C-61 gained its first regression test on any client**,
  closing **G-36 residual 6** outright.

### The blocking defect — the gate is bypassed, not defeated

`lakko-security` returned **REQUEST CHANGES** on the landed arc and **reproduced
the defect by execution** on web: two envelopes driven concurrently the way
`Chat.tsx` actually drives them produced verdicts
`['accept-admin-rotate', 'accept-identical']` and a final stored secret of `0xEE`
**at epoch 10** — the stale one. Tracked in full as **G-36 residual 10**
(**HIGH**, open).

**The primitive is not at fault.** `push_routing_secret_is_newer` is a correct
strict total order, the clamp is correctly placed ahead of authority, and the
gate's rule sequence is as specified. The defect is one layer up: the gate is a
**read-modify-write** — read the stored `(secret, mint_epoch)`, decide, persist —
with **suspension points between the read and the write**, and **no client
serializes it per `(account, group)`**. Web dispatches it `void`-prefixed and
fire-and-forget from a batch loop that never awaits; Android launches it per
envelope on `pushScope` (`inflight` guards `ensure`, `rotatingLocks` guards
`rotate` — neither covers receive); iOS spawns a detached `Task`, and `@MainActor`
serializes execution but **not across `await`**. So two routing envelopes in one
poll batch both decide against the **same pre-batch snapshot**, and the survivor
is decided by scheduling.

**`accept-identical` is the vehicle, and precisely why is worth stating.** That
rule deliberately short-circuits **both** the future clamp and the total order —
correct behaviour for a benign retry, and § 5 correctly stopped it from moving the
stored epoch. But **nothing stops it being *reached* on a stale snapshot.** In the
exact journal order F-A describes — a device holding `S_OLD@10` polling
`[rotation S1@11, stale S_OLD@10]` — gate A accepts the rotation and persists
`(S1, 11)`, while gate B, reading the same obsolete `(S_OLD, 10)`, finds its
incoming bytes byte-identical to it, resolves `accept-identical`, and rewrites the
superseded secret one turn behind A. The § 5 hardening does not help: B files the
*stored* epoch it read, which is the stale `10`. The whole verdict was computed
against a snapshot that was already gone.

The same cause undercuts residual 7: the N+1 same-epoch rotations from one removal
typically arrive in **one batch**, so all N+1 gates compare against one snapshot
and the winner is scheduling order. **Stated exactly: the total order's
arrival-order independence holds for the pure function, but not for the path that
calls it.** It becomes a property of the device's resulting state only once the
gate that consumes it is serialized.

**Fix in flight** — a per-`(account, group)` serializing lock spanning the read
and the write, on all three clients. Each already has the machinery (Android's
`ConcurrentHashMap<String, Mutex>` keyed on `lockKey(accountIdB64, groupIdHex)`,
web's per-group promise chain, iOS's serializing predecessor task). Taking the
**same** lock the rotate path takes would additionally close the rotate-vs-receive
interleaving of **residual 5 / F-E**.

- **Negative / new risks.** A **`crates/lakko-contacts` wire-format change** — a
  hard flag day with no dual-accept — plus a **store schema change on all three
  clients** and a signature change on the native rotation and receive entry
  points. The owner can lose an exact epoch tie (§ semantic change). The fix's
  correctness now **depends on C-61**, which was untested when this ADR was
  written and **is now test-pinned on all three clients** (below). A stamp sourced
  from the group epoch means the gate reads `Group::epoch()` on a hot receive
  path. As built, the store change landed **read-legacy** on every client rather
  than as a migration: an unstamped pre-v2 row reads at **epoch 0**, the floor of
  the order, so any stamped mint supersedes it — web `row.mintEpoch ?? 0n`, iOS
  `decodeRecord`'s two legacy shapes (bare 32-byte, and `flag(1) ‖ secret(32)`),
  Android `LEGACY_MINT_EPOCH = 0uL` for both its 1-part and 2-part rows. **Web did
  NOT bump `DB_VERSION`** (it is still `1`, contrary to this ADR's own S4 plan):
  the `secrets` object store is `keyPath`-only with no indexes and `public/sw.js`
  opens it version-less, so an added optional field needs no upgrade transaction —
  and bumping it would have made the service worker's version-less open race the
  page's upgrade. Android additionally round-trips `ULong.MAX_VALUE` through its
  store on both sides, since the JVM has no unsigned primitive and a `toLong()`
  shortcut is exactly what a freeze attempt would exploit.

- **Neutral.** `services/push-gw` stays **byte-identical unchanged** — it never
  saw the envelope, only the derived 32-byte `push_gid`. `derive_push_gid` and its
  golden vector are **untouched**: the epoch never enters the derivation
  (`tests::derive_push_gid_ignores_the_mint_epoch`), so C-60 sub-invariant 1
  (algebraic independence from the `group_id`) and the routing id's stability
  across ordinary commits both hold exactly as before. The ADR-0041 / C-59
  background decrypt path is untouched — its `push_gid → (account, group_id)`
  reverse map is keyed on the derived id, which does not change shape. No
  `lakko-mls` change. The 29-byte dispatch prefix is unchanged, so no sibling
  envelope kind is disturbed.

### What this does NOT fix

- **F-D (the stale-role window) is untouched and orthogonal.** A demoted
  rotator's envelope is refused on **authority**, not on recency: it queued behind
  a predecessor on the serializing lock with a role captured before the wait, so
  peers evaluate it against the current role table and reject it. Recency has no
  bearing on that path. Still open, still availability-only, still self-healing at
  the next authorised envelope (**G-36 residual 7**, `lakko-security` **F-D**).

- **It does not repair an already-stale holder.** The order rule stops a stale
  value from **spreading**; it does not push the live value to the device holding
  the stale one. That device keeps `S_old`, keeps routing under a gid nobody
  notifies, and — because its own re-broadcast **succeeded on the wire** even
  though every recipient refused it — marks itself distributed and stops
  retrying. It recovers when an authorised newer envelope reaches it, which in
  the normal case is the `S1` envelope still sitting in the journal ahead of its
  cursor; it does not recover if it has already polled past that position.

  The narrow complementary fix — **clear or skip the `pending` re-broadcast once a
  DIFFERENT secret has been accepted** — is **complementary, not superseded**: it
  removes the trigger, this ADR removes the effect, and neither subsumes the
  other. **As built it turned out to be already structural on all three clients
  rather than something to add**, and it is pinned rather than rebuilt: every
  store writes the secret, the `distributed` flag and the stamp in **one atomic
  write**, so accepting a different secret replaces the bytes and clears the
  pending flag in the same operation. There is no window in which a device holds
  the new secret and a stale pending marker. A third option was considered and
  **not** taken: have a receiver that refuses a stale envelope reply with its own
  newer one, making the group self-heal. It is attractive but needs a storm guard
  (N members each answering one stale broadcast is an N-fold amplification on the
  journal), so it is left as an open question rather than folded in.

- **It inherited G-36 residual 6 and promoted it from advisable to mandatory —
  and, as built, CLOSED it.** The F-A closure rests on `E_rot > E_old`, which
  rests **entirely** on **C-61** — the rotation broadcast being emitted strictly
  after the Remove commit. When this ADR was written C-61 was enforced
  **structurally only**, by call-site sequencing, with **no test on any client**,
  contrary to ADR-0043's own "must be asserted in a test".

  Under ADR-0043 alone, a C-61 regression cost the rotation's forward secrecy: the
  envelope would be sealed at the pre-removal epoch and the evicted member could
  open it. Under this ADR it costs that **and** inverts the recency comparison —
  a rotation stamped at the pre-removal epoch `N` ties with an `S_old` minted at
  `N`, so the tie falls to the bytes and the rotation may simply **lose to the
  stale secret**. The fix degrades from "always wins" to "coin flip", silently.
  That is why the **C-61 regression test on each client landed in this change**
  rather than being deferred as a nice-to-have: this ADR converts C-61 from a
  property that protects one guarantee into a precondition of two.

  **What landed.** The ordering moved behind a single named seam so the two steps
  cannot be reordered independently — iOS
  `PushGroupRouting.publishRemoveThenRotate(publishRemove:rotate:onRotationStarted:)`,
  whose sole production call site is `ChatRepository.removeMemberFromGroup`; web
  and Android the equivalent. Each client then asserts on the **emitted
  envelope's stamp**, not merely on call order, and each carries a **permanent
  inverted-order companion** that runs the swap and asserts the consequence:
  web `the INVERTED order would stamp the pre-removal epoch and lose the
  guarantee`, iOS `testTheInvertedOrderStampsThePreRemovalEpochAndTheRotationLoses`,
  Android `` `C-61 - the INVERTED order stamps PRE-removal and the rotation LOSES` ``.
  On both natives the discrimination was **proven by running the swap**, not
  asserted: iOS temporarily added an inverted copy and recorded three failures;
  Android replaced the seam with rotate-then-advance and recorded the stamp
  assertion failing 11 vs 10. Both probes were reverted, leaving the permanent
  companions.

  **The fixture is load-bearing and its first cut was latently flaky.** A test
  that exists to prove the rotation *loses* under the swap must make it lose with
  certainty, and at an epoch tie the winner is the bytewise-greater secret. The
  web fixture landed in `936d849` filled the stale secret with `0xEE`, so a freshly
  minted **random** rotation secret sorts higher roughly once in fourteen runs and
  the rotation would have **won** — passing a test written to watch it lose. All
  three now use `0xFF`, the bytewise maximum, so the rotation loses
  deterministically.

- **Residual 4 is unchanged.** Legacy / no-role-table groups still get no push at
  all and still never rotate. Nothing here touches the mint gate.

- **Residual 1 is unchanged.** The co-membership cluster is still materialised at
  push-gw and at the providers; the epoch changes nothing about it. Closing it
  still needs per-device-blinded routing ids.

### Mixed-version fleets, and why the flag day is affordable now

An old client meeting a v2 envelope refuses it (`BadLength`); a new client meeting
a v1 envelope refuses it (`UnknownVersion`). Both refusals are **fail-closed to
defer** — the client does not subscribe at all rather than falling back to the
leaky raw `group_id` — so **invariant #4 holds throughout the transition** and no
member is ever downgraded.

There is no deployed-compatibility blocker: `services/push-gw` is byte-identical
and never parses the envelope; the envelope exists only in the DS journal, which
prunes at 30 days; and no production store ids exist yet (ADR-0038 records every
id as `me.lakko.dev`). The cost of the flag day is a dev-fleet window in which
mixed clients defer push, which is the same posture a member awaiting its first
envelope has always had.

**One transition-window tie class the design should name explicitly.** Every
client reads a legacy unstamped row at `mint_epoch = 0` — the floor, so any
stamped mint supersedes it, which is the right direction. But during the
transition **two DIFFERENT pre-v2 secrets both sit at 0**, so a
current-versus-superseded pair among them is decided by the **byte tiebreak
rather than by recency** — i.e. F-A once, at roughly even odds, for that pair. It
is bounded on both sides: it is confined to the transition window (only
pre-v2 rows read 0), and it **self-heals at the next rotation**, because a
rotation follows a Remove commit and is therefore always stamped strictly above
0. Accepted rather than fixed — the alternative would be a real migration
assigning distinct synthetic epochs to legacy rows, which cannot be done
consistently across devices that never agreed on an ordering in the first place,
so it would invent an order rather than recover one.

### Invariants touched

- **#2 (the server sees only ciphertext + public artefacts) — unchanged.** The
  `mint_epoch` rides inside the same E2EE MLS application-message plaintext; the
  DS sees ciphertext and push-gw sees only the derived 32-byte id. `push-gw` is
  not modified and needs no migration.
- **#3 (the MLS state machine is sacrosanct) — untouched.** No `crates/lakko-mls`
  edit. The clients read `Group::epoch()`, an accessor that already exists on both
  FFI surfaces.
- **#4 (no fallback to non-E2EE) — holds, and the fail-closed surface widens.**
  Both new refusals — **reject-future** and **reject-stale** — drop the envelope
  entirely rather than downgrading anything, and both v1/v2 cross-version
  refusals fail to defer. The codec stays bounded and exact-length.
- **#5 (the social graph is not materialised on the server) — unchanged posture.**
  Still **improved but not fully satisfied**. This ADR restores the ADR-0043
  forward-unlinkability property in the cases F-A was silently revoking it; it
  does not touch the cluster (residual 1).
- **Invariant-index / gap touchpoints (landed in the S7 docs reconcile).**
  **C-60** is amended for the v2 envelope, with the `derive_push_gid` stability
  property called out as explicitly **unchanged** (no epoch input — that is what
  lets a subscriber survive ordinary commits without re-subscribing); **C-61**
  records its now-live regression coverage on all three clients and its promotion
  to a precondition of the recency fix; a new **C-62** pins the recency ordering
  itself (the total order, the persist-and-replay-verbatim rule, the future
  clamp, and the rule that `accept-identical` never moves the stored stamp);
  **G-36 residual 6 CLOSES** (C-61 test-pinned on all three clients); **residual
  9 STAYS OPEN** and **residual 7 is NOT core-closed**, both blocked on the new
  **residual 10** (the unserialized receive path, HIGH); residuals 1 / 3 / 4 / 5 /
  8 are untouched. C-62 is minted with its closure claims explicitly conditioned
  on residual 10.

**`lakko-security` gate — required before merge.** This change touches a
`crates/lakko-contacts` wire format and the security gate that ADR-0042's
MEDIUM-1 finding installed. It narrows a previously stated property
(owner-priority at a tie), adds two new refusal paths on a hot receive path, and
its correctness depends on a sequencing rule (C-61) that was untested when this
ADR was written and is now test-pinned on all three clients. **A review of the
landed arc is in flight at the time of this reconcile; its findings are not yet
folded in here.**

## Open questions

- **Should a receiver that refuses a stale envelope answer with its own newer
  one?** It would make the group self-heal a stale holder rather than waiting for
  that device to poll forward. Needs a storm guard, since N members answering one
  stale broadcast is an N-fold journal amplification. Deferred, not rejected.
- **~~Should the mint epoch be surfaced in diagnostics?~~ ANSWERED IN CODE — yes.**
  All three clients log **both** epochs alongside the verdict tag, each with a
  comment citing this question. Defensible on the ADR-0042 discipline of never
  logging anything that narrows the secret: an MLS epoch is **public in the
  message framing pre-decrypt** (`epoch_of_message` reads it without any group
  secret, which is the whole basis of ADR-0028's join-ticket), so the pair reveals
  nothing an observer of the journal does not already have, while making a
  stale-replay visible in the field. Closed as answered rather than left listed.
- **~~Does `accept-identical` want to move behind the clamp?~~ ANSWERED, and the
  answer corrects this question's own premise.** It stays first, so a retry needs
  neither authority nor ordering. But the premise "it is inert either way, since
  it neither persists nor changes the stored epoch" was **false as first
  implemented** — it *did* re-persist the incoming epoch, which is precisely what
  made it an inflation channel around the clamp (§ 5). Rule 1 is therefore safe in
  front of the clamp **only because** it now writes nothing, and that is a
  standing constraint on the rule rather than an incidental property of it: any
  future change that gives `accept-identical` a side effect must first move it
  behind the clamp.
- **What sources the stamp for a mint in a group with no prior secret?** The mint
  path reads the current epoch like any other, so a group founded at epoch 0
  stamps 0. Harmless, but it means the very first envelope for a long-lived group
  and a rotation in a young group can share a stamp value across *different*
  groups — irrelevant, since the comparison is only ever within one
  `(account, group)` row, and worth confirming nobody builds a cross-group index
  on the epoch.

## Sprint decomposition

**All of S1-S6 landed** — `936d849` (S1-S4: core, fuzz + dispatch, FFI, web
reference) and `014e84e` (S5-S6: the iOS + Android mirrors and the C-61
regression tests on all three). S7 is this reconcile. The per-sprint text below
is kept as written, with landing notes where the implementation diverged from the
plan.

**Verification, as recorded firsthand on the two commits.** `936d849`:
`cargo test -p lakko-contacts` (214 passed), `cargo check --workspace
--all-targets`, `clippy -D warnings` on both FFI crates, `regen-bindings` + wasm +
`LakkoCore.xcframework` + the Android `.so` all rebuilt, `apps/web` `tsc --noEmit`
clean, `vitest` 548 passed (130 across the two push suites), `vite build` ok,
`just spec-check` OK. `014e84e`: `gradlew :app:compileDebugKotlin` BUILD
SUCCESSFUL, `GroupPushRoutingTest` 98 + `PushRoutingSecretStoreTest` 10 with zero
failures, `xcodebuild` BUILD SUCCEEDED, `LakkoTests` `PushGroupRoutingTests` 67
passing, `apps/web` vitest `group-routing` 110 passing. Two **pre-existing** red
gates are explicitly not caused by this arc and were left for their own change:
iOS `SfuHealthTest` / `SfuSignalingTest` need a dockerised SFU (318/318 pass
without them), and 7 Android failures in `ds/`, `storage/` and `calls/` where each
suite's own Ktor `MockEngine` handler raises `error("unexpected OutgoingContent")`
— Ktor's `defaultTransformers` now yields an anonymous subclass of the **abstract**
`OutgoingContent.ByteArrayContent` while those tests match the concrete
`io.ktor.http.content.ByteArrayContent`.

- **S1 — core.** `crates/lakko-contacts/src/push_routing.rs`: the v2 envelope
  (`PUSH_ROUTING_SECRET_VERSION_V2`, `mint_epoch(8, BE)`, 41-byte payload /
  70-byte envelope), `PushRoutingSecretEnvelope { mint_epoch, secret }`, the
  canonical `push_routing_secret_is_newer` order, version-before-length decoding,
  and the hard v1 refusal. `derive_push_gid` and its golden untouched.
  **Present in the working tree at the time of writing**, with the codec test
  matrix (v1-refusal, every-other-version, version-gate-before-length,
  epoch-extremes, big-endian encoding, domain-bytes-survived-the-bump, and the
  existing cross-kind isolation set) **and** the order's own matrix —
  `is_newer_is_a_strict_total_order` (irreflexive / antisymmetric / transitive),
  `is_newer_matches_lexicographic_wire_bytes` (the `u64` comparison agrees with a
  lexicographic compare of the raw `mint_epoch_be(8) ‖ secret(32)`),
  `is_newer_rejects_a_wrong_length_secret`, and
  `supersedes_matches_the_free_function`. **Landed in `936d849`** — 41 `#[test]`
  functions in `push_routing.rs`, including
  `a_stale_rebroadcast_never_undoes_a_rotation`,
  `the_owner_can_lose_an_exact_epoch_tie`,
  `convergence_is_arrival_order_independent`, and
  `derive_push_gid_ignores_the_mint_epoch`.
- **S2 — fuzz + dispatch.** Extend `push_routing_secret_decode` coverage over the
  wider payload and the v1-refusal path; thread `mint_epoch` through
  `IncomingEnvelope::PushRoutingSecret` (`incoming_envelope.rs:103`, dispatch
  `:172`) and its `incoming_envelope_dispatch` target.
- **S3 — FFI.** uniffi (`crates/lakko-ffi-uniffi/src/contacts.rs`: compose `:1488`
  gains the epoch, decode `:1508` returns the pair, the dispatch kind `:1318` /
  `:1371` carries it) + wasm (`crates/lakko-ffi-wasm/src/contacts.rs:1326` /
  `:1343` / `:1153` / `:1228`), plus `pushRoutingSecretIsNewer` exposed on both so
  no client reimplements the order. Regenerate bindings, rebuild wasm, rebuild
  `LakkoCore.xcframework`, rebuild the Android `.so` via cargo-ndk.
  `lakko-ffi-extension` is unaffected (it depends only on `lakko-mls` +
  `lakko-attachments`, so the NSE never sees this envelope).
- **S4 — web reference.** Store schema (`SecretRow` gains `mintEpoch`; bump
  `DB_VERSION`), all compose sites carrying the persisted epoch verbatim, the
  clamp + order rules in `routingSecretVerdict` with the two new verdicts, and
  `rotatePushRoutingSecret` stamping `group.epoch()` (it already holds the group).
  **Landed with two deviations from this plan.** (a) `DB_VERSION` was **not**
  bumped — `mintEpoch` is an optional field on a `keyPath`-only store with no
  indexes, and `public/sw.js` opens the database version-less, so an upgrade
  transaction is both unnecessary and a race (see § Consequences). A legacy row
  reads `row.mintEpoch ?? 0n`. (b) The two new verdicts landed exactly as
  planned: `RoutingSecretVerdict` went from the 7 members ADR-0042/0043 left it
  with to 9, adding only `reject-future` and `reject-stale`. This sprint is also
  where the § 5 `accept-identical` inflation channel was found.
- **S5 — iOS + Android mirrors.** The same, plus the signature work the natives
  need: an epoch parameter on `rotateSecret` / `rotate` (neither takes a `Group`)
  and on Android's `receiveGated` / `ensure` (which take pre-resolved pure
  values), sourced from `Group::epoch()` at the call site. Port S4's verdict
  matrix verbatim, as ADR-0037's `restoreVerdict` gate was. **Landed in
  `014e84e`, with the epoch threaded LAZILY rather than as the planned plain
  parameter** — an iOS provider closure and an Android suspending port, because a
  value parameter is necessarily evaluated *before* the serializing-lock wait and
  would stamp the trigger epoch instead of the mint epoch (§ the core owns the
  ORDER). Android went further than the plan and past the web reference with the
  sealed `MintStamp` (§ 3). The verdict matrix was ported but not verbatim: iOS
  kept a coarser 5-case enum (§ the accept rule).
- **S6 — the C-61 regression test on all three clients.** Not optional (§ What
  this does NOT fix). Assert that the rotation broadcast is emitted only after the
  Remove commit is published, on each client. **Landed** — six tests on web, five
  on iOS, six on Android, each including a permanent inverted-order companion, and
  on both natives the discrimination was proven by running the swap rather than
  asserted. The `0xEE` -> `0xFF` fixture fix belongs to this sprint.
- **S7 — docs reconcile.** Flip this ADR's status; update **C-60** and **C-61**;
  mint **C-62** for the recency ordering; close **G-36 residual 9**, **residual
  6**, and the concurrent-rotator core of **residual 7**; update the `CLAUDE.md`
  ADR table + the `lakko-contacts` module-map row + the fuzz-target description.

## References

- **ADR-0042** — opaque per-group push routing id. This ADR extends it: the
  derivation, the domain separator, the carrier, the defer posture, the
  owner-only mint, and the untouched push-gw all carry through. It revisits the
  receive gate's authority-only decision surface (`0042:386-394`, the verdict
  matrix).
- **ADR-0043** — rotation on member removal. Source of the N+1-rotations-per-
  removal shape that makes the same-epoch tiebreak the common path, and of
  **C-61**, on which this ADR's closure of F-A entirely depends
  (`0043:269-275`). Its pre-merge re-review raised **F-A** (`0043:892`) and
  recorded the fix direction this ADR takes (`0043:989-995`).
- **G-36 residual 9** (`docs/security/known-gaps.md:3250-3318`) — the full F-A
  mechanics, the per-client reachability differences, and the two recorded fix
  directions. **Residual 7** (`:3205-3241`) — the concurrent-rotator partition
  this ADR's order also closes. **Residual 6** (`:3187-3203`) — the untested C-61
  ordering.
- **C-60** / **C-61** (`docs/security/invariant-index.md:193`, `:199`) — the
  indexed properties this ADR refines rather than replaces.
- `crates/lakko-contacts/src/push_routing.rs` — the v2 envelope, the canonical
  order (`:313-331`), the caller's non-decreasing obligation (`:241-250`), the
  re-broadcast-must-replay rule (`:358-362`), version-before-length (`:434`).
- `crates/lakko-contacts/src/group_join_attribution.rs:62-64` — the in-crate
  v1 → v2 bump precedent that refused its predecessor rather than dual-accepting.
- `apps/web/src/push/group-routing.ts:468-519` ·
  `apps/ios/Lakko/Push/PushGroupRouting.swift:575-618` ·
  `apps/android/.../push/GroupPushRouting.kt:350-381` — the three authority-only
  decision cores this ADR gives a recency predicate.
- `crates/lakko-mls/src/group.rs:807` (`Group::epoch`), exposed at
  `crates/lakko-ffi-uniffi/src/lib.rs:813` and
  `crates/lakko-ffi-wasm/src/mls.rs:674` — the stamp source.
- `openmls-0.8.1/src/group/mls_group/config.rs:50-52` — `max_past_epochs`
  defaults to 0 and is never overridden in `crates/lakko-mls/`, the fact behind
  the rejected seal-epoch alternative.
- **RFC 9420** §12 — MLS forward secrecy, the property that makes the
  post-Remove seal unreadable to the evicted leaf and therefore makes
  `E_rot > E_old` meaningful.
