// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! When a client offers to translate a message or transcribe a voice message.
//!
//! ADR-0082 section 6, `docs/ux/translation-and-transcription.md` sections 3,
//! 4 and 7. The translating and transcribing are platform engines on the
//! device; what is here is only the decisions about them that the three
//! clients must make alike: whether a message gets a `Translate` row, whether a
//! conversation shows the "Messages here are in ..." bar, whether a voice
//! message gets a transcript button, whether a stored result still describes
//! the message, and how two language tags compare.
//!
//! Nothing here sees message text. A client runs its own language detector and
//! hands over the tag it produced and how sure it was; this module compares
//! tags. That keeps the rule free of any platform and keeps the text where it
//! is.

/// Detections below this confidence, in thousandths, are treated as guesses.
///
/// Chosen, not measured: the detectors the clients use (NaturalLanguage, ML Kit
/// Language Identification, the browser `LanguageDetector`) each report a
/// probability, and a short message ("ok", "lol") comes back from all of them
/// below this. Revisit with data from real conversations.
pub const DETECTION_CONFIDENT_PERMILLE: u16 = 700;

/// How many of the newest incoming messages the conversation bar looks at.
/// Chosen, not measured.
pub const BAR_WINDOW: usize = 20;

/// How many confident detections the bar needs inside its window before it
/// says anything. Below this a conversation is too young to have a language.
/// Chosen, not measured.
pub const BAR_MIN_CONFIDENT: usize = 5;

/// What the device can do for one language, as the platform engine reports it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EngineState {
    /// Works now, on this device.
    Ready,
    /// Works after a language pack is on the device.
    NeedsPack,
    /// This device cannot do it for this language, locally. There is no other
    /// way: the control is not offered (ADR-0082 decision).
    Unavailable,
}

impl EngineState {
    /// Whether a control may be offered at all.
    #[must_use]
    pub const fn offerable(self) -> bool {
        !matches!(self, Self::Unavailable)
    }
}

/// A language a detector found in some text, and how sure it was.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DetectedLanguage {
    /// The tag the detector produced (BCP 47, any case, `-` or `_`).
    pub tag: String,
    /// Confidence in thousandths, 0..=1000. Values above 1000 read as 1000.
    pub confidence_permille: u16,
}

impl DetectedLanguage {
    fn is_confident(&self) -> bool {
        self.confidence_permille >= DETECTION_CONFIDENT_PERMILLE
    }
}

/// What the message menu offers for one message.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TranslateOffer {
    /// No `Translate` row.
    None,
    /// A `Translate` row; the source language is the detected one.
    Translate,
    /// A `Translate` row whose translation opens with the language picker,
    /// because the device could not tell the language.
    TranslateAskLanguage,
}

/// A language tag reduced to what decides "is this the same language":
/// the primary language subtag and the script, explicit or implied.
#[derive(Debug, Clone, PartialEq, Eq)]
struct LanguageKey {
    language: String,
    script: Option<String>,
}

/// Deprecated primary subtags that platforms still emit.
fn replace_deprecated(language: &str) -> &str {
    match language {
        "iw" => "he",
        "in" => "id",
        "ji" => "yi",
        other => other,
    }
}

/// The script a language is written in when the tag does not say, for the
/// languages where the region or the default decides it. Only Chinese needs
/// this today: `zh-TW` is Traditional and `zh` alone is Simplified on every
/// platform the clients run on.
fn implied_script(language: &str, region: Option<&str>) -> Option<&'static str> {
    match language {
        "zh" => Some(match region {
            Some("TW" | "HK" | "MO") => "Hant",
            _ => "Hans",
        }),
        _ => None,
    }
}

fn language_key(tag: &str) -> Option<LanguageKey> {
    let mut parts = tag.trim().split(['-', '_']).filter(|p| !p.is_empty());
    let primary = parts.next()?.to_ascii_lowercase();
    if !(2..=3).contains(&primary.len()) || !primary.bytes().all(|b| b.is_ascii_alphabetic()) {
        return None;
    }
    // `und` is the detector saying it does not know; `mul` / `zxx` are not
    // one language. None of them is something to compare.
    if matches!(primary.as_str(), "und" | "mul" | "zxx" | "mis") {
        return None;
    }
    let language = replace_deprecated(&primary).to_string();
    let mut script = None;
    let mut region = None;
    for part in parts {
        if script.is_none()
            && region.is_none()
            && part.len() == 4
            && part.bytes().all(|b| b.is_ascii_alphabetic())
        {
            let mut s = part.to_ascii_lowercase();
            s[..1].make_ascii_uppercase();
            script = Some(s);
        } else if region.is_none()
            && ((part.len() == 2 && part.bytes().all(|b| b.is_ascii_alphabetic()))
                || (part.len() == 3 && part.bytes().all(|b| b.is_ascii_digit())))
        {
            region = Some(part.to_ascii_uppercase());
        } else {
            // Variants and extensions say nothing about mutual readability.
            break;
        }
    }
    let script =
        script.or_else(|| implied_script(&language, region.as_deref()).map(str::to_string));
    Some(LanguageKey { language, script })
}

