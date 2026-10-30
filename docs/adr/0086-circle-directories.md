<!-- SPDX-License-Identifier: AGPL-3.0-or-later -->
<!-- SPDX-FileCopyrightText: 2026 Lakko Contributors -->

# ADR-0086: Circle directories: finding people through contacts, groups and channels at any scale

- **Status:** Proposed (2026-10-30). The maintainer chose this mechanism over
  private information retrieval and over a hardware enclave the same day. Open
  question 1 needs its own ADR. Nothing is built.
- **Date:** 2026-10-30
- **Context:** the scale requirement in `CLAUDE.md`: users potentially on par with
  Telegram or WhatsApp.
- **Supersedes:** [ADR-0060](0060-scoped-discovery.md) section 4 (the card and
  bundle relay) and the rotation rules of its section 3 for groups and channels.
  The setting, the mailbox gate, Everyone and the rest of ADR-0060 stand.
- **UX spec:** [`docs/ux/who-can-find-me.md`](../ux/who-can-find-me.md)

## Context

ADR-0060 section 4 finds contacts of contacts by relay: Anna sends a sealed record
to every contact, and each contact sends a padded bundle of every record it holds
to all of its contacts, again whenever any record changes. Records rotate weekly,
so a person with 150 contacts sends about 22,500 bundles a week if the app is
always open. A bundle is about 370 KB by extrapolation from one measured 23 KB
row. That is 390 MB to 8 GB a week per person. Groups and channels fail the same
requirement another way: every shrink makes every remaining member rotate and
republish, and every channel epoch makes every subscriber re-upload a card, while
a client reads only the first 1,000 cards of a channel.

Measured against the requirement, four things must hold:

- Traffic for this feature is kilobytes in normal use, and nothing moves because
  of what someone else did.
- Nothing depends on another person's phone being online, and a search wakes
  nobody.
- A person removed from a contact list, or who leaves a group or channel, loses
  access.
