<!-- SPDX-License-Identifier: AGPL-3.0-or-later -->
<!-- SPDX-FileCopyrightText: 2026 Lakko Contributors -->

# ADR-0092: Attachment kinds — one menu, nine kinds, and polls whose anonymity is proven

- **Status:** Accepted (2026-11-15, maintainer: every kind in v1 and one menu; location
  without tiles; real anonymity in group polls with the log-size proof only; newcomers vote
  in open polls; ballot delay 0 to 10 s; channel polls one account one vote by an anonymous
  credential; no video re-encoding in v1; OpenStreetMap on web). Nothing is built.
- **Date:** 2026-11-15
- **Numbering (2026-11-18):** written as ADR-0090 on its branch; renumbered to 0092 when main was
  merged (main holds 0090, PublisherGroup device leaves, and 0091, conversation rejoin).
- **Context:** UX spec [`docs/ux/attachments.md`](../ux/attachments.md); the chat envelope
  ([ADR-0023](0023-message-edit-delete-reactions.md), [ADR-0024](0024-typing-read-receipts.md),
  [ADR-0047](0047-media-messages-and-gif-proxy.md), [ADR-0052](0052-reply-forward-pin.md));
  journal access ([ADR-0053](0053-ds-journal-authorisation.md)); anonymous tokens
  ([ADR-0054](0054-anti-abuse-without-client-work.md)); who can find me
  ([ADR-0060](0060-scoped-discovery.md)); attachment lifetime
  ([ADR-0065](0065-attachment-lifetime-and-access.md)); servers cannot link accounts
  ([ADR-0078](0078-servers-cannot-link-accounts.md)); channels as conversations, comments
  and reactions ([ADR-0087](0087-group-and-channel-handles-and-privacy.md) sections 12,
  13, 13a); the external crypto review gate ([ADR-0046](0046-external-crypto-review.md),
  G-39)
- **Amends:** ADR-0047 (a fifth media kind, `audio`); ADR-0087 section 13a (a vote route beside the
  reaction route); ADR-0054 (auth also issues an anonymous credential)

## Context

### What the composer offers today

The paperclip behaves differently on each client (verified in the tree, 2026-11-15):

- **Web and desktop:** `apps/web/src/routes/Chat.tsx` and
  `apps/web/src/channels/ChannelComposer.tsx` wrap a hidden `<input type="file">` in the
  paperclip's `<label>`; one click opens the browser file chooser.
- **iOS:** `apps/ios/Lakko/UI/Chat/ChatScreen.swift` opens an in-app `lakkoActionSheet`
  with two rows, the system `PhotosPicker` and `.fileImporter`.
- **Android:** `apps/android/.../ui/chat/ChatScreen.kt` opens a Material `DropdownMenu`
  (the stock menu the design system bans) with two rows, `PickVisualMedia(ImageAndVideo)`
  and `OpenDocument`.

The maintainer asked (2026-11-15) for Telegram's menu — Photo or Video, File, Camera, Poll,
Audio, Checklist, Location, Article, and Contact — on every client, in one-to-one chats,
groups and channels, all in the first version, with three conditions: location without map
tiles and nothing leaving the device before the person taps; anonymous polls whose
anonymity and one-person-one-vote are enforced cryptographically, not by hiding names;
articles in a small bounded format that is neither HTML nor Markdown rendered as HTML.

### What the wire offers today

Every MLS application message and every channel post carries the JSON chat `Envelope` of
`crates/lakko-attachments`: text, `AttachmentRef`s with optional media metadata
(`MediaKind::{Voice, VideoNote, Gif, Other}`), and mutually exclusive op fields
(`edit_of`, `delete_of`, `react_to`, `typing`, `read_up_to`) classified by `parse_body`
into `MessageBody`. An op's author is the sender's account key resolved at receipt (C-68).
Inline playback is decided by a MIME allowlist alone. A channel post's plaintext is the
same envelope (ADR-0087 section 13); a subscriber is not an MLS member and writes through
`services/channels` under a key per write and an anonymous token (section 13a). The DS
accepts a journal row of at most 1 MiB (`services/ds/src/journal.rs`
`MAX_MESSAGE_BYTES`) and parses none.

### Why anonymous polls need a design

In a group every application message carries its sender's leaf, and every member resolves
it to an account. A vote sent as an ordinary message from the voter's leaf names the
voter to every member, whatever the UI hides. So an anonymous vote cannot travel as an
ordinary MLS message, and "one person, one vote" must then be checked some other way.

The scale requirement applies: a design that only works for small groups is not
acceptable as the final one.

## Decision

The menu is one `LakkoMenu` with nine items on every client (UX section 2). Photo or
Video, File and Camera reuse the existing attachment path. Six things are new on the wire:
an `audio` media kind, polls (with an anonymous ballot path), checklists, locations,
contact cards, and articles. Each is decoded strictly: a payload that breaks a bound is
dropped whole, and a message that carried nothing else shows the existing
unsupported-message sentence, never JSON or raw text.

### 1. Envelope layout

New **content** fields beside `text` (at most one per envelope; a second makes the
envelope `Unsupported`):

| Field | Body kind | Section |
|---|---|---|
| `poll` | `MessageBody::Poll` | 3 |
| `checklist` | `MessageBody::Checklist` | 5 |
| `location` | `MessageBody::Location` | 6 |
| `contact_card` | `MessageBody::ContactCard` | 7 |
| `article` | `MessageBody::Article` | 8 |

New **op** fields, joining the mutually exclusive op set of ADR-0023 (more than one op is
the existing degrade):

| Field | Body kind | Target |
|---|---|---|
| `poll_vote_of` + `choices` | `PollVote` | DS `messageId` of a non-anonymous poll |
| `poll_close_of` (+ optional `tally`) | `PollClose` | DS `messageId` of a poll |
| `check_of` + `item` + `done` | `ChecklistTick` | DS `messageId` of a checklist |
| `check_add_of` + `item_text` | `ChecklistAdd` | DS `messageId` of a checklist |

`edit_of` gains an optional replacement payload: `checklist` or `article` (an edit of a
poll is refused, UX section 7.3). The wire version stays 1; no production deployment
exists, so no dual reading is built.

FFI: one `compose*` and one typed parse result per kind on UniFFI and wasm. The NSE
extension surface (`lakko-ffi-extension`) gains the body-kind discriminants and the title
or question string only, so a notification reads `Poll: {question}`, `Checklist:
{title}`, `Location`, `Contact: {name}`, `Article: {title}` and never JSON; ops are
suppressed in the background like reactions.

### 2. Audio

`MediaKind::Audio` (`"audio"`), with two new optional `AttachmentRef` fields, `title` and
`artist`, each at most 255 bytes, UTF-8, no `Cc` and no `Cf` from the profile table
(dropped alone when they fail, the ADR-0047 lenient rule), and `duration_ms` kept for
`audio` in `1..=86_400_000` (chosen). The cover art is the existing `thumb`. Tags are read on
the sender's device; no server sees them. Inline playback stays on the MIME allowlist,
which gains `audio/mpeg` beside the existing `audio/mp4` (added 2026-11-16; each client's
player is still to be checked with it on a device).

### 3. Polls

Polls are offered in groups and channels only (amended 2026-11-18, maintainer: Telegram has
no poll in a private chat). The one-to-one menu has every other row; a poll that arrives in
a one-to-one is still shown and folded, the wire does not refuse it.

#### 3.1 The poll message

