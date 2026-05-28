<!-- SPDX-License-Identifier: AGPL-3.0-or-later -->
<!-- SPDX-FileCopyrightText: 2026 Lakko Contributors -->

# Test JWKS — fixture material for `ci/sfu-e2e/`

This directory holds the public JWKS the dockerised
`lakko-svc-sfu` container fetches during the G3-4 E2E
gate, plus the fixed seed the test side uses to mint
matching JWTs.

## Files

| File              | Purpose                                                                   |
|-------------------|---------------------------------------------------------------------------|
| `jwks.json`       | Static JWKS served by the nginx sidecar at `http://test-jwks:8080/.well-known/jwks.json`. |
| `README.md`       | This file.                                                                |

## The seed

The JWKS in `jwks.json` is generated from a deterministic
seed: **`0x42` repeated 32 times** (i.e. `[0x42; 32]`).
The matching `kid` is `fef6dfa48b073924` and the
verifying-key base64url is `IVL40Zt5HSRFMkLhXy6rbLfP-ntqXtMAl5YOBpiB2xI`.

The test side hardcodes the same seed in
[`services/sfu/tests/e2e_dockerized.rs`](../../../services/sfu/tests/e2e_dockerized.rs)
so it can construct a `CallSigningKey` that matches what
the SFU is willing to verify. That coupling is
intentional — passing the seed through env vars or
mounted secrets would let CI drift between the two sides
silently. A fixed constant fails loud the moment either
side rotates without coordination.

## Security posture

**This material is test-only.** The seed is checked into
the repository in plaintext (this README) and the
corresponding signing key is therefore _public_. A
production SFU MUST point at a real JWKS endpoint (the
`services/auth` deployment publishes one at
`https://auth.lakko.me/.well-known/jwks.json`) and MUST
NOT load this fixture.

Refusing to deploy this seed to production is structural:
the fixture lives under `ci/` (CI-only path), and the
production Dockerfile (`services/sfu/Dockerfile`) does
not COPY the `ci/` tree — only the workspace's
`Cargo.toml` + `src/` make it into the image.

## Regenerating `jwks.json`

If the JWKS shape published by `Jwk::from_signing_key`
ever changes (new field, different encoding) regenerate
this file via:

```bash
cargo run -p lakko-call-jwt --example gen_test_jwks \
  > ci/sfu-e2e/test-jwks/jwks.json
```

The example binary lives at
[`crates/lakko-call-jwt/examples/gen_test_jwks.rs`](../../../crates/lakko-call-jwt/examples/gen_test_jwks.rs).
