// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! Which language a message's text is in, found on the device.
//!
//! ADR-0082, note of 2026-11-15. The translation rules of
//! [`crate::translation`] take a detector's tag and confidence; a client with
//! a usable platform detector (iOS `NLLanguageRecognizer`) keeps using it.
//! Android has no detector Lakko may use (ML Kit Language ID reports the
//! languages it identifies, `TextClassifier` may run in another vendor's
//! process), and the browser's `LanguageDetector` is used only when its model
//! is already on the device, which in practice it rarely is. Those clients call
//! [`detect_text_language`] instead.
//!
//! The detector is the `whatlang` crate: trigram profiles compiled into the
//! binary, 70 languages, no network and no file access. The text never leaves
//! the function; nothing is stored or logged.
//!
//! # What the confidence means
//!
//! The answer is a [`DetectedLanguage`] whose `confidence_permille` is read by
//! the same [`DETECTION_CONFIDENT_PERMILLE`] threshold every other detector's
//! confidence is read by: at or above it the clients translate from the
//! detected language, below it they open the language picker. `whatlang`'s own
//! confidence is not a probability (a correct one-sentence message often scores
//! 0.1 to 0.4), so it is mapped onto that scale at
//! [`CONFIDENT_FROM_PERMILLE`], set from two measurements:
//!
//! - **Chat corpus**, the one that decides: 534 ordinary short messages
//!   (greetings, questions, one or two sentences, 3 to 20 words), 28 each in
//!   nl de en es pt fr it ru uk pl tr ar fa he hi and 12 to 18 each in the
//!   close neighbours af da nb sv bg ca cs. Written by the agent that built
//!   this module, not collected from people, so it is small and has one
//!   author's phrasing; the per-language numbers carry that.
//! - **UI corpus**: the interface strings of `l10n/catalog/` in 11 languages,
//!   26 183 strings (machine translations of short UI text). "Wrong" there
//!   includes English left untranslated in another catalog.
//!
//! `whatlang` picks the wrong language for many short messages whatever its
//! confidence (Italian 18 of 28 right, Spanish 19, English 21, Russian 20), so
//! no cut-off makes most one-sentence messages both confident and right.
//! With every rule below, confident answers and wrong confident answers:
//!
//! | cut-off | chat, 15 major languages | chat, all 534 | UI corpus |
//! |---|---|---|---|
//! | 0.5 | 184 of 420 (44%), 0 wrong | 217, 0 wrong | 8 843, 6 wrong |
//! | 0.3 | 241 of 420 (57%), 2 wrong | 282, 3 wrong | 11 184, 44 wrong |
//! | 0.2 | 279 of 420 (66%), 5 wrong | 330, 8 wrong | 12 675, 184 wrong |
//! | 0.15 | 302 of 420 (72%), 13 wrong | 355, 17 wrong | 13 452, 335 wrong |
//!
//! At 0.3, per language on the chat corpus: pl 26 of 28, uk 22, de 23, he
//! 22, fa 18, fr 18, tr 18, ar 20, hi 14, en 13, ru 11, es 10, pt 9, it 9,
//! nl 8 (2 wrong), nb 2 of 18 (1 wrong). The cut-off is 0.3; moving it is a
//! product choice between the columns above. A wrong confident answer costs a
//! wrong-source translation the person corrects with `Not Spanish?` (UX 3.4);
//! an unsure one costs a picker. Short text ("ok", "lol", "hola") comes out
//! unsure, as it does from every platform detector.
//!
//! # When the answer is unsure although `whatlang` is sure
//!
//! - Fewer than [`MIN_LETTERS`] letters in a script that several languages
//!   share (Latin, Cyrillic, Arabic, Devanagari, Hebrew). A script that only
//!   one language uses (Hangul, Greek, Thai, Georgian, ...) and Chinese and
//!   Japanese characters are exempt: the script is the evidence. This rule
//!   also holds a platform detector's answer
//!   ([`candidates_from_hypotheses`]) since 2026-11-20, and with it most
//!   one-sentence messages are unsure: the corpus figures below were taken
//!   with a minimum of 8 letters.
//! - Less than [`MIN_SCRIPT_SHARE_PERMILLE`] of the letters are in the script
//!   of the detected language: "Lakko 仍在运行" is not Javanese. On the UI
//!   corpus this rule removed 11 of the 20 wrong confident answers at 0.6.
//! - Chinese: the trigram model cannot tell Simplified from Traditional, and
//!   the tags the clients compare imply Simplified for a bare `zh`
//!   ([`crate::translation::canonical_language_tag`]). The tag is `zh` and the
//!   answer is never confident, so a Chinese reader gets no `Translate` row and
//!   anyone else gets the picker.
//! - The letters contradict the answer. Each rule is a fact about an alphabet
//!   and was added for a confusion one of the corpora showed: Portuguese
//!   without `ã õ ç` (Spanish read as Portuguese), Catalan without
//!   `à è ò ï ç` (Spanish), Bulgarian without `ъ` or with `ы э ё`, Ukrainian
//!   without `і ї є ґ` or with `ы э ё ъ`, Belarusian without `ў`, Serbian
//!   without `ђ ћ џ љ њ ј`, Macedonian without `ѓ ќ ѕ љ њ џ ј` (Russian and
//!   Ukrainian read as these), Azerbaijani without `ə` and any language but
//!   Turkish and Azerbaijani with `ı` (Turkish read as Azerbaijani or
//!   Indonesian), Romanian without `ă â î ș ț` (English read as Romanian),
//!   Spanish with `ã õ ç`, Russian with `і ї є ґ ў`. They only ever lower an
//!   answer, so a message in one of these languages typed without its
//!   distinctive letters gets the picker.
//!
//! # Close candidates
//!
//! A short message is often a near tie between neighbours (measured with
//! `whatlang` =0.18.0, combined scores: "hola, qué tal?" Catalan 0.737,
//! Spanish 0.729, French 0.715, Italian 0.710; "Ciao, come stai?" Spanish
//! 0.739, Portuguese 0.736, Italian 0.735; "goedendag" Turkmen 0.748, Danish
//! 0.739, Dutch 0.737; against confident cases "¿Qué tal? ¿Cómo estás?"
//! Spanish 0.688 vs Portuguese 0.540 and "Привіт, як справи?" Ukrainian 0.777
//! vs Bulgarian 0.630). [`detect_text_language_candidates`] therefore returns
//! the detection plus up to [`MAX_ALTERNATIVES`] other languages whose score is
//! within [`CANDIDATE_SCORE_MARGIN_PERMILLE`] of the top one, nearest first,
//! leaving out any whose letters contradict it (the rules above). The margin
//! is chosen, not measured: those five cases are the only data. A confident
//! detection has no alternatives. A platform detector that reports
//! probabilities (NaturalLanguage, the browser `LanguageDetector`) goes
//! through [`candidates_from_hypotheses`] instead, with
//! [`CANDIDATE_PROBABILITY_RATIO_PERMILLE`], also chosen.
//!
//! The candidate set is what lets a conversation's language decide an unsure
//! message ([`conversation_detection`]) and what the `Not X?` list shows first
//! ([`order_picker_languages`]). The ranked list comes from
//! `whatlang::dev::raw_detect`, which upstream calls internal; the version is
//! pinned exactly and the tests below pin the measured cases.

use whatlang::{Lang, Script};

use crate::translation::{
    canonical_language_tag, same_language, DetectedLanguage, DETECTION_CONFIDENT_PERMILLE,
};

/// At most this many characters of the text are read. Chosen, not measured:
/// a message longer than this is decided by its start, and the work per
/// detection stays bounded.
pub const MAX_DETECT_CHARS: usize = 2048;

