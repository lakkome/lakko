<!-- SPDX-License-Identifier: AGPL-3.0-or-later -->
<!-- SPDX-FileCopyrightText: 2026 Lakko Contributors -->

# Transport-layer security invariants (Phase 5)

Audit-pack subsystem doc for the anti-censorship +
rendezvous transport surface. Phase 5 per
[ADR-0004](../adr/0004-anti-censorship.md); decomposition
in [`docs/ROADMAP-PHASE3-CLOSEOUT.md`](../ROADMAP-PHASE3-CLOSEOUT.md)
group **G5-\***. Sister doc to
[`calls.md`](./calls.md) (call-side crypto) and
[`mls-state-machine.md`](./mls-state-machine.md) (MLS
group state); same audit-pack convention — every
invariant cites a property test or a fuzz target.

The transport layer's invariants get their own `T-N`
namespace so the C-N call-side numbering stays
contiguous as it accumulates.

## Crate layout

| Module | Status | What |
|---|---|---|
| [`crates/lakko-transport/src/transport.rs`](../../crates/lakko-transport/src/transport.rs) | shipped | `Transport` / `Connection` / `Listener` trait surface (the physical wire) |
| [`crates/lakko-transport/src/quic.rs`](../../crates/lakko-transport/src/quic.rs) | shipped | quinn-backed QUIC v1 + 0-RTT |
| [`crates/lakko-transport/src/http.rs`](../../crates/lakko-transport/src/http.rs) | shipped | reqwest+rustls REST client with SPKI pinning |
| [`crates/lakko-transport/src/pluggable.rs`](../../crates/lakko-transport/src/pluggable.rs) | shipped | `PluggableTransport` policy + `HappyEyeballs` selector |
| [`crates/lakko-transport/src/rendezvous.rs`](../../crates/lakko-transport/src/rendezvous.rs) | shipped | Ed25519-signed bootstrap config + sign / verify |
| [`crates/lakko-transport/src/reality.rs`](../../crates/lakko-transport/src/reality.rs) | scaffold (G5-1) | REALITY TLS-in-TLS config + stub transport |
| [`crates/lakko-transport/src/masque.rs`](../../crates/lakko-transport/src/masque.rs) | scaffold (G5-2) | MASQUE / CONNECT-UDP config + stub transport |
| [`crates/lakko-transport/src/shadowsocks.rs`](../../crates/lakko-transport/src/shadowsocks.rs) | scaffold (G5-3) | Shadowsocks-2022 config + stub transport |
| [`crates/lakko-transport/src/webtransport_pt.rs`](../../crates/lakko-transport/src/webtransport_pt.rs) | scaffold (G5-4) | WebTransport config + stub transport |

## Threat-model context

Transport-side adversary in Lakko's threat model:

- **Censor on path** sees ciphertext + packet metadata
  (size, timing). Goal: drop or distinguish Lakko's
  traffic from background flow.
- **Active probe** drills the apparent rendezvous host
  with handshake variants to fingerprint. Goal: confirm
  it's a Lakko endpoint so it can be blocked.
- **Rendezvous-server compromise** — a malicious or
  coerced rendezvous can serve an attacker-crafted
  bootstrap config that points clients at honeypot
  transports.

The pluggable + rendezvous surfaces defend against each
of these three. Server-side TLS termination + SFU /
DS / channels / auth opacity are out of scope of THIS
file — they live in [`calls.md`](./calls.md) §"What the
SFU sees" and [`auth-flow.md`](./auth-flow.md).

## Invariants the transport layer MUST preserve

### T-1: Pluggable-kind wire identifiers are frozen at v1

[`PluggableKind`](../../crates/lakko-transport/src/pluggable.rs)
enumerates the five transport classes Lakko's
rendezvous + selector know about. Each variant has a
**stable lowercase string identifier** returned by
`PluggableKind::as_str()`:

| Variant | Wire string |
|---|---|
| `Quic` | `"quic"` |
| `Reality` | `"reality"` |
| `Shadowsocks2022` | `"shadowsocks-2022"` |
| `WebTransport` | `"webtransport"` |
| `Masque` | `"masque"` |

These strings are the discriminator the rendezvous JSON
config (`TransportSpec`) tags transport entries with.
**Renaming any of them is a breaking protocol change**:
every already-pinned bootstrap config the client side
holds in its trust store would silently mis-route.

Test: `pluggable_kind_string_table_pinned` in
`pluggable.rs::tests` — explicit byte-string equality
for every variant.

### T-2: Scaffold transports surface `NotImplemented` loudly

