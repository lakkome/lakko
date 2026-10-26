// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! A group call's screen: the grid, the stage, and whether anything rings.
//!
//! `docs/ux/group-call-grid.md` sections 3 to 6 and `docs/ux/calls.md`
//! section 3.4. Three decisions with no platform in them, written once
//! because each client was answering them on its own:
//!
//! - **Whether the outgoing tone plays.** Every client looped it for any call
//!   that was not yet connected, so a person who started a group call alone,
//!   or joined one from a link, heard a phone ringing although nobody was
//!   being called. Found live on 2026-10-02.
//! - **The grid.** Web used CSS columns of a fixed minimum, iOS an adaptive
//!   grid of another minimum, Android two fixed columns, and all three put the
//!   tiles in a scroll area with fixed shapes. The same call on the same width
//!   drew three different grids, and none of them filled the screen.
//! - **Who is on the stage** in the layout with one large tile.
//! - **Which picture a tile asks for** (ADR-0070 step 5): the main one, the
//!   middle one or the light one alone, by how many people are in the call
//!   and how large the tile is drawn.
//!
//! Everything here is integer arithmetic, so wasm, iOS and Android cannot
//! round a tile differently. The numbers (page caps, minimum widths, gaps,
//! the aspect limits) are chosen, not measured.

use crate::call_bar::CallBarPhase;

/// Whether the outgoing tone loops.
///
/// Only while a one-to-one call this device placed is waiting for the other
/// person to answer. A group call rings nobody: its first participant is
/// simply in it, and a person who joins is joining, not calling. An answered
/// call that is still connecting does not ring either, which also keeps the
/// tone from playing on the device that just answered.
#[must_use]
pub const fn ringback_plays(group_call: bool, phase: CallBarPhase) -> bool {
    if group_call {
        return false;
    }
    matches!(phase, CallBarPhase::Preparing | CallBarPhase::Calling)
}

/// The width class of the call area, which sets the page cap, the minimum
/// tile width and the gap.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CallAreaClass {
    /// Narrower than 600: a phone in portrait, a narrow window.
    Compact,
    /// 600 to 1023: a tablet, a phone in landscape, half a laptop screen.
    Regular,
    /// 1024 and wider.
    Wide,
}

impl CallAreaClass {
    /// The class of a call area `width` points wide.
    #[must_use]
    pub const fn of_width(width: u32) -> Self {
        if width < 600 {
            Self::Compact
        } else if width < 1024 {
            Self::Regular
        } else {
            Self::Wide
        }
    }

    /// The most tiles one page draws.
    #[must_use]
    pub const fn page_cap(self) -> u32 {
        match self {
            Self::Compact => 6,
            Self::Regular => 9,
            Self::Wide => 16,
        }
    }

    /// The narrowest a tile may be before the grid drops a column.
    #[must_use]
    pub const fn min_tile_width(self) -> u32 {
        match self {
            Self::Compact => 150,
            Self::Regular => 200,
            Self::Wide => 240,
        }
    }

    /// The space between tiles.
    #[must_use]
    pub const fn gap(self) -> u32 {
        match self {
            Self::Compact => 8,
            Self::Regular | Self::Wide => 10,
        }
    }
}

/// How the grid is drawn: every tile the same size, filling the call area,
/// with no scrolling.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CallGrid {
    /// Columns on a page.
    pub cols: u32,
    /// Rows on a full page.
    pub rows: u32,
    /// Width of every tile.
    pub tile_width: u32,
    /// Height of every tile.
    pub tile_height: u32,
    /// The space between tiles.
    pub gap: u32,
    /// Tiles on a full page.
    pub per_page: u32,
    /// Pages; 1 when everyone fits.
    pub pages: u32,
}

/// Tile shape limits: no taller than 3:4, no wider than 16:9.
const fn clamp_tile(width: u32, height: u32) -> (u32, u32) {
    // Wider than 16:9: give up width.
    if width.saturating_mul(9) > height.saturating_mul(16) {
        return (height.saturating_mul(16) / 9, height);
    }
    // Taller than 3:4: give up height.
    if width.saturating_mul(4) < height.saturating_mul(3) {
        return (width, width.saturating_mul(4) / 3);
    }
    (width, height)
}

