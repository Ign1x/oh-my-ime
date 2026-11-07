//! Where engine windows go for X11 applications.
//!
//! X11 applications report their text cursor in X11 screen coordinates. Compositors map
//! those to their layout differently (Hyprland with `xwayland:force_zero_scaling` uses
//! physical pixels, sway logical ones), but XWayland describes its monitors through
//! RandR under the names of the compositor's outputs. Matching the names maps any point.

/// `(x, y, width, height)`
pub type Rect = (i32, i32, i32, i32);

/// The largest coordinates and dimensions accepted from a client protocol.
pub(crate) const COORDINATE_LIMIT: i32 = 1 << 24;
pub(crate) const SIZE_LIMIT: i32 = 1 << 16;

/// Clamps a protocol rectangle before it can reach placement arithmetic.
pub(crate) fn clamp_rect((x, y, w, h): Rect) -> Rect {
    (
        x.clamp(-COORDINATE_LIMIT, COORDINATE_LIMIT),
        y.clamp(-COORDINATE_LIMIT, COORDINATE_LIMIT),
        w.clamp(0, SIZE_LIMIT),
        h.clamp(0, SIZE_LIMIT),
    )
}

/// Converts an integer calculation back to the protocol's coordinate type.
pub(crate) fn saturate_i32(value: i64) -> i32 {
    value.clamp(i64::from(i32::MIN), i64::from(i32::MAX)) as i32
}

/// A monitor: of the X screen (in X11 pixels) or of the compositor (in its logical
/// coordinates).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Monitor {
    pub name: String,
    pub rect: Rect,
}

/// The squared distance from `point` to the nearest pixel of `rect` (0 inside).
fn distance((x, y, w, h): Rect, (px, py): (i32, i32)) -> i64 {
    let (x, y, w, h) = (i64::from(x), i64::from(y), i64::from(w), i64::from(h));
    let (px, py) = (i64::from(px), i64::from(py));
    let (right, bottom) = (x + w.max(1) - 1, y + h.max(1) - 1);
    let dx = (x - px).max(px - right).max(0);
    let dy = (y - py).max(py - bottom).max(0);
    dx.saturating_mul(dx).saturating_add(dy.saturating_mul(dy))
}

/// Maps one coordinate between two monitor rectangles without integer overflow.
fn map(value: i32, from: i32, to: i32, scale: f64) -> i32 {
    let delta = i64::from(value) - i64::from(from);
    let scaled = (delta as f64 * scale).round() as i64;
    saturate_i32(i64::from(to).saturating_add(scaled))
}

/// Maps the X11 rectangle `rect` to compositor coordinates, by the X11 monitor nearest
/// to its centre (an application may report a cursor partly off its monitor). Without
/// monitors (or names that match), X11 and compositor coordinates are taken to be the
/// same.
pub fn x11_to_layout(rect: Rect, x11: &[Monitor], outputs: &[Monitor]) -> Rect {
    let (x, y, w, h) = rect;
    let centre = (
        saturate_i32(i64::from(x) + i64::from(w) / 2),
        saturate_i32(i64::from(y) + i64::from(h) / 2),
    );
    let pair = x11
        .iter()
        .filter_map(|m| outputs.iter().find(|o| o.name == m.name).map(|o| (m, o)))
        .min_by_key(|(m, _)| distance(m.rect, centre));
    let Some((from, to)) = pair else {
        return rect;
    };
    let (fx, fy, fw, fh) = from.rect;
    let (tx, ty, tw, th) = to.rect;
    if fw <= 0 || fh <= 0 {
        return rect;
    }
    let scale_x = f64::from(tw) / f64::from(fw);
    let scale_y = f64::from(th) / f64::from(fh);
    (
        map(x, fx, tx, scale_x),
        map(y, fy, ty, scale_y),
        saturate_i32((f64::from(w) * scale_x).round() as i64),
        saturate_i32((f64::from(h) * scale_y).round() as i64),
    )
}

