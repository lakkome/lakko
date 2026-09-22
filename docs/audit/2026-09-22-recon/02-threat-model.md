<!-- SPDX-License-Identifier: AGPL-3.0-or-later -->
<!-- SPDX-FileCopyrightText: 2026 Lakko Contributors -->

# 02 — Independent threat model, and the delta against the vendor's

**Frozen commit:** `ab1a85e8e112dd656fadfc01904e518bdfebbb79`

Sections 1-4 were written **before** `docs/security/threat-model.md` was opened,
from the surface map in `01-surface.md` alone. Section 5 was written after. The
ordering is the deliverable: having read theirs, the independent model cannot be
recovered, so it had to come first.

---

## 1. Assets

Ordered by what an attacker would actually want, not by how the system is layered.

### 1.1 Content

- **Message plaintext**, live and historical. Historical matters separately: MLS
  forward secrecy means a key compromise at time *T* should not open messages from
  before *T*, and whether that survives the *client's* at-rest storage is a
  different question from whether it survives the protocol.
- **Attachment plaintext** — images, voice notes, video notes. Stored as blobs on
  a server, so the ciphertext is durable in a way the messages are not.
- **Call media.** Audio and video, live. Distinct from messages because the SFU
  holds every participant's stream simultaneously.

### 1.2 Metadata, which for a messenger is often the higher-value asset

- **Who talks to whom** — the social graph. Invariant #5 names it.
- **Who is in which group**, and the churn: who joined, who was removed, when.
- **When someone is active.** Push wakes, journal writes, committer beacons and
  typing indicators all emit a timestamped "this person had this app in front of
  them" signal.
- **Group size and message volume.** Derivable from a journal's row count.
- **Channel subscription** — which accounts follow which public channel.
- **GIF search terms.** The vendor states this one plainly in ADR-0047; it is a
  real asset and belongs on the list.

### 1.3 Identity and capability

- **The recovery phrase.** The root of identity; whoever holds it *is* the account
  for every purpose the protocol recognises.
- **The account identity key**, and therefore the safety number two users compare.
- **The binding between a username and a key.** The asset a key-transparency log
  exists to protect. An attacker who can substitute a key at first contact needs
  nothing else.
- **Continued membership of a group after removal.** Worth naming as an asset in
  its own right: it is what an ex-employee, an ex-partner or an infiltrator wants,
  and it is not the same as "message plaintext".
- **Account access** (a session token). Distinct from identity — the vendor's
  ACCESS/IDENTITY split in ADR-0018 is the right cut and I adopt it.

### 1.4 Availability

- **The ability to send at all.** A messenger that fails closed on attack is
  correct for confidentiality and is still broken for the user.
- **The ability of a group to change its own membership.** The project's own
  C-66 names this, and it is a genuine asset: a group that cannot remove someone
  is a group that cannot respond to compromise.
- **The ability to decrypt one's own history.**

---

## 2. Adversaries and their capabilities

### A1 — The server operator (primary)

The design claims zero trust, so this is the adversary the whole architecture is
aimed at, and the one a reviewer should spend most of their time on. Capabilities,
derived from `01-surface.md` §2.1 and §3.1:

- Reads and writes every database: OPAQUE password files, identity bundles,
  KeyPackages, the DS journal, blobs, channel envelopes, push subscriptions, TOTP
  secrets, recovery-contact hashes, word-vault blobs.
- Holds every server-side signing key: call JWT, auth attestation (which signs
  device-removal, genesis and backup-head attestations), and — where deployed —
  the KT operator key.
- Chooses what to serve each client, independently per client. **This is the
  capability that matters most**, because it is the one cryptography does not
  answer by itself: serving Alice a different key for Bob than it serves Carol is
  undetectable without an external consistency mechanism.
- Can withhold, delay, reorder within its own ordering freedom, and drop.
- Can decline to run any check it is supposed to run.

Notably it can also do everything A5 (any network peer) can, because every
unauthenticated endpoint in `01-surface.md` §2.1 is reachable from inside.