/// Lay `count` equal tiles into a call area of `width` by `height`.
///
/// The area is the space between the status line and the control bar, inside
/// the side margins; the caller measures it. The answer is the column count
/// that gives the largest tile, within the shape limits and the minimum tile
/// width of the area's class. More people than the class's page cap means
/// pages of `per_page`, never a scroll.
///
/// A last row that is not full keeps the tile size of the rows above and is
/// centred by the caller. A count of zero, or an area with no room, answers
/// a grid of zero columns.
#[must_use]
pub fn call_grid(count: u32, width: u32, height: u32) -> CallGrid {
    let class = CallAreaClass::of_width(width);
    let gap = class.gap();
    let empty = CallGrid {
        cols: 0,
        rows: 0,
        tile_width: 0,
        tile_height: 0,
        gap,
        per_page: 0,
        pages: 0,
    };
    if count == 0 || width == 0 || height == 0 {
        return empty;
    }
    // The most columns the minimum tile width allows; one column always fits.
    let max_cols = ((width + gap) / (class.min_tile_width() + gap)).max(1);
    let cap = class.page_cap();
    let on_page = count.min(cap);

    let mut best: Option<(u64, CallGrid)> = None;
    for cols in 1..=on_page.min(max_cols) {
        let rows = on_page.div_ceil(cols);
        let raw_width = width.saturating_sub(gap * (cols - 1)) / cols;
        let raw_height = height.saturating_sub(gap * (rows - 1)) / rows;
        if raw_width == 0 || raw_height == 0 {
            continue;
        }
        let (tile_width, tile_height) = clamp_tile(raw_width, raw_height);
        let area = u64::from(tile_width) * u64::from(tile_height);
        // Strictly greater: on a tie the fewer columns found first stay, so
        // the answer does not depend on iteration details elsewhere.
        if best.map_or(true, |(best_area, _)| area > best_area) {
            best = Some((
                area,
                CallGrid {
                    cols,
                    rows,
                    tile_width,
                    tile_height,
                    gap,
                    per_page: on_page,
                    pages: count.div_ceil(on_page),
                },
            ));
        }
    }
    best.map_or(empty, |(_, grid)| grid)
}

/// One person, as the stage rule sees them.
///
/// Four independent facts about a tile, so four booleans rather than a
/// state enum.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[allow(clippy::struct_excessive_bools)]
pub struct StageCandidate {
    /// This device's own tile.
    pub is_local: bool,
    /// The person is sending a picture.
    pub camera_on: bool,
    /// The person is the active speaker right now.
    pub speaking: bool,
    /// When the person last was the active speaker, in milliseconds on the
    /// caller's own clock; `0` for never.
    pub last_spoke_at_ms: u64,
    /// The tile is a shared screen, not a person's camera (ADR-0083). A
    /// share tile is its own entry beside the sharer's camera tile.
    pub is_screen: bool,
}

/// Who is on the stage in the layout with one large tile: an index into
/// `people`, or `None` when the call is empty.
///
/// In order: the pinned person; a shared screen (`docs/ux/screen-share.md`,
/// a pin wins over a share); the person speaking now; the person who spoke
/// most recently; the first person sending a picture; the first other
/// person. This device's own tile takes the stage only when nobody else is
/// in the call, and this device's own share never does. A `pinned` index
/// that is out of range, or that names this device's own tile, is ignored.
///
/// Stateless on purpose: how long a speaker must speak before they count, and
/// how long the stage holds them, is the caller's smoothing of `speaking` and
/// `last_spoke_at_ms`; given the same values, the three clients answer alike.
#[must_use]
pub fn call_stage(people: &[StageCandidate], pinned: Option<usize>) -> Option<usize> {
    if let Some(index) = pinned {
        if people.get(index).is_some_and(|p| !p.is_local) {
            return Some(index);
        }
    }
    let others = || people.iter().enumerate().filter(|(_, p)| !p.is_local);
    if let Some((index, _)) = others().find(|(_, p)| p.is_screen) {
        return Some(index);
    }
    let others = || {
        people
            .iter()
            .enumerate()
            .filter(|(_, p)| !p.is_local && !p.is_screen)
    };
    if let Some((index, _)) = others().find(|(_, p)| p.speaking) {
        return Some(index);
    }
    let recent = others()
        .filter(|(_, p)| p.last_spoke_at_ms > 0)
        // `max_by_key` keeps the last maximum; reversing the tie on the index
        // makes the earliest person win an exact tie.
        .max_by_key(|(index, p)| (p.last_spoke_at_ms, usize::MAX - index));
    if let Some((index, _)) = recent {
        return Some(index);
    }
    if let Some((index, _)) = others().find(|(_, p)| p.camera_on) {
        return Some(index);
    }
    if let Some((index, _)) = others().next() {
        return Some(index);
    }
    people.iter().position(|p| p.is_local && !p.is_screen)
}

