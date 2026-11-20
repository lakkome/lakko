// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! ADR-0092 content messages on the wasm surface: polls, checklists,
//! locations, contact cards, articles; their ops, folds, saved state and
//! limits. The UniFFI mirror is `lakko-ffi-uniffi/src/content.rs`; every
//! rule is `lakko-attachments`'.
//!
//! The content values cross as plain JS objects whose TypeScript shapes are
//! declared below (`MessageContent`, `SignedPoll`, ...). Inside them, byte
//! fields (poll id, secrets, keys, signatures) are **standard base64
//! strings** and message ids / times are **numbers** (DS ids and unix
//! milliseconds are far below 2^53). Function arguments that are bytes are
//! `Uint8Array`.
//!
//! Errors throw an `Error` whose message is `"<code>: <reason>"`, the code
//! being `lakko_attachments::ContentError::code` (`"question.tooLong"`,
//! `"signatureInvalid"`, ...), `"<field>.badLength"`, `"<field>.badBase64"`,
//! `"state.mismatch"`, or `"js.shape"` for an object of the wrong shape.

use base64::engine::general_purpose::STANDARD as B64;
use base64::Engine as _;
use lakko_attachments as inner;
use lakko_attachments::{article, checklist, content_state as cs, poll};
use serde::{Deserialize, Serialize};
use wasm_bindgen::prelude::*;

#[wasm_bindgen(typescript_custom_section)]
const CONTENT_TS: &'static str = r#"
/** Standard base64 of raw bytes. */
export type B64 = string;
export interface PollQuiz { correct: number; explanation: string; }
export interface PollAnonymous { secret: B64; ring0Hash: B64; }
export interface PollBody {
  id: B64; question: string; options: string[]; multiple: boolean;
  quiz?: PollQuiz | null; closeAtMs?: number | null; anonymous?: PollAnonymous | null;
}
export interface SignedPoll { body: PollBody; authorSig?: B64 | null; }
export interface ChecklistItem { id: number; text: string; }
export interface ChecklistBody { title: string; othersAdd: boolean; othersMark: boolean; items: ChecklistItem[]; }
export interface SignedChecklist { body: ChecklistBody; authorSig?: B64 | null; }
export type ChecklistItemRef = { kind: "author"; id: number } | { kind: "added"; messageId: number };
export interface SharedLocation { latE7: number; lonE7: number; accuracyM: number; fixAtMs: number; name?: string | null; }
export interface SharedContactCard { name: string; username: string; accountKey: B64; }
export type ArticleRun =
  | { kind: "text"; bold: boolean; italic: boolean; text: string }
  | { kind: "link"; url: string; bold: boolean; italic: boolean; text: string };
export interface ArticleListItem { depth: number; runs: ArticleRun[]; }
export type ArticleBlock =
  | { kind: "paragraph"; runs: ArticleRun[] }
  | { kind: "heading"; level: "h1" | "h2"; runs: ArticleRun[] }
  | { kind: "quote"; runs: ArticleRun[] }
  | { kind: "list"; ordered: boolean; items: ArticleListItem[] }
  | { kind: "image"; attachmentIndex: number; caption: ArticleRun[] }
  | { kind: "divider" };
export interface Article { title: string; blocks: ArticleBlock[]; }
export type MessageContent =
  | { kind: "poll"; poll: SignedPoll }
  | { kind: "checklist"; checklist: SignedChecklist }
  | { kind: "location"; location: SharedLocation }
  | { kind: "contactCard"; card: SharedContactCard }
  | { kind: "article"; article: Article };
export interface ContentEnvelopeParts {
  replyTo?: number | null; replyAuthor?: string | null; replyExcerpt?: string | null;
  forwardedFrom?: string | null; forwardOriginGroupB64?: string | null; forwardOriginMessageId?: number | null;
}
export interface PollVoteRecord { messageId: number; createdAtMs: number; voter: B64; choices: number[]; }
export interface PollCloseRecord { messageId: number; closer: B64; closerIsAdmin: boolean; }
export interface PollVoterChoices { voter: B64; choices: number[]; }
export interface PollTally {
  counts: number[]; voters: number; choicesByVoter: PollVoterChoices[];
  closedBy?: number | null; closeAtMs?: number | null;
}
export interface ChecklistEditRecord { messageId: number; editor: B64; body: ChecklistBody; }
export interface ChecklistTickRecord { messageId: number; ticker: B64; privileged: boolean; item: ChecklistItemRef; done: boolean; }
export interface ChecklistAddRecord { messageId: number; adder: B64; privileged: boolean; text: string; }
export interface ChecklistCloseRecord { messageId: number; closer: B64; }
export interface ChecklistStateItem { item: ChecklistItemRef; text: string; addedBy?: B64 | null; doneBy?: B64 | null; }
export interface ChecklistState {
  title: string; othersAdd: boolean; othersMark: boolean; items: ChecklistStateItem[]; closedBy?: number | null;
}
export type PollClosedState = { kind: "open" } | { kind: "byTime" } | { kind: "byOp"; messageId: number };
export type ContentState =
  | { kind: "poll"; counts: number[]; voters: number; closed: PollClosedState }
  | { kind: "checklist"; closedBy?: number | null;
      added: { messageId: number; text: string }[];
      done: { item: ChecklistItemRef; by: B64 }[] };
export interface ContentMessage { content: MessageContent; state?: ContentState | null; }
export interface ContentLimits {
  maxPollQuestionBytes: number; minPollOptions: number; maxPollOptions: number;
  maxPollOptionBytes: number; maxPollExplanationBytes: number; maxChecklistTitleBytes: number;
  maxChecklistItems: number; maxChecklistItemBytes: number; maxLocationNameBytes: number;
  maxArticleTitleBytes: number; maxArticleTextBytes: number; maxArticleBlocks: number;
  maxArticleImages: number; maxArticleUrlBytes: number; maxAudioTagBytes: number;
  maxAudioDurationMs: number;
}
"#;

