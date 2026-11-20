// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! When to offer translation and transcription — UniFFI surface.
//!
//! Mirror of `lakko-ffi-wasm/src/translation.rs` over
//! [`lakko_ui_rules::translation`] (ADR-0082 section 6). The engines are
//! platform code; these are only the decisions about them, so the three
//! clients offer the same controls for the same message. Only
//! [`detect_text_language`] takes message text: the on-device detector of
//! [`lakko_ui_rules::language_detect`] for a client with no usable platform
//! detector (Android). Every other function takes the tag a detector produced.

use lakko_ui_rules::language_detect::{
    candidates_from_hypotheses as rust_candidates_from_hypotheses,
    conversation_detection as rust_conversation_detection,
    detect_text_language as rust_detect_text_language,
    detect_text_language_candidates as rust_detect_text_language_candidates,
    order_picker_languages as rust_order_picker_languages,
    resolve_translation_source as rust_resolve_translation_source,
    ConversationLanguages as RustLanguages, LanguageCandidates as RustCandidates,
    LanguagePick as RustPick, MessageLanguage as RustMessageLanguage, MAX_CONVERSATION_DETECTIONS,
    MAX_CONVERSATION_PICKS,
};
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

/// The language a message's text is in, detected on this device, or `nil`
/// when the text has no letters or the language has no tag.
///
/// For a client without a usable platform detector (Android; ADR-0082 note of
/// 2026-11-15). The answer feeds `TranslateOfferInput.detected` and the bar's
/// detections unchanged: below `translationDetectionConfidentPermille()` it is
/// a guess and the translation opens with the language picker. The text is
/// read here and nowhere else; nothing is stored or logged.
#[uniffi::export]
#[must_use]
pub fn detect_text_language(text: String) -> Option<DetectedLanguage> {
    rust_detect_text_language(&text).map(|d| DetectedLanguage {
        tag: d.tag,
        confidence_permille: d.confidence_permille,
    })
}

/// A detection and the languages it was a near tie with (ADR-0082 note of
/// 2026-11-15): the close candidates.
#[derive(Debug, Clone, PartialEq, Eq, uniffi::Record)]
pub struct LanguageCandidates {
    /// The detection.
    pub detected: DetectedLanguage,
    /// Up to three other languages, nearest first; empty when confident.
    pub alternatives: Vec<String>,
}

impl From<RustCandidates> for LanguageCandidates {
    fn from(c: RustCandidates) -> Self {
        Self {
            detected: DetectedLanguage {
                tag: c.detected.tag,
                confidence_permille: c.detected.confidence_permille,
            },
            alternatives: c.alternatives,
        }
    }
}

impl From<LanguageCandidates> for RustCandidates {
    fn from(c: LanguageCandidates) -> Self {
        Self {
            detected: c.detected.into(),
            alternatives: c.alternatives,
        }
    }
}

/// One hypothesis of a platform detector: a tag and its probability in
/// thousandths.
#[derive(Debug, Clone, PartialEq, Eq, uniffi::Record)]
pub struct LanguageHypothesis {
    /// The detector's tag (BCP 47, any case).
    pub tag: String,
    /// Probability 0..=1000.
    pub probability_permille: u16,
}

/// [`detect_text_language`] with its close candidates. The text is read here
/// and nowhere else; nothing is stored or logged.
#[uniffi::export]
#[must_use]
pub fn detect_text_language_candidates(text: String) -> Option<LanguageCandidates> {
    rust_detect_text_language_candidates(&text).map(Into::into)
}

/// The close candidates from a platform detector's hypotheses
/// (`NLLanguageRecognizer.languageHypotheses`), through the same rule.
#[uniffi::export]
#[must_use]
pub fn language_candidates_from_hypotheses(
    text: String,
    hypotheses: Vec<LanguageHypothesis>,
) -> Option<LanguageCandidates> {
    let list: Vec<(String, u16)> = hypotheses
        .into_iter()
        .map(|h| (h.tag, h.probability_permille))
        .collect();
    rust_candidates_from_hypotheses(&text, &list).map(Into::into)
}

/// A language the person picked for a conversation, and when (ms).
#[derive(Debug, Clone, PartialEq, Eq, uniffi::Record)]
pub struct LanguagePick {
    /// The language.
    pub tag: String,
    /// When it was last picked, ms since the epoch.
    pub at_ms: u64,
}

