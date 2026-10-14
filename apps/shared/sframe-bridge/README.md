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
3. Runs the test functions listed under Tests below.

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

Every test works in the ADR-0071 frame layout
(`clear_prefix || ciphertext_and_tag || sframe_header || trailer`),
which is the only one the bridge seals or opens.

| Test | What it proves |
|---|---|
| `test_round_trip_single_sender` | Encrypt + Decrypt recover the frame; the sealed frame fits frame + 36 |
| `test_additional_data_ignored_header_is_aad` | The host `additional_data` is not authenticated; the SFrame header is |
| `test_multi_sender_into_one_receiver` | Three encryptors with distinct leaves open through one decryptor |
| `test_bad_seed_length` | `Create` returns nullptr on a short seed |
| `test_ciphertext_tamper` | A flipped ciphertext byte fails |
| `test_counter_monotonicity_and_out_of_order` | 1000 frames open in any order |
| `test_per_call_key_isolation` | A frame of one call does not open under another call's seed |
| `test_payload_size_extremes` | 1 byte to 64 KiB round-trips |
| `test_media_frame_golden_vector` | The canonical ADR-0071 golden frame opens; a seal of the same input has the same shape and opens |
| `test_codec_decides_the_clear_prefix` | VP8 key frame keeps 10 bytes, other VP8 frames 3, audio / Opus / OTHER / unknown none; `CodecFromName` |
| `test_every_byte_is_authenticated` | Flipping any byte of a sealed frame, or changing its length, fails |
| `test_header_first_layout_is_refused` | The ADR-0013 layout does not open |
| `test_set_codec_races_encrypt` | `SetCodec` on one thread while another seals |
| `test_mid_call_rekey_overlap` | Rekey keeps the overlap window and the codec |
| `test_empty_frame_is_not_a_decrypt_failure` | An empty frame is zero bytes out with an OK status |

## A receiver's minimum playout delay (`receiver-delay.h`)

Not SFrame, but the same layer and the same kind of dependence on the
binary: `receiver-delay.h` sets a remote receiver's minimum playout delay
by calling the engine's
`webrtc::RtpReceiverInterface::SetJitterBufferMinimumDelay` through its
vtable slot, because neither public binding wraps it (ADR-0070, note of
2026-10-12). It lives here because the probe it runs needs the decryptor
this directory attaches, and because both platform shims already compile
this directory.

The header is self-contained and header-only: the iOS shim
(`SFrameAttach.mm`) and the Android JNI file (`sframe_jni.cc`) include it,
so no build recipe lists a new source. The evidence for the slot, for each
binary's slot encoding (Android arm64 uses relative vtables, the others
absolute ones) and for the argument's calling convention is in the
header's opening comment. Each shim additionally refuses to call it
unless the loaded WebRTC image is one that evidence was read from.

```bash
apps/shared/sframe-bridge/test-receiver-delay.sh
```

builds and runs `test_receiver_delay.cc` (optimised and not, and for
x86_64 too on an Apple Silicon Mac that can run it):

| Test | What it proves |
|---|---|
| `test_layout_of_the_argument` | `EngineOptionalDouble` is `std::optional<double>` byte for byte |
| `test_sets_through_a_real_vtable` | The calls land on slots 8, 13 and 16 of a class declared in the engine's order; 0 is `nullopt`; the ceiling; the probe's reference is returned |
| `test_no_decryptor_is_not_a_failure` | Without a decryptor nothing is called and the probe stays open |
| `test_wrong_kind_stops_everything` | A wrong `media_type()` stops every set, said once |
| `test_wrong_decryptor_stops_everything` | A wrong or missing `GetFrameDecryptor()` answer stops every set; nothing unknown is released |
| `test_a_pass_after_a_kind_mismatch_is_not_possible` | The kind is checked on every set, after the probe passed too |
| `test_sets_through_a_relative_vtable` | The relative encoding, on a table built by hand |

The test shows that this code calls the slots the engine's header
declares. It cannot show how the shipped binaries are laid out: that was
read from the binaries.

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