#[wasm_bindgen]
extern "C" {
    /// `MessageContent` (see the TypeScript section).
    #[wasm_bindgen(typescript_type = "MessageContent")]
    pub type JsMessageContent;
    /// `PollBody`.
    #[wasm_bindgen(typescript_type = "PollBody")]
    pub type JsPollBody;
    /// `SignedPoll`.
    #[wasm_bindgen(typescript_type = "SignedPoll")]
    pub type JsSignedPoll;
    /// `ChecklistBody`.
    #[wasm_bindgen(typescript_type = "ChecklistBody")]
    pub type JsChecklistBody;
    /// `SignedChecklist`.
    #[wasm_bindgen(typescript_type = "SignedChecklist")]
    pub type JsSignedChecklist;
    /// `ChecklistItemRef`.
    #[wasm_bindgen(typescript_type = "ChecklistItemRef")]
    pub type JsChecklistItemRef;
    /// `Article`.
    #[wasm_bindgen(typescript_type = "Article")]
    pub type JsArticle;
    /// `ContentEnvelopeParts | undefined`.
    #[wasm_bindgen(typescript_type = "ContentEnvelopeParts | undefined")]
    pub type JsContentEnvelopeParts;
    /// `PollVoteRecord[]`.
    #[wasm_bindgen(typescript_type = "PollVoteRecord[]")]
    pub type JsPollVoteRecords;
    /// `PollCloseRecord[]`.
    #[wasm_bindgen(typescript_type = "PollCloseRecord[]")]
    pub type JsPollCloseRecords;
    /// `PollTally`.
    #[wasm_bindgen(typescript_type = "PollTally")]
    pub type JsPollTally;
    /// `ChecklistEditRecord[]`.
    #[wasm_bindgen(typescript_type = "ChecklistEditRecord[]")]
    pub type JsChecklistEditRecords;
    /// `ChecklistAddRecord[]`.
    #[wasm_bindgen(typescript_type = "ChecklistAddRecord[]")]
    pub type JsChecklistAddRecords;
    /// `ChecklistTickRecord[]`.
    #[wasm_bindgen(typescript_type = "ChecklistTickRecord[]")]
    pub type JsChecklistTickRecords;
    /// `ChecklistCloseRecord[]`.
    #[wasm_bindgen(typescript_type = "ChecklistCloseRecord[]")]
    pub type JsChecklistCloseRecords;
    /// `ChecklistState`.
    #[wasm_bindgen(typescript_type = "ChecklistState")]
    pub type JsChecklistState;
    /// `ContentState`.
    #[wasm_bindgen(typescript_type = "ContentState")]
    pub type JsContentState;
    /// `PollClosedState`.
    #[wasm_bindgen(typescript_type = "PollClosedState")]
    pub type JsPollClosedState;
    /// `ContentMessage | undefined`.
    #[wasm_bindgen(typescript_type = "ContentMessage | undefined")]
    pub type JsContentMessage;
    /// `ContentLimits`.
    #[wasm_bindgen(typescript_type = "ContentLimits")]
    pub type JsContentLimits;
}

// ---------------------------------------------------------------------------
// Errors and JS <-> serde
// ---------------------------------------------------------------------------

pub(crate) fn content_err(e: inner::ContentError) -> JsError {
    JsError::new(&format!("{}: {e}", e.code()))
}

pub(crate) fn attachment_content_err(e: inner::AttachmentError) -> JsError {
    match e {
        inner::AttachmentError::Content(c) => content_err(c),
        other => JsError::new(&format!("attachment: {other}")),
    }
}

fn shape(e: impl std::fmt::Display) -> JsError {
    JsError::new(&format!("js.shape: {e}"))
}

pub(crate) fn from_js<T: for<'de> Deserialize<'de>>(v: &JsValue) -> Result<T, JsError> {
    let text = js_sys::JSON::stringify(v)
        .map_err(|_| shape("value does not stringify"))?
        .as_string()
        .ok_or_else(|| shape("value does not stringify"))?;
    serde_json::from_str(&text).map_err(shape)
}

pub(crate) fn to_js<T: Serialize>(v: &T) -> Result<JsValue, JsError> {
    let text = serde_json::to_string(v).map_err(shape)?;
    js_sys::JSON::parse(&text).map_err(|_| shape("value does not parse"))
}

fn arr<const N: usize>(b64: &str, field: &str) -> Result<[u8; N], JsError> {
    let v = B64.decode(b64).map_err(|_| {
        JsError::new(&format!(
            "{field}.badBase64: {field} is not standard base64"
        ))
    })?;
    bytes::<N>(&v, field)
}

fn bytes<const N: usize>(v: &[u8], field: &str) -> Result<[u8; N], JsError> {
    <[u8; N]>::try_from(v).map_err(|_| {
        JsError::new(&format!(
            "{field}.badLength: {field} must be {N} bytes, got {}",
            v.len()
        ))
    })
}

fn opt_sig(v: Option<&String>) -> Result<Option<[u8; 64]>, JsError> {
    v.map(|s| arr::<64>(s, "authorSig")).transpose()
}

fn b(v: &[u8]) -> String {
    B64.encode(v)
}

// ---------------------------------------------------------------------------
// The serde mirror
// ---------------------------------------------------------------------------

#[derive(Serialize, Deserialize, Clone)]
#[serde(rename_all = "camelCase")]
pub(crate) struct WPollQuiz {
    correct: u8,
    explanation: String,
}

#[derive(Serialize, Deserialize, Clone)]
#[serde(rename_all = "camelCase")]
pub(crate) struct WPollAnonymous {
    secret: String,
    ring0_hash: String,
}

#[derive(Serialize, Deserialize, Clone)]
#[serde(rename_all = "camelCase")]
pub(crate) struct WPollBody {
    id: String,
    question: String,
    options: Vec<String>,
    multiple: bool,
    #[serde(default)]
    quiz: Option<WPollQuiz>,
    #[serde(default)]
    close_at_ms: Option<i64>,
    #[serde(default)]
    anonymous: Option<WPollAnonymous>,
}

#[derive(Serialize, Deserialize, Clone)]
#[serde(rename_all = "camelCase")]
pub(crate) struct WSignedPoll {
    body: WPollBody,
    #[serde(default)]
    author_sig: Option<String>,
}

#[derive(Serialize, Deserialize, Clone)]
pub(crate) struct WChecklistItem {
    id: u32,
    text: String,
}

#[derive(Serialize, Deserialize, Clone)]
#[serde(rename_all = "camelCase")]
pub(crate) struct WChecklistBody {
    title: String,
    others_add: bool,
    others_mark: bool,
    items: Vec<WChecklistItem>,
}

#[derive(Serialize, Deserialize, Clone)]
#[serde(rename_all = "camelCase")]
pub(crate) struct WSignedChecklist {
    body: WChecklistBody,
    #[serde(default)]
    author_sig: Option<String>,
}

#[derive(Serialize, Deserialize, Clone, Copy)]
#[serde(
    tag = "kind",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
pub(crate) enum WItemRef {
    Author { id: u32 },
    Added { message_id: i64 },
}

#[derive(Serialize, Deserialize, Clone)]
#[serde(rename_all = "camelCase")]
pub(crate) struct WLocation {
    lat_e7: i32,
    lon_e7: i32,
    accuracy_m: u32,
    fix_at_ms: i64,
    #[serde(default)]
    name: Option<String>,
}

#[derive(Serialize, Deserialize, Clone)]
#[serde(rename_all = "camelCase")]
pub(crate) struct WContactCard {
    name: String,
    username: String,
    account_key: String,
}

#[derive(Serialize, Deserialize, Clone)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub(crate) enum WRun {
    Text {
        bold: bool,
        italic: bool,
        text: String,
    },
    Link {
        url: String,
        bold: bool,
        italic: bool,
        text: String,
    },
}

#[derive(Serialize, Deserialize, Clone, Copy)]
#[serde(rename_all = "camelCase")]
pub(crate) enum WHeading {
    H1,
    H2,
}

#[derive(Serialize, Deserialize, Clone)]
pub(crate) struct WListItem {
    depth: u8,
    runs: Vec<WRun>,
}

