// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! When a burst of loss on a publisher's video stream has ended and left
//! packets nobody will repair, so that the SFU itself asks the publisher for
//! a key frame instead of waiting for a subscriber to do it
//! ([ADR-0070](../../../docs/adr/0070-call-media-through-a-loss-burst.md),
//! notes of 2026-10-09 and 2026-10-11).
//!
//! ## Why
//!
//! Seen on a live call: a home Wi-Fi uplink lost about 80% of its datagrams
//! for one second every 30 s. The SFU asked for the missing packets all
//! through the burst and after it, and most never came. Every viewer then
//! held frames it could not decode until its own decoder gave up and asked
//! for a key frame, about three seconds after the burst had ended. The SFU
//! knew at the end of the burst what the viewers found out three seconds
//! later.
//!
//! ## The rule
//!
//! Per video stream of a publisher (main, middle and light each on its own;
//! each simulcast layer on its own), from the sequence numbers of the packets
//! as they reach the SFU, retransmissions, FEC packets and packets rebuilt
//! from FEC included:
//!
//! - a **burst** opens with the first missing packet and stays open while
//!   packets go on missing;
//! - it has **ended** when [`BURST_END_PACKETS`] packets in a row arrived in
//!   sequence and no new packet went missing for [`BURST_END_QUIET`];
//! - it is **judged** when, at that moment or later, a packet is still
//!   missing [`UNREPAIRED_AFTER`] after it was first known missing, or when
//!   the burst left packets that retransmission cannot cover: missing for
//!   longer than [`RETRANSMISSION_SPAN`], or further than
//!   [`RETRANSMISSION_WINDOW`] packets behind the newest;
//! - a burst that ended with every packet there is closed without a word.
//!   So is one whose missing packets all precede a key frame that has
//!   started arriving since.
//!
//! One answer per burst ([`BurstEnds::poll`]). A packet that goes missing
//! before the answer is the same burst going on, and the wait starts over.
//!
//! ## What a missing packet was
//!
//! A sequence number that stays missing is not always media. A publisher
//! that sends ULPFEC inside RED numbers its FEC packets in the sequence of
//! the media stream and does not send a lost FEC packet again (libwebrtc
//! M125 `ulpfec_generator.cc`, `GetFecPackets`: the FEC packet reuses the
//! last media packet's header and is marked not to be retransmitted; FEC
//! and retransmissions are themselves not protected, so a mask names media
//! only). When the burst is judged, each packet
//! still missing is one of three kinds ([`BurstEnd::media`],
//! [`BurstEnd::not_media`], [`BurstEnd::unknown`]):
//!
//! - **known media**, by either of two facts:
//!   - an FEC packet that did arrive lists it as protected
//!     ([`BurstEnds::fec_protects`]): FEC protects media packets only;
//!   - the frame descriptors of the packets around it
//!     ([`BurstEnds::on_described`]). Between two media packets that
//!     arrived, with nothing but missing packets and FEC packets between
//!     them: a packet on the left that does not end its frame, or one on
//!     the right that does not begin its frame, means a packet of that
//!     frame is missing; and frame ids that step by more than one across
//!     the gap mean whole frames are missing, one packet at least each;
//! - **known not media**: every packet missing between a packet that ends
//!   frame `n` and one that begins frame `n + 1`. Nothing of any frame lies
//!   there: an FEC packet, or nothing anybody needs;
//! - **unknown**: everything else. A packet with no descriptor next to it,
//!   a packet let go before the burst was judged (see the bounds), a gap
//!   whose frames cannot be counted.
//!
//! The answer:
//!
//! - one packet of known media still missing: the burst ended
//!   **unrepaired** ([`Why::MediaMissing`]), whatever share of the stream
//!   is FEC;
//! - packets known not to be media never ask for anything
//!   ([`Why::NotMedia`] when they are all that is missing);
//! - the unknown ones are judged by number, as every missing packet was
//!   before 2026-10-11. On a stream on which no FEC packet has been seen,
//!   one is unrepaired. On a stream that carries FEC, unrepaired only when
//!   more of them stay missing than [`FEC_SLACK`] plus one and a half times
//!   the FEC packets the burst is expected to have lost and that are not
//!   already accounted for as known not media: the FEC share of the stream
//!   times what the burst lost, less the known not media
//!   ([`Why::Unexplained`], else [`Why::WithinFecShare`]). The share is
//!   counted over the packets that arrived in sequence, lately.
//!
//! ### What the descriptor facts rest on, and when they are not used
//!
//! - **Frame ids.** The generic frame descriptor carries a 16-bit frame id
//!   on the first packet of a frame; it wraps, and ids are compared by
//!   their wrapping difference (a step of `0x8000` or more says nothing).
//!   A libwebrtc sender numbers the frames of one sender one after the
//!   other, but the simulcast layers of one sender share a counter, so a
//!   layer's ids skip (libwebrtc M125 `rtp_video_sender.cc`,
//!   `OnEncodedImage`: one counter per sender, stepped once per encoded
//!   image and used as the frame id unless a field trial asks for
//!   independent ids). The id is given before an encoded transform runs,
//!   so a frame the transform drops also leaves a step of two with nothing
//!   lost on the way; that costs one key frame nobody needed. The rule
//!   therefore
//!   learns it per stream: frame ids are used only after
//!   [`FRAME_ID_TRUST`] frames in a row began exactly one id after the
//!   frame before with no packet missing between, and never again on that
//!   stream once two such frames were seen to differ by anything else.
//! - **Frame boundaries.** "The packet after one that does not end its
//!   frame is media" holds only if FEC packets sit between frames, never
//!   inside one (libwebrtc M125 `ulpfec_generator.cc`,
//!   `AddPacketAndGenerateFec`: FEC is generated only on a packet that
//!   carries the marker bit; that the pacer keeps a frame's packets
//!   together was not read). A stream on which
//!   an FEC packet was seen right after a packet that does not end its
//!   frame, or right before one that does not begin its frame, loses that
//!   inference for good.
//! - A packet without the extension, an FEC packet (libwebrtc copies the
//!   last media packet's extensions onto it, so its descriptor is not its
//!   own) and a descriptor value libwebrtc's parser would refuse say
//!   nothing: the packets missing next to them are unknown.
//!
//! The quiet time doubles as the last chance of retransmission: by the time a
//! burst counts as ended, every packet still missing has been asked for at
//! least once more on a path that is carrying packets again (every 60 ms,
//! every 20 ms on a light stream).
//!
//! ## Bounds
//!
//! Every fact here is chosen by an admitted peer: sequence numbers, FEC
//! masks, descriptors. At most [`MAX_STREAMS`] streams per publisher; per
//! stream at most [`RETRANSMISSION_WINDOW`] missing packets (a time and one
//! flag each) and a fixed table of [`ARRIVED_WINDOW`] eight-byte entries
//! for the packets that arrived lately (their kind, the two boundary flags
//! and a frame id), plus a handful of counters. A packet costs one table
//! write and, when it steps over missing ones, one map entry each; an FEC
//! mask is read only while packets are missing on its stream, and marks at
//! most [`crate::fec_receive::MAX_PROTECTED`] of them. The kinds are worked
//! out once per burst, when it is judged, in one pass over the missing
//! packets and the table. A missing packet let go before that (older than
//! the span, or pushed out of the window) keeps only what an FEC packet
//! said of it: known media, else unknown.
//!
//! ## What it is not
//!
//! Pure bookkeeping over header fields: no I/O, no clock of its own (the
//! caller supplies every time), no payload, no key (C-39). Asking the
//! publisher, and deciding whether anybody is being sent the stream, is the
//! caller's ([`crate::media`]).
//!
//! Every constant below was chosen, not measured.

use std::collections::{BTreeMap, HashMap};
use std::hash::Hash;
use std::time::{Duration, Instant};

use crate::frame_descriptor::PacketPosition;

/// How long no new packet may have gone missing on a stream for its burst to
/// count as ended. Chosen: two rounds of the SFU's 60 ms retransmission
/// schedule, so what is still missing afterwards was asked for on the
/// recovered path and did not come.
pub const BURST_END_QUIET: Duration = Duration::from_millis(120);

/// How many packets in a row must have arrived in sequence since the last
/// one went missing. Chosen: in a burst that loses eight datagrams in ten,
/// five in a row arrive by chance about once in three thousand tries, so a
/// lull inside the burst is not taken for its end; at the 50 packets a second
/// of a light stream it is a tenth of a second.
pub const BURST_END_PACKETS: u32 = 5;

/// How long after a packet was first known missing it counts as not
/// repaired. Chosen: the playout delay a viewer is asked to hold after a
/// burst ([`crate::playout_delay::BURST_DELAY_MS`]). A packet that arrives
/// later than that is too late for the frame it belongs to.
pub const UNREPAIRED_AFTER: Duration = Duration::from_millis(300);

/// How long the SFU keeps asking a publisher for one missing packet: 25
/// requests 60 ms apart in the vendored `str0m`
/// (`streams/register_nack.rs`). A packet missing for longer will not be
/// asked for again.
pub const RETRANSMISSION_SPAN: Duration = Duration::from_millis(1500);

/// How far behind the newest packet of its stream a missing packet is still
/// asked for (`MAX_MISORDER` in the vendored `str0m`).
pub const RETRANSMISSION_WINDOW: u64 = 512;

/// The largest step between two sequence numbers read as loss (forward) or
/// as a late packet (back). A larger one, either way, is a stream that
/// started over, which is not a burst. Chosen: about forty seconds of a 250
/// packet a second stream.
pub const MAX_JUMP: u64 = 10_000;

/// Most streams of one publisher kept. A stream beyond that starts the table
/// over. Chosen: three senders or three simulcast layers, camera and screen,
/// with room to spare.
pub const MAX_STREAMS: usize = 12;

/// On a stream that carries FEC, how many packets may stay missing beyond
/// the FEC share of what a burst lost before the burst counts as
/// unrepaired. Chosen: a lost FEC packet or two must never cost a key
/// frame.
pub const FEC_SLACK: u64 = 2;

