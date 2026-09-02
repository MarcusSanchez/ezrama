//! Pixel work for the notification-area icon: a cardboard box with another
//! program's icon stuck on its front. Everything here is plain arithmetic on
//! premultiplied `0xAARRGGBB` pixels, so it runs anywhere.

/// One premultiplied pixel: alpha in the top byte, then red, green, blue.
pub type Pixel = u32;

/// A square image in row-major order.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Image {
    pub size: usize,
    pub pixels: Vec<Pixel>,
}

/// Smallest icon that still has room for a lid, a seam, and a label.
pub const MIN_SIZE: usize = 8;

const OUTLINE: Pixel = opaque(88, 56, 30);
const BODY: Pixel = opaque(196, 138, 76);
const LID: Pixel = opaque(222, 170, 106);
const TAPE: Pixel = opaque(240, 226, 190);

/// A fully opaque pixel.
pub const fn opaque(r: u8, g: u8, b: u8) -> Pixel {
    0xff00_0000 | (r as u32) << 16 | (g as u32) << 8 | b as u32
}

/// The alpha byte of a pixel.
pub fn alpha(pixel: Pixel) -> u8 {
    (pixel >> 24) as u8
}

fn channels(pixel: Pixel) -> [u32; 4] {
    [pixel >> 24, (pixel >> 16) & 0xff, (pixel >> 8) & 0xff, pixel & 0xff]
}

fn from_channels([a, r, g, b]: [u32; 4]) -> Pixel {
    a.min(255) << 24 | r.min(255) << 16 | g.min(255) << 8 | b.min(255)
}

/// Converts a straight-alpha pixel to premultiplied form.
pub fn premultiply(straight: Pixel) -> Pixel {
    let [a, r, g, b] = channels(straight);
    let scale = |c: u32| (c * a + 127) / 255;
    from_channels([a, scale(r), scale(g), scale(b)])
}

/// Composites `src` over `dst`, both premultiplied.
pub fn over(dst: Pixel, src: Pixel) -> Pixel {
    let keep = 255 - (src >> 24);
    let d = channels(dst);
    let s = channels(src);
    let blend = |i: usize| s[i] + (d[i] * keep + 127) / 255;
    from_channels([blend(0), blend(1), blend(2), blend(3)])
}

impl Image {
    /// A transparent image.
    pub fn blank(size: usize) -> Self {
        Self {
            size,
            pixels: vec![0; size * size],
        }
    }

    /// Wraps existing pixels; their count must be `size` squared.
    pub fn from_pixels(size: usize, pixels: Vec<Pixel>) -> Option<Self> {
        (pixels.len() == size * size).then_some(Self { size, pixels })
    }

    pub fn get(&self, x: usize, y: usize) -> Pixel {
        self.pixels[y * self.size + x]
    }

    fn fill(&mut self, x0: usize, y0: usize, x1: usize, y1: usize, pixel: Pixel) {
        for y in y0..y1.min(self.size) {
            for x in x0..x1.min(self.size) {
                self.pixels[y * self.size + x] = pixel;
            }
        }
    }

    /// Whether any pixel has a non-zero alpha.
    pub fn has_alpha(&self) -> bool {
        self.pixels.iter().any(|&p| alpha(p) != 0)
    }

    /// Draws `other` onto this image with its top-left corner at `(x, y)`.
    pub fn blit(&mut self, other: &Image, x: usize, y: usize) {
        for oy in 0..other.size {
            for ox in 0..other.size {
                let (tx, ty) = (x + ox, y + oy);
                if tx < self.size && ty < self.size {
                    let index = ty * self.size + tx;
                    self.pixels[index] = over(self.pixels[index], other.get(ox, oy));
                }
            }
        }
    }

