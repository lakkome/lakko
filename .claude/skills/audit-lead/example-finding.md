# Worked examples

Three of them: a finding as it should be filed, an excerpt of a workstream note, and
a candidate that died in verification. They exist because the format documents
*fields* and the fields do not convey *depth* — the difference between an audit
finding and an opinion is entirely in how the mechanism is traced, and that can only
be shown.

---

## A note on where this example comes from, and what you must not copy from it

**The defect below is real and it is already fixed.** It is the author-impersonation
the project found in its own `lakko-security` review on 2026-08-29 and closed; it is
indexed as **C-68** and described in ADR-0045. It is reproduced here as a *format*
example because a reader can check it against the repository's own history, which a
fabricated example could never offer.

Two consequences for how you read it:

1. **It is not a live finding.** Never file it. If you find yourself writing it, you
   have found the documentation of the fix, not the defect.
2. **Its citations do not resolve at today's commit**, and are marked so. **In a real
   finding this would be disqualifying.** Every citation you write must resolve at
   the engagement's frozen SHA, because `audit-verify` will open every one of them.

What to copy is the *shape*: the precondition stated as ordinary behaviour, the
mechanism walked one step at a time with the code that carries each step, the impact
bounded in both directions, and a recommendation that names the rule rather than the
patch.

---

## Example 1 — a filed finding

```markdown
<!-- SPDX-License-Identifier: AGPL-3.0-or-later -->
<!-- SPDX-FileCopyrightText: 2026 Lakko Contributors -->

# LAK-04 — A new member inherits a departed member's identity and can rewrite their history

- **Severity:** High
- **Confidence:** CONFIRMED
- **Workstream:** clients
- **Tracking id:** LAK-CL-02
- **Status:** Open
- **Component:** the author gate on all three clients — web `apps/web/src/chat/…`,
  iOS `apps/ios/Lakko/Chat/…`, Android `.../chat/…`
  *(illustrative: real citations carry file and line at the frozen SHA)*
- **Invariants touched:** #4; ADR-0023's author-only edit/delete gate
- **Vendor claim contradicted:** ADR-0023 — *"a member cannot rewrite or erase
  another's message"*

## Preconditions

None that an attacker must arrange. One member is removed from a group and another
is added — the ordinary sequence when somebody leaves a team and somebody else
joins it. The new member needs no special role, no modified client, no cooperation
from the server, and no timing.

This is worth stating first and in these words, because the finding reads at a glance
like a race and it is not one. It is the intended behaviour of two features meeting.

## Mechanism

1. The author gate decides who may edit or delete a message by comparing the
   message's stored author against the sender of the edit. Both sides of that
   comparison are an **MLS leaf index**.

2. A leaf index is a *position in the ratchet tree*, not an identity. RFC 9420 lets
   an implementation reuse a vacated leaf, and OpenMLS does: the next member added
   is placed in the lowest free leaf, which is the one the removed member just left.

3. So after `remove(Bob)` and `add(Dave)`, Dave occupies Bob's former leaf. Every
   message Bob sent is stored against that number, and the gate now reads it as
   Dave's.

4. ADR-0045 is what turns this from possible into easy. Before it, the two
   membership changes were two commits and an observer could plausibly land between
   them. Under proposal-based membership they compose into **one** commit, arriving
   in a single poll batch — so there is no window in which anything looks wrong.

5. The damage differs by platform, which is itself informative:
   - **iOS and Android** persist the author with the message, so the
     misattribution survives a restart and is **permanent**.
   - **Web** does not persist the author at all, so it is session-scoped. That is
     a deferral narrowing the window, not a defence, and it should not be read as
     one — the web client is not correct here, it is less wrong by accident.

6. The same shape appears twice more against the same root cause:
   - Reactions are toggled on the `(reactor, emoji)` pair, so Dave un-reacts on
     Bob's behalf; and because removing the last reactor prunes the set, the chip
     **disappears** rather than merely decrementing.
   - The read-receipt watermark map is never reset on a membership change, so Dave
     inherits Bob's watermark and the UI reports messages as read by someone who
     never saw them.

## Reproduction

1. Group with Alice (owner), Bob, Carol. Bob sends a message, Carol reacts.
2. Alice removes Bob and adds Dave in one commit.
3. Dave's client applies the batch and renders the history.
4. Dave edits Bob's message. The gate permits it on every client.
5. On iOS and Android, restart: the edit is still attributed to Bob.

Runnable here as a core-level test driving two membership changes in one commit and
asserting the resolved author of a pre-removal message. Not runnable end to end
without three devices.

## Impact

A member who joins after somebody leaves can rewrite, delete and un-react that
person's entire history in the group, permanently on two of three platforms, with no
indication to anyone that it happened. The content of the group is no longer
attributable, which is the property the gate exists to provide.

**Bounded honestly:** this does not break confidentiality — Dave reads only what any
member reads — and does not forge new messages from Bob, since sending is still MLS
-authenticated. It is an integrity and attribution break over *stored* history, not
an impersonation on the wire.

## Recommendation

Do not fix the comparison; fix what is compared. Resolve the sender to its **Ed25519
signature key at receipt** — in the epoch the message arrived in — and store that.

The second half is load-bearing and easy to lose: a batch carrying both the removal
and the add would resolve an epoch-N message against the N+1 roster, which *is* the
impersonation again under a comparison that now looks correct. Resolution therefore
belongs inside the batch loop, one step after the receive call returns — never in a
view model and never hoisted out of the loop for speed.

Decide explicitly what happens to rows already stored under the old shape. Refusing
the gate for everyone, including the genuine author, is the safe direction and should
be chosen deliberately rather than fallen into.

## Notes

Severity is High rather than Critical because it is an integrity break over stored
history rather than a compromise of message confidentiality or of the identity
system. It is not reduced for having no attacker: the reachability is
*unconditional*, which under the rubric raises it, and the reviewer notes the
temptation to read it as a mere bug precisely because nobody has to do anything
clever.

Worth checking beyond this finding: every other place the codebase keys a durable
decision on a leaf index. The convention elsewhere is to key on the signature key;
this gate predates the convention and was never revisited when it formed.
```