/// How many packets that arrived in sequence the FEC share of a stream is
/// counted over; at this many both counts are halved. Chosen: a few seconds
/// of a main stream, tens of seconds of a light one.
pub const FEC_SHARE_WINDOW: u32 = 1024;

/// How many packets that arrived lately a stream remembers the kind and the
/// frame position of: twice the window missing packets are tracked in, so
/// the packets on both sides of every tracked missing one are in it.
pub const ARRIVED_WINDOW: u64 = 2 * RETRANSMISSION_WINDOW;

/// How many frames in a row must have begun exactly one frame id after the
/// frame before, with no packet missing between, before the frame ids of a
/// stream are used to say what a missing packet was. Chosen: a quarter of a
/// second of a 30 frames a second stream.
pub const FRAME_ID_TRUST: u32 = 8;

/// What a packet handed to [`BurstEnds::on_packet`] is, by its header.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Arrived {
    /// A media packet.
    Media,
    /// The first media packet of a frame with no dependencies, by its
    /// frame descriptor: every packet before it is no longer needed by
    /// anybody.
    KeyFrameStart,
    /// An FEC packet (it takes a sequence number of the stream).
    Fec,
}

/// Why a burst that ended with packets still missing asks for a key frame
/// or does not.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Why {
    /// A packet known to be media is still missing. Asks.
    MediaMissing,
    /// More packets of unknown kind are missing than lost FEC packets
    /// explain; on a stream without FEC, any. Asks.
    Unexplained,
    /// Every packet still missing is known not to be media. Asks nothing.
    NotMedia,
    /// The packets of unknown kind still missing are no more than the lost
    /// FEC packets of the burst explain. Asks nothing.
    WithinFecShare,
}

impl Why {
    /// Whether the burst ended unrepaired: the publisher is to be asked for
    /// a key frame.
    #[must_use]
    pub const fn asks(self) -> bool {
        matches!(self, Self::MediaMissing | Self::Unexplained)
    }

    /// Stable, low-cardinality label for logs.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::MediaMissing => "media-missing",
            Self::Unexplained => "unexplained",
            Self::NotMedia => "not-media",
            Self::WithinFecShare => "within-fec-share",
        }
    }
}

/// A burst that ended with packets still missing on one stream, and what
/// was made of it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BurstEnd<K> {
    /// The stream, as the caller named it.
    pub stream: K,
    /// Packets that went missing during the burst, repaired or not.
    pub lost: u64,
    /// Of those, how many never arrived: [`Self::media`] +
    /// [`Self::not_media`] + [`Self::unknown`].
    pub unrepaired: u64,
    /// Of the packets that never arrived, how many are known to be media.
    pub media: u64,
    /// How many are known not to be media.
    pub not_media: u64,
    /// How many nothing is known of.
    pub unknown: u64,
    /// Whether some went beyond what retransmission covers
    /// ([`RETRANSMISSION_SPAN`], [`RETRANSMISSION_WINDOW`]).
    pub beyond_repair: bool,
    /// From the first missing packet to the last one that went missing.
    pub lasted: Duration,
    /// The verdict.
    pub why: Why,
}

impl<K> BurstEnd<K> {
    /// Whether the burst ended unrepaired ([`Why::asks`]).
    #[must_use]
    pub const fn asks(&self) -> bool {
        self.why.asks()
    }
}

/// The burst that is open on a stream.
#[derive(Debug, Clone, Copy)]
struct Burst {
    /// When the first packet was known missing.
    started: Instant,
    /// When the latest one was.
    last_hole: Instant,
    /// Packets that arrived in sequence since.
    in_order: u32,
    /// Packets that went missing in it.
    lost: u64,
    /// Of those, how many have arrived since.
    repaired: u64,
    /// Some of them are past retransmission.
    beyond_repair: bool,
    /// Missing packets let go before the burst was judged that an FEC
    /// packet had listed as protected: known media.
    gone_media: u64,
}

/// One packet known missing.
#[derive(Debug, Clone, Copy)]
struct Hole {
    /// When it was first known missing.
    seen: Instant,
    /// An FEC packet that arrived lists it as protected: it is media.
    protected: bool,
}

/// What a missing packet was.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Kind {
    Media,
    NotMedia,
    Unknown,
}

/// A packet arrived under this slot's sequence number.
const SLOT_PRESENT: u8 = 0x01;
/// It was an FEC packet.
const SLOT_FEC: u8 = 0x02;
/// It was a media packet with a frame descriptor that says where in its
/// frame it sits.
const SLOT_DESCRIBED: u8 = 0x04;
/// It begins its frame.
const SLOT_BEGINS: u8 = 0x08;
/// It ends its frame.
const SLOT_ENDS: u8 = 0x10;
/// `frame` is the id of its frame.
const SLOT_FRAME: u8 = 0x20;

/// What is remembered of one packet that arrived: eight bytes.
#[derive(Debug, Clone, Copy, Default)]
struct Slot {
    /// Low 32 bits of the extended sequence number the slot holds.
    seq: u32,
    /// Its frame's id, when `SLOT_FRAME` is set.
    frame: u16,
    bits: u8,
}

impl Slot {
    const fn is(self, bit: u8) -> bool {
        self.bits & bit != 0
    }

    /// A media packet that says where in its frame it sits.
    const fn described(self) -> bool {
        self.is(SLOT_DESCRIBED) && !self.is(SLOT_FEC)
    }

    /// What this packet says to the missing packets next to it.
    const fn edge(self) -> Edge {
        if !self.described() {
            return Edge::Unknown;
        }
        Edge::Media {
            begins: self.is(SLOT_BEGINS),
            ends: self.is(SLOT_ENDS),
            frame: if self.is(SLOT_FRAME) {
                Some(self.frame)
            } else {
                None
            },
        }
    }
}

/// The nearest media packet on one side of a run of missing packets.
#[derive(Debug, Clone, Copy)]
enum Edge {
    /// None that says anything: no descriptor, or not remembered.
    Unknown,
    /// A media packet with a frame descriptor.
    Media {
        begins: bool,
        ends: bool,
        /// The id of its frame, when known.
        frame: Option<u16>,
    },
}

/// The low 32 bits of an extended sequence number.
fn low32(seq: u64) -> u32 {
    u32::try_from(seq & u64::from(u32::MAX)).unwrap_or(0)
}

/// One stream of a publisher.
#[derive(Debug)]
struct Stream {
    /// Highest extended sequence number seen.
    highest: u64,
    /// Missing sequence numbers. Never more than [`RETRANSMISSION_WINDOW`]
    /// entries: a hole further behind the newest packet is dropped and
    /// counted as beyond repair.
    holes: BTreeMap<u64, Hole>,
    burst: Option<Burst>,
    /// Packets that arrived in sequence lately, and how many of them were
    /// FEC ([`FEC_SHARE_WINDOW`]).
    in_sequence: u32,
    in_sequence_fec: u32,
    /// The packets that arrived lately, by sequence number modulo
    /// [`ARRIVED_WINDOW`]. Always that many entries.
    arrived: Vec<Slot>,
    /// The id of the newest frame that began, and whether no packet has
    /// gone missing since it did.
    last_frame: Option<u16>,
    unbroken_since_frame: bool,
    /// Frames in a row that began one id after the frame before
    /// ([`FRAME_ID_TRUST`]).
    frame_steps: u32,
    /// Two frames with nothing missing between them differed by another
    /// step: the ids of this stream do not count frames.
    frame_ids_skip: bool,
    /// An FEC packet was seen inside a frame: a frame's boundary flags say
    /// nothing about the packet missing next to them.
    fec_inside_frames: bool,
}

impl Stream {
    fn new(seq: u64, what: Arrived, part: Option<PacketPosition>) -> Self {
        let slots = usize::try_from(ARRIVED_WINDOW).unwrap_or(0);
        let mut stream = Self {
            highest: seq,
            holes: BTreeMap::new(),
            burst: None,
            in_sequence: 0,
            in_sequence_fec: 0,
            arrived: vec![Slot::default(); slots],
            last_frame: None,
            unbroken_since_frame: false,
            frame_steps: 0,
            frame_ids_skip: false,
            fec_inside_frames: false,
        };
        stream.remember(seq, what, part, true);
        stream
    }

    /// Count one packet that arrived ahead of every earlier one.
    fn count(&mut self, what: Arrived) {
        if self.in_sequence >= FEC_SHARE_WINDOW {
            self.in_sequence /= 2;
            self.in_sequence_fec /= 2;
        }
        self.in_sequence += 1;
        if what == Arrived::Fec {
            self.in_sequence_fec += 1;
        }
    }

    /// The packet that arrived under `seq`, if it is still remembered.
    fn slot(&self, seq: u64) -> Option<Slot> {
        let idx = usize::try_from(seq % ARRIVED_WINDOW).ok()?;
        self.arrived
            .get(idx)
            .copied()
            .filter(|slot| slot.is(SLOT_PRESENT) && slot.seq == low32(seq))
    }

    /// Remember what arrived under `seq`. `newest`: it is ahead of every
    /// earlier packet of the stream.
    fn remember(&mut self, seq: u64, what: Arrived, part: Option<PacketPosition>, newest: bool) {
        let before = seq.checked_sub(1).and_then(|prev| self.slot(prev));
        let mut slot = Slot {
            seq: low32(seq),
            frame: 0,
            bits: SLOT_PRESENT,
        };
        if what == Arrived::Fec {
            slot.bits |= SLOT_FEC;
            if before.is_some_and(|b| b.described() && !b.is(SLOT_ENDS)) {
                self.fec_inside_frames = true;
            }
        } else if let Some(part) = part {
            slot.bits |= SLOT_DESCRIBED;
            if part.ends_frame {
                slot.bits |= SLOT_ENDS;
            }
            if let Some(id) = part.begins_frame {
                slot.bits |= SLOT_BEGINS | SLOT_FRAME;
                slot.frame = id;
                if newest {
                    self.learn_frame_step(id);
                }
            } else if let Some(before) = before {
                if before.is(SLOT_FEC) {
                    self.fec_inside_frames = true;
                } else if before.described() && !before.is(SLOT_ENDS) && before.is(SLOT_FRAME) {
                    // A later packet of the frame the packet before is of.
                    slot.bits |= SLOT_FRAME;
                    slot.frame = before.frame;
                }
            }
        }
        if let Some(held) = usize::try_from(seq % ARRIVED_WINDOW)
            .ok()
            .and_then(|idx| self.arrived.get_mut(idx))
        {
            *held = slot;
        }
    }