    /// Resizes by area averaging; premultiplied pixels average correctly.
    pub fn resample(&self, size: usize) -> Image {
        if size == self.size {
            return self.clone();
        }
        let scale = self.size as f32 / size as f32;
        let overlap = |start: usize, low: f32, high: f32| -> f32 {
            let (a, b) = (start as f32, start as f32 + 1.0);
            (b.min(high) - a.max(low)).max(0.0)
        };
        let mut out = Image::blank(size);
        for y in 0..size {
            let (y0, y1) = (y as f32 * scale, (y + 1) as f32 * scale);
            for x in 0..size {
                let (x0, x1) = (x as f32 * scale, (x + 1) as f32 * scale);
                let mut sum = [0f32; 4];
                let mut weight = 0f32;
                for sy in (y0.floor() as usize)..(y1.ceil() as usize).min(self.size) {
                    let wy = overlap(sy, y0, y1);
                    for sx in (x0.floor() as usize)..(x1.ceil() as usize).min(self.size) {
                        let w = wy * overlap(sx, x0, x1);
                        let c = channels(self.get(sx, sy));
                        for i in 0..4 {
                            sum[i] += c[i] as f32 * w;
                        }
                        weight += w;
                    }
                }
                let value = |i: usize| (sum[i] / weight.max(f32::EPSILON) + 0.5) as u32;
                out.pixels[y * size + x] = from_channels([value(0), value(1), value(2), value(3)]);
            }
        }
        out
    }

    /// Gives an image without an alpha channel one from a 1-bit mask
    /// rendered as pixels: a lit mask pixel is transparent.
    pub fn with_mask(&self, mask: &Image) -> Image {
        let pixels = self
            .pixels
            .iter()
            .zip(&mask.pixels)
            .map(|(&color, &bit)| {
                if bit & 0x00ff_ffff != 0 {
                    0
                } else {
                    color | 0xff00_0000
                }
            })
            .collect();
        Image {
            size: self.size,
            pixels,
        }
    }
}

/// Where the parts of a box of a given size sit.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Layout {
    /// First row of the box; the rows above stay transparent.
    pub top: usize,
    /// Row of the seam between lid and body.
    pub seam: usize,
    /// Columns covered by the tape strip.
    pub tape: (usize, usize),
    /// Side of the label square and its top-left corner.
    pub label: usize,
    pub label_at: (usize, usize),
}

/// Positions the parts for an icon `size` pixels square.
pub fn layout(size: usize) -> Layout {
    let size = size.max(MIN_SIZE);
    let round = |fraction: f32| (size as f32 * fraction + 0.5) as usize;
    let top = round(0.06);
    let lid = round(0.14).max(2);
    let seam = top + lid;
    let tape_width = round(0.25).max(2);
    let tape_start = (size - tape_width) / 2;
    let body_top = seam + 1;
    let body_height = size - 1 - body_top;
    let label = round(0.62).min(body_height.saturating_sub(1)).max(1);
    let label_at = ((size - label) / 2, body_top + (body_height - label) / 2);
    Layout {
        top,
        seam,
        tape: (tape_start, tape_start + tape_width),
        label,
        label_at,
    }
}

/// A closed cardboard box seen from the front: a lighter lid, a seam, a
/// strip of tape across the seam, and an outline.
pub fn package_box(size: usize) -> Image {
    let size = size.max(MIN_SIZE);
    let parts = layout(size);
    let mut image = Image::blank(size);
    image.fill(0, parts.top, size, size, OUTLINE);
    image.fill(1, parts.top + 1, size - 1, parts.seam, LID);
    image.fill(1, parts.seam + 1, size - 1, size - 1, BODY);
    image.fill(parts.tape.0, parts.top + 1, parts.tape.1, parts.seam + 1, TAPE);
    image
}

