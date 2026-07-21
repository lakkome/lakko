<!-- SPDX-License-Identifier: AGPL-3.0-or-later -->
<!-- SPDX-FileCopyrightText: 2026 Lakko Contributors -->

# Recovery + KT-log live e2e

Closes the deferred "live 2-device e2e (no deployed environment)" residual of
ADR-0016 / 0017 / 0018 + G-30 #2 (ADR-0019) at the **server + crypto** layer: a
dockerised `discovery` + `transparency` stack plus a host-side driver that runs
the full **publish → genesis-log-append → fetch-verify → recovery →
rotation-chain** pipeline with the REAL client-side crypto.

The driver is `services/discovery/tests/recovery_kt_e2e.rs` (`#[ignore]`-gated).

## What it proves

1. **Genesis publish + log-append** — mint a session token, solve the PoW,
   derive an `Identity`, mint a genesis self-sig (`sign_leaf_genesis`), upload
   to discovery, and poll until the transparency log commits the entry.
2. **Fetch-verify (genesis)** — `verify_inclusion` (operator-root sig + Merkle)
   + `verify_leaf_genesis` (the served self-sig authorises exactly the served
   key).
3. **Recovery (same key)** — a same-key re-publish is idempotent; the log
   version does not advance.
4. **Rotation + chain** — a new key + a supersession proof, verified by
   `verify_supersession_chain` against the live log (one committed, strictly-
   newer version under a no-rollback root).
5. **Negative (C-56)** — a new-key binding with NO `leaf_auth` is refused at the
   transparency append (`LAKKO_TRANSPARENCY_REQUIRE_LEAF_AUTH=1`), so it never
   enters the log.
6. **K-of-N witness quorum (ADR-0020 + ADR-0039)** — `GET /transparency/root`
   carries a BUNDLE of witness co-signatures (`witness_cosigs_b64`) from the
   three independent witnesses (`witness` / `witness2` / `witness3`). The driver
   pins all three keys, asserts the bundle carries >= K=2 cosigs (each verifying
   via `verify_witness_cosig` against a pinned key), and that the **2-of-3
   quorum** verifies (`verify_witness_quorum`) over the served
   `(tree_root, log_size)`; the operator signature verifies too. The NEGATIVE:
   pinning only ONE of the three keys but demanding a threshold of 2 fails closed
   (a single witness cannot fake a quorum). This proves the transparency ->
   N-witness fan-out round-trip live: the operator cannot serve a root fewer than
   K independent witnesses have co-signed (the K-of-N split-view defence).
7. **Live fork-refusal (ADR-0020 split-view defence)** — the driver reconstructs
   the operator SIGNING key from the container's fixed seed
   (`LAKKO_TRANSPARENCY_OPERATOR_SEED_HEX`), mints a VALIDLY operator-signed but
   FORKED root (the same `log_size` as the witness's last-co-signed head, a
   different `tree_root`), and POSTs it directly to `POST /witness/cosign`. The
   live witness REFUSES it with **409** (`CosignError::NotConsistent`) — proving
   against the real service that the honest witness will not co-sign two
   mutually-inconsistent roots even with a valid operator signature. A root with
   an INVALID operator signature is refused with **400** at the operator-sig gate.

## Scope

- **Server + crypto only.** The driver exercises the
  discovery → transparency → fetch → verify pipeline with the real crypto
  primitives. It does NOT drive the three client RUNTIMES (web / iOS / Android)
  — that needs browser + simulator harnesses (the `g3-4-mobile.yml` shape) and
  stays deferred.
- **No auth / OPAQUE.** Discovery's upload is gated by a shared-secret session
  token (`lakko-auth-token`, `LAKKO_SESSION_TOKEN_SECRET`), so the driver mints
  it directly — the auth service is not in the stack.
- **Operator key fetched at runtime; witness pins a fixed-seed fixture.** The
  driver fetches the operator key from `GET /transparency/operator-key` and the
  witness key from `GET /witness/key` at runtime (a production client PINS both
  out-of-band; for this server-side test, fetching is correct — it validates the
  log mechanics, not the pin-security). The transparency container runs with a
  FIXED operator seed (`LAKKO_TRANSPARENCY_OPERATOR_SEED_HEX = 0xAB×32`) so the
  witness can PIN the derived verifying key via `LAKKO_WITNESS_OPERATOR_PUBKEY`
  (the `fixed_seed_env_override_derives_deterministic_key` test pins the value);
  this mirrors the sfu-e2e fixture-seed pattern.
- **The witnesses are co-located here; production needs separate trust domains.**
  ADR-0020's split-view guarantee — and ADR-0039's K-of-N collusion-resistance —
  hold only when the witnesses run in trust domains DISTINCT from the operator
  and from each other. The e2e co-locates all three (`witness` / `witness2` /
  `witness3`, each its own DB + cosign secret, all pinning the same operator key)
  for testing; a single operator running all N gains nothing against
  operator+witness collusion (ADR-0039 caveat).

## Run it locally

Needs Docker + a Rust toolchain.

```bash
# 1. Bring up the stack (builds the two service images; --wait blocks until
#    both services + both DBs are healthy).
docker compose -f ci/recovery-e2e/docker-compose.yml up --build --detach --wait

# 2. Run the ignored driver against the published ports.
cargo test --package lakko-svc-discovery --test recovery_kt_e2e \
  -- --include-ignored --nocapture

# 3. Tear down (drop the volumes so the next run starts clean).
docker compose -f ci/recovery-e2e/docker-compose.yml down --volumes
```

The driver uses a run-unique `account_id` each invocation, so a re-run without
`down --volumes` still passes (it never collides with a prior run's log state).

## Config

Defaults match the committed compose; override via env when pointing the driver
at a non-default stack:

| Env var | Default | Meaning |
|---|---|---|
| `LAKKO_RECOVERY_E2E_DISCOVERY_URL` | `http://127.0.0.1:8084` | discovery base URL |
| `LAKKO_RECOVERY_E2E_TRANSPARENCY_URL` | `http://127.0.0.1:8087` | transparency base URL |
| `LAKKO_RECOVERY_E2E_WITNESS_URL` | `http://127.0.0.1:8088` | witness 1 base URL (ADR-0020) |
| `LAKKO_RECOVERY_E2E_WITNESS2_URL` | `http://127.0.0.1:8089` | witness 2 base URL (ADR-0039 quorum) |
| `LAKKO_RECOVERY_E2E_WITNESS3_URL` | `http://127.0.0.1:8090` | witness 3 base URL (ADR-0039 quorum) |
| `LAKKO_RECOVERY_E2E_SESSION_SECRET` | `base64(32× 0x5e)` | session-token secret (must match the discovery container's `LAKKO_SESSION_TOKEN_SECRET`) |

The compose sets the discovery container's PoW difficulty to `0`
(`LAKKO_DISCOVERY_POW_EFFORT_BITS`) and the reconciler interval to `2 s`
(`LAKKO_DISCOVERY_TRANSPARENCY_RECONCILE_SECS`) so the host-side driver solves
instantly and the log catches up within a poll cycle.

## CI

`.github/workflows/recovery-e2e.yml` runs it on `workflow_dispatch` (the
`recovery-e2e` PR-label gate + nightly cron are commented out for now, matching
`sfu-e2e.yml`'s quota-conservation posture).