The four PT scaffolds (REALITY / MASQUE / Shadowsocks-
2022 / WebTransport) return
[`crate::error::Error::NotImplemented { feature, sprint }`](../../crates/lakko-transport/src/error.rs)
from their `Transport::connect` impl. The error message
carries the sprint label (`S-5-reality`, `S-5-masque`,
…) so an operator log line points straight at the open
ROADMAP item.

This is structural: a build that accidentally enables a
scaffold transport in production fails at the first
connect attempt with a typed error, never falls back to
unauthenticated transport, never crashes silently.

The `Pluggable` selector wraps these per-candidate
errors inside
[`PluggableError::AllFailed::per_candidate`](../../crates/lakko-transport/src/pluggable.rs)
so the operator can see "every candidate is still a
stub" at a glance.

Test: `scaffold_transport_surfaces_not_implemented` in
`pluggable.rs::tests`.

### T-3: Selector race preserves submission order

[`HappyEyeballs::new`](../../crates/lakko-transport/src/pluggable.rs)
takes a `Vec<PluggableCandidate>` and races them in
submission order. The race is sequential in the current
scaffold — one candidate's outer-`tokio::time::timeout`
slice of the deadline, then the next — so the
bootstrap config's priority list is honoured exactly
as the server signed it.

A future concurrent (true happy-eyeballs) refactor MUST
preserve "first success wins". The first candidate to
return `Ok` shorts the rest; the
[`PluggableTransport::connect`](../../crates/lakko-transport/src/pluggable.rs)
return type is `(PluggableKind, Box<dyn Connection>)` so
the caller knows which kind won and can record the
choice in metrics + the user-facing connection-detail
panel.

Test: `candidate_kinds_in_race_order_preserved` in
`pluggable.rs::tests`.

### T-4: Rendezvous verify is a 4-stage gate

[`BootstrapConfig::verify`](../../crates/lakko-transport/src/rendezvous.rs)
enforces four checks **in this order** before returning
`Ok(())`:

1. **Schema version equals `SCHEMA_VERSION`** (currently
   `1`). A forward-incompatible bump is a hard reject:
   the client refuses rather than try to parse new
   fields it does not understand. Stops a coerced
   rendezvous from silencing a client by pushing a
   not-yet-supported config shape.
2. **`expires_at > now`** (`RendezvousError::Expired`).
3. **`issued_at <= now + ISSUED_AT_CLOCK_SKEW_SECS`**
   (currently 1 hour;
   `RendezvousError::IssuedInFuture`). Two-sided
   bound — too-future configs are rejected on the
   theory that either the server clock is wrong or
   the attacker is replaying.
4. **Ed25519 verify** against the trust anchor over the
   canonical bytes (T-5). Failure produces
   `RendezvousError::SignatureRejected`.

The four checks produce four distinct error variants
so the caller can branch on "fetched a stale cache"
vs "the rendezvous endpoint is compromised"; the wire
error surface is part of the public contract.

Tests: `verify_rejects_expired`,
`verify_rejects_issued_in_future`,
`verify_rejects_version_mismatch`,
`verify_rejects_wrong_trust_anchor`,
`verify_rejects_tampered_payload`,
`verify_tolerates_minor_clock_skew`,
`skew_window_boundary` in `rendezvous.rs::tests`.

### T-5: Canonical bytes = JSON with `signature_hex` zeroed

The Ed25519 signature covers
`serde_json::to_vec(&self)` **with `signature_hex` set
to `String::new()`**. The signer
([`BootstrapConfig::sign`](../../crates/lakko-transport/src/rendezvous.rs))
and the verifier
([`BootstrapConfig::canonical_bytes`](../../crates/lakko-transport/src/rendezvous.rs))
use the same code path; that one source of truth is the
audit-pack guarantee.

`serde` preserves struct field order on re-serialise,
so a parsed `BootstrapConfig` round-tripped through
JSON re-produces the same canonical bytes the signer
saw. The test
`json_round_trip_preserves_canonical_bytes` makes the
property visible end-to-end (encode → parse → verify
still green).

Tamper detection covers any field except
`signature_hex` itself: modifying a `TransportSpec` (or
any of its inner config fields) after signing makes
the canonical bytes differ → Ed25519 verify
returns `SignatureRejected`.