/// Letters a shared-script text needs before an answer can be confident,
/// from any detector (this module's and a platform's through
/// [`candidates_from_hypotheses`]). Chosen, not measured. It was 8 until
/// 2026-11-20 and only guarded this module's detector; on test.lakko.me that
/// day the English messages "W7 web to iphone" (13 letters) and "W6 iphone
/// while behind" (18) were named Polish and Afrikaans up front, by the iOS
/// detector and by this one. A shorter message is still translated from a
/// guess when the person asks, it is just never named before (UX section
/// 3.1).
pub const MIN_LETTERS: usize = 20;

/// The share of letters, in thousandths, that must be in the detected
/// language's script for a confident answer. Measured on the UI corpus (see
/// the module doc).
pub const MIN_SCRIPT_SHARE_PERMILLE: usize = 800;

/// `whatlang` confidence, in thousandths, at and above which an answer is
/// confident; the trade-off at other values is in the module doc. It is
/// mapped onto [`DETECTION_CONFIDENT_PERMILLE`].
pub const CONFIDENT_FROM_PERMILLE: u16 = 300;

/// The language a text is in, if this device can tell, with a confidence the
/// rules of [`crate::translation`] read.
///
/// `None` for text with no letters (empty, white space, digits, punctuation,
/// emoji) and for a language with no tag here. The tag is the BCP 47 form the
/// platform translators take: the ISO 639-1 code (`nl`, `ru`, `en`, `pt`,
/// `fa`, `nb`, `zh`, ...), never a region.
#[must_use]
pub fn detect_text_language(text: &str) -> Option<DetectedLanguage> {
    // Only letters reach the detector: punctuation, digits and symbols become
    // spaces. Measured: a `·` alone was read as Catalan with confidence 1.0
    // and a full-width comma as Korean.
    let letters_only = letters_of(text);
    let letters = letters_only.chars().filter(|c| c.is_alphabetic()).count();
    if letters == 0 {
        return None;
    }
    let info = whatlang::detect(&letters_only)?;
    let lang = info.lang();
    let tag = tag_of(lang);
    let raw = raw_permille(info.confidence());

    let script = info.script();
    let in_script = letters_only
        .chars()
        .filter(|&c| c.is_alphabetic() && counts_for(script, lang, c))
        .count();
    let share_ok = in_script * 1000 >= letters * MIN_SCRIPT_SHARE_PERMILLE;
    let length_ok = letters >= MIN_LETTERS || script_names_the_language(script);
    let confident = raw >= CONFIDENT_FROM_PERMILLE
        && share_ok
        && length_ok
        && lang != Lang::Cmn
        && letters_agree(lang, &letters_only);

    Some(DetectedLanguage {
        tag: tag.to_string(),
        confidence_permille: scaled(raw, confident),
    })
}

/// A tag in the form a detector names the language: `no` -> `nb`,
/// `fil` -> `tl`, everything else canonical (or as given when it names none).
fn detector_tag(tag: &str) -> String {
    let Some(canonical) = canonical_language_tag(tag) else {
        return tag.to_string();
    };
    match canonical.split('-').next().unwrap_or_default() {
        "no" => "nb".to_string(),
        "fil" => "tl".to_string(),
        _ => canonical,
    }
}

/// How close, in thousandths of `whatlang`'s combined score, another
/// language must be to the top one to be a candidate. Chosen, not measured
/// (see the module doc): it takes in every near tie of the measured short
/// messages (differences of 0.004 to 0.027) and none of the confident
/// runner-ups (0.147, 0.148).
pub const CANDIDATE_SCORE_MARGIN_PERMILLE: u16 = 30;

/// For a detector that reports probabilities: another language is a
/// candidate when its probability is at least this share, in thousandths, of
/// the top one. Chosen, not measured.
pub const CANDIDATE_PROBABILITY_RATIO_PERMILLE: u16 = 500;

/// At most this many languages besides the detected one.
pub const MAX_ALTERNATIVES: usize = 3;

/// A detection and the other languages it was a near tie with.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LanguageCandidates {
    /// The detection, exactly as [`detect_text_language`] or the platform
    /// detector answered.
    pub detected: DetectedLanguage,
    /// Up to [`MAX_ALTERNATIVES`] other languages, nearest first, in the
    /// canonical tag form. Empty for a confident detection.
    pub alternatives: Vec<String>,
}

impl LanguageCandidates {
    /// The detected language followed by the alternatives.
    #[must_use]
    pub fn tags(&self) -> Vec<String> {
        let mut out = Vec::with_capacity(1 + self.alternatives.len());
        out.push(self.detected.tag.clone());
        out.extend(self.alternatives.iter().cloned());
        out
    }

    /// Whether `tag` names the detected language or one of the alternatives.
    #[must_use]
    pub fn contains(&self, tag: &str) -> bool {
        self.tags().iter().any(|t| same_language(t, tag))
    }

    fn is_confident(&self) -> bool {
        self.detected.confidence_permille >= DETECTION_CONFIDENT_PERMILLE
    }
}

/// [`detect_text_language`] with the languages it was a near tie with.
#[must_use]
pub fn detect_text_language_candidates(text: &str) -> Option<LanguageCandidates> {
    let detected = detect_text_language(text)?;
    if detected.confidence_permille >= DETECTION_CONFIDENT_PERMILLE {
        return Some(LanguageCandidates {
            detected,
            alternatives: Vec::new(),
        });
    }
    let letters_only = letters_of(text);
    let ranked = ranked_scores(&letters_only);
    let top = ranked.first().map_or(0.0, |&(_, score)| score);
    let margin = f64::from(CANDIDATE_SCORE_MARGIN_PERMILLE) / 1000.0;
    let mut alternatives: Vec<String> = Vec::new();
    for &(lang, score) in &ranked {
        if alternatives.len() == MAX_ALTERNATIVES {
            break;
        }
        if score < top - margin {
            break;
        }
        let tag = tag_of(lang);
        if same_language(tag, &detected.tag)
            || lang == Lang::Cmn
            || !letters_agree(lang, &letters_only)
            || alternatives.iter().any(|a| same_language(a, tag))
        {
            continue;
        }
        if let Some(canonical) = canonical_language_tag(tag) {
            alternatives.push(canonical);
        }
    }
    Some(LanguageCandidates {
        detected,
        alternatives,
    })
}

/// The same set from a platform detector's hypotheses: tags with
/// probabilities in thousandths, in any order. The top hypothesis is the
/// detection, with its probability as the confidence; the others whose
/// probability is at least [`CANDIDATE_PROBABILITY_RATIO_PERMILLE`] of the
/// top one, and whose letters do not contradict them, are the alternatives.
/// `None` when no hypothesis names a language.
#[must_use]
pub fn candidates_from_hypotheses(
    text: &str,
    hypotheses: &[(String, u16)],
) -> Option<LanguageCandidates> {
    let mut merged: Vec<(String, u16)> = Vec::new();
    for (tag, permille) in hypotheses {
        let Some(canonical) = canonical_language_tag(tag) else {
            continue;
        };
        let permille = (*permille).min(1000);
        match merged
            .iter_mut()
            .find(|(t, _)| same_language(t, &canonical))
        {
            Some(entry) => entry.1 = entry.1.max(permille),
            None => merged.push((canonical, permille)),
        }
    }
    // Stable: equal probabilities keep the platform's order.
    merged.sort_by_key(|entry| std::cmp::Reverse(entry.1));
    let (top_tag, top_permille) = merged.first()?.clone();
    let letters_only = letters_of(text);
    // A platform detector is held to the same length rule as this module's:
    // too few letters in a shared script and the answer is a guess, however
    // probable the platform says it is.
    let top_permille = if long_enough(&letters_only) {
        top_permille
    } else {
        top_permille.min(DETECTION_CONFIDENT_PERMILLE - 1)
    };
    let mut candidates = LanguageCandidates {
        detected: DetectedLanguage {
            tag: top_tag,
            confidence_permille: top_permille,
        },
        alternatives: Vec::new(),
    };
    if candidates.is_confident() {
        return Some(candidates);
    }
    let floor = u32::from(top_permille) * u32::from(CANDIDATE_PROBABILITY_RATIO_PERMILLE) / 1000;
    for (tag, permille) in merged.iter().skip(1) {
        if candidates.alternatives.len() == MAX_ALTERNATIVES || u32::from(*permille) < floor {
            break;
        }
        let contradicted = candidate_langs(std::slice::from_ref(tag))
            .first()
            .is_some_and(|&lang| !letters_agree(lang, &letters_only));
        if !contradicted {
            candidates.alternatives.push(tag.clone());
        }
    }
    Some(candidates)
}