/// Which of a person's video tracks a call tile asks the call server for
/// (`docs/ux/group-call-grid.md` section 7.6, ADR-0070 step 5).
///
/// The light track always arrives beside the one named here, and
/// [`crate::layer_display`] goes on deciding, frame by frame, whether the
/// tile shows it instead.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum TileVideoQuality {
    /// The main track: the full picture.
    Main,
    /// The middle track: the camera at half size.
    Middle,
    /// The light track alone.
    Light,
}

/// A pinned or stage tile at least this wide, in physical pixels, asks for
/// the main track. Chosen, not measured: three quarters of the 1280-pixel
/// width of the main picture, below which the middle picture stretched to
/// the tile is taken to be good enough.
pub const MAIN_MIN_WIDTH_PX: u32 = 960;

/// A tile narrower than this, in physical pixels, asks for the light track
/// alone. Chosen, not measured: three quarters of the 320-pixel width of
/// the light picture, so the light picture is never stretched by more than
/// a third before the middle one takes over.
pub const LIGHT_MAX_WIDTH_PX: u32 = 240;

/// A person sends the middle copy of their camera from this many people in
/// the call on (this device's own person included). Chosen: in a call of
/// two the one other person is shown large and asks for the main track.
pub const MIDDLE_SENDER_MIN_PEOPLE: u32 = 3;

/// Which track a tile asks for.
///
/// `people` is everyone in the call, this device's own person included.
/// `tile_width_px` is the width the tile is drawn at in **physical**
/// pixels (points times the display scale), since that is what a picture
/// is stretched over. `pinned_or_stage` says the tile is the pinned person
/// or the stage of the focus layout.
///
/// - A call of two (one other person), whatever the tile's size: `Main`.
/// - A pinned or stage tile at least [`MAIN_MIN_WIDTH_PX`] wide: `Main`.
/// - Otherwise a tile narrower than [`LIGHT_MAX_WIDTH_PX`]: `Light`.
/// - Otherwise: `Middle`.
///
/// A tile that is not on screen at all (another page) is the caller's to
/// leave out or ask `Light` for; this rule is asked about a drawn tile.
#[must_use]
pub const fn tile_video_quality(
    people: u32,
    tile_width_px: u32,
    pinned_or_stage: bool,
) -> TileVideoQuality {
    if people < MIDDLE_SENDER_MIN_PEOPLE {
        return TileVideoQuality::Main;
    }
    if pinned_or_stage && tile_width_px >= MAIN_MIN_WIDTH_PX {
        return TileVideoQuality::Main;
    }
    if tile_width_px < LIGHT_MAX_WIDTH_PX {
        return TileVideoQuality::Light;
    }
    TileVideoQuality::Middle
}

/// Whether this device sends the middle copy of its camera: only from
/// [`MIDDLE_SENDER_MIN_PEOPLE`] people in the call on (this device's own
/// person included). In a call of two nobody would be forwarded it, so the
/// sender stays inactive and costs no encoder and no uplink.
#[must_use]
pub const fn middle_sender_wanted(people: u32) -> bool {
    people >= MIDDLE_SENDER_MIN_PEOPLE
}

#[cfg(test)]
mod tests {
    use super::*;

    const EVERY_PHASE: [CallBarPhase; 8] = [
        CallBarPhase::Idle,
        CallBarPhase::Ringing,
        CallBarPhase::Preparing,
        CallBarPhase::Calling,
        CallBarPhase::Connecting,
        CallBarPhase::Connected,
        CallBarPhase::Reconnecting,
        CallBarPhase::Ended,
    ];

    /// The defect: a group call, or a call joined from a link, played the
    /// outgoing tone although nobody was being called.
    #[test]
    fn a_group_call_never_rings_out() {
        for phase in EVERY_PHASE {
            assert!(!ringback_plays(true, phase), "{phase:?}");
        }
    }

    #[test]
    fn a_one_to_one_call_rings_out_only_until_it_is_answered() {
        assert!(ringback_plays(false, CallBarPhase::Preparing));
        assert!(ringback_plays(false, CallBarPhase::Calling));
        for phase in [
            CallBarPhase::Idle,
            CallBarPhase::Ringing,
            CallBarPhase::Connecting,
            CallBarPhase::Connected,
            CallBarPhase::Reconnecting,
            CallBarPhase::Ended,
        ] {
            assert!(!ringback_plays(false, phase), "{phase:?}");
        }
    }