The `poll` field carries a canonical binary body and the author's signature over it:

```text
poll      = { body: b64(poll_body), author_sig: b64(64 bytes) }
poll_body = "lakko/poll/v1\0" || 0x01 || id(16) || flags(u8: bit0 multiple, bit1 quiz,
            bit2 anonymous, bit3 close_at present) || question_len(u8) || question
            || option_count(u8: 2..=10) || (len(u8) || option)*
            || [quiz: correct(u8) || explanation_len(u16 BE) || explanation]
            || [close_at: close_at_ms(i64 BE)]
            || [anonymous: secret(32) || ring0_hash(32)]
author_sig = Ed25519 by the author's account key over
             "lakko/poll/author-sig/v1\0" || group_id || poll_body
```

- `id` is random; inside the group a poll is named by `(group_id, DS messageId of the poll
  row)`, which every member reads identically and which votes, closes and ballots target.
- Question 1..=255 bytes, options 1..=100 bytes each, explanation 0..=600 bytes, all UTF-8
  under the display-text rule (no `Cc`, no `Cf` from the profile table); all bounds chosen,
  not measured. Decoding is strict and canonical; any violation drops the whole poll.
- **The author signature.** A receiver of the poll message checks it against the account
  key the MLS sender resolves to at receipt (C-68) and drops the poll if it fails. It is
  redundant for that receiver; it exists for the hand-over of section 3.9, where the poll
  reaches a newcomer through another member, so that the member passing it on cannot
  forge or alter a poll and attribute it to someone else.
- An anonymous poll is refused in a conversation of kind `OneToOne` (ADR-0062) by every
  receiver, and clients do not offer it in a group of fewer than three accounts (UX section
  7.1). In a quiz the correct index is in the message; a modified client can read it before
  voting. A quiz is not an exam.

#### 3.2 Non-anonymous votes

