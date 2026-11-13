// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! Slim `lakko-attachments` surface — the decode/classify half only
//! (ADR-0041 Phase 2).
//!
//! Mirror of the read subset of `lakko-ffi-uniffi`'s `attachments` module:
//! [`parse_body_envelope`] + the [`ParsedBody`] / [`MessageBodyKind`] /
//! [`AttachmentRef`] shapes it returns. The compose / encrypt / decrypt-blob
//! functions are DELIBERATELY absent — the NSE only classifies an
//! already-decrypted MLS application body to render a preview; it never
//! composes an envelope or downloads a blob.

use lakko_attachments as inner;

/// The classification of a decoded message body (ADR-0023). Mirror of
/// [`lakko_attachments::MessageBody`] flattened for Swift: the `kind` selects
/// which of [`ParsedBody`]'s optional fields are meaningful.
#[derive(uniffi::Enum, Clone, Copy, PartialEq, Eq, Debug)]
pub enum MessageBodyKind {
    /// A plain text + attachments message.
    Chat,
    /// Edit of `target` — `text` is the new body.
    Edit,
    /// Delete (tombstone) of `target`.
    Delete,
    /// Reaction `emoji` to `target`.
    Reaction,
    /// ADR-0024 — an ephemeral typing signal (`is_typing`).
    Typing,
    /// ADR-0024 — a read receipt: read up to `target` (inclusive).
    ReadReceipt,
    /// A Lakko service envelope this build did not recognise (a newer
    /// profile, group-name, pin or beacon version). Never render it; drop it
    /// like any other service envelope. Appended last so every earlier
    /// variant keeps its ordinal.
    ServiceEnvelope,
    /// ADR-0061 section 6 — a chat envelope from a newer wire version; the
    /// version rides `target` and `text` is empty. The extension renders its
    /// own catalog word for the notification, never core prose. Appended
    /// last so every earlier variant keeps its ordinal.
    Unsupported,
}

/// A decoded attachment reference (ADR-0023).
///
/// Flat record of raw bytes; the base64 the JSON wire format uses is decoded
/// inside [`parse_body_envelope`]. The NSE renders the `name` / `mime` / `size`
/// into a preview but does not fetch the blob.
#[derive(uniffi::Record, Clone)]
pub struct AttachmentRef {
    /// Raw 32-byte SHA-256 blob id.
    pub blob_id: Vec<u8>,
    /// Raw 32-byte CEK.
    pub key: Vec<u8>,
    /// Raw 12-byte AEAD nonce.
    pub nonce: Vec<u8>,
    /// Optional MIME type (e.g. `image/jpeg`).
    pub mime: Option<String>,
    /// Plaintext size in bytes.
    pub size: u64,
    /// Optional human-visible filename.
    pub name: Option<String>,
    /// ADR-0047 — `"voice"`, `"video_note"`, `"gif"`, another (unknown)
    /// value, or `None` for a plain file. Lets a preview say "Voice
    /// message" instead of a file name.
    #[uniffi(default = None)]
    pub kind: Option<String>,
    /// ADR-0047 — playback length in ms (voice / video note only, bounded
    /// by the core parser).
    #[uniffi(default = None)]
    pub duration_ms: Option<u32>,
    /// ADR-0047 — raw waveform amplitude bytes, 1..=128.
    #[uniffi(default = None)]
    pub waveform: Option<Vec<u8>>,
    /// ADR-0047 — pixel width, 1..=4096.
    #[uniffi(default = None)]
    pub width: Option<u32>,
    /// ADR-0047 — pixel height, 1..=4096.
    #[uniffi(default = None)]
    pub height: Option<u32>,
    /// ADR-0047 — the poster-frame blob reference (decoded, never fetched
    /// here).
    #[uniffi(default = None)]
    pub thumb: Option<ThumbRef>,
}

/// ADR-0047 — a decoded poster-frame blob reference, the parse-side mirror
/// of the full surface's `ThumbRef`.
#[derive(uniffi::Record, Clone)]
pub struct ThumbRef {
    /// Raw 32-byte SHA-256 blob id.
    pub blob_id: Vec<u8>,
    /// Raw 32-byte CEK.
    pub key: Vec<u8>,
    /// Raw 12-byte AEAD nonce.
    pub nonce: Vec<u8>,
    /// Optional MIME type of the poster.
    pub mime: Option<String>,
    /// Plaintext size in bytes, 1..=256 KiB.
    pub size: u64,
    /// Pixel width, 1..=4096.
    #[uniffi(default = None)]
    pub width: Option<u32>,
    /// Pixel height, 1..=4096.
    #[uniffi(default = None)]
    pub height: Option<u32>,
}