#[derive(Serialize, Deserialize, Clone)]
#[serde(
    tag = "kind",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
pub(crate) enum WBlock {
    Paragraph {
        runs: Vec<WRun>,
    },
    Heading {
        level: WHeading,
        runs: Vec<WRun>,
    },
    Quote {
        runs: Vec<WRun>,
    },
    List {
        ordered: bool,
        items: Vec<WListItem>,
    },
    Image {
        attachment_index: u8,
        caption: Vec<WRun>,
    },
    Divider,
}

#[derive(Serialize, Deserialize, Clone)]
pub(crate) struct WArticle {
    title: String,
    blocks: Vec<WBlock>,
}

#[derive(Serialize, Deserialize, Clone)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub(crate) enum WContent {
    Poll { poll: WSignedPoll },
    Checklist { checklist: WSignedChecklist },
    Location { location: WLocation },
    ContactCard { card: WContactCard },
    Article { article: WArticle },
}

// --- conversions ----------------------------------------------------------

impl WPollBody {
    fn to_core(&self) -> Result<poll::PollBody, JsError> {
        Ok(poll::PollBody {
            id: arr::<{ poll::POLL_ID_LEN }>(&self.id, "pollId")?,
            question: self.question.clone(),
            options: self.options.clone(),
            multiple: self.multiple,
            quiz: self.quiz.as_ref().map(|q| poll::PollQuiz {
                correct: q.correct,
                explanation: q.explanation.clone(),
            }),
            close_at_ms: self.close_at_ms,
            anonymous: self
                .anonymous
                .as_ref()
                .map(|a| -> Result<poll::PollAnonymous, JsError> {
                    Ok(poll::PollAnonymous {
                        secret: arr::<{ poll::POLL_SECRET_LEN }>(&a.secret, "pollSecret")?,
                        ring0_hash: arr::<{ poll::POLL_RING_HASH_LEN }>(
                            &a.ring0_hash,
                            "ring0Hash",
                        )?,
                    })
                })
                .transpose()?,
        })
    }

    fn of(p: &poll::PollBody) -> Self {
        Self {
            id: b(&p.id),
            question: p.question.clone(),
            options: p.options.clone(),
            multiple: p.multiple,
            quiz: p.quiz.as_ref().map(|q| WPollQuiz {
                correct: q.correct,
                explanation: q.explanation.clone(),
            }),
            close_at_ms: p.close_at_ms,
            anonymous: p.anonymous.as_ref().map(|a| WPollAnonymous {
                secret: b(&a.secret),
                ring0_hash: b(&a.ring0_hash),
            }),
        }
    }
}

impl WSignedPoll {
    pub(crate) fn to_core(&self) -> Result<inner::SignedPoll, JsError> {
        Ok(inner::SignedPoll {
            body: self.body.to_core()?,
            author_sig: opt_sig(self.author_sig.as_ref())?,
        })
    }

    pub(crate) fn of(p: &inner::SignedPoll) -> Self {
        Self {
            body: WPollBody::of(&p.body),
            author_sig: p.author_sig.map(|s| b(&s)),
        }
    }
}

impl WChecklistBody {
    pub(crate) fn to_core(&self) -> checklist::ChecklistBody {
        checklist::ChecklistBody {
            title: self.title.clone(),
            others_add: self.others_add,
            others_mark: self.others_mark,
            items: self
                .items
                .iter()
                .map(|i| checklist::ChecklistItem {
                    id: i.id,
                    text: i.text.clone(),
                })
                .collect(),
        }
    }

    pub(crate) fn of(c: &checklist::ChecklistBody) -> Self {
        Self {
            title: c.title.clone(),
            others_add: c.others_add,
            others_mark: c.others_mark,
            items: c
                .items
                .iter()
                .map(|i| WChecklistItem {
                    id: i.id,
                    text: i.text.clone(),
                })
                .collect(),
        }
    }
}

impl WSignedChecklist {
    pub(crate) fn to_core(&self) -> Result<inner::SignedChecklist, JsError> {
        Ok(inner::SignedChecklist {
            body: self.body.to_core(),
            author_sig: opt_sig(self.author_sig.as_ref())?,
        })
    }

    pub(crate) fn of(c: &inner::SignedChecklist) -> Self {
        Self {
            body: WChecklistBody::of(&c.body),
            author_sig: c.author_sig.map(|s| b(&s)),
        }
    }
}

impl From<WItemRef> for inner::ChecklistItemRef {
    fn from(r: WItemRef) -> Self {
        match r {
            WItemRef::Author { id } => Self::Author(id),
            WItemRef::Added { message_id } => Self::Added(message_id),
        }
    }
}

impl From<inner::ChecklistItemRef> for WItemRef {
    fn from(r: inner::ChecklistItemRef) -> Self {
        match r {
            inner::ChecklistItemRef::Author(id) => Self::Author { id },
            inner::ChecklistItemRef::Added(message_id) => Self::Added { message_id },
        }
    }
}

fn run_to_core(r: &WRun) -> article::Run {
    match r {
        WRun::Text { bold, italic, text } => article::Run::Text {
            marks: article::Marks {
                bold: *bold,
                italic: *italic,
            },
            text: text.clone(),
        },
        WRun::Link {
            url,
            bold,
            italic,
            text,
        } => article::Run::Link {
            url: url.clone(),
            marks: article::Marks {
                bold: *bold,
                italic: *italic,
            },
            text: text.clone(),
        },
    }
}

fn run_of(r: &article::Run) -> WRun {
    match r {
        article::Run::Text { marks, text } => WRun::Text {
            bold: marks.bold,
            italic: marks.italic,
            text: text.clone(),
        },
        article::Run::Link { url, marks, text } => WRun::Link {
            url: url.clone(),
            bold: marks.bold,
            italic: marks.italic,
            text: text.clone(),
        },
    }
}

fn runs_to_core(r: &[WRun]) -> Vec<article::Run> {
    r.iter().map(run_to_core).collect()
}

fn runs_of(r: &[article::Run]) -> Vec<WRun> {
    r.iter().map(run_of).collect()
}

impl WArticle {
    pub(crate) fn to_core(&self) -> inner::Article {
        inner::Article {
            title: self.title.clone(),
            blocks: self
                .blocks
                .iter()
                .map(|bl| match bl {
                    WBlock::Paragraph { runs } => article::Block::Paragraph(runs_to_core(runs)),
                    WBlock::Heading { level, runs } => article::Block::Heading {
                        level: match level {
                            WHeading::H1 => article::HeadingLevel::H1,
                            WHeading::H2 => article::HeadingLevel::H2,
                        },
                        runs: runs_to_core(runs),
                    },
                    WBlock::Quote { runs } => article::Block::Quote(runs_to_core(runs)),
                    WBlock::List { ordered, items } => article::Block::List {
                        ordered: *ordered,
                        items: items
                            .iter()
                            .map(|i| article::ListItem {
                                depth: i.depth,
                                runs: runs_to_core(&i.runs),
                            })
                            .collect(),
                    },
                    WBlock::Image {
                        attachment_index,
                        caption,
                    } => article::Block::Image {
                        attachment_index: *attachment_index,
                        caption: runs_to_core(caption),
                    },
                    WBlock::Divider => article::Block::Divider,
                })
                .collect(),
        }
    }

