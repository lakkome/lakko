<!-- SPDX-License-Identifier: AGPL-3.0-or-later -->
<!-- SPDX-FileCopyrightText: 2026 Lakko Contributors -->

# LAK-RC-02 — Channel rotation is unauthenticated; a stranger can permanently destroy every subscriber's ability to decrypt

- **Severity:** High (finder's proposal; `audit-verify` calibrates)
- **Confidence:** CONFIRMED
- **Workstream:** recon
- **Tracking id:** LAK-RC-02
- **Status:** Open
- **Component:** `services/channels/src/api.rs:1015-1044` (router),
  `:765-811` (`rotate_channel`), `:479-497` (`list_subscribers`),
  `services/channels/src/sql_store.rs:531-611` (`rotate`)
- **Invariants touched:** #4 (fail-closed), C-42 (atomic rotation)
- **Vendor claim contradicted:** ADR-0031, *"a post whose epoch seed is absent
  renders an 'older key window — no longer available' placeholder + is skipped,
  never force-decrypted"* — stated as a graceful-degradation property for an
  offline subscriber, not as something an unauthenticated stranger can induce

## Preconditions

The attacker needs the 16-byte channel id, and nothing else. No account, no
session token, no subscription, no key. The channel id is resolvable from the
public `@handle` through `POST /discovery/channel/lookup`, and it is in the URL of
every channel operation any subscriber's client performs.

## Mechanism

**Step 1 — the channels router installs no authentication.**
`services/channels/src/api.rs:1015-1044` builds the whole router with
`.with_state(state)` and no `route_layer`, no middleware, no auth extractor.
Confirmed negatively:
`grep -rn "require_session|AuthenticatedAccount|Authorization|Bearer" services/channels/src/`
returns **zero lines**. That covers `POST /v1/channels` (create),
`POST /v1/channels/{id}/posts` (publish), `GET/POST .../subscribers`,
`.../subscribers/{pk}/envelope`, and all three rotation routes.

**Step 2 — the rotate handler authorises nothing.**
`services/channels/src/api.rs:765-776`:

```rust
async fn rotate_channel(
    State(state): State<Arc<AppState>>,
    Path(channel_id): Path<String>,
    Json(req): Json<RotateRequest>,
) -> Response {
    let Ok(channel) = ChannelId::from_hex(&channel_id) else { … };
```

The only inputs are the path id and the body. There is no publisher key, no
signature over the rotation, and nothing tying the caller to the channel's
`PublisherGroup`. The handler validates entry shapes and calls
`store.rotate(channel, batch, EXPECTED_ENVELOPE_LEN)` at `:811`.

**Step 3 — rotate bumps the epoch and prunes old envelopes, in one transaction.**
`services/channels/src/sql_store.rs:584-609`:

```rust
let new_epoch = current.saturating_add(1);
for (sub, env) in batch {
    sqlx::query("INSERT INTO channel_envelopes … ON DUPLICATE KEY UPDATE envelope = VALUES(envelope) …")
        .bind(channel.as_bytes().as_slice()).bind(sub.as_bytes().as_slice())
        .bind(new_epoch).bind(&env).execute(&mut *tx).await…;
    prune_envelopes(&mut *tx, channel, sub, self.envelope_retention_epochs).await?;
}
sqlx::query("UPDATE channels SET current_epoch = ? WHERE id = ?")
```

The two guards that exist are not authorisation guards: each envelope must be
exactly `EXPECTED_ENVELOPE_LEN = 80` bytes (`api.rs:46`), and each
`subscriber_id` must already be registered (`sql_store.rs:564-578`). Both are
trivially satisfiable — see step 4.

**Step 4 — the subscriber list is readable by the same unauthenticated caller.**
`services/channels/src/api.rs:479-497` (`list_subscribers`) takes only the channel
id and returns every subscriber id as hex. So the attacker obtains exactly the set
of ids the rotate path cross-checks against, from the same service, with the same
absence of credentials.

**Step 5 — `prune_envelopes` evicts the real seeds.** ADR-0031 retains the
highest `LAKKO_CHANNELS_ENVELOPE_RETENTION_EPOCHS` envelopes per subscriber,
default 4. Each attacker rotation files an 80-byte garbage envelope at the new
epoch for every subscriber and prunes to the highest N. After N rotations, every
retained envelope for every subscriber is attacker-supplied garbage and every
genuine seed has been pruned out of the table.

## Reproduction

```
# 1. resolve the channel id from the public handle
curl -X POST https://lakko.me/api/channels/../discovery/channel/lookup …   # or read it from any client URL

# 2. enumerate subscribers — no credential
curl "https://lakko.me/api/channels/v1/channels/<cid>/subscribers"
#   -> {"subscribers":["<64 hex>", …]}

# 3. rotate, four times, with 80 bytes of garbage per subscriber
for i in 1 2 3 4; do
  curl -X POST "https://lakko.me/api/channels/v1/channels/<cid>/rotate" \
       -H 'content-type: application/json' \
       -d '{"envelopes":[{"subscriber_id":"<hex>","envelope":"<base64 of 80 zero bytes>"}, …]}'
done
```

Not executed — read-only engagement, and no deployment I may touch. Each step is
settled by the cited code.

## Impact

**What breaks.**

- *Every subscriber permanently loses the ability to decrypt the channel.* Each
  `ChannelPost` is AEAD-sealed under its own epoch's seed and carries the
  `epoch_id` in its clear header; a subscriber decrypts by looking up that epoch
  in its local `epoch → seed` map. After the attack, the server holds no genuine
  seed envelope at any retained epoch. New subscribers can never bootstrap.
  Existing subscribers keep whatever seeds they already cached locally, so they
  retain posts made under epochs they had already fetched — but everything from
  the attack forward is unreadable, and any subscriber who reinstalls or restores
  is fully locked out.
- *The recovery path is not client-side.* The publisher must notice, then re-seal
  and re-fan-out. Nothing in the server refuses the attacker from immediately
  rotating again, so the attacker wins any race indefinitely at negligible cost.
  This is denial of service recoverable only by an out-of-band action, and only
  for as long as the attacker chooses to stop.
- *`POST /v1/channels` is also unauthenticated,* so channel-id squatting and
  unbounded channel creation are available on the same surface.

**What does not break.** Confidentiality and authenticity of real posts. Posts
are Ed25519-signed by the publisher and sealed under a seed the attacker cannot
derive, so no forged post will verify and no existing post becomes readable. This
is an availability and integrity-of-service break, not a confidentiality break —
which is why the proposed severity is High and not Critical.

## Recommendation

**The rotation and publish paths need publisher authentication.** The material is
already there: a channel has a `PublisherGroup`, and every post already carries an
Ed25519 signature the subscribers verify. Requiring the same key to sign a
rotation request — over `(channel_id, target_epoch, hash of the envelope batch)` —
gives the server a check it can perform while remaining opaque to the content, and
costs no new key management. The server would store the publisher verifying key at
channel creation, which is a public artefact and so is consistent with
invariant #2.

**`list_subscribers` should not be an anonymous read.** Independently of the
above: it enumerates the subscriber set of a channel to anyone who asks, which is
a social-graph disclosure in its own right (invariant #5) even with rotation
fixed.

**Bound the epoch.** `current.saturating_add(1)` with no ceiling and no rate limit
means the epoch can be marched arbitrarily far by anyone. A per-channel rotation
rate limit is a cheap independent mitigation that does not wait for the
authentication work.

## Notes

- Same root cause as **LAK-RC-01** — authorisation by possession of a pseudonym
  that is published — and a verification pass may reasonably group them. I filed
  them separately because the consequences differ in kind: LAK-RC-01 leaks
  metadata and LAK-RC-02 destroys availability of content.
- I did **not** check the two-phase `rotate/stage` + `rotate/commit` path
  (ADR-0030) for the same exposure. It is on the same unauthenticated router, and
  `storage.rs:436-438` describes `commit_rotation` as bumping `current_epoch` the
  same way, so the same attack very likely applies through it as well — but I did
  not read those handlers and do not assert it.
- I did not verify the deployed value of
  `LAKKO_CHANNELS_ENVELOPE_RETENTION_EPOCHS`; the default is 4
  (`CLAUDE.md`, `services/channels` row) and the floor is 1. A smaller value makes
  the attack cheaper, not more or less possible.
- ADR-0031's graceful-degradation behaviour — skip the post, show a placeholder,
  never force-decrypt — is correct and is what stops this being worse. It was
  designed for an honest subscriber who went offline across a rotation; it also
  happens to be the failure mode a hostile rotation produces.
