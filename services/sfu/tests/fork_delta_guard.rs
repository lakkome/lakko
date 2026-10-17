// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! Fork-delta guard for the vendored `str0m`.
//!
//! Lakko vendors `str0m` 0.19.0 under `crates/vendor/str0m` and consumes it
//! via `[patch.crates-io]`, the same shape as the `webauthn-rs-core` fork
//! guarded by `services/auth/tests/fork_delta_guard.rs`.
//!
//! ## What the fork contract is
//!
//! One capability, added because it did not exist: **tell an
//! `ExtensionSerializer` which packet of the frame it is writing.**
//!
//! The SFU has to relay the `generic-frame-descriptor-00` RTP header
//! extension — without it a libwebrtc receiver cannot recognise a key frame
//! in an SFrame-sealed stream, because the flag it reads lives in the
//! bitstream bytes the encryptor replaced. That extension's value is
//! *per packet*: its `B` and `E` bits mark the first and last packet of a
//! frame, so the frame-level value cannot simply be copied onto every packet
//! of a re-packetized frame. Upstream's `write_to(&self, buf, ev)` is handed
//! only the frame-level `ExtensionValues` and has no way to know its
//! position, which makes a correct relay inexpressible.
//!
//! The delta across six files, all additive except three lines replaced in
//! place:
//!
//! - `src/rtp/ext.rs` — new `FramePosition` type and
//!   `ExtensionValues::frame_position` field; new
//!   `UserExtensionValues::fill_missing_from`.
//! - `src/packet/payload.rs` — the packetizer sets `frame_position`, in the
//!   loop that already strips other per-packet fields.
//! - `src/packet/buffer_rx.rs` — new `Depacketized::ext_vals_for_frame`,
//!   which fills user extension values missing from the last packet in from
//!   the first. Upstream takes the last packet's values only, because some
//!   extensions are last-packet-only; a frame descriptor is the opposite,
//!   so neither packet alone carries the full set.
//! - `src/media/mod.rs` — `MediaData` is built from `ext_vals_for_frame()`.
//! - `src/rtp/mod.rs`, `src/lib.rs` — re-export `FramePosition`.
//!
//! No existing behaviour changes: every serializer that ignores the new
//! field behaves exactly as before, and `fill_missing_from` only ever adds a
//! value for a type the last packet did not already carry.
//!
//! ## Second change: how long a missing packet is asked for (2026-10-03)
//!
//! [ADR-0067](../../../docs/adr/0067-loss-resilient-call-media.md). Upstream
//! asks a publisher for a missing packet at most five times, once per 33 ms
//! NACK tick, and tracks a hole only within 100 packets of the newest. On a
//! live call the uplink lost 20-40% of packets for about a second at a time,
//! so all five requests fell inside the burst and the packet never reached
//! the SFU; every subscriber then waited 2-3 s for a keyframe. The fork asks
//! up to ten times on a schedule that doubles from 33 ms to a 200 ms cap
//! (about 1.4 s in all) and tracks holes within 512 packets. This one IS a
//! behaviour change, confined to the receive-side NACK register:
//!
//! - `src/streams/register_nack.rs` — `MAX_MISORDER` 100 -> 512,
//!   `MAX_NACKS` 5 -> 10, `retry_interval`, a `last_nack` time per packet,
//!   `nack_reports` takes the time; three tests added, upstream tests pass a
//!   time.
//! - `src/streams/register.rs`, `src/streams/receive.rs`,
//!   `src/streams/mod.rs`, `src/_internal_test_exports/fuzz.rs` — pass the
//!   time through.
//!
//! ## Fifth change: the loss fraction in a receiver report (2026-10-04)
//!
//! Upstream computes the RFC 3550 interval loss and casts it to a byte
//! without the appendix A.3 clamp. When packets lost in one interval are
//! repaired in the next, the interval receives more than it expected, the
//! value is a small negative number, and the byte wraps to about 97% loss.
//! Seen on a live call: reports of 248/255 and 241/255 for a publisher whose
//! video arrived at a full 30 fps, each followed by the sending browser
//! cutting its bitrate estimate to a tenth or less and climbing back over
//! about 25 s. The fork reports zero for a negative interval and saturates
//! at 255:
//!
//! - `src/streams/register.rs` - `fraction_lost` clamps; two tests added.
//!
//! ## Eighth change: steady NACK retries, no retransmission cap (2026-10-04)
//!
//! Measured through the SFU (`measure_uplink_dip` in
//! `synthetic_client_e2e.rs`): with 70% of a publisher's datagrams lost for
//! a second, the second change's schedule (gaps doubling to 200 ms) repaired
//! the slowest packet 0.7-1.1 s after it was sent, at any packet rate. And
//! upstream's send side throws queued retransmissions away once they pass
//! 15% of the bytes sent in the last second, which left a subscriber's holes
//! unfilled after a burst on its downlink.
//!
//! - `src/streams/register_nack.rs` - `MAX_NACKS` 10 -> 25, `retry_interval`
//!   a steady 60 ms (every second NACK tick, about 1.6 s in all).
//! - `src/streams/send.rs` - `DEFAULT_RTX_RATIO_CAP` `Some(0.15)` -> `None`.
//!
//! ## Third change: redundant audio, RED (2026-10-04)
//!
//! [ADR-0067](../../../docs/adr/0067-loss-resilient-call-media.md) step 3.
//! Chrome and libwebrtc can wrap Opus in RED (RFC 2198), so an isolated lost
//! audio packet is repaired from the next one. Upstream has no `red` codec:
//! it parses `a=rtpmap:63 red/48000/2` as unknown and drops it from every
//! answer, so nobody sends RED. The fork lets an audio `red` payload type be
//! negotiated and carried by payload type; it never looks inside one (the
//! container is built before SFrame seals the frame, so it is ciphertext).
//! It is opt-in (`CodecConfig::enable_red`), not part of the default codec
//! set, so an endpoint that does not ask for it behaves as upstream.
//!
//! - `src/format/codec.rs` — `Codec::Red`, an audio codec named `red`.
//! - `src/format/format_params.rs` — `RedParams` (the fmtp list, `111/111`:
//!   2 to 32 entries of one payload type; any other list does not parse) and
//!   `FormatParams::red`.
//! - `src/format/mod.rs` — re-export `RedParams`.
//! - `src/format/codec_config.rs` — `CodecConfig::enable_red`: payload type
//!   63, 48 kHz, 2 channels, placed directly before Opus because the order
//!   of the list is the order of the `m=audio` format list and libwebrtc
//!   sends RED only when `red` precedes `opus`; no RTX, no NACK. Nine tests
//!   for the fmtp, the ordering and the answer rules.
//! - `src/format/payload_params.rs` — two `red` entries match when both
//!   carry a list; until locked the local entry takes the remote's list, so
//!   an answer echoes the offer's fmtp.
//! - `src/sdp/data.rs` — `FormatParam::Red`; `MediaLine::rtp_params` keeps a
//!   remote `red` entry only when its fmtp names an Opus payload type of the
//!   same m-line (which also drops video `red/90000`).
//! - `src/sdp/parser.rs` — a bare fmtp value that is a `red` list is kept
//!   instead of discarded.
//! - `src/packet/mod.rs` — `Codec::Red` maps to the pass-through packetizer
//!   and depacketizer (two exhaustive matches needed an arm; RTP mode uses
//!   neither).
//!
//! Receiving and `write_rtp` needed no change: both look a payload type up
//! in the session's codec list, where the `red` entry now is.
//!
//! ## Fourth change: video FEC, RED + ULPFEC (2026-10-04)
//!
//! [ADR-0067](../../../docs/adr/0067-loss-resilient-call-media.md) step 4.
//! Chrome and libwebrtc offer video `red/90000` (with an RTX) and
//! `ulpfec/90000`, and once both are negotiated send every video packet
//! inside RED, FEC packets included. Upstream knows neither, so no answer
//! keeps them. The fork lets the pair be negotiated and carried by payload
//! type; it never looks inside a RED or FEC payload (the SFU does, in
//! `services/sfu/src/video_fec.rs`, and only at the cleartext header bytes).
//! Opt-in (`CodecConfig::enable_video_fec`), not part of the default codec
//! set, so an endpoint that does not ask for it behaves as upstream. No file
//! is touched that the third change had not touched already:
//!
//! - `src/format/codec.rs` — `Codec::RedVideo` (SDP name `red`, told from
//!   audio `Codec::Red` by the 90 kHz clock) and `Codec::Ulpfec`, both
//!   video.
//! - `src/sdp/parser.rs` — an `a=rtpmap` named `red` with clock rate 90000
//!   parses as `Codec::RedVideo`.
//! - `src/sdp/data.rs` — `MediaLine::rtp_params` keeps video `red` and
//!   `ulpfec` only as a pair: a media line with one and not the other is
//!   read as having neither, so neither is matched or answered. The red RTX
//!   mapping (`apt`) comes from the existing generic code.
//! - `src/format/codec_config.rs` — `CodecConfig::enable_video_fec`: `red`
//!   116 with RTX 117 and `ulpfec` 118 (libwebrtc's numbers), 90 kHz, no
//!   RTCP feedback lines, appended after the video codecs. Seven tests for
//!   the configuration and the answer rules.
//! - `src/format/payload_params.rs` — two `RedVideo` or two `Ulpfec` entries
//!   match on codec and clock rate alone.
//! - `src/packet/mod.rs` — both map to the pass-through packetizer and
//!   depacketizer (exhaustive matches; RTP mode uses neither).
//!
//! Receiving, `write_rtp`, NACK and RTX needed no change. A packet under the
//! `red` payload type is looked up in the session's codec list like any
//! other and delivered as a packet of the video stream; because the `red`
//! entry has an RTX payload type, a retransmitted RED packet is rewritten
//! back onto the main stream under the `red` payload type before the SFU
//! sees it, and a RED packet the SFU sends is kept for a subscriber's NACK.
//!
//! ## Sixth change: what a forward-error-correction receiver needs (2026-10-04)
//!
//! [ADR-0067](../../../docs/adr/0067-loss-resilient-call-media.md) step 4,
//! part 1. The SFU rebuilds a publisher's lost video packets from the
//! publisher's own ULPFEC (`services/sfu/src/fec_receive.rs`). ULPFEC covers
//! every byte of a packet after the 12-byte fixed header, the header
//! extension block included, and upstream keeps only the parsed extension
//! values and the payload. A rebuilt packet also has to stop being asked for
//! and stop counting as lost. Three methods on `DirectApi`, nothing changed
//! for a caller that does not use them:
//!
//! - `src/session.rs` — `Session::keep_received_rtp` (off by default) and
//!   `pending_packet_received`: when on, `handle_rtp` copies the packet as it
//!   is after SRTP decryption (header bytes off the datagram, then the
//!   decrypted payload with its padding) and keeps the copy beside
//!   `pending_packet`. Not for a packet that arrived on the RTX stream: its
//!   extension block is the retransmission's own. One allocation and one
//!   copy per received packet while on. `Session::handle_recovered_rtp`
//!   parses a whole cleartext RTP packet and runs it through the receive
//!   stream's sequence extension, duplicate check and register update, the
//!   same calls `handle_rtp` makes for a packet off the wire, without SRTP,
//!   TWCC or absolute-send-time handling, and returns the `RtpPacket`
//!   instead of queueing an event.
//! - `src/change/direct.rs` — `DirectApi::keep_received_rtp`,
//!   `take_received_rtp` and `handle_recovered_rtp`, thin wrappers over the
//!   above; two imports.
//!
//! The choice between feeding a rebuilt packet through the receive path and
//! a bare "mark as received" call: the receive path, because the SFU needs
//! the packet parsed the way a received one is (extended sequence number,
//! header extension values such as the frame descriptor) to relay it, and
//! one call then does both. It returns the packet rather than emitting
//! `Event::RtpPacket` because RTP mode holds a single pending packet, and
//! one arrival can rebuild several.
//!
//! This change was made on a tree where `git status` showed the vendored
//! crate clean before it; the two files are its whole delta by
//! `git diff --stat -- crates/vendor/str0m`. It was not compared against the
//! registry copy of pristine 0.19.0.
//!
//! ## Seventh change: what a forward-error-correction sender needs (2026-10-04)
//!
//! [ADR-0067](../../../docs/adr/0067-loss-resilient-call-media.md) step 4,
//! part 2. The SFU computes an ULPFEC packet of its own for each subscriber
//! (`services/sfu/src/fec_send.rs`), over the media packets as that
//! subscriber receives them. ULPFEC covers the header extension block, and
//! `str0m` writes that block when it serializes a packet for sending: which
//! extensions are present is decided then (`mid` and `rid` are written only
//! until a receiver report has come back for the SSRC). So the bytes to
//! protect exist only after `StreamTx::poll_packet` has written them, and a
//! retransmission of the same packet gets a different block. One setter and
//! one getter, nothing changed for a caller that does not use them:
//!
//! - `src/streams/send.rs` — `StreamTx::keep_sent_rtp` (a field, off by
//!   default); `poll_packet` reports in its receipt whether the packet is a
//!   first transmission of a stream that keeps its packets.
//! - `src/session.rs` — `PacketReceipt::keep_wire`; `Session::sent_rtp`, a
//!   queue of at most 1024 packets; `poll_packet` copies the serialized
//!   packet (the buffer handed to SRTP, before encryption) into it when the
//!   receipt says so. One allocation and one copy per kept packet.
//! - `src/change/direct.rs` — `DirectApi::take_sent_rtp`, which pops the
//!   oldest.
//!
//! The alternative considered was to compute the FEC inside `str0m` (a
//! request queued on the send stream and filled in when it is serialized).
//! It needs the FEC arithmetic, the zeroing of the pacer-written extensions
//! and a per-stream packet window inside the vendored crate; handing the
//! bytes out keeps the fork to a copy and leaves all of that in the SFU,
//! where it is tested and fuzzed. An FEC packet the SFU builds goes back in
//! through `StreamTx::write_rtp` like any packet: not nackable, so it never
//! enters the retransmission cache, and numbered for transport-wide
//! congestion control like any other.
//!
//! By `git diff --stat -- crates/vendor/str0m` over the working tree that
//! carried the sixth change uncommitted, this change touches `send.rs` and
//! the two files of the sixth. It was not compared against the registry copy
//! of pristine 0.19.0.
//!
//! ## Ninth change: a NACK schedule per receive stream (2026-10-05)
//!
//! [ADR-0070](../../../docs/adr/0070-call-media-through-a-loss-burst.md)
//! step 3. The eighth change asks for a missing packet of every stream every
//! 60 ms. A publisher's light video (tens of packets a second) is what a
//! viewer falls back to in a burst of loss, and it was repaired no faster
//! than the full stream. The fork lets one receive stream be asked on an
//! interval of its own; a stream that is not given one, and a session in
//! which no stream is, behave exactly as after the eighth change.
//!
//! - `src/streams/register_nack.rs` - `NackRegister::set_retry_interval`
//!   and a `retry` field; `PacketStatus::needs_nack` takes it. With an
//!   interval of its own a packet is asked for as many times as cover the
//!   span of the default schedule (`max_nacks_for`: 25 x 60 ms = 1.5 s, so
//!   75 times at 20 ms), at least once, at most 255 times, and the interval
//!   is never below 10 ms (`MIN_RETRY_INTERVAL`). One test added.
//! - `src/streams/register.rs` - `ReceiverRegister::set_nack_retry_interval`,
//!   a pass-through.
//! - `src/streams/receive.rs` - `StreamRx::set_nack_retry_interval` (public,
//!   reached through `DirectApi::stream_rx`) and a field; the interval is
//!   handed to the register each time a NACK is built, because the register
//!   is created with the first packet and again after a reset.
//! - `src/streams/mod.rs` - `Streams::min_nack_retry_interval`.
//! - `src/session.rs` - `nack_at` uses the shortest per-stream interval when
//!   it is below the 33 ms `NACK_MIN_INTERVAL`. Without this a 20 ms
//!   interval would still be served on the 33 ms tick. It is a behaviour
//!   change for the other streams of that session only in this: their
//!   60 ms is then met on a 20 ms tick (every 60 ms) instead of a 33 ms one
//!   (every 66 ms).
//!
//! ## Tenth change: resend a sent packet without being asked (2026-10-05)
//!
//! ADR-0070 step 3. The SFU sends each packet of a light companion track to
//! a subscriber a second time some tens of milliseconds after the first.
//! Upstream resends only what a NACK names (or, with bandwidth estimation
//! on, what its padding picks). One method, nothing changed for a caller
//! that does not use it:
//!
//! - `src/streams/send.rs` - `StreamTx::resend(seq_no, now)`: queues the
//!   cached packet exactly as `handle_nack` does (RTX stream, ahead of new
//!   packets) and returns whether it was in the cache. A stream without an
//!   RTX payload type caches nothing, so it resends nothing.
//!
//! ## Eleventh change: read what a receiver reported (2026-10-05)
//!
//! ADR-0070 step 4. The SFU raises a subscriber's playout delay after a
//! burst of loss on its downlink, which it knows from the subscriber's
//! receiver reports and NACKs. Upstream exposes both only through the
//! periodic statistics events, averaged, and only with statistics on. Three
//! getters and their counters, read-only:
//!
//! - `src/streams/send.rs` - `StreamTx::nacked_packets` (sequence numbers
//!   asked for by NACK that were in the cache, each request counted; a
//!   `resend` of the tenth change is not counted),
//!   `StreamTx::receiver_reports` and `StreamTx::last_fraction_lost` (for
//!   the main SSRC). Three tests added for the tenth and eleventh changes,
//!   in a `lakko_test` module at the end of the file. One of them pins what
//!   was found while reading for ADR-0070: upstream has no hold-off on
//!   answering a NACK, every request for a cached packet queues a resend.
//!
//! The playout-delay header extension itself needed no change: upstream
//! parses, writes and negotiates `Extension::PlayoutDelay` once it is in the
//! session's extension map.
//!
//! By `git status -- crates/vendor/str0m` on the tree that carried changes
//! one to eight committed, the ninth to eleventh touch six files
//! (`register_nack.rs`, `register.rs`, `receive.rs`, `streams/mod.rs`,
//! `session.rs`, `send.rs`), all of them already pinned below. Not compared
//! against the registry copy of pristine 0.19.0.
//!
//! ## Twelfth change: transport-wide congestion control per direction (2026-10-05)
//!
//! [ADR-0070](../../../docs/adr/0070-call-media-through-a-loss-burst.md)
//! step 6. The SFU estimates each subscriber's downlink with str0m's
//! send-side estimator, which needs the transport-wide sequence number on
//! the packets the SFU sends and `transport-cc` feedback for them. Under
//! server-side bitrate control (ADR-0068) the same peer must NOT be given
//! either on the media lines it publishes on, or its own send-side
//! congestion controller would take over its publishing bitrate again.
//! Upstream negotiates both from one session extension map and one codec
//! configuration, on every media line alike. One switch, off by default;
//! with it off nothing changes:
//!
//! - `src/session.rs` - `Session::twcc_on_local_media_only`, a field.
//! - `src/change/direct.rs` - `DirectApi::set_twcc_on_local_media_only`.
//! - `src/change/sdp.rs` - with the switch on, a media line the REMOTE side
//!   offered (`Media::remote_created`) is given no transport-wide sequence
//!   number in `update_media` (so the extension is neither echoed in the
//!   answer nor expected on the line) and no `transport-cc` feedback line
//!   in `as_sdp`; a media line this side offers is written as before. The
//!   `AsSdpMediaLine` trait gains `remote_created`, `false` by default.
//!
//! What is not changed: transport-wide feedback is still enabled per
//! session when any media line negotiated it, and a peer that sends the
//! extension on a line that did not negotiate it is still reported on.
//!
//! ## Thirteenth change: hand out the transport-wide feedback records (2026-10-05)
//!
//! ADR-0070 step 6. The SFU runs its own estimate of each subscriber's
//! downlink (`services/sfu/src/uplink_estimate.rs`, the rules of a
//! publisher's uplink), because str0m's own send-side estimator does not
//! serve an SFU that pauses tracks: measured through the SFU it took a
//! second of loss on a 7.5 Mbit/s path to 178 kbit/s, could not rise
//! above 1.5 times what was being sent while the heavy tracks were
//! paused, and lowered the estimate of an unlimited path on the word of
//! its first probe. An estimator of the SFU's own needs, per packet it
//! sent, the send time, the size and the arrival time the receiver's
//! transport-wide feedback reports (or that it reports none). str0m
//! computes exactly those records for its own estimator and exposes none
//! of them. One hook, off by default, read-only:
//!
//! - `src/session.rs` - `Session::keep_twcc_feedback` and
//!   `Session::twcc_feedback`, two fields; in `handle_rtcp` the records of
//!   a report are copied into the second when the first is set (bounded:
//!   `MAX_TWCC_FEEDBACK_KEPT`, 8192, the oldest dropped) and then go to
//!   str0m's own estimator as before; a unit test in the file.
//! - `src/change/direct.rs` - `DirectApi::keep_twcc_feedback` and
//!   `DirectApi::take_twcc_feedback`.
//!
//! It does not need `RtcConfig::enable_bwe`, and the SFU does not enable
//! it under downlink control: str0m's estimator and its pacer stay off.
//!
//! (An earlier thirteenth change, `Bwe::current_estimate`, was made and
//! taken out again the same day with the estimator it read;
//! `src/bwe/api.rs` is as upstream has it and is not pinned.)
//!
//! By `git status -- crates/vendor/str0m` on the tree that carried changes
//! one to eleven (nine to eleven uncommitted), the twelfth and thirteenth
//! touch three files: `session.rs` and `change/direct.rs`, already
//! pinned, and `change/sdp.rs`, pinned from now on. Not compared against
//! the registry copy of pristine 0.19.0.
//!
//! ## The fourteenth change: a retransmission carries no padding
//!
//! Upstream pads every RTP packet it sends to the SRTP block size (16
//! bytes of payload) and sets the padding bit. For a first transmission
//! that is as upstream has it. For a retransmission (an RTX packet: a
//! NACK's answer, `StreamTx::resend`, a spurious resend used as padding)
//! the fork adds none:
//!
//! - `src/streams/send.rs` - in `poll_packet`, `NextPacketKind::Resend`
//!   is written without `RtpHeader::pad_packet`.
//!
//! By RFC 4588 and RFC 3550 the padded packet was correct. A libwebrtc
//! receiver restores the original behind a copy of the RTX packet's
//! header, padding bit included, without the padding, and the restored
//! packet then loses as many bytes from its tail as its last byte says.
//! Seen on a live call (2026-10-07): of 12 957 second copies of light
//! video frames a Chrome viewer restored, 848 were intact, which is the
//! one in sixteen whose RTX packet needed no padding, and 12 109 were
//! the frame less its last byte, 0x01. One file, already pinned; the
//! test is `a_retransmission_restores_to_the_payload_that_was_sent` in
//! that file.
//!
//! ## The fifteenth change: what is missing can be given up (2026-10-09)
//!
//! [ADR-0070](../../../docs/adr/0070-call-media-through-a-loss-burst.md),
//! note of 2026-10-09. Upstream (and the fork until now) asks for a missing
//! packet until its request count runs out or it leaves the window, whatever
//! has happened since. `StreamRx::give_up_missing` ends the requests for
//! every packet of a receive stream that is missing at that moment:
//!
//! - `src/streams/register_nack.rs` - `NackRegister::give_up_missing` sets
//!   the request count of every missing packet in the active window to its
//!   maximum; one test added.
//! - `src/streams/register.rs`, `src/streams/receive.rs` - pass it through
//!   (`ReceiverRegister::give_up_missing`, `StreamRx::give_up_missing`).
//!
//! Nothing else changes: the packets stay missing, one that still arrives is
//! taken as new, and a packet that goes missing afterwards is asked for on
//! the usual schedule. The SFU calls it when it has asked the publisher for a
//! key frame at the end of a burst of loss that was not repaired, and for a
//! stream nobody is being sent (`media::drive_burst_keyframes`): each further
//! request would be a retransmission sent ahead of the key frame on a path
//! that has just recovered. Three files, all already pinned.
//!
//! ## The sixteenth change: an unpadded packet under AES-CM (2026-10-16)
//!
//! The fourteenth change sends a retransmission without padding, but the
//! `AES_CM_128_HMAC_SHA1_80` branch of `SrtpContext::protect_rtp` kept
//! upstream's `assert!` that the body is a multiple of 16 bytes. With the
//! release profile's `panic = "abort"` the first retransmission to a peer that
//! negotiated that profile took the whole SFU down, and every call with it
//! (seen on test.lakko.me, 2026-10-16, payload type 107). AES counter mode
//! needs no block alignment, and the rust-crypto backend encrypts any length:
//!
//! - `src/rtp/srtp.rs` - the assert is removed; one test protects and
//!   unprotects a 21-byte body.
//!
//! The AEAD-GCM profiles never asserted, which is why the fourteenth
//! change's tests and the synthetic client, both on AEAD-GCM, passed.
//!
//! ## What this guard is for
//!
//! `crates/vendor/str0m` is ~40k lines of third-party code that nobody on
//! this project reviews. This test freezes the files the fork touches,
//! so an edit to any of them — an accidental one above all — fails CI and
//! forces a conscious look. It says nothing about the other files; those are
//! upstream-verbatim by virtue of never having been opened.
//!
//! When the failure is intentional (an upstream rebase, or a reviewed change
//! to the fork):
//!   1. `diff -ru` the vendored `src` against the pristine registry copy of
//!      the same version and confirm the delta is still only the capability
//!      described above;
//!   2. update the hash this test prints.
//!
//! The second change was confirmed on 2026-10-03 the same way: the delta is
//! now eleven files, the six above plus the five NACK files.
//!
//! The third change was confirmed on 2026-10-04 the same way (`diff -rq`
//! against the registry copy): nineteen files, the eleven above plus the
//! eight RED files.
//!
//! The fourth change (2026-10-04) touches six of those nineteen files and no
//! others, by `git diff --stat -- crates/vendor/str0m` over the working tree
//! that carried the third change uncommitted. It was NOT re-confirmed against
//! the registry copy of pristine 0.19.0; do that before relying on the count.
//!
//! First confirmed on 2026-09-18 against the registry-cached pristine
//! `str0m` 0.19.0: six files, +63 / -3 lines, and all three removals are
//! lines replaced in place (two re-export lists gaining `FramePosition`, and
//! the `MediaData` construction switching to `ext_vals_for_frame()`).