impl LanguageKey {
    fn same_as(&self, other: &Self) -> bool {
        self.language == other.language
            && match (&self.script, &other.script) {
                (Some(a), Some(b)) => a == b,
                _ => true,
            }
    }

    fn canonical(&self) -> String {
        self.script.as_ref().map_or_else(
            || self.language.clone(),
            |s| format!("{}-{s}", self.language),
        )
    }
}

/// The form of a tag a client stores and compares: the primary language,
/// plus the script when the tag names one or the language implies one.
///
/// `pt-BR` is `pt`, `zh-TW` is `zh-Hant`, `zh` is `zh-Hans`, `iw` is `he`,
/// `sr-Latn-RS` is `sr-Latn`. `None` for a tag that names no language (`und`,
/// empty, malformed). Use it as the key of a stored choice ("dismissed for
/// Spanish", "chosen source language") so that two spellings of one language
/// are one entry.
#[must_use]
pub fn canonical_language_tag(tag: &str) -> Option<String> {
    language_key(tag).map(|k| k.canonical())
}

/// Whether two tags name a language a reader of one reads in the other.
///
/// Same primary language, and no conflicting script: `pt-BR` and `pt-PT` are
/// the same, `zh-Hans` and `zh-Hant` are not, `sr` and `sr-Cyrl` are. A tag
/// that names no language is the same as nothing.
#[must_use]
pub fn same_language(a: &str, b: &str) -> bool {
    match (language_key(a), language_key(b)) {
        (Some(a), Some(b)) => a.same_as(&b),
        _ => false,
    }
}

/// Whether a tag is one of the person's reading languages.
#[must_use]
pub fn reads_language(reading_languages: &[String], tag: &str) -> bool {
    language_key(tag).is_some_and(|key| {
        reading_languages
            .iter()
            .filter_map(|r| language_key(r))
            .any(|r| r.same_as(&key))
    })
}

/// Inputs to [`translation_offered`] for one message.
// Four independent facts about one message; an enum would merge them.
#[allow(clippy::struct_excessive_bools)]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TranslateOfferInput {
    /// Settings: `Show Translate button`.
    pub enabled: bool,
    /// The message has text (a caption, or a transcript that exists).
    pub has_text: bool,
    /// The person wrote it.
    pub is_own: bool,
    /// What the detector found, if it found anything.
    pub detected: Option<DetectedLanguage>,
    /// The engine's answer for translating from the detected language into the
    /// target. Ignored when nothing was detected.
    pub engine_for_detected: EngineState,
    /// Whether the device can translate from at least one language into the
    /// target, for the case where the source is unknown.
    pub any_engine: bool,
}

/// Whether the message menu offers `Translate` (UX section 3.1).
///
/// - Never for the person's own message, a message with no text, or with the
///   setting off.
/// - A confident detection decides: a reading language gets nothing, any other
///   language gets the row when the engine can do it, now or after a pack.
/// - A guess (below [`DETECTION_CONFIDENT_PERMILLE`]) that lands on a reading
///   language gets nothing either. Otherwise every short "ok" in the person's
///   own language would carry a `Translate` row; this refines the UX spec's
///   "could not tell" case to "could not tell, and the best guess is not
///   something you read".
/// - No usable detection gets the row with the language picker, when the device
///   can translate at all.
#[must_use]
pub fn translation_offered(
    input: &TranslateOfferInput,
    reading_languages: &[String],
) -> TranslateOffer {
    if !input.enabled || !input.has_text || input.is_own {
        return TranslateOffer::None;
    }
    let ask = if input.any_engine {
        TranslateOffer::TranslateAskLanguage
    } else {
        TranslateOffer::None
    };
    let Some(detected) = input.detected.as_ref() else {
        return ask;
    };
    if language_key(&detected.tag).is_none() {
        return ask;
    }
    if reads_language(reading_languages, &detected.tag) {
        return TranslateOffer::None;
    }
    if !detected.is_confident() {
        return ask;
    }
    if input.engine_for_detected.offerable() {
        TranslateOffer::Translate
    } else {
        TranslateOffer::None
    }
}