/// At most this many messages' confident detections are kept per
/// conversation: those with the highest message ids. Chosen, not measured.
pub const MAX_CONVERSATION_DETECTIONS: usize = 200;

/// At most this many picked languages are kept per conversation: the most
/// recently picked. Chosen, not measured.
pub const MAX_CONVERSATION_PICKS: usize = 16;

/// A language the person picked for a conversation (`Not X?` or the
/// picker), and when.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LanguagePick {
    /// The language, canonical.
    pub tag: String,
    /// When it was last picked, ms since the epoch.
    pub at_ms: u64,
}

/// The language of one message, confidently detected.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MessageLanguage {
    /// The message's id in its conversation.
    pub message_id: i64,
    /// The language, canonical.
    pub tag: String,
}

/// What this device knows of a conversation's languages (ADR-0082 note of
/// 2026-11-15): the languages the person picked, the latest time per
/// language, and the confidently detected language of each message, once
/// per message id. Kept on the device only.
///
/// Its form is canonical (picks sorted by tag, detections by message id, both
/// bounded by [`MAX_CONVERSATION_PICKS`] and
/// [`MAX_CONVERSATION_DETECTIONS`]), so the same picks and detections give
/// the same record, and the same answers, in whatever order they arrived.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ConversationLanguages {
    /// One per language, sorted by tag.
    pub picks: Vec<LanguagePick>,
    /// One per message id, sorted by message id.
    pub detections: Vec<MessageLanguage>,
}

fn canonical_or_given(tag: &str) -> String {
    canonical_language_tag(tag).unwrap_or_else(|| tag.to_string())
}

impl ConversationLanguages {
    /// The canonical form of any record (a stored one included): tags
    /// canonical, one pick per language at its latest time, one detection per
    /// message id, sorted and bounded. A message id detected as two languages
    /// keeps the one sorting first, so even that is order-independent.
    #[must_use]
    pub fn normalized(&self) -> Self {
        let mut picks: Vec<LanguagePick> = Vec::new();
        for p in &self.picks {
            let tag = canonical_or_given(&p.tag);
            match picks.iter_mut().find(|q| same_language(&q.tag, &tag)) {
                Some(q) => q.at_ms = q.at_ms.max(p.at_ms),
                None => picks.push(LanguagePick {
                    tag,
                    at_ms: p.at_ms,
                }),
            }
        }
        // The most recent picks survive the bound; a tie goes by tag.
        picks.sort_by(|a, b| b.at_ms.cmp(&a.at_ms).then_with(|| a.tag.cmp(&b.tag)));
        picks.truncate(MAX_CONVERSATION_PICKS);
        picks.sort_by(|a, b| a.tag.cmp(&b.tag));

        let mut detections: Vec<MessageLanguage> = self
            .detections
            .iter()
            .map(|d| MessageLanguage {
                message_id: d.message_id,
                tag: canonical_or_given(&d.tag),
            })
            .collect();
        detections.sort_by(|a, b| {
            b.message_id
                .cmp(&a.message_id)
                .then_with(|| a.tag.cmp(&b.tag))
        });
        detections.dedup_by_key(|d| d.message_id);
        detections.truncate(MAX_CONVERSATION_DETECTIONS);
        detections.reverse();
        Self { picks, detections }
    }

    /// The record with a pick of `tag` at `at_ms` (a `Not X?` correction or
    /// a picker pick).
    #[must_use]
    pub fn with_pick(&self, tag: &str, at_ms: u64) -> Self {
        let mut next = self.clone();
        next.picks.push(LanguagePick {
            tag: tag.to_string(),
            at_ms,
        });
        next.normalized()
    }

    /// The record with message `message_id` confidently detected as `tag`,
    /// replacing what that message was recorded as before (an edit).
    #[must_use]
    pub fn with_detection(&self, message_id: i64, tag: &str) -> Self {
        let mut next = self.clone();
        next.detections.retain(|d| d.message_id != message_id);
        next.detections.push(MessageLanguage {
            message_id,
            tag: tag.to_string(),
        });
        next.normalized()
    }

    fn detections_of(&self, tag: &str) -> usize {
        self.detections
            .iter()
            .filter(|d| same_language(&d.tag, tag))
            .count()
    }

    /// Among `candidates` (in the detector's order): a picked one, the most
    /// recently picked first; else the one with the most detections in the
    /// conversation, a tie going in candidate order; `None` when none was
    /// picked or detected.
    #[must_use]
    pub fn preferred_among(&self, candidates: &[String]) -> Option<String> {
        let record = self.normalized();
        let mut best_pick: Option<(&String, u64)> = None;
        for c in candidates {
            if let Some(p) = record.picks.iter().find(|p| same_language(&p.tag, c)) {
                if best_pick.map_or(true, |(_, at)| p.at_ms > at) {
                    best_pick = Some((c, p.at_ms));
                }
            }
        }
        if let Some((c, _)) = best_pick {
            return Some(canonical_or_given(c));
        }
        let mut best: Option<(&String, usize)> = None;
        for c in candidates {
            let n = record.detections_of(c);
            if n > 0 && best.map_or(true, |(_, m)| n > m) {
                best = Some((c, n));
            }
        }
        best.map(|(c, _)| canonical_or_given(c))
    }

    /// The conversation's language whatever the message: the most recently
    /// picked one, else the most detected one, a tie going by tag. `None`
    /// for a conversation with neither.
    #[must_use]
    pub fn usual(&self) -> Option<String> {
        let record = self.normalized();
        if let Some(p) = record
            .picks
            .iter()
            .max_by(|a, b| a.at_ms.cmp(&b.at_ms).then_with(|| b.tag.cmp(&a.tag)))
        {
            return Some(p.tag.clone());
        }
        let mut best: Option<(&str, usize)> = None;
        for d in &record.detections {
            let n = record.detections_of(&d.tag);
            let better = match best {
                None => true,
                Some((t, m)) => n > m || (n == m && d.tag.as_str() < t),
            };
            if better {
                best = Some((&d.tag, n));
            }
        }
        best.map(|(t, _)| t.to_string())
    }
}

/// The language a message is taken to be in, given what this device knows of
/// its conversation's languages ([`ConversationLanguages`]).
///
/// A confident detection stands. An unsure one yields to
/// [`ConversationLanguages::preferred_among`] its candidates (a picked
/// language, else the most detected one), keeping its own (unsure)
/// confidence; otherwise the detection stands, so a conversation's language
/// never overrides a guess it was not a near tie with (Dutch in a chat never
/// names an unsure Ukrainian message).
#[must_use]
pub fn conversation_detection(
    candidates: Option<&LanguageCandidates>,
    languages: &ConversationLanguages,
) -> Option<DetectedLanguage> {
    let candidates = candidates?;
    if candidates.is_confident() {
        return Some(candidates.detected.clone());
    }
    match languages.preferred_among(&candidates.tags()) {
        Some(tag) => Some(DetectedLanguage {
            tag,
            confidence_permille: candidates.detected.confidence_permille,
        }),
        None => Some(candidates.detected.clone()),
    }
}

/// The `Not X?` list: `languages` (the picker's list, as the client built it)
/// with the close candidates of the message first, in candidate order, and
/// the rest in their own order. Nothing is added or removed.
#[must_use]
pub fn order_picker_languages(
    languages: &[String],
    candidates: Option<&LanguageCandidates>,
) -> Vec<String> {
    let Some(candidates) = candidates else {
        return languages.to_vec();
    };
    let mut first: Vec<String> = Vec::new();
    for tag in candidates.tags() {
        if let Some(l) = languages
            .iter()
            .find(|l| same_language(l, &tag) && !first.contains(l))
        {
            first.push(l.clone());
        }
    }
    let rest: Vec<String> = languages
        .iter()
        .filter(|l| !first.contains(l))
        .cloned()
        .collect();
    first.extend(rest);
    first
}

