// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! How the photos and videos of one message are laid out in its bubble
//! (`docs/ux/media-messages.md` section 11, and section 10 for one item).
//!
//! The clients draw the tiles; this rule decides where each one goes, once,
//! so web, iOS and Android place the same message the same way. Integer
//! arithmetic only: every client rounds alike.
//!
//! Coordinates are measured from the START edge of the mosaic (the left
//! edge in a left-to-right layout, the right edge in a right-to-left one).
//! A right-to-left client places a tile at `mosaic.width - x - width`.
//! Every tile is drawn cropped to fill its rectangle.
//!
//! Every number below is chosen, not measured.

/// The most photos and videos one message's mosaic shows. A sender cannot
/// attach more (section 11, limits); a received message carrying more shows
/// the first this many, in the order sent, and nothing of the rest.
pub const MOSAIC_MAX_ITEMS: usize = 10;

/// The gap between tiles the spec draws, in px / pt / dp.
pub const MOSAIC_GAP: u32 = 2;

/// The tallest a single photo or video is drawn (section 10 box).
pub const SINGLE_MAX_HEIGHT: u32 = 360;

/// The tallest two side-by-side tiles are drawn.
pub const PAIR_MAX_HEIGHT: u32 = 260;

/// The tallest the three-item layout (one large, two stacked) is drawn.
pub const TRIPLE_MAX_HEIGHT: u32 = 300;

/// The lowest a row of tiles, a single tile, or one of the two stacked
/// tiles of the three-item layout is drawn.
pub const ROW_MIN_HEIGHT: u32 = 80;

/// The tallest a row is drawn in the four-to-ten-item layouts.
pub const ROW_MAX_HEIGHT: u32 = 200;

/// The pixel size of one photo or video, as its attachment reference
/// carries it. `0` in either field means unknown; the item is then laid out
/// as 4:3 landscape. A caller with no `width` / `height` but a thumbnail of
/// known size passes the thumbnail's size (section 10).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct MediaSize {
    /// Width in pixels; `0` when unknown.
    pub width: u32,
    /// Height in pixels; `0` when unknown.
    pub height: u32,
}

/// Where one tile goes, from the top-start corner of the mosaic.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MosaicTile {
    /// Distance from the start edge.
    pub x: u32,
    /// Distance from the top edge.
    pub y: u32,
    /// Tile width.
    pub width: u32,
    /// Tile height.
    pub height: u32,
}

/// The laid-out mosaic.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct MediaMosaic {
    /// One tile per item, in input order, for the first
    /// [`MOSAIC_MAX_ITEMS`] items. Empty when there is nothing to lay out
    /// or the width cannot hold every tile.
    pub tiles: Vec<MosaicTile>,
    /// Width of the mosaic: the bubble width, or less for a single narrow
    /// item.
    pub width: u32,
    /// Total height of the mosaic, gaps included.
    pub height: u32,
}

/// The rows of the four-to-ten-item layouts, top to bottom (section 11).
const fn rows_for(count: usize) -> &'static [usize] {
    match count {
        4 => &[2, 2],
        5 => &[2, 3],
        6 => &[3, 3],
        7 => &[2, 2, 3],
        8 => &[3, 3, 2],
        9 => &[3, 3, 3],
        _ => &[3, 2, 2, 3],
    }
}

/// The item's height when drawn `width` wide with its aspect kept; 4:3
/// landscape when the size is unknown.
fn height_at(width: u32, size: MediaSize) -> u64 {
    if size.width == 0 || size.height == 0 {
        u64::from(width) * 3 / 4
    } else {
        u64::from(width) * u64::from(size.height) / u64::from(size.width)
    }
}

/// `value` clamped to `[lo, hi]`, where `lo` wins when the two cross.
fn clamp(value: u64, lo: u32, hi: u32) -> u32 {
    let capped = value.min(u64::from(hi));
    // `capped <= hi <= u32::MAX`, so the conversion cannot fail.
    u32::try_from(capped).unwrap_or(hi).max(lo)
}

