// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! Articles (ADR-0090 section 8): a closed, bounded, canonical block format.
//! Neither HTML nor Markdown; clients build native views from the decoded
//! tree and never produce an HTML string.
//!
//! ```text
//! article  = "lakko/article/v1\0" || 0x01 || title_len(u8) || title
//!            || block_count(u16 BE) || block*
//! block    = 0x01 paragraph || inlines
//!          | 0x02 heading   || level(u8: 1|2) || inlines
//!          | 0x03 quote     || inlines
//!          | 0x04 list      || ordered(u8: 0|1) || item_count(u16 BE)
//!                           || (depth(u8: 0|1) || inlines)*
//!          | 0x05 image     || attachment_index(u8) || inlines   // caption, may be empty
//!          | 0x06 divider
//! inlines  = run_count(u16 BE) || run*
//! run      = 0x01 text || marks(u8: bit0 bold, bit1 italic) || len(u16 BE) || utf8
//!          | 0x02 link || url_len(u16 BE) || url || marks(u8) || len(u16 BE) || utf8
//! ```
//!
//! Bounds: encoded at most [`MAX_ARTICLE_BYTES`]; run text in all at most
//! [`MAX_ARTICLE_TEXT_BYTES`]; at most [`MAX_ARTICLE_BLOCKS`] blocks, at most
//! [`MAX_RUNS_PER_BLOCK`] runs in one block (a list counts the runs of all
//! its items), at most [`MAX_ARTICLE_IMAGES`] image blocks with distinct
//! attachment indices. Canonical: no empty run, no two adjacent runs that
//! could be one (text runs with equal marks, link runs with equal URL and
//! marks), no empty paragraph, heading, quote or list item, a list's first
//! item at depth 0. The decoder refuses anything else, so an article
//! re-encodes to the same bytes.

use crate::content::{check_display_text, push_len_u16, push_len_u8, ContentError, Reader};
use crate::{AttachmentRef, InlineMediaType};

/// Domain separator that opens an article. Frozen.
pub const ARTICLE_DOMAIN: &[u8] = b"lakko/article/v1\0";
/// The only article version this build reads.
pub const ARTICLE_VERSION: u8 = 0x01;
/// Largest encoded article.
pub const MAX_ARTICLE_BYTES: usize = 131_072;
/// Most run text, in bytes, over the whole article (title not counted).
pub const MAX_ARTICLE_TEXT_BYTES: usize = 65_536;
/// Most blocks.
pub const MAX_ARTICLE_BLOCKS: usize = 2_048;
/// Most runs in one block.
pub const MAX_RUNS_PER_BLOCK: usize = 256;
/// Most image blocks.
pub const MAX_ARTICLE_IMAGES: usize = 20;
/// Longest title, bytes (at least 1).
pub const MAX_ARTICLE_TITLE_BYTES: usize = 255;
/// Longest link URL, bytes.
pub const MAX_ARTICLE_URL_BYTES: usize = 2_048;
/// Length of the excerpt [`Article::excerpt`] returns, in characters.
pub const ARTICLE_EXCERPT_CHARS: usize = 200;

const TAG_PARAGRAPH: u8 = 0x01;
const TAG_HEADING: u8 = 0x02;
const TAG_QUOTE: u8 = 0x03;
const TAG_LIST: u8 = 0x04;
const TAG_IMAGE: u8 = 0x05;
const TAG_DIVIDER: u8 = 0x06;
const RUN_TEXT: u8 = 0x01;
const RUN_LINK: u8 = 0x02;
const MARK_BOLD: u8 = 0x01;
const MARK_ITALIC: u8 = 0x02;
const HTTPS: &str = "https://";

/// Inline marks.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct Marks {
    /// Bold.
    pub bold: bool,
    /// Italic.
    pub italic: bool,
}

impl Marks {
    const fn byte(self) -> u8 {
        (if self.bold { MARK_BOLD } else { 0 }) | (if self.italic { MARK_ITALIC } else { 0 })
    }