/// What the line under a message names and what `Translate` translates
/// from, decided once for both so they cannot disagree (ADR-0082 note of
/// 2026-11-15).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SourceResolution {
    /// The language the line up front names, `None` for no line.
    pub line_tag: Option<String>,
    /// The confidence of the detection the line stands on, for the reading
    /// language rule the client applies before showing it.
    pub line_confidence_permille: u16,
    /// Whether the line carries `Translate`; `false` is a name alone.
    pub line_can_translate: bool,
    /// The language `Translate` starts from, `None` for the picker.
    pub translate_source: Option<String>,
    /// The guess the picker marks when it opens.
    pub guess: Option<String>,
}

/// The source language of a message, in the order `Translate` uses:
///
/// 1. a confident detection, the only one the line names (named alone when
///    the device cannot translate from it);
/// 2. the conversation's language among the candidates of an unsure
///    detection ([`conversation_detection`]);
/// 3. that guess, when the device can translate from it into `target`;
/// 4. the first other close candidate the device can translate from;
/// 5. for `Translate` only, the conversation's usual language whatever the
///    candidates ([`ConversationLanguages::usual`]; no line names it: it was
///    no near tie of this message);
/// 6. otherwise the picker, with the guess marked.
///
/// `translatable` lists the languages the client's engine answered it can
/// translate from into `target` (ready or with a pack to download); a client
/// asks at least for every tag of `candidates` and the usual language.
/// A language that is `target` itself is never a source to translate from.
/// The line names only a confident detection (since 2026-11-20): an unsure
/// one, or a text shorter than [`MIN_LETTERS`] in a shared script (which no
/// detector answers confidently here), shows no line, and `Translate` in the
/// menu still starts from steps 2 to 6.
#[must_use]
pub fn resolve_translation_source(
    candidates: Option<&LanguageCandidates>,
    languages: &ConversationLanguages,
    target: &str,
    translatable: &[String],
) -> SourceResolution {
    let usable = |tag: &str| {
        !same_language(tag, target) && translatable.iter().any(|t| same_language(t, tag))
    };
    let remembered_source = languages.usual();
    let Some(effective) = conversation_detection(candidates, languages) else {
        return SourceResolution {
            line_tag: None,
            line_confidence_permille: 0,
            line_can_translate: false,
            translate_source: remembered_source,
            guess: None,
        };
    };
    let confidence = effective.confidence_permille;
    if confidence >= DETECTION_CONFIDENT_PERMILLE {
        return SourceResolution {
            line_can_translate: usable(&effective.tag),
            line_tag: Some(effective.tag.clone()),
            line_confidence_permille: confidence,
            translate_source: Some(effective.tag),
            guess: None,
        };
    }
    let mut order = vec![effective.tag.clone()];
    if let Some(candidates) = candidates {
        for tag in candidates.tags() {
            if !order.iter().any(|t| same_language(t, &tag)) {
                order.push(tag);
            }
        }
    }
    // An unsure detection is never named up front (maintainer, 2026-11-20:
    // English messages were labelled Polish and Italian). `Translate` in the
    // menu still starts from the best usable candidate.
    if let Some(source) = order.into_iter().find(|tag| usable(tag)) {
        return SourceResolution {
            line_tag: None,
            line_confidence_permille: confidence,
            line_can_translate: false,
            translate_source: Some(source),
            guess: None,
        };
    }
    SourceResolution {
        line_tag: None,
        line_confidence_permille: confidence,
        line_can_translate: false,
        translate_source: remembered_source,
        guess: Some(effective.tag),
    }
}

/// Only the letters of `text` (others become spaces), at most
/// [`MAX_DETECT_CHARS`] characters.
fn letters_of(text: &str) -> String {
    text.chars()
        .take(MAX_DETECT_CHARS)
        .map(|c| if c.is_alphabetic() { c } else { ' ' })
        .collect()
}

/// `whatlang`'s ranked scores, best first, for text in a script several
/// languages share; empty otherwise. Through `whatlang::dev`, which upstream
/// calls internal (see the module doc and the Cargo.toml comment).
fn ranked_scores(letters_only: &str) -> Vec<(Lang, f64)> {
    match whatlang::dev::raw_detect(letters_only).lang_info {
        Some(whatlang::dev::RawLangInfo::MultiScript(info)) => {
            let mut scores = info.scores;
            scores.sort_by(|a, b| b.1.total_cmp(&a.1));
            scores
        },
        _ => Vec::new(),
    }
}

/// The `whatlang` languages the candidate tags name, without repeats.
fn candidate_langs(candidates: &[String]) -> Vec<Lang> {
    let mut langs: Vec<Lang> = Vec::new();
    for tag in candidates {
        if canonical_language_tag(tag).is_none() {
            continue;
        }
        // Norwegian: the detector knows Bokmål only; Filipino is written as
        // Tagalog (`detector_tag`).
        let tag = detector_tag(tag);
        let primary = tag.split('-').next().unwrap_or_default();
        if let Some(&lang) = Lang::all().iter().find(|&&l| tag_of(l) == primary) {
            if !langs.contains(&lang) {
                langs.push(lang);
            }
        }
    }
    langs
}

/// `whatlang` confidence (0.0..=1.0) in thousandths, clamped.
fn raw_permille(confidence: f64) -> u16 {
    if confidence.is_nan() || confidence <= 0.0 {
        return 0;
    }
    if confidence >= 1.0 {
        return 1000;
    }
    // 0 < confidence < 1, so the product is in (0, 1000).
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    let permille = (confidence * 1000.0).round() as u16;
    permille.min(1000)
}

/// Maps a raw confidence onto the clients' scale: a confident answer lands at
/// or above [`DETECTION_CONFIDENT_PERMILLE`], an unsure one below it, and the
/// order of raw values is kept within each.
fn scaled(raw: u16, confident: bool) -> u16 {
    let raw = u32::from(raw.min(1000));
    let threshold = u32::from(DETECTION_CONFIDENT_PERMILLE);
    let from = u32::from(CONFIDENT_FROM_PERMILLE);
    let value = if confident {
        // raw in [from, 1000] -> [threshold, 1000].
        threshold + (raw.saturating_sub(from)) * (1000 - threshold) / (1000 - from)
    } else {
        // raw in [0, 1000] -> [0, threshold - 1].
        raw * (threshold - 1) / 1000
    };
    u16::try_from(value.min(1000)).unwrap_or(1000)
}

/// Whether a letter counts toward the detected language's script.
fn counts_for(script: Script, lang: Lang, c: char) -> bool {
    let mut buf = [0u8; 4];
    let letter_script = whatlang::detect_script(c.encode_utf8(&mut buf));
    match lang {
        // Japanese is written in kana and Chinese characters together.
        Lang::Jpn => matches!(
            letter_script,
            Some(Script::Hiragana | Script::Katakana | Script::Mandarin)
        ),
        _ => letter_script == Some(script),
    }
}

/// Whether the script alone says which language it is.
/// Whether `letters_only` has enough letters for a confident answer: at
/// least [`MIN_LETTERS`], or in a script only one language uses.
fn long_enough(letters_only: &str) -> bool {
    let letters = letters_only.chars().filter(|c| c.is_alphabetic()).count();
    letters >= MIN_LETTERS
        || whatlang::detect_script(letters_only).is_some_and(script_names_the_language)
}

fn script_names_the_language(script: Script) -> bool {
    matches!(
        script,
        Script::Mandarin | Script::Hiragana | Script::Katakana
    ) || script.langs().len() == 1
}