---

## Example 2 — a workstream-note excerpt

What a section of `13-clients.md` looks like. Note that it records the *method*, the
*negative* results and the *not-reached*, none of which appear in any finding:

```markdown
### Discipline: AuthorIdentity

Read all three implementations in full.

| | web | iOS | Android |
|---|---|---|---|
| Rule, in the implementation's own terms | only the author may edit/delete | same | same |
| Keyed on | leaf index | leaf index | leaf index |
| Resolved when | at render | at receipt | at receipt |
| Persisted | no | yes, with the message | yes, with the message |
| Failure arm | permits (no author stored) | refuses | refuses |

The three agree on the rule and disagree on two of the five properties that make the
rule mean anything. Filed as LAK-CL-02.

**The divergence in the failure arm is a second issue and is not in that finding.**
Web permits the edit when no author is stored; both natives refuse. One of these is
fail-open on an authorisation decision. Filed separately as LAK-CL-03 because it
survives any fix to the keying.

**Checked and found sound:** the send path binds the author through MLS framing on
all three, so nothing here lets a member forge a *new* message as someone else. That
distinction bounds LAK-CL-02 and is why it is not Critical.

**Not reached:** the channel and call timelines carry their own attribution and were
not read. If the pattern here is a convention rather than an accident, they will
have it too — a follow-up should start there.
```

---

## Example 3 — a candidate that died, and why that is a deliverable

From the non-findings appendix. A reader uses this section to calibrate how much the
surviving findings are worth:

```markdown
### KILLED — LAK-IM-07: "the role table is read after the merge, so a self-promoting
commit authorises itself"

**Claimed:** the commit gate reads the role table from the group context, and a
commit that rewrites the table would therefore be judged against the table it wrote.

**Killed by:** kill attempt 1 (find the guard). The read is taken from the
**pre-merge** context, explicitly, with the reason in a comment two lines above. The
finder read the accessor and not its argument.

**Why this is worth recording rather than deleting:** the defect the finding
describes is real in a neighbouring design, and the thing that prevents it here is
one argument to one call. Nothing in the type system distinguishes pre-merge from
post-merge, so a future refactor can reintroduce it silently and no test names the
property. That observation moved to hardening observation H-03 — *bind the pre-merge
read into a type so the distinction cannot be lost in a refactor* — which is a
smaller and more useful recommendation than the finding would have been.
```