    pub(crate) fn of(a: &inner::Article) -> Self {
        Self {
            title: a.title.clone(),
            blocks: a
                .blocks
                .iter()
                .map(|bl| match bl {
                    article::Block::Paragraph(runs) => WBlock::Paragraph {
                        runs: runs_of(runs),
                    },
                    article::Block::Heading { level, runs } => WBlock::Heading {
                        level: match level {
                            article::HeadingLevel::H1 => WHeading::H1,
                            article::HeadingLevel::H2 => WHeading::H2,
                        },
                        runs: runs_of(runs),
                    },
                    article::Block::Quote(runs) => WBlock::Quote {
                        runs: runs_of(runs),
                    },
                    article::Block::List { ordered, items } => WBlock::List {
                        ordered: *ordered,
                        items: items
                            .iter()
                            .map(|i| WListItem {
                                depth: i.depth,
                                runs: runs_of(&i.runs),
                            })
                            .collect(),
                    },
                    article::Block::Image {
                        attachment_index,
                        caption,
                    } => WBlock::Image {
                        attachment_index: *attachment_index,
                        caption: runs_of(caption),
                    },
                    article::Block::Divider => WBlock::Divider,
                })
                .collect(),
        }
    }
}

impl WContent {
    pub(crate) fn to_core(&self) -> Result<inner::Content, JsError> {
        Ok(match self {
            Self::Poll { poll } => inner::Content::Poll(poll.to_core()?),
            Self::Checklist { checklist } => inner::Content::Checklist(checklist.to_core()?),
            Self::Location { location: l } => inner::Content::Location(inner::Location {
                lat_e7: l.lat_e7,
                lon_e7: l.lon_e7,
                accuracy_m: l.accuracy_m,
                fix_at_ms: l.fix_at_ms,
                name: l.name.clone(),
            }),
            Self::ContactCard { card } => inner::Content::ContactCard(inner::ContactCard {
                name: card.name.clone(),
                username: card.username.clone(),
                account_key: arr::<32>(&card.account_key, "accountKey")?,
            }),
            Self::Article { article } => inner::Content::Article(article.to_core()),
        })
    }

    pub(crate) fn of(c: &inner::Content) -> Self {
        match c {
            inner::Content::Poll(p) => Self::Poll {
                poll: WSignedPoll::of(p),
            },
            inner::Content::Checklist(c) => Self::Checklist {
                checklist: WSignedChecklist::of(c),
            },
            inner::Content::Location(l) => Self::Location {
                location: WLocation {
                    lat_e7: l.lat_e7,
                    lon_e7: l.lon_e7,
                    accuracy_m: l.accuracy_m,
                    fix_at_ms: l.fix_at_ms,
                    name: l.name.clone(),
                },
            },
            inner::Content::ContactCard(card) => Self::ContactCard {
                card: WContactCard {
                    name: card.name.clone(),
                    username: card.username.clone(),
                    account_key: b(&card.account_key),
                },
            },
            inner::Content::Article(a) => Self::Article {
                article: WArticle::of(a),
            },
        }
    }
}

/// The JS object of a core content value (used by the parsed body).
pub(crate) fn content_to_js(c: &inner::Content) -> JsValue {
    to_js(&WContent::of(c)).unwrap_or(JsValue::UNDEFINED)
}

// ---------------------------------------------------------------------------
// Poll
// ---------------------------------------------------------------------------

/// Sign a poll for the group `groupId` (32 bytes) with the author's 32-byte
/// account seed.
///
/// # Errors
///
/// A content error or a wrong length.
#[wasm_bindgen(js_name = signPoll)]
pub fn sign_poll(
    body: &JsPollBody,
    group_id: &[u8],
    author_seed: &[u8],
) -> Result<JsSignedPoll, JsError> {
    let body: WPollBody = from_js(body)?;
    let group = bytes::<32>(group_id, "groupId")?;
    let seed = bytes::<32>(author_seed, "authorSeed")?;
    let signed = inner::SignedPoll::sign(body.to_core()?, &group, &seed).map_err(content_err)?;
    Ok(to_js(&WSignedPoll::of(&signed))?.unchecked_into())
}

/// Check a poll's author signature against the sender's account key (C-68).
///
/// # Errors
///
/// `signatureMissing`, `signatureInvalid`, a bound or a wrong length.
#[wasm_bindgen(js_name = verifyPoll)]
pub fn verify_poll(poll: &JsSignedPoll, group_id: &[u8], author_key: &[u8]) -> Result<(), JsError> {
    let poll: WSignedPoll = from_js(poll)?;
    let group = bytes::<32>(group_id, "groupId")?;
    let key = bytes::<32>(author_key, "authorKey")?;
    poll.to_core()?.verify(&group, &key).map_err(content_err)
}

/// Whether `choices` is a valid vote for this poll.
///
/// # Errors
///
/// A shape error or a wrong length.
#[wasm_bindgen(js_name = pollChoicesValid)]
pub fn poll_choices_valid(body: &JsPollBody, choices: &[u8]) -> Result<bool, JsError> {
    let body: WPollBody = from_js(body)?;
    Ok(body.to_core()?.choices_valid(choices))
}

#[derive(Serialize, Deserialize, Clone)]
#[serde(rename_all = "camelCase")]
pub(crate) struct WVote {
    pub(crate) message_id: i64,
    pub(crate) created_at_ms: i64,
    pub(crate) voter: String,
    pub(crate) choices: Vec<u8>,
}

impl WVote {
    pub(crate) fn to_core(&self) -> Result<poll::PollVoteRecord, JsError> {
        Ok(poll::PollVoteRecord {
            message_id: self.message_id,
            created_at_ms: self.created_at_ms,
            voter: arr::<32>(&self.voter, "voter")?,
            choices: self.choices.clone(),
        })
    }

    pub(crate) fn of(v: &poll::PollVoteRecord) -> Self {
        Self {
            message_id: v.message_id,
            created_at_ms: v.created_at_ms,
            voter: b(&v.voter),
            choices: v.choices.clone(),
        }
    }
}

#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct WClose {
    message_id: i64,
    closer: String,
    closer_is_admin: bool,
}

#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct WVoterChoices {
    voter: String,
    choices: Vec<u8>,
}

#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct WTally {
    counts: Vec<u32>,
    voters: u32,
    choices_by_voter: Vec<WVoterChoices>,
    #[serde(default)]
    closed_by: Option<i64>,
    #[serde(default)]
    close_at_ms: Option<i64>,
}

impl WTally {
    fn to_core(&self) -> Result<poll::PollTally, JsError> {
        Ok(poll::PollTally {
            counts: self.counts.clone(),
            voters: self.voters,
            choices_by_voter: self
                .choices_by_voter
                .iter()
                .map(|v| Ok((arr::<32>(&v.voter, "voter")?, v.choices.clone())))
                .collect::<Result<_, JsError>>()?,
            closed_by: self.closed_by,
            close_at_ms: self.close_at_ms,
        })
    }
}