    fn from_byte(b: u8) -> Result<Self, ContentError> {
        if b & !(MARK_BOLD | MARK_ITALIC) != 0 {
            return Err(ContentError::UnknownFlags);
        }
        Ok(Self {
            bold: b & MARK_BOLD != 0,
            italic: b & MARK_ITALIC != 0,
        })
    }
}

/// A run of inline text.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Run {
    /// Plain text with marks.
    Text {
        /// Marks.
        marks: Marks,
        /// The text, at least one byte.
        text: String,
    },
    /// A link: `https://` only.
    Link {
        /// The URL.
        url: String,
        /// Marks.
        marks: Marks,
        /// The visible text, at least one byte.
        text: String,
    },
}

impl Run {
    /// The visible text.
    #[must_use]
    pub fn text(&self) -> &str {
        match self {
            Self::Text { text, .. } | Self::Link { text, .. } => text,
        }
    }
}

/// Heading level.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HeadingLevel {
    /// Heading.
    H1,
    /// Subheading.
    H2,
}

/// One list item.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ListItem {
    /// 0 or 1; the first item of a list is 0.
    pub depth: u8,
    /// At least one run.
    pub runs: Vec<Run>,
}

/// A block.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Block {
    /// A paragraph, at least one run.
    Paragraph(Vec<Run>),
    /// A heading, at least one run.
    Heading {
        /// Level.
        level: HeadingLevel,
        /// Runs.
        runs: Vec<Run>,
    },
    /// A quote, at least one run.
    Quote(Vec<Run>),
    /// A list, at least one item.
    List {
        /// Numbered.
        ordered: bool,
        /// Items.
        items: Vec<ListItem>,
    },
    /// An image: an attachment of the same envelope, with a caption that may
    /// be empty.
    Image {
        /// Index into the envelope's attachments.
        attachment_index: u8,
        /// Caption runs, possibly none.
        caption: Vec<Run>,
    },
    /// A divider.
    Divider,
}

/// An article.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Article {
    /// 1..=[`MAX_ARTICLE_TITLE_BYTES`] bytes of display text.
    pub title: String,
    /// 1..=[`MAX_ARTICLE_BLOCKS`] blocks.
    pub blocks: Vec<Block>,
}

/// Whether `url` is a link the article format carries: lower-case
/// `https://`, and the whole URL one link under the message-link rule
/// ([`crate::message_links::is_whole_link`]: at most
/// [`MAX_ARTICLE_URL_BYTES`], no white space, control or format character,
/// a non-empty host naming no user, no trailing sentence punctuation).
#[must_use]
pub fn article_url_valid(url: &str) -> bool {
    url.starts_with(HTTPS) && crate::message_links::is_whole_link(url)
}

const _: () = assert!(MAX_ARTICLE_URL_BYTES == crate::message_links::MAX_LINK_BYTES);

struct Budget {
    text_bytes: usize,
    images: usize,
    image_indices: [bool; 256],
}

fn check_runs(
    runs: &[Run],
    may_be_empty: bool,
    budget: &mut Budget,
    block_runs: &mut usize,
) -> Result<(), ContentError> {
    if runs.is_empty() && !may_be_empty {
        return Err(ContentError::NotCanonical { what: "emptyBlock" });
    }
    *block_runs += runs.len();
    if *block_runs > MAX_RUNS_PER_BLOCK {
        return Err(ContentError::OutOfRange {
            field: "runsPerBlock",
        });
    }
    for (i, run) in runs.iter().enumerate() {
        let text = run.text();
        check_display_text(text, 1, usize::from(u16::MAX), "runText")?;
        budget.text_bytes += text.len();
        if budget.text_bytes > MAX_ARTICLE_TEXT_BYTES {
            return Err(ContentError::OutOfRange { field: "textBytes" });
        }
        if let Run::Link { url, .. } = run {
            if !article_url_valid(url) {
                return Err(ContentError::Malformed { what: "link" });
            }
        }
        if i > 0 {
            let mergeable = match (&runs[i - 1], run) {
                (Run::Text { marks: a, .. }, Run::Text { marks: b, .. }) => a == b,
                (
                    Run::Link {
                        url: ua, marks: a, ..
                    },
                    Run::Link {
                        url: ub, marks: b, ..
                    },
                ) => ua == ub && a == b,
                _ => false,
            };
            if mergeable {
                return Err(ContentError::NotCanonical { what: "runs" });
            }
        }
    }
    Ok(())
}

