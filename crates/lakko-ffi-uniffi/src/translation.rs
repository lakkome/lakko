// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! When to offer translation and transcription — UniFFI surface.
//!
//! Mirror of `lakko-ffi-wasm/src/translation.rs` over
//! [`lakko_ui_rules::translation`] (ADR-0082 section 6). The engines are
//! platform code; these are only the decisions about them, so the three
//! clients offer the same controls for the same message. Nothing here takes
//! message text: a client passes the tag its own detector produced.

use lakko_ui_rules::translation::{
    canonical_language_tag as rust_canonical_language_tag,
    conversation_bar_language as rust_conversation_bar_language,
    reads_language as rust_reads_language, same_language as rust_same_language,
    stored_result_is_current as rust_stored_result_is_current,
    transcript_offered as rust_transcript_offered, translation_offered as rust_translation_offered,
    ConversationBarInput as RustBarInput, DetectedLanguage as RustDetected,
    EngineState as RustEngineState, TranslateOffer as RustOffer,
    TranslateOfferInput as RustOfferInput, BAR_MIN_CONFIDENT, BAR_WINDOW,
    DETECTION_CONFIDENT_PERMILLE,
};

/// What the device can do for one language, as its platform engine reports it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, uniffi::Enum)]
pub enum TranslationEngineState {
    /// Works now, on this device.
    Ready,
    /// Works after a language pack is on the device.
    NeedsPack,
    /// This device cannot do it for this language, locally. The control is not
    /// offered; there is no other way.
    Unavailable,
}

impl From<TranslationEngineState> for RustEngineState {
    fn from(s: TranslationEngineState) -> Self {
        match s {
            TranslationEngineState::Ready => Self::Ready,
            TranslationEngineState::NeedsPack => Self::NeedsPack,
            TranslationEngineState::Unavailable => Self::Unavailable,
        }
    }
}

/// A language a detector found, and how sure it was in thousandths.
#[derive(Debug, Clone, PartialEq, Eq, uniffi::Record)]
pub struct DetectedLanguage {
    /// The detector's tag (BCP 47, any case).
    pub tag: String,
    /// Confidence 0..=1000.
    pub confidence_permille: u16,
}

impl From<DetectedLanguage> for RustDetected {
    fn from(d: DetectedLanguage) -> Self {
        Self {
            tag: d.tag,
            confidence_permille: d.confidence_permille,
        }
    }
}

/// What the message menu offers for one message.
#[derive(Debug, Clone, Copy, PartialEq, Eq, uniffi::Enum)]
pub enum TranslateOffer {
    /// No `Translate` row.
    None,
    /// A `Translate` row from the detected language.
    Translate,
    /// A `Translate` row that opens with the language picker.
    TranslateAskLanguage,
}

impl From<RustOffer> for TranslateOffer {
    fn from(o: RustOffer) -> Self {
        match o {
            RustOffer::None => Self::None,
            RustOffer::Translate => Self::Translate,
            RustOffer::TranslateAskLanguage => Self::TranslateAskLanguage,
        }
    }
}

/// Inputs for one message's `Translate` row.
// Four independent facts about one message; an enum would merge them.
#[allow(clippy::struct_excessive_bools)]
#[derive(Debug, Clone, PartialEq, Eq, uniffi::Record)]
pub struct TranslateOfferInput {
    /// Settings: `Show Translate button`.
    pub enabled: bool,
    /// The message has text (a caption or an existing transcript).
    pub has_text: bool,
    /// The person wrote it.
    pub is_own: bool,
    /// What the detector found, if anything.
    pub detected: Option<DetectedLanguage>,
    /// The engine's answer for the detected language into the target.
    pub engine_for_detected: TranslationEngineState,
    /// Whether the device can translate from any language into the target.
    pub any_engine: bool,
}

/// Inputs for the "Messages here are in ..." bar of one conversation.
#[derive(Debug, Clone, PartialEq, Eq, uniffi::Record)]
pub struct ConversationBarInput {
    /// Settings: `Show Translate button`.
    pub enabled: bool,
    /// Automatic translation is on for this conversation.
    pub auto_translate_on: bool,
    /// Detections of the newest incoming messages with text, newest first.
    pub recent_incoming: Vec<DetectedLanguage>,
    /// Languages whose bar was dismissed in this conversation.
    pub dismissed: Vec<String>,
}

