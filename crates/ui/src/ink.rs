//! Drying-ink model (Issue #779, phase P1).
//!
//! A display-only ledger of "which source ranges were confirmed, and when".
//! It is independent of GPUI and of the document: it never writes to either,
//! it follows edits through `RopeBuffer::deltas_since`, and every time-dependent
//! function receives the current time as an argument.

#![allow(
    dead_code,
    reason = "the band, boundary and tuning APIs are wired in phase P3"
)]

use crate::color::shift_lightness;
#[cfg(test)]
use crate::color::{from_oklab, to_oklab};
use hane_document::{Revision, RevisionDelta, RopeBuffer, SourceRange, TextBuffer};
use hane_session::{DryingInkStrength, SessionId};
use std::time::{Duration, Instant};

/// Number of discrete wetness levels: 4 is the wettest, 0 is dry.
const WET_LEVELS: u8 = 4;
/// Common grid that level changes are aligned to.
const DRY_TICK: Duration = Duration::from_secs(5);
/// Confirmations closer than this in time are merged when their ranges touch.
const MERGE_WINDOW: Duration = Duration::from_millis(1500);
/// Upper bound on tracked ranges; the oldest are discarded first.
const MAX_SPANS: usize = 512;
/// The band stays fully opaque this long.
const BAND_HOLD: Duration = Duration::from_millis(600);
/// Length of one fade stage (opacity 75% / 50% / 25%).
const BAND_STEP: Duration = Duration::from_millis(100);
/// Number of visible fade stages after the hold.
const BAND_FADE_STAGES: u32 = 3;
/// A band covering more bytes than this keeps only its tail side.
const BAND_MAX_BYTES: usize = 4_096;

/// User-tunable parameters (3.1).
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct InkParams {
    pub(crate) dry_duration: Duration,
    pub(crate) delta_l: f32,
    pub(crate) band_delta_l: f32,
    pub(crate) band_enabled: bool,
    /// Whether wet ink soaks the paper behind it (Issue #788, phase P2). When
    /// on, the band is drawn by the soak layer instead of as a segment
    /// background.
    pub(crate) soak_enabled: bool,
    /// The strength setting; the soak depth and the band's alpha cap follow it.
    pub(crate) strength: DryingInkStrength,
}

impl Default for InkParams {
    fn default() -> Self {
        Self {
            dry_duration: Duration::from_secs(60),
            delta_l: 0.04,
            band_delta_l: 0.03,
            band_enabled: true,
            soak_enabled: true,
            strength: DryingInkStrength::Standard,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct InkSpan {
    range: SourceRange,
    committed_at: Instant,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct InkBand {
    range: SourceRange,
    started: Instant,
}

/// Identifies the document instance the ledger describes.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct InkOwner {
    pub(crate) session: SessionId,
    pub(crate) generation: u64,
    pub(crate) workspace: u64,
}

/// A non-text event that ends a run of confirmed text.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum InkBoundary {
    Newline,
}

/// What the renderer needs for one frame.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct InkFrame {
    /// Wet ranges (sorted, non-overlapping) with their level in `1..=4`.
    pub(crate) wet: Vec<(SourceRange, u8)>,
    /// Band range with its opacity in `(0, 1]`.
    pub(crate) band: Option<(SourceRange, f32)>,
    pub(crate) params: InkParams,
}

#[derive(Clone, Debug)]
pub(crate) struct InkLedger {
    owner: Option<InkOwner>,
    revision: Revision,
    epoch: Instant,
    /// Non-overlapping and sorted by start.
    spans: Vec<InkSpan>,
    pending_band: Option<SourceRange>,
    band: Option<InkBand>,
}

impl InkLedger {
    /// `epoch` anchors the `DRY_TICK` grid.
    pub(crate) fn new(epoch: Instant) -> Self {
        Self {
            owner: None,
            revision: Revision(0),
            epoch,
            spans: Vec::new(),
            pending_band: None,
            band: None,
        }
    }

    fn clear(&mut self) {
        self.spans.clear();
        self.pending_band = None;
        self.band = None;
    }

    /// Drops everything, e.g. when the master switch is turned off.
    pub(crate) fn reset(&mut self) {
        self.clear();
        self.owner = None;
    }

    /// Follows the document's edits since the last sync. A different document
    /// instance, or edit history that cannot be read, clears everything.
    pub(crate) fn sync(&mut self, owner: InkOwner, document: &RopeBuffer) {
        let revision = document.revision();
        if self.owner != Some(owner) {
            self.clear();
            self.owner = Some(owner);
            self.revision = revision;
            return;
        }
        if self.revision == revision {
            return;
        }
        match document.deltas_since(self.revision) {
            Ok(deltas) => {
                for delta in &deltas {
                    self.apply_delta(delta);
                }
            }
            Err(_) => self.clear(),
        }
        self.revision = revision;
    }

    fn apply_delta(&mut self, delta: &RevisionDelta) {
        let mut next = Vec::with_capacity(self.spans.len() + 1);
        for span in std::mem::take(&mut self.spans) {
            for piece in rebase_range(span.range, delta).into_iter().flatten() {
                if !piece.is_empty() {
                    next.push(InkSpan {
                        range: piece,
                        committed_at: span.committed_at,
                    });
                }
            }
        }
        self.spans = next;
        self.enforce_span_cap();
        self.pending_band = self
            .pending_band
            .and_then(|range| hull(rebase_range(range, delta)));
        self.band = self.band.and_then(|band| {
            hull(rebase_range(band.range, delta)).map(|range| InkBand { range, ..band })
        });
    }

    /// Records text confirmed into `range` (coordinates of the synced document).
    pub(crate) fn record_commit(
        &mut self,
        range: SourceRange,
        text: &str,
        now: Instant,
        params: &InkParams,
    ) {
        if range.is_empty() {
            return;
        }
        self.insert_span(range, now);
        self.pending_band = Some(match self.pending_band {
            Some(pending) if pending.start.0 <= range.end.0 && range.start.0 <= pending.end.0 => {
                SourceRange::new(
                    pending.start.0.min(range.start.0),
                    pending.end.0.max(range.end.0),
                )
            }
            _ => range,
        });
        if text.chars().next_back().is_some_and(is_sentence_end) {
            self.trigger_band(now, params);
        }
    }

    /// Records a non-text boundary such as Enter in the body.
    pub(crate) fn record_boundary(
        &mut self,
        boundary: InkBoundary,
        now: Instant,
        params: &InkParams,
    ) {
        match boundary {
            InkBoundary::Newline => self.trigger_band(now, params),
        }
    }

    fn trigger_band(&mut self, now: Instant, params: &InkParams) {
        let Some(mut range) = self.pending_band.take() else {
            return;
        };
        if !params.band_enabled {
            return;
        }
        if range.len_bytes() > BAND_MAX_BYTES {
            range = SourceRange::new(range.end.0 - BAND_MAX_BYTES, range.end.0);
        }
        self.band = Some(InkBand {
            range,
            started: now,
        });
    }

    /// Hides the band immediately (IME composition started).
    pub(crate) fn dismiss_band(&mut self) {
        self.band = None;
    }