/// Output of [`parse_body_envelope`]: the classified message body
/// (ADR-0023/0024). `text` + `attachments` are meaningful for `Chat` (and
/// `text` for `Edit`); `target` for every op; `emoji` for `Reaction`.
#[derive(uniffi::Record)]
pub struct ParsedBody {
    /// Which kind of message this is.
    pub kind: MessageBodyKind,
    /// Chat / edit text (empty for delete / reaction).
    pub text: String,
    /// Attachment references (chat only; empty otherwise).
    pub attachments: Vec<AttachmentRef>,
    /// The DS `messageId` the op acts on (`None` for `Chat`/`Typing`;
    /// for `ReadReceipt` it is the read-up-to watermark; for `Unsupported`
    /// the envelope's wire version).
    pub target: Option<i64>,
    /// The reaction emoji (`Some` only for `Reaction`).
    pub emoji: Option<String>,
    /// ADR-0024 — the typing state (`Some` only for `Typing`).
    pub is_typing: Option<bool>,
    /// ADR-0073 R.7: the group a re-sent message was first sent into
    /// (standard base64), carried only together with
    /// `resend_origin_message_id`. Never shown: the extension uses it to stay
    /// quiet about a message this device already holds.
    #[uniffi(default = None)]
    pub resend_origin_group_b64: Option<String>,
    /// ADR-0073 R.7: the DS `messageId` of that first send.
    #[uniffi(default = None)]
    pub resend_origin_message_id: Option<i64>,
    /// UX reactions-and-emoji section 12: add or take off (`Some` only for
    /// `Reaction`). The extension notifies only on an add, and a remove
    /// withdraws the notification an add raised.
    #[uniffi(default = None)]
    pub reaction_action: Option<ReactionAction>,
    /// UX reactions-and-emoji section 12: the 32-byte account key of the
    /// reacted-to message's author as the reactor states it (`Some` only for
    /// `Reaction`, when it decoded). The reactor's claim: the extension
    /// notifies only when it names this account, and never treats it as an
    /// identity.
    #[uniffi(default = None)]
    pub reaction_target_author: Option<Vec<u8>>,
    /// ADR-0087 section 14 — the message carries a channel invitation card
    /// (`Chat` only). The extension then says what the card says ("Invitation
    /// to {channel}" / "Invitation to publish in {channel}") and never the
    /// message text, which is the `/g/` link with its secret.
    #[uniffi(default = None)]
    pub channel_invite: Option<ChannelInviteNotice>,
}

/// What a notification needs from a channel invitation card, opened offline
/// (ADR-0087 section 14; UX 15.2 for a publisher invitation). Nothing of the
/// link itself: no secret, no ledger id.
#[derive(uniffi::Record, Clone, Debug, PartialEq, Eq)]
pub struct ChannelInviteNotice {
    /// The card opened as a channel's invitation. `false`: a card that does
    /// not open (the notification says only that a message came).
    pub opened: bool,
    /// The channel's name the card carries, `None` when it names none.
    pub channel_name: Option<String>,
    /// A publisher invitation (preview kind 2) rather than a subscriber one.
    pub publisher: bool,
}

/// The notice for the card bytes a chat envelope carried.
fn channel_invite_notice(card: &[u8]) -> ChannelInviteNotice {
    let opened = lakko_contacts::group_invite::ChannelInviteCard::decode(card)
        .and_then(|c| c.open())
        .ok();
    match opened {
        Some((_, preview)) => ChannelInviteNotice {
            opened: true,
            channel_name: preview
                .group_name
                .map(|n| n.trim().to_owned())
                .filter(|n| !n.is_empty()),
            publisher: preview.subscriber.is_none(),
        },
        None => ChannelInviteNotice {
            opened: false,
            channel_name: None,
            publisher: false,
        },
    }
}

/// Add or take off a reaction (UX reactions-and-emoji section 17), the
/// same enum the full surface exposes.
#[derive(uniffi::Enum, Clone, Copy, PartialEq, Eq, Debug)]
pub enum ReactionAction {
    /// Add the emoji to the sender's reactions on the target.
    Add,
    /// Take the emoji off the sender's reactions on the target.
    Remove,
}

impl From<inner::ReactionAction> for ReactionAction {
    fn from(a: inner::ReactionAction) -> Self {
        match a {
            inner::ReactionAction::Add => Self::Add,
            inner::ReactionAction::Remove => Self::Remove,
        }
    }
}