/// The letter rules of the module doc: `false` when the text's letters
/// contradict the detected language.
fn letters_agree(lang: Lang, text: &str) -> bool {
    let has = |set: &str| {
        text.chars()
            .flat_map(char::to_lowercase)
            .any(|c| set.contains(c))
    };
    // The dotless `ı` is in the Turkish and Azerbaijani alphabets only.
    if has("ı") && !matches!(lang, Lang::Tur | Lang::Aze) {
        return false;
    }
    match lang {
        Lang::Por => has("ãõç"),
        Lang::Bul => has("ъ") && !has("ыэё"),
        Lang::Spa => !has("ãõç"),
        Lang::Rus => !has("іїєґў"),
        Lang::Ukr => has("іїєґ") && !has("ыэёъ"),
        Lang::Aze => has("ə"),
        Lang::Ron => has("ăâîșşțţ"),
        Lang::Bel => has("ў"),
        Lang::Srp => has("ђћџљњј"),
        Lang::Mkd => has("ѓќѕљњџј"),
        Lang::Cat => has("àèòïç"),
        _ => true,
    }
}

/// The BCP 47 tag for a `whatlang` language. Every language `whatlang` knows
/// has an ISO 639-1 code; the match is exhaustive, so a new `whatlang`
/// language fails the build until it is given one here.
const fn tag_of(lang: Lang) -> &'static str {
    match lang {
        Lang::Epo => "eo",
        Lang::Eng => "en",
        Lang::Rus => "ru",
        // Mandarin, script unknown (see the module doc).
        Lang::Cmn => "zh",
        Lang::Spa => "es",
        Lang::Por => "pt",
        Lang::Ita => "it",
        Lang::Ben => "bn",
        Lang::Fra => "fr",
        Lang::Deu => "de",
        Lang::Ukr => "uk",
        Lang::Kat => "ka",
        Lang::Ara => "ar",
        Lang::Hin => "hi",
        Lang::Jpn => "ja",
        Lang::Heb => "he",
        Lang::Yid => "yi",
        Lang::Pol => "pl",
        Lang::Amh => "am",
        Lang::Jav => "jv",
        Lang::Kor => "ko",
        Lang::Nob => "nb",
        Lang::Dan => "da",
        Lang::Swe => "sv",
        Lang::Fin => "fi",
        Lang::Tur => "tr",
        Lang::Nld => "nl",
        Lang::Hun => "hu",
        Lang::Ces => "cs",
        Lang::Ell => "el",
        Lang::Bul => "bg",
        Lang::Bel => "be",
        Lang::Mar => "mr",
        Lang::Kan => "kn",
        Lang::Ron => "ro",
        Lang::Slv => "sl",
        Lang::Hrv => "hr",
        Lang::Srp => "sr",
        Lang::Mkd => "mk",
        Lang::Lit => "lt",
        Lang::Lav => "lv",
        Lang::Est => "et",
        Lang::Tam => "ta",
        Lang::Vie => "vi",
        Lang::Urd => "ur",
        Lang::Tha => "th",
        Lang::Guj => "gu",
        Lang::Uzb => "uz",
        Lang::Pan => "pa",
        Lang::Aze => "az",
        Lang::Ind => "id",
        Lang::Tel => "te",
        // Iranian Persian; `fa` is the tag every platform uses.
        Lang::Pes => "fa",
        Lang::Mal => "ml",
        Lang::Ori => "or",
        Lang::Mya => "my",
        Lang::Nep => "ne",
        Lang::Sin => "si",
        Lang::Khm => "km",
        Lang::Tuk => "tk",
        Lang::Aka => "ak",
        Lang::Zul => "zu",
        Lang::Sna => "sn",
        Lang::Afr => "af",
        Lang::Lat => "la",
        Lang::Slk => "sk",
        Lang::Cat => "ca",
        Lang::Tgl => "tl",
        Lang::Hye => "hy",
        Lang::Cym => "cy",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::translation::{canonical_language_tag, translation_offered, TranslateOffer};

    fn detect(text: &str) -> DetectedLanguage {
        match detect_text_language(text) {
            Some(d) => d,
            None => panic!("no language for {text:?}"),
        }
    }

    fn confident(d: &DetectedLanguage) -> bool {
        d.confidence_permille >= DETECTION_CONFIDENT_PERMILLE
    }

    #[test]
    fn sentences_are_detected_and_confident() {
        for (text, tag) in [
            (
                "Ik ga morgen naar de markt om groenten te kopen. Wil je met me mee, of heb je het te druk?",
                "nl",
            ),
            (
                "Я завтра пойду на рынок за овощами. Хочешь пойти со мной, или ты занят на работе?",
                "ru",
            ),
            (
                "I am going to the market tomorrow to buy vegetables. Do you want to come with me, or are you busy?",
                "en",
            ),
            (
                "Ich gehe morgen auf den Markt, um Gemüse zu kaufen. Willst du mitkommen?",
                "de",
            ),
            (
                "Mañana voy al mercado a comprar verduras. ¿Quieres venir conmigo o estás ocupado con el trabajo?",
                "es",
            ),
        ] {
            let d = detect(text);
            assert_eq!(d.tag, tag, "{text}");
            assert!(confident(&d), "{text}: {}", d.confidence_permille);
        }
    }

    #[test]
    fn short_sentences_get_the_right_tag_even_when_unsure() {
        for (text, tag) in [
            ("Ik ga morgen naar de markt om groenten te kopen.", "nl"),
            ("Я завтра пойду на рынок за овощами.", "ru"),
            ("I am going to the market tomorrow to buy vegetables.", "en"),
            ("Mañana voy al mercado a comprar verduras.", "es"),
        ] {
            assert_eq!(detect(text).tag, tag, "{text}");
        }
    }

    #[test]
    fn text_without_letters_has_no_language() {
        for text in [
            "",
            "   ",
            "\n\t",
            "12345 678",
            "!!! ???",
            "😀😀",
            "·",
            "，",
            "+31 6 1234 5678",
        ] {
            assert_eq!(detect_text_language(text), None, "{text:?}");
        }
    }

    #[test]
    fn a_single_short_word_is_never_confident() {
        for text in [
            "ok",
            "lol",
            "hola",
            "Hallo",
            "thanks",
            "merci",
            "да",
            "Привет",
            "ja",
            "Tak",
        ] {
            if let Some(d) = detect_text_language(text) {
                assert!(
                    !confident(&d),
                    "{text}: {} {}",
                    d.tag,
                    d.confidence_permille
                );
            }
        }
    }

    #[test]
    fn a_script_only_one_language_uses_is_confident_when_short() {
        assert_eq!(detect("안녕하세요").tag, "ko");
        assert!(confident(&detect("안녕하세요")));
        let ja = detect("こんにちは、元気ですか");
        assert_eq!(ja.tag, "ja");
        assert!(confident(&ja));
    }

    #[test]
    fn chinese_is_named_but_never_confident() {
        for text in ["你好，我明天去市场买菜。", "我們明天去市場買菜。"] {
            let d = detect(text);
            assert_eq!(d.tag, "zh");
            assert!(!confident(&d), "{text}");
        }
    }

    #[test]
    fn mixed_scripts_are_unsure() {
        // Measured: whatlang reads this as Javanese with 0.79.
        if let Some(d) = detect_text_language("邀请你加入 Lakko") {
            assert!(!confident(&d), "{} {}", d.tag, d.confidence_permille);
        }
    }

    #[test]
    fn contradicting_letters_lower_the_answer() {
        // Measured: whatlang reads this Spanish string as Portuguese at 0.86.
        let d =
            detect("No pudimos traer de vuelta tu cuenta. Inténtalo de nuevo o elige otra forma.");
        assert!(d.tag != "pt" || !confident(&d), "{d:?}");
        // And this Russian one as Bulgarian at 0.78.
        let d = detect("Сохранено. Подписчики увидят новое название вместе со следующими постами.");
        assert!(d.tag != "bg" || !confident(&d), "{d:?}");
        // Measured on the chat corpus: each of these was read as the
        // neighbour named, with whatlang confidence at or above 0.25.
        for (text, not_confidently) in [
            ("Можешь мне позже перезвонить?", "uk"),
            ("On dakikaya oradayım.", "id"),
            ("Burada bütün gün yağmur yağıyor.", "az"),
            ("Volver a la llamada.", "ca"),
            ("Назад к каналам", "be"),
            ("Would you like coffee or tea?", "ro"),
        ] {
            let d = detect(text);
            assert!(d.tag != not_confidently || !confident(&d), "{text}: {d:?}");
        }
    }

    #[test]
    fn an_ordinary_dutch_sentence_is_confident() {
        // Reported from the web client: whatlang gives Dutch 0.31 here, with
        // Afrikaans second. At the 0.3 cut-off it is confident, narrowly.
        let d = detect("Ik ga morgen met de trein naar Amsterdam om mijn moeder te bezoeken.");
        assert_eq!(d.tag, "nl");
        assert!(confident(&d), "{}", d.confidence_permille);
    }

    #[test]
    fn every_tag_is_a_language_the_translation_rules_read() {
        for &lang in Lang::all() {
            let tag = tag_of(lang);
            assert!(canonical_language_tag(tag).is_some(), "{lang:?} -> {tag}");
            assert!(!tag.contains(['-', '_']), "{tag}");
        }
    }

    #[test]
    fn an_unsure_answer_opens_the_picker_and_a_sure_one_does_not() {
        use crate::translation::{EngineState, TranslateOfferInput};
        let reading = vec!["en".to_string()];
        let offer = |text: &str| {
            translation_offered(
                &TranslateOfferInput {
                    enabled: true,
                    has_text: true,
                    detected: detect_text_language(text),
                    engine_for_detected: EngineState::Ready,
                    any_engine: true,
                },
                &reading,
            )
        };
        assert_eq!(
            offer("Ich gehe morgen auf den Markt, um Gemüse zu kaufen. Willst du mitkommen?"),
            TranslateOffer::Translate
        );
        assert_eq!(offer("hola"), TranslateOffer::TranslateAskLanguage);
        assert_eq!(offer("12345"), TranslateOffer::TranslateAskLanguage);
    }

    #[test]
    fn the_scale_keeps_order_and_the_threshold() {
        assert_eq!(
            scaled(CONFIDENT_FROM_PERMILLE, true),
            DETECTION_CONFIDENT_PERMILLE
        );
        assert_eq!(scaled(1000, true), 1000);
        assert_eq!(scaled(1000, false), DETECTION_CONFIDENT_PERMILLE - 1);
        assert_eq!(scaled(0, false), 0);
        for r in 0..1000u16 {
            assert!(scaled(r, false) <= scaled(r + 1, false));
            if r >= CONFIDENT_FROM_PERMILLE {
                assert!(scaled(r, true) <= scaled(r + 1, true));
            }
        }
        assert_eq!(raw_permille(f64::NAN), 0);
        assert_eq!(raw_permille(-1.0), 0);
        assert_eq!(raw_permille(2.0), 1000);
    }

    #[test]
    fn only_the_first_characters_are_read() {
        let long = "Ich gehe morgen auf den Markt, um Gemüse zu kaufen. ".repeat(200);
        assert!(long.chars().count() > MAX_DETECT_CHARS);
        assert_eq!(detect(&long).tag, "de");
    }

    fn tags(list: &[&str]) -> Vec<String> {
        list.iter().map(|t| (*t).to_string()).collect()
    }

    fn none() -> ConversationLanguages {
        ConversationLanguages::default()
    }

    fn picked(tag: &str) -> ConversationLanguages {
        none().with_pick(tag, 1)
    }

    fn guess(tag: &str, permille: u16, alternatives: &[&str]) -> LanguageCandidates {
        LanguageCandidates {
            detected: DetectedLanguage {
                tag: tag.into(),
                confidence_permille: permille,
            },
            alternatives: tags(alternatives),
        }
    }

    #[test]
    fn translate_starts_from_a_usable_candidate_of_an_unsure_guess() {
        // Seen on a Pixel: "hola, qué tal?" is an unsure Catalan with
        // Spanish, French and Italian close; the device cannot translate
        // from Catalan, so Translate starts from Spanish. An unsure guess is
        // never named up front (2026-11-20), so there is no line.
        let hola = cands("hola, qué tal?");
        assert_eq!(hola.detected.tag, "ca");
        let engine = tags(&["es", "fr", "it", "de", "nl", "en"]);
        let r = resolve_translation_source(Some(&hola), &none(), "en", &engine);
        assert_eq!(r.line_tag, None);
        assert!(!r.line_can_translate);
        assert_eq!(r.translate_source.as_deref(), Some("es"));
        // "goedendag": Turkmen unsure, Dutch first among those the device
        // can translate from.
        let goedendag = cands("goedendag");
        let r = resolve_translation_source(Some(&goedendag), &none(), "en", &engine);
        assert_eq!(r.translate_source.as_deref(), Some("nl"));
        assert_eq!(r.line_tag, None);
        // The conversation's Dutch decides it before the order.
        let r = resolve_translation_source(
            Some(&guess("tk", 200, &["da", "nl"])),
            &picked("nl"),
            "en",
            &tags(&["da", "nl"]),
        );
        assert_eq!(r.translate_source.as_deref(), Some("nl"));
    }

    #[test]
    fn the_line_and_translate_never_disagree_on_a_language_shown() {
        let engine = tags(&["es"]);
        // A usable guess is its own source, and is not named up front.
        let r =
            resolve_translation_source(Some(&guess("es", 300, &["ca"])), &none(), "en", &engine);
        assert_eq!(
            (r.line_tag.as_deref(), r.translate_source.as_deref()),
            (None, Some("es"))
        );
        // A confident detection the device can translate from is named.
        let r = resolve_translation_source(Some(&guess("es", 900, &[])), &none(), "en", &engine);
        assert_eq!(
            (r.line_tag.as_deref(), r.translate_source.as_deref()),
            (Some("es"), Some("es"))
        );
        assert!(r.line_can_translate);
        // A confident language the device cannot translate from: the name
        // alone, and Translate goes to it (and says it cannot).
        let r =
            resolve_translation_source(Some(&guess("tk", 900, &[])), &picked("es"), "en", &engine);
        assert_eq!(r.line_tag.as_deref(), Some("tk"));
        assert!(!r.line_can_translate);
        assert_eq!(r.translate_source.as_deref(), Some("tk"));
        // Unsure and nothing translatable: no line; Translate uses the
        // remembered language, else the picker with the guess marked.
        let r = resolve_translation_source(
            Some(&guess("tk", 200, &["da"])),
            &picked("de"),
            "en",
            &engine,
        );
        assert_eq!(r.line_tag, None);
        assert_eq!(r.translate_source.as_deref(), Some("de"));
        let r =
            resolve_translation_source(Some(&guess("tk", 200, &["da"])), &none(), "en", &engine);
        assert_eq!((r.translate_source, r.guess.as_deref()), (None, Some("tk")));
        // The language translated into is never a source.
        let r = resolve_translation_source(
            Some(&guess("en", 300, &["es"])),
            &none(),
            "en",
            &tags(&["en", "es"]),
        );
        assert_eq!(r.translate_source.as_deref(), Some("es"));
        // No detection: the remembered language for Translate, no line.
        let r = resolve_translation_source(None, &picked("es"), "en", &engine);
        assert_eq!(
            (r.line_tag, r.translate_source.as_deref()),
            (None, Some("es"))
        );
    }

    fn detected(list: &[(i64, &str)]) -> ConversationLanguages {
        list.iter()
            .fold(none(), |r, (id, tag)| r.with_detection(*id, tag))
    }

    #[test]
    fn g55_two_names_hola_spanish_in_any_order() {
        // Seen on a Pixel: one launch named "hola, qué tal?" Spanish, the
        // next Italian, because the remembered language was whichever
        // confident detection ran last. The chat holds one confident message
        // each in es, it, uk, de and ru: Spanish and Italian tie, and the
        // detector lists Spanish first.
        let hola = guess("ca", 180, &["es", "fr", "it"]);
        let seen = [(1, "es"), (2, "it"), (3, "uk"), (4, "de"), (5, "ru")];
        let mut orders = vec![seen.to_vec()];
        let mut reversed = seen.to_vec();
        reversed.reverse();
        orders.push(reversed);
        orders.push(vec![seen[1], seen[4], seen[0], seen[3], seen[2]]);
        // Ends on Italian: the order that named it Italian before.
        orders.push(vec![seen[0], seen[2], seen[3], seen[4], seen[1]]);
        for order in orders {
            let record = detected(&order);
            assert_eq!(
                conversation_detection(Some(&hola), &record).map(|d| d.tag),
                Some("es".to_string()),
                "{order:?}"
            );
            let r =
                resolve_translation_source(Some(&hola), &record, "en", &tags(&["es", "fr", "it"]));
            assert_eq!(r.translate_source.as_deref(), Some("es"));
            assert_eq!(r.line_tag, None);
        }
        // A second Italian message makes Italian the chat's language.
        let more = detected(&[(1, "es"), (2, "it"), (6, "it")]);
        assert_eq!(more.preferred_among(&hola.tags()).as_deref(), Some("it"));
        // A pick beats any count; the latest pick wins.
        let p = more.with_pick("es", 10).with_pick("fr", 20);
        assert_eq!(p.preferred_among(&hola.tags()).as_deref(), Some("fr"));
        assert_eq!(
            p.with_pick("es", 30)
                .preferred_among(&hola.tags())
                .as_deref(),
            Some("es")
        );
        // Nothing known: the detection stands.
        assert_eq!(none().preferred_among(&hola.tags()), None);
        // A message's own confident detection is never changed.
        let it = guess("it", 950, &[]);
        assert_eq!(
            conversation_detection(Some(&it), &picked("es")).map(|d| d.tag),
            Some("it".to_string())
        );
    }

    #[test]
    fn the_record_is_bounded() {
        let mut r = none();
        for id in 0..(i64::try_from(MAX_CONVERSATION_DETECTIONS).unwrap_or(0) + 50) {
            r = r.with_detection(id, "es");
        }
        assert_eq!(r.detections.len(), MAX_CONVERSATION_DETECTIONS);
        assert_eq!(r.detections.first().map(|d| d.message_id), Some(50));
        let langs = [
            "es", "it", "de", "fr", "nl", "pt", "ru", "uk", "pl", "tr", "ar", "fa", "he", "hi",
            "ja", "ko", "zh", "sv", "da",
        ];
        let mut p = none();
        for (i, l) in langs.iter().enumerate() {
            p = p.with_pick(l, i as u64);
        }
        assert_eq!(p.picks.len(), MAX_CONVERSATION_PICKS);
        assert!(!p.picks.iter().any(|q| q.tag == "es"));
        assert!(p.picks.iter().any(|q| q.tag == "da"));
        // The same language twice keeps its latest time.
        let twice = none().with_pick("es", 5).with_pick("es-419", 3);
        assert_eq!(
            twice.picks,
            vec![LanguagePick {
                tag: "es".into(),
                at_ms: 5
            }]
        );
    }

    fn cands(text: &str) -> LanguageCandidates {
        match detect_text_language_candidates(text) {
            Some(c) => c,
            None => panic!("no candidates for {text:?}"),
        }
    }

    #[test]
    fn the_measured_near_ties_carry_their_neighbours() {
        // Measured with whatlang =0.18.0 (module doc).
        let hola = cands("hola, qué tal?");
        assert_eq!(hola.detected.tag, "ca");
        assert!(hola.contains("es"), "{hola:?}");
        let ciao = cands("Ciao, come stai?");
        assert!(ciao.contains("it"), "{ciao:?}");
        let goed = cands("goedendag");
        assert!(goed.contains("nl"), "{goed:?}");
        // Portuguese without its letters is not a candidate.
        assert!(!cands("hola que tal").contains("pt"));
        for c in [&hola, &ciao, &goed] {
            assert!(c.alternatives.len() <= MAX_ALTERNATIVES);
            assert!(!confident(&c.detected));
        }
    }

    #[test]
    fn a_confident_detection_has_no_alternatives() {
        let uk = cands("Привіт, як справи? Що ти робиш сьогодні ввечері?");
        assert_eq!(uk.detected.tag, "uk");
        assert!(confident(&uk.detected));
        assert!(uk.alternatives.is_empty());
        let de = cands("Ich gehe morgen auf den Markt, um Gemüse zu kaufen. Willst du mitkommen?");
        assert!(de.alternatives.is_empty());
    }

    #[test]
    fn the_conversation_language_decides_a_near_tie_and_nothing_else() {
        let hola = cands("hola, qué tal?");
        let chosen = conversation_detection(Some(&hola), &picked("es"));
        assert_eq!(chosen.as_ref().map(|d| d.tag.as_str()), Some("es"));
        // Still a guess: memory does not make it sure.
        assert!(chosen.is_some_and(|d| !confident(&d)));
        // A remembered language outside the set changes nothing (Dutch never
        // names an unsure Ukrainian message).
        let uk = cands("Можешь мне позже перезвонить?");
        assert!(!uk.contains("nl"));
        assert_eq!(
            conversation_detection(Some(&uk), &picked("nl")),
            Some(uk.detected.clone())
        );
        // A confident detection is never overridden.
        let de = cands("Ich gehe morgen auf den Markt, um Gemüse zu kaufen. Willst du mitkommen?");
        assert_eq!(
            conversation_detection(Some(&de), &picked("nl")),
            Some(de.detected.clone())
        );
        assert_eq!(conversation_detection(None, &picked("es")), None);
        assert_eq!(
            conversation_detection(Some(&hola), &none()),
            Some(hola.detected.clone())
        );
        // Remembered in another form of the same language.
        assert_eq!(
            conversation_detection(Some(&hola), &picked("es-419")).map(|d| d.tag),
            Some("es".to_string())
        );
    }

    #[test]
    fn short_messages_are_never_named_up_front() {
        // Seen on test.lakko.me, 2026-11-20: English test messages named
        // Polish, Italian and Afrikaans by the line under the message.
        let engine = tags(&["pl", "it", "af", "es", "de", "nl", "fr"]);
        for text in [
            "W7 web to iphone",
            "W7 droid to iphone",
            "W5 iphone hello",
            "W6 iphone while behind",
            "hola, qué tal?",
            "ok",
        ] {
            let c = detect_text_language_candidates(text);
            assert!(
                c.as_ref().is_none_or(|c| !confident(&c.detected)),
                "{text}: {c:?}"
            );
            let r = resolve_translation_source(c.as_ref(), &none(), "en", &engine);
            assert_eq!(r.line_tag, None, "{text}");
        }
    }

    #[test]
    fn hypotheses_go_through_the_same_rule() {
        let h = |list: &[(&str, u16)]| -> Vec<(String, u16)> {
            list.iter().map(|(t, p)| ((*t).to_string(), *p)).collect()
        };
        // Probabilities in any order; a duplicate in another form merges.
        let c = candidates_from_hypotheses(
            "hola, qué tal?",
            &h(&[
                ("es", 300),
                ("ca", 400),
                ("ES-es", 310),
                ("it", 150),
                ("und", 900),
                ("", 5),
            ]),
        );
        let c = c.as_ref();
        assert_eq!(c.map(|c| c.detected.tag.as_str()), Some("ca"));
        assert_eq!(c.map(|c| c.detected.confidence_permille), Some(400));
        // es (310 >= 200) in, it (150 < 200) out.
        assert_eq!(
            c.map(|c| c.alternatives.clone()),
            Some(vec!["es".to_string()])
        );
        // Letters that contradict drop a hypothesis.
        let pt = candidates_from_hypotheses("hola que tal", &h(&[("es", 450), ("pt", 400)]));
        assert_eq!(pt.map(|c| c.alternatives), Some(vec![]));
        // A confident top has no alternatives.
        let long = "Ich gehe morgen auf den Markt, um Gemüse zu kaufen.";
        let sure = candidates_from_hypotheses(long, &h(&[("de", 800), ("nl", 600)]));
        assert_eq!(sure.map(|c| c.alternatives), Some(vec![]));
        // Too short for any detector: a guess however probable (seen on
        // iOS, 2026-11-20: "W7 web to iphone" named Polish).
        let short = candidates_from_hypotheses("W7 web to iphone", &h(&[("pl", 900), ("en", 80)]));
        let short = short.as_ref();
        assert!(short.is_some_and(|c| !confident(&c.detected)));
        assert_eq!(
            resolve_translation_source(short, &none(), "en", &tags(&["pl"])).line_tag,
            None
        );
        // A script only one language uses is evidence enough.
        let el = candidates_from_hypotheses("Γεια σου", &h(&[("el", 950)]));
        assert!(el.is_some_and(|c| confident(&c.detected)));
        assert_eq!(candidates_from_hypotheses("x", &h(&[("und", 900)])), None);
    }

    #[test]
    fn not_x_lists_the_close_candidates_first() {
        let langs = tags(&["de", "en", "es", "fr", "it", "nl", "pt"]);
        let hola = cands("hola, qué tal?");
        let ordered = order_picker_languages(&langs, Some(&hola));
        // ca is not a picker language, so the list starts with es, fr, it.
        assert_eq!(&ordered[..3], &tags(&["es", "fr", "it"])[..]);
        assert_eq!(ordered.len(), langs.len());
        let mut sorted = ordered.clone();
        sorted.sort();
        assert_eq!(sorted, langs);
        assert_eq!(order_picker_languages(&langs, None), langs);
    }

    #[test]
    fn candidate_tags_in_any_form_name_the_language() {
        assert_eq!(
            candidate_langs(&tags(&[
                "pt-BR", "zh-Hant", "iw", "no", "fil", "PT_pt", "es-419"
            ])),
            vec![
                Lang::Por,
                Lang::Cmn,
                Lang::Heb,
                Lang::Nob,
                Lang::Tgl,
                Lang::Spa
            ]
        );
    }

    mod properties {
        use super::super::*;
        use proptest::prelude::*;

        const POOL: &[&str] = &[
            "en", "ru", "es", "ca", "nl", "tk", "de", "fr", "pt-BR", "uk", "bg", "ar", "fa", "he",
            "el", "ko", "ja", "zh-Hans", "zh-Hant", "hi", "tr", "no", "ms", "und", "",
        ];

        proptest! {
            #[test]
            fn never_panics_and_stays_in_range(text in any::<String>()) {
                if let Some(d) = detect_text_language(&text) {
                    prop_assert!(d.confidence_permille <= 1000);
                    prop_assert!(crate::translation::canonical_language_tag(&d.tag).is_some());
                }
            }

            #[test]
            fn never_panics_on_mixed_scripts(text in "[a-zA-Zа-яА-Яёъыэ一-龥ぁ-ゟ가-힣 0-9.,!?]{0,200}") {
                let _ = detect_text_language(&text);
            }

            #[test]
            fn candidates_are_bounded_and_the_conversation_never_leaves_them(
                text in "[a-zA-Zа-яА-Яёіїєґàéèçãõñüßø ,.?!]{0,80}",
                remembered in proptest::sample::select(POOL),
            ) {
                if let Some(c) = detect_text_language_candidates(&text) {
                    prop_assert!(c.alternatives.len() <= MAX_ALTERNATIVES);
                    prop_assert!(!c.alternatives.iter().any(|a| same_language(a, &c.detected.tag)));
                    for a in &c.alternatives {
                        prop_assert!(canonical_language_tag(a).as_deref() == Some(a.as_str()));
                    }
                    if c.detected.confidence_permille >= DETECTION_CONFIDENT_PERMILLE {
                        prop_assert!(c.alternatives.is_empty());
                    }
                    let chosen = conversation_detection(Some(&c), &ConversationLanguages::default().with_pick(remembered, 1));
                    prop_assert!(chosen.is_some_and(|d| c.contains(&d.tag)
                        && d.confidence_permille == c.detected.confidence_permille));
                }
            }

            #[test]
            fn a_line_with_translate_names_the_source(
                text in "[a-zA-Zàéèçãõñüßø ,.?!]{0,60}",
                remembered in proptest::sample::select(POOL),
                engine in proptest::collection::vec(proptest::sample::select(POOL), 0..6),
                target in proptest::sample::select(&["en", "ru", "es", "nl"][..]),
            ) {
                let c = detect_text_language_candidates(&text);
                let engine: Vec<String> = engine.iter().map(|t| (*t).to_string()).collect();
                let r = resolve_translation_source(c.as_ref(), &ConversationLanguages::default().with_pick(remembered, 1), target, &engine);
                if r.line_tag.is_some() && (r.line_can_translate || c.as_ref().is_some_and(|c| c.detected.confidence_permille >= DETECTION_CONFIDENT_PERMILLE)) {
                    prop_assert_eq!(&r.line_tag, &r.translate_source);
                }
                if r.line_can_translate {
                    let tag = r.line_tag.clone().unwrap_or_default();
                    prop_assert!(!same_language(&tag, target));
                    prop_assert!(engine.iter().any(|t| same_language(t, &tag)));
                }
            }

            #[test]
            fn the_conversation_language_does_not_depend_on_arrival_order(
                entries in proptest::collection::vec((0i64..400, proptest::sample::select(POOL)), 0..60),
                picks in proptest::collection::vec((proptest::sample::select(POOL), 0u64..50), 0..6),
                seed in any::<u64>(),
                alternatives in proptest::collection::vec(proptest::sample::select(POOL), 0..4),
            ) {
                // Distinct message ids: one language per message.
                let mut seen = std::collections::HashSet::new();
                let entries: Vec<(i64, &str)> = entries.into_iter().filter(|(id, _)| seen.insert(*id)).collect();
                let build = |order: &[(i64, &str)], picks: &[(&str, u64)]| {
                    let mut r = ConversationLanguages::default();
                    for (id, tag) in order {
                        r = r.with_detection(*id, tag);
                    }
                    for (tag, at) in picks {
                        r = r.with_pick(tag, *at);
                    }
                    r
                };
                let mut shuffled = entries.clone();
                let mut shuffled_picks = picks.clone();
                // A deterministic shuffle from the seed.
                let mut state = seed | 1;
                for i in (1..shuffled.len()).rev() {
                    state ^= state << 13; state ^= state >> 7; state ^= state << 17;
                    shuffled.swap(i, usize::try_from(state % (i as u64 + 1)).unwrap_or(0));
                }
                shuffled_picks.reverse();
                let a = build(&entries, &picks);
                let b = build(&shuffled, &shuffled_picks);
                prop_assert_eq!(&a, &b);
                prop_assert!(a.detections.len() <= MAX_CONVERSATION_DETECTIONS);
                prop_assert!(a.picks.len() <= MAX_CONVERSATION_PICKS);
                let c = LanguageCandidates {
                    detected: DetectedLanguage { tag: "ca".into(), confidence_permille: 100 },
                    alternatives: alternatives.iter().map(|t| (*t).to_string()).collect(),
                };
                prop_assert_eq!(conversation_detection(Some(&c), &a), conversation_detection(Some(&c), &b));
                prop_assert_eq!(a.usual(), b.usual());
            }

            #[test]
            fn text_without_letters_is_none(text in "[0-9 .,!?:;()+\\-_/\\n\\t]{0,100}") {
                prop_assert_eq!(detect_text_language(&text), None);
            }
        }
    }
}