/// Inputs to [`conversation_bar_language`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConversationBarInput {
    /// Settings: `Show Translate button`.
    pub enabled: bool,
    /// Automatic translation is already on for this conversation.
    pub auto_translate_on: bool,
    /// Detections of the newest incoming messages, newest first. Only the
    /// first [`BAR_WINDOW`] are read; messages with no text are left out by the
    /// caller.
    pub recent_incoming: Vec<DetectedLanguage>,
    /// Languages whose bar the person dismissed in this conversation.
    pub dismissed: Vec<String>,
}

/// Which language the "Messages here are in ..." bar names, if any
/// (UX section 3.6).
///
/// Among the newest [`BAR_WINDOW`] incoming messages, only confident
/// detections count; with fewer than [`BAR_MIN_CONFIDENT`] of them, nothing.
/// The most common language among them must hold more than half of them, must
/// not be a reading language, and must not have been dismissed here. The
/// answer is in [`canonical_language_tag`] form.
///
/// The caller then asks its engine about the returned language and shows the
/// bar only when the answer is not [`EngineState::Unavailable`]; the engine
/// is per platform and cannot be asked from here.
#[must_use]
pub fn conversation_bar_language(
    input: &ConversationBarInput,
    reading_languages: &[String],
) -> Option<String> {
    if !input.enabled || input.auto_translate_on {
        return None;
    }
    let confident: Vec<LanguageKey> = input
        .recent_incoming
        .iter()
        .take(BAR_WINDOW)
        .filter(|d| d.is_confident())
        .filter_map(|d| language_key(&d.tag))
        .collect();
    if confident.len() < BAR_MIN_CONFIDENT {
        return None;
    }
    // A strict majority has at most one candidate; count each distinct key.
    let leader = confident.iter().find(|candidate| {
        confident.iter().filter(|k| k.same_as(candidate)).count() * 2 > confident.len()
    })?;
    let tag = leader.canonical();
    if reads_language(reading_languages, &tag) {
        return None;
    }
    if input.dismissed.iter().any(|d| same_language(d, &tag)) {
        return None;
    }
    Some(tag)
}

/// Whether a voice or video message gets the transcript button (UX 4.1).
///
/// Offered on own messages too. When this returns false the button's space
/// stays reserved and empty; that is the client's, not this rule's.
#[must_use]
pub const fn transcript_offered(enabled: bool, has_audio: bool, engine: EngineState) -> bool {
    enabled && has_audio && engine.offerable()
}