### A2 — A malicious or compromised group member

- Holds current epoch secrets, so present confidentiality is not in question.
- Can author proposals, application messages and — depending on role — commits.
- Can craft arbitrary bytes into every envelope parser on every peer's receive
  path. This is the widest *parser* surface in the system.
- Wants: persistence after removal, reading history from before they joined,
  wedging the group, impersonating another member, learning the wider graph.

### A3 — A malicious group admin or owner

Strictly stronger than A2 and worth separating, because Lakko layers a role
system on top of an egalitarian protocol (MLS lets any member commit). Every
enforcement is therefore a peer-side refusal convention, and the interesting
question is whether all three clients refuse identically.

- Can add and remove members, change roles, rotate the push routing secret,
  create invite links.
- Wants: to add an unseen member, to keep a removed member's access alive, to
  make the group permanently un-administrable.

### A4 — A removed member

The adversary the project has spent the most effort on (ADR-0043, ADR-0044) and
the one with the sharpest success criterion: *anything* they can still observe
after removal is a failure.

- Retains: every past epoch secret, every group id they ever saw, every invite
  link, every push routing secret current at removal, every pseudonym.
- Wants: to keep reading, or failing that to keep observing activity.

### A5 — Any network peer with an invite link

Deliberately separated from A2 because Lakko publishes capabilities in URLs.
Someone handed a `/g/` link — including someone whose join request was **rejected**
— holds a durable identifier. The interesting question is what that identifier is
worth after the social relationship ends. `01-surface.md` §2.1 answers it:
unauthenticated read and write on the DS journal, forever.

### A6 — A network observer / censor

- Sees TLS metadata: SNI, timing, volume, destination IP.
- Wants to identify Lakko traffic and block or throttle it, or to learn who is
  using it.
- Note the asymmetry the design already concedes: the browser cannot do raw-socket
  transports, so web users get no censorship resistance at all.

### A7 — A push provider (Apple, Google, Mozilla, a self-hosted UnifiedPush)

- Sees every wake: the destination device token and whatever the payload carries,
  in cleartext, at a rate that reveals conversation rhythm.
- Wants, or is compelled to produce, the co-membership graph.
- This adversary is often left out of messenger threat models and should not be:
  it sees a superset of what the operator sees about *timing*, for free.

### A8 — Another app, or a forensic tool, on the same device

- Reads whatever the OS lets it: browser storage on web, backup extracts on
  mobile, a filesystem image after seizure.
- With the user's password or recovery phrase, reads anything derived from them.
- Wants history, identity, and the ability to impersonate.

### A9 — The build and supply chain

- A dependency, a CI runner, a developer machine, the build inputs.
- Wants to reach every user at once. This is the cheapest adversary per victim in
  the whole model, and `01-surface.md` §4.3 shows the automated gate against it is
  one documentation-drift check.

### A10 — Nobody

Accident, concurrency, clock skew, elapsed time, a crash at the wrong moment.
Listed as an adversary on purpose. Lakko's own history — ADR-0040's crash window,
ADR-0044's F-A stale rebroadcast, ADR-0045's unbuildable sweep, the C-68
leaf-reuse impersonation — is a list of defects that needed **no attacker at all**,
and four of ADR-0045's seven security findings needed nobody. For a system of this
shape, A10 has the best track record of any adversary in this list, and a reviewer
who models only malice will under-weight it.

---

## 3. Attack goals, per adversary

Stated as things they want, not as vulnerabilities.