impl ParsedBody {
    /// A body of `kind` with every optional field empty.
    const fn of(kind: MessageBodyKind) -> Self {
        Self {
            kind,
            text: String::new(),
            attachments: Vec::new(),
            target: None,
            emoji: None,
            is_typing: None,
            resend_origin_group_b64: None,
            resend_origin_message_id: None,
            reaction_action: None,
            reaction_target_author: None,
            channel_invite: None,
        }
    }
}

/// Decode an MLS Application message body into a classified [`ParsedBody`]
/// (ADR-0023/0024). Never fails — a malformed / ambiguous op degrades to
/// `Chat`, the same fail-safe posture the full surface uses.
#[uniffi::export]
#[must_use]
pub fn parse_body_envelope(bytes: Vec<u8>) -> ParsedBody {
    match inner::parse_body(&bytes) {
        // ADR-0052: a reply's quote is not previewed by a notification, and
        // this surface stays the smallest that decrypts (C-59), so the reply
        // and the forward attribution are matched away rather than carried.
        // The ADR-0073 re-send marker IS carried: without it the extension
        // would notify a second time about a message the device holds.
        inner::MessageBody::Chat {
            text,
            attachments,
            resend,
            channel_invite,
            ..
        } => ParsedBody {
            text,
            attachments: refs_to_ffi(attachments),
            resend_origin_group_b64: resend.as_ref().map(|r| r.group_id_b64.clone()),
            resend_origin_message_id: resend.map(|r| r.message_id),
            channel_invite: channel_invite.as_deref().map(channel_invite_notice),
            ..ParsedBody::of(MessageBodyKind::Chat)
        },
        inner::MessageBody::Edit { target, text } => ParsedBody {
            text,
            target: Some(target),
            ..ParsedBody::of(MessageBodyKind::Edit)
        },
        inner::MessageBody::Delete { target } => ParsedBody {
            target: Some(target),
            ..ParsedBody::of(MessageBodyKind::Delete)
        },
        // UX reactions-and-emoji section 12: the action and the stated
        // target author are what the extension needs to notify on an add to
        // this account's message and to stay quiet on a remove.
        inner::MessageBody::Reaction {
            target,
            emoji,
            action,
            target_author,
        } => ParsedBody {
            target: Some(target),
            emoji: Some(emoji),
            reaction_action: Some(action.into()),
            reaction_target_author: target_author.map(|k| k.to_vec()),
            ..ParsedBody::of(MessageBodyKind::Reaction)
        },
        inner::MessageBody::Typing { is_typing } => ParsedBody {
            is_typing: Some(is_typing),
            ..ParsedBody::of(MessageBodyKind::Typing)
        },
        inner::MessageBody::ReadReceipt { up_to } => ParsedBody {
            target: Some(up_to),
            ..ParsedBody::of(MessageBodyKind::ReadReceipt)
        },
        inner::MessageBody::Unsupported { version } => ParsedBody {
            target: Some(i64::from(version)),
            ..ParsedBody::of(MessageBodyKind::Unsupported)
        },
        inner::MessageBody::ServiceEnvelope => ParsedBody::of(MessageBodyKind::ServiceEnvelope),
    }
}