- The server does not learn who is whose contact (invariant #5), and sees no
  plaintext names (invariant #1).

The study behind this ADR compared private information retrieval, enclaves on one
and on several providers, and this design. Its figures are derived from published
numbers and stated assumptions; nothing was run. It is summarised under
Alternatives.

## Decision

**A person's contacts are found through one small encrypted directory that person
keeps on the server. Other people's phones hold a compact filter of it and read
one row on a hit. Revocation is a read key that changes, not data that moves.**

Worked example: Bob and Katya are contacts, Katya and Anna are contacts, Bob and
Anna are not. Bob types `anna`.

### 1. The offer

Anna's device computes the OPRF output of each of her search terms once (one
anonymous token each, kept, ADR-0060 section 5) and sends each contact it allows
(`allows_contact`) an offer in their one-to-one conversation: her card and the
term tags `BLAKE3-derive-key("lakko/fof/tag/v1", oprf_output)`.

- The card is what the record payload is today, with the reach seed of section 5.
- The terms are the username and, for each word of the name, its prefixes of 3 to
  6 letters and the whole word when it is longer. A query longer than the longest
  prefix asks with that prefix, and the phone filters the opened cards by the whole
  query. This replaces prefixes of 3 to 16 letters.
- An offer goes once per contact and again only when Anna's name, username or
  grant for that contact changes.

### 2. The directory

Katya's device keeps one directory on the Delivery Service, under an id derived
from her account seed:

- **Header**, readable by anyone holding the id, opaque: the epoch, the key tree of
  section 3, and the chain of older epoch keys.
- **Filter**, sealed under the current epoch key: a static retrieval filter of the
  binary fuse or ribbon family, mapping `BLAKE3-keyed(filter key, term tag)` to a
  row index, at about 3 bytes per term.
- **Rows**, each a card sealed under its own key. A row key is wrapped under the
  epoch key current when the row was written.

The filter is rebuilt when a contact joins or a card changes, and only the filter
and the changed row are uploaded. A directory reveals its size in buckets, as a
bundle does today.

### 3. Who can read: a read key and a key tree

Read access is the ADR-0053 model: the directory registers one Ed25519 public key
per epoch, and a read of the filter or a row needs a proof by the current key. A
stale proof is the uniform `401`.

The epoch key reaches members through a one-way key tree over Katya's contacts
(Canetti et al. 1999). Each contact holds one leaf key, given once in the one-to-one
conversation; the path keys are in the header.

**Katya removes Bob:**

1. Her device derives a new epoch key and writes log2 N wrapped path keys into the
   header, with one wrap of the previous epoch key for the chain. That is about
   1 KB at N = 10,000, a single write.
2. She registers the new read key. **From that moment Bob's reads are refused.**
3. No row and no filter moves. Everyone else does nothing until their next read,
   when a refused proof sends them to the public header for the new key, about
   0.5 KB.

Rows from before the removal stay wrapped under the older epoch, and the chain lets
a contact who joined later open them. A row written after the removal is under a
key Bob does not have. An operator colluding with Bob can open what Bob could read
when he was removed, and nothing written after it.

### 4. The search

Bob's phone holds a filter per contact.

1. It gets the OPRF output of `anna`: one request, one anonymous token.
2. It tests every held filter locally. No network per circle.
3. On a hit it reads that row, with the read proof of that circle.
4. It opens the card, filters by the whole query, runs the reach check and shows
   Anna.

At most 20 rows are read per search (chosen, not measured). A query that hits more
says so, and asks for more letters.

**Keeping filters fresh:** a device reads the header of a directory when it needs
it, in the foreground, a budgeted number per session (20, chosen). There is no push
and no background work. A stale filter costs a missed or a refused result, never a
wrong one, and the search says nothing about it. On a new device directories load
over the first sessions, most used contacts first.

**The 150 most used contacts (decided 2026-10-30).** A device keeps the filters of
its 150 most used contacts current. The rest load gradually, in batches inside the
session budget, so a person with 10,000 contacts reaches full coverage over time
rather than in one 30 MB load. Their contacts are not searched until their filter
has arrived.

Version checks are the largest load at scale, about 750,000 reads a second at peak
at a billion accounts (derived). From about 100 million accounts they move onto
ADR-0055 notices or ride one-to-one messages.

### 5. Revocation on the reached side: one reach key per circle

The `fof` reach key stops being one key shared by all contacts. Anna registers one
reach key per contact she allows, in size buckets, and puts that key in the card she
offers to that contact. This resolves ADR-0060's open question 1.

- **Anna turns Katya off, or removes her:** one delete on Anna's mailbox. Reach
  checks through Katya fail at once, and nobody else's key changes. The weekly
  rotation and the shared `fof` bump go away, including the one
  `only the contacts I choose` uses today.
- **Widening:** an offer to the new contact.

### 6. Groups: no reach key at all

Search in a group stays local. A contact request to a co-member goes **inside the
group**, as an MLS application message addressed to that person: a person who left
cannot write there, by MLS itself. So there is no `group` or `member` reach key to
rotate, and nobody does any work when someone leaves. A member the setting hides the
person from is refused by the person's client rather than by the server, which is a
change to UX section 10; a co-member can already post to them in the group, so
nothing new is reachable.

### 7. Channels: one card, no epoch

A subscriber's card is stored once, sealed under a stable per-channel member key
handed out with the seed, and read with a subscriber signature. It is not tied to a
post epoch, so it is never re-uploaded. Leaving deletes one's own row. Others do
nothing, at millions of members and constant churn.

A search adds one tag per channel to one request: under 1 KB for 20 channels, about
200 KB for 10,000 (derived). So a search covers the 64 most recently used channels,
and the rest are searched from inside the channel. The rotation of the post seed is
not part of this ADR.

### 8. The relay

A directory read says "someone read circle X". To keep that from naming the reader,
reads go through an **independent relay at another operator**, OHTTP-shaped, so the
Delivery Service sees the relay and the relay sees sealed requests. Timing can still
link reads, at the level journal polling does today (ADR-0078 class c).

The relay is a release gate, not a first-build dependency. Until it exists a read
shows the reader's address to the Delivery Service, recorded as a gap.

### What the setting does

| Setting | Effect |
|---|---|
| Contacts of my contacts, all | an offer to every contact |
| Contacts of my contacts, chosen | an offer to chosen contacts; turning one off deletes that reach key |
| Off | no offers; every `fof` reach key deleted |
| Members of my groups, all or chosen | the grant in the profile as today |
| Subscribers of my channels, all or chosen | the card in those channels |
| Everyone | ADR-0060 section 5, unchanged |

## Cost

Derived from stated assumptions, not measured: 150 contacts, 9 terms, 2 searches a
day per account, 1 removal per 180 days, 1 added contact per 30 days, 200-byte rows.

| | Typical | 10,000 contacts |
|---|---|---|
| Search | 1-2 KB | about 3 KB with channels capped |
| Removal, the remover | about 1 KB | about 1 KB |
| Removal, others | 0.5 KB at next read | same |
| At rest | about 8 KB a day | bounded by the session budget |
| First load | 450 KB, a few directories per session | about 450 KB for the 150 most used, the rest in batches |
| Stored per account | about 61 KB with 2 copies | scales with contacts |

The 1 KB removal is lower than the study's 10 KB because rows are not re-wrapped: the
epoch chain makes that unnecessary, at the cost stated in section 3.

Service cost, EUR a month on Hetzner list prices of 2026-10: 174 at 100 accounts, 174
at 100,000, 217 at 1 million, 4,510 at 100 million, about 36,000 at a billion. The
floor prices the relay as a stand-in: no relay price was found.

## Alternatives

- **Keep the relay of ADR-0060 section 4.** Rejected: 390 MB to 8 GB a week per person.
- **Keyed rows on the server, one per contact and term, written by the person found
  or by their contact.** Rejected by the maintainer: a removal re-uploads megabytes,
  or leaves the removed person with access.
- **Private information retrieval over every circle.** Rejected: 7.4 MB per search at
  150 circles, 1.45 GB at 30,000. PIR hides reads and cannot refuse one, so revocation
  rests on the same keys. It stays possible later for the single row read of step 3,
  and then the reach check must be hidden too.
- **Enclave, one or several providers.** Not available on Hetzner as far as could be
  established; about EUR 2.1 million a month at a billion, driven by an ORAM figure
  nobody has measured at this size; and the graph exists on a server, where a leak
  cannot be rotated. Signal's multi-provider service (SVR3) stored a split secret, not
  a graph, and was removed from libsignal in March 2025 per the study; copying one
  graph to three vendors is less safe than one. Rejected for now.
- **Sharing a contact by hand.** Rejected by the maintainer.

## Consequences

- **Positive:** traffic falls by about four orders of magnitude for a typical person.
  A removal takes effect in seconds and costs the same at any circle size. Nobody's
  phone does work because of someone else's removal, leave or rotation. Groups and
  channels lose their rotation storms. Nothing new is stored in the clear.
- **Negative:** a phone holds a filter per contact and refreshes it in the foreground.
  A stale filter misses people silently. **A person removed from a circle keeps what
  he already fetched**, including a reach key found before the removal, until the
  credential of open question 1 exists. A person with thousands of
  contacts has partial coverage or a heavy first load. The relay is a new dependency
  at another operator. The server sees bucketed directory sizes and read counts.
- **Neutral:** channels are off by default (decided 2026-10-30): the part of the
  setting splits into members of groups, on, and subscribers of channels, off. UX
  section 1.
- **Neutral:** enumeration by prefix stays what it is, bound by tokens; it now also
  needs a member's filter key. The Everyone path does not change.

**Invariants:** #1 and #2 hold: a directory is ciphertext. #5 holds with the relay;
until then the server sees reader addresses per directory. #3 is untouched: the tree
is not MLS and no group state changes. #4 is untouched: a failed read shows no result.
A `lakko-security` pass is due before merge; the key tree and the filter are new
attacker-touchable parsers and get fuzz targets.

## Plan

1. `lakko-circle` crate: the tree, the epoch chain, the filter, the directory codec,
   property tests and fuzz targets.
2. `services/ds`: directory routes and store, read proofs, public header.
3. Both FFI surfaces.
4. Web, then iOS and Android: offers, the directory writer, the filter store, search,
   per-circle reach keys. Remove bundles, the ask and the `fof` rotation.
5. Groups: the in-group request, removal of `group` and `member` keys.
6. Channels: the stable card and the 64-channel search.
7. The relay and the credential of open question 1, both before release.

Flag day: no deployment holds data worth keeping.

## Open questions

1. **What a removed person keeps (decided 2026-10-30: this must be closed, the
   first build ships with it open only as a release gate).** Bob keeps a reach key he
   fetched before Katya removed him, a modified client can use it, and nothing says
   Anna will ever turn Katya off. It is closed by a short-lived anonymous credential
   at send time: Bob proves to the Delivery Service that he is a current member of
   some circle Anna accepts, without saying which, and a removed member stops getting
   new ones. No existing construction fits as is: Signal's keyed-verification
   credentials have one issuer that also verifies, while here each circle is its own
   issuer and the verifier must not learn which, so this is a set-membership proof
   over Anna's accepted issuers. It needs its own ADR, its own implementation in
   Lakko's crates rather than libsignal, and the G-39 review. A row holding a
   time-limited delegation instead of a key was considered and rejected by the
   maintainer.

Also open: the relay operator, and the exact filter construction.

## References

- [ADR-0060](0060-scoped-discovery.md) (who can find me), [ADR-0053](0053-ds-journal-authorisation.md)
  (read proofs), [ADR-0054](0054-anti-abuse-without-client-work.md) (tokens),
  [ADR-0055](0055-websocket-delivery.md) (notices), [ADR-0078](0078-servers-cannot-link-accounts.md)
- Canetti et al., Multicast security: a taxonomy and some efficient constructions, 1999
- Graf and Lemire, Binary fuse filters, 2022
- RFC 9458, Oblivious HTTP
