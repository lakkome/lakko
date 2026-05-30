<!-- SPDX-License-Identifier: AGPL-3.0-or-later -->
<!-- SPDX-FileCopyrightText: 2026 Lakko Contributors -->

# sframe-bridge — C++ ↔ Rust glue for libwebrtc SFrame

C++ subclass of `webrtc::FrameEncryptorInterface` /
`webrtc::FrameDecryptorInterface` (M125, branch-heads/6422) that
trampolines through the flat C ABI of `crates/lakko-sframe-native`
(N-1) for every AEAD seal / open / wire-header pack / unpack.

This is the middle layer in the SFrame native-shim stack:

```
 +---------------------------------------------------------+
 | Platform glue   apps/ios/Lakko/Calls/SFrameAttach.mm    |
 | (N-4 / N-5)     apps/android/lakko-sframe/.../jni.cc    |
 +---------------------------------------------------------+
                          | C++ vtable
 +---------------------------------------------------------+
 | THIS CRATE      apps/shared/sframe-bridge/              |
 |                 LakkoFrameEncryptor / LakkoFrameDecryptor
 +---------------------------------------------------------+
                          | flat C ABI (cbindgen header)
 +---------------------------------------------------------+
 | Rust core       crates/lakko-sframe-native/             |
 |                 (N-1, lakko_sframe_seal / open / wrap)  |
 +---------------------------------------------------------+
                          | Rust call
 +---------------------------------------------------------+
 | Crypto          crates/lakko-calls/src/sframe/          |
 |                 (RFC 9605 + ChaCha20-Poly1305 / AES-GCM)|
 +---------------------------------------------------------+
```

## Public surface

Two classes; both are libwebrtc-shape ref-counted heap objects.

| Class                 | Constructor                           |
|-----------------------|---------------------------------------|
| `LakkoFrameEncryptor` | `Create(seed, seed_len, local_leaf)`  |
| `LakkoFrameDecryptor` | `Create(seed, seed_len)`              |

Both `Create` symbols return `nullptr` on bad seed length (must
be exactly 32 bytes — the
`Group::export_secret("lakko/call/v1", call_id, 32)` output) or
key-derivation failure. The platform shim MUST refuse to install
the encryptor / decryptor on `nullptr` (security invariant #4: no
fallback to non-E2EE).

The decryptor lazily derives + caches per-sender `ParticipantKey`
entries as frames from new `sender_leaf` values arrive; this
matches how the SFU forwards N senders' frames into one receiver
track.

## Threading

libwebrtc serialises `Encrypt` per encryptor (codec thread) and
`Decrypt` per decryptor (network thread). The encryptor's seal
path therefore needs no lock. The decryptor's per-sender cache is
guarded by `std::mutex` because the same decryptor instance
receives frames from multiple senders interleaved (one incoming
RTP track per remote participant, but the SFU may demux into a
shared receiver in some topologies).

## Building locally

```bash
apps/shared/sframe-bridge/build.sh
```

What that does:
1. `cargo build -p lakko-sframe-native --release` (workspace root).
2. `c++ -std=c++17` compile of `sframe-bridge.cc` + `test_bridge.cc`
   + `test_checks_stub.cc`, linking against the staticlib.
3. Runs the four-test round-trip suite.

The `test_checks_stub.cc` file supplies a stub for
`rtc::webrtc_checks_impl::FatalLog` so the test binary can link
without a libwebrtc runtime. The real libwebrtc binary on iOS
(`WebRTC.framework`) and Android (`libwebrtc.so`) supplies the
production implementation; the stub is ONLY compiled into the
standalone test binary.

Build requires `apps/shared/webrtc-cpp-headers/M125/include/` to
be populated — if it is not, run
`apps/shared/webrtc-cpp-headers/M125/fetch.sh` first.

## Tests

| Test                                  | What it proves                                                        |
|---------------------------------------|------------------------------------------------------------------------|
| `test_round_trip_single_sender`       | Encrypt + Decrypt recover the original plaintext under the same seed   |
| `test_aad_binding`                    | A tampered `additional_data` (RTP header bits) causes Decrypt to fail  |
| `test_multi_sender_into_one_receiver` | Three encryptors with distinct `local_leaf` all decode into one cache  |
| `test_bad_seed_length`                | `Create` returns nullptr on short seed (no silent E2EE downgrade)      |

## Platform integration

This crate ships nothing platform-specific. The iOS shim (N-4)
will compile these two files into an Objective-C++ module and call
`Create` from a non-public selector probe; the Android shim (N-5)
will compile them into a JNI `.so` and return the raw pointer
through a `getNativeFrameEncryptor()` JNI method. Both shims are
the responsibility of the next two sub-sprints.

## Memory ownership

`Create` returns a raw heap pointer with refcount 0. The caller
must immediately wrap it in `rtc::scoped_refptr<T>` (which calls
`AddRef`, bumping the count to 1) or call `AddRef` directly. The
matching `Release` runs `delete this` when the count reaches 0.

This is the same lifecycle convention `rtc::FinalRefCountedObject<T>`
implements upstream; we inline it to avoid pulling in one more
header file we'd have to vendor under `webrtc-cpp-headers/M125/`.