/// Decode inner `AttachmentRef`s into the FFI flat shape, dropping any
/// malformed ref silently (the same policy as the full surface).
fn refs_to_ffi(refs: Vec<inner::AttachmentRef>) -> Vec<AttachmentRef> {
    refs.into_iter()
        .filter_map(|r| {
            let blob_id = r.decode_blob_id().ok()?;
            let key = r.decode_key().ok()?;
            let nonce = r.decode_nonce().ok()?;
            let waveform = r.decode_waveform();
            let thumb = r.thumb.as_ref().and_then(|t| {
                Some(ThumbRef {
                    blob_id: t.decode_blob_id().ok()?.to_vec(),
                    key: t.decode_key().ok()?.as_bytes().to_vec(),
                    nonce: t.decode_nonce().ok()?.to_vec(),
                    mime: t.mime.clone(),
                    size: t.size,
                    width: t.width,
                    height: t.height,
                })
            });
            Some(AttachmentRef {
                blob_id: blob_id.to_vec(),
                key: key.as_bytes().to_vec(),
                nonce: nonce.to_vec(),
                mime: r.mime,
                size: r.size,
                name: r.name,
                kind: r.kind,
                duration_ms: r.duration_ms,
                waveform,
                width: r.width,
                height: r.height,
                thumb,
            })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn media_metadata_reaches_the_extension_parse_shape() {
        let enc = inner::encrypt_attachment(b"voice").unwrap();
        let r = inner::AttachmentRef::from_encrypted(
            &enc,
            5,
            Some("audio/mp4".to_string()),
            Some("voice-1.m4a".to_string()),
        )
        .with_kind(inner::MEDIA_KIND_VOICE)
        .with_duration_ms(4200)
        .with_waveform(&[9, 8, 7]);
        let body = parse_body_envelope(inner::compose("", vec![r]).unwrap());
        assert_eq!(body.kind, MessageBodyKind::Chat);
        let got = &body.attachments[0];
        assert_eq!(got.kind.as_deref(), Some("voice"));
        assert_eq!(got.duration_ms, Some(4200));
        assert_eq!(got.waveform, Some(vec![9, 8, 7]));
        assert!(got.thumb.is_none());
    }

    #[test]
    fn a_reaction_carries_its_action_and_target_author() {
        let author = [7u8; inner::REACTION_TARGET_AUTHOR_LEN];
        let add = parse_body_envelope(
            inner::compose_reaction(42, "👍", inner::ReactionAction::Add, Some(&author)).unwrap(),
        );
        assert_eq!(add.kind, MessageBodyKind::Reaction);
        assert_eq!(add.target, Some(42));
        assert_eq!(add.reaction_action, Some(ReactionAction::Add));
        assert_eq!(add.reaction_target_author, Some(author.to_vec()));

        let remove = parse_body_envelope(
            inner::compose_reaction(42, "👍", inner::ReactionAction::Remove, None).unwrap(),
        );
        assert_eq!(remove.reaction_action, Some(ReactionAction::Remove));
        assert!(remove.reaction_target_author.is_none());

        let chat = parse_body_envelope(inner::compose("hi", Vec::new()).unwrap());
        assert!(chat.reaction_action.is_none());
        assert!(chat.reaction_target_author.is_none());
    }

    /// ADR-0087 section 14, UX 15.2 — a message carrying a publisher
    /// invitation card tells the extension what the card says, so the
    /// notification never reads the text (the `/g/` link with its secret).
    /// Seen live 2026-11-12 on an iPhone.
    #[test]
    fn a_channel_invitation_card_reaches_the_extension_as_a_notice() {
        use lakko_contacts::group_invite as gi;
        let secret = gi::generate_link_key();
        let preview = gi::GroupInvitePreview {
            group_name: Some("Walk 22 channel".into()),
            inviter_name: Some("Icons QA".into()),
            member_count: 0,
            channel_id: Some([0x42; 16]),
            subscriber: None,
            avatar: None,
        };
        let knock = gi::GroupKnockKey {
            public_key: [0x4B; 32],
            version: 1,
        };
        let signing = lakko_crypto::signing::SigningKey::from_seed(&[0x5A; 32]);
        let wire =
            gi::compose_with_preview(&signing, &[0x11; 32], 0, Some(&knock), &preview, &secret)
                .unwrap();
        let card = gi::ChannelInviteCard {
            ledger_id: [7; 6],
            link_secret: secret,
            sealed: gi::seal_link(&secret, &wire).unwrap(),
        }
        .encode()
        .unwrap();
        let body = parse_body_envelope(
            inner::compose_channel_invite("https://lakko.me/g/BwcHBwcH", &card).unwrap(),
        );
        assert_eq!(body.kind, MessageBodyKind::Chat);
        assert_eq!(
            body.channel_invite,
            Some(ChannelInviteNotice {
                opened: true,
                channel_name: Some("Walk 22 channel".into()),
                publisher: true,
            })
        );
        // A card that does not open is still a card: no text is previewed.
        let mut broken = card;
        let last = broken.len() - 1;
        broken[last] ^= 1;
        let body = parse_body_envelope(inner::compose_channel_invite("x", &broken).unwrap());
        assert_eq!(body.channel_invite.map(|n| n.opened), Some(false));
        // A plain message carries none.
        let plain = parse_body_envelope(inner::compose("hi", Vec::new()).unwrap());
        assert!(plain.channel_invite.is_none());
    }

    #[test]
    fn newer_wire_version_is_unsupported_with_no_text() {
        let body = parse_body_envelope(br#"{"v":7,"text":"future"}"#.to_vec());
        assert_eq!(body.kind, MessageBodyKind::Unsupported);
        assert_eq!(body.target, Some(7));
        assert!(body.text.is_empty());
    }
}
