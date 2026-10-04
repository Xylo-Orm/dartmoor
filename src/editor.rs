//! Pure editing operations shared by canvas and keyboard interactions.

use crate::{
    config::Config,
    core::{CaptureSelection, Light, Point, Route, Shape},
};
use std::{collections::VecDeque, mem::size_of};

pub const MAX_HISTORY: usize = 64;
pub const MAX_HISTORY_BYTES: usize = 8 * 1024 * 1024;
pub const DEFAULT_STRIP_INSET: f32 = 0.04;
const MAX_PATH_POINTS: usize = 128;
const MAX_STRIP_INSET: f32 = 0.49;

/// Monitor-edge paths in clockwise screen order (screen y increases downward).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StripPreset {
    /// Left to right.
    Top,
    /// Right to left.
    Bottom,
    /// Bottom to top.
    Left,
    /// Top to bottom.
    Right,
    /// Bottom-left → top-left → top-right → bottom-right.
    ThreeSides,
    /// Bottom-left → top-left → top-right → bottom-right → bottom-left.
    Perimeter,
}

/// Return a normalized edge path. Insets clamp to 0..=0.49, preserving nonzero
/// length even for extreme input. NaN uses the practical default of 0.04;
/// positive/negative infinity clamp to the upper/lower limit respectively.
pub fn strip_preset_points(preset: StripPreset, inset: f32) -> Vec<Point> {
    let inset = if inset.is_nan() {
        DEFAULT_STRIP_INSET
    } else {
        inset.clamp(0.0, MAX_STRIP_INSET)
    };
    let far = 1.0 - inset;
    let top_left = Point { x: inset, y: inset };
    let top_right = Point { x: far, y: inset };
    let bottom_left = Point { x: inset, y: far };
    let bottom_right = Point { x: far, y: far };
    match preset {
        StripPreset::Top => vec![top_left, top_right],
        StripPreset::Bottom => vec![bottom_right, bottom_left],
        StripPreset::Left => vec![bottom_left, top_left],
        StripPreset::Right => vec![top_right, bottom_right],
        StripPreset::ThreeSides => vec![bottom_left, top_left, top_right, bottom_right],
        StripPreset::Perimeter => {
            vec![bottom_left, top_left, top_right, bottom_right, bottom_left]
        }
    }
}

#[derive(Debug)]
struct Snapshot {
    config: Config,
    bytes: usize,
}

/// Configuration history. Call `checkpoint` once at the end of an interaction,
/// passing the configuration from before the interaction and its final state.
#[derive(Debug, Default)]
pub struct EditHistory {
    undo: VecDeque<Snapshot>,
    redo: VecDeque<Snapshot>,
}

impl EditHistory {
    /// Empty transactions do not consume history or invalidate redo.
    /// Returns whether a snapshot was recorded. An oversized before-state clears
    /// history so a later undo cannot silently jump over the unrecorded edit.
    pub fn checkpoint(&mut self, before: &Config, after: &Config) -> bool {
        if before == after {
            return false;
        }
        self.redo.clear();
        let bytes = config_bytes(before);
        if bytes > MAX_HISTORY_BYTES {
            self.undo.clear();
            return false;
        }
        self.make_room(bytes, true);
        push_bounded(&mut self.undo, before, bytes);
        true
    }

    pub fn undo(&mut self, current: &Config) -> Option<Config> {
        let previous = self.undo.pop_back()?;
        let bytes = config_bytes(current);
        if bytes > MAX_HISTORY_BYTES {
            // Restore the target, but do not expose redo when the current state
            // cannot be retained. Existing older undo states remain reachable.
            self.redo.clear();
        } else {
            self.make_room(bytes, false);
            push_bounded(&mut self.redo, current, bytes);
        }
        Some(previous.config)
    }

    pub fn redo(&mut self, current: &Config) -> Option<Config> {
        let next = self.redo.pop_back()?;
        let bytes = config_bytes(current);
        if bytes > MAX_HISTORY_BYTES {
            self.undo.clear();
        } else {
            self.make_room(bytes, true);
            push_bounded(&mut self.undo, current, bytes);
        }
        Some(next.config)
    }