/// Fold a non-anonymous poll's votes and closes (ADR-0092 section 3.2).
///
/// # Errors
///
/// A shape error or a wrong length.
#[wasm_bindgen(js_name = foldPoll)]
pub fn fold_poll(
    body: &JsPollBody,
    author: &[u8],
    poll_message_id: i64,
    votes: &JsPollVoteRecords,
    closes: &JsPollCloseRecords,
) -> Result<JsPollTally, JsError> {
    let body: WPollBody = from_js(body)?;
    let body = body.to_core()?;
    let author = bytes::<32>(author, "author")?;
    let votes: Vec<WVote> = from_js(votes)?;
    let votes = votes
        .iter()
        .map(WVote::to_core)
        .collect::<Result<Vec<_>, _>>()?;
    let closes: Vec<WClose> = from_js(closes)?;
    let closes = closes
        .iter()
        .map(|c| {
            Ok(poll::PollCloseRecord {
                message_id: c.message_id,
                closer: arr::<32>(&c.closer, "closer")?,
                closer_is_admin: c.closer_is_admin,
            })
        })
        .collect::<Result<Vec<_>, JsError>>()?;
    let t = poll::fold(&body, &author, poll_message_id, &votes, &closes);
    let w = WTally {
        counts: t.counts,
        voters: t.voters,
        choices_by_voter: t
            .choices_by_voter
            .into_iter()
            .map(|(voter, choices)| WVoterChoices {
                voter: b(&voter),
                choices,
            })
            .collect(),
        closed_by: t.closed_by,
        close_at_ms: t.close_at_ms,
    };
    Ok(to_js(&w)?.unchecked_into())
}

/// Whether a folded poll is closed at `nowMs`.
///
/// # Errors
///
/// A shape error.
#[wasm_bindgen(js_name = pollTallyIsClosed)]
pub fn poll_tally_is_closed(tally: &JsPollTally, now_ms: i64) -> Result<bool, JsError> {
    let t: WTally = from_js(tally)?;
    Ok(t.to_core()?.is_closed(now_ms))
}

// ---------------------------------------------------------------------------
// Checklist
// ---------------------------------------------------------------------------

/// Sign a checklist for `groupId` with the author's account seed.
///
/// # Errors
///
/// A content error or a wrong length.
#[wasm_bindgen(js_name = signChecklist)]
pub fn sign_checklist(
    body: &JsChecklistBody,
    group_id: &[u8],
    author_seed: &[u8],
) -> Result<JsSignedChecklist, JsError> {
    let body: WChecklistBody = from_js(body)?;
    let group = bytes::<32>(group_id, "groupId")?;
    let seed = bytes::<32>(author_seed, "authorSeed")?;
    let signed =
        inner::SignedChecklist::sign(body.to_core(), &group, &seed).map_err(content_err)?;
    Ok(to_js(&WSignedChecklist::of(&signed))?.unchecked_into())
}

/// Check a checklist's author signature (C-68 key).
///
/// # Errors
///
/// `signatureMissing`, `signatureInvalid`, a bound or a wrong length.
#[wasm_bindgen(js_name = verifyChecklist)]
pub fn verify_checklist(
    checklist: &JsSignedChecklist,
    group_id: &[u8],
    author_key: &[u8],
) -> Result<(), JsError> {
    let c: WSignedChecklist = from_js(checklist)?;
    let group = bytes::<32>(group_id, "groupId")?;
    let key = bytes::<32>(author_key, "authorKey")?;
    c.to_core()?.verify(&group, &key).map_err(content_err)
}

#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct WEdit {
    message_id: i64,
    editor: String,
    body: WChecklistBody,
}

#[derive(Serialize, Deserialize, Clone)]
#[serde(rename_all = "camelCase")]
pub(crate) struct WTick {
    pub(crate) message_id: i64,
    pub(crate) ticker: String,
    pub(crate) privileged: bool,
    pub(crate) item: WItemRef,
    pub(crate) done: bool,
}

impl WTick {
    pub(crate) fn to_core(&self) -> Result<checklist::ChecklistTickRecord, JsError> {
        Ok(checklist::ChecklistTickRecord {
            message_id: self.message_id,
            ticker: arr::<32>(&self.ticker, "ticker")?,
            privileged: self.privileged,
            item: self.item.into(),
            done: self.done,
        })
    }

    pub(crate) fn of(t: &checklist::ChecklistTickRecord) -> Self {
        Self {
            message_id: t.message_id,
            ticker: b(&t.ticker),
            privileged: t.privileged,
            item: t.item.into(),
            done: t.done,
        }
    }
}

#[derive(Serialize, Deserialize, Clone)]
#[serde(rename_all = "camelCase")]
pub(crate) struct WAdd {
    pub(crate) message_id: i64,
    pub(crate) adder: String,
    pub(crate) privileged: bool,
    pub(crate) text: String,
}

impl WAdd {
    pub(crate) fn of(a: &checklist::ChecklistAddRecord) -> Self {
        Self {
            message_id: a.message_id,
            adder: b(&a.adder),
            privileged: a.privileged,
            text: a.text.clone(),
        }
    }
}

#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct WChecklistClose {
    message_id: i64,
    closer: String,
}

#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct WStateItem {
    item: WItemRef,
    text: String,
    #[serde(default)]
    added_by: Option<String>,
    #[serde(default)]
    done_by: Option<String>,
}

#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct WChecklistState {
    title: String,
    others_add: bool,
    others_mark: bool,
    items: Vec<WStateItem>,
    #[serde(default)]
    closed_by: Option<i64>,
}

impl WChecklistState {
    pub(crate) fn to_core(&self) -> Result<checklist::ChecklistState, JsError> {
        Ok(checklist::ChecklistState {
            title: self.title.clone(),
            others_add: self.others_add,
            others_mark: self.others_mark,
            items: self
                .items
                .iter()
                .map(|i| {
                    Ok(checklist::ChecklistStateItem {
                        item: i.item.into(),
                        text: i.text.clone(),
                        added_by: i
                            .added_by
                            .as_ref()
                            .map(|k| arr::<32>(k, "addedBy"))
                            .transpose()?,
                        done_by: i
                            .done_by
                            .as_ref()
                            .map(|k| arr::<32>(k, "doneBy"))
                            .transpose()?,
                    })
                })
                .collect::<Result<_, JsError>>()?,
            closed_by: self.closed_by,
        })
    }
}