use sha2::{Digest, Sha256};

/// The vendored files the fork touches, relative to `crates/vendor/str0m`,
/// each with the SHA-256 of its LF-normalized contents at the reviewed fork
/// state.
const PINNED: &[(&str, &str)] = &[
    (
        "src/rtp/ext.rs",
        "a843d9b1c70b44236fd9da9804d3943f5be4fc08041ae0b1b1f52ed8580a550a",
    ),
    (
        "src/rtp/mod.rs",
        "6373fdd79ccf8c3d866241b40f50c2eba809ec0b312e250e5179ff150670f46f",
    ),
    (
        "src/packet/payload.rs",
        "e028e08b64b016d96330c4bccf9f6538bf1c09f0c5e60d427acca48f75c62d9e",
    ),
    (
        "src/packet/buffer_rx.rs",
        "52e2162a35c7b9e68f44ccf194ca41d13dbc08aa874e73a1a26d9fdc7dd8bfaa",
    ),
    (
        "src/media/mod.rs",
        "1d280e4a578ab2fae4a350290618a5d8cb9381b0e32e32ccab35afeec9ec36cc",
    ),
    (
        "src/lib.rs",
        "f365c38ac92b64a86ec78a154d195fefd7166320203026538dc4eb3c463d73e4",
    ),
    (
        "src/streams/register_nack.rs",
        "ba87d6479f618c2898f494dfe79c1c1ba5624e71fad68ca8dc220b9fc4c0a4ae",
    ),
    (
        "src/streams/register.rs",
        "f4e40ef8e4e96035e559160aaead050d58b364d3172b768e7ccd915ce7c025ca",
    ),
    (
        "src/streams/receive.rs",
        "6c52c3d6519a516289c6cd8f2626372286041bb2bec0cd42b7c45035a1db4c28",
    ),
    (
        "src/streams/mod.rs",
        "dbfc18db1a403814096f2a75f6c1d45fc44b2119073ab996f19b8ea73ed9c4c8",
    ),
    (
        "src/_internal_test_exports/fuzz.rs",
        "bae5c3b1d66c072128aae974e3b2f859b70cc1cd3beeb001c75cd143733cb577",
    ),
    (
        "src/format/codec.rs",
        "2fbebe989cc36a0c2a895757ad92972749c76790081ef88205409c06542b4adc",
    ),
    (
        "src/format/codec_config.rs",
        "d4c49c71cfab81fa35ca8acf748b7015c637306addfd0ead5ed57dfa2b0f1b6f",
    ),
    (
        "src/format/format_params.rs",
        "2e135c425ce0cf23409369a8e43bbb1a412a50164daf72a31ebf5e71e51a7354",
    ),
    (
        "src/format/payload_params.rs",
        "a9833b7a9065a6714c633930090a9f187604d5e95dba4e04abf215dc4cb5b012",
    ),
    (
        "src/format/mod.rs",
        "92b575ec6c27fb667e0c118823dcc89137ada8affd3582695b82c2b7a5d19308",
    ),
    (
        "src/packet/mod.rs",
        "bc6d39baf66d2baf19032c25ed5b03b7c5d50b6c91060c346aac518d6d8cc842",
    ),
    (
        "src/sdp/data.rs",
        "2e7a85008e7abbb73adb8595bbc579b14bed4adfb96dcdb08dcd43ecd6e7d318",
    ),
    (
        "src/sdp/parser.rs",
        "2c95e93b07589857b7b88e25502d9cda92984c4cd69a4fc49ffd152f0c77568e",
    ),
    (
        "src/session.rs",
        "a79e27702f8ca4a08d65ee2053bc918c409790150e10f2da86e42a27af4bb0c9",
    ),
    (
        "src/streams/send.rs",
        "d80928fccf18cef13fbb741aabb272df00391f235fa0154ab87ae33dde6d084c",
    ),
    (
        "src/change/direct.rs",
        "793584faf3371099a3cc2f533a9e6b42402f43c2b771e71f9919919a3ac45a8b",
    ),
    (
        "src/change/sdp.rs",
        "12274a94b56b88b0c4893e482f9eff14b1ac7040dffb35288c79639bd7603ce5",
    ),
    (
        "src/rtp/srtp.rs",
        "b2cdd8f0433fdf7bf2484531f17a3640cbf47e1882672d40e59784b17eec3b94",
    ),
];