    pub fn can_undo(&self) -> bool {
        !self.undo.is_empty()
    }

    pub fn can_redo(&self) -> bool {
        !self.redo.is_empty()
    }

    /// Approximate memory retained by all configuration snapshots. Counts inline
    /// structs and cloned heap data, excluding allocator and deque bookkeeping.
    pub fn retained_bytes(&self) -> usize {
        self.undo
            .iter()
            .chain(&self.redo)
            .fold(0usize, |total, snapshot| {
                total.saturating_add(snapshot.bytes)
            })
    }

    fn make_room(&mut self, bytes: usize, added_undo: bool) {
        // Release discarded snapshots before cloning the next one, avoiding a
        // temporary allocation above the retained-history budget.
        while self.retained_bytes().saturating_add(bytes) > MAX_HISTORY_BYTES {
            // Discard the farthest state in the opposite direction first, then
            // the oldest state in the direction receiving the new snapshot.
            let removed = if added_undo {
                self.redo.pop_front().or_else(|| self.undo.pop_front())
            } else {
                self.undo.pop_front().or_else(|| self.redo.pop_front())
            };
            if removed.is_none() {
                break;
            }
        }
    }
}

fn push_bounded(history: &mut VecDeque<Snapshot>, config: &Config, bytes: usize) {
    if history.len() == MAX_HISTORY {
        history.pop_front();
    }
    history.push_back(Snapshot {
        config: config.clone(),
        bytes,
    });
}

/// Cloning Config gives strings and vectors capacity for their current length.
/// Estimate those allocations directly; avoid serializing large drafts merely
/// to decide whether retaining a snapshot is affordable.
fn config_bytes(config: &Config) -> usize {
    let mut bytes = size_of::<Config>()
        .saturating_add(config.ha_url.len())
        .saturating_add(config.lights.len().saturating_mul(size_of::<Light>()));
    if let CaptureSelection::Desktop { id: Some(id) } = &config.source {
        bytes = bytes.saturating_add(id.len());
    }
    for light in &config.lights {
        bytes = bytes
            .saturating_add(light.id.len())
            .saturating_add(light.name.len());
        if let Shape::Strip { points, .. } = &light.shape {
            bytes = bytes.saturating_add(points.len().saturating_mul(size_of::<Point>()));
        }
        match &light.route {
            Route::Wled {
                host, device_id, ..
            } => {
                bytes = bytes
                    .saturating_add(host.len())
                    .saturating_add(device_id.len());
            }
            Route::HomeAssistant { entity_id } => {
                bytes = bytes.saturating_add(entity_id.len());
            }
            Route::Mock => {}
        }
    }
    bytes
}

/// Zone centers along a normalized polyline, in output order. Each center is
/// halfway through its equal-arclength zone; bulbs repeat their center.
pub fn zone_positions(shape: &Shape, zones: usize) -> Vec<Point> {
    zone_positions_scaled(shape, zones, 1.0, 1.0)
}

/// Normalized zone centers using the frame's physical aspect ratio for path
/// lengths, matching the sampling plan's equal-arclength divisions.
pub fn zone_positions_scaled(shape: &Shape, zones: usize, width: f32, height: f32) -> Vec<Point> {
    if zones == 0 || !width.is_finite() || !height.is_finite() || width <= 0.0 || height <= 0.0 {
        return Vec::new();
    }
    match shape {
        Shape::Bulb { center, .. } => vec![*center; zones],
        Shape::Strip {
            points, reverse, ..
        } => {
            let Some(&first) = points.first() else {
                return Vec::new();
            };
            if points.iter().any(|point| !finite(*point)) {
                return Vec::new();
            }
            let lengths: Vec<f32> = points
                .windows(2)
                .map(|pair| {
                    ((pair[1].x - pair[0].x) * width).hypot((pair[1].y - pair[0].y) * height)
                })
                .collect();
            let total: f32 = lengths.iter().sum();
            if !total.is_finite() {
                return Vec::new();
            }
            if total == 0.0 {
                return vec![first; zones];
            }
            let mut positions = Vec::with_capacity(zones);
            let mut segment = 0;
            let mut offset = 0.0;
            for zone in 0..zones {
                let target = total * (zone as f32 + 0.5) / zones as f32;
                while segment + 1 < lengths.len()
                    && (lengths[segment] == 0.0 || offset + lengths[segment] < target)
                {
                    offset += lengths[segment];
                    segment += 1;
                }
                let fraction = ((target - offset) / lengths[segment]).clamp(0.0, 1.0);
                positions.push(interpolate(points[segment], points[segment + 1], fraction));
            }
            if *reverse {
                positions.reverse();
            }
            positions
        }
    }
}

