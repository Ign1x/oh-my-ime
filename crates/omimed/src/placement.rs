//! Where engine windows go for X11 applications.
//!
//! X11 applications report their text cursor in X11 screen coordinates. Compositors map
//! those to their layout differently (Hyprland with `xwayland:force_zero_scaling` uses
//! physical pixels, sway logical ones), but XWayland describes its monitors through
//! RandR under the names of the compositor's outputs. Matching the names maps any point.

/// `(x, y, width, height)`
pub type Rect = (i32, i32, i32, i32);

/// A monitor: of the X screen (in X11 pixels) or of the compositor (in its logical
/// coordinates).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Monitor {
    pub name: String,
    pub rect: Rect,
}

fn contains((x, y, w, h): Rect, (px, py): (i32, i32)) -> bool {
    px >= x && py >= y && px < x + w && py < y + h
}

/// Maps the X11 rectangle `rect` to compositor coordinates, by the X11 monitor its
/// top-left corner is on. Without monitors (or names that match), X11 and compositor
/// coordinates are taken to be the same.
pub fn x11_to_layout(rect: Rect, x11: &[Monitor], outputs: &[Monitor]) -> Rect {
    let (x, y, w, h) = rect;
    let pair = x11
        .iter()
        .filter(|m| contains(m.rect, (x, y)))
        .find_map(|m| outputs.iter().find(|o| o.name == m.name).map(|o| (m, o)));
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
    let map =
        |v: i32, from: i32, to: i32, scale: f64| to + (f64::from(v - from) * scale).round() as i32;
    (
        map(x, fx, tx, scale_x),
        map(y, fy, ty, scale_y),
        (f64::from(w) * scale_x).round() as i32,
        (f64::from(h) * scale_y).round() as i32,
    )
}

/// The output `point` is on, or the first one.
pub fn output_at(outputs: &[Monitor], point: (i32, i32)) -> Option<&Monitor> {
    outputs
        .iter()
        .find(|o| contains(o.rect, point))
        .or_else(|| outputs.first())
}

/// Where a `size` window goes for the text cursor `cursor` on `output`: below the
/// cursor, above it when there is no room below, and moved left when it would leave
/// the output on the right.
pub fn place(cursor: Rect, size: (i32, i32), output: Rect) -> (i32, i32) {
    let (cx, cy, _, ch) = cursor;
    let (w, h) = size;
    let (ox, oy, ow, oh) = output;
    let mut x = cx;
    let mut y = cy + ch;
    if y + h > oy + oh {
        y = cy - h;
    }
    if x + w > ox + ow {
        x = ox + ow - w;
    }
    (x.max(ox), y.max(oy))
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
            Some("A")
        );
        assert!(output_at(&[], (0, 0)).is_none());
    }
}