    fn fits(grid: CallGrid, count: u32, width: u32, height: u32) {
        let shown = count.min(grid.per_page);
        assert!(grid.cols * grid.rows >= shown, "{grid:?}");
        assert!(
            grid.cols * grid.tile_width + (grid.cols - 1) * grid.gap <= width,
            "{grid:?} wider than {width}"
        );
        assert!(
            grid.rows * grid.tile_height + (grid.rows - 1) * grid.gap <= height,
            "{grid:?} taller than {height}"
        );
        assert!(
            grid.tile_width * 9 <= grid.tile_height * 16 + 16,
            "{grid:?}"
        );
        assert!(grid.tile_width * 4 + 4 >= grid.tile_height * 3, "{grid:?}");
    }

    /// The property the old grids did not have: whatever the count and the
    /// area, one page of tiles fits without scrolling.
    #[test]
    fn a_page_always_fits_its_area() {
        for count in 1..=40 {
            for (width, height) in [
                (366, 560),
                (320, 420),
                (760, 250),
                (700, 800),
                (1200, 600),
                (1600, 900),
                (2400, 500),
            ] {
                let grid = call_grid(count, width, height);
                fits(grid, count, width, height);
                assert_eq!(grid.pages, count.div_ceil(grid.per_page));
            }
        }
    }

    #[test]
    fn a_phone_in_portrait() {
        let area = (366, 560);
        assert_eq!(call_grid(1, area.0, area.1).cols, 1);
        // Two: one above the other, wide tiles.
        let two = call_grid(2, area.0, area.1);
        assert_eq!((two.cols, two.rows), (1, 2));
        let four = call_grid(4, area.0, area.1);
        assert_eq!((four.cols, four.rows), (2, 2));
        let six = call_grid(6, area.0, area.1);
        assert_eq!((six.cols, six.rows, six.pages), (2, 3, 1));
        // Seven do not fit a page of six.
        let seven = call_grid(7, area.0, area.1);
        assert_eq!((seven.per_page, seven.pages), (6, 2));
    }

    #[test]
    fn a_laptop() {
        let area = (1200, 600);
        let two = call_grid(2, area.0, area.1);
        assert_eq!((two.cols, two.rows), (2, 1));
        let three = call_grid(3, area.0, area.1);
        assert_eq!((three.cols, three.rows), (3, 1));
        let four = call_grid(4, area.0, area.1);
        assert_eq!((four.cols, four.rows), (2, 2));
        let nine = call_grid(9, area.0, area.1);
        assert_eq!((nine.cols, nine.rows, nine.pages), (3, 3, 1));
        let twenty = call_grid(20, area.0, area.1);
        assert_eq!((twenty.per_page, twenty.pages), (16, 2));
    }

    /// A window too narrow for two tiles of the minimum width draws one
    /// column rather than two slivers.
    #[test]
    fn the_minimum_tile_width_drops_a_column() {
        let grid = call_grid(4, 280, 700);
        assert_eq!(grid.cols, 1);
    }

    #[test]
    fn nothing_to_draw_is_an_empty_grid() {
        assert_eq!(call_grid(0, 400, 400).cols, 0);
        assert_eq!(call_grid(3, 0, 400).cols, 0);
        assert_eq!(call_grid(3, 400, 0).cols, 0);
    }

    const fn person(is_local: bool, camera_on: bool) -> StageCandidate {
        StageCandidate {
            is_local,
            camera_on,
            speaking: false,
            last_spoke_at_ms: 0,
            is_screen: false,
        }
    }

    #[test]
    fn the_pin_wins_and_never_lands_on_this_devices_own_tile() {
        let mut people = [
            person(false, true),
            person(false, false),
            person(true, true),
        ];
        people[0].speaking = true;
        assert_eq!(call_stage(&people, Some(1)), Some(1));
        // A pin on the own tile, or out of range, is ignored.
        assert_eq!(call_stage(&people, Some(2)), Some(0));
        assert_eq!(call_stage(&people, Some(9)), Some(0));
    }

    #[test]
    fn the_speaker_takes_the_stage_then_the_most_recent_one_keeps_it() {
        let mut people = [
            person(false, true),
            person(false, false),
            person(false, false),
            person(true, true),
        ];
        // Nobody has spoken: the first person with a picture.
        assert_eq!(call_stage(&people, None), Some(0));
        people[2].speaking = true;
        people[2].last_spoke_at_ms = 500;
        assert_eq!(call_stage(&people, None), Some(2));
        // They stop; they stay until someone else speaks.
        people[2].speaking = false;
        assert_eq!(call_stage(&people, None), Some(2));
        people[1].last_spoke_at_ms = 900;
        assert_eq!(call_stage(&people, None), Some(1));
    }