/// Return the closest nonzero segment, its projected point, and the distance.
/// Coordinates may be normalized or canvas pixels; distances use the same unit.
pub fn nearest_segment(points: &[Point], position: Point) -> Option<(usize, Point, f32)> {
    if !finite(position) {
        return None;
    }
    points
        .windows(2)
        .enumerate()
        .filter_map(|(index, pair)| {
            let projected = project_segment(pair[0], pair[1], position)?;
            Some((index, projected, distance(projected, position)))
        })
        .min_by(|left, right| left.2.total_cmp(&right.2))
}

/// Insert on the nearest segment. The caller applies its canvas hit threshold
/// before calling this helper. Endpoints and zero-length segments add no point.
pub fn insert_path_point(points: &mut Vec<Point>, position: Point) -> bool {
    if points.len() >= MAX_PATH_POINTS || !finite(position) {
        return false;
    }
    let position = normalized(position);
    let Some((segment, _, _)) = nearest_segment(points, position) else {
        return false;
    };
    insert_path_point_at(points, segment, position)
}

/// Insert on a designated segment, preserving its identity even when a path
/// overlaps or retraces another segment. Position is projected onto that segment.
pub fn insert_path_point_at(points: &mut Vec<Point>, segment: usize, position: Point) -> bool {
    if points.len() >= MAX_PATH_POINTS || !finite(position) {
        return false;
    }
    let Some(pair) = points.get(segment..).and_then(|tail| tail.get(..2)) else {
        return false;
    };
    let Some(projected) = project_segment(pair[0], pair[1], normalized(position)) else {
        return false;
    };
    let projected = normalized(projected);
    if projected == pair[0] || projected == pair[1] {
        return false;
    }
    points.insert(segment + 1, projected);
    true
}

/// Keep at least two handles and a nonzero path when removing a handle.
pub fn remove_path_point(points: &mut Vec<Point>, index: usize) -> bool {
    if points.len() <= 2 || index >= points.len() {
        return false;
    }
    let mut retained = points.iter().enumerate().filter(|(i, _)| *i != index);
    let Some((_, first)) = retained.next() else {
        return false;
    };
    if retained.all(|(_, point)| point == first) {
        return false;
    }
    points.remove(index);
    true
}

/// Move one selected handle by a normalized delta, clamping to the canvas.
/// Bulb centers use handle index zero.
pub fn move_selected(shape: &mut Shape, point_index: usize, delta: Point) -> bool {
    if !finite(delta) {
        return false;
    }
    let point = match shape {
        Shape::Bulb { center, .. } if point_index == 0 => center,
        Shape::Strip { points, .. } => {
            let Some(point) = points.get_mut(point_index) else {
                return false;
            };
            point
        }
        _ => return false,
    };
    if !finite(*point) {
        return false;
    }
    let moved = normalized(Point {
        x: point.x + delta.x,
        y: point.y + delta.y,
    });
    if *point == moved {
        return false;
    }
    *point = moved;
    true
}

fn finite(point: Point) -> bool {
    point.x.is_finite() && point.y.is_finite()
}

fn normalized(point: Point) -> Point {
    Point {
        x: point.x.clamp(0.0, 1.0),
        y: point.y.clamp(0.0, 1.0),
    }
}

fn distance(a: Point, b: Point) -> f32 {
    (a.x - b.x).hypot(a.y - b.y)
}