/// Split `total` into `parts` widths separated by `gap`, the first
/// `remainder` tiles one pixel wider, so the row fills `total` exactly.
/// `None` when a tile would be zero wide.
fn split_row(total: u32, gap: u32, parts: u32) -> Option<Vec<(u32, u32)>> {
    let gaps = u64::from(gap) * u64::from(parts.saturating_sub(1));
    let avail = u64::from(total).checked_sub(gaps)?;
    let parts64 = u64::from(parts);
    let base = avail / parts64;
    if base == 0 {
        return None;
    }
    let extra = avail % parts64;
    let mut out = Vec::with_capacity(parts as usize);
    let mut x = 0u64;
    for i in 0..parts64 {
        let w = base + u64::from(i < extra);
        out.push((u32::try_from(x).ok()?, u32::try_from(w).ok()?));
        x += w + u64::from(gap);
    }
    Some(out)
}

fn single(width: u32, size: MediaSize) -> MediaMosaic {
    let min_side = ROW_MIN_HEIGHT.min(width);
    let natural = height_at(width, size);
    let (w, h) = if natural > u64::from(SINGLE_MAX_HEIGHT) {
        // Too tall at the full width: fit the height, narrow the width.
        let w = if size.width == 0 || size.height == 0 {
            u64::from(SINGLE_MAX_HEIGHT) * 4 / 3
        } else {
            u64::from(SINGLE_MAX_HEIGHT) * u64::from(size.width) / u64::from(size.height)
        };
        (clamp(w, min_side, width), SINGLE_MAX_HEIGHT)
    } else {
        (width, clamp(natural, ROW_MIN_HEIGHT, SINGLE_MAX_HEIGHT))
    };
    MediaMosaic {
        tiles: vec![MosaicTile {
            x: 0,
            y: 0,
            width: w,
            height: h,
        }],
        width: w,
        height: h,
    }
}

fn triple(width: u32, gap: u32, items: &[MediaSize]) -> Option<MediaMosaic> {
    let avail = width.checked_sub(gap)?;
    let large = avail * 2 / 3;
    let side = avail - large;
    if large == 0 || side == 0 {
        return None;
    }
    let lo = ROW_MIN_HEIGHT.saturating_mul(2).saturating_add(gap);
    let height = clamp(height_at(large, items[0]), lo, TRIPLE_MAX_HEIGHT);
    let top = (height - gap) / 2;
    let bottom = height - gap - top;
    let side_x = large + gap;
    Some(MediaMosaic {
        tiles: vec![
            MosaicTile {
                x: 0,
                y: 0,
                width: large,
                height,
            },
            MosaicTile {
                x: side_x,
                y: 0,
                width: side,
                height: top,
            },
            MosaicTile {
                x: side_x,
                y: top + gap,
                width: side,
                height: bottom,
            },
        ],
        width,
        height,
    })
}

fn rows(width: u32, gap: u32, items: &[MediaSize], counts: &[usize]) -> Option<MediaMosaic> {
    let max_height = if items.len() == 2 {
        PAIR_MAX_HEIGHT
    } else {
        ROW_MAX_HEIGHT
    };
    let mut tiles = Vec::with_capacity(items.len());
    let mut y = 0u32;
    let mut next = 0usize;
    for (row_index, &count) in counts.iter().enumerate() {
        let row_items = items.get(next..next + count)?;
        next += count;
        let cells = split_row(width, gap, u32::try_from(count).ok()?)?;
        // The row is as tall as its most landscape item at its own width.
        let natural = row_items
            .iter()
            .zip(&cells)
            .map(|(&size, &(_, w))| height_at(w, size))
            .min()?;
        let row_height = clamp(natural, ROW_MIN_HEIGHT, max_height);
        if row_index > 0 {
            y = y.checked_add(gap)?;
        }
        for &(x, w) in &cells {
            tiles.push(MosaicTile {
                x,
                y,
                width: w,
                height: row_height,
            });
        }
        y = y.checked_add(row_height)?;
    }
    Some(MediaMosaic {
        tiles,
        width,
        height: y,
    })
}