    /// A frame with id `id` began ahead of everything seen.
    fn learn_frame_step(&mut self, id: u16) {
        if let (true, Some(last)) = (self.unbroken_since_frame, self.last_frame) {
            if id.wrapping_sub(last) == 1 {
                self.frame_steps = self.frame_steps.saturating_add(1);
            } else {
                self.frame_ids_skip = true;
            }
        }
        self.last_frame = Some(id);
        self.unbroken_since_frame = true;
    }

    /// Whether the frame ids of this stream count its frames.
    const fn frame_ids_count(&self) -> bool {
        !self.frame_ids_skip && self.frame_steps >= FRAME_ID_TRUST
    }

    /// Whether `unknown` missing packets of unknown kind, of a burst that
    /// lost `lost` and left `not_media` packets known not to be media, are
    /// more than FEC packets nobody sends again explain (see the module
    /// documentation). On a stream without FEC any is.
    fn beyond_fec(&self, lost: u64, unknown: u64, not_media: u64) -> bool {
        if self.in_sequence_fec == 0 {
            return unknown > 0;
        }
        // unknown - slack > 1.5 * (share * lost - not_media), in whole
        // numbers, with share = in_sequence_fec / in_sequence.
        let over = u128::from(unknown.saturating_sub(FEC_SLACK));
        let expected = u128::from(self.in_sequence_fec) * u128::from(lost);
        let accounted = u128::from(not_media) * u128::from(self.in_sequence);
        over * 2 * u128::from(self.in_sequence) > 3 * expected.saturating_sub(accounted)
    }

    /// The nearest media packet before `seq`, across FEC packets that
    /// arrived.
    fn edge_before(&self, seq: u64) -> Edge {
        let mut at = seq;
        for _ in 0..ARRIVED_WINDOW {
            let Some(prev) = at.checked_sub(1) else {
                return Edge::Unknown;
            };
            at = prev;
            match self.slot(at) {
                Some(slot) if slot.is(SLOT_FEC) => {},
                Some(slot) => return slot.edge(),
                None => return Edge::Unknown,
            }
        }
        Edge::Unknown
    }

    /// What each packet still missing was, in the order of `holes`.
    ///
    /// One pass: the missing packets are taken gap by gap, a gap being
    /// what lies between two media packets that arrived (missing packets
    /// and FEC packets only). Every step moves forward over the sequence
    /// numbers up to the newest, so the work is bounded by
    /// [`ARRIVED_WINDOW`] plus the missing packets.
    fn classify(&self) -> Vec<Kind> {
        let mut kinds = Vec::with_capacity(self.holes.len());
        let mut rest = self.holes.iter().peekable();
        while let Some((&first, _)) = rest.peek() {
            let left = self.edge_before(first);
            let start = kinds.len();
            let mut at = first;
            let right = loop {
                if rest.peek().is_some_and(|(seq, _)| **seq == at) {
                    let protected = rest.next().is_some_and(|(_, hole)| hole.protected);
                    kinds.push(if protected {
                        Kind::Media
                    } else {
                        Kind::Unknown
                    });
                } else {
                    match self.slot(at) {
                        Some(slot) if slot.is(SLOT_FEC) => {},
                        Some(slot) => break slot.edge(),
                        None => break Edge::Unknown,
                    }
                }
                at = at.saturating_add(1);
            };
            if let Some(gap) = kinds.get_mut(start..) {
                self.judge_gap(left, right, gap);
            }
        }
        kinds
    }

    /// Say what the missing packets of one gap were, from the media
    /// packets on its two sides. `gap` comes in with the packets an FEC
    /// packet listed as media, the others unknown.
    fn judge_gap(&self, left: Edge, right: Edge, gap: &mut [Kind]) {
        let mut least = 0u64;
        if !self.fec_inside_frames {
            // The frame on the left has no end, the one on the right no
            // beginning: a packet of each is missing.
            least += u64::from(matches!(left, Edge::Media { ends: false, .. }));
            least += u64::from(matches!(right, Edge::Media { begins: false, .. }));
        }
        if let (
            true,
            Edge::Media {
                ends,
                frame: Some(before),
                ..
            },
            Edge::Media {
                begins: true,
                frame: Some(after),
                ..
            },
        ) = (self.frame_ids_count(), left, right)
        {
            let step = after.wrapping_sub(before);
            if step == 1 && ends {
                // Frame n ended, frame n + 1 begins: nothing of any frame
                // lies between. What an FEC packet named stays media.
                for kind in gap.iter_mut().filter(|k| **k == Kind::Unknown) {
                    *kind = Kind::NotMedia;
                }
                return;
            }
            if (2..0x8000).contains(&step) {
                // Whole frames are missing, a packet at least each.
                least += u64::from(step - 1);
            }
        }
        let marked = gap.iter().filter(|k| **k == Kind::Media).count() as u64;
        let mut more = least.min(gap.len() as u64).saturating_sub(marked);
        for kind in gap.iter_mut().filter(|k| **k == Kind::Unknown) {
            if more == 0 {
                break;
            }
            *kind = Kind::Media;
            more -= 1;
        }
    }
}

/// The burst bookkeeping of every video stream one publisher sends, keyed by
/// whatever the caller names a stream by.
#[derive(Debug)]
pub struct BurstEnds<K> {
    streams: HashMap<K, Stream>,
}

impl<K: Copy + Eq + Hash> Default for BurstEnds<K> {
    fn default() -> Self {
        Self::new()
    }
}

impl<K: Copy + Eq + Hash> BurstEnds<K> {
    /// A publisher nothing has been received from yet.
    #[must_use]
    pub fn new() -> Self {
        Self {
            streams: HashMap::new(),
        }
    }

    /// One packet of `stream` reached the SFU, of which nothing is known
    /// but its extended sequence number, arrival time and what it is:
    /// [`Self::on_described`] without a frame position.
    pub fn on_packet(&mut self, stream: K, seq: u64, arrival: Instant, what: Arrived) {
        self.on_described(stream, seq, arrival, what, None);
    }

    /// One packet of `stream` reached the SFU: its extended sequence number,
    /// arrival time, what it is and, for a media packet whose frame
    /// descriptor says so, where in its frame it sits (ignored for an FEC
    /// packet). Every packet that takes a sequence number of the stream is
    /// handed over: media, retransmissions (as the packet they repair), FEC
    /// packets and packets rebuilt from FEC.
    pub fn on_described(
        &mut self,
        stream: K,
        seq: u64,
        arrival: Instant,
        what: Arrived,
        part: Option<PacketPosition>,
    ) {
        let key_frame_start = what == Arrived::KeyFrameStart;
        if !self.streams.contains_key(&stream) && self.streams.len() >= MAX_STREAMS {
            self.streams.clear();
        }
        let Some(state) = self.streams.get_mut(&stream) else {
            self.streams.insert(stream, Stream::new(seq, what, part));
            return;
        };
        if seq > state.highest {
            let step = seq - state.highest;
            if step > MAX_JUMP {
                *state = Stream::new(seq, what, part);
                return;
            }
            state.count(what);
            if step == 1 {
                if let Some(burst) = state.burst.as_mut() {
                    burst.in_order = burst.in_order.saturating_add(1);
                }
            } else {
                state.unbroken_since_frame = false;
                let burst = state.burst.get_or_insert(Burst {
                    started: arrival,
                    last_hole: arrival,
                    in_order: 0,
                    lost: 0,
                    repaired: 0,
                    beyond_repair: false,
                    gone_media: 0,
                });
                burst.last_hole = arrival;
                burst.in_order = 0;
                burst.lost = burst.lost.saturating_add(step - 1);
                // Only what retransmission still covers is kept one by one.
                let keep_from = seq.saturating_sub(RETRANSMISSION_WINDOW);
                if state.highest + 1 < keep_from {
                    burst.beyond_repair = true;
                }
                for missing in (state.highest + 1).max(keep_from)..seq {
                    state.holes.insert(
                        missing,
                        Hole {
                            seen: arrival,
                            protected: false,
                        },
                    );
                }
            }
            state.remember(seq, what, part, true);
            state.highest = seq;
            // Holes the newest packet has left too far behind.
            let keep_from = seq.saturating_sub(RETRANSMISSION_WINDOW);
            while state
                .holes
                .first_key_value()
                .is_some_and(|(missing, _)| *missing < keep_from)
            {
                let gone = state.holes.pop_first();
                if let Some(burst) = state.burst.as_mut() {
                    burst.beyond_repair = true;
                    if gone.is_some_and(|(_, hole)| hole.protected) {
                        burst.gone_media = burst.gone_media.saturating_add(1);
                    }
                }
            }
        } else if state.highest - seq > MAX_JUMP {
            *state = Stream::new(seq, what, part);
            return;
        } else {
            if state.highest - seq < ARRIVED_WINDOW {
                state.remember(seq, what, part, false);
            }
            // A late packet: it closes its hole if that is still tracked.
            if state.holes.remove(&seq).is_some() {
                if let Some(burst) = state.burst.as_mut() {
                    burst.repaired = burst.repaired.saturating_add(1);
                }
            }
        }
        if key_frame_start {
            // What is missing before a key frame is missing for nobody.
            let before = state.holes.len();
            state.holes = state.holes.split_off(&seq);
            let closed = (before - state.holes.len()) as u64;
            if let Some(burst) = state.burst.as_mut() {
                if seq == state.highest {
                    // The newest packet: everything the burst lost is
                    // older, tracked or not.
                    burst.beyond_repair = false;
                    burst.lost = 0;
                    burst.repaired = 0;
                    burst.gone_media = 0;
                } else {
                    burst.repaired = burst.repaired.saturating_add(closed);
                }
            }
            if state.holes.is_empty() {
                state.burst = None;
            }
        }
    }