/// The confidently detected language of one message.
#[derive(Debug, Clone, PartialEq, Eq, uniffi::Record)]
pub struct MessageLanguage {
    /// The message id.
    pub message_id: i64,
    /// The language.
    pub tag: String,
}

/// What this device knows of a conversation's languages (ADR-0082 note of
/// 2026-11-15): picks and per-message confident detections, in a canonical
/// bounded form. Stored by the client as it is; changed only through
/// [`conversation_languages_with_pick`] and
/// [`conversation_languages_with_detection`].
#[derive(Debug, Clone, Default, PartialEq, Eq, uniffi::Record)]
pub struct ConversationLanguages {
    /// One per language.
    pub picks: Vec<LanguagePick>,
    /// One per message id.
    pub detections: Vec<MessageLanguage>,
}

impl From<RustLanguages> for ConversationLanguages {
    fn from(r: RustLanguages) -> Self {
        Self {
            picks: r
                .picks
                .into_iter()
                .map(|p| LanguagePick {
                    tag: p.tag,
                    at_ms: p.at_ms,
                })
                .collect(),
            detections: r
                .detections
                .into_iter()
                .map(|d| MessageLanguage {
                    message_id: d.message_id,
                    tag: d.tag,
                })
                .collect(),
        }
    }
}

impl From<ConversationLanguages> for RustLanguages {
    fn from(r: ConversationLanguages) -> Self {
        Self {
            picks: r
                .picks
                .into_iter()
                .map(|p| RustPick {
                    tag: p.tag,
                    at_ms: p.at_ms,
                })
                .collect(),
            detections: r
                .detections
                .into_iter()
                .map(|d| RustMessageLanguage {
                    message_id: d.message_id,
                    tag: d.tag,
                })
                .collect(),
        }
    }
}

/// The record with a pick (`Not X?` or the picker) of `tag` at `at_ms`.
#[uniffi::export]
#[must_use]
pub fn conversation_languages_with_pick(
    languages: ConversationLanguages,
    tag: String,
    at_ms: u64,
) -> ConversationLanguages {
    RustLanguages::from(languages).with_pick(&tag, at_ms).into()
}

/// The record with message `message_id` confidently detected as `tag`.
#[uniffi::export]
#[must_use]
pub fn conversation_languages_with_detection(
    languages: ConversationLanguages,
    message_id: i64,
    tag: String,
) -> ConversationLanguages {
    RustLanguages::from(languages)
        .with_detection(message_id, &tag)
        .into()
}

/// The conversation's usual language: the latest pick, else the most
/// detected one (a tie by tag); `nil` with neither.
#[uniffi::export]
#[must_use]
pub fn conversation_languages_usual(languages: ConversationLanguages) -> Option<String> {
    RustLanguages::from(languages).usual()
}

/// How many messages' detections and picked languages a record keeps.
#[uniffi::export]
#[must_use]
pub fn conversation_languages_limits() -> Vec<u32> {
    vec![
        u32::try_from(MAX_CONVERSATION_DETECTIONS).unwrap_or(u32::MAX),
        u32::try_from(MAX_CONVERSATION_PICKS).unwrap_or(u32::MAX),
    ]
}

/// The language a message is taken to be in: a confident detection, else
/// the conversation's language among its close candidates (a picked one,
/// else the most detected one), else the detection.
#[uniffi::export]
#[must_use]
pub fn conversation_detection(
    candidates: Option<LanguageCandidates>,
    languages: ConversationLanguages,
) -> Option<DetectedLanguage> {
    let candidates: Option<RustCandidates> = candidates.map(Into::into);
    rust_conversation_detection(candidates.as_ref(), &languages.into()).map(|d| DetectedLanguage {
        tag: d.tag,
        confidence_permille: d.confidence_permille,
    })
}

/// The `Not X?` list: `languages` with the message's close candidates first.
#[uniffi::export]
#[must_use]
pub fn order_picker_languages(
    languages: Vec<String>,
    candidates: Option<LanguageCandidates>,
) -> Vec<String> {
    let candidates: Option<RustCandidates> = candidates.map(Into::into);
    rust_order_picker_languages(&languages, candidates.as_ref())
}

/// What the line under a message names and what `Translate` starts from,
/// decided once so they cannot disagree (ADR-0082 note of 2026-11-15).
#[derive(Debug, Clone, PartialEq, Eq, uniffi::Record)]
pub struct TranslationSourceResolution {
    /// The language the line names; `nil` for no line.
    pub line_tag: Option<String>,
    /// Confidence of the detection the line stands on (for the reading rule).
    pub line_confidence_permille: u16,
    /// Whether the line carries `Translate`; `false` is a name alone.
    pub line_can_translate: bool,
    /// The language `Translate` starts from; `nil` opens the picker.
    pub translate_source: Option<String>,
    /// The guess the picker marks.
    pub guess: Option<String>,
}