fn project_segment(a: Point, b: Point, position: Point) -> Option<Point> {
    if !finite(a) || !finite(b) || !finite(position) {
        return None;
    }
    let dx = b.x - a.x;
    let dy = b.y - a.y;
    let length_squared = dx * dx + dy * dy;
    if length_squared == 0.0 || !length_squared.is_finite() {
        return None;
    }
    let fraction =
        (((position.x - a.x) * dx + (position.y - a.y) * dy) / length_squared).clamp(0.0, 1.0);
    Some(interpolate(a, b, fraction))
}

fn interpolate(a: Point, b: Point, fraction: f32) -> Point {
    Point {
        x: a.x + (b.x - a.x) * fraction,
        y: a.y + (b.y - a.y) * fraction,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn point(x: f32, y: f32) -> Point {
        Point { x, y }
    }

    fn strip(points: Vec<Point>, reverse: bool) -> Shape {
        Shape::Strip {
            points,
            radius: 0.05,
            reverse,
        }
    }

    fn assert_point(actual: Point, expected: Point) {
        assert!(
            distance(actual, expected) < 0.00001,
            "{actual:?} != {expected:?}"
        );
    }

    #[test]
    fn edge_presets_have_consistent_clockwise_direction_and_default_margin() {
        for (preset, expected) in [
            (StripPreset::Top, [point(0.04, 0.04), point(0.96, 0.04)]),
            (StripPreset::Bottom, [point(0.96, 0.96), point(0.04, 0.96)]),
            (StripPreset::Left, [point(0.04, 0.96), point(0.04, 0.04)]),
            (StripPreset::Right, [point(0.96, 0.04), point(0.96, 0.96)]),
        ] {
            let points = strip_preset_points(preset, DEFAULT_STRIP_INSET);
            assert_eq!(points.len(), 2);
            assert_point(points[0], expected[0]);
            assert_point(points[1], expected[1]);
        }
    }

    #[test]
    fn three_sides_is_open_and_perimeter_closes_in_the_same_order() {
        let open = strip_preset_points(StripPreset::ThreeSides, 0.1);
        let expected = [
            point(0.1, 0.9),
            point(0.1, 0.1),
            point(0.9, 0.1),
            point(0.9, 0.9),
        ];
        assert_eq!(open.len(), expected.len());
        for (&actual, expected) in open.iter().zip(expected) {
            assert_point(actual, expected);
        }
        assert_ne!(open.first(), open.last());
        let closed = strip_preset_points(StripPreset::Perimeter, 0.1);
        assert_eq!(&closed[..4], &open);
        assert_eq!(closed.first(), closed.last());
        assert_eq!(closed.len(), 5);
        // Four equal square-edge zones land at the midpoint of each edge,
        // including the closing bottom segment.
        for (actual, expected) in zone_positions(&strip(closed, false), 4).into_iter().zip([
            point(0.1, 0.5),
            point(0.5, 0.1),
            point(0.9, 0.5),
            point(0.5, 0.9),
        ]) {
            assert_point(actual, expected);
        }
    }

    #[test]
    fn extreme_preset_insets_produce_finite_normalized_valid_geometry() {
        for preset in [
            StripPreset::Top,
            StripPreset::Bottom,
            StripPreset::Left,
            StripPreset::Right,
            StripPreset::ThreeSides,
            StripPreset::Perimeter,
        ] {
            for inset in [
                f32::NEG_INFINITY,
                f32::MIN,
                -0.5,
                0.0,
                0.04,
                0.49,
                0.5,
                1.0,
                f32::MAX,
                f32::INFINITY,
                f32::NAN,
            ] {
                let points = strip_preset_points(preset, inset);
                assert!(points.iter().all(|point| finite(*point)
                    && (0.0..=1.0).contains(&point.x)
                    && (0.0..=1.0).contains(&point.y)));
                assert!(points.windows(2).all(|pair| pair[0] != pair[1]));
                let mut config = Config::default();
                config.lights[0].shape = strip(points, false);
                assert!(config.validate().is_ok(), "{preset:?}, inset={inset}");
            }
            assert_eq!(
                strip_preset_points(preset, f32::NAN),
                strip_preset_points(preset, DEFAULT_STRIP_INSET)
            );
            assert_eq!(
                strip_preset_points(preset, f32::INFINITY),
                strip_preset_points(preset, 0.49)
            );
            assert_eq!(
                strip_preset_points(preset, f32::NEG_INFINITY),
                strip_preset_points(preset, 0.0)
            );
        }
    }

    #[test]
    fn drag_transaction_is_one_undo_and_redo_restores_its_final_state() {
        let before = Config::default();
        let mut after = before.clone();
        for _ in 0..12 {
            move_selected(&mut after.lights[0].shape, 0, point(0.01, -0.01));
        }
        let mut history = EditHistory::default();
        assert!(!history.can_undo());
        assert!(history.checkpoint(&before, &after));
        assert_eq!(history.undo(&after), Some(before.clone()));
        assert!(!history.can_undo());
        assert!(history.can_redo());
        assert!(!history.checkpoint(&before, &before));
        assert_eq!(history.redo(&before), Some(after.clone()));
        assert!(!history.can_redo());
        assert_eq!(history.undo(&after), Some(before.clone()));
        let mut branch = before.clone();
        branch.fps += 1;
        history.checkpoint(&before, &branch);
        assert!(!history.can_redo());
        assert_eq!(history.undo(&branch), Some(before));
    }

    #[test]
    fn history_evicts_oldest_transactions_and_keeps_redo_bounded() {
        let mut history = EditHistory::default();
        let mut current = Config::default();
        for index in 1..=MAX_HISTORY + 5 {
            let before = current.clone();
            current.lights[0].name = format!("Edit {index}");
            history.checkpoint(&before, &current);
        }
        for _ in 0..MAX_HISTORY {
            current = history.undo(&current).unwrap();
        }
        assert_eq!(current.lights[0].name, "Edit 5");
        assert!(history.undo(&current).is_none());
        for _ in 0..MAX_HISTORY {
            current = history.redo(&current).unwrap();
        }
        assert_eq!(current.lights[0].name, "Edit 69");
        assert!(history.redo(&current).is_none());
    }

    #[test]
    fn oversized_checkpoint_clears_inaccessible_history_without_retaining_it() {
        let before = Config::default();
        let mut edited = before.clone();
        edited.fps += 1;
        let mut history = EditHistory::default();
        history.checkpoint(&before, &edited);
        let mut oversized = edited;
        oversized.lights[0].name = "x".repeat(MAX_HISTORY_BYTES + 1);
        let mut after = oversized.clone();
        after.fps += 1;
        assert!(!history.checkpoint(&oversized, &after));
        assert_eq!(history.retained_bytes(), 0);
        assert!(!history.can_undo());
        assert!(!history.can_redo());
        assert!(history.undo(&after).is_none());
        assert!(history.redo(&after).is_none());
        // History resumes when a representable before-state is available.
        let mut valid_edit = before.clone();
        valid_edit.fps += 2;
        assert!(history.checkpoint(&before, &valid_edit));
        assert_eq!(history.undo(&valid_edit), Some(before));
    }

    #[test]
    fn oversized_current_can_be_restored_without_an_unrepresentable_reverse_step() {
        let base = Config::default();
        let mut first = base.clone();
        first.fps += 1;
        let mut second = first.clone();
        second.fps += 1;
        let mut huge = second.clone();
        huge.lights[0].name = "x".repeat(MAX_HISTORY_BYTES + 1);
        let mut history = EditHistory::default();
        history.checkpoint(&base, &first);
        history.checkpoint(&first, &huge);
        assert_eq!(history.undo(&huge), Some(first.clone()));
        assert!(!history.can_redo());
        assert!(history.can_undo());
        assert!(history.retained_bytes() <= MAX_HISTORY_BYTES);
        assert_eq!(history.undo(&first), Some(base.clone()));

        let mut history = EditHistory::default();
        history.checkpoint(&base, &first);
        history.checkpoint(&first, &second);
        assert_eq!(history.undo(&second), Some(first.clone()));
        assert_eq!(history.undo(&first), Some(base.clone()));
        assert_eq!(history.redo(&huge), Some(first.clone()));
        assert!(!history.can_undo());
        assert!(history.can_redo());
        assert!(history.retained_bytes() <= MAX_HISTORY_BYTES);
        let restored = history.redo(&first).unwrap();
        assert_eq!(restored, second);
        assert_eq!(history.undo(&restored), Some(first));
    }

    #[test]
    fn memory_pruning_keeps_recent_states_contiguous_through_undo_and_redo() {
        let mut history = EditHistory::default();
        let mut current = Config::default();
        for step in 1..=8 {
            let before = current.clone();
            current.fps += 1;
            current.lights[0].name = "x".repeat(2 * 1024 * 1024 + step * 64 * 1024);
            assert!(history.checkpoint(&before, &current));
            assert!(history.retained_bytes() <= MAX_HISTORY_BYTES);
        }
        let final_fps = current.fps;
        let mut undo_count = 0;
        while let Some(previous) = history.undo(&current) {
            assert_eq!(previous.fps + 1, current.fps);
            current = previous;
            undo_count += 1;
            assert!(history.retained_bytes() <= MAX_HISTORY_BYTES);
        }
        assert!(undo_count > 0 && undo_count < 8);
        assert!(!history.can_undo());
        let mut redo_count = 0;
        while let Some(next) = history.redo(&current) {
            assert_eq!(next.fps, current.fps + 1);
            current = next;
            redo_count += 1;
            assert!(history.retained_bytes() <= MAX_HISTORY_BYTES);
        }
        assert_eq!(redo_count, undo_count);
        assert_eq!(current.fps, final_fps);
        assert!(!history.can_redo());
    }

    #[test]
    fn zone_centers_follow_arclength_across_corners_and_reverse_output() {
        let points = vec![point(0.0, 0.0), point(1.0, 0.0), point(1.0, 0.5)];
        let positions = zone_positions(&strip(points.clone(), false), 3);
        for (actual, expected) in
            positions
                .iter()
                .zip([point(0.25, 0.0), point(0.75, 0.0), point(1.0, 0.25)])
        {
            assert_point(*actual, expected);
        }
        let mut reversed = positions;
        reversed.reverse();
        assert_eq!(zone_positions(&strip(points, true), 3), reversed);
        let bulb = Shape::Bulb {
            center: point(0.4, 0.6),
            radius: 0.1,
        };
        assert_eq!(zone_positions(&bulb, 3), vec![point(0.4, 0.6); 3]);
        assert!(zone_positions(&bulb, 0).is_empty());
    }

    #[test]
    fn duplicate_handles_do_not_interrupt_zone_placement() {
        let shape = strip(
            vec![point(0.0, 0.0), point(0.0, 0.0), point(1.0, 0.0)],
            false,
        );
        assert_eq!(
            zone_positions(&shape, 2),
            vec![point(0.25, 0.0), point(0.75, 0.0)]
        );
    }

    #[test]
    fn widescreen_zone_centers_use_physical_arclength() {
        let points = vec![point(0.0, 0.0), point(1.0, 0.0), point(1.0, 1.0)];
        let shape = strip(points.clone(), false);
        let positions = zone_positions_scaled(&shape, 5, 16.0, 9.0);
        // The path is 25 pixels long. Zone centers occur every five pixels,
        // starting at 2.5 pixels; the first three lie on the 16-pixel leg.
        for (actual, expected) in positions.iter().zip([
            point(2.5 / 16.0, 0.0),
            point(7.5 / 16.0, 0.0),
            point(12.5 / 16.0, 0.0),
            point(1.0, 1.5 / 9.0),
            point(1.0, 6.5 / 9.0),
        ]) {
            assert_point(*actual, expected);
        }
        assert_point(
            zone_positions_scaled(&shape, 1, 160.0, 90.0)[0],
            point(12.5 / 16.0, 0.0),
        );
        let mut reversed = positions;
        reversed.reverse();
        assert_eq!(
            zone_positions_scaled(&strip(points, true), 5, 16.0, 9.0),
            reversed
        );
        assert!(zone_positions_scaled(&shape, 5, 0.0, 9.0).is_empty());
        assert!(zone_positions_scaled(&shape, 5, 16.0, f32::NAN).is_empty());
    }

    #[test]
    fn insertion_projects_to_nearest_segment_and_preserves_bounds() {
        let mut points = vec![point(0.0, 0.0), point(1.0, 0.0), point(1.0, 1.0)];
        let (segment, projected, distance) = nearest_segment(&points, point(0.7, 0.8)).unwrap();
        assert_eq!(segment, 1);
        assert_point(projected, point(1.0, 0.8));
        assert!((distance - 0.3).abs() < 0.00001);
        assert!(insert_path_point(&mut points, point(0.7, 0.8)));
        assert_point(points[2], point(1.0, 0.8));
        assert!(!insert_path_point(&mut points, point(4.0, 4.0)));
        let mut outside = vec![point(0.0, 0.5), point(1.0, 0.5)];
        assert!(insert_path_point(&mut outside, point(0.3, -2.0)));
        assert_point(outside[1], point(0.3, 0.5));
        let mut full: Vec<_> = (0..MAX_PATH_POINTS)
            .map(|i| point(i as f32 / (MAX_PATH_POINTS - 1) as f32, 0.5))
            .collect();
        assert!(!insert_path_point(&mut full, point(0.5, 0.5)));
        assert!(!insert_path_point(&mut outside, point(f32::NAN, 0.5)));
        assert!(nearest_segment(&[point(0.2, 0.2); 2], point(0.3, 0.3)).is_none());
    }

    #[test]
    fn removal_retains_valid_path_and_nudges_clamp_selected_handle_only() {
        let mut points = vec![point(0.0, 0.0), point(0.5, 0.5), point(1.0, 1.0)];
        assert!(!remove_path_point(&mut points, 3));
        assert!(remove_path_point(&mut points, 1));
        assert!(!remove_path_point(&mut points, 0));
        let mut doubled_back = vec![point(0.0, 0.0), point(1.0, 1.0), point(0.0, 0.0)];
        assert!(!remove_path_point(&mut doubled_back, 1));
        let mut shape = strip(points, false);
        assert!(move_selected(&mut shape, 0, point(2.0, -1.0)));
        if let Shape::Strip { points, .. } = &shape {
            assert_eq!(*points, vec![point(1.0, 0.0), point(1.0, 1.0)]);
        }
        assert!(!move_selected(&mut shape, 0, point(0.1, -0.1)));
        assert!(!move_selected(&mut shape, 9, point(0.1, 0.1)));
        assert!(!move_selected(&mut shape, 0, point(f32::NAN, 0.1)));
        let mut bulb = Shape::Bulb {
            center: point(0.5, 0.5),
            radius: 0.1,
        };
        assert!(!move_selected(&mut bulb, 1, point(0.1, 0.1)));
        assert!(move_selected(&mut bulb, 0, point(-1.0, 1.0)));
        assert!(matches!(bulb, Shape::Bulb { center, .. } if center == point(0.0, 1.0)));
    }

    #[test]
    fn designated_insertion_preserves_retraced_segment_identity() {
        let a = point(0.0, 0.5);
        let b = point(1.0, 0.5);
        let mut points = vec![a, b, a, b];
        assert!(insert_path_point_at(&mut points, 2, point(0.4, 0.8)));
        assert_eq!(points, vec![a, b, a, point(0.4, 0.5), b]);
        let unchanged = points.clone();
        assert!(!insert_path_point_at(
            &mut points,
            usize::MAX,
            point(0.5, 0.5)
        ));
        assert!(!insert_path_point_at(&mut points, 4, point(0.5, 0.5)));
        assert!(!insert_path_point_at(&mut points, 0, a));
        assert!(!insert_path_point_at(
            &mut points,
            0,
            point(f32::INFINITY, 0.5)
        ));
        assert_eq!(points, unchanged);
        let mut repeated = vec![a, a, b];
        assert!(!insert_path_point_at(&mut repeated, 0, point(0.5, 0.5)));
        let mut full = vec![a; MAX_PATH_POINTS];
        full[1] = b;
        assert!(!insert_path_point_at(&mut full, 0, point(0.5, 0.5)));
    }
}
