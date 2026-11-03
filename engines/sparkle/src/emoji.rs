//! Color emoji as images. Slint's software renderer, which draws the panel, draws a
//! glyph only from its outline, as a single color: emoji fonts have colored layers
//! (COLR) or pictures (CBDT, as Noto Color Emoji) instead, so the panel would show them
//! black or not at all. Here the emoji font is looked up among the system's fonts, the
//! emoji shaped with it (flags and sequences such as 👩‍💻 are several characters, one
//! glyph) and drawn in color with swash, into an image the panel shows.

use swash::scale::{Render, ScaleContext, Source, StrikeWith, image::Content};
use swash::shape::ShapeContext;
use swash::zeno::Format;
use swash::{FontRef, scale::image::Image};

/// Emoji fonts by family, best first.
const FAMILIES: [&str; 5] = [
    "Noto Color Emoji",
    "Twemoji",
    "JoyPixels",
    "Apple Color Emoji",
    "Segoe UI Emoji",
];

/// An emoji font and what draws with it.
pub struct Painter {
    data: Vec<u8>,
    index: u32,
    shaping: ShapeContext,
    scaling: ScaleContext,
}

impl std::fmt::Debug for Painter {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Painter")
            .field("bytes", &self.data.len())
            .field("index", &self.index)
            .finish_non_exhaustive()
    }
}

impl Painter {
    /// The painter for the system's emoji font, if there is one.
    pub fn new() -> Option<Self> {
        let mut fonts = fontique::Collection::new(fontique::CollectionOptions {
            shared: false,
            system_fonts: true,
        });
        let font = FAMILIES
            .iter()
            .find_map(|name| fonts.family_by_name(name)?.fonts().first().cloned())?;
        let blob = font.load(None)?;
        let painter = Self {
            data: blob.data().to_vec(),
            index: font.index(),
            shaping: ShapeContext::new(),
            scaling: ScaleContext::new(),
        };
        // A font that swash cannot read is no font.
        painter.font()?;
        Some(painter)
    }

    fn font(&self) -> Option<FontRef<'_>> {
        FontRef::from_index(&self.data, self.index as usize)
    }

    /// `emoji` in color, `size` pixels high (about: a bitmap font draws the size it has
    /// nearest), as premultiplied RGBA.
    pub fn paint(&mut self, emoji: &str, size: u32) -> Option<slint::Image> {
        let font = FontRef::from_index(&self.data, self.index as usize)?;
        let mut glyph = None;
        let mut shaper = self.shaping.builder(font).size(size as f32).build();
        shaper.add_str(emoji);
        shaper.shape_with(|cluster| {
            if glyph.is_none() {
                glyph = cluster.glyphs.first().map(|g| g.id).filter(|&id| id != 0);
            }
        });
        let mut scaler = self.scaling.builder(font).size(size as f32).build();
        let image = Render::new(&[
            Source::ColorOutline(0),
            Source::ColorBitmap(StrikeWith::BestFit),
            Source::Outline,
        ])
        .format(Format::Alpha)
        .render(&mut scaler, glyph?)?;
        to_slint(&image)
    }
}

/// A swash image as a Slint one: colors as they are, masks in black.
fn to_slint(image: &Image) -> Option<slint::Image> {
    let (width, height) = (image.placement.width, image.placement.height);
    if width == 0 || height == 0 {
        return None;
    }
    let pixels: Vec<slint::Rgba8Pixel> = match image.content {
        // swash's color bitmaps and layers are straight RGBA.
        Content::Color => image
            .data
            .as_chunks::<4>()
            .0
            .iter()
            .map(|&[r, g, b, a]| {
                let premultiply = |c: u8| (u16::from(c) * u16::from(a) / 255) as u8;
                slint::Rgba8Pixel::new(premultiply(r), premultiply(g), premultiply(b), a)
            })
            .collect(),
        Content::Mask => image
            .data
            .iter()
            .map(|&a| slint::Rgba8Pixel::new(0, 0, 0, a))
            .collect(),
        Content::SubpixelMask => return None,
    };
    if pixels.len() != (width * height) as usize {
        return None;
    }
    let mut buffer = slint::SharedPixelBuffer::<slint::Rgba8Pixel>::new(width, height);
    buffer.make_mut_slice().copy_from_slice(&pixels);
    Some(slint::Image::from_rgba8_premultiplied(buffer))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Draws what the system has; passes on machines without an emoji font.
    #[test]
    fn emoji_come_out_in_color() {
        let Some(mut painter) = Painter::new() else {
            eprintln!("no emoji font; nothing to test");
            return;
        };
        for emoji in ["🚀", "❤️", "👩‍💻", "🇨🇳"] {
            let image = painter
                .paint(emoji, 52)
                .unwrap_or_else(|| panic!("{emoji}"));
            let pixels = image
                .to_rgba8_premultiplied()
                .unwrap_or_else(|| panic!("{emoji} has pixels"));
            // Color: some pixel is neither gray nor transparent.
            let colored = pixels
                .as_slice()
                .iter()
                .any(|p| p.a > 0 && (p.r != p.g || p.g != p.b));
            assert!(colored, "{emoji} is in color");
        }
    }
}