Test:
- `sign_then_verify_round_trip` (happy path)
- `verify_rejects_tampered_payload` (modify a
  RealityConfig's `sni` after signing → rejected)
- `json_round_trip_preserves_canonical_bytes` (the
  encoder-decoder symmetry property)

### T-6: TransportSpec kebab-case kind is part of the protocol surface

[`TransportSpec`](../../crates/lakko-transport/src/rendezvous.rs)
is tagged via `#[serde(tag = "kind", rename_all =
"kebab-case")]`. The kind strings inside the rendered
JSON match T-1 exactly:

```json
{ "kind": "reality", ... }
{ "kind": "shadowsocks-2022", ... }
{ "kind": "webtransport", ... }
{ "kind": "masque", ... }
```

The discriminator on the rendezvous wire is the same
string `PluggableKind::as_str()` produces — a single
source of truth.

Test: `transport_spec_kebab_kind_pinned` (asserts the
JSON-serialised form carries `"kind":"shadowsocks-2022"`
literally).

### T-7: Trust anchor is binary-pinned

The Ed25519 public key
[`BootstrapConfig::verify`](../../crates/lakko-transport/src/rendezvous.rs)
checks against is **not** fetched from the network. It
ships in the client binary (TUF / app bundle). Compromise
of the signing key requires a new build with a rotated
trust anchor — the rotation procedure is tracked in
[`known-gaps.md`](./known-gaps.md) until the TUF
auto-update sprint (GA-5) lands.

Network-resident attackers can substitute a malicious
rendezvous response but cannot mint a signature that
verifies against the pinned key.

This is a **structural** invariant — it lives in the
caller's wiring rather than inside the rendezvous
module — so the test for it is the absence of any
`reqwest::get` / network fetch path inside
`rendezvous.rs`. A future PR that introduces a
network-resolved trust anchor MUST update this
section.

### T-8: DoH fetcher preserves the rendezvous trust chain end-to-end

Sprint G5-5-doh adds the client-side bootstrap fetcher
in `crates/lakko-transport/src/doh_fetch.rs`. The
module rides four hardcoded JSON-DoH endpoints
(Cloudflare / Google / Quad9 / AdGuard) — a happy-
eyeballs first-success-wins resolve over the
`DOH_ENDPOINTS` constant — then a separately-pinned
HTTPS GET against the resolved IP for the signed
config blob.

**Pinned invariants:**

1. **The signature is the cryptographic backstop, not
   the CA chain.** A censor controlling the recursive
   DNS path can spoof `bootstrap.lakko.me` to point at
   their own host; the payload signature check at the
   bootstrap-fetch stage catches that — they cannot
   forge an Ed25519 signature without the publisher
   key (T-7's binary-pinned trust anchor). A censor
   controlling a public CA could mount a valid TLS
   interception against the DoH endpoint itself; the
   signature check at the bootstrap-fetch stage
   remains the cryptographic backstop. The DoH legs
   use the embedded Mozilla CA bundle
   (`rustls-tls-webpki-roots`); Lakko's own API trust
   (auth / ds / storage / channels) continues to use
   SPKI pinning via [`PinnedVerifier`] — the two
   CA-trust roots are intentionally independent.

2. **HTTPS leg pins the resolved IP into reqwest's
   resolver via `Client::builder().resolve(...)`.**
   Once a DoH endpoint resolves the domain, the
   HTTPS GET targets the resolved IP directly (Host
   header + SNI still carry the original domain
   string so the TLS cert validates). A censor that
   contests a recursive DNS lookup *after* the DoH
   step cannot redirect the HTTPS fetch.

3. **`DohFetcher::fetch_bootstrap` always runs the
   4-stage rendezvous verify (T-4).** No partial-
   acceptance path exists — the function returns
   `DohError::Rendezvous` if any of schema-version /
   expiry / issued-at clock-skew / Ed25519 stages
   fail. There is no fallback to an unsigned
   transport list.

4. **Per-leg timeout is bounded at `PER_LEG_TIMEOUT`
   (10s).** With 4 endpoints the cold-start budget
   tops out at 40s (DoH) + 10s (HTTPS) = 50s worst
   case. Pinned in `per_leg_timeout_is_sane`.

5. **DoH endpoints are HTTPS-only.** Pin test
   `doh_endpoints_pinned_and_https` rejects any
   endpoint entry that doesn't start with `https://`,
   blocking a future PR that accidentally adds a
   plaintext DoH-over-HTTP endpoint.

6. **Reproducible-build pin.** The `DOH_ENDPOINTS`
   constant is part of the compiled artefact — same
   bytes across reproducible-build runners. A change
   to the list shows up in the build hash and is
   visible to the audit-grep marker `DOH_ENDPOINTS`.

7. **Generic IP-pinned fetch reuses the same trust
   path.** `DohFetcher::fetch_pinned_bytes(domain,
   path, max_bytes)` and the pre-resolved
   `fetch_pinned_bytes_at` variant route through
   exactly the same DoH-resolve + IP-pin HTTPS + `https_only(true)`
   pipeline as `fetch_bootstrap`. Downstream callers
   that need to pull other signed artefacts over the
   same trust shape — TUF metadata
   ([`lakko_tuf::urls`](../../crates/lakko-tuf/src/urls.rs)
   composes the URLs;
   [`lakko_tuf::Signed::from_json`](../../crates/lakko-tuf/src/lib.rs)
   parses the body), KeyPackage bundles, signed
   identity blobs — reuse this helper rather than
   re-implementing IP pinning. The size cap
   (`max_bytes`) defends against an attacker-served
   unbounded payload that would otherwise let a
   single GET exhaust memory; the streaming-body
   loop bails out with `DohError::BodyTooLarge` on
   the first chunk that crosses the cap.

### T-9: Chunked AEAD stream primitive pins ChaCha20-Poly1305 + monotonic nonce

Sprint G5-aead-stream lands the
`crates/lakko-transport/src/aead_stream.rs` primitive
— a chunked ChaCha20-Poly1305 reader/writer pair that
both upcoming PT implementations (G5-1 REALITY, G5-3
Shadowsocks-2022) build on. Same crypto, same framing
contract, different outer headers.

**Pinned invariants:**

1. **Ciphersuite is ChaCha20-Poly1305 only.** The
   module-scope constant
   `aead_stream::ALG = Algorithm::ChaCha20Poly1305` is
   pinned by `pinned_constants`. SS-2022 and REALITY
   both default to ChaCha20-Poly1305; AES-256-GCM
   stays a separate variant on `lakko_crypto::aead`
   for the rare FIPS-friendly call site. A change to
   this constant requires bumping a wire-version
   identifier in every PT impl that depends on it.

2. **Two sealed frames per chunk — length first, then
   payload.** The length-prefix is sealed under its
   own nonce so the receiver can authenticate the
   next-frame-size **before** reserving a buffer for
   it. Reading the length unencrypted would let an
   active attacker swap in a 64 KB length to force an
   oversized allocation; sealing it ties the length
   to the per-stream key + nonce counter, and the
   AEAD authenticator rejects on read if it was
   tampered.

3. **Nonce model: 12-byte big-endian counter, low 8
   bytes carry the u64, high 4 bytes zero.**
   `nonce_counter_layout_pinned` asserts the exact
   layout. Matches the SS-2022 wire format. The
   writer's counter is independent from the reader's,
   so a duplex stream uses two
   `AeadStreamWriter` / `AeadStreamReader` pairs with
   different keys when direction separation matters
   (it does for SS-2022).

4. **Counter advances per seal, never reused.** Every
   `write_chunk` / `read_chunk` call bumps the
   counter twice (one seal for the length, one for
   the payload). `checked_add(1)` returns
   `AeadStreamError::NonceWrap` at the
   2^64-chunk-on-the-same-key ceiling — astronomical
   in practice but the error variant exists so a
   counter-wrap path cannot be silently misused.

5. **`MAX_CHUNK = 16 KiB` plaintext per chunk.**
   Same pin SS-2022 + REALITY use; matches TLS 1.3
   record size for cache-friendly buffer reuse.
   `write_chunk` returns
   `AeadStreamError::ChunkTooLarge` on any larger
   plaintext, never silently splits — splitting is
   the caller's job. Readers reject any sealed length
   that decodes above `MAX_CHUNK` (`DecodedLengthTooLarge`),
   stopping a malformed stream from forcing a
   megabyte allocation.

6. **Clean-EOF semantics: `Ok(None)` only at frame
   boundary.** A read that finds the underlying
   stream EOF *between* frames returns `Ok(None)`. A
   read that hits EOF *mid-frame* (sealed length
   landed but sealed payload truncated) returns
   `Err(AeadStreamError::Io(_))` — never `Ok(None)`.
   Garbage bytes that masquerade as a sealed length
   surface as `Err(AeadStreamError::AeadOpen)`.
   Tests pin all three: `read_chunk_clean_eof_returns_none`,
   `read_chunk_truncated_payload_is_io_error`,
   `read_chunk_garbage_bytes_rejected_not_none`.

7. **No domain separator in the AEAD AAD.** AAD is
   empty for both length and payload seals — the
   nonce counter itself is the only binding. SS-2022
   and REALITY both add their own outer framing
   (timestamped headers, salt, etc.) above this
   primitive; the AAD field is reserved for that
   layer if a future revision needs domain
   separation between the two PT impls.

### T-10: Shadowsocks-2022 outer header layer pins BLAKE3 context + timestamp window + replay window

Sprint G5-3-header lands the SS-2022 prelude codec in
`crates/lakko-transport/src/shadowsocks.rs`. The
prelude is what sits in front of the chunked-AEAD body
(T-9) on every SS-2022 TCP stream — salt + sealed
fixed header + sealed variable header — and the
per-direction subkey derivation that ties body chunks
to a specific `(psk, salt)` pair.

**Pinned invariants:**

1. **BLAKE3 `derive_key` context is the wire contract.**
   `SS_SUBKEY_CONTEXT = "shadowsocks 2022 session subkey"`
   matches the SS-2022 spec byte-for-byte. Two pin tests
   assert the literal: `pinned_constants` here and
   `derive_session_subkey_is_deterministic` exercises the
   path. Any change to the context string breaks wire
   compatibility with every previously-shipped client
   and every interop partner.

2. **Subkey input is `psk || salt`, in that order.**
   `derive_session_subkey` concatenates the 32-byte PSK
   with the 32-byte per-session salt and feeds the
   resulting 64 bytes as `key_material` to BLAKE3
   `derive_key`. Order matters — flipping it produces
   a different subkey and breaks every interop. The
   `derive_session_subkey_psk_separates` /
   `derive_session_subkey_salt_separates` tests
   confirm independent input-keying.

3. **Timestamp drift cap is `±SS_TIMESTAMP_TOLERANCE_SECS = 30`
   seconds.** `check_timestamp_window` rejects a
   request whose declared timestamp drifts from
   `now_unix` by more than 30 seconds — pinned
   verbatim from the SS-2022 spec. Tests pin both
   directions (`decode_request_rejects_timestamp_skew_past`
   for past, `decode_request_accepts_timestamp_within_tolerance`
   for in-window).

4. **Replay window is salt-keyed, not timestamp-keyed.**
   `ReplayWindow` tracks the last
   `SS_REPLAY_WINDOW_CAPACITY = 4096` distinct salts
   in a FIFO queue. A second `decode_request` call
   with the same wire bytes is rejected as
   `ShadowsocksHeaderError::ReplayedSalt` even if the
   timestamp is still within tolerance. The window's
   FIFO eviction at capacity means a 60-second drift
   tolerance × 4096 entries gives ~136 unique
   requests/second of replay-window coverage —
   comfortably above any expected per-server traffic.

5. **Response binds the originating request-salt.**
   `encode_response` prepends the 32-byte
   `request_salt` to the variable-header payload; the
   response-side `decode_response` rejects any wire
   where the echoed salt does not match
   `expected_request_salt` (verified by
   `decode_response_rejects_wrong_request_salt`). A
   replayed response from a different session cannot
   be spliced into the wrong request without forging
   the salt — and the salt is sealed inside the
   response's own AEAD so the binding survives an
   active-attacker rewrite.

6. **Header type byte is part of the AEAD-protected
   plaintext.** The 1-byte direction tag (`0 = client`,
   `1 = server`) lives inside the sealed fixed header,
   not in the clear-text salt prefix. A flipped type
   byte fails the AEAD authenticator before the
   decoder ever inspects it; the explicit type-check
   after `aead::open` is a defence-in-depth layer that
   surfaces `ShadowsocksHeaderError::WrongHeaderType`
   with the observed byte for sysadmin debugging.

7. **`Transport::connect` returns a working
   `ShadowsocksConnection`.** Closed in sprint
   S-5-shadowsocks-tcp (T-11). The prelude codec
   here is wired to `tokio::net::TcpStream` +
   `aead_stream` body via the new public
   `ShadowsocksTransport::connect` + symmetric
   server-side `accept_inbound` helper.

### T-11: SS-2022 TCP wiring preserves per-direction key separation + frame-size cap

Sprint S-5-shadowsocks-tcp stitches the prelude codec
(T-10) and the chunked-AEAD body (T-9) into a working
`Transport::connect` + `Connection` impl. The wiring is
visible end-to-end through three new public surfaces in
`crates/lakko-transport/src/shadowsocks.rs`:
`ShadowsocksConnection`, the now-functional
`ShadowsocksTransport::connect`, and the standalone
server-side helper `accept_inbound`.

**Pinned invariants:**

1. **Per-direction subkey separation.**
   `ShadowsocksConnection::new` takes two distinct
   `AeadKey`s — one for the client→server direction
   (`tx_subkey` from `encode_request` on the client,
   echoed by the server's `decode_request` output),
   one for the server→client direction (`rx_subkey`
   from `encode_response` / `decode_response`). The
   writer half holds the tx subkey, the reader half
   holds the rx subkey; neither is swapped. A bug
   that crossed them would surface immediately as
   AEAD-open failure on the very first body frame.

2. **Frame size cap is `aead_stream::MAX_CHUNK` (16
   KiB) at the Connection layer.** `Connection::send`
   returns `Error::FrameTooLarge { actual, limit:
   16384 }` for any frame above the SS-2022 wire cap.
   This is tighter than the generic
   `transport::MAX_FRAME_BYTES` (16 MiB) — the SS-2022
   spec takes precedence because the wire field is
   2-byte length-prefixed. Caller code must chunk at
   a higher layer if larger frames are needed.
   `tcp_send_oversize_frame_rejected` pins this.

3. **TCP_NODELAY enabled on both endpoints.**
   `connect` and `accept_inbound` both call
   `stream.set_nodelay(true)` (non-fatal on failure)
   so the 2-write AEAD chunk pattern doesn't pile up
   in Nagle's send buffer. Per-chunk latency stays
   close to the underlying RTT.

4. **Clean EOF surfaces as `Error::PeerClosed`,
   mid-frame EOF as `Error::Io`.** The
   `aead_err_to_transport` helper maps
   `AeadStreamError::Io(UnexpectedEof)` →
   `Error::PeerClosed` and other I/O errors →
   `Error::Io(string)`. This matches the
   Connection-trait contract (`recv` returns
   `PeerClosed` on clean EOF, `Io` on truncation)
   and means caller code can branch on the variant
   without parsing error strings.

5. **`accept_inbound` reuses the replay window
   across calls.** The caller owns the
   `&mut ReplayWindow` and threads it through every
   acceptance call on the same listener; a replayed
   prelude across two separate TCP connections to
   the same server is rejected via the window's
   FIFO LRU (T-10 #4). The listener-loop is not
   shipped in this sprint — the helper alone is the
   point where the window is consumed.

6. **PSK decode is eager.** `ShadowsocksTransport::new`
   calls `psk_from_b64` at construct time so a bad
   PSK string surfaces as `Error::Config(...)`
   immediately, not on first connect. Both padded and
   unpadded base64 forms are accepted defensively;
   the length check confirms exactly 32 bytes.
   `psk_from_b64_round_trips_padded` /
   `psk_from_b64_round_trips_unpadded` /
   `psk_from_b64_rejects_wrong_length` /
   `psk_from_b64_rejects_invalid_base64` pin all four
   paths.

7. **End-to-end coverage on live TCP.**
   `tcp_round_trip_frame_traffic` spawns a server on
   `127.0.0.1:0`, dials it through
   `ShadowsocksTransport::connect`, runs a
   request/response/second-frame round trip, and
   closes cleanly. `tcp_connect_rejects_wrong_psk`
   verifies that a PSK mismatch refuses the
   handshake on both sides (no body bytes ever flow).

### T-12: ECH config list parser is version-pinned + length-bounded

**Property.** [`parse_ech_config_list`](../../crates/lakko-transport/src/ech.rs)
strictly enforces the draft-ietf-tls-esni-18 §4
wire format. Strictly:

- Only the pinned version code `0xfe0d` (draft-13) is
  accepted. Any other version code surfaces as
  `EchError::UnsupportedVersion` rather than being
  silently skipped — the audit-safe stance until a
  forward-compat path is wired.
- Outer `ECHConfigList` length is capped at
  `MAX_ECH_CONFIG_LIST_LEN = 16 KiB`.
- Inner config count is capped at
  `MAX_INNER_CONFIGS = 16`.
- Each inner `ECHConfig` body is capped at
  `MAX_INNER_CONFIG_LEN = 4 KiB`.
- `cipher_suites` must be non-empty + a multiple of 4
  bytes (KDF id + AEAD id pair).
- `public_name` is 1..=255 bytes + valid UTF-8.
- `maximum_name_length` is non-zero.
- `public_key` is non-zero length.
- Trailing bytes past the declared outer length are
  refused (`EchError::TrailingBytes`) — TLS extensions
  are length-prefixed for a reason; trailing junk
  means something went wrong upstream.

**Why this matters.** The DNS HTTPS resource record
(RR type 65) carries the `ech=...` parameter as opaque
bytes the client MUST parse before it can configure
rustls. Any attacker that can spoof / poison the DNS
answer (or any cache between the recursive resolver
and the client) can deliver arbitrary bytes through
this parser. The parser is the gate that separates
attacker-controlled bytes from rustls — a panic /
overflow / stack abuse here is a vulnerability on the
anti-censorship bootstrap path.

The parser is fully sync, no allocation exceeds a hard
cap from arbitrary attacker input, and the
[`HpkeSymmetricCipherSuite`](../../crates/lakko-transport/src/ech.rs)
+ [`EchConfig`](../../crates/lakko-transport/src/ech.rs)
types are opaque to rustls (just the keying material it
needs to build the inner ClientHello).

**Tests.** 12 unit tests in
[`crates/lakko-transport/src/ech.rs`](../../crates/lakko-transport/src/ech.rs)
covering happy path (one config + two configs),
empty list, truncated outer length, truncated inner
body, unsupported version, outer too large, trailing
bytes, empty cipher suites, bad cipher-suites length
(non-multiple-of-4), zero `maximum_name_length`,
invalid UTF-8 in `public_name`.

**Not covered yet.** Integration with
`rustls::client::ClientConfig::with_ech_config_list` is
a follow-up sprint gated on a working ECH-supporting
test endpoint (Cloudflare publishes one). The parser
stands alone so the invariant can be pinned
independently of network-side wiring.

### T-13: REALITY authenticator is ECDH-keyed HMAC over (short_id || timestamp), tag verified before any downstream byte is trusted

**Property.** The REALITY authenticator that rides in
the TLS 1.3 ClientHello.session_id slot is built from:

- X25519 ECDH between the client's TLS-1.3-key_share
  ephemeral secret and the publisher-distributed
  REALITY server static public key;
- HKDF-SHA-256 over the ECDH shared secret with the
  pinned label `"lakko/reality/v1"`;
- HMAC-SHA-256(key, short_id || ts_be8) truncated to
  16 bytes.

Wire layout:

```
0           8           16                          32
|  short_id  |  ts_be8    |       hmac_tag[..16]      |
```

`short_id` is plaintext so the receiver can look up
its keying material in O(1) before doing any HMAC
work; `ts_be8` is a u64 big-endian unix-seconds
timestamp; the 16-byte tag is the cryptographic
backstop. Constant-time comparison via the `subtle`
crate gates the verifier path.

**Why this matters.** An attacker on the wire sees a
TLS 1.3 ClientHello with the cover-site SNI, a
standard-shaped key_share, and a session_id that is
opaque random bytes to a non-REALITY peer. The
authenticator binds the handshake to a specific
publisher key + a recent timestamp; a captured
session_id cannot be replayed outside the
`tolerance_secs` window the receiver pins (default
suggested 30 s).

**Tests.** 10 unit tests in
[`crates/lakko-transport/src/reality/authenticator.rs`](../../crates/lakko-transport/src/reality/authenticator.rs)
covering: compute + verify round-trip; wrong short_id
in allow-set is `ShortIdNotAllowed`; timestamp outside
tolerance is `TimestampSkew`; timestamp inside window
verifies; tampered tag is `TagMismatch`; wrong server
secret (attacker without the publisher's static
private key) is `TagMismatch`; wrong client ephemeral
pubkey is `TagMismatch`; authenticator is exactly 32
bytes; short_id is extractable directly from the
authenticator (the receiver's O(1) lookup path is
preserved); two distinct ephemerals yield distinct
tags even with identical short_id + timestamp (a
passive observer cannot replay-detect by tag
equality).

Plus 7 unit tests on
[`crates/lakko-transport/src/reality/config.rs`](../../crates/lakko-transport/src/reality/config.rs)
covering happy parse + every rejection path (bad
pubkey base64, wrong pubkey length, bad short_id hex,
wrong short_id length, empty SNI, empty remote).

Plus 5 unit tests on
[`crates/lakko-transport/src/reality/transport.rs`](../../crates/lakko-transport/src/reality/transport.rs)
covering the constructor's config-validation gate and
the `Transport::connect` `NotImplemented` shape.

**Wire-compatibility caveat.** Lakko's REALITY pins
its own canonical bytes via the `"lakko/reality/v1"`
HKDF label and is therefore NOT byte-for-byte
compatible with upstream xtls/REALITY today. A
follow-up sprint aligns the exact label / field
layout to enable interop. Until then, Lakko REALITY
clients only handshake with Lakko REALITY servers.

**Not covered yet.** TLS handshake injection (writing
the authenticator into ClientHello.session_id and
validating the server's response in
ServerHello.Random) is the next sub-sprint
(S-5-reality-handshake). rustls 0.23's public API
does not expose a session_id override hook, so the
wire-up either takes a `rustls-hooks` fork dependency
or writes the ClientHello on top of a raw TCP socket.

### T-14: WebTransport client uses cert-hash pinning + u32-BE length-prefixed framing with MAX_FRAME_BYTES enforcement

**Property.**
[`WebTransportPt`](../../crates/lakko-transport/src/webtransport_pt.rs)
is the real-implementation `Transport` impl over the
[`web-transport-quinn`](https://crates.io/crates/web-transport-quinn)
crate (built on top of the same quinn 0.11 + rustls 0.23
QUIC stack the rest of `lakko-transport` already
depends on; crypto-provider-agnostic so the whole crate
stays on `ring`).

Two trust modes, both validated at construction time
(`WebTransportPt::new` returns `Error::Config` upfront —
no connect-time surprises):

- **Cert-hash pinning** (recommended for production):
  the signed bootstrap config carries SHA-256 hashes of
  the server's full DER certificate, base64-encoded
  without padding. Matches the W3C
  `serverCertificateHashes` mode the browser WebTransport
  API uses; the bootstrap-rendezvous signed channel is
  the out-of-band trust root the W3C spec assumes.
- **System-roots fallback**: empty `cert_hashes_b64` →
  the platform CA chain. Convenience for dev against a
  CA-signed origin; production SHOULD pin.

Note: this is **cert-hash** pinning (SHA-256 of full DER
cert), not **SPKI** pinning (SHA-256 of the
SubjectPublicKeyInfo) — different from the SPKI pinning
[`PinnedVerifier`](../../crates/lakko-transport/src/pinning.rs)
uses for HTTPS REST clients. The W3C WebTransport spec
defines this specific mode; the rendezvous-trust
audit-grep marker is `cert_hashes_b64`.

Framing: every `Connection::send` writes a u32-BE
length prefix followed by the payload; every
`Connection::recv` reads the same 4-byte prefix +
`MAX_FRAME_BYTES`-bounded payload. Frames larger than
[`MAX_FRAME_BYTES`](../../crates/lakko-transport/src/transport.rs)
(16 MiB) are refused with `Error::FrameTooLarge` —
both the send-time guard (against local-side bug) and
the recv-time guard (against attacker-supplied
length prefix). Each `Connection` rides one
bidirectional WebTransport stream within one session;
multiplexing across multiple Lakko channels is the next
layer's job.

**Tests.** 8 unit tests in
[`crates/lakko-transport/src/webtransport_pt.rs`](../../crates/lakko-transport/src/webtransport_pt.rs)
covering: happy parse, reject non-https URL, reject
unparseable URL, reject base64 garbage in cert hash,
reject wrong-length cert hash, accept empty hash list
(system-roots mode), `WebTransportPt::new` rejects bad
config eagerly, and a serde round-trip of the wire
config.

**Not covered yet.** Live network integration test
against an in-test wtransport server with an
rcgen-generated self-signed cert and a known-hash
pinning round-trip — deferred to a follow-up sprint
because the wtransport server-side TLS configuration
takes an `rcgen` cert via several layers of conversion
(rustls-pki-types `PrivateKeyDer` → `CertificateDer` →
quinn `ServerConfig`) that needs its own wiring
beyond the scope of this implementation sprint.
The unit tests above pin the config / framing surface;
the actual session round-trip rides on `web-transport-quinn`'s
own test suite + production deployments.

## Known soft spots — out of scope for Phase 5 scaffold

### Real `Transport` implementations for REALITY / MASQUE / SS-2022 / WebTransport

Tracked as G5-1 / G5-2 / G5-3 / G5-4 in
[`docs/ROADMAP-PHASE3-CLOSEOUT.md`](../ROADMAP-PHASE3-CLOSEOUT.md).
Until those land, the only candidate that **actually
completes a connect** through the pluggable selector
is the existing QUIC transport. T-2 protects us from
silently using a half-built scaffold.

### DoH-resolved bootstrap fetch

The rendezvous module implements parse + sign + verify.
The actual fetch path (DoH → innocuous-looking domain →
HTTP GET → `BootstrapConfig`) is a follow-up sub-sprint
that reuses [`crate::http::HttpTransport`](../../crates/lakko-transport/src/http.rs)
with SPKI pinning.

### Concurrent happy-eyeballs

The scaffold selector is sequential (per-candidate
outer-`tokio::time::timeout`). True concurrent race
(`tokio::select!` over spawned per-candidate tasks)
needs the `Transport` trait to support cheap cloning
(`Arc<dyn Transport>` wrapping or a `Clone` bound). The
first G5-N sprint that wants sub-RTT first-byte does
the refactor.

### ECH (Encrypted Client Hello)

Parser-side closed by T-12 (see invariants section).
Remaining work for full ECH coverage: wire the parsed
[`EchConfigList`](../../crates/lakko-transport/src/ech.rs)
into `rustls::client::ClientConfig::with_ech_config_list`
on the HTTPS-based transports (REALITY, WebTransport,
HttpTransport) once a working ECH-supporting test
endpoint is stood up. Until then the outer SNI on those
transports remains observable to the path even though
the parser is ready.

### Censorship lab tests

Tracked as G5-7. A real CI gate that exercises every
PT against GFW / TSPU / IR DPI emulators is the audit
acceptance for Phase 5; today the invariants above are
unit-level + offline.

## Cross-links

- [ADR-0004](../adr/0004-anti-censorship.md) —
  pluggable-transport strategy
- [`docs/ROADMAP-PHASE3-CLOSEOUT.md`](../ROADMAP-PHASE3-CLOSEOUT.md) —
  G5-\* sprint decomposition
- [`calls.md`](./calls.md) — call-side cryptography
  (sister doc — disjoint subsystem, shared audit
  approach)
- [`known-gaps.md`](./known-gaps.md) — open work register
- [`threat-model.md`](./threat-model.md) — the censor
  + active-probe + compromised-rendezvous adversary
  models this doc's invariants resist