/// Root of the vendored fork, relative to this crate's manifest dir.
const VENDOR_ROOT: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/../../crates/vendor/str0m");

/// Read a vendored file and normalize line endings to LF so the hash does not
/// depend on the checkout's autocrlf setting.
fn read_normalized(rel: &str) -> String {
    let path = format!("{VENDOR_ROOT}/{rel}");
    let raw = std::fs::read_to_string(&path)
        .unwrap_or_else(|e| panic!("cannot read vendored str0m file at {path}: {e}"));
    raw.replace("\r\n", "\n").replace('\r', "\n")
}

fn sha256_hex(s: &str) -> String {
    let digest = Sha256::digest(s.as_bytes());
    let mut out = String::with_capacity(64);
    for b in digest {
        use std::fmt::Write as _;
        let _ = write!(out, "{b:02x}");
    }
    out
}

#[test]
fn vendored_str0m_matches_pinned_fork_state() {
    let mut drifted = Vec::new();
    for (rel, pinned) in PINNED {
        let actual = sha256_hex(&read_normalized(rel));
        if actual != *pinned {
            drifted.push(format!(
                "    (\n        {rel:?},\n        {actual:?},\n    ),"
            ));
        }
    }
    assert!(
        drifted.is_empty(),
        "the vendored str0m fork changed.\n\n\
         Confirm the delta against pristine str0m 0.19.0 is still only the \
         per-packet `frame_position` capability, the NACK retry schedule, \
         the audio `red` payload type, the video `red` + `ulpfec` pair, the \
         receiver-report loss clamp, the FEC hooks on `DirectApi`, the \
         per-stream NACK schedule, `StreamTx::resend`, the receiver \
         feedback getters, transport-wide congestion control per direction \
         the transport-wide feedback records and retransmissions without \
         padding (see this file's module \
         doc), then \
         re-pin:\n\n{}\n",
        drifted.join("\n")
    );
}