| Adversary | Goal |
|---|---|
| A1 operator | Substitute a key at first contact so a conversation is with me, not with Bob |
| A1 | Serve two users different views of the same account and not be caught |
| A1 | Recover message plaintext from anything I store |
| A1 | Build the social graph from what my own tables already contain |
| A1 | Roll a user back to an older state they have since moved past |
| A1 | Use a signing key I already hold to mint an attestation the client trusts |
| A2 member | Read what was said before I joined |
| A2 | Make another member's words appear to be mine, or erase theirs |
| A2 | Crash, wedge or desynchronise every other member from one message |
| A3 admin | Add a member nobody approved |
| A3 | Leave the group permanently unable to change its own membership |
| A4 removed | Keep receiving, or failing that keep observing that the group is alive |
| A5 link holder | Read the journal of a group that refused me |
| A5 | Write into that journal |
| A6 censor | Tell Lakko traffic apart from ordinary HTTPS |
| A7 provider | Cluster devices into conversations from wake patterns alone |
| A8 device | Decrypt the local stores using the password I phished |
| A9 supply chain | Ship one build that reaches everyone |
| A10 nobody | — (no goal; the question is what breaks, and whether it breaks loudly) |

---

## 4. Trust boundaries, and what each is supposed to hold

| # | Boundary | Supposed to hold against | Actually rests on |
|---|---|---|---|
| B1 | client ↔ `ds` | A1 reading plaintext | MLS. **Nothing** protects metadata or write access (`01-surface.md` §2.1) |
| B2 | client ↔ `discovery` | A1 learning usernames | OPRF for lookup; bundles are stored in the clear as public artefacts |
| B3 | client ↔ `auth` | A1 learning the password | OPAQUE |
| B4 | client ↔ `storage` | A1 reading attachments/backups | per-blob AEAD; backup under a phrase-derived KEK |
| B5 | client ↔ `sfu` | A1 hearing the call | SFrame, keyed per `(leaf, stream)` from the MLS exporter |
| B6 | client ↔ `channels` | A1 reading posts | per-epoch AEAD + post signature. **No authorisation at all** |
| B7 | client ↔ KT log | A1 equivocating | **Not deployed; client check off by default** |
| B8 | client ↔ push provider | A7 clustering | a derived opaque routing id; the cluster itself is not hidden |
| B9 | app ↔ OS keystore | A8 | Keychain / AndroidKeyStore / non-extractable WebCrypto |
| B10 | app ↔ its own extension (iOS NSE, web SW, Android background) | a compromised extension | convention: read-only, never persist |
| B11 | member ↔ non-member | A5 | possession of a pseudonym |
| B12 | member ↔ admin | A3 | a peer-verified role table each of three clients enforces separately |
| B13 | source ↔ build ↔ user | A9 | one `pull_request`-triggered documentation check |

B1, B6, B7 and B13 are the four where the boundary is materially thinner than a
reader of the project's documentation would expect. Each is a finding in
`01-surface.md` §5.

---

## 5. Delta against `docs/security/threat-model.md`

Read after writing everything above. The document is 111 lines: 8 assets, 8
adversary rows, the 7 invariants, one ASCII trust-boundary diagram, and 3
out-of-scope classes (hostile OS/browser, side channels, quantum).

Its central premise is stated well and we adopt it without reservation: *"The
model assumes a **fully compromised server**. […] The model is what survives
that."* That is the right premise for this product, and the asset table, the
OPAQUE and OPRF choices and the misrouted-Welcome row are all careful work.

### 5.1 Claims we agree hold — as *design intent*

Not as verified behaviour; this engagement verified no cryptographic mechanism.

