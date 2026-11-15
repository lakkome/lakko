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
//!   Japanese characters are exempt: the script is the evidence.
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

use whatlang::{Lang, Script};

use crate::translation::{DetectedLanguage, DETECTION_CONFIDENT_PERMILLE};

/// At most this many characters of the text are read. Chosen, not measured:
/// a message longer than this is decided by its start, and the work per
/// detection stays bounded.
pub const MAX_DETECT_CHARS: usize = 2048;

/// Letters a shared-script text needs before an answer can be confident.
/// Chosen, not measured: on the UI corpus a minimum of 0, 8 or 16 letters
/// changed no wrong answer, so this is a guard for very short Latin text only.
pub const MIN_LETTERS: usize = 8;

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
    let letters_only: String = text
        .chars()
        .take(MAX_DETECT_CHARS)
        .map(|c| if c.is_alphabetic() { c } else { ' ' })
        .collect();
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
                    is_own: false,
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

    mod properties {
        use super::super::*;
        use proptest::prelude::*;

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
            fn text_without_letters_is_none(text in "[0-9 .,!?:;()+\\-_/\\n\\t]{0,100}") {
                prop_assert_eq!(detect_text_language(&text), None);
            }
        }
    }
}