/// The point of the whole fork, asserted against the library rather than
/// described in prose: a serializer is told where its packet sits in the
/// frame. If an upstream rebase silently dropped the field, the hash guard
/// above would fire — but so would this, with a message that says what was
/// lost rather than that bytes moved.
#[test]
fn the_fork_capability_is_present() {
    use str0m::rtp::{ExtensionValues, FramePosition};

    let mut ev = ExtensionValues::default();
    assert_eq!(
        ev.frame_position, None,
        "a value that did not come from the packetizer has no frame position, \
         and a serializer must be able to tell that from a real position"
    );
    ev.frame_position = Some(FramePosition {
        first: true,
        last: false,
    });
    assert_eq!(
        ev.frame_position,
        Some(FramePosition {
            first: true,
            last: false
        })
    );
}

/// The third fork change, asserted against the library: audio RED can be
/// switched on, and it then sits before Opus and names Opus's payload type.
#[test]
fn the_red_capability_is_present() {
    use str0m::format::{Codec, CodecConfig};

    let mut config = CodecConfig::new_with_defaults();
    assert!(
        config.find(|p| p.spec().codec == Codec::Red).is_none(),
        "red is opt-in, not part of the default codec set"
    );
    config.enable_red(true);
    let audio: Vec<_> = config
        .params()
        .iter()
        .filter(|p| p.spec().codec.is_audio())
        .collect();
    assert_eq!(audio.len(), 2);
    assert_eq!(audio[0].spec().codec, Codec::Red, "red precedes opus");
    assert_eq!(audio[1].spec().codec, Codec::Opus);
    assert_eq!(
        audio[0].spec().format.red.map(|red| red.pt),
        Some(audio[1].pt()),
        "the red fmtp names the Opus payload type"
    );
    assert_eq!(audio[0].resend(), None, "audio has no RTX");
    assert!(!audio[0].fb_nack(), "audio has no NACK");
}

