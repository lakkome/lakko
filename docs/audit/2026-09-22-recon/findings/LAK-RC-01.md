<!-- SPDX-License-Identifier: AGPL-3.0-or-later -->
<!-- SPDX-FileCopyrightText: 2026 Lakko Contributors -->

# LAK-RC-01 — The delivery service has no authentication, and a group invite link is a permanent read/write capability on that group's journal

- **Severity:** High (finder's proposal; `audit-verify` calibrates)
- **Confidence:** CONFIRMED
- **Workstream:** recon
- **Tracking id:** LAK-RC-01
- **Status:** Open
- **Component:** `services/ds/src/main.rs:146-162`, `services/ds/src/journal.rs:416-434`,
  `crates/lakko-contacts/src/group_invite.rs:78-90`, `apps/web/src/ds/client.ts:191`
- **Invariants touched:** #5 (social graph not materialised on the server), #4 (no fail-open)
- **Vendor claim contradicted:** `docs/security/threat-model.md` asset table,
  *"Social graph | Client-side only"*; and the ADR-0028 framing that
  *"a leaked link lets a stranger ask, never in"*

## Preconditions

The attacker needs **one thing and nothing else**: the 32-byte MLS `group_id` of
the target group. They do not need an account, a session token, a key, or ever to
have been a member. Anyone who has at any point been sent a `/g/` invite link
holds it — including someone whose join request was rejected, and someone the
group later removed.

## Mechanism

**Step 1 — the DS router installs no authentication.**
`services/ds/src/main.rs:146-162` is the entire public router:

```rust
pub(crate) fn build_app(state: AppState) -> Router {
    Router::new()
        .route("/health", get(health))
        .route("/ready", get(ready))
        .route("/version", get(version))
        .route(
            "/ds/group/{group_hex}/messages",
            post(journal::post_group).get(journal::get_group),
        )
        .route(
            "/ds/mailbox/{device_hex}/messages",
            post(journal::post_mailbox).get(journal::get_mailbox),
        )
        .layer(DefaultBodyLimit::max(journal::MAX_MESSAGE_BYTES))
        .layer(TraceLayer::new_for_http())
        .with_state(state)
}
```

There is no `route_layer`, no middleware, no extractor that consults a caller
identity. Confirmed negatively:
`grep -rn "require_session|Authorization|bearer|Bearer|token" services/ds/src/`
returns **zero lines** across the whole service.

**Step 2 — the only check on the path parameter is that it is hex.**
`services/ds/src/journal.rs:140-156` (`decode_pseudonym`) verifies the string is
exactly 64 characters drawn from `[0-9a-f]` and decodes 32 bytes. The handler at
`:416-434` then fetches straight from the database:

```rust
pub(crate) async fn get_group(
    State(state): State<AppState>,
    AxumPath(group_hex): AxumPath<String>,
    Query(q): Query<FetchQuery>,
) -> Result<Response, (StatusCode, String)> {
    let owner = decode_pseudonym(&group_hex)?;
    …
    let resp = fetch_with_wait(&state.db, &state.notifiers, Queue::Group, &owner, q.after, limit, wait).await?;
```

Possession of the pseudonym *is* the authorisation.

**Step 3 — the pseudonym is the raw MLS group id, unhashed.**
`apps/web/src/ds/client.ts:191` builds the endpoint as

```ts
const endpoint = `/ds/group/${pseudonymHex(groupId)}/messages`;
```

and `pseudonymHex` (`:45-52`) is a plain lowercase-hex encoder — no derivation, no
domain separation, no keyed hash. Contrast the deliberate indirection the same
project built for push: ADR-0042's `push_gid` is `keyed-BLAKE3(secret, …)`
precisely so the routing id is *not* the journal key. The DS never received that
treatment.

**Step 4 — the group id is published in invite links.**
`crates/lakko-contacts/src/group_invite.rs:78-90`:

```rust
pub struct GroupInviteCapability {
    /// The target group's opaque MLS `GroupId` bytes …
    pub group_id: Vec<u8>,
```

An owner or admin mints this, base64url-encodes it and shares it as
`https://lakko.me/g/<slug>`. Every recipient of that URL — forwarded, screenshotted,
pasted into another chat, or simply kept after a rejection — permanently holds the
DS journal key for that group.

**Step 5 — the capability is not revocable.** ADR-0028's OQ1 blob ledger can
revoke the *pointer link*, and `group_revoke_canonical_bytes` lets an issuer delete
the discovery blob. Neither changes the `group_id`, which is fixed for the life of
the group and is the only thing the DS consults. MLS has no group-id rotation.

## Reproduction

Against any deployment, with only a `/g/` link in hand:

```
# 1. decode the link slug (base64url) and read out the group_id field
# 2. read the entire journal, oldest first, without any credential:
curl "https://lakko.me/ds/group/<group_id_hex>/messages?after=0&limit=100"
# 3. long-poll for new traffic in real time:
curl "https://lakko.me/ds/group/<group_id_hex>/messages?after=<n>&wait=25"
# 4. write into it (accepted up to MAX_MESSAGE_BYTES = 1 MiB, journal.rs:43):
curl -X POST "https://lakko.me/ds/group/<group_id_hex>/messages" --data-binary @junk
```

I did not execute this — there is no deployment I am permitted to touch and the
engagement is read-only. Every step above is settled by the code cited, which is
why the confidence is CONFIRMED rather than PLAUSIBLE: no runtime observation is
needed to establish that a router with no auth layer performs no auth.

## Impact

**What breaks.**

- *Metadata, continuously and in real time.* Every message's arrival time, size
  and ordering; the exact count of messages ever sent; the cadence of membership
  changes (commits are visibly different in size and rhythm from application
  messages); when the group is active and when it is dormant. The committer
  beacons ADR-0045 emits every 60 s from each admin turn this into a per-admin
  "app was foregrounded at T" feed. This is the social-graph-adjacent data
  invariant #5 exists to deny, available to anyone with a URL.
- *Retention.* The journal holds 30 days, so a newly-acquired link yields a
  month of history immediately.
- *Write access.* Up to 1 MiB per POST, unbounded in count. At minimum this is
  storage exhaustion and a poll-loop burden on every member. Whether an injected
  row can do worse than be dropped depends on each client's receive-loop error
  handling, which I did not review — see Notes.
- *The property ADR-0028 states is narrower than it reads.* "A leaked link lets a
  stranger ask, never in" is true of *membership*. It is not true of *the
  journal*, and the journal is where the metadata lives.

**What does not break.** Message confidentiality. Every journal row is an MLS
ciphertext and this finding gives no key material. An attacker reads envelopes and
lengths, not content. Integrity of real messages is likewise untouched — an
injected row is not a valid MLS message and will not authenticate.

## Recommendation

Two fixes, and they are complementary rather than alternatives.

**(a) Stop using the group id as the journal key.** Route the DS on a derived
pseudonym the way push already is: `journal_id = keyed-BLAKE3(journal_secret, …)`
with the secret distributed in-group over an MLS application message, exactly the
ADR-0042 pattern, including its rotation-on-removal (ADR-0043) and mint-epoch
ordering (ADR-0044). This is the structural fix: it makes a leaked link convey
what it was designed to convey and nothing more, it makes the capability
revocable by rotation, and it reuses machinery the project has already built,
reviewed and tested. The cost is a flag day and a new failure mode for a member
who has not yet received the secret — both of which ADR-0042 has already worked
through once.

**(b) Authenticate the DS.** Require the session token the other services already
require. This is much cheaper and closes anonymous access, but it does not close
*member* access after removal, since an ex-member keeps a valid account, and it
moves the question to "can the server tell who may read this journal" — which
under invariant #5 the server must not be able to answer. So (b) is a mitigation
and (a) is the fix.

Whichever is chosen, the write path deserves its own bound independently: a
per-pseudonym rate limit and a total-bytes cap would blunt the exhaustion case
without any protocol change.

## Notes

- `services/channels` has the identical shape and is filed separately as
  **LAK-RC-02** because its consequence is different (permanent loss of
  decryptability rather than metadata exposure). A verification pass may prefer to
  group them under one root cause — *authorisation by pseudonym possession, with
  pseudonyms that are published* — and I would not object.
- The mailbox surface (`/ds/mailbox/{device_hex}/messages`) has the same absent
  authentication but a different exposure, because the device pseudonym is not
  published in a link. I did not assess how discoverable a device pseudonym is and
  make no claim about it.
- **I did not review any client's receive loop**, so I do not claim an injected
  row does anything beyond consuming bandwidth and storage. If some client treats
  a malformed row as fatal rather than skipping it, the severity rises; ADR-0040
  and ADR-0045 both describe catch-and-continue handling, which suggests it does
  not, but I did not verify that and it should not be assumed.
- ADR-0045 states the premise of this finding in passing, as an argument against a
  different design: *"the DS has no auth and the pseudonym IS the `group_id`"*,
  offered there as a reason a DS-side epoch gate would hand a link-holder a
  group-freeze primitive. The consequence was correctly identified for that one
  hypothetical feature and not carried back to the surface that already exists.
