use serde::{Deserialize, Serialize};
use std::fmt;

use crate::packet::MediaKind;
use crate::rtp_::Frequency;

use super::format_params::FormatParams;

/// Codec specification
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct CodecSpec {
    /// The codec identifier.
    pub codec: Codec,

    /// Clock rate of the codec.
    pub clock_rate: Frequency,

    /// Number of audio channels (if any).
    pub channels: Option<u8>,

    /// Codec specific format parameters. This might carry additional config for
    /// things like h264.
    pub format: FormatParams,
}

/// Known codecs.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[non_exhaustive]
#[allow(missing_docs)]
pub enum Codec {
    Opus,
    PCMU,
    PCMA,
    // LAKKO FORK: redundant audio (RFC 2198), `a=rtpmap:<pt> red/48000/2`.
    // Not a codec of its own: the payload is a container of Opus encodings.
    // It is negotiated and carried by payload type; str0m never looks inside
    // it. Its fmtp is `FormatParams::red`.
    Red,
    // LAKKO FORK: the video `red/90000` wrapper (RFC 2198 with one block) that
    // libwebrtc sends every video packet in once `red` and `ulpfec` are both
    // negotiated. Distinct from audio `Red`: the SDP name is the same, the
    // clock rate (90 kHz) tells them apart (see the rtpmap parser). Carried by
    // payload type; str0m never looks inside it.
    RedVideo,
    // LAKKO FORK: `ulpfec/90000` (RFC 5109). Never an RTP payload type on the
    // wire of its own: FEC packets travel inside `RedVideo`. It exists so the
    // payload type can be negotiated.
    Ulpfec,
    H264,
    // TODO show this when we support h265.
    #[doc(hidden)]
    H265,
    Vp8,
    Vp9,
    // TODO show this when we support Av1.
    #[doc(hidden)]
    Av1,
    /// Technically not a codec, but used in places where codecs go
    /// in `a=rtpmap` lines.
    #[doc(hidden)]
    Rtx,
    /// For RTP mode. No codec.
    #[doc(hidden)]
    Null,
    #[doc(hidden)]
    Unknown,
}

impl Codec {
    /// Tells if codec is audio.
    pub fn is_audio(&self) -> bool {
        use Codec::*;
        // LAKKO FORK: `Red` counts as audio so it is listed in `m=audio`.
        matches!(self, Opus | PCMU | PCMA | Red)
    }

    /// Tells if codec is video.
    pub fn is_video(&self) -> bool {
        use Codec::*;
        // LAKKO FORK: `RedVideo` and `Ulpfec` are listed in `m=video`.
        matches!(self, H265 | H264 | Vp8 | Vp9 | Av1 | RedVideo | Ulpfec)
    }

    /// Audio/Video.
    pub fn kind(&self) -> MediaKind {
        if self.is_audio() {
            MediaKind::Audio
        } else {
            MediaKind::Video
        }
    }
}

impl<'a> From<&'a str> for Codec {
    fn from(v: &'a str) -> Self {
        let lc = v.to_ascii_lowercase();
        match &lc[..] {
            "opus" => Codec::Opus,
            "pcmu" => Codec::PCMU,
            "pcma" => Codec::PCMA,
            // LAKKO FORK: see `Codec::Red`.
            "red" => Codec::Red,
            // LAKKO FORK: see `Codec::Ulpfec`.
            "ulpfec" => Codec::Ulpfec,
            "h264" => Codec::H264,
            "h265" => Codec::H265,
            "vp8" => Codec::Vp8,
            "vp9" => Codec::Vp9,
            "av1" => Codec::Av1,
            "rtx" => Codec::Rtx, // resends
            _ => Codec::Unknown,
        }
    }
}

impl fmt::Display for Codec {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Codec::Opus => write!(f, "opus"),
            Codec::PCMU => write!(f, "PCMU"),
            Codec::PCMA => write!(f, "PCMA"),
            // LAKKO FORK: see `Codec::Red`.
            Codec::Red => write!(f, "red"),
            // LAKKO FORK: see `Codec::RedVideo` and `Codec::Ulpfec`.
            Codec::RedVideo => write!(f, "red"),
            Codec::Ulpfec => write!(f, "ulpfec"),
            Codec::H264 => write!(f, "H264"),
            Codec::H265 => write!(f, "H265"),
            Codec::Vp8 => write!(f, "VP8"),
            Codec::Vp9 => write!(f, "VP9"),
            Codec::Av1 => write!(f, "AV1"),
            Codec::Rtx => write!(f, "rtx"),
            Codec::Null => write!(f, "null"),
            Codec::Unknown => write!(f, "unknown"),
        }
    }
}