impl Article {
    /// Check every bound and canonical rule (not the attachments; see
    /// [`Self::check_images`]).
    ///
    /// # Errors
    ///
    /// The first [`ContentError`] that applies.
    pub fn validate(&self) -> Result<(), ContentError> {
        check_display_text(&self.title, 1, MAX_ARTICLE_TITLE_BYTES, "title")?;
        if self.blocks.is_empty() || self.blocks.len() > MAX_ARTICLE_BLOCKS {
            return Err(ContentError::OutOfRange {
                field: "blockCount",
            });
        }
        let mut budget = Budget {
            text_bytes: 0,
            images: 0,
            image_indices: [false; 256],
        };
        for block in &self.blocks {
            let mut block_runs = 0usize;
            match block {
                Block::Paragraph(runs) | Block::Quote(runs) | Block::Heading { runs, .. } => {
                    check_runs(runs, false, &mut budget, &mut block_runs)?;
                },
                Block::List { items, .. } => {
                    if items.is_empty() {
                        return Err(ContentError::NotCanonical { what: "emptyList" });
                    }
                    for (i, item) in items.iter().enumerate() {
                        if item.depth > 1 || (i == 0 && item.depth != 0) {
                            return Err(ContentError::OutOfRange { field: "listDepth" });
                        }
                        check_runs(&item.runs, false, &mut budget, &mut block_runs)?;
                    }
                },
                Block::Image {
                    attachment_index,
                    caption,
                } => {
                    budget.images += 1;
                    if budget.images > MAX_ARTICLE_IMAGES {
                        return Err(ContentError::OutOfRange { field: "images" });
                    }
                    let slot = &mut budget.image_indices[usize::from(*attachment_index)];
                    if *slot {
                        return Err(ContentError::NotCanonical { what: "imageIndex" });
                    }
                    *slot = true;
                    check_runs(caption, true, &mut budget, &mut block_runs)?;
                },
                Block::Divider => {},
            }
        }
        Ok(())
    }

    /// Check that every image block points at an attachment of the envelope
    /// whose MIME type the inline allowlist accepts as an image.
    ///
    /// # Errors
    ///
    /// [`ContentError::OutOfRange`] for an index past the attachments, or
    /// [`ContentError::Malformed`] for one that is not an image.
    pub fn check_images(&self, attachments: &[AttachmentRef]) -> Result<(), ContentError> {
        for block in &self.blocks {
            if let Block::Image {
                attachment_index, ..
            } = block
            {
                let att = attachments.get(usize::from(*attachment_index)).ok_or(
                    ContentError::OutOfRange {
                        field: "imageIndex",
                    },
                )?;
                if att.inline_media_type() != Some(InlineMediaType::Image) {
                    return Err(ContentError::Malformed { what: "image" });
                }
            }
        }
        Ok(())
    }

    /// The canonical encoding.
    ///
    /// # Errors
    ///
    /// What [`Self::validate`] refuses, or [`ContentError::TooLarge`].
    pub fn encode(&self) -> Result<Vec<u8>, ContentError> {
        self.validate()?;
        let mut out = Vec::new();
        out.extend_from_slice(ARTICLE_DOMAIN);
        out.push(ARTICLE_VERSION);
        push_len_u8(&mut out, self.title.len(), "title")?;
        out.extend_from_slice(self.title.as_bytes());
        push_len_u16(&mut out, self.blocks.len(), "blockCount")?;
        for block in &self.blocks {
            match block {
                Block::Paragraph(runs) => {
                    out.push(TAG_PARAGRAPH);
                    encode_runs(&mut out, runs)?;
                },
                Block::Heading { level, runs } => {
                    out.push(TAG_HEADING);
                    out.push(match level {
                        HeadingLevel::H1 => 1,
                        HeadingLevel::H2 => 2,
                    });
                    encode_runs(&mut out, runs)?;
                },
                Block::Quote(runs) => {
                    out.push(TAG_QUOTE);
                    encode_runs(&mut out, runs)?;
                },
                Block::List { ordered, items } => {
                    out.push(TAG_LIST);
                    out.push(u8::from(*ordered));
                    push_len_u16(&mut out, items.len(), "listItems")?;
                    for item in items {
                        out.push(item.depth);
                        encode_runs(&mut out, &item.runs)?;
                    }
                },
                Block::Image {
                    attachment_index,
                    caption,
                } => {
                    out.push(TAG_IMAGE);
                    out.push(*attachment_index);
                    encode_runs(&mut out, caption)?;
                },
                Block::Divider => out.push(TAG_DIVIDER),
            }
        }
        if out.len() > MAX_ARTICLE_BYTES {
            return Err(ContentError::TooLarge);
        }
        Ok(out)
    }