/// The fourth fork change, asserted against the library: video FEC can be
/// switched on, and it then adds `red` with an RTX and `ulpfec` without one,
/// distinct from audio RED.
#[test]
fn the_video_fec_capability_is_present() {
    use str0m::format::{Codec, CodecConfig};

    let mut config = CodecConfig::new_with_defaults();
    for codec in [Codec::RedVideo, Codec::Ulpfec] {
        assert!(
            config.find(|p| p.spec().codec == codec).is_none(),
            "video fec is opt-in, not part of the default codec set"
        );
    }
    config.enable_red(true);
    config.enable_video_fec(true);
    let red = config
        .find(|p| p.spec().codec == Codec::RedVideo)
        .expect("video red");
    let ulpfec = config
        .find(|p| p.spec().codec == Codec::Ulpfec)
        .expect("ulpfec");
    assert!(red.spec().codec.is_video() && ulpfec.spec().codec.is_video());
    assert!(
        red.resend().is_some(),
        "a retransmitted RED packet needs an RTX payload type to come back under"
    );
    assert_eq!(ulpfec.resend(), None, "an FEC packet travels inside RED");
    assert!(!red.fb_nack() && !ulpfec.fb_nack(), "no feedback lines");
    let audio_red = config
        .find(|p| p.spec().codec == Codec::Red)
        .expect("audio red");
    assert_ne!(audio_red.pt(), red.pt());
    assert!(audio_red.spec().codec.is_audio());
}