/// Whether the message menu offers `Translate`, and in which form.
#[uniffi::export]
#[must_use]
pub fn translation_offered(
    input: TranslateOfferInput,
    reading_languages: Vec<String>,
) -> TranslateOffer {
    let rust = RustOfferInput {
        enabled: input.enabled,
        has_text: input.has_text,
        is_own: input.is_own,
        detected: input.detected.map(Into::into),
        engine_for_detected: input.engine_for_detected.into(),
        any_engine: input.any_engine,
    };
    rust_translation_offered(&rust, &reading_languages).into()
}

/// The language the conversation bar names, in canonical form, or `nil`.
///
/// Show the bar only when the engine's answer for the returned language is not
/// `unavailable`.
#[uniffi::export]
#[must_use]
pub fn conversation_bar_language(
    input: ConversationBarInput,
    reading_languages: Vec<String>,
) -> Option<String> {
    let rust = RustBarInput {
        enabled: input.enabled,
        auto_translate_on: input.auto_translate_on,
        recent_incoming: input.recent_incoming.into_iter().map(Into::into).collect(),
        dismissed: input.dismissed,
    };
    rust_conversation_bar_language(&rust, &reading_languages)
}

/// Whether a voice or video message gets the transcript button.
#[uniffi::export]
#[must_use]
pub fn transcript_offered(enabled: bool, has_audio: bool, engine: TranslationEngineState) -> bool {
    rust_transcript_offered(enabled, has_audio, engine.into())
}

/// Whether a stored translation or transcript still describes the message.
#[uniffi::export]
#[must_use]
pub fn stored_result_is_current(
    stored_last_op_id: i64,
    current_last_op_id: i64,
    stored_target: String,
    current_target: String,
) -> bool {
    rust_stored_result_is_current(
        stored_last_op_id,
        current_last_op_id,
        &stored_target,
        &current_target,
    )
}

/// The stored form of a language tag (`pt-BR` -> `pt`, `zh-TW` -> `zh-Hant`),
/// or `nil` for a tag that names no language.
#[uniffi::export]
#[must_use]
pub fn canonical_language_tag(tag: String) -> Option<String> {
    rust_canonical_language_tag(&tag)
}

/// Whether a reader of one tag reads the other.
#[uniffi::export]
#[must_use]
pub fn same_language(a: String, b: String) -> bool {
    rust_same_language(&a, &b)
}

/// Whether a tag is one of the person's reading languages.
#[uniffi::export]
#[must_use]
pub fn reads_language(reading_languages: Vec<String>, tag: String) -> bool {
    rust_reads_language(&reading_languages, &tag)
}

/// Detections below this confidence, in thousandths, are guesses.
#[uniffi::export]
#[must_use]
pub fn translation_detection_confident_permille() -> u16 {
    DETECTION_CONFIDENT_PERMILLE
}

/// How many of the newest incoming messages the conversation bar reads.
#[uniffi::export]
#[must_use]
pub fn translation_bar_window() -> u32 {
    u32::try_from(BAR_WINDOW).unwrap_or(u32::MAX)
}

/// How many confident detections the bar needs before it says anything.
#[uniffi::export]
#[must_use]
pub fn translation_bar_min_confident() -> u32 {
    u32::try_from(BAR_MIN_CONFIDENT).unwrap_or(u32::MAX)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_surface_answers_like_the_core() {
        let input = TranslateOfferInput {
            enabled: true,
            has_text: true,
            is_own: false,
            detected: Some(DetectedLanguage {
                tag: "es".into(),
                confidence_permille: 950,
            }),
            engine_for_detected: TranslationEngineState::NeedsPack,
            any_engine: true,
        };
        assert_eq!(
            translation_offered(input, vec!["en".into()]),
            TranslateOffer::Translate
        );
        let bar = ConversationBarInput {
            enabled: true,
            auto_translate_on: false,
            recent_incoming: vec![
                DetectedLanguage {
                    tag: "de-AT".into(),
                    confidence_permille: 900
                };
                6
            ],
            dismissed: vec![],
        };
        assert_eq!(
            conversation_bar_language(bar, vec!["en".into()]).as_deref(),
            Some("de")
        );
        assert!(!transcript_offered(
            true,
            true,
            TranslationEngineState::Unavailable
        ));
        assert_eq!(
            canonical_language_tag("zh-TW".into()).as_deref(),
            Some("zh-Hant")
        );
    }
}