    fn insert_span(&mut self, range: SourceRange, now: Instant) {
        // Carve the new range out of anything it overlaps.
        let mut next = Vec::with_capacity(self.spans.len() + 2);
        for span in std::mem::take(&mut self.spans) {
            if !span.range.intersects(range) {
                next.push(span);
                continue;
            }
            if span.range.start.0 < range.start.0 {
                next.push(InkSpan {
                    range: SourceRange::new(span.range.start.0, range.start.0),
                    ..span
                });
            }
            if span.range.end.0 > range.end.0 {
                next.push(InkSpan {
                    range: SourceRange::new(range.end.0, span.range.end.0),
                    ..span
                });
            }
        }
        self.spans = next;

        let mut index = self
            .spans
            .partition_point(|span| span.range.start.0 < range.start.0);
        self.spans.insert(
            index,
            InkSpan {
                range,
                committed_at: now,
            },
        );
        if index > 0 && self.mergeable(index - 1, index) {
            self.merge_into_previous(index);
            index -= 1;
        }
        if index + 1 < self.spans.len() && self.mergeable(index, index + 1) {
            self.merge_into_previous(index + 1);
        }
        self.enforce_span_cap();
    }

    /// Discards the oldest spans until at most `MAX_SPANS` remain.
    fn enforce_span_cap(&mut self) {
        while self.spans.len() > MAX_SPANS {
            let oldest = self
                .spans
                .iter()
                .enumerate()
                .min_by_key(|(_, span)| span.committed_at)
                .map(|(i, _)| i);
            match oldest {
                Some(i) => {
                    self.spans.remove(i);
                }
                None => break,
            }
        }
    }

    fn mergeable(&self, left: usize, right: usize) -> bool {
        let (a, b) = (&self.spans[left], &self.spans[right]);
        if a.range.end != b.range.start {
            return false;
        }
        let gap = a
            .committed_at
            .saturating_duration_since(b.committed_at)
            .max(b.committed_at.saturating_duration_since(a.committed_at));
        gap <= MERGE_WINDOW
    }

    fn merge_into_previous(&mut self, index: usize) {
        let removed = self.spans.remove(index);
        let previous = &mut self.spans[index - 1];
        previous.range.end = removed.range.end;
        previous.committed_at = previous.committed_at.max(removed.committed_at);
    }

    fn quantize(&self, now: Instant) -> Instant {
        let elapsed = now.saturating_duration_since(self.epoch).as_nanos();
        let tick = DRY_TICK.as_nanos();
        let aligned = elapsed / tick * tick;
        self.epoch + Duration::from_nanos(u64::try_from(aligned).unwrap_or(u64::MAX))
    }

    fn level_at(&self, committed_at: Instant, now: Instant, params: &InkParams) -> u8 {
        let age = self
            .quantize(now)
            .saturating_duration_since(committed_at);
        level_for_age(age, params.dry_duration)
    }

    /// Drops dry ranges and an expired band, then describes what to draw.
    pub(crate) fn frame(&mut self, now: Instant, params: &InkParams) -> InkFrame {
        let quantized = self.quantize(now);
        self.spans.retain(|span| {
            level_for_age(
                quantized.saturating_duration_since(span.committed_at),
                params.dry_duration,
            ) > 0
        });
        let mut wet: Vec<(SourceRange, u8)> = Vec::with_capacity(self.spans.len());
        for span in &self.spans {
            let level = self.level_at(span.committed_at, now, params);
            match wet.last_mut() {
                Some((last, last_level)) if *last_level == level && last.end == span.range.start => {
                    last.end = span.range.end;
                }
                _ => wet.push((span.range, level)),
            }
        }

        let band = match self.band {
            Some(band) => match band_opacity(now.saturating_duration_since(band.started)) {
                Some(opacity) if params.band_enabled => Some((band.range, opacity)),
                Some(_) => None,
                None => {
                    self.band = None;
                    None
                }
            },
            None => None,
        };
        InkFrame {
            wet,
            band,
            params: *params,
        }
    }