/// Lay out the photos and videos of one message in a bubble `bubble_width`
/// wide with `gap` between tiles.
///
/// - 1 item: its own aspect, at most `bubble_width` wide and
///   [`SINGLE_MAX_HEIGHT`] high; never lower than [`ROW_MIN_HEIGHT`] nor
///   narrower than `min(ROW_MIN_HEIGHT, bubble_width)`, cropping an extreme
///   aspect to that.
/// - 2: side by side, equal width, as tall as the more landscape one at
///   that width, within `[ROW_MIN_HEIGHT, PAIR_MAX_HEIGHT]`.
/// - 3: the first large on the start side, two thirds of the width less the
///   gap, as tall as it is at that width within
///   `[2 * ROW_MIN_HEIGHT + gap, TRIPLE_MAX_HEIGHT]`; the other two stacked
///   on the end side, splitting that height.
/// - 4 to 10: rows of 2 and 3 (4 = 2+2, 5 = 2+3, 6 = 3+3, 7 = 2+2+3,
///   8 = 3+3+2, 9 = 3+3+3, 10 = 3+2+2+3), equal widths in a row, each row as
///   tall as its most landscape item at its width, within
///   `[ROW_MIN_HEIGHT, ROW_MAX_HEIGHT]`.
/// - More than [`MOSAIC_MAX_ITEMS`]: the first ten are laid out.
///
/// Every row fills the width exactly: when the width does not divide
/// evenly, the first tiles of the row are one pixel wider. Returns an empty
/// mosaic when there are no items, `bubble_width` is 0, or the width cannot
/// give every tile at least one pixel.
#[must_use]
pub fn media_mosaic(bubble_width: u32, gap: u32, items: &[MediaSize]) -> MediaMosaic {
    let items = &items[..items.len().min(MOSAIC_MAX_ITEMS)];
    if bubble_width == 0 {
        return MediaMosaic::default();
    }
    let laid = match items.len() {
        0 => None,
        1 => Some(single(bubble_width, items[0])),
        2 => rows(bubble_width, gap, items, &[2]),
        3 => triple(bubble_width, gap, items),
        n => rows(bubble_width, gap, items, rows_for(n)),
    };
    laid.unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    const fn size(width: u32, height: u32) -> MediaSize {
        MediaSize { width, height }
    }

    const fn tile(x: u32, y: u32, width: u32, height: u32) -> MosaicTile {
        MosaicTile {
            x,
            y,
            width,
            height,
        }
    }

    #[test]
    fn one_landscape_photo_fills_the_width() {
        let m = media_mosaic(320, 2, &[size(4000, 3000)]);
        assert_eq!(m.tiles, vec![tile(0, 0, 320, 240)]);
        assert_eq!((m.width, m.height), (320, 240));
    }

    #[test]
    fn one_tall_photo_is_capped_at_the_box_height() {
        let m = media_mosaic(260, 2, &[size(1080, 1920)]);
        // 360 * 1080 / 1920 = 202.
        assert_eq!(m.tiles, vec![tile(0, 0, 202, 360)]);
        assert_eq!(m.width, 202);
    }

    #[test]
    fn one_unknown_photo_is_a_four_by_three_placeholder() {
        let m = media_mosaic(260, 2, &[size(0, 0)]);
        assert_eq!(m.tiles, vec![tile(0, 0, 260, 195)]);
        let half = media_mosaic(320, 2, &[size(0, 500)]);
        assert_eq!(half.tiles, vec![tile(0, 0, 320, 240)]);
    }

    #[test]
    fn one_extreme_aspect_is_kept_within_the_minimum_sides() {
        let wide = media_mosaic(320, 2, &[size(4096, 1)]);
        assert_eq!(wide.tiles, vec![tile(0, 0, 320, ROW_MIN_HEIGHT)]);
        let tall = media_mosaic(320, 2, &[size(1, 4096)]);
        assert_eq!(tall.tiles, vec![tile(0, 0, ROW_MIN_HEIGHT, 360)]);
    }

    #[test]
    fn two_sit_side_by_side_as_tall_as_the_more_landscape_one() {
        let m = media_mosaic(320, 2, &[size(1000, 1000), size(1600, 900)]);
        // Each tile is 159 wide; 159 * 900 / 1600 = 89.
        assert_eq!(m.tiles, vec![tile(0, 0, 159, 89), tile(161, 0, 159, 89)]);
        assert_eq!(m.height, 89);
        let portraits = media_mosaic(320, 2, &[size(900, 1600), size(900, 1600)]);
        assert_eq!(portraits.height, PAIR_MAX_HEIGHT);
    }

    #[test]
    fn three_put_one_large_on_the_start_side() {
        let m = media_mosaic(320, 2, &[size(1000, 1000), size(1, 1), size(1, 1)]);
        // avail 318: large 212, side 106; height 212 (square at 212).
        assert_eq!(
            m.tiles,
            vec![
                tile(0, 0, 212, 212),
                tile(214, 0, 106, 105),
                tile(214, 107, 106, 105),
            ]
        );
        assert_eq!(m.height, 212);
    }

    #[test]
    fn the_rows_follow_the_spec_table() {
        let expected: [&[usize]; 7] = [
            &[2, 2],
            &[2, 3],
            &[3, 3],
            &[2, 2, 3],
            &[3, 3, 2],
            &[3, 3, 3],
            &[3, 2, 2, 3],
        ];
        for (n, rows) in (4..=10).zip(expected) {
            let m = media_mosaic(320, 2, &vec![size(4, 3); n]);
            let mut ys: Vec<u32> = m.tiles.iter().map(|t| t.y).collect();
            ys.dedup();
            let per_row: Vec<usize> = ys
                .iter()
                .map(|y| m.tiles.iter().filter(|t| t.y == *y).count())
                .collect();
            assert_eq!(per_row, rows, "{n} items");
        }
    }

    #[test]
    fn more_than_ten_lays_out_the_first_ten() {
        let m = media_mosaic(320, 2, &[size(4, 3); 14]);
        assert_eq!(m.tiles.len(), MOSAIC_MAX_ITEMS);
        assert_eq!(m, media_mosaic(320, 2, &[size(4, 3); 10]));
    }

    #[test]
    fn nothing_to_lay_out_is_empty() {
        assert_eq!(media_mosaic(320, 2, &[]), MediaMosaic::default());
        assert_eq!(media_mosaic(0, 2, &[size(4, 3)]), MediaMosaic::default());
        // Three tiles and two gaps of 10 do not fit in 22.
        assert_eq!(
            media_mosaic(22, 10, &[size(4, 3); 6]),
            MediaMosaic::default()
        );
    }

    #[test]
    fn row_heights_stay_in_range() {
        let m = media_mosaic(
            320,
            2,
            &[size(1, 4096), size(1, 4096), size(1, 4096), size(1, 4096)],
        );
        assert!(m.tiles.iter().all(|t| t.height == ROW_MAX_HEIGHT));
        let m = media_mosaic(320, 2, &[size(4096, 1), size(1, 1), size(1, 1), size(1, 1)]);
        assert_eq!(m.tiles[0].height, ROW_MIN_HEIGHT);
    }

    fn separated(a: &MosaicTile, b: &MosaicTile, gap: u32) -> bool {
        let (ax, ay, aw, ah) = (
            u64::from(a.x),
            u64::from(a.y),
            u64::from(a.width),
            u64::from(a.height),
        );
        let (bx, by, bw, bh) = (
            u64::from(b.x),
            u64::from(b.y),
            u64::from(b.width),
            u64::from(b.height),
        );
        let g = u64::from(gap);
        ax + aw + g <= bx || bx + bw + g <= ax || ay + ah + g <= by || by + bh + g <= ay
    }

    const fn overlap(a0: u32, a_len: u32, b0: u32, b_len: u32) -> bool {
        a0 < b0 + b_len && b0 < a0 + a_len
    }

    /// Every side edge of a tile is the mosaic edge or exactly one gap from
    /// a tile beside it in the same band, and every top and bottom edge is
    /// the mosaic edge or exactly one gap from a row above or below (rows of
    /// different column counts meet across a gap column, so the tile below
    /// need not overlap in x). With [`separated`] this pins the gaps exactly.
    fn edges_are_exact(m: &MediaMosaic, gap: u32) -> bool {
        let any = |pred: &dyn Fn(&MosaicTile) -> bool| m.tiles.iter().any(pred);
        m.tiles.iter().all(|t| {
            let same_band = |o: &MosaicTile| overlap(o.y, o.height, t.y, t.height);
            let left = t.x == 0 || any(&|o| o.x + o.width + gap == t.x && same_band(o));
            let right =
                t.x + t.width == m.width || any(&|o| t.x + t.width + gap == o.x && same_band(o));
            let top = t.y == 0 || any(&|o| o.y + o.height + gap == t.y);
            let bottom = t.y + t.height == m.height || any(&|o| t.y + t.height + gap == o.y);
            left && right && top && bottom
        })
    }

    fn arb_size() -> impl Strategy<Value = MediaSize> {
        prop_oneof![
            Just(size(0, 0)),
            (0u32..=4096, 0u32..=4096).prop_map(|(w, h)| size(w, h)),
            (1u32..=u32::MAX, 1u32..=u32::MAX).prop_map(|(w, h)| size(w, h)),
        ]
    }

    proptest! {
        #[test]
        fn tiles_never_overlap_and_keep_at_least_the_gap(
            width in 0u32..=2000,
            gap in 0u32..=16,
            items in proptest::collection::vec(arb_size(), 0..=14),
        ) {
            let m = media_mosaic(width, gap, &items);
            for (i, a) in m.tiles.iter().enumerate() {
                for b in &m.tiles[i + 1..] {
                    prop_assert!(separated(a, b, gap), "{a:?} {b:?}");
                }
            }
        }

        #[test]
        fn tiles_stay_inside_the_mosaic(
            width in 0u32..=2000,
            gap in 0u32..=16,
            items in proptest::collection::vec(arb_size(), 0..=14),
        ) {
            let m = media_mosaic(width, gap, &items);
            prop_assert!(m.width <= width);
            for t in &m.tiles {
                prop_assert!(t.width > 0 && t.height > 0);
                prop_assert!(u64::from(t.x) + u64::from(t.width) <= u64::from(m.width));
                prop_assert!(u64::from(t.y) + u64::from(t.height) <= u64::from(m.height));
            }
        }

        #[test]
        fn gaps_are_exact_and_the_mosaic_is_filled(
            width in 0u32..=2000,
            gap in 0u32..=16,
            items in proptest::collection::vec(arb_size(), 0..=14),
        ) {
            let m = media_mosaic(width, gap, &items);
            prop_assert!(edges_are_exact(&m, gap), "{m:?}");
            if items.len() >= 2 && !m.tiles.is_empty() {
                prop_assert_eq!(m.width, width);
            }
        }

        #[test]
        fn one_tile_per_item_in_order_and_deterministic(
            width in 200u32..=2000,
            gap in 0u32..=16,
            items in proptest::collection::vec(arb_size(), 0..=14),
        ) {
            let m = media_mosaic(width, gap, &items);
            prop_assert_eq!(m.tiles.len(), items.len().min(MOSAIC_MAX_ITEMS));
            prop_assert_eq!(&m, &media_mosaic(width, gap, &items));
            // Reading order: each tile starts below the one before it, or
            // level with it and further from the start edge.
            for pair in m.tiles.windows(2) {
                let (a, b) = (pair[0], pair[1]);
                prop_assert!(b.y > a.y || (b.y == a.y && b.x > a.x), "{a:?} {b:?}");
            }
        }
    }
}