/// The box with `label` scaled down and stuck on its front. Without a
/// label the plain box is returned.
pub fn boxed(size: usize, label: Option<&Image>) -> Image {
    let mut image = package_box(size);
    if let Some(label) = label {
        let parts = layout(image.size);
        let scaled = label.resample(parts.label);
        image.blit(&scaled, parts.label_at.0, parts.label_at.1);
    }
    image
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn premultiplication_scales_colour_by_alpha() {
        assert_eq!(premultiply(0x80ff_ff00), 0x8080_8000);
        assert_eq!(premultiply(0xffff_8000), 0xffff_8000);
        assert_eq!(premultiply(0x00ff_ffff), 0);
    }

    #[test]
    fn over_is_identity_at_the_extremes() {
        let below = opaque(10, 20, 30);
        assert_eq!(over(below, 0), below);
        assert_eq!(over(below, opaque(1, 2, 3)), opaque(1, 2, 3));
        assert_eq!(over(below, 0x8080_0000), opaque(133, 10, 15));
    }

    #[test]
    fn layout_keeps_the_label_inside_the_body_at_every_size() {
        for size in MIN_SIZE..=64 {
            let parts = layout(size);
            assert!(parts.top < parts.seam, "size {size}");
            assert!(parts.seam + 1 < size - 1, "size {size}");
            assert!(parts.tape.0 < parts.tape.1 && parts.tape.1 <= size, "size {size}");
            assert!(parts.label_at.1 > parts.seam, "size {size}");
            assert!(parts.label_at.1 + parts.label < size - 1, "size {size}");
            assert!(parts.label_at.0 + parts.label < size, "size {size}");
        }
    }

    #[test]
    fn sixteen_pixel_layout() {
        assert_eq!(
            layout(16),
            Layout {
                top: 1,
                seam: 3,
                tape: (6, 10),
                label: 10,
                label_at: (3, 4),
            }
        );
    }

    #[test]
    fn the_box_has_transparent_rows_above_and_an_outline() {
        let image = package_box(16);
        let parts = layout(16);
        assert!((0..parts.top).all(|y| (0..16).all(|x| image.get(x, y) == 0)));
        assert!((parts.top..16).all(|y| image.get(0, y) == OUTLINE && image.get(15, y) == OUTLINE));
        assert!((0..16).all(|x| image.get(x, 15) == OUTLINE));
        assert_eq!(image.get(2, parts.seam), OUTLINE);
        assert_eq!(image.get(8, parts.seam), TAPE);
        assert_eq!(image.get(2, parts.top + 1), LID);
        assert_eq!(image.get(2, parts.seam + 1), BODY);
        assert!(image.pixels.iter().all(|&p| p == 0 || alpha(p) == 255));
    }

    #[test]
    fn a_label_lands_on_the_body_and_nowhere_else() {
        let label = Image::from_pixels(4, vec![opaque(0, 0, 0); 16]).unwrap();
        let plain = package_box(16);
        let image = boxed(16, Some(&label));
        let parts = layout(16);
        for y in 0..16 {
            for x in 0..16 {
                let inside = (parts.label_at.0..parts.label_at.0 + parts.label).contains(&x)
                    && (parts.label_at.1..parts.label_at.1 + parts.label).contains(&y);
                if inside {
                    assert_eq!(image.get(x, y), opaque(0, 0, 0), "({x},{y})");
                } else {
                    assert_eq!(image.get(x, y), plain.get(x, y), "({x},{y})");
                }
            }
        }
        assert_eq!(boxed(16, None), plain);
    }

    #[test]
    fn resampling_averages_areas_and_keeps_alpha() {
        let mut source = Image::blank(4);
        source.fill(0, 0, 2, 4, opaque(200, 0, 0));
        source.fill(2, 0, 4, 4, opaque(0, 0, 100));
        let half = source.resample(2);
        assert_eq!(half.get(0, 0), opaque(200, 0, 0));
        assert_eq!(half.get(1, 1), opaque(0, 0, 100));
        let one = source.resample(1);
        assert_eq!(one.get(0, 0), opaque(100, 0, 50));
        let three = source.resample(3);
        assert_eq!(three.get(0, 0), opaque(200, 0, 0));
        assert_eq!(three.get(2, 2), opaque(0, 0, 100));
        assert_eq!(three.get(1, 1), opaque(100, 0, 50));
        assert_eq!(source.resample(4), source);
        let transparent = Image::blank(4).resample(2);
        assert!(transparent.pixels.iter().all(|&p| p == 0));
    }

    #[test]
    fn a_mask_supplies_the_missing_alpha() {
        let color = Image::from_pixels(2, vec![0x0010_2030, 0x0040_5060, 0, 0]).unwrap();
        let mask = Image::from_pixels(2, vec![0, 0x00ff_ffff, 0, 0x00ff_ffff]).unwrap();
        let masked = color.with_mask(&mask);
        assert_eq!(masked.pixels, vec![0xff10_2030, 0, 0xff00_0000, 0]);
        assert!(!color.has_alpha());
        assert!(masked.has_alpha());
    }
}