    #[test]
    fn this_devices_own_voice_never_takes_the_stage_from_someone_else() {
        let mut people = [person(false, false), person(true, true)];
        people[1].speaking = true;
        people[1].last_spoke_at_ms = 100;
        assert_eq!(call_stage(&people, None), Some(0));
    }

    #[test]
    fn a_share_takes_the_stage_unless_someone_is_pinned() {
        let mut people = [
            person(false, true),
            person(false, false),
            person(true, true),
            person(false, false),
        ];
        people[0].speaking = true;
        people[3].is_screen = true;
        assert_eq!(call_stage(&people, None), Some(3));
        assert_eq!(call_stage(&people, Some(1)), Some(1));
        // This device's own share never takes the stage.
        people[3].is_screen = false;
        people[2].is_screen = true;
        assert_eq!(call_stage(&people, None), Some(0));
    }

    #[test]
    fn alone_in_the_call_the_own_tile_is_the_stage() {
        assert_eq!(call_stage(&[person(true, false)], None), Some(0));
        assert_eq!(call_stage(&[], None), None);
    }

    #[test]
    fn a_call_of_two_shows_the_main_picture_at_any_size() {
        for people in [0, 1, 2] {
            for width in [0, 1, LIGHT_MAX_WIDTH_PX - 1, 400, 5000] {
                for pinned in [false, true] {
                    assert_eq!(
                        tile_video_quality(people, width, pinned),
                        TileVideoQuality::Main,
                        "{people} people, {width} px, pinned {pinned}"
                    );
                }
            }
        }
    }

    #[test]
    fn from_three_people_a_grid_tile_is_middle_or_light_by_its_width() {
        for people in [3, 4, 10, u32::MAX] {
            // The light boundary: narrower than it is light.
            assert_eq!(
                tile_video_quality(people, 0, false),
                TileVideoQuality::Light
            );
            assert_eq!(
                tile_video_quality(people, LIGHT_MAX_WIDTH_PX - 1, false),
                TileVideoQuality::Light
            );
            assert_eq!(
                tile_video_quality(people, LIGHT_MAX_WIDTH_PX, false),
                TileVideoQuality::Middle
            );
            // A grid tile is never main, however wide.
            for width in [MAIN_MIN_WIDTH_PX - 1, MAIN_MIN_WIDTH_PX, 4000, u32::MAX] {
                assert_eq!(
                    tile_video_quality(people, width, false),
                    TileVideoQuality::Middle,
                    "{width} px"
                );
            }
        }
    }

    #[test]
    fn a_pinned_or_stage_tile_is_main_only_when_wide_enough() {
        assert_eq!(
            tile_video_quality(3, MAIN_MIN_WIDTH_PX, true),
            TileVideoQuality::Main
        );
        assert_eq!(
            tile_video_quality(3, u32::MAX, true),
            TileVideoQuality::Main
        );
        // One pixel short: the middle picture.
        assert_eq!(
            tile_video_quality(3, MAIN_MIN_WIDTH_PX - 1, true),
            TileVideoQuality::Middle
        );
        assert_eq!(
            tile_video_quality(3, LIGHT_MAX_WIDTH_PX, true),
            TileVideoQuality::Middle
        );
        // A pinned tile drawn tiny is still light.
        assert_eq!(
            tile_video_quality(3, LIGHT_MAX_WIDTH_PX - 1, true),
            TileVideoQuality::Light
        );
    }

    #[test]
    fn the_middle_sender_runs_from_three_people() {
        assert!(!middle_sender_wanted(0));
        assert!(!middle_sender_wanted(1));
        assert!(!middle_sender_wanted(2));
        assert!(middle_sender_wanted(3));
        assert!(middle_sender_wanted(u32::MAX));
        // The two rules turn at the same count: nobody asks for a middle
        // track in a call where nobody sends one.
        for people in 0..6 {
            let asked = tile_video_quality(people, 400, false) == TileVideoQuality::Middle;
            assert_eq!(asked, middle_sender_wanted(people), "{people}");
        }
    }

    #[test]
    fn the_thresholds_are_the_documented_ones() {
        assert_eq!(
            (
                MAIN_MIN_WIDTH_PX,
                LIGHT_MAX_WIDTH_PX,
                MIDDLE_SENDER_MIN_PEOPLE
            ),
            (960, 240, 3)
        );
    }
}