    /// Whether packets are missing on `stream` right now, so that the mask
    /// of an FEC packet of it is worth reading ([`Self::fec_protects`]).
    #[must_use]
    pub fn has_holes(&self, stream: K) -> bool {
        self.streams
            .get(&stream)
            .is_some_and(|s| !s.holes.is_empty())
    }

    /// An FEC packet of `stream` that arrived under the extended sequence
    /// number `fec_seq` lists `protected` (16-bit sequence numbers, as its
    /// mask names them) as the packets it protects. Each of them that is
    /// missing is media from now on: FEC protects media packets only.
    ///
    /// A protected packet was sent before the FEC packet, so its extended
    /// number is the FEC packet's minus the distance; a number that is not
    /// behind the FEC packet by 1 to [`RETRANSMISSION_WINDOW`] is ignored,
    /// and so is everything past the first
    /// [`crate::fec_receive::MAX_PROTECTED`] numbers. Nothing is stored
    /// but a flag on a packet already tracked as missing.
    pub fn fec_protects(&mut self, stream: K, fec_seq: u64, protected: &[u16]) {
        let Some(state) = self.streams.get_mut(&stream) else {
            return;
        };
        if state.holes.is_empty() {
            return;
        }
        let wire = u16::try_from(fec_seq & 0xffff).unwrap_or(0);
        for named in protected.iter().take(crate::fec_receive::MAX_PROTECTED) {
            let back = u64::from(wire.wrapping_sub(*named));
            if back == 0 || back > RETRANSMISSION_WINDOW {
                continue;
            }
            let Some(seq) = fec_seq.checked_sub(back) else {
                continue;
            };
            if let Some(hole) = state.holes.get_mut(&seq) {
                hole.protected = true;
            }
        }
    }

    /// Move to `now` and hand out the bursts that have ended with packets
    /// still missing since the last call, each once, with what was made of
    /// it: [`BurstEnd::asks`] says whether it ended unrepaired. A burst
    /// that ended with nothing missing is closed without a word.
    pub fn poll(&mut self, now: Instant) -> Vec<BurstEnd<K>> {
        let mut ended = Vec::new();
        for (key, state) in &mut self.streams {
            let Some(mut burst) = state.burst else {
                continue;
            };
            // Holes are inserted in order of sequence number with times that
            // do not decrease as long as the caller's clock does not: the
            // first is the oldest.
            while state.holes.first_key_value().is_some_and(|(_, hole)| {
                now.saturating_duration_since(hole.seen) >= RETRANSMISSION_SPAN
            }) {
                if state
                    .holes
                    .pop_first()
                    .is_some_and(|(_, hole)| hole.protected)
                {
                    burst.gone_media = burst.gone_media.saturating_add(1);
                }
                burst.beyond_repair = true;
            }
            if state.holes.is_empty() && !burst.beyond_repair {
                // Repaired, every packet of it.
                state.burst = None;
                continue;
            }
            state.burst = Some(burst);
            let over = burst.in_order >= BURST_END_PACKETS
                && now.saturating_duration_since(burst.last_hole) >= BURST_END_QUIET;
            if !over {
                continue;
            }
            let late = burst.beyond_repair
                || state.holes.first_key_value().is_some_and(|(_, hole)| {
                    now.saturating_duration_since(hole.seen) >= UNREPAIRED_AFTER
                });
            if !late {
                continue;
            }
            // The burst is over and what is still missing will not come.
            let missing = burst.lost.saturating_sub(burst.repaired);
            if missing > 0 {
                let kinds = state.classify();
                let count = |kind: Kind| kinds.iter().filter(|k| **k == kind).count() as u64;
                let media = burst
                    .gone_media
                    .saturating_add(count(Kind::Media))
                    .min(missing);
                let not_media = count(Kind::NotMedia).min(missing - media);
                let unknown = missing - media - not_media;
                let why = if media > 0 {
                    Why::MediaMissing
                } else if unknown == 0 {
                    Why::NotMedia
                } else if state.beyond_fec(burst.lost, unknown, not_media) {
                    Why::Unexplained
                } else {
                    Why::WithinFecShare
                };
                ended.push(BurstEnd {
                    stream: *key,
                    lost: burst.lost,
                    unrepaired: missing,
                    media,
                    not_media,
                    unknown,
                    beyond_repair: burst.beyond_repair,
                    lasted: burst.last_hole.saturating_duration_since(burst.started),
                    why,
                });
            }
            state.holes.clear();
            state.burst = None;
        }
        ended
    }

    /// Packets currently tracked as missing across all streams. For tests
    /// and bounds.
    #[must_use]
    pub fn open_holes(&self) -> usize {
        self.streams.values().map(|s| s.holes.len()).sum()
    }

    /// Streams kept. At most [`MAX_STREAMS`]. For tests and bounds.
    #[must_use]
    pub fn streams_kept(&self) -> usize {
        self.streams.len()
    }

    /// Forget `stream`: its sender was switched off on the SFU's word
    /// ([`crate::video_needed`]). What was missing when it stopped is
    /// missing for nobody, and the first packet after it is switched back
    /// on starts the stream's books afresh, whatever its sequence number:
    /// neither the stop nor the return is a burst.
    pub fn forget(&mut self, stream: K) {
        self.streams.remove(&stream);
    }