/// Fold a checklist (ADR-0092 section 5).
///
/// # Errors
///
/// A shape error or a wrong length.
#[wasm_bindgen(js_name = foldChecklist)]
pub fn fold_checklist(
    body: &JsChecklistBody,
    author: &[u8],
    checklist_message_id: i64,
    edits: &JsChecklistEditRecords,
    adds: &JsChecklistAddRecords,
    ticks: &JsChecklistTickRecords,
    closes: &JsChecklistCloseRecords,
) -> Result<JsChecklistState, JsError> {
    let body: WChecklistBody = from_js(body)?;
    let author = bytes::<32>(author, "author")?;
    let edits: Vec<WEdit> = from_js(edits)?;
    let edits = edits
        .iter()
        .map(|e| {
            Ok(checklist::ChecklistEditRecord {
                message_id: e.message_id,
                editor: arr::<32>(&e.editor, "editor")?,
                body: e.body.to_core(),
            })
        })
        .collect::<Result<Vec<_>, JsError>>()?;
    let adds: Vec<WAdd> = from_js(adds)?;
    let adds = adds
        .iter()
        .map(|a| {
            Ok(checklist::ChecklistAddRecord {
                message_id: a.message_id,
                adder: arr::<32>(&a.adder, "adder")?,
                privileged: a.privileged,
                text: a.text.clone(),
            })
        })
        .collect::<Result<Vec<_>, JsError>>()?;
    let ticks: Vec<WTick> = from_js(ticks)?;
    let ticks = ticks
        .iter()
        .map(WTick::to_core)
        .collect::<Result<Vec<_>, _>>()?;
    let closes: Vec<WChecklistClose> = from_js(closes)?;
    let closes = closes
        .iter()
        .map(|c| {
            Ok(checklist::ChecklistCloseRecord {
                message_id: c.message_id,
                closer: arr::<32>(&c.closer, "closer")?,
            })
        })
        .collect::<Result<Vec<_>, JsError>>()?;
    let s = checklist::fold(
        &body.to_core(),
        &author,
        checklist_message_id,
        &edits,
        &adds,
        &ticks,
        &closes,
    );
    let w = WChecklistState {
        title: s.title,
        others_add: s.others_add,
        others_mark: s.others_mark,
        items: s
            .items
            .into_iter()
            .map(|i| WStateItem {
                item: i.item.into(),
                text: i.text,
                added_by: i.added_by.map(|k| b(&k)),
                done_by: i.done_by.map(|k| b(&k)),
            })
            .collect(),
        closed_by: s.closed_by,
    };
    Ok(to_js(&w)?.unchecked_into())
}

// ---------------------------------------------------------------------------
// Article
// ---------------------------------------------------------------------------

/// The bubble excerpt of an article.
///
/// # Errors
///
/// A shape error.
#[wasm_bindgen(js_name = articleExcerpt)]
pub fn article_excerpt(article: &JsArticle) -> Result<String, JsError> {
    let a: WArticle = from_js(article)?;
    Ok(a.to_core().excerpt())
}

/// Check an article's bounds (not its images).
///
/// # Errors
///
/// The first content refusal.
#[wasm_bindgen(js_name = validateArticle)]
pub fn validate_article(article: &JsArticle) -> Result<(), JsError> {
    let a: WArticle = from_js(article)?;
    a.to_core().validate().map_err(content_err)
}

/// Whether `url` is a link an article may carry.
#[wasm_bindgen(js_name = articleUrlValid)]
#[must_use]
pub fn article_url_valid(url: &str) -> bool {
    article::article_url_valid(url)
}

// ---------------------------------------------------------------------------
// Compose
// ---------------------------------------------------------------------------

#[derive(Deserialize, Default)]
#[serde(rename_all = "camelCase")]
struct WParts {
    #[serde(default)]
    reply_to: Option<i64>,
    #[serde(default)]
    reply_author: Option<String>,
    #[serde(default)]
    reply_excerpt: Option<String>,
    #[serde(default)]
    forwarded_from: Option<String>,
    #[serde(default)]
    forward_origin_group_b64: Option<String>,
    #[serde(default)]
    forward_origin_message_id: Option<i64>,
}

/// Build an ADR-0092 content message. `attachments` are an article's
/// images (empty for any other content); `parts` an optional reply /
/// forward.
///
/// # Errors
///
/// A content error, a wrong-length attachment field, or a shape error.
#[wasm_bindgen(js_name = composeContentEnvelope)]
pub fn compose_content_envelope(
    content: &JsMessageContent,
    attachments: Vec<crate::attachments::AttachmentRef>,
    parts: &JsContentEnvelopeParts,
) -> Result<Vec<u8>, JsError> {
    let content: WContent = from_js(content)?;
    let content = content.to_core()?;
    let parts_val: &JsValue = parts;
    let parts: WParts = if parts_val.is_undefined() || parts_val.is_null() {
        WParts::default()
    } else {
        from_js(parts_val)?
    };
    let reply = parts.reply_to.map(|target| inner::Reply {
        target,
        author: parts.reply_author,
        excerpt: parts.reply_excerpt,
    });
    let origin = match (
        parts.forward_origin_group_b64,
        parts.forward_origin_message_id,
    ) {
        (Some(group_id_b64), Some(message_id)) => Some(inner::ForwardOrigin {
            group_id_b64,
            message_id,
        }),
        _ => None,
    };
    let forward = (parts.forwarded_from.is_some() || origin.is_some()).then_some(inner::Forward {
        author: parts.forwarded_from,
        origin,
    });
    let refs = crate::attachments::refs_to_inner(&attachments)?;
    inner::compose_content(&content, refs, reply.as_ref(), forward.as_ref())
        .map_err(attachment_content_err)
}

/// A non-anonymous VOTE on the poll `target` (`bigint`); empty retracts.
///
/// # Errors
///
/// `choices.outOfRange`.
#[wasm_bindgen(js_name = composePollVoteEnvelope)]
pub fn compose_poll_vote_envelope(target: i64, choices: &[u8]) -> Result<Vec<u8>, JsError> {
    inner::compose_poll_vote(target, choices).map_err(attachment_content_err)
}

/// A CLOSE of the poll or checklist `target`; a channel poll's close carries
/// its frozen tally (`encodeChannelPollTally`), `undefined` elsewhere.
///
/// # Errors
///
/// `tally.outOfRange` for an empty or over-long tally.
#[wasm_bindgen(js_name = composePollCloseEnvelope)]
pub fn compose_poll_close_envelope(
    target: i64,
    tally: Option<Vec<u8>>,
) -> Result<Vec<u8>, JsError> {
    inner::compose_poll_close(target, tally.as_deref()).map_err(attachment_content_err)
}

/// A TICK of `item` in the checklist `target`.
///
/// # Errors
///
/// A shape error.
#[wasm_bindgen(js_name = composeChecklistTickEnvelope)]
pub fn compose_checklist_tick_envelope(
    target: i64,
    item: &JsChecklistItemRef,
    done: bool,
) -> Result<Vec<u8>, JsError> {
    let item: WItemRef = from_js(item)?;
    inner::compose_checklist_tick(target, item.into(), done).map_err(attachment_content_err)
}

/// An ADD-ITEM to the checklist `target`.
///
/// # Errors
///
/// `item.<reason>`.
#[wasm_bindgen(js_name = composeChecklistAddEnvelope)]
pub fn compose_checklist_add_envelope(target: i64, text: &str) -> Result<Vec<u8>, JsError> {
    inner::compose_checklist_add(target, text).map_err(attachment_content_err)
}