- Message and attachment content is end-to-end encrypted with no non-E2EE path,
  and the server is architecturally outside the key graph (invariants #2, #4).
- Phone and email are never collected (invariant #1). The asset table's
  "NOT collected" is the strongest and cleanest claim in the document.
- Username lookup is OPRF-blinded.
- The misrouted-Welcome defence — a deterministic DeviceGroup `group_id` that
  lets a receiver reject before any state advance — is a genuinely good piece of
  modelling that we would not have thought to list.
- The out-of-scope section is honest and correctly scoped. In particular,
  declaring a live compromised device out of scope is the right call, and saying
  so beats pretending otherwise.

### 5.2 Claims we would state more narrowly

**On the social graph (asset table row 6, invariant #5, and the mass-correlation
adversary row).** All three cite *"presence = blinded per-conversation tokens"*
in the present tense as an active protection. `lakko-presence` has **no production
consumer on any platform**: it is reachable only through `lakko-ffi-wasm`, and
grepping the three client trees for `PresenceToken`, `deriveToken`,
`PresenceTracker` or `presence_seed` returns zero hits in each
(`01-surface.md` §1.2). The `presence_seed` MLS exporter the crate specifies
itself against does not exist in `lakko-mls`. The document lists a mitigation
that is not wired. See **LAK-RC-10** (and **LAK-RC-05** for the dead-code half).

**On the local-disk attacker.** The row reads: *"Every at-rest piece is wrapped
under a hardware-backed key […]. Plaintext is unreachable without the device's
biometric / passcode."* On web the at-rest key is HKDF-SHA-512 of the OPAQUE
export key, with no device entropy (`apps/web/src/auth/device-key.ts:71-105`), so
plaintext is reachable with the **password** and no device secret at all. On iOS
the items are `kSecAttrAccessibleAfterFirstUnlockThisDeviceOnly`, which is
unlocked from first unlock until reboot and demands no biometric per access; no
Secure Enclave key exists anywhere in `apps/ios` (`01-surface.md` §3.3). We would
state this per platform. This row's invariant #7 also drops "TPM 2.0", which
`CLAUDE.md`'s copy of the same invariant includes — the two lists disagree.

**On the compromised-server row.** *"What we MUST survive: […] identity
material"* is the equivocation question, and the document does not say what
answers it. Elsewhere the project answers "the KT log". That log is not deployed
and the client gate is off by default (`01-surface.md` §4.1, §4.2), so the
deployed posture against a server that substitutes a key at first contact is TOFU
plus a pin, with out-of-band safety-number comparison as the only real check —
and a later contradiction reaching `console.warn` only (§4.7). We would state the
surviving property as exactly that.

**On the coerced-maintainer row.** *"Reproducible builds + transparency log +
cosign-signed releases. A backdoored binary cannot ride the release channel
without forking the public hash record."* We measured the CI: `repro.yml` is
`workflow_dispatch`-only, and so is every other workflow except the
documentation-drift check (`01-surface.md` §4.3). `lakko-tuf`, the crate that
would verify a signed release artefact, has zero reverse dependencies (§1.2).
The web deploy path is `rsync` of a locally-built `dist/` (`justfile:798`). We
would not make this claim in the present tense.

**On the trust-boundary diagram.** It labels the client `TRUSTED` and the user
`assumed correct`. That is a legitimate modelling choice, but it is the reason
adversaries A2-A5 in §2 above have no row in this document, and those are the
adversaries the project's own last year of work (ADR-0022, 0027, 0043, 0045) was
almost entirely about. Two clients of the same app are not one trusted region.
The diagram also names the boundary as *"(client wasm/uniffi) ↔ (server gRPC /
HTTP)"*; there is no gRPC in the tree.

### 5.3 Claims we would make that they do not

The document's eight adversaries are all either network positions, the server,
or the physical device. **Every adversary who is a legitimate participant is
absent.** Specifically:

- **A2, a malicious group member,** and **A3, a malicious admin.** Not listed.
  This is the largest gap: Lakko layers a role system on an egalitarian protocol,
  so every membership rule is a peer-side refusal convention implemented three
  times, and the project's own security reviews found a confirmed impersonation
  (C-68) and three HIGH findings there. The threat model gives that whole class
  no row.
- **A4, a removed member.** Not listed, despite ADR-0043 and ADR-0044 existing
  almost entirely to constrain them.
- **A5, an invite-link holder,** including one whose join request was refused.
  Not listed, and it is the concrete attacker for `01-surface.md` §2.1 — a
  durable, unprivileged capability that outlives the social relationship that
  created it.
- **A7, the push provider.** Not listed. ADR-0042/0043 reason about it carefully
  in their own text, so the residual they accept has no home in the top-level
  document where a reader would look for it.
- **A9, the supply chain,** beyond the coerced maintainer. A compromised
  dependency or CI runner is not a coerced maintainer and is not covered by
  reproducible builds.
- **A10, accident.** Not modelled as a class at all. We would elevate it on the
  project's own evidence: of the seven findings ADR-0045's internal review
  produced, four needed no attacker whatsoever.

### 5.4 What neither of us claims — the column that matters

Properties a user of a "Telegram-class secure messenger with E2EE by default"
would reasonably assume, that nobody has committed to. These are not findings;
they are the honest edge of the product, and they are what a security summary
should say out loud.

1. **That your key is the right key on first contact.** Nothing verifies this
   today except comparing a safety number out of band. Nothing in the UI tells the
   user that this is the only thing standing there.
2. **That a group you were removed from stops being observable to you.** Forward
   secrecy stops you reading new messages. It does not stop you polling a journal
   whose id you retain.
3. **That being refused entry to a group means being outside it.** A rejected
   `/g/` link holder keeps the same journal access an approved member has, minus
   the keys.
4. **That your local history survives password compromise, on web.** It does not;
   the at-rest key is derived from the password.
5. **That a public channel cannot be silenced by a stranger.** `01-surface.md`
   §2.1 and LAK-RC-02 say otherwise.
6. **That "E2EE by default for everything" includes the metadata.** It does not.
   The vendor's out-of-scope section covers hostile OS, side channels and quantum
   — traffic analysis is *not* among the three, and the mass-correlation row
   claims defences (rotated pseudonyms, blinded presence, no global online
   surface) of which at least the presence half is unwired. So here neither of us
   is claiming the property, but the document reads as though one of us is.
7. **That the security features described in the changelog are switched on.**
   Three services and seven client-side pins say otherwise
   (`01-surface.md` §4.1, §4.2). This is the one we would put first in any
   user-facing summary.
8. **That what reaches the user is what was reviewed.** With no automatic CI gate,
   the relationship between a reviewed commit and a shipped artefact is a
   convention, not a control.

### 5.5 Our risk ranking, before any deep review

Stated so that the deep workstreams can confirm or embarrass it; either outcome is
useful.

1. **Deployment reality** — the strongest claims rest on services that do not run.
2. **Unauthenticated deployed services** (`ds`, `channels`) — the cheapest real
   attack in the system, needing only a link or a public handle.
3. **Accident and concurrency (A10)** in the group state machine — the highest
   base rate, on the project's own history.
4. **Three-client divergence** — one spec, three implementations, and the
   device-key story already differs materially.
5. **`lakko-sframe-native`** — 98 unsafe blocks on the untrusted media path, not
   fuzzed.
6. **The supply chain** — one automatic gate, and it does not check code.

Note what is *not* near the top: the cryptographic constructions. Nothing in this
pass suggested weakness there, and the primitive choices, domain separation
discipline and fuzz coverage of the envelope codecs are the strongest part of the
surface we saw. But we did not review them — see `01-surface.md` §7 — so this is
an absence of signal, not a clean bill.

---

## 6. Coverage statement

This model was derived from the surface map, not from the protocol. Specifically:

- **We did not model the MLS state machine's own transitions.** Sections 2-3 treat
  A2/A3/A4 by capability, not by reachable state. `audit-protocol` would do that
  work and this document should not be read as having done it.
- **We did not model the cryptographic constructions.** No key-derivation graph,
  no nonce-reuse analysis, no signature-format analysis. §5.5's placement of
  cryptography at the bottom reflects *what we saw*, which was almost nothing.
- **We did not model the call path beyond naming the SFU as a boundary.**
- **We did not model the recovery cascade** (ADR-0018/0049) beyond adopting its
  ACCESS/IDENTITY split. The word vault's 2^33 memory-hard search against a
  malicious operator is stated in ADR-0049 as arithmetic on an assumed
  per-evaluation cost, explicitly "to be measured". It has not been measured here
  either, and it is the one place the project concedes invariant #2 is weakened.
  That is the single highest-value unmeasured number in the engagement.