/// The output `point` is on, or the nearest one.
pub fn output_at(outputs: &[Monitor], point: (i32, i32)) -> Option<&Monitor> {
    outputs.iter().min_by_key(|o| distance(o.rect, point))
}

/// Where a `size` window goes for the text cursor `cursor` on `output`: below the
/// cursor, above it when there is no room below, and moved left when it would leave
/// the output on the right.
pub fn place(cursor: Rect, size: (i32, i32), output: Rect) -> (i32, i32) {
    let (cx, cy, _, ch) = cursor;
    let (w, h) = size;
    let (ox, oy, ow, oh) = output;
    let (cx, cy, ch) = (i64::from(cx), i64::from(cy), i64::from(ch));
    let (w, h) = (i64::from(w), i64::from(h));
    let (ox, oy, ow, oh) = (i64::from(ox), i64::from(oy), i64::from(ow), i64::from(oh));
    let mut x = cx;
    let mut y = cy + ch;
    if y + h > oy + oh {
        y = cy - h;
    }
    if x + w > ox + ow {
        x = ox + ow - w;
    }
    (saturate_i32(x.max(ox)), saturate_i32(y.max(oy)))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn monitor(name: &str, rect: Rect) -> Monitor {
        Monitor {
            name: name.into(),
            rect,
        }
    }

    #[test]
    fn extreme_rectangles_map_without_overflow() {
        let x11 = [monitor("A", (0, 0, 100, 100))];
        let outputs = [monitor("A", (0, 0, 200, 200))];
        for value in [i32::MIN, i32::MAX] {
            let rect = (value, value, value, value);
            assert_eq!(x11_to_layout(rect, &[], &[]), rect);
            assert_eq!(x11_to_layout(rect, &x11, &outputs), rect);
        }
        let x11 = [monitor("A", (i32::MIN, i32::MIN, 1, 1))];
        let outputs = [monitor("A", (i32::MAX, i32::MAX, 1, 1))];
        assert_eq!(
            x11_to_layout((i32::MIN, i32::MIN, 1, 1), &x11, &outputs),
            (i32::MAX, i32::MAX, 1, 1)
        );
    }

    #[test]
    fn extreme_distances_saturate_without_overflow() {
        assert_eq!(
            distance(
                (i32::MAX, i32::MAX, i32::MAX, i32::MAX),
                (i32::MAX, i32::MAX)
            ),
            0
        );
        assert_eq!(
            distance((i32::MIN, i32::MIN, 0, 0), (i32::MIN, i32::MIN)),
            0
        );
        assert_eq!(
            distance((i32::MIN, i32::MIN, 1, 1), (i32::MAX, i32::MAX)),
            i64::MAX
        );
        assert_eq!(
            distance((i32::MAX, i32::MAX, 1, 1), (i32::MIN, i32::MIN)),
            i64::MAX
        );
        assert_eq!(distance((i32::MIN, 0, i32::MAX, 1), (0, 0)), 4);
    }

    #[test]
    fn extreme_coordinate_maps_saturate_after_translation() {
        assert_eq!(map(i32::MAX, i32::MIN, 0, 1.0), i32::MAX);
        assert_eq!(map(i32::MIN, i32::MAX, 0, 1.0), i32::MIN);
        assert_eq!(map(i32::MAX, i32::MIN, i32::MIN, 1.0), i32::MAX);
        assert_eq!(map(i32::MIN, i32::MAX, i32::MAX, 1.0), i32::MIN);
        assert_eq!(map(i32::MAX, 0, i32::MAX, 2.0), i32::MAX);
        assert_eq!(map(i32::MIN, 0, i32::MIN, 2.0), i32::MIN);
    }

    #[test]
    fn extreme_placement_saturates_without_overflow() {
        assert_eq!(
            place(
                (i32::MAX, i32::MAX, 1, i32::MAX),
                (1, 1),
                (0, 0, i32::MAX, i32::MAX)
            ),
            (i32::MAX - 1, i32::MAX - 1)
        );
        assert_eq!(
            place(
                (i32::MIN, i32::MIN, 0, 0),
                (i32::MAX, i32::MAX),
                (i32::MIN, i32::MIN, 1, 1)
            ),
            (i32::MIN, i32::MIN)
        );
    }

    #[test]
    fn physical_x11_pixels_scale_to_the_layout() {
        // Hyprland with force_zero_scaling: a 3840×2160 monitor at scale 1.5 is 2560×1440.
        let x11 = [monitor("eDP-1", (0, 0, 3840, 2160))];
        let outputs = [monitor("eDP-1", (0, 0, 2560, 1440))];
        assert_eq!(
            x11_to_layout((1500, 900, 3, 30), &x11, &outputs),
            (1000, 600, 2, 20)
        );
    }

    #[test]
    fn monitors_are_matched_by_name() {
        let x11 = [
            monitor("DP-1", (0, 0, 1920, 1080)),
            monitor("DP-2", (1920, 0, 1920, 1080)),
        ];
        // The compositor arranges them the other way round.
        let outputs = [
            monitor("DP-2", (0, 0, 1920, 1080)),
            monitor("DP-1", (1920, 0, 1920, 1080)),
        ];
        assert_eq!(x11_to_layout((100, 50, 1, 20), &x11, &outputs).0, 2020);
        assert_eq!(x11_to_layout((2000, 50, 1, 20), &x11, &outputs).0, 80);
    }

    #[test]
    fn cursors_partly_off_their_monitor_map_by_the_nearest_one() {
        // A spot at the top of the second monitor makes a rectangle that starts above it.
        let x11 = [
            monitor("DP-1", (0, 0, 1280, 720)),
            monitor("DP-2", (1280, 0, 1600, 900)),
        ];
        let outputs = [
            monitor("DP-1", (0, 0, 1280, 720)),
            monitor("DP-2", (3000, 0, 800, 450)),
        ];
        assert_eq!(
            x11_to_layout((1300, -16, 1, 20), &x11, &outputs),
            (3010, -8, 1, 10)
        );
    }

    #[test]
    fn unknown_monitors_keep_coordinates() {
        let rect = (10, 20, 1, 18);
        assert_eq!(x11_to_layout(rect, &[], &[]), rect);
        let x11 = [monitor("XWAYLAND0", (0, 0, 800, 600))];
        let outputs = [monitor("HEADLESS-1", (0, 0, 800, 600))];
        assert_eq!(x11_to_layout(rect, &x11, &outputs), rect);
    }

    #[test]
    fn windows_go_below_above_or_left_of_the_cursor() {
        let output = (0, 0, 1280, 720);
        assert_eq!(place((100, 100, 2, 20), (300, 100), output), (100, 120));
        // No room below: above.
        assert_eq!(place((100, 650, 2, 20), (300, 100), output), (100, 550));
        // No room on the right: moved left.
        assert_eq!(place((1200, 100, 2, 20), (300, 100), output), (980, 120));
        // On a second output.
        assert_eq!(
            place((1300, 10, 2, 20), (300, 100), (1280, 0, 1280, 720)),
            (1300, 30)
        );
    }

    #[test]
    fn the_output_under_a_point_is_found() {
        let outputs = [
            monitor("A", (0, 0, 100, 100)),
            monitor("B", (100, 0, 100, 100)),
        ];
        assert_eq!(
            output_at(&outputs, (150, 10)).map(|o| o.name.as_str()),
            Some("B")
        );
        assert_eq!(
            output_at(&outputs, (500, 10)).map(|o| o.name.as_str()),
            Some("B")
        );
        assert_eq!(
            output_at(&outputs, (-20, -5)).map(|o| o.name.as_str()),
            Some("A")
        );
        assert!(output_at(&[], (0, 0)).is_none());
    }
}