    /// Decode a canonical article.
    ///
    /// # Errors
    ///
    /// The first [`ContentError`] that applies.
    pub fn decode(bytes: &[u8]) -> Result<Self, ContentError> {
        if bytes.len() > MAX_ARTICLE_BYTES {
            return Err(ContentError::TooLarge);
        }
        let mut r = Reader::new(bytes);
        if r.take(ARTICLE_DOMAIN.len())? != ARTICLE_DOMAIN {
            return Err(ContentError::WrongDomain);
        }
        let version = r.u8()?;
        if version != ARTICLE_VERSION {
            return Err(ContentError::UnknownVersion(version));
        }
        let title_len = usize::from(r.u8()?);
        let title = r.str(title_len, "title")?.to_owned();
        let block_count = usize::from(r.u16()?);
        if block_count == 0 || block_count > MAX_ARTICLE_BLOCKS {
            return Err(ContentError::OutOfRange {
                field: "blockCount",
            });
        }
        let mut blocks = Vec::with_capacity(block_count);
        for _ in 0..block_count {
            let block = match r.u8()? {
                TAG_PARAGRAPH => Block::Paragraph(decode_runs(&mut r)?),
                TAG_HEADING => {
                    let level = match r.u8()? {
                        1 => HeadingLevel::H1,
                        2 => HeadingLevel::H2,
                        _ => {
                            return Err(ContentError::OutOfRange {
                                field: "headingLevel",
                            })
                        },
                    };
                    Block::Heading {
                        level,
                        runs: decode_runs(&mut r)?,
                    }
                },
                TAG_QUOTE => Block::Quote(decode_runs(&mut r)?),
                TAG_LIST => {
                    let ordered = match r.u8()? {
                        0 => false,
                        1 => true,
                        _ => {
                            return Err(ContentError::OutOfRange {
                                field: "listOrdered",
                            })
                        },
                    };
                    let count = usize::from(r.u16()?);
                    if count == 0 || count > MAX_RUNS_PER_BLOCK {
                        return Err(ContentError::OutOfRange { field: "listItems" });
                    }
                    let mut items = Vec::with_capacity(count);
                    for _ in 0..count {
                        let depth = r.u8()?;
                        items.push(ListItem {
                            depth,
                            runs: decode_runs(&mut r)?,
                        });
                    }
                    Block::List { ordered, items }
                },
                TAG_IMAGE => Block::Image {
                    attachment_index: r.u8()?,
                    caption: decode_runs(&mut r)?,
                },
                TAG_DIVIDER => Block::Divider,
                _ => return Err(ContentError::Malformed { what: "blockTag" }),
            };
            blocks.push(block);
        }
        r.finish()?;
        let article = Self { title, blocks };
        article.validate()?;
        Ok(article)
    }