A vote is an MLS application message with `poll_vote_of` and `choices` (indices, sorted,
distinct; exactly one unless `multiple`; empty = retract). The fold is last-writer-wins per
(poll, voter account key) in DS `messageId` order. A vote is counted when its target is a
non-anonymous poll, its row precedes the poll's close (section 3.5) and its choices are
valid; otherwise dropped. Any member may vote, including one who joined after the poll was
created. The fold is written once in Rust (`lakko-attachments::poll::fold`, over the
client's stored ops) and called through the FFI, so the clients cannot disagree on a tally.

#### 3.3 Anonymous votes: the requirements

- No member, the poll's creator and admins included, learns which member cast a ballot.
- Each account in the poll's electorate casts at most one counted ballot; a re-vote by the
  same account (any of its devices) replaces the earlier one; an account outside the
  electorate casts none.
- Every member checks both properties itself; nobody is trusted to count.
- A member who joins while the poll is open sees it, sees its results and can vote.
- The DS learns no more than it does for any other row.
- No change to the `lakko-mls` state machine (invariant #3): only existing read accessors.

#### 3.4 The electorate (the ring) and its versions

The electorate is **every account that was a member at any moment while the poll was
open**. Members who leave stay in it and their ballots stay counted, as in Telegram.

- **Version 0** is the distinct account signature keys of the roster at the epoch the
  poll message was sealed, sorted bytewise. Every receiver computes it at receipt (its
  group is at that epoch when it processes the poll) from `Group::members()` through
  `account_key_at_leaf` (ADR-0050) and checks
  `ring0_hash = BLAKE3-derive-key("lakko/poll/ring/v1", keys)`, which the author signed;
  a mismatch makes the poll unverifiable (no totals, no voting), fail-closed.
- **Extensions are derived, not sent.** Every member processes every commit. When a
  merged commit adds a leaf whose account is not yet in the ring of an open anonymous poll,
  each member appends the new accounts of that commit, sorted bytewise among themselves,
  to the ring, in commit order. That gives version `k + 1`. No message is needed, and every
  member derives the same versions because every member applies the same commits in the
  same order.
- **Versions are prefixes.** Because extension only appends, version `k` is the first
  `L_k` keys of the current ring; a device stores the ring once plus the list of lengths.
  `ring_hash_k = BLAKE3-derive-key("lakko/poll/ring/v1", first L_k keys)`.
- A poll stops growing its ring when it closes.
- **Which key a voter proves with.** The ring holds, for each account, the key its leaves
  in this group are bound to (`account_key_at_leaf`), and a voter always proves with the
  secret of the key **its own leaf in this group** is bound to. Section 3.4a shows that the
  person holds that secret and that no rotation adds a second key of the same person.

#### 3.4a Rotation and reset: one person, one key in the ring

The rule must come from data every member holds identically, or members would derive
different ring versions and stop agreeing on what verifies. It does:

- **Rotation today (ADR-0074, Stage A).** A rotation leaves every conversation leaf bound
  to the account's **oldest** key, the conversation key, new conversations included, and a
  device restored or paired after a rotation mints its binding under that same oldest key
  (ADR-0074 section 3.7). The roster therefore never shows the new key, so ring
  extension (which reads the roster) never appends it: one person stays one key and one
  tag. The person's devices keep the predecessor seeds (`BackupPayload` v7 and the pairing
  state bundle format 2 carry them), so they hold the secret of the ring key and can vote.
  No new message, no `lakko-mls` change.
- **Rotation, Stage B (ADR-0074 section 3.6, not built).** Stage B would move leaves to the
  new key by removing old-key leaves and adding new-key leaves, which would put a second
  key of one person into the roster. This ADR sets two requirements on Stage B, recorded
  in that ADR when it is designed:
  1. **The succession travels before the Add.** Before the commit that adds the first
     new-key leaf of an account, the rotating account posts in the group, from an old-key
     leaf, a **ring-alias record** carrying the ADR-0074 two-way conversation link (201
     bytes, signed by both keys, `lakko-transparency::conversation_link`). Every member
     receives it in the same journal order before the commit, checks that its sender
     resolves (C-68) to the link's conversation key and that both signatures verify, and
     records the alias. Ring extension never appends a key recorded as an alias of a key
     already in the ring. All members hold the same journal, so all derive the same ring;
     the open-items hand-over carries the aliases to a newcomer. An application message,
     so no `lakko-mls` change. A new key that arrives without a prior alias record is
     appended as an ordinary newcomer, and the honest app never does that.
  2. **The old seed lives as long as an open poll needs it.** Stage B destroys a
     predecessor seed once no conversation holds an old-key leaf. It must also wait until
     no open poll's ring holds the old key without the new one, or the person loses their
     vote in that poll. The voter keeps proving with the old key, which is the one in the
     ring; one tag per person holds throughout.
- **Reset (ADR-0073).** A reset is a new identity with no link: the new key enters a group
  as a new account and is appended like any newcomer. That is not a double vote, because
  the old key has no usable holder: the reset clears the account's own predecessors
  (ADR-0073, as amended by ADR-0078 phase 6), and every other device is signed out and
  wipes everything local at its next landing (ADR-0073 section 3.5). One gap remains: a
  device that was offline during the reset still holds the old seed until it lands, and a
  ballot needs only the group's journal, not a session. So a client posts no ballot in a run
  until its session landing has succeeded in that run (a `key-generation-superseded`
  answer wipes it instead). A modified app on an old device kept offline across its own
  account's reset could still cast one ballot under the old key; recorded in section 3.8.

#### 3.5 Closing

`poll_close_of` by the poll's author, or in a group by the owner or an admin of the live
role table at receipt; otherwise dropped. Votes and ballots in rows after the close's DS
`messageId` are ignored, as are rows whose DS `created_at` is at or after `close_at_ms`.
Both are server-assigned values every member reads identically, so every tally is the
same. Closing is final.

#### 3.6 The ballot (recommended construction)

A ballot is **not** an MLS message. It is a journal row posted to the group's journal and
proven with the current epoch's access key (ADR-0053), which every member holds, so the
proof names no member:

```text
row      = "lakko/poll-ballot/v1\0" || 0x01 || nonce(24) || XChaCha20-Poly1305(seal_key, aad, pad(ballot))
aad      = "lakko/poll-ballot/v1\0" || 0x01
seal_key = HKDF-SHA256(secret, info = "lakko/poll/ballot-seal/v1" || id)
ballot   = id || ring_hash_k || set || choices || tag || proof
set      = count(u16 BE) || index(u32 BE)*    strictly ascending, each < L_k,
                                             count == min(256, L_k)
```

- **The proof** is a *linkable one-out-of-many proof* (the Groth–Kohlweiss family, of
  which Triptych is the linkable instance): a signature of knowledge, bound to
  `group_id || id || ring_hash_k || set || choices`, that the signer knows the secret
  scalar of one key in the ballot's **anonymity set**, without revealing which. Its size
  grows with the logarithm of the set.
- **The anonymity set** (amended 2026-11-15, maintainer: proving over the whole ring was
  rejected because its cost grows with the group). At most `ANONYMITY_SET = 256` keys of
  ring version `k`, chosen, not measured. A version of at most 256 keys is its own set. In a
  larger one the set is the voter's key and 255 others drawn by rejection sampling over an
  XOF seeded by `BLAKE3-derive-key("lakko/poll/anonymity-set/v1", x || group_id || id ||
  version)` with `x` the voter's secret scalar, skipping keys that are not valid points,
  sorted ascending. The account's **first** vote in the poll draws from the **newest**
  version its device holds; that version is recorded as `set_version` in the account's own
  poll-choice register (below), and every later vote from any device of the account draws
  from the recorded version again, never a newer one, so it gets the identical set and
  repeated votes cannot be intersected. Where the register has not arrived but the
  account's own earlier ballot is readable (found by its tag), the client takes that
  ballot's version instead. The ballot carries the index list; the
  verifier cannot recompute the draw (it needs `x`) and accepts any set of `min(256, L_k)`
  strictly ascending indices below `L_k` whose keys are valid points. That gives nobody a
  second vote: the tag does not depend on the set. Cost per ballot no longer depends on the
  group (section 3.7).
- **The tag** is `x^-1 · U`, with `x` the signer's secret scalar and `U` a point hashed from
  `"lakko/poll/tag-base/v1" || group_id || id` only. The tag base does **not** depend on
  the ring, so one account has one tag across every ring version: a ring extension cannot
  give anyone a second vote, a re-vote is recognised whatever version it was proven
  against, and nothing links a person's ballots in two polls.
- **Which version.** A ballot names the version its set was drawn from by its hash: the
  newest version at the account's first vote, or the recorded one on a revote (above). Receivers keep every version and accept a ballot against
  any version they hold; one naming a hash they do not hold is kept unverified and retried
  after the next commit, then dropped once the poll closes.
- **The tally** is: for each tag, the last valid ballot in DS order before the close; count
  its choices; an empty `choices` is a retracted vote. A ballot that does not open, does
  not verify, names another poll, or carries invalid choices is dropped.
- **Batching.** Not built: each ballot is verified on its own over its set of at most 256
  keys.
- **Posting delay.** The ballot leaves the device after a random delay uniform in
  `0..=10 s` (the maintainer's choice), from a queue that survives a restart, and never in
  the same request as a read receipt for the poll's conversation. It carries no push wake.
  The cost is stated plainly: ten seconds narrows who voted far more than a delay of
  minutes would, and the DS sees the network address of every post anyway (ADR-0078 class
  (c)). The delay is a small measure that stops the most direct "they read it, a ballot
  appeared" match; it is not timing protection, which waits for the class (c) work.
- **Own choice.** The voter's devices learn their own choice through a `lakko-sync`
  register in the DeviceGroup (poll -> choices), not from the ballot.
  Decided 2026-11-15 (maintainer): one register for group and channel polls, keyed on
  (conversation ref, poll message id as `i64`: the DS `messageId` of the poll row in a
  group, the post cursor in a channel), holding the choice indices (empty = retracted)
  with an HLC stamp, last writer wins. Kept until the conversation ends (pruned with an
  ended `conversation_ends` register), with a client creation cap of 10,000 (chosen, not a
  decode bound). Carried in its own DeviceGroup service envelope
  (`lakko/sync/poll-choice/v1\0`), persisted and backed up with the synced state, not in
  the growth snapshot; built in `lakko-sync::synced::poll_choice`. The entry also carries
  `set_version` (u32), the ring version of the account's anonymity set (0 for a channel
  poll); envelope version `0x02` and persisted state version 16 (14 on the branch; renumbered
  when main was merged), register tag 22, older ones refused.
- **The poll secret** travels in the poll body rather than coming from
  `Group::export_secret`: it survives later commits, reaches a newcomer through the
  hand-over, and needs no `lakko-mls` call at all.
- **Receivers** recognise the domain prefix and never feed a ballot row to MLS processing
  (the `WelcomeRendezvous` pattern); the NSE skips it.
- **The witness.** The signer's secret scalar is the Ed25519 signing scalar of the key its
  leaf in this group is bound to: the account key, or after a rotation the oldest own key,
  whose seed every device of the account keeps (ADR-0017, ADR-0074 section 3.7). Using one key in two signature schemes is a
  cross-protocol question; the transcripts are domain-separated and the proof cannot be
  read as an EdDSA signature, but no proof of joint security is cited here. It is listed
  for the G-39 review, with the alternative of a dedicated voting key.
- **Key checks.** Every key a set selects is decoded as an Edwards point and refused unless
  canonical, torsion-free and not the identity; a set that selects a refused key does not
  verify, and the draw skips such keys.
- **Implementation.** New crate `lakko-ballot` on `curve25519-dalek` (4.1.3, in the tree).
  No ring-signature or one-out-of-many crate is in `Cargo.lock`. On crates.io, `triptych`
  (0.1.x, from the Tari project) describes itself as experimental; whether it has been
  audited is not known and was not found. The construction is new cryptographic code in
  Lakko either way and enters the G-39 scope.

#### 3.7 Cost, from the sizes above (arithmetic, not measurement)

- Per device and poll, the ring: 32 bytes per account that was ever a member while the poll
  was open (320 KB for 10,000 accounts), plus 4 bytes per version.
- Per ballot: at most 2,366 bytes on the journal for any ring above 256 keys (proof 993
  bytes, set 1,026 bytes; exact, from the code), 830 bytes at 16 keys. Measured, single
  loaded runs on a laptop, no phone: casting about 0.1 s and verifying about 0.03 s,
  independent of the ring size between 256 and 262,144 keys (table in
  `crates/lakko-ballot/src/proof.sc.md`).
- The ring is capped at 262,144 keys (`MAX_RING_LEN`, chosen): 8 MiB per poll per device at
  the cap, and a hand-over of about nine 1 MiB rows.
- Per device, all ballots: one row per vote, as with any group message. A poll with `m`
  voters costs every member `m` downloads, the same order as `m` reactions in that group.
- Verification per device: one set-sized proof per ballot, so `m` ballots cost `m` times
  the figure above.
- The hand-over (section 3.9) costs one or more extra rows per admission.

#### 3.8 What remains visible

- **The anonymity set, not the group.** In a group of more than 256 members a ballot
  hides its voter among the 256 members of its set, not among the whole group.
- **How often a key appears.** Every voter's own key is in its own set; a member who never
  votes appears only in other voters' sets, at random. Counting how often a member's key
  appears across a poll's ballots therefore says a little about whether that member voted
  (a voter appears in at least one ballot, its own); it never says which ballot is theirs,
  since every ballot holds 256 keys on equal footing in the proof. The signal is weaker the
  more ballots and the larger the group; no figure is given here.
- **The version a ballot names.** It says the voter was in the ring at that version: every
  account's first vote names the newest version its device then held, so it does not say
  when the voter joined. A ballot naming an older version than others in the same period
  says its voter's first vote came before the later extensions.
- **Two sets for one account, in a race.** A device that votes before both the
  own-choice register and the account's earlier ballot reach it draws at its own newest
  version. If the ring has not grown since the account's first vote, that is the same
  version and the same set. If it has, the second ballot proves over another set; it carries
  the same tag, so the account is still counted once (its last ballot wins), and the last
  register write decides the version later votes reuse. What it costs is anonymity: anyone
  who reads both ballots sees one tag over two sets, so the voter is in their
  intersection. For two sets drawn independently from versions of about `N` keys, the
  expected intersection is the voter plus about `255 * 255 / N` others, a handful when `N`
  is in the thousands (arithmetic, not a measurement), and a third set narrows it further.
- **Timing of a newcomer's ballot.** A ballot that arrives right after an admission, in a
  poll that had been quiet, points at the newcomer by timing, not by content.
- **To the DS:** a row whose bytes do not look like MLS framing, at a time, from an
  address. Under ADR-0078 the DS does not know which account the address is; the address
  and time are the class (c) residual of that ADR, until a relay exists.
- **To members:** that a ballot arrived at a time. The 10-second delay hardly blurs it; a
  member who watches who is online (read receipts, typing, beacons) in a quiet group can
  often guess.
- **Small electorates:** with three voters who all choose one option, the totals say how
  each voted. Nothing hides that, here or in Telegram.
- **Re-votes:** members see that the holder of one tag changed its choice, not who.
- **An old device across a reset.** A modified app on a device that was offline during its
  own account's reset (section 3.4a) can cast one ballot under the old key before it
  lands and wipes, so that person can be counted twice in polls open across the reset.
  The honest app cannot: it posts no ballot before its session landing succeeds.

#### 3.9 The hand-over to a newcomer

A newcomer reads nothing sealed before its admission (the group's ordinary history rule),
so it would not see an open poll at all. The device that commits an admission (the
ADR-0045 sweep committer, an owner or admin) therefore sends the group, after the commit
merges and sealed at the new epoch, one **open-items hand-over** per admission commit:

```text
handover   = "lakko/open-items/v1\0" || 0x01 || item_count(u8: 0..=64) || item*
                                              // items strictly ascending by poll_ref
item       = poll_ref(i64 BE)                 // the DS messageId of the poll or checklist row
             || kind(u8) || author_account_key(32) || author_sig(64)
             || body_len(u16 BE: 1..=4096) || body || part
kind 1, anonymous poll part:
             ring_total(u32 BE: 1..=262144)
             || version_count(u16 BE) || version_count x length(u32 BE)   // the last == ring_total
             || key_offset(u32 BE) || key_count(u32 BE: 1..) || key_count x key(32)
kind 2, non-anonymous poll part:
             vote_total(u32 BE) || vote_offset(u32 BE) || vote_count(u32 BE)
             || vote_count x (voter_account_key(32) || message_id(i64 BE) || created_at_ms(i64 BE)
                              || choice_count(u8: 1..=10) || choice(u8)*)   // sorted by voter
kind 3, checklist (the current body, after the author's edits, with its signature):
             added_count(u8: 0..=30) || (message_id(i64 BE) || adder_key(32) || len(u8) || text)*
             || tick_count(u8: 0..=30)
             || (item_ref || ticker_key(32) || message_id(i64 BE) || done(u8))*
item_ref   = 0x01 || item_id(u32 BE)         // an item of the author's body
           | 0x02 || add_message_id(i64 BE)  // an added item, by the DS messageId of its add row
```

- **As built (2026-11-15, S4).** Changes to the first layout: a tick names an
  added item by the `messageId` of its add row (a `u32` could name only the author's
  items). A ring or a vote list larger than one row is split into parts: each part
  repeats the item header and carries its offset and the total, and the newcomer joins the
  parts of one item from one sender (a ring at the 262,144-key cap is about ten rows); a
  part that overlaps another from the same sender with different content drops that
  sender's copy of the item. A vote carries its DS `created_at` (the fold compares it with
  `close_at_ms`). The committer sends each voter's latest vote and one tick per done item,
  not the full op history; retracted votes and undone marks are left out, which folds the
  same. The codec, planner and assembler are `lakko_contacts::open_items` and
  `lakko_contacts::poll_ballots`, where bodies, rings and votes stay bytes
  (`lakko-attachments` depends on `lakko-contacts`, and the notification extension links
  `lakko-contacts`); the typed check is the crate `lakko-open-items`. A newcomer takes a
  hand-over only from an owner or admin, or from the account that committed its admission
  (the sender of its Welcome): the ADR rests the uncheckable parts on the committer's
  word, so a plain member who did not admit it is not taken. Added items and ticks are
  adopted as `privileged` records, since the committer's fold already applied the flags
  as they were and the newcomer never saw the edits that may have changed them. Old
  ballot rows: those whose DS `created_at` is older than the 30-day retention less one day
  (chosen), sorted and de-duplicated, `count(u16 BE)`, each row at most 4,096 bytes and
  starting with the ballot row domain.
- **Its own domain**, bounded and strict like every other envelope, dispatched as its own
  kind (never a bubble), fuzz target `open_items_decode`. Several rows when one would
  exceed the DS row cap; the 64 items per row is chosen, not measured. The checklist body
  gains the same author signature as a poll (section 5).
- **Open items only.** Closed polls and checklists stay history, which the newcomer does
  not read.
- **Anonymous ballots** are not in the hand-over: they are rows on the group journal under
  the poll secret, which the newcomer now holds, and it fetches them from the journal from
  `poll_message_id` onward and verifies each itself. A poll open longer than the journal
  retention (30 days, ADR-0024) has ballots the journal no longer holds; for those the
  committer appends its stored ballot rows (the sealed bytes, verifiable by the newcomer) in
  `lakko/poll-ballots/v1\0` rows (`poll_ref || count || (len(u16 BE) || row)*`, fuzz target
  `poll_ballots_decode`).
- **Who receives it.** All members receive it; members already holding the items ignore
  it, the newcomer adopts it.
- **When it is sent** (coordinator decision, 2026-11-15). After every admission commit
  that adds a leaf, **including a leaf of an account already in the group**: a new device
  of a member is admitted by an Add as well (ADR-0050), and it too reads nothing sealed
  before its admission, so without the hand-over it could not vote in or tick an open poll
  or checklist. The device that receives it adopts it by the same rules as a newcomer.

**What the newcomer trusts.** The hand-over is authenticated as coming from a member of
the group (its MLS sender resolved at receipt, C-68), not from each poll's author. Then:

- **The poll itself** is the author's: the newcomer checks `author_sig` against
  `author_account_key` and drops the item if it fails, so the committer cannot invent or
  alter a poll in someone else's name. The newcomer cannot check that the author was a
  member when the poll was created; it shows the author's name as it resolves the key.
- **The ring versions and the pre-admission votes and ticks** rest on the committer's
  word. Version 0 is checked against the signed `ring0_hash`; the extensions and the
  non-anonymous votes are not checkable by a device that did not see the commits and
  messages they come from. A dishonest committer could add a key it controls to the
  newcomer's ring (an extra ballot only that newcomer would count), drop or invent earlier
  public votes, or withhold old ballot rows, changing the totals **the newcomer** shows.
  It cannot change what any other member counts. The committer is an owner or admin of the
  group, who can already remove members; the ADR records this as accepted.
- **Votes after the admission** reach the newcomer as they reach everyone, and each is
  checked as usual.

#### 3.10 What changes for a member who leaves

Nothing about the poll. The account stays in the ring and its ballot or vote stays counted.
After its epoch keys rotate out it can no longer post to the journal, so it cannot vote
again.

#### 3.11 History bundles and the vault

Decided 2026-11-15 by the coordinator of the build (not by the maintainer). A message
reaches a new device of a member as a history-bundle placeholder
(`lakko_mls::history_placeholder`) or as a vault record (`lakko_vault::record`, ADR-0064),
not as its envelope. Both carried only text and attachments, so a poll, checklist,
location, contact card or article arrived as an empty row. Now:

- **Content.** Both carry, per message, the content kind (1 poll, 2 checklist, 3 location,
  4 contact card, 5 article) and the content payload: the JSON value of the envelope's
  content field, exactly what the envelope carried (for an edited checklist or article,
  the latest replacement the device shows). An article's images are the message's
  attachments, carried as before. The payload is opaque and length-bounded in
  `lakko-mls` and `lakko-vault` (1..=196,608 bytes, chosen: above the largest article as
  base64 JSON, 174,766 bytes) and decoded at the FFI with the envelope's own decoders
  (`lakko_attachments::Content::from_payload`), the ADR-0047 pattern, so the two routes
  cannot disagree.
- **Folded state.** For a poll or a checklist they also carry a snapshot of its state as
  the sending device saw it (`lakko_attachments::content_state::ContentState`, at most
  8,192 bytes, chosen): a poll's counts per option, voter count, and whether it is open,
  closed by its time, or closed by a close op (with that op's message id); a checklist's
  close message id, its added items (message id, text) and its done items (item, who
  marked it). The sender is one of the account's own devices, trusted as the rest of the
  bundle is. A restored item renders from the snapshot; live ops after the bundle continue
  from the journal on top of it.
- **Audio.** Both carry an audio attachment's `title` and `artist`.
- **Voting and ticking** in an open poll or checklist from the new device needs the
  section 3.9 hand-over, which since this decision also fires for a new leaf of an
  existing account. The bundle alone shows the item; it does not give the new device the
  ring or the pre-admission votes.
- **Flag day.** The placeholder is version 4 (3 and older refused) and the vault records
  version 2 (1 refused).

### 4. Polls in channels

#### 4.1 The rule

A poll post is an ordinary post: its plaintext is the chat envelope with the poll block of
section 3.1 (without the anonymous fields; the author signature is the post signature). A
subscriber is not an MLS member, so the ring of section 3 is not available. **One account,
one vote** is enforced instead by an **anonymous credential**: auth issues each account one
credential at a time; a vote proves possession of a valid credential and carries a tag
that is the same for one credential secret and one poll, so the channel service refuses a
second vote with that tag, while learning neither the account nor any link between one
person's votes in two polls. Nobody, publishers included, learns who voted for what. Every
channel poll is anonymous and the switch is not shown.

#### 4.2 The credential

A **keyed-verification anonymous credential** (an algebraic MAC on group elements, the
construction of Chase, Meiklejohn and Zaverucha (CMZ 2014) that Signal's private group
system builds on), on Ristretto255.

- **Attributes.** A client secret `s` (a scalar, hidden from auth) and a public `period`
  (the 30-day window the credential is for; chosen, not measured).
- **The secret is fixed per account.** On its first issuance an account registers with auth
  a commitment `C = s * G_cred` (`G_cred` a dedicated generator) and auth stores it. Every
  later issuance is blind: the client sends a proof that it knows the discrete log `s` of
  the stored `C`, bound to the period key; auth computes the MAC on `C` itself
  (`U = b * G_cred`, the `s` term as `b * C`) and returns it with a proof that it used its
  published key. Auth never sees `s`. (Amended while building `lakko-anon-cred`: the
  first text had the client send `s * G_cred` under ElGamal, but auth already stores exactly
  that point, so the encryption hid nothing.) Because `s` cannot change between periods, an
  account's tag for a poll is the same whichever period's credential it presents, so a poll
  that runs across a period boundary gives no second vote. Under the DDH assumption in
  Ristretto, `C` does not let auth recognise tags made from `s`.
- **Issuance.** Bearer-authenticated `POST /auth/anon-credential` (and the first-time
  `PUT /auth/anon-credential/commitment`). At most one issuance per (account, period); a
  repeat in the same period re-issues for the same `C` (a second device of the account, a
  reinstall) and yields an equivalent credential for the same `s`, so it buys nothing. Rows:
  `anon_credential_commitments (account_id, commitment)` and `anon_credential_issued
  (account_id, period)`, one auth migration. Rate limit: the per-account counters of
  ADR-0054 section S4. The client derives `s` from the lineage seed (the oldest own key,
  ADR-0078 phase 6), `BLAKE3-derive-key("lakko/anon-credential/secret/v1", lineage_seed)`
  reduced to a scalar, so every device of the account computes the same `s` and nothing
  extra is synced.
- **Lifetime.** A credential is accepted for its period and the next one (a grace so a
  device need not refresh at a boundary). The client asks for the current period's
  credential when it first needs one in that period.
- **Rotation (ADR-0074).** The lineage seed does not change, so `s`, `C` and the tags stay
  the same. Nothing to do.
- **Key reset (ADR-0073).** The old seed is gone, so the person cannot produce `s`. A reset
  therefore replaces `C` (inside the reset transaction, which already runs under the
  fresh-auth grant and the `KeyReset` limit of 3 a day). The new `s` gives new tags, so a
  person who resets can vote again in a channel poll they already voted in. Recorded in
  4.6; bounded by how often an account can be reset.
- **Account deletion** deletes both rows.

#### 4.3 A vote

- **Poll identifier.** `poll = (channel_id, post_cursor)`: what the reaction route already
  names. The tag base is `H_poll = hash-to-group("lakko/channel-poll/tag-base/v1" ||
  channel_id || post_cursor)`.
- **Tag.** `T = s * H_poll`, a Ristretto point: deterministic per (credential secret, poll),
  unlinkable across polls and to `C` under DDH.
- **Presentation.** The vote request carries a fresh, unlinkable presentation of the
  credential (randomised as in CMZ) and a proof that the same hidden `s` is the credential's
  attribute and the discrete log of `T` to base `H_poll`, with the period revealed. The proof
  is bound by Fiat-Shamir to the channel, the post, `T` and a message the service defines
  canonically: the action (`PUT` or `DELETE`), the labels and a freshness value, so a
  captured presentation cannot be replayed as a retract or to restore a vote its voter has
  since changed.
- **Labels.** As before, `label_i = BLAKE3-keyed(reaction_key, post_cursor ||
  "lakko/poll-option/v1\0" || option_index)`, 16 bytes, so the service sees opaque labels
  only. One vote carries 1..=10 labels.
- **Route.** `PUT /v1/channels/{id}/posts/{post}/votes/{tag}` with the presentation and the
  labels: verifies, then stores or replaces the row `(post, tag, labels)` and adjusts the
  per-label counters in the same transaction. A retract is `DELETE` on the same path with a
  fresh presentation for the same tag (proving the caller holds `s`). No anonymous token is
  spent: the credential is the rate limit for the vote itself; repeated replaces under one
  tag are capped in memory per tag (a chosen number). Readers read the counts with the
  reaction counts (`GET .../reactions?posts=` gains `votes`).
- **Close** is unchanged from before: the publisher edits the post with the frozen tally it
  read at close time; readers show that tally and stop offering a vote; an automatic close
  time lives only in the encrypted post. The service keeps counting after a close and
  readers ignore it. No option count and no close time reach the service.
  The frozen tally rides in the close op itself: the `poll_close_of` envelope gains an
  optional `tally` field, standard base64 of the encoded `lakko_channels::ChannelPollTally`
  (at most 118 bytes); a reader that cannot decode it keeps the close and drops the tally.

#### 4.4 Who verifies

KVAC is keyed-verification: the verifier needs the issuer's secret key. Two choices:

- **The channels service holds a key derived for this purpose** from a master secret it
  shares with auth (the ADR-0054 anonymous-token pattern). It is a master of its own,
  not the token master, which discovery, the DS and push-gw also hold. Chosen. Cost: whoever compromises the
  channels service can mint credentials and cast any number of votes. The same service
  already stores and serves the counts readers see, so a compromise of it already decides
  what results look like; the key adds no new kind of power over results.
- **Auth verifies each presentation on the channels service's behalf.** Auth would then
  see every vote's channel, post, tag and time, and the account's other traffic reaches
  the same service, so timing could link votes to accounts. Rejected.

#### 4.5 What the service now learns

The earlier claim that the service cannot tell a vote from a reaction **no longer holds**:
a vote arrives on its own route with a credential presentation. The service learns:

- that a post has votes, how many distinct voters (tags) and the count per opaque label,
  and therefore a lower bound on the number of options (the labels that received votes);
- that the holder of one tag changed or withdrew a vote (not who);
- the period of the credential presented (shared by every account issued that period);
- the address and time of each request, until the ADR-0078 class (c) relay exists.

It does not learn the question, the options, which label is which option, whether the poll
is a quiz, when it closes, or which account voted. Auth learns that an account obtained a
credential for a period, not where it is shown.

#### 4.6 Limits

- **Many accounts, many votes.** One credential per account bounds votes per account, not
  per person. Creating an account costs the one heavy proof of work at signup and its rate
  limits (ADR-0054); that cost is what bounds a Sybil voter.
- **A key reset** gives the account a new `s` and so a second vote in polls it already
  voted in (4.2). Bounded by the reset limit and by what a reset costs the person (every
  other device signed out, passkeys deleted).
- **A compromised channels service** can inflate counts (4.4).
- **The frozen tally is the publisher's reading**; a publisher can misreport it, as a
  publisher controls what its channel says anyway.
- **Quiz:** the correct index is in the post, as in a group.

#### 4.7 Implementation and the shared mechanism

- **Crate.** New `lakko-anon-cred` on `curve25519-dalek` (4.1.3, in the tree): the algebraic
  MAC, blind issuance against a stored commitment, presentation with a tag, a Fiat-Shamir
  transcript, bounded fail-closed decoders. Fuzz targets `credential_decode`,
  `presentation_verify` (under a fixed key; nothing the fuzzer could not have produced
  verifies) and `issuance_request_decode`. Property tests: a presentation verifies only for
  its post; two presentations by one `s` on one poll share a tag, on two polls do not; a
  presentation is unlinkable to its issuance in the sense testable without a reduction
  (fresh randomness, no repeated values); a request for a different `s` than `C` is refused.
- **What exists.** Signal's `zkgroup` and `poksho` crates in the `libsignal` repository
  implement this family; the libsignal README states the repository is licensed under the
  GNU AGPLv3, compatible with Lakko's licence. They are built for Signal's own credential
  types. I found no evidence of a public audit report for them and claim none. Nothing of
  this kind is in `Cargo.lock`. The maintainer chose `lakko-anon-cred` (2026-11-15); these crates are
  the reference, not a dependency.
- **ADR-0086.** That ADR's release gate needs a short-lived anonymous credential too, but
  as a set-membership proof over many issuers, which it says no existing construction fits
  as is. `lakko-anon-cred`'s MAC, transcript and proof layer are meant to be reusable by it;
  the mechanism is not designed here.
- **G-39.** The credential, its blind issuance and the tag proof are new cryptographic code
  and join the external review scope, beside `lakko-ballot`.

### 5. Checklists

```text
checklist      = { body: b64(checklist_body), author_sig: b64(64 bytes) }
checklist_body = "lakko/checklist/v1\0" || 0x01 || flags(u8: bit0 others_add, bit1 others_mark)
                 || title_len(u8) || title || item_count(u8: 1..=30)
                 || (item_id(u32 BE) || len(u8) || text)*
author_sig     = Ed25519 by the author's account key over
                 "lakko/checklist/author-sig/v1\0" || group_id || checklist_body
```

Title 1..=255 bytes, item text 1..=100 bytes, strict and canonical, as for a poll; the
author signature serves the hand-over of section 3.9, where a newcomer receives an open
checklist with its added items and ticks. A checklist is open until its author closes it
(the poll close op, applied to a checklist) or deletes it. Item ids are unique and assigned by the author; an added item's id is the DS `messageId` of
the `ChecklistAdd` row (namespaced from the author's ids by a flag bit in the parse result).
A tick is LWW per (checklist, item) in DS order and records the ticker's account key, shown
by name. Authority at receipt: the author always; another member only when the matching
flag is set. In a channel checklists are posts: only publishers tick and add, as signed
post ops (the ADR-0087 section 13 edit path), and both flags are absent. An edit by the
author (section 1) replaces title, texts and order; ticks stay with item ids, and a tick on
an item the edit removed is dropped. Bounds chosen, not measured.

### 6. Location

```text
location = { lat_e7: i32 in -900_000_000..=900_000_000,
             lon_e7: i32 in -1_800_000_000..=1_800_000_000,
             accuracy_m: u32 in 1..=1_000_000,
             fix_at_ms: i64,
             name: optional 1..=255 bytes, display-text rule }
```

Fixed point at 1e-7 degree; no floating point on the wire. No tiles anywhere, no
geocoding, no Lakko request. Opening a location hands the coordinates to Apple Maps
(`maps.apple.com` URL scheme), the `geo:` intent (Android), or `openstreetmap.org` in the
browser (web, desktop, through the host's `open-url`) only after the person taps (UX
section 9.3). The fix itself comes from the platform's location service; Android uses
`LocationManager` so no Google Play services dependency is added. The platform's service
may use its vendor's network to compute a fix; that is outside Lakko and is not claimed
otherwise. **Live location is not in v1.**

### 7. Contact card

```text
contact_card = { name: 1..=255 bytes (display-name rule), username: canonical,
                 account_key: 32 bytes }
```

No reach grant, no mailbox binding, nothing that lets the receiver write to the person
described. The receiver adds through the ordinary username lookup (ADR-0060), which
respects that person's "who can find me" setting, and accepts the result only when the
discovered account key equals the card's; a different key is shown as "this username now
belongs to someone else" and nothing is sent.

Why not carry a reach grant: a grant is the described person's permission to write to
their mailbox, handed to the sender for the sender's use. Forwarding it would let the
sender widen who can reach a third person beyond what that person chose. A person who wants
to be reachable by strangers hands out their own invite link (ADR-0079).

**A person on `Nobody` is never shared (amended 2026-11-18, maintainer).** The profile
envelope (ADR-0048, `lakko-contacts::profile`) gains one flag byte after the avatar,
version 5 (flag day, v4 refused): bit 0 set = "my card may not be shared", every other bit
refused. A client sets it exactly when its own "who can find me" setting is `Nobody`, and a
change of the setting republishes the profile in every one-to-one and group it already
publishes to. A sender's contact picker and the forward of a contact-card message read the
newest profile held from the person on the card and refuse when the bit is set; a person
who never sent a profile counts as shareable. Like every profile field it is the person's
own statement and the receiver's rule: a modified client can still send a card, which then
gives the receiver nothing, since `Nobody` answers no lookup and the card carries no grant.

### 8. Article

A bounded binary document, carried base64 in the `article` field:

```text
article  = "lakko/article/v1\0" || 0x01 || title_len(u8) || title || block_count(u16 BE) || block*
block    = 0x01 paragraph   || inlines
         | 0x02 heading     || level(u8: 1|2) || inlines
         | 0x03 quote       || inlines
         | 0x04 list        || ordered(u8: 0|1) || item_count(u16 BE) || (depth(u8: 0|1) || inlines)*
         | 0x05 image       || attachment_index(u8) || inlines        // caption, may be empty
         | 0x06 divider
inlines  = run_count(u16 BE) || run*
run      = 0x01 text        || marks(u8: bit0 bold, bit1 italic) || len(u16 BE) || utf8
         | 0x02 link        || url_len(u16 BE) || url || marks(u8) || len(u16 BE) || utf8
```

- **Closed node set.** Any other tag, mark bit, level or depth is a refusal of the whole
  article.
- **Bounds (chosen, not measured):** encoded size at most 131,072 bytes; text bytes in all
  runs at most 65,536; at most 2,048 blocks; at most 256 runs per block; at most 20 image
  blocks, each index pointing at a distinct `AttachmentRef` of the same envelope that the
  MIME allowlist accepts as an image; a link URL `https://` only, at most 2,048 bytes, the
  host rules of the message-links splitter; no `Cc` and no `Cf` in any text (a line break
  is a block boundary, never a character).
- **Canonical.** Empty runs are refused and adjacent runs with the same marks must be
  merged; the decoder refuses non-canonical input, so an article re-encodes to the same
  bytes and a hash of it is meaningful.
- **Rendering** builds native views on each client (SolidJS elements, SwiftUI, Compose)
  from the decoded tree; no HTML string is ever produced, no `innerHTML`, no Markdown
  parser. The excerpt for the bubble is computed by the core from the text runs.
- **Size against the carriers:** 131,072 bytes encode to 174,764 bytes of base64, inside
  the 1 MiB DS row and the channel post's `MAX_PLAINTEXT_LEN`.
- Images inside are ordinary attachments: their lifetime is ADR-0065's (and ADR-0087's 45
  days in a channel); an expired image shows the existing gone state.

### 9. Files

Photo or Video, Camera and File change no wire format. Two client rules move into the core
so the clients agree: `lakko_ui_rules::attachment_menu(conversation_kind, role,
has_camera, has_location)` returns the menu rows of UX section 2.4, and the photo
re-encode target (longest side 2,560 px, chosen) is a constant on both FFI surfaces.

### 10. Fuzz targets and tests

- `lakko-attachments/message_body_parse` extended over every new field, with the
  round-trip assertions of ADR-0047 / 0052 for each new payload.
- `lakko-attachments/article_decode`: decode, canonical re-encode equality, bounds.
- `lakko-ballot/ballot_open`: row split, AEAD refusal, ballot parse, proof verification
  under a fixed ring; asserts nothing the fuzzer could not have signed verifies.
- `lakko-ballot` property tests: completeness for every ring position; a ballot verifies
  under its ring only; two ballots by one key in one poll share a tag and in two polls do
  not; a ballot from outside the ring never verifies; tally is independent of the order
  ballots were verified in; the tag of one key in one poll is the same against every ring
  version; a ring extension never lets one account be counted twice.
- `lakko-contacts` (or wherever the hand-over lands) `open_items_decode` and
  `poll_ballots_decode`: bounded strict decode, canonical round trip, author-signature
  refusal under a fixed key.
- `lakko-channels`: option labels never equal a reaction label for any emoji in the bundled
  set (property test).
- `lakko-anon-cred`: the fuzz targets and property tests of section 4.7.
- `services/auth` and `services/channels`: DB-gated route tests for commitment, issuance
  (one per period, re-issue for the same commitment, reset replaces it), vote, replace,
  retract and refusal of a second tag-less or foreign-post presentation.

## Alternatives

### For anonymous votes

**(a1) LSAG (Liu–Wei–Wong linkable ring signature) over the same ring.** Simple and widely
analysed (Monero's CLSAG descends from it). Its signature is one challenge plus one
response per ring member: `32 * (n + 2)` bytes for a ring of `n`. That is 32,064 bytes at
`n = 1,000`; a ring of 32,767 members would not fit a 1 MiB DS row at all; and every member
downloads `n` ballots of that size, `32 * n * (n + 2)` bytes, about 3.2 GB at `n = 10,000`.
Verification is linear per ballot, quadratic in all. It works for small groups and is
therefore not the final design under the scale requirement, and the maintainer chose the
log-size proof alone (2026-11-15).

**(b) Blind signatures issued by the poll's creator.** Each member asks the creator, in an
MLS message, to blindly sign a token; votes carry an unblinded token. Small and fast, but:
the creator must be online for every voter; every member sees who asked, so participation
is public and only the choice is hidden; a privately verifiable token (the VOPRF of
`lakko-anon-token`) lets only the creator check ballots, so it would need a publicly
verifiable blind signature (blind RSA, RFC 9474), which is not in the tree; and the creator
can mint tokens for itself with nothing to detect it but a count of requests against
ballots, which a creator fills from members who asked and did not vote. It makes the
creator a trusted counter. Rejected.

**(c) A zk-SNARK membership proof with a nullifier (the Semaphore shape).** Constant-size
proofs and constant verification per ballot, the best scaling of all. Costs: a pairing
curve, a circuit with an in-circuit hash, a proving system (Groth16 with a per-circuit
trusted setup, or a universal-setup or transparent system), a prover on phones and in
wasm whose speed is unmeasured here, and a large new dependency and review surface; each
member's commitment would also have to be distributed and kept in a Merkle tree per group.
Not chosen now; the strongest candidate if the log-size proof turns out too slow at the
group sizes Lakko must serve.

**(d) Curve Trees and other transparent log-size membership proofs.** Comparable aims to
(a) with different trade-offs; not chosen because the Groth–Kohlweiss family is older and
more analysed. Open to the review.

**(e) A tallying server.** The DS or a new service could count; it would have to learn the
electorate (the group's account keys, i.e. the social graph, ADR-0078) to check
membership, or trust clients. Rejected.

**(f) Hiding names in the UI.** What the maintainer rejected.

### For the rest

- **Articles as HTML or Markdown.** A renderer of either is a parser of attacker-chosen
  input whose output reaches a web view; a sanitiser is a list of what to forbid. The
  closed format makes the forbidden things inexpressible. Rejected.
- **Articles as a separate encrypted blob.** Keeps messages small, but makes the text
  subject to the blob lifetime and to a download before reading. The bound fits in a row,
  so no.
- **Map tiles from a Lakko proxy or a provider.** Tiles reveal the place to whoever serves
  them, before any tap. Rejected by the maintainer.
- **Contact cards carrying a reach grant.** Section 7.
- **New body kinds as separate envelope domains** (the service-envelope pattern). Those are
  for state no person reads as a message; these are messages, replied to, forwarded,
  pinned and saved like any other, so they belong in the chat envelope.
  (Forwarding, amended 2026-11-18: location, contact card and article are forwarded as
  the same content with the forward line; a poll or checklist is not forwarded, since its
  votes and ticks belong to its conversation. UX spec section 11a.)

## Consequences

- **Positive:** one menu and one behaviour on four clients; anonymous polls in groups whose
  anonymity and one-vote-per-person every member checks; one vote per account in channel
  polls without the service learning the account; a newcomer sees, reads and votes in
  every open poll and checklist; no server learns poll contents, places, contacts shared or
  articles; no map provider sees a place before a tap; no photo-library permission is ever
  asked.
- **Negative:** two new cryptographic constructions in Lakko (`lakko-ballot`,
  `lakko-anon-cred`), which grow the G-39 review scope and gate release; the 10-second
  ballot delay leaves arrival time a usable hint until the ADR-0078 class (c) work exists;
  a newcomer trusts the admitting owner or admin for ring extensions and earlier public
  votes it cannot check; the ring and the stored ballots of a long-open poll grow with every
  admission; ADR-0074 Stage B inherits two requirements (section 3.4a); the channel service
  can now tell votes from reactions and counts voters per post; one vote per account in a
  channel is one vote per account, not per person, and a key reset gives one more; a
  compromised channels service can inflate counts; a channel poll's final tally is the
  publisher's reading; the account key is used as a ring-proof witness pending review.
- **Neutral:** no `lakko-mls` change and no DS change; one `services/auth` migration and
  route, one `services/channels` migration and route, one master secret shared by auth and
  channels; an extra hand-over row per admission into a group with open items.

### Invariants

1. No PII on the server: unchanged; places, contacts and polls are inside the seal; auth
   stores a commitment and an issued-period flag per account, no personal data.
2. The server sees ciphertext and public artefacts: ballots and hand-overs are sealed
   rows; a channel vote is a tag, opaque labels and a credential presentation.
3. MLS state machine: untouched. Only `members()`, `account_key_at_leaf` and the commit
   effects every client already reads are used. The ballot proof and the credential are
   adjacent crypto and enter G-39.
4. No fallback: an unverifiable poll shows no totals and takes no vote; a ballot or a
   presentation that does not verify is refused; a hand-over item whose author signature
   fails is dropped; nothing falls back to a named vote.
5. Social graph: a ballot is proven by the shared epoch key and names nobody; a channel
   vote names no account, and its tag spans one poll; auth never learns where a credential
   is shown.
6. Identity rotation: a rotation never puts a second key of one person into a poll's ring
   (section 3.4a), and leaves the channel credential secret unchanged (section 4.2). A reset
   is a new identity whose old key has no remaining holder; in channels it gives a new
   credential secret (section 4.6).
7. Device keys: unchanged.

## Build plan

| Sprint | Content |
|---|---|
| S0 | The G-39 brief extended with the ballot construction, the growing ring, the witness question and the channel credential |
| S1 | `lakko-attachments`: audio fields, location, contact card, poll and checklist bodies with author signatures, non-anonymous votes, ticks and their folds, strict decoding, fuzz extension; both FFI surfaces; NSE kinds; `lakko_ui_rules::attachment_menu` |
| S2 | Article codec, excerpt, fuzz target, FFI |
| S3 | `lakko-ballot`: the log-size linkable proof only, tag, ring versions, row seal, tally fold, property tests, fuzz target, measured size and timing table; FFI; the `lakko-sync` own-choice register |
| S4 | The open-items hand-over and the old-ballot rows (codec, fuzz targets, the committer's send after an admission, the newcomer's adoption); FFI |
| S5 | `lakko-anon-cred` (credential, blind issuance against a commitment, presentation with tag, fuzz targets, property tests); `services/auth` commitment and issuance routes and migration, the reset hook, account deletion; `services/channels` vote route, counters and migration, the shared master secret; channel option labels and the frozen tally in `lakko-channels`; FFI |
| S6 | Web and desktop: the menu, every editor, bubble, reader, location, camera view, ballot queue, hand-over, channel votes |
| S7 | iOS and Android, the same; the Android `DropdownMenu` replaced by `LakkoMenu` |
| S8 | `lakko-security` on S3, S4 and S5 on the maintainer's request; PRODUCT-READINESS rows; walks on two devices per kind, one walk that admits a member while a poll is open, and one channel poll voted from two accounts and re-voted |

## Decisions and open questions

Decided by the maintainer on 2026-11-15:

- **The scalable proof only.** Group anonymous polls use the log-size linkable proof of
  section 3.6; LSAG is an alternative considered, with no capped fallback in the plan.
- **A newcomer sees, reads and votes in open polls** (section 3.9, the growing ring of 3.4).
- **The ballot delay is `0..=10 s`** (section 3.6).
- **Channel polls: one account, one vote, by an anonymous credential** (section 4), with
  no option count and no close time at the service.
- **A newcomer trusts the admitting owner or admin for what it cannot check** (ring
  extensions, earlier public votes and ticks, old ballot rows; section 3.9). Accepted:
  that person already decides who is in the group at all, and a dishonest one can change
  only the totals the newcomer shows, never what any other member counts.
- **A rotation during an open poll must not give a second vote.** A defect, closed by
  section 3.4a.
- **Defaults kept:** no video re-encoding in v1 (a picked video that is not MP4 / H.264 or
  over 50 MB is sent as a file); a location opens on OpenStreetMap on web and desktop.

- **The credential implementation is Lakko's own crate** `lakko-anon-cred`, written with
  libsignal's `zkgroup` / `poksho` source and the published papers as the reference; no
  dependency on libsignal.

Still open:

1. **The witness** (for the review, not the product). The key the voter's leaf is bound to,
   used as the ring-proof secret, or a dedicated voting key published in the profile
   envelope. Recommendation: the bound key, unless G-39 objects.

## References

- ADR-0023, 0024, 0046, 0047, 0050, 0052, 0053, 0054, 0060, 0062, 0065, 0074, 0078, 0079,
  0085, 0087
- J. K. Liu, V. K. Wei, D. S. Wong, "Linkable Spontaneous Anonymous Group Signature for
  Ad Hoc Groups", ACISP 2004
- J. Groth, M. Kohlweiss, "One-out-of-Many Proofs: Or How to Leak a Secret and Spend a
  Coin", EUROCRYPT 2015
- S. Noether, B. Goodell, "Triptych: logarithmic-sized linkable ring signatures with
  applications" (2020)
- RFC 9474, RSA Blind Signatures
- The Semaphore protocol (zero-knowledge group membership with nullifiers)
- crates.io `triptych` (Tari project, self-described experimental; audit status unknown)