/// The sixth fork change, asserted against the library: a received packet
/// can be asked for whole, and a packet can be handed in as recovered. On a
/// session that has received nothing, both answer `None` and nothing panics.
#[test]
fn the_fec_receiver_hooks_are_present() {
    use std::time::Instant;
    use str0m::Rtc;

    let mut rtc = Rtc::builder().set_rtp_mode(true).build(Instant::now());
    let mut api = rtc.direct_api();
    assert_eq!(api.take_received_rtp(), None, "nothing is kept by default");
    api.keep_received_rtp(true);
    assert_eq!(api.take_received_rtp(), None, "and nothing was received");
    // Not RTP, and RTP for a stream the session does not have.
    assert!(api.handle_recovered_rtp(Instant::now(), &[]).is_none());
    let mut packet = vec![0x80, 96, 0, 1, 0, 0, 0, 2, 0, 0, 0, 3];
    packet.extend_from_slice(b"payload");
    assert!(api.handle_recovered_rtp(Instant::now(), &packet).is_none());
}

/// The seventh fork change, asserted against the library: a session hands
/// out sent packets only for a stream that asked for them. On a session that
/// has sent nothing there is nothing to take, and nothing panics.
#[test]
fn the_fec_sender_hooks_are_present() {
    use std::time::Instant;
    use str0m::media::{Direction, MediaKind};
    use str0m::Rtc;

    let mut rtc = Rtc::builder().set_rtp_mode(true).build(Instant::now());
    assert_eq!(rtc.direct_api().take_sent_rtp(), None, "nothing was sent");
    let mut change = rtc.sdp_api();
    let mid = change.add_media(MediaKind::Video, Direction::SendOnly, None, None, None);
    let _ = change.apply().expect("an offer");
    let mut api = rtc.direct_api();
    if let Some(stream) = api.stream_tx_by_mid(mid, None) {
        stream.keep_sent_rtp(true);
        stream.keep_sent_rtp(false);
    }
    assert_eq!(api.take_sent_rtp(), None, "asking keeps nothing by itself");
}