/// Whether a stored translation or transcript still describes the message.
///
/// A result is stored with the message's `last_op_id` (ADR-0023: the highest op
/// that touched it) and, for a translation, the target language. An edit
/// raises `last_op_id`, so the stored result is stale; a change of target
/// language makes a stored translation stale. Pass the same target twice for
/// a transcript.
#[must_use]
pub fn stored_result_is_current(
    stored_last_op_id: i64,
    current_last_op_id: i64,
    stored_target: &str,
    current_target: &str,
) -> bool {
    stored_last_op_id == current_last_op_id && same_language(stored_target, current_target)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn det(tag: &str, confidence_permille: u16) -> DetectedLanguage {
        DetectedLanguage {
            tag: tag.to_string(),
            confidence_permille,
        }
    }

    fn reading(tags: &[&str]) -> Vec<String> {
        tags.iter().map(|t| (*t).to_string()).collect()
    }

    fn offer_input(detected: Option<DetectedLanguage>) -> TranslateOfferInput {
        TranslateOfferInput {
            enabled: true,
            has_text: true,
            is_own: false,
            detected,
            engine_for_detected: EngineState::Ready,
            any_engine: true,
        }
    }

    #[test]
    fn canonical_tags_fold_spellings_of_one_language() {
        assert_eq!(canonical_language_tag("pt-BR").as_deref(), Some("pt"));
        assert_eq!(canonical_language_tag("pt_PT").as_deref(), Some("pt"));
        assert_eq!(canonical_language_tag("EN-us").as_deref(), Some("en"));
        assert_eq!(canonical_language_tag("zh").as_deref(), Some("zh-Hans"));
        assert_eq!(canonical_language_tag("zh-TW").as_deref(), Some("zh-Hant"));
        assert_eq!(
            canonical_language_tag("zh-Hant-CN").as_deref(),
            Some("zh-Hant")
        );
        assert_eq!(canonical_language_tag("iw").as_deref(), Some("he"));
        assert_eq!(
            canonical_language_tag("sr-latn-RS").as_deref(),
            Some("sr-Latn")
        );
        assert_eq!(canonical_language_tag("es-419").as_deref(), Some("es"));
        assert_eq!(canonical_language_tag("fil").as_deref(), Some("fil"));
    }

    #[test]
    fn tags_that_name_no_language_have_no_canonical_form() {
        for tag in ["", "und", "zxx", "mul", "e", "english", "12", "--", "en1"] {
            assert_eq!(canonical_language_tag(tag), None, "{tag}");
        }
    }

    #[test]
    fn scripts_decide_only_when_both_sides_name_one() {
        assert!(same_language("pt-BR", "pt-PT"));
        assert!(same_language("sr", "sr-Cyrl"));
        assert!(!same_language("sr-Latn", "sr-Cyrl"));
        assert!(!same_language("zh-Hans", "zh-Hant"));
        assert!(!same_language("zh", "zh-TW"));
        assert!(same_language("zh-CN", "zh-Hans"));
        assert!(same_language("he", "iw-IL"));
        assert!(!same_language("en", "es"));
        assert!(!same_language("und", "und"));
    }

    #[test]
    fn nothing_is_offered_for_own_empty_or_switched_off_messages() {
        let reading = reading(&["en"]);
        let base = offer_input(Some(det("es", 990)));
        assert_eq!(
            translation_offered(&base, &reading),
            TranslateOffer::Translate
        );
        for input in [
            TranslateOfferInput {
                is_own: true,
                ..base.clone()
            },
            TranslateOfferInput {
                has_text: false,
                ..base.clone()
            },
            TranslateOfferInput {
                enabled: false,
                ..base.clone()
            },
        ] {
            assert_eq!(translation_offered(&input, &reading), TranslateOffer::None);
        }
    }

    #[test]
    fn a_reading_language_is_never_offered() {
        let reading = reading(&["en", "pt-BR"]);
        for tag in ["en", "en-GB", "pt", "pt-PT"] {
            let input = offer_input(Some(det(tag, 990)));
            assert_eq!(
                translation_offered(&input, &reading),
                TranslateOffer::None,
                "{tag}"
            );
        }
        // A guess that lands on a reading language is not offered either.
        let input = offer_input(Some(det("en", 300)));
        assert_eq!(translation_offered(&input, &reading), TranslateOffer::None);
    }

    #[test]
    fn an_engine_that_cannot_hides_the_row() {
        let reading = reading(&["en"]);
        let mut input = offer_input(Some(det("es", 990)));
        input.engine_for_detected = EngineState::NeedsPack;
        assert_eq!(
            translation_offered(&input, &reading),
            TranslateOffer::Translate
        );
        input.engine_for_detected = EngineState::Unavailable;
        assert_eq!(translation_offered(&input, &reading), TranslateOffer::None);
    }

    #[test]
    fn an_unknown_language_asks_when_the_device_translates_at_all() {
        let reading = reading(&["en"]);
        for detected in [None, Some(det("und", 900)), Some(det("es", 200))] {
            let mut input = offer_input(detected);
            assert_eq!(
                translation_offered(&input, &reading),
                TranslateOffer::TranslateAskLanguage
            );
            input.any_engine = false;
            assert_eq!(translation_offered(&input, &reading), TranslateOffer::None);
        }
    }

    fn bar(recent: Vec<DetectedLanguage>) -> ConversationBarInput {
        ConversationBarInput {
            enabled: true,
            auto_translate_on: false,
            recent_incoming: recent,
            dismissed: Vec::new(),
        }
    }

    #[test]
    fn the_bar_names_a_clear_majority_language() {
        let reading = reading(&["en"]);
        let mut recent = vec![det("es", 950); 6];
        recent.extend(vec![det("en", 950); 3]);
        assert_eq!(
            conversation_bar_language(&bar(recent), &reading).as_deref(),
            Some("es")
        );
    }

    #[test]
    fn the_bar_needs_enough_confident_messages() {
        let reading = reading(&["en"]);
        let few = vec![det("es", 950); BAR_MIN_CONFIDENT - 1];
        assert_eq!(conversation_bar_language(&bar(few), &reading), None);
        let unsure = vec![det("es", 500); BAR_WINDOW];
        assert_eq!(conversation_bar_language(&bar(unsure), &reading), None);
    }

    #[test]
    fn the_bar_looks_only_at_the_newest_window() {
        let reading = reading(&["en"]);
        let mut recent = vec![det("en", 950); BAR_WINDOW];
        recent.extend(vec![det("es", 950); 40]);
        assert_eq!(conversation_bar_language(&bar(recent), &reading), None);
    }

    #[test]
    fn the_bar_needs_a_strict_majority() {
        let reading = reading(&["en"]);
        let mut recent = vec![det("es", 950); 3];
        recent.extend(vec![det("de", 950); 3]);
        assert_eq!(conversation_bar_language(&bar(recent), &reading), None);
    }

    #[test]
    fn the_bar_stays_away_for_reading_dismissed_or_auto_translated() {
        let recent = vec![det("es-MX", 950); 8];
        assert_eq!(
            conversation_bar_language(&bar(recent.clone()), &reading(&["es"])),
            None
        );
        let mut dismissed = bar(recent.clone());
        dismissed.dismissed = vec!["es".to_string()];
        assert_eq!(
            conversation_bar_language(&dismissed, &reading(&["en"])),
            None
        );
        let mut auto = bar(recent.clone());
        auto.auto_translate_on = true;
        assert_eq!(conversation_bar_language(&auto, &reading(&["en"])), None);
        let mut off = bar(recent);
        off.enabled = false;
        assert_eq!(conversation_bar_language(&off, &reading(&["en"])), None);
    }

    #[test]
    fn the_bar_counts_spellings_of_one_language_together() {
        let reading = reading(&["en"]);
        let recent = vec![
            det("pt-BR", 950),
            det("pt-PT", 950),
            det("pt", 950),
            det("pt_br", 950),
            det("de", 950),
        ];
        assert_eq!(
            conversation_bar_language(&bar(recent), &reading).as_deref(),
            Some("pt")
        );
    }

    #[test]
    fn transcript_button_follows_the_engine() {
        assert!(transcript_offered(true, true, EngineState::Ready));
        assert!(transcript_offered(true, true, EngineState::NeedsPack));
        assert!(!transcript_offered(true, true, EngineState::Unavailable));
        assert!(!transcript_offered(false, true, EngineState::Ready));
        assert!(!transcript_offered(true, false, EngineState::Ready));
    }

    #[test]
    fn an_edit_or_a_new_target_makes_a_stored_result_stale() {
        assert!(stored_result_is_current(7, 7, "en", "en-US"));
        assert!(!stored_result_is_current(7, 9, "en", "en"));
        assert!(!stored_result_is_current(7, 7, "en", "de"));
    }

    mod properties {
        use super::super::*;
        use proptest::prelude::*;

        proptest! {
            #[test]
            fn tag_handling_never_panics(tag in ".{0,40}", other in ".{0,40}") {
                let _ = canonical_language_tag(&tag);
                let _ = same_language(&tag, &other);
                let _ = reads_language(std::slice::from_ref(&other), &tag);
            }

            #[test]
            fn same_language_is_symmetric(a in "[a-zA-Z]{2,3}(-[a-zA-Z]{4})?(-[A-Z]{2})?",
                                          b in "[a-zA-Z]{2,3}(-[a-zA-Z]{4})?(-[A-Z]{2})?") {
                prop_assert_eq!(same_language(&a, &b), same_language(&b, &a));
            }

            #[test]
            fn a_canonical_tag_is_a_fixed_point(tag in "[a-zA-Z]{2,3}(-[a-zA-Z]{4})?(-[A-Za-z]{2})?") {
                if let Some(c) = canonical_language_tag(&tag) {
                    prop_assert_eq!(canonical_language_tag(&c), Some(c.clone()));
                    prop_assert!(same_language(&tag, &c));
                }
            }

            #[test]
            fn the_bar_is_independent_of_order_inside_the_window(
                langs in proptest::collection::vec(prop_oneof!["es", "de", "en", "pt-BR"], 0..BAR_WINDOW),
                seed in any::<u64>(),
            ) {
                let reading = vec!["en".to_string()];
                let recent: Vec<DetectedLanguage> = langs
                    .iter()
                    .map(|t| DetectedLanguage { tag: t.clone(), confidence_permille: 900 })
                    .collect();
                let mut shuffled = recent.clone();
                // A deterministic rotation is enough to change the order.
                let len = shuffled.len().max(1);
                let by = usize::try_from(seed % (len as u64)).unwrap_or(0);
                shuffled.rotate_left(by);
                let a = conversation_bar_language(
                    &ConversationBarInput { enabled: true, auto_translate_on: false, recent_incoming: recent, dismissed: vec![] },
                    &reading,
                );
                let b = conversation_bar_language(
                    &ConversationBarInput { enabled: true, auto_translate_on: false, recent_incoming: shuffled, dismissed: vec![] },
                    &reading,
                );
                prop_assert_eq!(a, b);
            }
        }
    }
}