/// The source of a message's translation and the language its line names:
/// a confident detection; the conversation's language among the close
/// candidates; the guess when usable; the first other candidate the engine
/// can translate from; for `Translate` only, the conversation's usual
/// language; otherwise the picker. `translatable` lists the languages the
/// engine answered it can translate from into `target` (ask at least every
/// candidate tag and the usual language).
#[uniffi::export]
#[must_use]
pub fn resolve_translation_source(
    candidates: Option<LanguageCandidates>,
    languages: ConversationLanguages,
    target: String,
    translatable: Vec<String>,
) -> TranslationSourceResolution {
    let candidates: Option<RustCandidates> = candidates.map(Into::into);
    let r = rust_resolve_translation_source(
        candidates.as_ref(),
        &languages.into(),
        &target,
        &translatable,
    );
    TranslationSourceResolution {
        line_tag: r.line_tag,
        line_confidence_permille: r.line_confidence_permille,
        line_can_translate: r.line_can_translate,
        translate_source: r.translate_source,
        guess: r.guess,
    }
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

    #[test]
    fn detection_crosses_the_surface_and_feeds_the_offer() {
        let detected = detect_text_language(
            "Ich gehe morgen auf den Markt, um Gemüse zu kaufen. Willst du mitkommen?".into(),
        );
        assert_eq!(detected.as_ref().map(|d| d.tag.as_str()), Some("de"));
        let input = TranslateOfferInput {
            enabled: true,
            has_text: true,
            is_own: false,
            detected,
            engine_for_detected: TranslationEngineState::Ready,
            any_engine: true,
        };
        assert_eq!(
            translation_offered(input, vec!["en".into()]),
            TranslateOffer::Translate
        );
        assert_eq!(detect_text_language("12345".into()), None);
    }

    #[test]
    fn the_source_resolution_crosses_the_surface() {
        let c = detect_text_language_candidates("hola, qué tal?".into());
        let r = resolve_translation_source(
            c.clone(),
            ConversationLanguages::default(),
            "en".into(),
            vec!["es".into(), "fr".into()],
        );
        // An unsure guess is not named up front; Translate starts from es.
        assert_eq!(r.line_tag, None);
        assert!(!r.line_can_translate);
        assert_eq!(r.translate_source.as_deref(), Some("es"));
    }

    #[test]
    fn the_conversation_language_crosses_the_surface() {
        // G55 Two: es and it once each, Spanish first among the candidates.
        let mut record = ConversationLanguages::default();
        for (id, tag) in [(2, "it"), (5, "ru"), (1, "es"), (4, "de"), (3, "uk")] {
            record = conversation_languages_with_detection(record, id, tag.into());
        }
        let hola = LanguageCandidates {
            detected: DetectedLanguage {
                tag: "ca".into(),
                confidence_permille: 180,
            },
            alternatives: vec!["es".into(), "fr".into(), "it".into()],
        };
        assert_eq!(
            conversation_detection(Some(hola), record.clone()).map(|d| d.tag),
            Some("es".to_string())
        );
        assert_eq!(conversation_languages_usual(record), Some("de".to_string()));
    }

    #[test]
    fn candidates_cross_the_surface() {
        let c = detect_text_language_candidates("hola, qué tal?".into());
        assert!(c
            .as_ref()
            .is_some_and(|c| c.alternatives.iter().any(|a| a == "es")));
        let picked =
            conversation_languages_with_pick(ConversationLanguages::default(), "es".into(), 1);
        let chosen = conversation_detection(c.clone(), picked);
        assert_eq!(chosen.map(|d| d.tag), Some("es".to_string()));
        let h = language_candidates_from_hypotheses(
            "hola".into(),
            vec![
                LanguageHypothesis {
                    tag: "ca".into(),
                    probability_permille: 400,
                },
                LanguageHypothesis {
                    tag: "es".into(),
                    probability_permille: 350,
                },
            ],
        );
        assert_eq!(h.map(|c| c.alternatives), Some(vec!["es".to_string()]));
        let ordered = order_picker_languages(vec!["de".into(), "es".into()], c);
        assert_eq!(ordered, vec!["es".to_string(), "de".to_string()]);
    }
}