/// The ninth, tenth and eleventh fork changes, asserted against the library:
/// a receive stream can be given a NACK schedule of its own, and a send
/// stream can be asked to resend and to say what its receiver reported. On a
/// session that has received and sent nothing, nothing is resent and every
/// counter is zero.
#[test]
fn the_loss_burst_hooks_are_present() {
    use std::time::{Duration, Instant};
    use str0m::media::{MediaKind, Mid};
    use str0m::Rtc;

    let now = Instant::now();
    let mut rtc = Rtc::builder().set_rtp_mode(true).build(now);
    let mid = Mid::from("v");
    let mut api = rtc.direct_api();
    let _ = api.declare_media(mid, MediaKind::Video);
    let stream = api.declare_stream_tx(1.into(), Some(2.into()), mid, None);
    assert!(!stream.resend(1.into(), now), "nothing was sent");
    assert_eq!(stream.nacked_packets(), 0);
    assert_eq!(stream.receiver_reports(), 0);
    assert_eq!(stream.last_fraction_lost(), 0);
    let stream = api.expect_stream_rx(7.into(), Some(8.into()), mid, None);
    stream.set_nack_retry_interval(Some(Duration::from_millis(20)));
    stream.set_nack_retry_interval(None);
}

/// The fifteenth fork change, asserted against the library: a receive stream
/// can be told to stop asking for what is missing. On a stream that has
/// received nothing, nothing is given up.
#[test]
fn missing_packets_can_be_given_up() {
    use std::time::Instant;
    use str0m::media::{MediaKind, Mid};
    use str0m::Rtc;

    let mut rtc = Rtc::builder().set_rtp_mode(true).build(Instant::now());
    let mid = Mid::from("v");
    let mut api = rtc.direct_api();
    let _ = api.declare_media(mid, MediaKind::Video);
    let stream = api.expect_stream_rx(7.into(), Some(8.into()), mid, None);
    assert_eq!(stream.give_up_missing(), 0);
}