    /// Whether a burst is open on `stream`.
    #[must_use]
    pub fn in_burst(&self, stream: K) -> bool {
        self.streams.get(&stream).is_some_and(|s| s.burst.is_some())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_forgotten_stream_starts_over_without_a_burst() {
        let t0 = Instant::now();
        let ms = Duration::from_millis;
        let mut ends: BurstEnds<u8> = BurstEnds::new();
        for seq in 0..10u64 {
            ends.on_packet(7, seq, t0 + ms(seq * 10), Arrived::Media);
        }
        // Two packets lost right before the sender is switched off.
        ends.on_packet(7, 12, t0 + ms(120), Arrived::Media);
        assert!(ends.in_burst(7));
        assert_eq!(ends.open_holes(), 2);
        ends.forget(7);
        assert!(!ends.in_burst(7));
        assert_eq!(ends.open_holes(), 0);
        // It comes back a minute later 200 sequence numbers on: no burst,
        // and nothing ever ends unrepaired.
        let back = t0 + Duration::from_secs(60);
        for i in 0..40u64 {
            ends.on_packet(7, 212 + i, back + ms(i * 10), Arrived::Media);
        }
        assert!(!ends.in_burst(7));
        assert!(ends.poll(back + Duration::from_secs(5)).is_empty());
        // Another stream of the publisher is untouched by it.
        ends.on_packet(8, 0, t0, Arrived::Media);
        ends.on_packet(8, 3, t0 + ms(10), Arrived::Media);
        ends.forget(7);
        assert!(ends.in_burst(8));
    }

    fn ms(n: u64) -> Duration {
        Duration::from_millis(n)
    }

    /// Packets `first..first + n`, `every` apart from `t0`, without the
    /// sequence numbers in `skip`. Returns the arrival time of the last
    /// slot.
    fn feed(
        b: &mut BurstEnds<u8>,
        stream: u8,
        first: u64,
        n: u64,
        skip: &[u64],
        t0: Instant,
        every: Duration,
    ) -> Instant {
        let mut at = t0;
        for i in 0..n {
            at = t0 + every * u32::try_from(i).expect("fits");
            let seq = first + i;
            if !skip.contains(&seq) {
                b.on_packet(stream, seq, at, Arrived::Media);
            }
        }
        at
    }

    /// Poll every 10 ms over `[from, until]` and collect every burst that
    /// ended with packets missing, asking or not, with the time of the
    /// poll that said so.
    fn judged_over(
        b: &mut BurstEnds<u8>,
        from: Instant,
        until: Instant,
    ) -> Vec<(Duration, BurstEnd<u8>)> {
        let mut out = Vec::new();
        let mut at = from;
        while at <= until {
            for end in b.poll(at) {
                out.push((at.duration_since(from), end));
            }
            at += ms(10);
        }
        out
    }

    /// The same, only the bursts that ended unrepaired.
    fn poll_over(
        b: &mut BurstEnds<u8>,
        from: Instant,
        until: Instant,
    ) -> Vec<(Duration, BurstEnd<u8>)> {
        let mut out = judged_over(b, from, until);
        out.retain(|(_, end)| end.asks());
        out
    }

    #[test]
    fn a_clean_stream_never_ends_a_burst() {
        let t0 = Instant::now();
        let mut b = BurstEnds::new();
        let last = feed(&mut b, 1, 100, 1000, &[], t0, ms(4));
        assert!(poll_over(&mut b, t0, last + ms(3000)).is_empty());
        assert!(!b.in_burst(1));
        assert_eq!(b.open_holes(), 0);
    }

    #[test]
    fn a_burst_that_retransmission_repaired_asks_for_nothing() {
        let t0 = Instant::now();
        let mut b = BurstEnds::new();
        // 100..150 at 250 packets a second, 110..=129 lost.
        let lost: Vec<u64> = (110..130).collect();
        feed(&mut b, 1, 100, 50, &lost, t0, ms(4));
        assert!(b.in_burst(1));
        assert_eq!(b.open_holes(), 20);
        // The stream goes on in sequence; the retransmissions arrive 80 ms
        // after the burst.
        let after = t0 + ms(200);
        feed(&mut b, 1, 150, 20, &[], after, ms(4));
        for seq in &lost {
            b.on_packet(1, *seq, after + ms(80), Arrived::Media);
        }
        feed(&mut b, 1, 170, 500, &[], after + ms(80), ms(4));
        assert!(poll_over(&mut b, t0, after + ms(4000)).is_empty());
        assert!(!b.in_burst(1), "closed, without a word");
    }

    #[test]
    fn an_unrepaired_burst_ends_once_after_the_stated_times() {
        let t0 = Instant::now();
        let mut b = BurstEnds::new();
        // One second at 250 packets a second in which only every fifth
        // packet arrives, then the stream in sequence again.
        let lost: Vec<u64> = (1000..1250).filter(|s| s % 5 != 0).collect();
        let burst_last = feed(&mut b, 1, 1000, 250, &lost, t0, ms(4));
        // In the burst: polled all through it, nothing ends.
        assert!(poll_over(&mut b, t0, burst_last).is_empty());
        let resumed = burst_last + ms(4);
        feed(&mut b, 1, 1250, 40, &[], resumed, ms(4));
        let ended = poll_over(&mut b, resumed, resumed + ms(160));
        assert_eq!(ended.len(), 1, "{ended:?}");
        let (after, end) = ended[0];
        // The stream goes on: the burst is not answered a second time.
        feed(&mut b, 1, 1290, 1000, &[], resumed + ms(160), ms(4));
        assert!(
            poll_over(&mut b, resumed + ms(160), resumed + ms(5000)).is_empty(),
            "one answer per burst"
        );
        // The last hole opened with the first packet in sequence; the quiet
        // time counts from there.
        assert!(
            after + ms(4) >= BURST_END_QUIET && after <= BURST_END_QUIET + ms(20),
            "asked {after:?} after the stream was in sequence again"
        );
        assert_eq!(end.stream, 1);
        assert_eq!(end.lost, 200);
        assert_eq!(end.unrepaired, 200);
        assert!(!end.beyond_repair);
        assert!(end.lasted >= ms(900) && end.lasted <= ms(1000));
        assert_eq!(b.open_holes(), 0, "what was missing is let go");
        assert!(!b.in_burst(1));
    }

    #[test]
    fn a_short_burst_waits_for_its_packets_before_it_counts_as_unrepaired() {
        let t0 = Instant::now();
        let mut b = BurstEnds::new();
        // Two packets lost, then in sequence.
        feed(&mut b, 1, 100, 10, &[104, 105], t0, ms(4));
        let known = t0 + ms(24);
        feed(&mut b, 1, 110, 500, &[], t0 + ms(40), ms(4));
        let ended = poll_over(&mut b, known, known + ms(2000));
        assert_eq!(ended.len(), 1);
        let (after, end) = ended[0];
        assert!(
            after >= UNREPAIRED_AFTER && after <= UNREPAIRED_AFTER + ms(20),
            "not at the end of the burst but when the repair is late: {after:?}"
        );
        assert_eq!((end.lost, end.unrepaired), (2, 2));
    }

    #[test]
    fn a_short_burst_repaired_before_the_stated_time_asks_for_nothing() {
        let t0 = Instant::now();
        let mut b = BurstEnds::new();
        feed(&mut b, 1, 100, 10, &[104, 105], t0, ms(4));
        feed(&mut b, 1, 110, 40, &[], t0 + ms(40), ms(4));
        // Ended (in sequence, quiet) but the holes are young: nothing yet.
        assert!(b.poll(t0 + ms(24) + ms(200)).is_empty());
        assert!(b.in_burst(1));
        b.on_packet(1, 104, t0 + ms(250), Arrived::Media);
        b.on_packet(1, 105, t0 + ms(250), Arrived::Media);
        assert!(poll_over(&mut b, t0 + ms(250), t0 + ms(3000)).is_empty());
        assert!(!b.in_burst(1));
    }

    #[test]
    fn a_second_burst_inside_the_wait_restarts_it() {
        let t0 = Instant::now();
        let mut b = BurstEnds::new();
        let lost: Vec<u64> = (100..150).filter(|s| s % 5 != 0).collect();
        feed(&mut b, 1, 100, 50, &lost, t0, ms(4));
        // In sequence for 80 ms: not yet the stated quiet time.
        let calm = t0 + ms(200);
        feed(&mut b, 1, 150, 20, &[], calm, ms(4));
        assert!(poll_over(&mut b, t0, calm + ms(80)).is_empty());
        // Loss again.
        let again = calm + ms(80);
        let lost2: Vec<u64> = (170..220).filter(|s| s % 5 != 0).collect();
        let end_of_second = feed(&mut b, 1, 170, 50, &lost2, again, ms(4));
        assert!(poll_over(&mut b, again, end_of_second).is_empty());
        let resumed = end_of_second + ms(4);
        feed(&mut b, 1, 220, 500, &[], resumed, ms(4));
        let ended = poll_over(&mut b, resumed, resumed + ms(3000));
        assert_eq!(ended.len(), 1, "one burst, one answer: {ended:?}");
        assert_eq!(ended[0].1.lost, 80, "both halves are the one burst");
        assert!(ended[0].0 + ms(4) >= BURST_END_QUIET);
    }

    #[test]
    fn a_lull_inside_a_burst_is_not_its_end() {
        let t0 = Instant::now();
        // Four packets in sequence right after a hole, 100 ms apart: quiet
        // for long enough, but not enough packets.
        let mut b = BurstEnds::new();
        b.on_packet(1, 100, t0, Arrived::Media);
        b.on_packet(1, 105, t0 + ms(20), Arrived::Media);
        for (i, seq) in (106..110).enumerate() {
            b.on_packet(
                1,
                seq,
                t0 + ms(100 * (u64::try_from(i).expect("fits") + 1)),
                Arrived::Media,
            );
        }
        // 420 ms after the hole, four packets in sequence: still the burst.
        assert!(poll_over(&mut b, t0, t0 + ms(1000)).is_empty());
        assert!(b.in_burst(1));
        // The fifth ends it; the hole is long past repair.
        b.on_packet(1, 110, t0 + ms(1010), Arrived::Media);
        let ended = b.poll(t0 + ms(1010));
        assert_eq!(ended.len(), 1);
        assert!(ended[0].asks());
    }

    #[test]
    fn a_stream_that_stops_in_a_burst_is_not_asked_until_it_runs_again() {
        let t0 = Instant::now();
        let mut b = BurstEnds::new();
        feed(&mut b, 1, 100, 10, &[104, 105, 106], t0, ms(4));
        // Nothing more arrives for ten seconds.
        assert!(poll_over(&mut b, t0, t0 + ms(10_000)).is_empty());
        assert_eq!(b.open_holes(), 0, "holes are let go after the span");
        assert!(b.in_burst(1), "but the burst is remembered");
        // The stream runs again, in sequence.
        let back = t0 + ms(10_000);
        feed(&mut b, 1, 110, 10, &[], back, ms(20));
        let ended = poll_over(&mut b, back, back + ms(1000));
        assert_eq!(ended.len(), 1);
        assert!(ended[0].1.beyond_repair);
        assert_eq!(ended[0].1.unrepaired, 3);
    }

    #[test]
    fn a_burst_longer_than_retransmission_covers_is_unrepaired_at_its_end() {
        let t0 = Instant::now();
        let mut b = BurstEnds::new();
        // 600 packets missing in one step: more than the window.
        b.on_packet(1, 1000, t0, Arrived::Media);
        b.on_packet(1, 1601, t0 + ms(2400), Arrived::Media);
        assert_eq!(b.open_holes() as u64, RETRANSMISSION_WINDOW);
        let resumed = t0 + ms(2400);
        feed(&mut b, 1, 1602, 20, &[], resumed + ms(4), ms(4));
        // Every tracked hole is repaired at once; the 88 beyond the window
        // cannot be.
        for seq in (1601 - RETRANSMISSION_WINDOW)..1601 {
            b.on_packet(1, seq, resumed + ms(60), Arrived::Media);
        }
        assert_eq!(b.open_holes(), 0);
        let ended = poll_over(&mut b, resumed, resumed + ms(1000));
        assert_eq!(ended.len(), 1);
        let (after, end) = ended[0];
        assert!(end.beyond_repair);
        // 20 more went out of the window with the packets in sequence.
        assert_eq!((end.lost, end.unrepaired), (600, 108));
        assert!(after <= BURST_END_QUIET + ms(20), "{after:?}");
    }

    #[test]
    fn holes_are_bounded_and_a_stream_that_starts_over_is_not_a_burst() {
        let t0 = Instant::now();
        let mut b = BurstEnds::new();
        // Every second packet missing for a long time.
        for i in 0..5000u64 {
            b.on_packet(1, 2 * i, t0, Arrived::Media);
        }
        assert!(b.open_holes() as u64 <= RETRANSMISSION_WINDOW);
        // A step too large to be loss, either way: the stream starts over.
        b.on_packet(1, 10_000 + MAX_JUMP + 1, t0, Arrived::Media);
        assert_eq!(b.open_holes(), 0);
        assert!(!b.in_burst(1));
        b.on_packet(1, 5, t0, Arrived::Media);
        assert!(!b.in_burst(1));
        // A late packet from further back than the window changes nothing.
        let mut late = BurstEnds::new();
        late.on_packet(1, 2000, t0, Arrived::Media);
        late.on_packet(1, 2003, t0, Arrived::Media);
        late.on_packet(1, 2003 - RETRANSMISSION_WINDOW - 50, t0, Arrived::Media);
        assert!(late.in_burst(1));
        assert_eq!(late.open_holes(), 2);
        // Many streams: the table starts over instead of growing.
        for stream in 10..60u8 {
            b.on_packet(stream, 1, t0, Arrived::Media);
        }
        assert!(b.streams_kept() <= MAX_STREAMS);
    }

    #[test]
    fn a_key_frame_that_has_started_arriving_closes_what_was_missing_before_it() {
        let t0 = Instant::now();
        let mut b = BurstEnds::new();
        let lost: Vec<u64> = (100..150).filter(|s| s % 5 != 0).collect();
        feed(&mut b, 1, 100, 50, &lost, t0, ms(4));
        assert!(b.in_burst(1));
        // The publisher sends a key frame of its own accord.
        b.on_packet(1, 150, t0 + ms(200), Arrived::KeyFrameStart);
        assert!(!b.in_burst(1));
        feed(&mut b, 1, 151, 500, &[], t0 + ms(204), ms(4));
        assert!(poll_over(&mut b, t0, t0 + ms(4000)).is_empty());

        // A hole after the key frame's first packet is still a hole.
        let t1 = t0 + ms(5000);
        feed(&mut b, 1, 651, 10, &[655], t1, ms(4));
        b.on_packet(1, 661, t1 + ms(40), Arrived::KeyFrameStart);
        b.on_packet(1, 663, t1 + ms(44), Arrived::Media);
        assert!(b.in_burst(1));
        assert_eq!(b.open_holes(), 1, "655 is forgotten, 662 is not");
    }

    #[test]
    fn each_stream_is_judged_on_its_own() {
        let t0 = Instant::now();
        let mut b = BurstEnds::new();
        // Stream 1 loses and never repairs; stream 2 loses and repairs;
        // stream 3 loses nothing.
        feed(&mut b, 1, 100, 20, &[105, 106, 107], t0, ms(4));
        feed(&mut b, 2, 500, 20, &[505, 506], t0, ms(4));
        feed(&mut b, 3, 900, 20, &[], t0, ms(4));
        b.on_packet(2, 505, t0 + ms(100), Arrived::Media);
        b.on_packet(2, 506, t0 + ms(100), Arrived::Media);
        for stream in 1..=3u8 {
            feed(
                &mut b,
                stream,
                [0, 120, 520, 920][usize::from(stream)],
                200,
                &[],
                t0 + ms(80),
                ms(4),
            );
        }
        let ended = poll_over(&mut b, t0, t0 + ms(3000));
        assert_eq!(ended.len(), 1);
        assert_eq!(ended[0].1.stream, 1);
    }
    /// A stream in which every third packet is FEC: `first..first + n`,
    /// 4 ms apart from `t0`, without the sequence numbers in `skip`.
    fn feed_with_fec(b: &mut BurstEnds<u8>, first: u64, n: u64, skip: &[u64], t0: Instant) {
        for i in 0..n {
            let seq = first + i;
            if skip.contains(&seq) {
                continue;
            }
            let what = if seq % 3 == 2 {
                Arrived::Fec
            } else {
                Arrived::Media
            };
            b.on_packet(1, seq, t0 + ms(4 * i), what);
        }
    }

    #[test]
    fn on_a_stream_with_fec_a_lost_packet_or_two_never_ask_for_anything() {
        let t0 = Instant::now();
        let mut b = BurstEnds::new();
        // Ten seconds; every 125th packet is lost and never comes again,
        // and once two in a row are.
        let mut skip: Vec<u64> = (0..2500).filter(|s| s % 125 == 60).collect();
        skip.push(1061);
        // Fed and polled as time passes, packet by packet.
        for i in 0..2500u64 {
            let at = t0 + ms(4 * i);
            feed_with_fec(&mut b, i, 1, &skip, at);
            assert!(
                b.poll(at).iter().all(|end| !end.asks()),
                "asked at packet {i}"
            );
        }
        assert!(poll_over(&mut b, t0 + ms(10_000), t0 + ms(14_000)).is_empty());
        assert!(!b.in_burst(1), "each is closed without a word");
        assert_eq!(b.open_holes(), 0);
    }

    #[test]
    fn on_a_stream_with_fec_only_more_than_its_share_left_missing_is_unrepaired() {
        let t0 = Instant::now();
        // A second of a clean stream first, so the share is known: 1/3.
        // Then 60 packets lost in a row.
        let lost: Vec<u64> = (300..360).collect();
        let run = |repaired: &[u64]| {
            let mut b = BurstEnds::new();
            feed_with_fec(&mut b, 0, 400, &lost, t0);
            let after = t0 + ms(4 * 400);
            for seq in repaired {
                b.on_packet(1, *seq, after + ms(60), Arrived::Media);
            }
            poll_over(&mut b, after, after + ms(3000))
        };
        // Every media packet came again; the 20 FEC packets did not.
        let media: Vec<u64> = lost.iter().copied().filter(|s| s % 3 != 2).collect();
        assert!(run(&media).is_empty(), "what is missing is FEC's share");
        // Three media packets short of that: still within the slack and
        // the half again.
        assert!(run(&media[3..]).is_empty());
        // Nothing came again: 60 missing against a share of 20.
        let ended = run(&[]);
        assert_eq!(ended.len(), 1);
        assert_eq!((ended[0].1.lost, ended[0].1.unrepaired), (60, 60));
        // Half of the media came again: 40 missing, more than 2 + 30.
        let ended = run(&media[..20]);
        assert_eq!(ended.len(), 1);
        assert_eq!(ended[0].1.unrepaired, 40);
    }

    // ---- What a missing packet was (2026-10-11) -----------------------

    /// What one sequence number of a modelled stream carries.
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    enum Sent {
        /// A media packet of the frame with this id, and whether it begins
        /// and ends it.
        Media {
            frame: u16,
            begins: bool,
            ends: bool,
        },
        /// An FEC packet that protects these sequence numbers.
        Fec { from: u64, to: u64 },
    }

    /// A stream as a libwebrtc publisher with ULPFEC sends it: frames of
    /// `per_frame` media packets, each followed by `fec_per_frame` FEC
    /// packets that protect the frame's packets. Sequence numbers from
    /// `first`, frame ids from `first_frame`.
    fn model(
        first: u64,
        first_frame: u16,
        frames: u64,
        per_frame: u64,
        fec_per_frame: u64,
    ) -> Vec<(u64, Sent)> {
        let mut out = Vec::new();
        let mut seq = first;
        for f in 0..frames {
            let frame = first_frame.wrapping_add(u16::try_from(f).expect("fits"));
            let from = seq;
            for i in 0..per_frame {
                out.push((
                    seq,
                    Sent::Media {
                        frame,
                        begins: i == 0,
                        ends: i + 1 == per_frame,
                    },
                ));
                seq += 1;
            }
            for _ in 0..fec_per_frame {
                out.push((
                    seq,
                    Sent::Fec {
                        from,
                        to: from + per_frame,
                    },
                ));
                seq += 1;
            }
        }
        out
    }

    /// What the SFU knows of the packets it is handed.
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    enum Knows {
        /// Sequence numbers only: the rule as it was before 2026-10-11.
        Numbers,
        /// And the masks of the FEC packets that arrive.
        Masks,
        /// And the frame descriptors of the media packets that arrive.
        Descriptors,
    }

    /// Hand `b` one packet of the model at `at`.
    fn hand(b: &mut BurstEnds<u8>, seq: u64, sent: Sent, at: Instant, knows: Knows) {
        match sent {
            Sent::Media {
                frame,
                begins,
                ends,
            } => {
                let part = (knows == Knows::Descriptors).then_some(PacketPosition {
                    begins_frame: begins.then_some(frame),
                    ends_frame: ends,
                });
                b.on_described(1, seq, at, Arrived::Media, part);
            },
            Sent::Fec { from, to } => {
                b.on_packet(1, seq, at, Arrived::Fec);
                if knows == Knows::Masks && b.has_holes(1) {
                    let protected: Vec<u16> = (from..to)
                        .map(|s| u16::try_from(s & 0xffff).expect("fits"))
                        .collect();
                    b.fec_protects(1, seq, &protected);
                }
            },
        }
    }

    /// Run the model 4 ms a packet from `t0` without the sequence numbers
    /// in `lost`; hand over `late` (the retransmissions that came), in
    /// order, 60 ms after the last of `lost` was due; poll with every
    /// packet and for three seconds after the last, and return every burst
    /// that was judged.
    fn run_model(
        stream: &[(u64, Sent)],
        lost: &[u64],
        late: &[u64],
        knows: Knows,
    ) -> Vec<BurstEnd<u8>> {
        let t0 = Instant::now();
        let mut b = BurstEnds::new();
        let mut judged = Vec::new();
        let mut at = t0;
        let last_lost = lost.iter().copied().max();
        for (i, (seq, sent)) in stream.iter().enumerate() {
            at = t0 + ms(4 * u64::try_from(i).expect("fits"));
            if !lost.contains(seq) {
                hand(&mut b, *seq, *sent, at, knows);
            }
            if last_lost.is_some_and(|last| *seq == last + 15) {
                for (seq, sent) in stream.iter().filter(|(seq, _)| late.contains(seq)) {
                    hand(&mut b, *seq, *sent, at, knows);
                }
            }
            judged.extend(b.poll(at));
        }
        judged.extend(
            judged_over(&mut b, at, at + ms(3000))
                .into_iter()
                .map(|(_, end)| end),
        );
        assert_eq!(b.open_holes(), 0, "what was missing is let go");
        judged
    }

    /// The measured case of 2026-10-11: half the stream is FEC, a burst
    /// loses about twenty packets, retransmission brings most of the media
    /// back and a few media packets stay missing.
    struct Measured {
        /// Two packets a frame and two FEC packets after it; the lead is
        /// long enough for the share and the frame ids to be known.
        stream: Vec<(u64, Sent)>,
        /// What the burst lost.
        lost: Vec<u64>,
        /// The media among it that came again.
        came: Vec<u64>,
        /// The media that never did.
        never: Vec<u64>,
    }

    fn measured_case() -> Measured {
        let stream = model(0, 65_500, 120, 2, 2);
        // Frames 100..=104: sequence numbers 400..420. The first packets of
        // the next frame are lost too, so the burst cuts into a frame.
        let lost: Vec<u64> = (402..423).collect();
        let media: Vec<u64> = lost.iter().copied().filter(|s| s % 4 < 2).collect();
        // Three media packets never come: the end of frame 101 (405), the
        // begin of frame 102 (408) and the end of frame 105 (421).
        let never = vec![405, 408, 421];
        let came: Vec<u64> = media
            .iter()
            .copied()
            .filter(|s| !never.contains(s))
            .collect();
        Measured {
            stream,
            lost,
            came,
            never,
        }
    }

    #[test]
    fn the_measured_case_asked_for_nothing_by_number_alone() {
        // The rule before 2026-10-11, which is the rule that still judges
        // packets of unknown kind: 21 lost, 14 still missing (11 of them
        // FEC), share one half. 14 - 2 is not more than 1.5 * 0.5 * 21.
        let Measured {
            stream,
            lost,
            came,
            never,
        } = measured_case();
        let judged = run_model(&stream, &lost, &came, Knows::Numbers);
        assert_eq!(judged.len(), 1, "{judged:?}");
        let end = judged[0];
        assert_eq!(end.lost, 21);
        assert_eq!(
            end.unrepaired,
            u64::try_from(lost.len() - came.len()).expect("fits")
        );
        assert_eq!(end.unrepaired, 11 + never.len() as u64);
        assert_eq!((end.media, end.not_media), (0, 0));
        assert_eq!(end.unknown, end.unrepaired);
        assert!(!end.asks(), "three media packets missing went unnoticed");
        assert_eq!(end.why, Why::WithinFecShare);
    }

    #[test]
    fn media_an_fec_packet_lists_as_protected_is_unrepaired_whatever_the_share() {
        let Measured {
            stream,
            lost,
            came,
            never,
        } = measured_case();
        // The FEC packets right after the burst (422 is lost, 423 arrives)
        // protect 420 and 421; the ones inside it are lost. So one of the
        // three missing media packets is named by a mask that arrived.
        let judged = run_model(&stream, &lost, &came, Knows::Masks);
        assert_eq!(judged.len(), 1, "{judged:?}");
        let end = judged[0];
        assert!(end.asks());
        assert_eq!(end.why, Why::MediaMissing);
        assert_eq!(end.media, 1, "421, protected by the FEC packet 423");
        assert_eq!(end.not_media, 0);
        assert_eq!(end.unknown, end.unrepaired - 1);
        assert_eq!(end.unrepaired, 11 + never.len() as u64);

        // Had 421 come again as well, no mask that arrived names a missing
        // packet and the count rule is all there is: nothing.
        let mut came = came;
        came.push(421);
        let judged = run_model(&stream, &lost, &came, Knows::Masks);
        assert_eq!(judged.len(), 1);
        assert!(!judged[0].asks());
        assert_eq!(judged[0].media, 0);
    }

    #[test]
    fn media_the_frame_descriptors_show_missing_is_unrepaired_whatever_the_share() {
        let Measured {
            stream,
            lost,
            came,
            never,
        } = measured_case();
        let judged = run_model(&stream, &lost, &came, Knows::Descriptors);
        assert_eq!(judged.len(), 1, "{judged:?}");
        let end = judged[0];
        assert!(end.asks());
        assert_eq!(end.why, Why::MediaMissing);
        // 405 follows a packet that does not end its frame, 408 precedes
        // one that does not begin its frame, 421 follows one that does not
        // end its frame: all three are found.
        assert_eq!(end.media, never.len() as u64);
        // The FEC packets lost between a frame's end and the next frame's
        // beginning are known not to be media: 402..=403, 414..=415 and
        // 418..=419 lie between two whole consecutive frames.
        assert_eq!(end.not_media, 6);
        // The rest nothing is known of: 406, 407 (in one gap with the two
        // media packets), 410, 411 (the frame before them has no begin, so
        // no id) and 422 (after the cut frame 105).
        assert_eq!(end.unknown, 5);
        assert_eq!(end.unrepaired, 14);
    }

    #[test]
    fn a_stream_that_loses_only_fec_packets_asks_for_nothing() {
        let stream = model(0, 10, 200, 2, 2);
        // A burst in which every media packet comes again and no FEC
        // packet does; then FEC packets lost one at a time, and many.
        let lost: Vec<u64> = (402..423).collect();
        let media: Vec<u64> = lost.iter().copied().filter(|s| s % 4 < 2).collect();
        for knows in [Knows::Numbers, Knows::Masks, Knows::Descriptors] {
            let judged = run_model(&stream, &lost, &media, knows);
            assert!(
                judged.iter().all(|end| !end.asks()),
                "{knows:?}: {judged:?}"
            );
            assert_eq!(judged.iter().map(|end| end.media).sum::<u64>(), 0);
        }
        // With descriptors every one of them is known not to be media.
        let judged = run_model(&stream, &lost, &media, Knows::Descriptors);
        assert_eq!(judged.len(), 1);
        assert_eq!(judged[0].why, Why::NotMedia);
        assert_eq!((judged[0].not_media, judged[0].unknown), (11, 0));

        // Every FEC packet of thirty frames in a row lost, the media
        // untouched: sixty packets missing, far beyond what the count rule
        // lets pass, and still nothing is asked, because each is known.
        let only_fec: Vec<u64> = (400..520).filter(|s| s % 4 >= 2).collect();
        let judged = run_model(&stream, &only_fec, &[], Knows::Descriptors);
        assert!(!judged.is_empty());
        assert!(
            judged.iter().all(|end| end.why == Why::NotMedia),
            "{judged:?}"
        );
        assert_eq!(judged.iter().map(|end| end.not_media).sum::<u64>(), 60);
        // By number alone the same loss does ask: that is what knowing
        // the kind is for.
        let judged = run_model(&stream, &only_fec, &[], Knows::Numbers);
        assert!(judged.iter().any(BurstEnd::asks));
    }

    /// One stream of single-packet or multi-packet frames without FEC,
    /// described, in which `lost` never arrives. Returns what was judged.
    fn described_without_fec(first_frame: u16, per_frame: u64, lost: &[u64]) -> Vec<BurstEnd<u8>> {
        let stream = model(0, first_frame, 200, per_frame, 0);
        run_model(&stream, lost, &[], Knows::Descriptors)
    }

    #[test]
    fn a_frame_without_its_begin_or_its_end_is_missing_media() {
        // Frames of four packets; frame 100 is 400..=403.
        // Its end is missing.
        let judged = described_without_fec(0, 4, &[403]);
        assert_eq!(judged.len(), 1);
        assert_eq!((judged[0].media, judged[0].unknown), (1, 0));
        assert!(judged[0].asks());
        // Its begin is missing.
        let judged = described_without_fec(0, 4, &[400]);
        assert_eq!((judged[0].media, judged[0].unknown), (1, 0));
        // A packet in its middle: both neighbours say so, it is one packet.
        let judged = described_without_fec(0, 4, &[401]);
        assert_eq!((judged[0].media, judged[0].unknown), (1, 0));
        // The end of one frame and the begin of the next: two packets,
        // each known.
        let judged = described_without_fec(0, 4, &[403, 404]);
        assert_eq!((judged[0].media, judged[0].unknown), (2, 0));
    }

    #[test]
    fn a_gap_in_the_frame_ids_is_missing_media_and_the_ids_wrap() {
        // One packet a frame: begins and ends it, so only the ids say
        // that frames are missing. Three whole frames lost.
        let judged = described_without_fec(0, 1, &[100, 101, 102]);
        assert_eq!(judged.len(), 1);
        assert_eq!((judged[0].media, judged[0].not_media), (3, 0));
        assert!(judged[0].asks());
        // The same across the wrap of the 16-bit id: frame ids 65_436..,
        // so sequence number 100 is frame 0.
        let judged = described_without_fec(65_436, 1, &[99, 100, 101]);
        assert_eq!((judged[0].media, judged[0].unknown), (3, 0));
        assert!(judged[0].asks());
    }

    #[test]
    fn a_hole_between_consecutive_frames_is_not_media_and_asks_for_nothing() {
        // Frames of two packets and one FEC packet; the FEC packet after
        // frame 100 (sequence number 302) is lost, across the id wrap too.
        for first_frame in [0u16, 65_436] {
            let stream = model(0, first_frame, 200, 2, 1);
            let judged = run_model(&stream, &[302], &[], Knows::Descriptors);
            assert_eq!(judged.len(), 1);
            assert_eq!(judged[0].why, Why::NotMedia);
            assert_eq!((judged[0].media, judged[0].not_media), (0, 1));
        }
        // Even on a stream on which no FEC packet has ever been seen,
        // where one packet of unknown kind would ask: a number nothing
        // was sent under between two whole consecutive frames.
        let mut gapped = model(0, 7, 100, 2, 0);
        for (seq, _) in gapped.iter_mut().filter(|(seq, _)| *seq >= 150) {
            *seq += 1;
        }
        let judged = run_model(&gapped, &[], &[], Knows::Descriptors);
        assert_eq!(judged.len(), 1);
        assert_eq!(judged[0].why, Why::NotMedia);
        // The same hole without descriptors is of unknown kind, and asks.
        let judged = run_model(&gapped, &[], &[], Knows::Numbers);
        assert_eq!(judged.len(), 1);
        assert_eq!(judged[0].why, Why::Unexplained);
    }

    #[test]
    fn what_cannot_be_classified_is_unknown_and_falls_under_the_count_rule() {
        // Frame ids that do not count frames (simulcast layers share a
        // counter): every frame is two ids after the last. A lost FEC
        // packet between two whole frames is then of unknown kind.
        let mut skipping = model(0, 0, 200, 2, 1);
        for (_, sent) in &mut skipping {
            if let Sent::Media { frame, .. } = sent {
                *frame = frame.wrapping_mul(2);
            }
        }
        let judged = run_model(&skipping, &[302], &[], Knows::Descriptors);
        assert_eq!(judged.len(), 1);
        assert_eq!(
            (judged[0].media, judged[0].not_media, judged[0].unknown),
            (0, 0, 1)
        );
        assert_eq!(judged[0].why, Why::WithinFecShare);
        // And whole frames lost there are not counted from the ids either,
        // and no frame boundary is cut: two frames and their FEC packets
        // are six packets of unknown kind, more than a share of one third
        // explains.
        let lost: Vec<u64> = (300..306).collect();
        let judged = run_model(&skipping, &lost, &[], Knows::Descriptors);
        assert_eq!(judged.len(), 1);
        assert_eq!((judged[0].media, judged[0].not_media), (0, 0));
        assert_eq!(judged[0].why, Why::Unexplained);

        // Ids not yet seen to count frames: the stream is too young. One
        // packet a frame, 100 ms apart; a number nothing was sent under
        // after the third frame. When the burst is judged, seven frames
        // have been seen to follow the frame before, one short.
        let t0 = Instant::now();
        let mut b = BurstEnds::new();
        let mut judged = Vec::new();
        for frame in 0..9u16 {
            let seq = u64::from(frame) + u64::from(frame >= 3);
            let at = t0 + ms(100 * u64::from(frame));
            let part = PacketPosition {
                begins_frame: Some(frame),
                ends_frame: true,
            };
            b.on_described(1, seq, at, Arrived::Media, Some(part));
            judged.extend(b.poll(at));
        }
        assert_eq!(judged.len(), 1, "{judged:?}");
        assert_eq!((judged[0].not_media, judged[0].unknown), (0, 1));
        // One frame later it would have been known not to be media.
        let mut b = BurstEnds::new();
        let mut judged = Vec::new();
        for frame in 0..10u16 {
            let seq = u64::from(frame) + u64::from(frame >= 4);
            let at = t0 + ms(100 * u64::from(frame));
            let part = PacketPosition {
                begins_frame: Some(frame),
                ends_frame: true,
            };
            b.on_described(1, seq, at, Arrived::Media, Some(part));
            judged.extend(b.poll(at));
        }
        assert_eq!(judged.len(), 1, "{judged:?}");
        assert_eq!((judged[0].not_media, judged[0].unknown), (1, 0));

        // A packet without a descriptor next to the hole: unknown.
        let t0 = Instant::now();
        let mut b = BurstEnds::new();
        for (i, (seq, sent)) in model(0, 0, 200, 4, 0).iter().enumerate() {
            let at = t0 + ms(4 * i as u64);
            match seq {
                401 => {},
                400 | 402 => b.on_packet(1, *seq, at, Arrived::Media),
                _ => hand(&mut b, *seq, *sent, at, Knows::Descriptors),
            }
        }
        let judged = judged_over(&mut b, t0, t0 + ms(6000));
        assert_eq!(judged.len(), 1);
        assert_eq!((judged[0].1.media, judged[0].1.unknown), (0, 1));
        assert!(judged[0].1.asks(), "no FEC on the stream: one is too many");
    }

    #[test]
    fn an_fec_packet_inside_a_frame_takes_the_boundary_inference_away() {
        // A sender that puts its FEC packet before the last packet of
        // each frame: media, media, FEC, media. A lost packet after one
        // that does not end its frame may then be FEC.
        let t0 = Instant::now();
        let mut b = BurstEnds::new();
        let mut at = t0;
        for f in 0..200u64 {
            let frame = u16::try_from(f).expect("fits");
            for i in 0..4u64 {
                let seq = 4 * f + i;
                at = t0 + ms(4 * seq);
                if seq == 402 {
                    // The FEC packet of frame 100: lost.
                    continue;
                }
                if i == 2 {
                    b.on_packet(1, seq, at, Arrived::Fec);
                } else {
                    let part = PacketPosition {
                        begins_frame: (i == 0).then_some(frame),
                        ends_frame: i == 3,
                    };
                    b.on_described(1, seq, at, Arrived::Media, Some(part));
                }
            }
        }
        let judged = judged_over(&mut b, t0, at + ms(3000));
        assert_eq!(judged.len(), 1);
        let end = judged[0].1;
        assert_eq!((end.media, end.not_media, end.unknown), (0, 0, 1));
        assert!(!end.asks());
    }

    #[test]
    fn a_mask_names_only_packets_behind_its_own_and_marks_nothing_else() {
        let t0 = Instant::now();
        let mut b = BurstEnds::new();
        // A stream with FEC in it, so one unknown packet does not ask.
        for seq in 0..300u64 {
            let what = if seq % 3 == 2 {
                Arrived::Fec
            } else {
                Arrived::Media
            };
            if seq != 280 {
                b.on_packet(1, seq, t0 + ms(4 * seq), what);
            }
        }
        assert!(b.has_holes(1));
        assert!(!b.has_holes(2), "another stream has none");
        // Masks that name everything but the missing packet's place
        // behind the FEC packet: its own number, numbers ahead of it,
        // numbers further behind than the window, and another stream.
        b.fec_protects(1, 290, &[290, 291, 300, 290u16.wrapping_sub(600)]);
        b.fec_protects(2, 290, &[280]);
        b.fec_protects(1, 70_000, &[280]);
        // More numbers than a mask can hold: only the first 48 count.
        let mut long: Vec<u16> = vec![289; 48];
        long.push(280);
        b.fec_protects(1, 290, &long);
        let judged = judged_over(&mut b, t0, t0 + ms(4 * 300 + 3000));
        assert_eq!(judged.len(), 1);
        assert_eq!(judged[0].1.media, 0);
        assert!(!judged[0].1.asks());

        // The same stream, and a mask that does name it.
        let mut b = BurstEnds::new();
        for seq in 0..300u64 {
            let what = if seq % 3 == 2 {
                Arrived::Fec
            } else {
                Arrived::Media
            };
            if seq != 280 {
                b.on_packet(1, seq, t0 + ms(4 * seq), what);
            }
        }
        b.fec_protects(1, 290, &[279, 280, 281]);
        let judged = judged_over(&mut b, t0, t0 + ms(4 * 300 + 3000));
        assert_eq!(judged.len(), 1);
        assert_eq!((judged[0].1.media, judged[0].1.unknown), (1, 0));
        assert_eq!(judged[0].1.why, Why::MediaMissing);
    }

    #[test]
    fn a_packet_rebuilt_from_fec_or_sent_again_is_not_missing_media() {
        // Named by a mask, then rebuilt: it arrives like any late packet.
        let Measured {
            stream,
            lost,
            came,
            never,
        } = measured_case();
        let mut all = came;
        all.extend(never);
        all.sort_unstable();
        for knows in [Knows::Masks, Knows::Descriptors] {
            let judged = run_model(&stream, &lost, &all, knows);
            assert!(
                judged.iter().all(|end| !end.asks()),
                "{knows:?}: {judged:?}"
            );
            assert_eq!(judged.iter().map(|end| end.media).sum::<u64>(), 0);
        }
    }

    #[test]
    fn a_protected_packet_let_go_before_the_burst_is_judged_is_still_media() {
        let t0 = Instant::now();
        let mut b = BurstEnds::new();
        // A stream with FEC; 100 is lost and a mask names it. Then the
        // stream stops inside the burst for ten seconds.
        for seq in 0..103u64 {
            let what = if seq % 3 == 2 {
                Arrived::Fec
            } else {
                Arrived::Media
            };
            if seq != 100 {
                b.on_packet(1, seq, t0 + ms(4 * seq), what);
            }
        }
        b.fec_protects(1, 101, &[99, 100]);
        assert!(judged_over(&mut b, t0, t0 + ms(10_000)).is_empty());
        assert_eq!(b.open_holes(), 0, "let go after the span");
        let back = t0 + ms(10_000);
        for seq in 103..120u64 {
            b.on_packet(1, seq, back + ms(4 * (seq - 103)), Arrived::Media);
        }
        let judged = judged_over(&mut b, back, back + ms(1000));
        assert_eq!(judged.len(), 1);
        let end = judged[0].1;
        assert!(end.beyond_repair);
        assert_eq!((end.media, end.unknown), (1, 0));
        assert!(end.asks());

        // Pushed out of the window by the packets after it: the same.
        let mut b = BurstEnds::new();
        b.on_packet(1, 0, t0, Arrived::Fec);
        b.on_packet(1, 2, t0, Arrived::Fec);
        b.fec_protects(1, 2, &[1]);
        for seq in 3..=(RETRANSMISSION_WINDOW + 10) {
            b.on_packet(1, seq, t0 + ms(seq), Arrived::Media);
        }
        assert_eq!(b.open_holes(), 0);
        let judged = b.poll(t0 + ms(RETRANSMISSION_WINDOW + 200));
        assert_eq!(judged.len(), 1);
        assert_eq!(judged[0].media, 1);
        assert!(judged[0].asks());
    }

    #[test]
    fn the_new_state_is_bounded_whatever_a_peer_sends() {
        let t0 = Instant::now();
        let mut b: BurstEnds<u8> = BurstEnds::new();
        // Many streams, every second packet missing, descriptors with
        // arbitrary ids and flags, masks that name everything.
        let mut x: u32 = 0x9e37_79b9;
        let mut next = || {
            x ^= x << 13;
            x ^= x >> 17;
            x ^= x << 5;
            x
        };
        let mut seqs = [0u64; 40];
        for round in 0..20_000u64 {
            let r = next();
            let stream = u8::try_from(r % 40).expect("fits");
            let seq = &mut seqs[usize::from(stream)];
            *seq = match (r >> 8) % 16 {
                0 => seq.saturating_sub(u64::from(r >> 20)),
                1 => *seq + MAX_JUMP + 5,
                step => *seq + u64::from(step),
            };
            let at = t0 + ms(round);
            let what = match (r >> 12) % 3 {
                0 => Arrived::Fec,
                1 => Arrived::KeyFrameStart,
                _ => Arrived::Media,
            };
            let part = ((r >> 14) % 4 != 0).then_some(PacketPosition {
                begins_frame: ((r >> 16) % 2 == 0).then_some(u16::try_from(r >> 16).expect("fits")),
                ends_frame: (r >> 17) % 2 == 0,
            });
            b.on_described(stream, *seq, at, what, part);
            let named: Vec<u16> = (0..64u16)
                .map(|i| u16::try_from(*seq & 0xffff).expect("fits").wrapping_sub(i))
                .collect();
            b.fec_protects(stream, *seq, &named);
            if round % 7 == 0 {
                for end in b.poll(at) {
                    assert_eq!(end.unrepaired, end.media + end.not_media + end.unknown);
                    assert!(end.unrepaired <= end.lost);
                    assert!(end.unrepaired > 0);
                }
            }
            assert!(b.streams_kept() <= MAX_STREAMS);
            for s in b.streams.values() {
                assert!(s.holes.len() as u64 <= RETRANSMISSION_WINDOW);
                assert_eq!(s.arrived.len() as u64, ARRIVED_WINDOW);
            }
        }
        assert_eq!(std::mem::size_of::<Slot>(), 8);
    }
}