    /// The earliest instant after `now` at which `frame` would differ.
    pub(crate) fn next_wakeup(&self, now: Instant, params: &InkParams) -> Option<Instant> {
        let mut earliest: Option<Instant> = None;
        let mut consider = |candidate: Instant| {
            if candidate > now && earliest.is_none_or(|current| candidate < current) {
                earliest = Some(candidate);
            }
        };

        let tick = DRY_TICK.as_nanos();
        let dry = params.dry_duration.as_nanos();
        for span in &self.spans {
            let level = self.level_at(span.committed_at, now, params);
            if level == 0 {
                continue;
            }
            // The level falls below `level` once age >= D * (5 - level) / 4.
            let threshold = dry * u128::from(WET_LEVELS + 1 - level) / 4;
            let threshold = Duration::from_nanos(u64::try_from(threshold).unwrap_or(u64::MAX));
            let change = span.committed_at + threshold;
            let since_epoch = change.saturating_duration_since(self.epoch).as_nanos();
            let steps = since_epoch.div_ceil(tick);
            let aligned = Duration::from_nanos(u64::try_from(steps * tick).unwrap_or(u64::MAX));
            consider(self.epoch + aligned);
        }

        if params.band_enabled
            && let Some(band) = self.band
        {
            for stage in 0..=BAND_FADE_STAGES {
                consider(band.started + BAND_HOLD + BAND_STEP * stage);
            }
        }
        earliest
    }
}

pub(crate) fn is_sentence_end(c: char) -> bool {
    matches!(c, '。' | '！' | '？')
}

/// Opacity of the band `age` after it started; `None` once it is gone.
/// Fully opaque for `BAND_HOLD`, then 75% / 50% / 25% in `BAND_STEP` stages.
fn band_opacity(age: Duration) -> Option<f32> {
    if age < BAND_HOLD {
        return Some(1.0);
    }
    let stage = (age - BAND_HOLD).as_millis() / BAND_STEP.as_millis();
    match stage {
        0 => Some(0.75),
        1 => Some(0.5),
        2 => Some(0.25),
        _ => None,
    }
}

/// `level = clamp(ceil(4 * (1 - age / D)), 0, 4)` in exact integer arithmetic.
fn level_for_age(age: Duration, dry_duration: Duration) -> u8 {
    if dry_duration.is_zero() || age >= dry_duration {
        return 0;
    }
    let remaining = (dry_duration - age).as_nanos();
    let level = (remaining * u128::from(WET_LEVELS)).div_ceil(dry_duration.as_nanos());
    u8::try_from(level.min(u128::from(WET_LEVELS))).unwrap_or(WET_LEVELS)
}

fn hull(pieces: [Option<SourceRange>; 2]) -> Option<SourceRange> {
    pieces.into_iter().flatten().reduce(|a, b| {
        SourceRange::new(a.start.0.min(b.start.0), a.end.0.max(b.end.0))
    })
}

fn shift(offset: usize, delta: isize) -> usize {
    offset.saturating_add_signed(delta)
}

/// Follows one edit (3.5). Returns the surviving pieces in order: the part
/// left of the edit and the part right of it.
pub(crate) fn rebase_range(range: SourceRange, delta: &RevisionDelta) -> [Option<SourceRange>; 2] {
    let before = delta.edited_source_range_before;
    let (bs, be) = (before.start.0, before.end.0);
    if range.end.0 <= bs {
        return [Some(range), None];
    }
    if range.start.0 >= be {
        return [
            Some(SourceRange::new(
                shift(range.start.0, delta.byte_delta),
                shift(range.end.0, delta.byte_delta),
            )),
            None,
        ];
    }
    let left = (range.start.0 < bs).then(|| SourceRange::new(range.start.0, bs));
    let right = (range.end.0 > be).then(|| {
        SourceRange::new(
            shift(be, delta.byte_delta),
            shift(range.end.0, delta.byte_delta),
        )
    });
    [left, right]
}

// --- Color (OKLab, lightness only) -------------------------------------
// The conversion functions live in `crate::color`.

/// Multiplier applied to the text `delta_l` in dark themes. The dark text color
/// already sits near OKLab lightness 0.92, so the same `delta_l` that is clearly
/// visible on dark ink in light themes moves it by only ~13/255 in sRGB and the
/// drying effect is hard to see (#782). Doubling it makes the effect visible.
const DARK_DELTA_L_FACTOR: f64 = 2.0;

/// Text color for a wet range (3.3): the base color with its OKLab lightness
/// moved toward more contrast by `delta_l * level / 4`. Light themes darken;
/// dark themes lighten by `DARK_DELTA_L_FACTOR` times that amount (lightness is
/// clamped to 1.0). Level 0 returns `base` untouched.
pub(crate) fn ink_color(base: u32, level: u8, delta_l: f32, dark: bool) -> u32 {
    if level == 0 {
        return base;
    }
    let level = level.min(WET_LEVELS);
    let direction = if dark { DARK_DELTA_L_FACTOR } else { -1.0 };
    let amount = direction * f64::from(delta_l) * f64::from(level) / f64::from(WET_LEVELS);
    shift_lightness(base, amount)
}

/// Band background (3.7): the editor background moved toward the text side.
pub(crate) fn band_color(background: u32, delta_l: f32, dark: bool) -> u32 {
    let direction = if dark { 1.0 } else { -1.0 };
    shift_lightness(background, direction * f64::from(delta_l))
}

// --- Soak (Issue #788, phase P2) ----------------------------------------

/// Opacity the soak gains per wetness level: 0.72 / 0.54 / 0.36 / 0.18 for
/// levels 4..=1.
const SOAK_ALPHA_PER_LEVEL: f32 = 0.18;
/// What the band adds to the soak opacity at full band opacity.
const SOAK_BAND_ALPHA: f32 = 0.25;
/// The soak with the band on top never gets more opaque than this.
const SOAK_ALPHA_MAX: f32 = 0.9;
/// Upper bound of the soak opacity with the band on top, per strength
/// (`[weak, standard, strong]`) and theme (`[light, dark]`).
///
/// Computed from the darkest texture pixel (`L_bg - 0.020` in light themes) and
/// the soak color `L_bg - depth`: the composite lightness is
/// `L_texture - alpha * (L_texture - L_soak)`, which has to stay at or above
/// `SOAK_LIGHT_FLOOR_L`. Only the strong light case reaches that floor before
/// `SOAK_ALPHA_MAX`: `0.042 / 0.055 = 0.76`, held back to 0.72 (the level-4
/// soak itself) for rounding. Dark soaks only make the background darker, so
/// the ceiling `SOAK_DARK_CEILING_L` is never in play.
const SOAK_ALPHA_CAPS: [[f32; 2]; 3] = [[0.9, 0.9], [0.9, 0.9], [0.72, 0.9]];
/// Lowest OKLab lightness the composite background may have in a light theme.
pub(crate) const SOAK_LIGHT_FLOOR_L: f64 = 0.92;
/// Highest OKLab lightness the composite background may have in a dark theme.
pub(crate) const SOAK_DARK_CEILING_L: f64 = 0.255;

/// Opacity of the soak for a wetness level: `0.18 * level`, and none when dry.
pub(crate) fn soak_level_alpha(level: u8) -> f32 {
    SOAK_ALPHA_PER_LEVEL * f32::from(level.min(WET_LEVELS))
}

/// The cap of the soak opacity with the band on top.
pub(crate) fn soak_alpha_cap(strength: DryingInkStrength, dark: bool) -> f32 {
    let row = match strength {
        DryingInkStrength::Weak => 0,
        DryingInkStrength::Standard => 1,
        DryingInkStrength::Strong => 2,
    };
    SOAK_ALPHA_CAPS[row][usize::from(dark)]
}

/// Opacity of the soak behind a stretch with wetness `level` and band opacity
/// `band` (0 when the band does not cover it). The band only adds to the soak,
/// never takes from it, and the sum stays within the cap.
pub(crate) fn soak_alpha(level: u8, band: f32, strength: DryingInkStrength, dark: bool) -> f32 {
    let base = soak_level_alpha(level);
    let band = band.clamp(0.0, 1.0);
    if band <= 0.0 {
        return base;
    }
    let capped = (base + SOAK_BAND_ALPHA * band)
        .min(SOAK_ALPHA_MAX)
        .min(soak_alpha_cap(strength, dark));
    capped.max(base)
}

/// The soak color: the editor background moved `depth` darker, in both themes.
pub(crate) fn soak_color(background: u32, depth: f32) -> u32 {
    shift_lightness(background, -f64::from(depth))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::theme::{DARK_THEME, DEFAULT_THEME};

    /// The model never reads the clock; tests need one starting instant.
    fn base() -> Instant {
        Instant::now()
    }

    fn secs(n: u64) -> Duration {
        Duration::from_secs(n)
    }

    fn millis(n: u64) -> Duration {
        Duration::from_millis(n)
    }

    fn owner(generation: u64) -> InkOwner {
        InkOwner {
            session: SessionId(1),
            generation,
            workspace: 1,
        }
    }

    fn delta(bs: usize, be: usize, new_len: usize) -> RevisionDelta {
        RevisionDelta {
            from_revision: Revision(0),
            to_revision: Revision(1),
            edited_source_range_before: SourceRange::new(bs, be),
            edited_source_range_after: SourceRange::new(bs, bs + new_len),
            byte_delta: new_len as isize - (be - bs) as isize,
        }
    }

    fn rebase(range: (usize, usize), d: &RevisionDelta) -> [Option<(usize, usize)>; 2] {
        rebase_range(SourceRange::new(range.0, range.1), d)
            .map(|piece| piece.map(|r| (r.start.0, r.end.0)))
    }

    fn spans_of(ledger: &InkLedger) -> Vec<(usize, usize)> {
        ledger
            .spans
            .iter()
            .map(|s| (s.range.start.0, s.range.end.0))
            .collect()
    }

    // --- rebase_range ---

    #[test]
    fn range_before_edit_is_unchanged() {
        assert_eq!(rebase((10, 20), &delta(25, 30, 1)), [Some((10, 20)), None]);
    }

    #[test]
    fn range_after_edit_is_shifted() {
        assert_eq!(rebase((10, 20), &delta(0, 5, 2)), [Some((7, 17)), None]);
        assert_eq!(rebase((10, 20), &delta(0, 0, 4)), [Some((14, 24)), None]);
    }

    #[test]
    fn edit_touching_range_end_does_not_extend_it() {
        assert_eq!(rebase((10, 20), &delta(20, 20, 3)), [Some((10, 20)), None]);
        assert_eq!(rebase((10, 20), &delta(20, 25, 1)), [Some((10, 20)), None]);
    }

    #[test]
    fn edit_touching_range_start_shifts_it() {
        assert_eq!(rebase((10, 20), &delta(10, 10, 3)), [Some((13, 23)), None]);
        assert_eq!(rebase((10, 20), &delta(5, 10, 1)), [Some((6, 16)), None]);
    }

    #[test]
    fn insertion_inside_range_splits_it() {
        assert_eq!(
            rebase((10, 20), &delta(15, 15, 4)),
            [Some((10, 15)), Some((19, 24))]
        );
    }

    #[test]
    fn edit_crossing_range_start_keeps_right_part() {
        assert_eq!(rebase((10, 20), &delta(5, 12, 1)), [None, Some((6, 14))]);
    }

    #[test]
    fn edit_crossing_range_end_keeps_left_part() {
        assert_eq!(rebase((10, 20), &delta(18, 25, 0)), [Some((10, 18)), None]);
    }

    #[test]
    fn edit_containing_range_removes_it() {
        assert_eq!(rebase((10, 20), &delta(8, 25, 3)), [None, None]);
        assert_eq!(rebase((10, 20), &delta(10, 20, 5)), [None, None]);
    }

    #[test]
    fn deletion_inside_range_splits_it() {
        assert_eq!(
            rebase((10, 20), &delta(12, 15, 0)),
            [Some((10, 12)), Some((12, 17))]
        );
    }

    // --- sync ---

    fn doc(len: usize) -> RopeBuffer {
        RopeBuffer::from_text(&"a".repeat(len))
    }

    #[test]
    fn replacement_spanning_two_ranges_keeps_outer_parts() {
        let mut buffer = doc(20);
        let t = base();
        let p = InkParams::default();
        let mut ledger = InkLedger::new(t);
        ledger.sync(owner(1), &buffer);
        ledger.record_commit(SourceRange::new(2, 5), "abc", t, &p);
        ledger.record_commit(SourceRange::new(8, 12), "abcd", t + secs(10), &p);
        buffer.edit(SourceRange::new(4, 10), "x").unwrap();
        ledger.sync(owner(1), &buffer);
        assert_eq!(spans_of(&ledger), vec![(2, 4), (5, 7)]);
        assert_eq!(ledger.spans[0].committed_at, t);
        assert_eq!(ledger.spans[1].committed_at, t + secs(10));
    }

    #[test]
    fn several_deltas_apply_in_order() {
        let mut buffer = doc(20);
        let t = base();
        let mut ledger = InkLedger::new(t);
        ledger.sync(owner(1), &buffer);
        ledger.record_commit(SourceRange::new(5, 10), "abcde", t, &InkParams::default());
        buffer.edit(SourceRange::empty(0), "XX").unwrap();
        buffer.edit(SourceRange::new(0, 1), "").unwrap();
        buffer.edit(SourceRange::empty(8), "YYY").unwrap();
        ledger.sync(owner(1), &buffer);
        // 5..10 -> 7..12 -> 6..11, then an insertion at 8 splits it.
        assert_eq!(spans_of(&ledger), vec![(6, 8), (11, 14)]);
    }

    #[test]
    fn sync_with_same_revision_is_a_no_op() {
        let buffer = doc(5);
        let t = base();
        let mut ledger = InkLedger::new(t);
        ledger.sync(owner(1), &buffer);
        ledger.record_commit(SourceRange::new(0, 2), "ab", t, &InkParams::default());
        ledger.sync(owner(1), &buffer);
        assert_eq!(spans_of(&ledger), vec![(0, 2)]);
    }

    #[test]
    fn different_document_instance_clears_everything() {
        let buffer = doc(5);
        let t = base();
        let p = InkParams::default();
        let mut ledger = InkLedger::new(t);
        ledger.sync(owner(1), &buffer);
        ledger.record_commit(SourceRange::new(0, 2), "ab。", t, &p);
        ledger.record_commit(SourceRange::new(2, 3), "c", t, &p);
        ledger.sync(owner(2), &buffer);
        assert!(ledger.spans.is_empty());
        assert!(ledger.pending_band.is_none());
        assert!(ledger.band.is_none());
        assert_eq!(ledger.owner, Some(owner(2)));
    }

    #[test]
    fn missing_history_clears_everything() {
        let t = base();
        let p = InkParams::default();
        let mut ledger = InkLedger::new(t);
        let mut buffer = doc(5);
        ledger.sync(owner(1), &buffer);
        ledger.record_commit(SourceRange::new(0, 2), "ab", t, &p);
        // The history window (4,096 edits) no longer reaches revision 0.
        for _ in 0..4_100 {
            buffer.edit(SourceRange::empty(0), "z").unwrap();
        }
        ledger.sync(owner(1), &buffer);
        assert!(ledger.spans.is_empty());
        assert_eq!(ledger.revision, buffer.revision());
    }

    #[test]
    fn unreadable_revision_clears_everything() {
        let t = base();
        let mut ledger = InkLedger::new(t);
        let mut first = doc(5);
        first.edit(SourceRange::empty(0), "z").unwrap();
        first.edit(SourceRange::empty(0), "z").unwrap();
        ledger.sync(owner(1), &first);
        ledger.record_commit(SourceRange::new(0, 2), "zz", t, &InkParams::default());
        // Same owner, but a document whose revision is behind the ledger's.
        ledger.sync(owner(1), &doc(5));
        assert!(ledger.spans.is_empty());
    }

    struct XorShift(u64);

    impl XorShift {
        fn draw(&mut self) -> usize {
            self.0 ^= self.0 << 13;
            self.0 ^= self.0 >> 7;
            self.0 ^= self.0 << 17;
            (self.0 >> 8) as usize
        }
    }

    #[test]
    fn random_edits_keep_spans_sorted_and_disjoint() {
        let t = base();
        let p = InkParams::default();
        let mut rng = XorShift(0x9e37_79b9_7f4a_7c15);
        let mut buffer = doc(200);
        let mut ledger = InkLedger::new(t);
        ledger.sync(owner(1), &buffer);
        for step in 0..600_u64 {
            let len = buffer.len_bytes().0;
            let start = rng.draw() % (len + 1);
            let end = (start + rng.draw() % 8).min(len);
            let replacement = "x".repeat(rng.draw() % 6);
            let summary = buffer
                .edit(SourceRange::new(start, end), &replacement)
                .unwrap();
            if rng.draw().is_multiple_of(3) {
                ledger.sync(owner(1), &buffer);
                if rng.draw().is_multiple_of(2) {
                    let now = t + millis(step * 700);
                    ledger.record_commit(summary.range_after, &replacement, now, &p);
                }
                let len = buffer.len_bytes().0;
                let mut previous_end = 0;
                for span in &ledger.spans {
                    assert!(!span.range.is_empty());
                    assert!(span.range.start.0 >= previous_end, "{:?}", spans_of(&ledger));
                    assert!(span.range.end.0 <= len);
                    previous_end = span.range.end.0;
                }
                assert!(ledger.spans.len() <= MAX_SPANS);
            }
        }
    }

    // --- merge and cap ---

    #[test]
    fn touching_commits_within_window_merge() {
        let t = base();
        let p = InkParams::default();
        let mut ledger = InkLedger::new(t);
        ledger.record_commit(SourceRange::new(0, 1), "a", t, &p);
        ledger.record_commit(SourceRange::new(1, 2), "b", t + millis(1500), &p);
        assert_eq!(spans_of(&ledger), vec![(0, 2)]);
        assert_eq!(ledger.spans[0].committed_at, t + millis(1500));
    }

    #[test]
    fn commits_outside_window_or_apart_do_not_merge() {
        let t = base();
        let p = InkParams::default();
        let mut ledger = InkLedger::new(t);
        ledger.record_commit(SourceRange::new(0, 1), "a", t, &p);
        ledger.record_commit(SourceRange::new(1, 2), "b", t + millis(1501), &p);
        ledger.record_commit(SourceRange::new(5, 6), "c", t + millis(1600), &p);
        assert_eq!(spans_of(&ledger), vec![(0, 1), (1, 2), (5, 6)]);
    }

    #[test]
    fn commit_bridging_two_spans_merges_both_sides() {
        let t = base();
        let p = InkParams::default();
        let mut ledger = InkLedger::new(t);
        ledger.record_commit(SourceRange::new(0, 1), "a", t, &p);
        ledger.record_commit(SourceRange::new(2, 3), "c", t, &p);
        ledger.record_commit(SourceRange::new(1, 2), "b", t + millis(500), &p);
        assert_eq!(spans_of(&ledger), vec![(0, 3)]);
    }

    #[test]
    fn commit_over_existing_span_replaces_the_overlap() {
        let t = base();
        let p = InkParams::default();
        let mut ledger = InkLedger::new(t);
        ledger.record_commit(SourceRange::new(0, 10), "a", t, &p);
        ledger.record_commit(SourceRange::new(3, 5), "b", t + secs(10), &p);
        assert_eq!(spans_of(&ledger), vec![(0, 3), (3, 5), (5, 10)]);
        assert_eq!(ledger.spans[1].committed_at, t + secs(10));
    }

    #[test]
    fn span_count_is_capped_and_oldest_are_discarded() {
        let t = base();
        let p = InkParams::default();
        let mut ledger = InkLedger::new(t);
        for i in 0..600_u64 {
            let start = usize::try_from(i).unwrap() * 2;
            ledger.record_commit(SourceRange::new(start, start + 1), "a", t + secs(i * 2), &p);
        }
        assert_eq!(ledger.spans.len(), MAX_SPANS);
        assert_eq!(ledger.spans[0].range.start.0, (600 - MAX_SPANS) * 2);
        assert_eq!(ledger.spans.last().unwrap().range.start.0, 599 * 2);
    }

    #[test]
    fn splitting_edit_keeps_span_count_within_cap() {
        let t = base();
        let p = InkParams::default();
        let mut buffer = doc(3000);
        let mut ledger = InkLedger::new(t);
        ledger.sync(owner(1), &buffer);
        for i in 0..MAX_SPANS {
            let start = i * 4;
            let at = t + secs(10 * u64::try_from(i).unwrap());
            ledger.record_commit(SourceRange::new(start, start + 3), "a", at, &p);
        }
        assert_eq!(ledger.spans.len(), MAX_SPANS);

        // Inserting inside the span at 1200..1203 splits it in two.
        buffer.edit(SourceRange::empty(1201), "X").unwrap();
        ledger.sync(owner(1), &buffer);

        assert_eq!(ledger.spans.len(), MAX_SPANS);
        // The oldest span (0..3) was dropped.
        assert_eq!(ledger.spans[0].range, SourceRange::new(4, 7));
        let split_time = t + secs(10 * 300);
        let pieces: Vec<_> = ledger
            .spans
            .iter()
            .filter(|span| span.committed_at == split_time)
            .map(|span| (span.range.start.0, span.range.end.0))
            .collect();
        assert_eq!(pieces, vec![(1200, 1201), (1202, 1204)]);
        // Spans after the edit are shifted by one byte.
        assert_eq!(ledger.spans.last().unwrap().range, SourceRange::new(2045, 2048));
    }

    // --- levels ---

    fn level_after(dry: u64, age: Duration) -> u8 {
        let t = base();
        let p = InkParams {
            dry_duration: secs(dry),
            ..InkParams::default()
        };
        let mut ledger = InkLedger::new(t);
        ledger.record_commit(SourceRange::new(0, 1), "a", t, &p);
        ledger
            .frame(t + age, &p)
            .wet
            .first()
            .map_or(0, |(_, level)| *level)
    }

    #[test]
    fn level_boundaries_follow_the_formula() {
        for dry in [20_u64, 60, 180] {
            let step = dry / 4;
            assert_eq!(level_after(dry, secs(0)), 4, "D={dry}");
            // The grid rounds down, so a step is reached on the first tick at
            // or after it; these ages are all tick multiples.
            let tick_ge = |n: u64| secs(n.div_ceil(5) * 5);
            assert_eq!(level_after(dry, tick_ge(step) - secs(5)), 4, "D={dry}");
            assert_eq!(level_after(dry, tick_ge(step)), 3, "D={dry}");
            assert_eq!(level_after(dry, tick_ge(step * 2)), 2, "D={dry}");
            assert_eq!(level_after(dry, tick_ge(step * 3)), 1, "D={dry}");
            assert_eq!(level_after(dry, secs(dry)), 0, "D={dry}");
        }
    }

    #[test]
    fn default_duration_drops_one_level_every_fifteen_seconds() {
        assert_eq!(level_for_age(secs(0), secs(60)), 4);
        assert_eq!(level_for_age(secs(14), secs(60)), 4);
        assert_eq!(level_for_age(secs(15), secs(60)), 3);
        assert_eq!(level_for_age(secs(30), secs(60)), 2);
        assert_eq!(level_for_age(secs(45), secs(60)), 1);
        assert_eq!(level_for_age(secs(59), secs(60)), 1);
        assert_eq!(level_for_age(secs(60), secs(60)), 0);
        assert_eq!(level_for_age(secs(20), secs(20)), 0);
        assert_eq!(level_for_age(secs(5), secs(20)), 3);
        assert_eq!(level_for_age(secs(45), secs(180)), 3);
        assert_eq!(level_for_age(secs(135), secs(180)), 1);
    }

    #[test]
    fn level_changes_only_on_the_grid() {
        let t = base();
        let p = InkParams::default();
        let mut ledger = InkLedger::new(t);
        ledger.record_commit(SourceRange::new(0, 1), "a", t + secs(3), &p);
        // age at the quantized time: 15 - 3 = 12 -> still 4.
        assert_eq!(ledger.frame(t + secs(19), &p).wet[0].1, 4);
        // quantized 20: age 17 -> 3.
        assert_eq!(ledger.frame(t + secs(20), &p).wet[0].1, 3);
    }

    #[test]
    fn frame_drops_dry_spans_and_merges_equal_levels() {
        let t = base();
        let p = InkParams::default();
        let mut ledger = InkLedger::new(t);
        ledger.record_commit(SourceRange::new(0, 2), "ab", t, &p);
        ledger.record_commit(SourceRange::new(2, 4), "cd", t + secs(10), &p);
        ledger.record_commit(SourceRange::new(10, 12), "ef", t + secs(10), &p);
        let frame = ledger.frame(t + secs(30), &p);
        // Spans (0,2) and (2,4) were not merged at record time (10 s apart).
        assert_eq!(
            frame.wet,
            vec![
                (SourceRange::new(0, 2), 2),
                (SourceRange::new(2, 4), 3),
                (SourceRange::new(10, 12), 3)
            ]
        );
        let frame = ledger.frame(t + secs(65), &p);
        assert_eq!(frame.wet, vec![(SourceRange::new(2, 4), 1), (SourceRange::new(10, 12), 1)]);
        let frame = ledger.frame(t + secs(70), &p);
        assert!(frame.wet.is_empty());
        assert!(ledger.spans.is_empty());
    }

    #[test]
    fn adjacent_spans_with_equal_level_are_joined_in_the_frame() {
        let t = base();
        let p = InkParams::default();
        let mut ledger = InkLedger::new(t);
        ledger.record_commit(SourceRange::new(0, 2), "ab", t, &p);
        ledger.record_commit(SourceRange::new(2, 4), "cd", t + secs(2), &p);
        // 2 s apart: separate spans, same level on the grid.
        assert_eq!(ledger.spans.len(), 2);
        let frame = ledger.frame(t + secs(5), &p);
        assert_eq!(frame.wet, vec![(SourceRange::new(0, 4), 4)]);
    }

    // --- wakeups ---

    #[test]
    fn next_wakeup_is_none_without_ink() {
        let t = base();
        let ledger = InkLedger::new(t);
        assert_eq!(ledger.next_wakeup(t, &InkParams::default()), None);
    }

    #[test]
    fn next_wakeup_walks_the_level_steps_and_ends_when_dry() {
        let t = base();
        let p = InkParams::default();
        let mut ledger = InkLedger::new(t);
        ledger.record_commit(SourceRange::new(0, 1), "a", t, &p);
        let mut now = t;
        let mut seen = Vec::new();
        while let Some(wakeup) = ledger.next_wakeup(now, &p) {
            assert!(wakeup > now);
            seen.push(wakeup.duration_since(t));
            now = wakeup;
            ledger.frame(now, &p);
        }
        assert_eq!(seen, vec![secs(15), secs(30), secs(45), secs(60)]);
        assert_eq!(ledger.next_wakeup(now, &p), None);
    }

    #[test]
    fn next_wakeup_aligns_to_the_grid() {
        let t = base();
        let p = InkParams::default();
        let mut ledger = InkLedger::new(t);
        ledger.record_commit(SourceRange::new(0, 1), "a", t + secs(3), &p);
        assert_eq!(ledger.next_wakeup(t + secs(3), &p), Some(t + secs(20)));
        let frame = ledger.frame(t + secs(20), &p);
        assert_eq!(frame.wet[0].1, 3);
    }

    #[test]
    fn next_wakeup_picks_the_earliest_span() {
        let t = base();
        let p = InkParams::default();
        let mut ledger = InkLedger::new(t);
        ledger.record_commit(SourceRange::new(0, 1), "a", t, &p);
        ledger.record_commit(SourceRange::new(5, 6), "b", t + secs(5), &p);
        assert_eq!(ledger.next_wakeup(t + secs(5), &p), Some(t + secs(15)));
    }

    // --- band ---

    #[test]
    fn sentence_end_triggers_a_band_over_the_pending_run() {
        let t = base();
        let p = InkParams::default();
        let mut ledger = InkLedger::new(t);
        ledger.record_commit(SourceRange::new(0, 3), "あ", t, &p);
        assert!(ledger.band.is_none());
        assert_eq!(ledger.pending_band, Some(SourceRange::new(0, 3)));
        ledger.record_commit(SourceRange::new(3, 9), "い。", t + millis(200), &p);
        let band = ledger.band.unwrap();
        assert_eq!(band.range, SourceRange::new(0, 9));
        assert_eq!(band.started, t + millis(200));
        assert_eq!(ledger.pending_band, None);
    }

    #[test]
    fn pending_band_restarts_when_confirmation_moves_away() {
        let t = base();
        let p = InkParams::default();
        let mut ledger = InkLedger::new(t);
        ledger.record_commit(SourceRange::new(0, 3), "あ", t, &p);
        ledger.record_commit(SourceRange::new(10, 13), "い", t, &p);
        assert_eq!(ledger.pending_band, Some(SourceRange::new(10, 13)));
        ledger.record_commit(SourceRange::new(13, 16), "う！", t, &p);
        assert_eq!(ledger.band.unwrap().range, SourceRange::new(10, 16));
    }

    #[test]
    fn question_mark_triggers_and_ascii_period_does_not() {
        let t = base();
        let p = InkParams::default();
        let mut ledger = InkLedger::new(t);
        ledger.record_commit(SourceRange::new(0, 1), "a.", t, &p);
        ledger.record_commit(SourceRange::new(1, 2), "3.14", t, &p);
        assert!(ledger.band.is_none());
        ledger.record_commit(SourceRange::new(2, 5), "？", t, &p);
        assert!(ledger.band.is_some());
    }

    #[test]
    fn newline_triggers_only_with_a_pending_run() {
        let t = base();
        let p = InkParams::default();
        let mut ledger = InkLedger::new(t);
        ledger.record_boundary(InkBoundary::Newline, t, &p);
        assert!(ledger.band.is_none());
        ledger.record_commit(SourceRange::new(0, 2), "ab", t, &p);
        ledger.record_boundary(InkBoundary::Newline, t + millis(10), &p);
        assert_eq!(ledger.band.unwrap().range, SourceRange::new(0, 2));
        assert_eq!(ledger.pending_band, None);
    }

    #[test]
    fn disabled_band_is_never_created_or_reported() {
        let t = base();
        let p = InkParams {
            band_enabled: false,
            ..InkParams::default()
        };
        let mut ledger = InkLedger::new(t);
        ledger.record_commit(SourceRange::new(0, 2), "あ。", t, &p);
        assert!(ledger.band.is_none());
        assert!(ledger.frame(t, &p).band.is_none());
        assert_eq!(ledger.pending_band, None);

        let enabled = InkParams::default();
        ledger.record_commit(SourceRange::new(2, 4), "い。", t, &enabled);
        assert!(ledger.band.is_some());
        assert!(ledger.frame(t, &p).band.is_none());
    }

    #[test]
    fn band_holds_then_fades_then_disappears() {
        let t = base();
        let p = InkParams::default();
        let mut ledger = InkLedger::new(t);
        ledger.record_commit(SourceRange::new(0, 3), "あ。", t, &p);
        let band = |ledger: &mut InkLedger, ms: u64| ledger.frame(t + millis(ms), &p).band;
        let range = SourceRange::new(0, 3);
        assert_eq!(band(&mut ledger, 0), Some((range, 1.0)));
        assert_eq!(band(&mut ledger, 599), Some((range, 1.0)));
        assert_eq!(band(&mut ledger, 600), Some((range, 0.75)));
        assert_eq!(band(&mut ledger, 700), Some((range, 0.5)));
        assert_eq!(band(&mut ledger, 800), Some((range, 0.25)));
        assert_eq!(band(&mut ledger, 899), Some((range, 0.25)));
        assert_eq!(band(&mut ledger, 900), None);
        assert!(ledger.band.is_none());
    }

    #[test]
    fn band_wakeups_cover_every_visible_change() {
        let t = base();
        let p = InkParams::default();
        let mut ledger = InkLedger::new(t);
        ledger.record_commit(SourceRange::new(0, 3), "あ。", t, &p);
        // The wet span's own next change (15 s) is later than every band step.
        let mut now = t;
        let mut seen = Vec::new();
        for _ in 0..4 {
            let wakeup = ledger.next_wakeup(now, &p).unwrap();
            seen.push(wakeup.duration_since(t));
            now = wakeup;
        }
        assert_eq!(seen, vec![millis(600), millis(700), millis(800), millis(900)]);
        ledger.frame(now, &p);
        assert_eq!(ledger.next_wakeup(now, &p), Some(t + secs(15)));
    }

    #[test]
    fn dismissing_the_band_removes_it() {
        let t = base();
        let p = InkParams::default();
        let mut ledger = InkLedger::new(t);
        ledger.record_commit(SourceRange::new(0, 3), "あ。", t, &p);
        ledger.dismiss_band();
        assert!(ledger.frame(t, &p).band.is_none());
    }

    #[test]
    fn band_and_pending_run_follow_edits() {
        let mut buffer = doc(30);
        let t = base();
        let p = InkParams::default();
        let mut ledger = InkLedger::new(t);
        ledger.sync(owner(1), &buffer);
        ledger.record_commit(SourceRange::new(10, 12), "あ", t, &p);
        ledger.record_commit(SourceRange::new(12, 14), "い。", t, &p);
        ledger.record_commit(SourceRange::new(14, 16), "う", t, &p);
        buffer.edit(SourceRange::empty(0), "XXX").unwrap();
        ledger.sync(owner(1), &buffer);
        assert_eq!(ledger.band.unwrap().range, SourceRange::new(13, 17));
        assert_eq!(ledger.pending_band, Some(SourceRange::new(17, 19)));
    }

    #[test]
    fn band_is_clipped_to_its_tail_when_too_long() {
        let t = base();
        let p = InkParams::default();
        let mut ledger = InkLedger::new(t);
        ledger.record_commit(SourceRange::new(0, 10_000), "a。", t, &p);
        assert_eq!(
            ledger.band.unwrap().range,
            SourceRange::new(10_000 - BAND_MAX_BYTES, 10_000)
        );
    }

    #[test]
    fn reset_clears_state_and_owner() {
        let t = base();
        let buffer = doc(5);
        let p = InkParams::default();
        let mut ledger = InkLedger::new(t);
        ledger.sync(owner(1), &buffer);
        ledger.record_commit(SourceRange::new(0, 2), "あ。", t, &p);
        ledger.reset();
        assert!(ledger.spans.is_empty());
        assert!(ledger.band.is_none());
        assert_eq!(ledger.owner, None);
    }

    // --- color ---

    fn lab_distance_parts(a: u32, b: u32) -> (f64, f64, f64) {
        let (a, b) = (to_oklab(a), to_oklab(b));
        (b[0] - a[0], (b[1] - a[1]).abs(), (b[2] - a[2]).abs())
    }

    /// Low-chroma foreground tokens that stay inside the sRGB gamut at every
    /// tested lightness, so the lightness-only contract holds exactly.
    fn base_colors() -> Vec<(u32, bool)> {
        let mut colors = Vec::new();
        for (theme, dark) in [(DEFAULT_THEME, false), (DARK_THEME, true)] {
            colors.push((theme.foreground, dark));
            colors.push((theme.quote_foreground, dark));
        }
        colors
    }

    #[test]
    fn ink_color_moves_only_lightness() {
        for (base_color, dark) in base_colors() {
            for delta_l in [0.025_f32, 0.04, 0.06] {
                for level in 1..=4_u8 {
                    let result = ink_color(base_color, level, delta_l, dark);
                    let (dl, da, db) = lab_distance_parts(base_color, result);
                    let expected = f64::from(delta_l) * f64::from(level) / 4.0;
                    let expected = if dark {
                        expected * DARK_DELTA_L_FACTOR
                    } else {
                        -expected
                    };
                    let base_l = to_oklab(base_color)[0];
                    // Lightness is clamped at 1.0, so the expected shift is
                    // the clamped target minus the original lightness.
                    let expected = (base_l + expected).min(1.0) - base_l;
                    assert!(
                        (dl - expected).abs() <= 0.005,
                        "{base_color:06x} level {level} delta {delta_l}: dL {dl} vs {expected}"
                    );
                    // Near white the sRGB gamut collapses to neutral, so chroma
                    // is reduced; only the lightness shift is checked there.
                    if base_l + expected > 0.96 {
                        continue;
                    }
                    assert!(da <= 0.005, "{base_color:06x} da {da}");
                    assert!(db <= 0.005, "{base_color:06x} db {db}");
                }
            }
        }
    }

    #[test]
    fn ink_color_dark_doubles_the_lightness_shift() {
        let base_color = 0xe8e5df_u32;
        let result = ink_color(base_color, 4, 0.04, true);
        // +0.08 requested; lightness is clamped at 1.0.
        let expected = (to_oklab(base_color)[0] + 0.08).min(1.0) - to_oklab(base_color)[0];
        let (dl, _, _) = lab_distance_parts(base_color, result);
        assert!((dl - expected).abs() <= 0.005, "dL {dl} vs {expected}");
        // Clearly stronger than the undoubled shift.
        let undoubled = shift_lightness(base_color, 0.04);
        assert!(to_oklab(result)[0] > to_oklab(undoubled)[0]);
    }

    #[test]
    fn ink_color_light_keeps_the_original_shift() {
        let base_color = 0x262626_u32;
        let result = ink_color(base_color, 4, 0.04, false);
        let (dl, da, db) = lab_distance_parts(base_color, result);
        assert!((dl + 0.04).abs() <= 0.005, "dL {dl}");
        assert!(da <= 0.005 && db <= 0.005);
    }

    #[test]
    fn ink_color_level_zero_is_exact_in_dark() {
        assert_eq!(ink_color(0xe8e5df, 0, 0.04, true), 0xe8e5df);
    }

    #[test]
    fn ink_color_level_zero_is_exact() {
        for theme in [DEFAULT_THEME, DARK_THEME] {
            for color in [
                theme.foreground,
                theme.quote_foreground,
                theme.link_foreground,
                theme.syntax_heading,
                theme.syntax_heading_minor,
                theme.syntax_code,
                theme.syntax_structure,
            ] {
                for dark in [false, true] {
                    assert_eq!(ink_color(color, 0, 0.06, dark), color);
                }
            }
        }
    }

    #[test]
    fn ink_color_contrast_direction_follows_theme() {
        let light = ink_color(DEFAULT_THEME.foreground, 4, 0.04, false);
        assert!(to_oklab(light)[0] < to_oklab(DEFAULT_THEME.foreground)[0]);
        let dark = ink_color(DARK_THEME.foreground, 4, 0.04, true);
        assert!(to_oklab(dark)[0] > to_oklab(DARK_THEME.foreground)[0]);
    }

    #[test]
    fn ink_color_is_a_valid_rgb_for_every_token() {
        for (theme, dark) in [(DEFAULT_THEME, false), (DARK_THEME, true)] {
            for color in [
                theme.foreground,
                theme.quote_foreground,
                theme.link_foreground,
                theme.syntax_heading,
                theme.syntax_heading_minor,
                theme.syntax_code,
                theme.syntax_structure,
            ] {
                for level in 1..=4 {
                    assert!(ink_color(color, level, 0.06, dark) <= 0x00ff_ffff);
                }
            }
        }
    }

    #[test]
    fn oklab_round_trip_is_close() {
        for color in [0x000000_u32, 0xffffff, 0x262626, 0x2867a9, 0xe8e5df, 0x9a3f1c] {
            let [l, a, b] = to_oklab(color);
            let back = from_oklab(l, a, b);
            for shift in [16, 8, 0] {
                let x = i64::from((color >> shift) & 0xff);
                let y = i64::from((back >> shift) & 0xff);
                assert!((x - y).abs() <= 1, "{color:06x} -> {back:06x}");
            }
        }
    }

    #[test]
    fn band_color_moves_toward_the_text_side() {
        for (theme, dark) in [(DEFAULT_THEME, false), (DARK_THEME, true)] {
            let band = band_color(theme.editor_background, 0.03, dark);
            let (dl, da, db) = lab_distance_parts(theme.editor_background, band);
            let expected = if dark { 0.03 } else { -0.03 };
            assert!((dl - expected).abs() <= 0.005, "dL {dl}");
            assert!(da <= 0.005 && db <= 0.005, "da {da} db {db}");
        }
    }

    #[test]
    fn band_theme_token_matches_the_standard_derivation() {
        for (theme, dark) in [(DEFAULT_THEME, false), (DARK_THEME, true)] {
            let derived = band_color(theme.editor_background, 0.03, dark);
            let (dl, da, db) = lab_distance_parts(derived, theme.ink_band_background);
            assert!(dl.abs() <= 0.01, "dL {dl}");
            assert!(da <= 0.01 && db <= 0.01, "da {da} db {db}");
        }
    }

    // --- soak ---

    const STRENGTHS: [DryingInkStrength; 3] = [
        DryingInkStrength::Weak,
        DryingInkStrength::Standard,
        DryingInkStrength::Strong,
    ];

    #[test]
    fn soak_alpha_follows_the_level_and_is_absent_when_dry() {
        for (level, expected) in [(4_u8, 0.72_f32), (3, 0.54), (2, 0.36), (1, 0.18)] {
            let alpha = soak_level_alpha(level);
            assert!((alpha - expected).abs() < 1e-6, "level {level}: {alpha}");
            for strength in STRENGTHS {
                for dark in [false, true] {
                    assert!((soak_alpha(level, 0.0, strength, dark) - expected).abs() < 1e-6);
                }
            }
        }
        assert_eq!(soak_level_alpha(0), 0.0);
        assert_eq!(soak_alpha(0, 0.0, DryingInkStrength::Standard, false), 0.0);
    }

    #[test]
    fn the_band_adds_a_quarter_of_its_opacity_up_to_the_cap() {
        let standard = DryingInkStrength::Standard;
        // 0.36 + 0.25 * 1.0
        assert!((soak_alpha(2, 1.0, standard, false) - 0.61).abs() < 1e-6);
        assert!((soak_alpha(2, 0.5, standard, true) - 0.485).abs() < 1e-6);
        // 0.72 + 0.25 is held at 0.9.
        assert!((soak_alpha(4, 1.0, standard, false) - 0.9).abs() < 1e-6);
        // A dry stretch inside the band still gets the band's own share.
        assert!((soak_alpha(0, 1.0, standard, false) - 0.25).abs() < 1e-6);
        // Strong light themes stop at their table value, never below the soak.
        let strong = DryingInkStrength::Strong;
        assert!((soak_alpha(3, 1.0, strong, false) - 0.72).abs() < 1e-6);
        assert!((soak_alpha(4, 1.0, strong, false) - 0.72).abs() < 1e-6);
        assert!((soak_alpha(4, 1.0, strong, true) - 0.9).abs() < 1e-6);
    }

    #[test]
    fn the_soak_color_is_darker_than_the_background_in_both_themes() {
        for (theme, dark) in [(DEFAULT_THEME, false), (DARK_THEME, true)] {
            for strength in STRENGTHS {
                let (light_depth, dark_depth) = strength.soak_depth();
                let depth = if dark { dark_depth } else { light_depth };
                let color = soak_color(theme.editor_background, depth);
                let (dl, da, db) = lab_distance_parts(theme.editor_background, color);
                assert!((dl + f64::from(depth)).abs() <= 0.005, "dL {dl}");
                assert!(da <= 0.005 && db <= 0.005, "da {da} db {db}");
            }
        }
    }

    fn channel_luminance(channels: [f64; 3]) -> f64 {
        let linear = |value: f64| {
            let value = value / 255.0;
            if value <= 0.03928 {
                value / 12.92
            } else {
                ((value + 0.055) / 1.055).powf(2.4)
            }
        };
        0.2126 * linear(channels[0]) + 0.7152 * linear(channels[1]) + 0.0722 * linear(channels[2])
    }

    fn rgb_channels(color: u32) -> [f64; 3] {
        [16, 8, 0].map(|shift| f64::from((color >> shift) & 0xff))
    }

    /// The tile pixel the soak is hardest on: the darkest one in a light theme,
    /// the brightest one in a dark theme.
    fn worst_texture_pixel(theme: &crate::theme::Theme, dark: bool) -> u32 {
        let budget = if dark {
            crate::paper_texture::DARK_BUDGET.max
        } else {
            crate::paper_texture::LIGHT_BUDGET.min
        };
        shift_lightness(theme.editor_background, f64::from(budget))
    }

    /// Alpha blending in sRGB channels, as the GPU does it.
    fn blend(texture: u32, soak: u32, alpha: f32) -> [f64; 3] {
        let alpha = f64::from(alpha);
        let (texture, soak) = (rgb_channels(texture), rgb_channels(soak));
        [0, 1, 2].map(|i| texture[i] * (1.0 - alpha) + soak[i] * alpha)
    }

    #[test]
    fn soak_and_band_keep_the_background_and_every_text_color_readable() {
        const BAND_OPACITIES: [f32; 5] = [0.0, 0.25, 0.5, 0.75, 1.0];
        for (theme, dark) in [(DEFAULT_THEME, false), (DARK_THEME, true)] {
            let tokens = [
                theme.foreground,
                theme.quote_foreground,
                theme.link_foreground,
                theme.syntax_heading,
                theme.syntax_heading_minor,
                theme.syntax_code,
                theme.syntax_structure,
            ];
            let texture = worst_texture_pixel(&theme, dark);
            for strength in STRENGTHS {
                let (light_depth, dark_depth) = strength.soak_depth();
                let depth = if dark { dark_depth } else { light_depth };
                let soak = soak_color(theme.editor_background, depth);
                for level in 0..=WET_LEVELS {
                    for band in BAND_OPACITIES {
                        let alpha = soak_alpha(level, band, strength, dark);
                        assert!(alpha <= SOAK_ALPHA_MAX + 1e-6);
                        let composite = blend(texture, soak, alpha);
                        let y = channel_luminance(composite);
                        let label = format!("dark={dark} {strength:?} level {level} band {band}");
                        if dark {
                            assert!(y <= SOAK_DARK_CEILING_L.powi(3), "{label}: Y {y}");
                        } else {
                            assert!(y >= SOAK_LIGHT_FLOOR_L.powi(3), "{label}: Y {y}");
                        }
                        for token in tokens {
                            let t = channel_luminance(rgb_channels(token));
                            let contrast = (t.max(y) + 0.05) / (t.min(y) + 0.05);
                            assert!(contrast >= 4.5, "{label}: {token:06x} contrast {contrast}");
                        }
                    }
                }
            }
        }
    }

    #[test]
    fn the_alpha_cap_table_is_fixed() {
        assert_eq!(SOAK_ALPHA_CAPS, [[0.9, 0.9], [0.9, 0.9], [0.72, 0.9]]);
        assert_eq!(soak_alpha_cap(DryingInkStrength::Strong, false), 0.72);
        assert_eq!(soak_alpha_cap(DryingInkStrength::Strong, true), 0.9);
        assert_eq!(soak_alpha_cap(DryingInkStrength::Weak, false), 0.9);
    }

    #[test]
    fn the_soak_adds_no_wakeups() {
        let t = base();
        let walk = |soak_enabled: bool| {
            let p = InkParams {
                soak_enabled,
                ..InkParams::default()
            };
            let mut ledger = InkLedger::new(t);
            ledger.record_commit(SourceRange::new(0, 3), "あ。", t, &p);
            let mut now = t;
            let mut seen = Vec::new();
            while let Some(wakeup) = ledger.next_wakeup(now, &p) {
                seen.push(wakeup.duration_since(t));
                now = wakeup;
                ledger.frame(now, &p);
            }
            seen
        };
        let with_soak = walk(true);
        assert_eq!(with_soak, walk(false));
        assert_eq!(
            with_soak,
            vec![
                millis(600),
                millis(700),
                millis(800),
                millis(900),
                secs(15),
                secs(30),
                secs(45),
                secs(60)
            ]
        );
    }
}
