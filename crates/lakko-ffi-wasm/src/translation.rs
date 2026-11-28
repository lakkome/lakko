// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! When to offer translation and transcription — wasm surface.
//!
//! Mirror of `lakko-ffi-uniffi/src/translation.rs` over
//! [`lakko_ui_rules::translation`] (ADR-0082 section 6). Lists of detections
//! cross as two parallel arrays (tags and confidences), the shape the other
//! rules here use for flat data. Only [`detect_text_language`] takes message
//! text: the on-device detector of [`lakko_ui_rules::language_detect`], for a
//! browser whose own `LanguageDetector` is absent or has no model on the
//! device.

use wasm_bindgen::prelude::*;

use lakko_ui_rules::language_detect::{
    candidates_from_hypotheses as rust_candidates_from_hypotheses,
    conversation_detection as rust_conversation_detection,
    detect_text_language as rust_detect_text_language,
    detect_text_language_candidates as rust_detect_text_language_candidates,
    order_picker_languages as rust_order_picker_languages,
    resolve_translation_source as rust_resolve_translation_source,
    ConversationLanguages as RustLanguages, LanguageCandidates as RustCandidates,
    LanguagePick as RustPick, MessageLanguage as RustMessageLanguage,
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
pub fn translation_offered(
    enabled: bool,
    has_text: bool,
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

/// A language detected in some text: the tag and the confidence in
/// thousandths, on the scale `translationDetectionConfidentPermille()` reads.
#[wasm_bindgen(js_name = DetectedTextLanguage)]
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DetectedTextLanguage {
    tag: String,
    confidence_permille: u16,
}

#[wasm_bindgen(js_class = DetectedTextLanguage)]
impl DetectedTextLanguage {
    /// The BCP 47 tag (`nl`, `ru`, `pt`, `zh`, ...), never with a region.
    #[wasm_bindgen(getter)]
    #[must_use]
    pub fn tag(&self) -> String {
        self.tag.clone()
    }

    /// Confidence 0..=1000; below `translationDetectionConfidentPermille()` it
    /// is a guess and the translation opens with the language picker.
    #[wasm_bindgen(getter, js_name = confidencePermille)]
    #[must_use]
    // wasm_bindgen cannot export a `const fn`.
    #[allow(clippy::missing_const_for_fn)]
    pub fn confidence_permille(&self) -> u16 {
        self.confidence_permille
    }
}

/// The language a message's text is in, detected on this device, or
/// `undefined` when the text has no letters or the language has no tag.
///
/// For a browser whose own `LanguageDetector` is absent or has no model on the
/// device (ADR-0082 note of 2026-11-15). The text is read here and nowhere
/// else; nothing is stored or logged.
#[wasm_bindgen(js_name = detectTextLanguage)]
#[must_use]
pub fn detect_text_language(text: &str) -> Option<DetectedTextLanguage> {
    rust_detect_text_language(text).map(|d| DetectedTextLanguage {
        tag: d.tag,
        confidence_permille: d.confidence_permille,
    })
}

/// A detection and the languages it was a near tie with (ADR-0082 note of
/// 2026-11-15).
#[wasm_bindgen(js_name = TextLanguageCandidates)]
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TextLanguageCandidates {
    tag: String,
    confidence_permille: u16,
    alternatives: Vec<String>,
}

impl From<RustCandidates> for TextLanguageCandidates {
    fn from(c: RustCandidates) -> Self {
        Self {
            tag: c.detected.tag,
            confidence_permille: c.detected.confidence_permille,
            alternatives: c.alternatives,
        }
    }
}

#[wasm_bindgen(js_class = TextLanguageCandidates)]
impl TextLanguageCandidates {
    /// The detected language.
    #[wasm_bindgen(getter)]
    #[must_use]
    pub fn tag(&self) -> String {
        self.tag.clone()
    }

    /// The detection's confidence, 0..=1000.
    #[wasm_bindgen(getter, js_name = confidencePermille)]
    #[must_use]
    // wasm_bindgen cannot export a `const fn`.
    #[allow(clippy::missing_const_for_fn)]
    pub fn confidence_permille(&self) -> u16 {
        self.confidence_permille
    }

    /// Up to three other close languages, nearest first; empty when confident.
    #[wasm_bindgen(getter)]
    #[must_use]
    pub fn alternatives(&self) -> Vec<String> {
        self.alternatives.clone()
    }
}

fn rust_candidates(
    tag: String,
    confidence_permille: u16,
    alternatives: Vec<String>,
) -> RustCandidates {
    RustCandidates {
        detected: RustDetected {
            tag,
            confidence_permille,
        },
        alternatives,
    }
}

/// [`detect_text_language`] with its close candidates, or `undefined`.
#[wasm_bindgen(js_name = detectTextLanguageCandidates)]
#[must_use]
pub fn detect_text_language_candidates(text: &str) -> Option<TextLanguageCandidates> {
    rust_detect_text_language_candidates(text).map(Into::into)
}

/// The close candidates from a platform detector's hypotheses (the browser
/// `LanguageDetector`): tags and probabilities in thousandths, as parallel
/// arrays.
#[wasm_bindgen(js_name = languageCandidatesFromHypotheses)]
#[must_use]
pub fn language_candidates_from_hypotheses(
    text: &str,
    tags: Vec<String>,
    probabilities_permille: Vec<u16>,
) -> Option<TextLanguageCandidates> {
    let list: Vec<(String, u16)> = tags.into_iter().zip(probabilities_permille).collect();
    rust_candidates_from_hypotheses(text, &list).map(Into::into)
}

/// The largest integer a JS number holds exactly.
const MAX_SAFE_INTEGER: f64 = 9_007_199_254_740_991.0;

/// A JS number that is a whole number in the safe range, else `None`.
fn safe_integer(n: f64) -> Option<i64> {
    if !n.is_finite() || n.fract() != 0.0 || n.abs() > MAX_SAFE_INTEGER {
        return None;
    }
    // Checked above: whole and within 2^53.
    #[allow(clippy::cast_possible_truncation)]
    Some(n as i64)
}

/// What this device knows of a conversation's languages (ADR-0082 note of
/// 2026-11-15): picks and per-message confident detections, canonical and
/// bounded. Stored by the client through the four array getters and rebuilt
/// with the constructor; entries that are not whole numbers in the safe
/// range are dropped.
#[wasm_bindgen(js_name = ConversationLanguages)]
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ConversationLanguages {
    inner: RustLanguages,
}

#[wasm_bindgen(js_class = ConversationLanguages)]
impl ConversationLanguages {
    /// A record from its stored arrays (parallel: tag and time per pick, id
    /// and tag per detection).
    #[wasm_bindgen(constructor)]
    #[must_use]
    pub fn new(
        pick_tags: Vec<String>,
        pick_at_ms: Vec<f64>,
        detection_ids: Vec<f64>,
        detection_tags: Vec<String>,
    ) -> Self {
        let picks = pick_tags
            .into_iter()
            .zip(pick_at_ms)
            .filter_map(|(tag, at)| {
                let at = safe_integer(at).and_then(|a| u64::try_from(a).ok())?;
                Some(RustPick { tag, at_ms: at })
            })
            .collect();
        let detections = detection_ids
            .into_iter()
            .zip(detection_tags)
            .filter_map(|(id, tag)| {
                Some(RustMessageLanguage {
                    message_id: safe_integer(id)?,
                    tag,
                })
            })
            .collect();
        Self {
            inner: RustLanguages { picks, detections }.normalized(),
        }
    }

    /// The record with a pick (`Not X?` or the picker) of `tag` at `at_ms`.
    #[wasm_bindgen(js_name = withPick)]
    #[must_use]
    pub fn with_pick(&self, tag: &str, at_ms: f64) -> Self {
        match safe_integer(at_ms).and_then(|a| u64::try_from(a).ok()) {
            Some(at) => Self {
                inner: self.inner.with_pick(tag, at),
            },
            None => self.clone(),
        }
    }

    /// The record with message `message_id` confidently detected as `tag`.
    #[wasm_bindgen(js_name = withDetection)]
    #[must_use]
    pub fn with_detection(&self, message_id: f64, tag: &str) -> Self {
        match safe_integer(message_id) {
            Some(id) => Self {
                inner: self.inner.with_detection(id, tag),
            },
            None => self.clone(),
        }
    }

    /// The conversation's usual language: the latest pick, else the most
    /// detected one (a tie by tag).
    #[must_use]
    pub fn usual(&self) -> Option<String> {
        self.inner.usual()
    }

    /// Picked languages.
    #[wasm_bindgen(getter, js_name = pickTags)]
    #[must_use]
    pub fn pick_tags(&self) -> Vec<String> {
        self.inner.picks.iter().map(|p| p.tag.clone()).collect()
    }

    /// When each was last picked, ms.
    #[wasm_bindgen(getter, js_name = pickAtMs)]
    #[must_use]
    pub fn pick_at_ms(&self) -> Vec<f64> {
        // Stored values came in through `safe_integer`, so they fit.
        #[allow(clippy::cast_precision_loss)]
        self.inner.picks.iter().map(|p| p.at_ms as f64).collect()
    }

    /// The message ids with a detection.
    #[wasm_bindgen(getter, js_name = detectionIds)]
    #[must_use]
    pub fn detection_ids(&self) -> Vec<f64> {
        #[allow(clippy::cast_precision_loss)]
        self.inner
            .detections
            .iter()
            .map(|d| d.message_id as f64)
            .collect()
    }

    /// Each message's detected language.
    #[wasm_bindgen(getter, js_name = detectionTags)]
    #[must_use]
    pub fn detection_tags(&self) -> Vec<String> {
        self.inner
            .detections
            .iter()
            .map(|d| d.tag.clone())
            .collect()
    }
}

/// The language a message is taken to be in, from its candidates (the
/// detection's tag, confidence and alternatives) and the conversation's
/// languages.
#[wasm_bindgen(js_name = conversationDetection)]
#[must_use]
pub fn conversation_detection(
    tag: String,
    confidence_permille: u16,
    alternatives: Vec<String>,
    languages: &ConversationLanguages,
) -> Option<DetectedTextLanguage> {
    let candidates = rust_candidates(tag, confidence_permille, alternatives);
    rust_conversation_detection(Some(&candidates), &languages.inner).map(|d| DetectedTextLanguage {
        tag: d.tag,
        confidence_permille: d.confidence_permille,
    })
}

/// What the line under a message names and what `Translate` starts from,
/// decided once so they cannot disagree (ADR-0082 note of 2026-11-15).
#[wasm_bindgen(js_name = TranslationSourceResolution)]
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TranslationSourceResolution {
    line_tag: Option<String>,
    line_confidence_permille: u16,
    line_can_translate: bool,
    translate_source: Option<String>,
    guess: Option<String>,
}

#[wasm_bindgen(js_class = TranslationSourceResolution)]
impl TranslationSourceResolution {
    /// The language the line names; `undefined` for no line.
    #[wasm_bindgen(getter, js_name = lineTag)]
    #[must_use]
    pub fn line_tag(&self) -> Option<String> {
        self.line_tag.clone()
    }

    /// Confidence of the detection the line stands on.
    #[wasm_bindgen(getter, js_name = lineConfidencePermille)]
    #[must_use]
    // wasm_bindgen cannot export a `const fn`.
    #[allow(clippy::missing_const_for_fn)]
    pub fn line_confidence_permille(&self) -> u16 {
        self.line_confidence_permille
    }

    /// Whether the line carries `Translate`; `false` is a name alone.
    #[wasm_bindgen(getter, js_name = lineCanTranslate)]
    #[must_use]
    // wasm_bindgen cannot export a `const fn`.
    #[allow(clippy::missing_const_for_fn)]
    pub fn line_can_translate(&self) -> bool {
        self.line_can_translate
    }

    /// The language `Translate` starts from; `undefined` opens the picker.
    #[wasm_bindgen(getter, js_name = translateSource)]
    #[must_use]
    pub fn translate_source(&self) -> Option<String> {
        self.translate_source.clone()
    }

    /// The guess the picker marks.
    #[wasm_bindgen(getter)]
    #[must_use]
    pub fn guess(&self) -> Option<String> {
        self.guess.clone()
    }
}

/// The source of a message's translation and the language its line names,
/// from its candidates (the detection's tag, absent for none, confidence and
/// alternatives), the conversation's languages, the language translated
/// into and the languages the engine answered it can translate from into it
/// (ask at least every candidate tag and the usual language).
#[wasm_bindgen(js_name = resolveTranslationSource)]
#[must_use]
pub fn resolve_translation_source(
    tag: Option<String>,
    confidence_permille: u16,
    alternatives: Vec<String>,
    languages: &ConversationLanguages,
    target: &str,
    translatable: Vec<String>,
) -> TranslationSourceResolution {
    let candidates = tag.map(|tag| rust_candidates(tag, confidence_permille, alternatives));
    let r = rust_resolve_translation_source(
        candidates.as_ref(),
        &languages.inner,
        target,
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

/// The `Not X?` list: `languages` with the candidate tags (detection first)
/// in front.
#[wasm_bindgen(js_name = orderPickerLanguages)]
#[must_use]
pub fn order_picker_languages(languages: Vec<String>, candidate_tags: Vec<String>) -> Vec<String> {
    let mut tags = candidate_tags.into_iter();
    let Some(first) = tags.next() else {
        return languages;
    };
    let candidates = rust_candidates(first, 0, tags.collect());
    rust_order_picker_languages(&languages, Some(&candidates))
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

    #[test]
    fn detection_crosses_the_surface() {
        let d = detect_text_language(
            "Ik ga morgen naar de markt om groenten te kopen. Wil je met me mee, of heb je het te druk?",
        );
        assert_eq!(
            d.as_ref().map(DetectedTextLanguage::tag).as_deref(),
            Some("nl")
        );
        assert!(d.is_some_and(|d| d.confidence_permille() >= DETECTION_CONFIDENT_PERMILLE));
        assert_eq!(detect_text_language("  !! 42"), None);
    }

    #[test]
    fn the_source_resolution_crosses_the_surface() {
        let c = detect_text_language_candidates("hola, qué tal?").unwrap_or_else(|| unreachable!());
        let r = resolve_translation_source(
            Some(c.tag()),
            c.confidence_permille(),
            c.alternatives(),
            &ConversationLanguages::default(),
            "en",
            vec!["es".into(), "fr".into()],
        );
        // An unsure guess is not named up front; Translate starts from es.
        assert_eq!(r.line_tag(), None);
        assert!(!r.line_can_translate());
        assert_eq!(r.translate_source().as_deref(), Some("es"));
    }

    #[test]
    fn the_conversation_language_crosses_the_surface() {
        // G55 Two: es and it once each, Spanish first among the candidates.
        let record = ConversationLanguages::new(
            vec![],
            vec![],
            vec![2.0, 5.0, 1.0, 4.0, 3.0],
            vec![
                "it".into(),
                "ru".into(),
                "es".into(),
                "de".into(),
                "uk".into(),
            ],
        );
        let chosen = conversation_detection(
            "ca".into(),
            180,
            vec!["es".into(), "fr".into(), "it".into()],
            &record,
        );
        assert_eq!(chosen.map(|d| d.tag()), Some("es".to_string()));
        // Stored and rebuilt, it is the same record.
        let again = ConversationLanguages::new(
            record.pick_tags(),
            record.pick_at_ms(),
            record.detection_ids(),
            record.detection_tags(),
        );
        assert_eq!(again, record);
        assert_eq!(record.with_detection(1.5, "it"), record);
    }

    #[test]
    fn candidates_cross_the_surface() {
        let c = detect_text_language_candidates("hola, qué tal?");
        assert!(c
            .as_ref()
            .is_some_and(|c| c.alternatives().iter().any(|a| a == "es")));
        let c = c.unwrap_or_else(|| unreachable!());
        let chosen = conversation_detection(
            c.tag(),
            c.confidence_permille(),
            c.alternatives(),
            &ConversationLanguages::default().with_pick("es", 1.0),
        );
        assert_eq!(chosen.map(|d| d.tag()), Some("es".to_string()));
        let h = language_candidates_from_hypotheses(
            "hola",
            vec!["ca".into(), "es".into()],
            vec![400, 350],
        );
        assert_eq!(h.map(|c| c.alternatives()), Some(vec!["es".to_string()]));
        assert_eq!(
            order_picker_languages(
                vec!["de".into(), "es".into()],
                vec!["ca".into(), "es".into()]
            ),
            vec!["es".to_string(), "de".to_string()]
        );
    }
}