/// An EDIT of the checklist `target` (the author's, signed).
///
/// # Errors
///
/// A content error.
#[wasm_bindgen(js_name = composeChecklistEditEnvelope)]
pub fn compose_checklist_edit_envelope(
    target: i64,
    checklist: &JsSignedChecklist,
) -> Result<Vec<u8>, JsError> {
    let c: WSignedChecklist = from_js(checklist)?;
    inner::compose_checklist_edit(target, &c.to_core()?).map_err(attachment_content_err)
}

/// An EDIT of the article `target`, replacing it whole.
///
/// # Errors
///
/// A content error or a wrong-length attachment field.
#[wasm_bindgen(js_name = composeArticleEditEnvelope)]
pub fn compose_article_edit_envelope(
    target: i64,
    article: &JsArticle,
    attachments: Vec<crate::attachments::AttachmentRef>,
) -> Result<Vec<u8>, JsError> {
    let a: WArticle = from_js(article)?;
    let refs = crate::attachments::refs_to_inner(&attachments)?;
    inner::compose_article_edit(target, &a.to_core(), refs).map_err(attachment_content_err)
}

// ---------------------------------------------------------------------------
// State snapshot
// ---------------------------------------------------------------------------

#[derive(Serialize, Deserialize, Clone, Copy)]
#[serde(
    tag = "kind",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
pub(crate) enum WClosed {
    Open,
    ByTime,
    ByOp { message_id: i64 },
}

#[derive(Serialize, Deserialize, Clone)]
#[serde(rename_all = "camelCase")]
pub(crate) struct WAddedSnap {
    message_id: i64,
    text: String,
}

#[derive(Serialize, Deserialize, Clone)]
pub(crate) struct WDoneSnap {
    item: WItemRef,
    by: String,
}

#[derive(Serialize, Deserialize, Clone)]
#[serde(
    tag = "kind",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
pub(crate) enum WState {
    Poll {
        counts: Vec<u32>,
        voters: u32,
        closed: WClosed,
    },
    Checklist {
        #[serde(default)]
        closed_by: Option<i64>,
        added: Vec<WAddedSnap>,
        done: Vec<WDoneSnap>,
    },
}

impl WState {
    fn to_core(&self) -> Result<cs::ContentState, JsError> {
        Ok(match self {
            Self::Poll {
                counts,
                voters,
                closed,
            } => cs::ContentState::Poll(cs::PollState {
                counts: counts.clone(),
                voters: *voters,
                closed: match closed {
                    WClosed::Open => cs::PollClosed::Open,
                    WClosed::ByTime => cs::PollClosed::ByTime,
                    WClosed::ByOp { message_id } => cs::PollClosed::ByOp(*message_id),
                },
            }),
            Self::Checklist {
                closed_by,
                added,
                done,
            } => cs::ContentState::Checklist(cs::ChecklistSnapshot {
                closed_by: *closed_by,
                added: added
                    .iter()
                    .map(|a| cs::AddedItemSnapshot {
                        message_id: a.message_id,
                        text: a.text.clone(),
                    })
                    .collect(),
                done: done
                    .iter()
                    .map(|d| Ok((d.item.into(), arr::<32>(&d.by, "doneBy")?)))
                    .collect::<Result<_, JsError>>()?,
            }),
        })
    }

    fn of(s: &cs::ContentState) -> Self {
        match s {
            cs::ContentState::Poll(p) => Self::Poll {
                counts: p.counts.clone(),
                voters: p.voters,
                closed: match p.closed {
                    cs::PollClosed::Open => WClosed::Open,
                    cs::PollClosed::ByTime => WClosed::ByTime,
                    cs::PollClosed::ByOp(message_id) => WClosed::ByOp { message_id },
                },
            },
            cs::ContentState::Checklist(c) => Self::Checklist {
                closed_by: c.closed_by,
                added: c
                    .added
                    .iter()
                    .map(|a| WAddedSnap {
                        message_id: a.message_id,
                        text: a.text.clone(),
                    })
                    .collect(),
                done: c
                    .done
                    .iter()
                    .map(|(item, by)| WDoneSnap {
                        item: (*item).into(),
                        by: b(by),
                    })
                    .collect(),
            },
        }
    }
}

/// The saved state of a non-anonymous poll's tally at `nowMs`.
///
/// # Errors
///
/// A shape error or a wrong length.
#[wasm_bindgen(js_name = pollStateFromTally)]
pub fn poll_state_from_tally(tally: &JsPollTally, now_ms: i64) -> Result<JsContentState, JsError> {
    let t: WTally = from_js(tally)?;
    let s = cs::ContentState::Poll(cs::PollState::from_tally(&t.to_core()?, now_ms));
    Ok(to_js(&WState::of(&s))?.unchecked_into())
}

/// The saved state of an anonymous poll's ballot tally.
///
/// # Errors
///
/// A shape error.
#[wasm_bindgen(js_name = pollStateFromCounts)]
pub fn poll_state_from_counts(
    counts: Vec<u32>,
    voters: u32,
    closed: &JsPollClosedState,
) -> Result<JsContentState, JsError> {
    let closed: WClosed = from_js(closed)?;
    Ok(to_js(&WState::Poll {
        counts,
        voters,
        closed,
    })?
    .unchecked_into())
}

/// The saved state of a folded checklist.
///
/// # Errors
///
/// A shape error or a wrong length.
#[wasm_bindgen(js_name = checklistStateSnapshot)]
pub fn checklist_state_snapshot(state: &JsChecklistState) -> Result<JsContentState, JsError> {
    let s: WChecklistState = from_js(state)?;
    let snap = cs::ContentState::Checklist(cs::ChecklistSnapshot::from_state(&s.to_core()?));
    Ok(to_js(&WState::of(&snap))?.unchecked_into())
}

#[derive(Serialize, Deserialize)]
pub(crate) struct WContentMessage {
    content: WContent,
    #[serde(default)]
    state: Option<WState>,
}

/// `(kind, payload, state)`: what both record codecs carry.
pub(crate) type ContentTriple = (u8, Vec<u8>, Vec<u8>);

/// Read an optional `ContentMessage` argument into the triple both record
/// codecs carry. `undefined` / `null` gives `None`.
pub(crate) fn content_message_from_js(v: &JsValue) -> Result<Option<ContentTriple>, JsError> {
    if v.is_undefined() || v.is_null() {
        return Ok(None);
    }
    let m: WContentMessage = from_js(v)?;
    let content = m.content.to_core()?;
    let payload = content.to_payload().map_err(content_err)?;
    let state = match m.state {
        None => Vec::new(),
        Some(s) => {
            let s = s.to_core()?;
            if !s.matches(&content) {
                return Err(JsError::new(
                    "state.mismatch: the state does not belong to the content",
                ));
            }
            s.encode().map_err(content_err)?
        },
    };
    Ok(Some((content.kind_byte(), payload, state)))
}