    /// The bubble excerpt: the text of paragraphs, headings, quotes and list
    /// items in order, joined by one space, cut to the first
    /// [`ARTICLE_EXCERPT_CHARS`] characters. Image captions are not part of
    /// it.
    #[must_use]
    pub fn excerpt(&self) -> String {
        let mut out = String::new();
        let mut chars = 0usize;
        let mut push_block = |runs: &[Run], out: &mut String| {
            if chars >= ARTICLE_EXCERPT_CHARS {
                return;
            }
            if !out.is_empty() {
                out.push(' ');
                chars += 1;
            }
            for run in runs {
                for c in run.text().chars() {
                    if chars >= ARTICLE_EXCERPT_CHARS {
                        return;
                    }
                    out.push(c);
                    chars += 1;
                }
            }
        };
        for block in &self.blocks {
            match block {
                Block::Paragraph(runs) | Block::Quote(runs) | Block::Heading { runs, .. } => {
                    push_block(runs, &mut out);
                },
                Block::List { items, .. } => {
                    for item in items {
                        push_block(&item.runs, &mut out);
                    }
                },
                Block::Image { .. } | Block::Divider => {},
            }
        }
        out.trim_end().to_owned()
    }
}

fn encode_runs(out: &mut Vec<u8>, runs: &[Run]) -> Result<(), ContentError> {
    push_len_u16(out, runs.len(), "runCount")?;
    for run in runs {
        match run {
            Run::Text { marks, text } => {
                out.push(RUN_TEXT);
                out.push(marks.byte());
                push_len_u16(out, text.len(), "runText")?;
                out.extend_from_slice(text.as_bytes());
            },
            Run::Link { url, marks, text } => {
                out.push(RUN_LINK);
                push_len_u16(out, url.len(), "linkUrl")?;
                out.extend_from_slice(url.as_bytes());
                out.push(marks.byte());
                push_len_u16(out, text.len(), "runText")?;
                out.extend_from_slice(text.as_bytes());
            },
        }
    }
    Ok(())
}

