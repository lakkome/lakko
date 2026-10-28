// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! When to offer translation and transcription — wasm surface.
//!
//! Mirror of `lakko-ffi-uniffi/src/translation.rs` over
//! [`lakko_ui_rules::translation`] (ADR-0082 section 6). Lists of detections
//! cross as two parallel arrays (tags and confidences), the shape the other
//! rules here use for flat data. Nothing here takes message text.

use wasm_bindgen::prelude::*;

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

/// What the device can do for one language, as its engine reports it.
#[wasm_bindgen(js_name = TranslationEngineState)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TranslationEngineState {
    /// Works now, on this device.
    Ready = 0,
    /// Works after a language pack is on the device.
    NeedsPack = 1,
    /// This device cannot do it for this language, locally.
    Unavailable = 2,
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

/// What the message menu offers for one message.
#[wasm_bindgen(js_name = TranslateOffer)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TranslateOffer {
    /// No `Translate` row.
    None = 0,
    /// A `Translate` row from the detected language.
    Translate = 1,
    /// A `Translate` row that opens with the language picker.
    TranslateAskLanguage = 2,
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

/// Whether the message menu offers `Translate`.
///
/// `detectedTag` empty means nothing was detected; `detectedPermille` is then
/// ignored.
#[wasm_bindgen(js_name = translationOffered)]
#[must_use]
#[allow(clippy::fn_params_excessive_bools, clippy::too_many_arguments)]
pub fn translation_offered(
    enabled: bool,
    has_text: bool,
    is_own: bool,
    detected_tag: &str,
    detected_permille: u16,
    engine_for_detected: TranslationEngineState,
    any_engine: bool,
    reading_languages: Vec<String>,
) -> TranslateOffer {
    let detected = (!detected_tag.is_empty()).then(|| RustDetected {
        tag: detected_tag.to_string(),
        confidence_permille: detected_permille,
    });
    let input = RustOfferInput {
        enabled,
        has_text,
        is_own,
        detected,
        engine_for_detected: engine_for_detected.into(),
        any_engine,
    };
    rust_translation_offered(&input, &reading_languages).into()
}

/// The language the conversation bar names, in canonical form, or `undefined`.
///
/// `recentTags[i]` and `recentPermille[i]` describe the same message, newest
/// first; extra entries of the longer array are ignored. Show the bar only
/// when the engine's answer for the returned language is not `Unavailable`.
#[wasm_bindgen(js_name = conversationBarLanguage)]
#[must_use]
pub fn conversation_bar_language(
    enabled: bool,
    auto_translate_on: bool,
    recent_tags: Vec<String>,
    recent_permille: Vec<u16>,
    dismissed: Vec<String>,
    reading_languages: Vec<String>,
) -> Option<String> {
    let recent_incoming = recent_tags
        .into_iter()
        .zip(recent_permille)
        .map(|(tag, confidence_permille)| RustDetected {
            tag,
            confidence_permille,
        })
        .collect();
    let input = RustBarInput {
        enabled,
        auto_translate_on,
        recent_incoming,
        dismissed,
    };
    rust_conversation_bar_language(&input, &reading_languages)
}

/// Whether a voice or video message gets the transcript button.
#[wasm_bindgen(js_name = transcriptOffered)]
#[must_use]
pub fn transcript_offered(enabled: bool, has_audio: bool, engine: TranslationEngineState) -> bool {
    rust_transcript_offered(enabled, has_audio, engine.into())
}

/// Whether a stored translation or transcript still describes the message.
#[wasm_bindgen(js_name = storedResultIsCurrent)]
#[must_use]
pub fn stored_result_is_current(
    stored_last_op_id: i64,
    current_last_op_id: i64,
    stored_target: &str,
    current_target: &str,
) -> bool {
    rust_stored_result_is_current(
        stored_last_op_id,
        current_last_op_id,
        stored_target,
        current_target,
    )
}

/// The stored form of a language tag, or `undefined` for a tag that names no
/// language.
#[wasm_bindgen(js_name = canonicalLanguageTag)]
#[must_use]
pub fn canonical_language_tag(tag: &str) -> Option<String> {
    rust_canonical_language_tag(tag)
}

/// Whether a reader of one tag reads the other.
#[wasm_bindgen(js_name = sameLanguage)]
#[must_use]
pub fn same_language(a: &str, b: &str) -> bool {
    rust_same_language(a, b)
}

/// Whether a tag is one of the person's reading languages.
#[wasm_bindgen(js_name = readsLanguage)]
#[must_use]
pub fn reads_language(reading_languages: Vec<String>, tag: &str) -> bool {
    rust_reads_language(&reading_languages, tag)
}

/// Detections below this confidence, in thousandths, are guesses.
#[wasm_bindgen(js_name = translationDetectionConfidentPermille)]
#[must_use]
pub fn translation_detection_confident_permille() -> u16 {
    DETECTION_CONFIDENT_PERMILLE
}

/// How many of the newest incoming messages the conversation bar reads.
#[wasm_bindgen(js_name = translationBarWindow)]
#[must_use]
pub fn translation_bar_window() -> u32 {
    u32::try_from(BAR_WINDOW).unwrap_or(u32::MAX)
}

/// How many confident detections the bar needs before it says anything.
#[wasm_bindgen(js_name = translationBarMinConfident)]
#[must_use]
pub fn translation_bar_min_confident() -> u32 {
    u32::try_from(BAR_MIN_CONFIDENT).unwrap_or(u32::MAX)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_surface_answers_like_the_core() {
        assert_eq!(
            translation_offered(
                true,
                true,
                false,
                "es",
                950,
                TranslationEngineState::Ready,
                true,
                vec!["en".into()]
            ),
            TranslateOffer::Translate
        );
        assert_eq!(
            translation_offered(
                true,
                true,
                false,
                "",
                0,
                TranslationEngineState::Unavailable,
                true,
                vec!["en".into()]
            ),
            TranslateOffer::TranslateAskLanguage
        );
        assert_eq!(
            conversation_bar_language(
                true,
                false,
                vec!["de".into(); 6],
                vec![900; 6],
                vec![],
                vec!["en".into()]
            )
            .as_deref(),
            Some("de")
        );
        assert!(stored_result_is_current(3, 3, "en", "en-GB"));
    }
}