/// Decode the triple into a `ContentMessage` object, or `undefined` when the
/// payload does not decode; a state that does not decode or belong is
/// dropped.
pub(crate) fn content_message_to_js(
    kind: u8,
    payload: &[u8],
    state: &[u8],
    attachment_mimes: &[Option<String>],
) -> JsValue {
    let refs: Vec<inner::AttachmentRef> = attachment_mimes
        .iter()
        .map(|mime| inner::AttachmentRef {
            blob_id: String::new(),
            key: String::new(),
            nonce: String::new(),
            mime: mime.clone(),
            size: 0,
            name: None,
            kind: None,
            duration_ms: None,
            waveform: None,
            width: None,
            height: None,
            thumb: None,
            title: None,
            artist: None,
            inline_thumb: None,
        })
        .collect();
    let Ok(content) = inner::Content::from_payload(kind, payload, &refs) else {
        return JsValue::UNDEFINED;
    };
    let state = if state.is_empty() {
        None
    } else {
        cs::ContentState::decode(state)
            .ok()
            .filter(|s| s.matches(&content))
            .map(|s| WState::of(&s))
    };
    to_js(&WContentMessage {
        content: WContent::of(&content),
        state,
    })
    .unwrap_or(JsValue::UNDEFINED)
}

// ---------------------------------------------------------------------------
// Limits
// ---------------------------------------------------------------------------

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct WLimits {
    max_poll_question_bytes: usize,
    min_poll_options: usize,
    max_poll_options: usize,
    max_poll_option_bytes: usize,
    max_poll_explanation_bytes: usize,
    max_checklist_title_bytes: usize,
    max_checklist_items: usize,
    max_checklist_item_bytes: usize,
    max_location_name_bytes: usize,
    max_article_title_bytes: usize,
    max_article_text_bytes: usize,
    max_article_blocks: usize,
    max_article_images: usize,
    max_article_url_bytes: usize,
    max_audio_tag_bytes: usize,
    max_audio_duration_ms: u32,
}

/// The ADR-0092 editor limits.
///
/// # Errors
///
/// Never in practice (a serialisation failure).
#[wasm_bindgen(js_name = contentLimits)]
pub fn content_limits() -> Result<JsContentLimits, JsError> {
    Ok(to_js(&WLimits {
        max_poll_question_bytes: poll::MAX_POLL_QUESTION_BYTES,
        min_poll_options: poll::MIN_POLL_OPTIONS,
        max_poll_options: poll::MAX_POLL_OPTIONS,
        max_poll_option_bytes: poll::MAX_POLL_OPTION_BYTES,
        max_poll_explanation_bytes: poll::MAX_POLL_EXPLANATION_BYTES,
        max_checklist_title_bytes: checklist::MAX_CHECKLIST_TITLE_BYTES,
        max_checklist_items: checklist::MAX_CHECKLIST_ITEMS,
        max_checklist_item_bytes: checklist::MAX_CHECKLIST_ITEM_BYTES,
        max_location_name_bytes: inner::location::MAX_LOCATION_NAME_BYTES,
        max_article_title_bytes: article::MAX_ARTICLE_TITLE_BYTES,
        max_article_text_bytes: article::MAX_ARTICLE_TEXT_BYTES,
        max_article_blocks: article::MAX_ARTICLE_BLOCKS,
        max_article_images: article::MAX_ARTICLE_IMAGES,
        max_article_url_bytes: article::MAX_ARTICLE_URL_BYTES,
        max_audio_tag_bytes: inner::MAX_AUDIO_TAG_BYTES,
        max_audio_duration_ms: inner::MAX_AUDIO_DURATION_MS,
    })?
    .unchecked_into())
}

// The record codecs' content bounds must equal the content codec's.
const _: () = assert!(
    lakko_mls::history_placeholder::MAX_CONTENT_PAYLOAD_BYTES == cs::MAX_CONTENT_PAYLOAD_BYTES
);
const _: () =
    assert!(lakko_mls::history_placeholder::MAX_CONTENT_STATE_BYTES == cs::MAX_CONTENT_STATE_BYTES);
const _: () =
    assert!(lakko_vault::record::MAX_CONTENT_PAYLOAD_BYTES == cs::MAX_CONTENT_PAYLOAD_BYTES);
const _: () = assert!(lakko_vault::record::MAX_CONTENT_STATE_BYTES == cs::MAX_CONTENT_STATE_BYTES);
const _: () = assert!(lakko_vault::record::MAX_AUDIO_TAG_BYTES == inner::MAX_AUDIO_TAG_BYTES);
const _: () = assert!(lakko_vault::record::MAX_INLINE_THUMB_BYTES == inner::MAX_INLINE_THUMB_BYTES);
const _: () = assert!(
    lakko_mls::history_placeholder::MAX_INLINE_THUMB_BYTES == inner::MAX_INLINE_THUMB_BYTES
);

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_js_shape_round_trips_a_signed_poll_and_an_article() {
        let body = poll::PollBody {
            id: [3; 16],
            question: "Lunch?".into(),
            options: vec!["Pizza".into(), "Soup".into()],
            multiple: false,
            quiz: None,
            close_at_ms: Some(5),
            anonymous: None,
        };
        let poll = inner::SignedPoll::sign(body, &[1; 32], &[7; 32]).unwrap();
        let w = WContent::of(&inner::Content::Poll(poll.clone()));
        let json = serde_json::to_string(&w).unwrap();
        assert!(json.contains("\"kind\":\"poll\""));
        assert!(json.contains("\"closeAtMs\":5"));
        let back: WContent = serde_json::from_str(&json).unwrap();
        let Ok(inner::Content::Poll(got)) = back.to_core() else {
            panic!("expected a poll");
        };
        assert_eq!(got, poll);

        let json = r#"{"kind":"article","article":{"title":"T","blocks":[
            {"kind":"heading","level":"h1","runs":[{"kind":"text","bold":true,"italic":false,"text":"Hi"}]},
            {"kind":"image","attachmentIndex":0,"caption":[]},
            {"kind":"divider"}]}}"#;
        let w: WContent = serde_json::from_str(json).unwrap();
        let Ok(inner::Content::Article(a)) = w.to_core() else {
            panic!("expected an article");
        };
        assert_eq!(a.blocks.len(), 3);
        assert_eq!(
            serde_json::to_value(WArticle::of(&a)).unwrap()["blocks"][1]["attachmentIndex"],
            0
        );
    }

    #[test]
    fn item_refs_and_states_use_the_documented_tags() {
        let r: WItemRef = serde_json::from_str(r#"{"kind":"added","messageId":12}"#).unwrap();
        assert_eq!(
            inner::ChecklistItemRef::from(r),
            inner::ChecklistItemRef::Added(12)
        );
        let s: WState = serde_json::from_str(
            r#"{"kind":"poll","counts":[1,0],"voters":1,"closed":{"kind":"byOp","messageId":9}}"#,
        )
        .unwrap();
        assert!(matches!(
            s.to_core().ok(),
            Some(cs::ContentState::Poll(cs::PollState {
                closed: cs::PollClosed::ByOp(9),
                ..
            }))
        ));
    }
}