fn decode_runs(r: &mut Reader<'_>) -> Result<Vec<Run>, ContentError> {
    let count = usize::from(r.u16()?);
    if count > MAX_RUNS_PER_BLOCK {
        return Err(ContentError::OutOfRange {
            field: "runsPerBlock",
        });
    }
    let mut runs = Vec::with_capacity(count);
    for _ in 0..count {
        let run = match r.u8()? {
            RUN_TEXT => {
                let marks = Marks::from_byte(r.u8()?)?;
                let len = usize::from(r.u16()?);
                Run::Text {
                    marks,
                    text: r.str(len, "runText")?.to_owned(),
                }
            },
            RUN_LINK => {
                let url_len = usize::from(r.u16()?);
                if url_len > MAX_ARTICLE_URL_BYTES {
                    return Err(ContentError::Malformed { what: "link" });
                }
                let url = r.str(url_len, "linkUrl")?.to_owned();
                let marks = Marks::from_byte(r.u8()?)?;
                let len = usize::from(r.u16()?);
                Run::Link {
                    url,
                    marks,
                    text: r.str(len, "runText")?.to_owned(),
                }
            },
            _ => return Err(ContentError::Malformed { what: "runTag" }),
        };
        runs.push(run);
    }
    Ok(runs)
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    fn text(t: &str) -> Run {
        Run::Text {
            marks: Marks::default(),
            text: t.into(),
        }
    }

    fn sample() -> Article {
        Article {
            title: "Notes".into(),
            blocks: vec![
                Block::Heading {
                    level: HeadingLevel::H1,
                    runs: vec![text("Intro")],
                },
                Block::Paragraph(vec![
                    text("Hello "),
                    Run::Text {
                        marks: Marks {
                            bold: true,
                            italic: false,
                        },
                        text: "world".into(),
                    },
                    Run::Link {
                        url: "https://lakko.me/x".into(),
                        marks: Marks::default(),
                        text: "link".into(),
                    },
                ]),
                Block::List {
                    ordered: true,
                    items: vec![
                        ListItem {
                            depth: 0,
                            runs: vec![text("one")],
                        },
                        ListItem {
                            depth: 1,
                            runs: vec![text("one.a")],
                        },
                    ],
                },
                Block::Image {
                    attachment_index: 0,
                    caption: vec![],
                },
                Block::Divider,
                Block::Quote(vec![text("q")]),
            ],
        }
    }

    fn any_marks() -> impl Strategy<Value = Marks> {
        (any::<bool>(), any::<bool>()).prop_map(|(bold, italic)| Marks { bold, italic })
    }

    fn any_run() -> impl Strategy<Value = Run> {
        prop_oneof![
            (any_marks(), "[a-zA-Z0-9 .,é日]{1,20}")
                .prop_map(|(marks, text)| Run::Text { marks, text }),
            (
                any_marks(),
                "[a-z]{1,10}",
                "[a-z0-9/]{0,10}",
                "[a-zA-Z ]{1,10}"
            )
                .prop_map(|(marks, host, path, text)| Run::Link {
                    url: format!("https://{host}.example/{path}"),
                    marks,
                    text
                }),
        ]
    }

    /// Merge adjacent runs that the canonical form forbids side by side.
    fn canonical_runs(runs: Vec<Run>) -> Vec<Run> {
        let mut out: Vec<Run> = Vec::new();
        for run in runs {
            let merged = match (out.last_mut(), &run) {
                (Some(Run::Text { marks: a, text: ta }), Run::Text { marks: b, text: tb })
                    if a == b =>
                {
                    ta.push_str(tb);
                    true
                },
                (
                    Some(Run::Link {
                        url: ua,
                        marks: a,
                        text: ta,
                    }),
                    Run::Link {
                        url: ub,
                        marks: b,
                        text: tb,
                    },
                ) if ua == ub && a == b => {
                    ta.push_str(tb);
                    true
                },
                _ => false,
            };
            if !merged {
                out.push(run);
            }
        }
        out
    }

    fn any_runs(min: usize) -> impl Strategy<Value = Vec<Run>> {
        proptest::collection::vec(any_run(), min..6).prop_map(canonical_runs)
    }

    fn any_block() -> impl Strategy<Value = Block> {
        prop_oneof![
            any_runs(1).prop_map(Block::Paragraph),
            (any::<bool>(), any_runs(1)).prop_map(|(h1, runs)| Block::Heading {
                level: if h1 {
                    HeadingLevel::H1
                } else {
                    HeadingLevel::H2
                },
                runs
            }),
            any_runs(1).prop_map(Block::Quote),
            (
                any::<bool>(),
                proptest::collection::vec((0u8..2, any_runs(1)), 1..5)
            )
                .prop_map(|(ordered, items)| Block::List {
                    ordered,
                    items: items
                        .into_iter()
                        .enumerate()
                        .map(|(i, (d, runs))| ListItem {
                            depth: if i == 0 { 0 } else { d },
                            runs
                        })
                        .collect()
                }),
            Just(Block::Divider),
        ]
    }

    prop_compose! {
        fn any_article()(
            title in "[A-Za-z0-9 ]{1,40}",
            blocks in proptest::collection::vec(any_block(), 1..12),
            images in proptest::collection::btree_set(any::<u8>(), 0..4),
        ) -> Article {
            let mut blocks = blocks;
            for i in images {
                blocks.push(Block::Image { attachment_index: i, caption: vec![] });
            }
            Article { title, blocks }
        }
    }

    proptest! {
        #[test]
        fn round_trip_and_canonical(a in any_article()) {
            let bytes = a.encode().unwrap();
            prop_assert!(bytes.len() <= MAX_ARTICLE_BYTES);
            let back = Article::decode(&bytes).unwrap();
            prop_assert_eq!(&back, &a);
            prop_assert_eq!(back.encode().unwrap(), bytes);
            prop_assert!(a.excerpt().chars().count() <= ARTICLE_EXCERPT_CHARS);
        }

        #[test]
        fn hostile_bytes_never_panic(bytes in proptest::collection::vec(any::<u8>(), 0..3000), prefix in any::<bool>()) {
            let mut input = if prefix { ARTICLE_DOMAIN.to_vec() } else { Vec::new() };
            if prefix { input.push(ARTICLE_VERSION); }
            input.extend_from_slice(&bytes);
            if let Ok(a) = Article::decode(&input) {
                prop_assert_eq!(a.encode().unwrap(), input);
            }
        }

        #[test]
        fn a_flipped_byte_never_yields_a_non_canonical_article(a in any_article(), pos in any::<usize>(), byte in any::<u8>()) {
            let mut bytes = a.encode().unwrap();
            let i = pos % bytes.len();
            bytes[i] = byte;
            if let Ok(b) = Article::decode(&bytes) {
                prop_assert_eq!(b.encode().unwrap(), bytes);
            }
        }
    }

    #[test]
    fn the_sample_round_trips_and_has_an_excerpt() {
        let a = sample();
        let bytes = a.encode().unwrap();
        assert_eq!(Article::decode(&bytes).unwrap(), a);
        assert_eq!(a.excerpt(), "Intro Hello worldlink one one.a q");
    }

    #[test]
    fn non_canonical_and_out_of_bound_forms_are_refused() {
        let mut a = sample();
        a.blocks[1] = Block::Paragraph(vec![text("a"), text("b")]);
        assert!(a.encode().is_err());
        let mut a = sample();
        a.blocks[1] = Block::Paragraph(vec![]);
        assert!(a.encode().is_err());
        let mut a = sample();
        a.blocks[1] = Block::Paragraph(vec![text("")]);
        assert!(a.encode().is_err());
        let mut a = sample();
        a.blocks[1] = Block::Paragraph(vec![text("a\nb")]);
        assert!(a.encode().is_err());
        let mut a = sample();
        a.blocks.push(Block::Image {
            attachment_index: 0,
            caption: vec![],
        });
        assert!(a.encode().is_err(), "a repeated image index");
        let mut a = sample();
        a.blocks[2] = Block::List {
            ordered: false,
            items: vec![ListItem {
                depth: 1,
                runs: vec![text("x")],
            }],
        };
        assert!(a.encode().is_err(), "first item nested");
        let mut a = sample();
        a.blocks.clear();
        assert!(a.encode().is_err());
        let mut a = sample();
        a.blocks = (0..=MAX_ARTICLE_IMAGES)
            .map(|i| Block::Image {
                attachment_index: u8::try_from(i).unwrap(),
                caption: vec![],
            })
            .collect();
        assert!(a.encode().is_err(), "too many images");
        let mut a = sample();
        a.blocks = vec![Block::Paragraph(vec![text(&"x".repeat(40_000))]); 2];
        assert!(a.encode().is_err(), "too much text");
    }

    #[test]
    fn links_are_https_only_with_a_clean_host() {
        assert!(article_url_valid("https://lakko.me/a?b#c"));
        assert!(!article_url_valid("http://lakko.me"));
        assert!(!article_url_valid("HTTPS://lakko.me"));
        assert!(!article_url_valid("https://"));
        assert!(!article_url_valid("https://user@evil.example"));
        assert!(!article_url_valid("https://a b"));
        assert!(!article_url_valid("javascript:alert(1)"));
        assert!(!article_url_valid(&format!(
            "https://a.example/{}",
            "x".repeat(2100)
        )));
    }

    #[test]
    fn decode_refuses_unknown_tags_marks_and_trailing_bytes() {
        let bytes = sample().encode().unwrap();
        let mut longer = bytes.clone();
        longer.push(0);
        assert_eq!(Article::decode(&longer), Err(ContentError::TrailingBytes));
        let mut wrong_version = bytes.clone();
        wrong_version[ARTICLE_DOMAIN.len()] = 2;
        assert_eq!(
            Article::decode(&wrong_version),
            Err(ContentError::UnknownVersion(2))
        );
        // A text run with an unknown mark bit.
        let a = Article {
            title: "t".into(),
            blocks: vec![Block::Paragraph(vec![text("x")])],
        };
        let mut b = a.encode().unwrap();
        // domain, version, title_len, title, block_count(2), tag, run_count(2), run tag, marks
        let marks_at = ARTICLE_DOMAIN.len() + 1 + 1 + 1 + 2 + 1 + 2 + 1;
        b[marks_at] = 0x04;
        assert_eq!(Article::decode(&b), Err(ContentError::UnknownFlags));
    }
}