/// The twelfth fork change, asserted against the library: with the switch
/// on, a media line the remote side offered is answered without the
/// transport-wide sequence number and without `transport-cc` feedback, and
/// a media line this side offers carries both. With it off (the default)
/// both carry both.
#[test]
fn transport_wide_congestion_control_can_be_per_direction() {
    use std::time::Instant;
    use str0m::change::SdpOffer;
    use str0m::media::{Direction, MediaKind};
    use str0m::Rtc;

    let sections = |sdp: &str| -> Vec<String> {
        sdp.split("\r\nm=")
            .skip(1)
            .map(|section| format!("m={section}"))
            .collect()
    };
    let names_it =
        |section: &str| section.contains("transport-wide-cc") && section.contains(" transport-cc");
    let names_none =
        |section: &str| !section.contains("transport-wide-cc") && !section.contains("transport-cc");

    for on in [false, true] {
        let now = Instant::now();
        let mut remote = Rtc::builder().build(now);
        let mut change = remote.sdp_api();
        change.add_media(MediaKind::Audio, Direction::SendOnly, None, None, None);
        change.add_media(MediaKind::Video, Direction::SendOnly, None, None, None);
        let (offer, _pending) = change.apply().expect("an offer");
        let offer = offer.to_sdp_string();
        assert!(sections(&offer).iter().all(|s| names_it(s)));

        let mut local = Rtc::builder().build(now);
        local.direct_api().set_twcc_on_local_media_only(on);
        let answer = local
            .sdp_api()
            .accept_offer(SdpOffer::from_sdp_string(&offer).expect("parses"))
            .expect("answers")
            .to_sdp_string();
        let answered = sections(&answer);
        assert_eq!(answered.len(), 2);
        for section in &answered {
            assert!(
                if on {
                    names_none(section)
                } else {
                    names_it(section)
                },
                "switch {on}: the answer to a remote line:\n{section}"
            );
            assert!(section.contains("abs-send-time"), "the rest stays");
        }
        assert!(answered[1].contains("goog-remb") && answered[1].contains("nack"));

        // A line this side offers, in a later offer of its own.
        let mut change = local.sdp_api();
        change.add_media(MediaKind::Video, Direction::SendOnly, None, None, None);
        let (own, _pending) = change.apply().expect("an offer");
        let own = sections(&own.to_sdp_string());
        assert_eq!(own.len(), 3);
        for section in &own[..2] {
            assert!(
                if on {
                    names_none(section)
                } else {
                    names_it(section)
                },
                "switch {on}: a remote line in a later offer:\n{section}"
            );
        }
        assert!(names_it(&own[2]), "switch {on}: an own line:\n{}", own[2]);
    }
}

/// The thirteenth fork change, asserted against the library: the records
/// of the remote side's transport-wide feedback can be kept and taken, and
/// nothing is kept unless asked.
#[test]
fn transport_wide_feedback_records_can_be_taken() {
    use std::time::Instant;
    use str0m::Rtc;

    let now = Instant::now();
    let mut rtc = Rtc::builder().set_rtp_mode(true).build(now);
    let mut api = rtc.direct_api();
    assert!(api.take_twcc_feedback().is_empty());
    api.keep_twcc_feedback(true);
    let records: Vec<(Instant, usize, Option<Instant>)> = api.take_twcc_feedback();
    assert!(records.is_empty(), "nothing was reported yet");
    api.keep_twcc_feedback(false);
    assert!(api.take_twcc_feedback().is_empty());
}

/// What ADR-0070 step 4 relies on and the fork did not have to add: the
/// playout-delay header extension is one upstream knows.
#[test]
fn upstream_knows_the_playout_delay_extension() {
    use str0m::rtp::{Extension, ExtensionMap};

    let mut map = ExtensionMap::standard();
    assert_eq!(
        map.id_of(Extension::PlayoutDelay),
        None,
        "not in the standard map: a session has to ask for it"
    );
    map.set(5, Extension::PlayoutDelay);
    assert_eq!(map.id_of(Extension::PlayoutDelay), Some(5));
    assert_eq!(
        Extension::PlayoutDelay.as_uri(),
        "http://www.webrtc.org/experiments/rtp-hdrext/playout-delay"
    );
}
