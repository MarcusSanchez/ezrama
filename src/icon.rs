//! The program's icon as pixels: the embedded artwork and the resampling
//! that fits it to whatever size the notification area draws. Everything
//! here is plain arithmetic on premultiplied `0xAARRGGBB` pixels, so it
//! runs anywhere.

/// One premultiplied pixel: alpha in the top byte, then red, green, blue.
pub type Pixel = u32;

/// Side of the embedded artwork.
pub const EMBEDDED_SIZE: usize = 64;

/// The artwork, premultiplied, one little-endian pixel after another in
/// row-major order.
const EMBEDDED: &[u8] = include_bytes!("../assets/icon.pargb");

/// A square image in row-major order.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Image {
    pub size: usize,
    pub pixels: Vec<Pixel>,
}

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

/// Converts a premultiplied pixel back to straight alpha, as icon files
/// store it.
pub fn unpremultiply(premultiplied: Pixel) -> Pixel {
    let [a, r, g, b] = channels(premultiplied);
    if a == 0 {
        return 0;
    }
    let scale = |c: u32| (c * 255 + a / 2) / a;
    from_channels([a, scale(r), scale(g), scale(b)])
}

/// Bytes of an icon file holding `image` as its one entry: 32-bit pixels
/// with straight alpha, bottom-up, followed by a 1-bit mask that is set
/// wherever the pixel is fully transparent.
pub fn ico_bytes(image: &Image) -> Vec<u8> {
    let size = image.size;
    let mask_row = size.div_ceil(32) * 4;
    let pixel_bytes = size * size * 4;
    let mask_bytes = mask_row * size;
    let header_len = 6 + 16 + 40;
    let mut out = Vec::with_capacity(header_len + pixel_bytes + mask_bytes);
    // ICONDIR: reserved, type 1 (icon), one entry.
    out.extend_from_slice(&0u16.to_le_bytes());
    out.extend_from_slice(&1u16.to_le_bytes());
    out.extend_from_slice(&1u16.to_le_bytes());
    // ICONDIRENTRY: width and height (0 means 256), colours, reserved,
    // planes, bits per pixel, bytes in the entry, offset of the entry.
    let side = if size >= 256 { 0 } else { size as u8 };
    out.push(side);
    out.push(side);
    out.push(0);
    out.push(0);
    out.extend_from_slice(&1u16.to_le_bytes());
    out.extend_from_slice(&32u16.to_le_bytes());
    out.extend_from_slice(&((40 + pixel_bytes + mask_bytes) as u32).to_le_bytes());
    out.extend_from_slice(&22u32.to_le_bytes());
    // BITMAPINFOHEADER with the height doubled for the mask.
    out.extend_from_slice(&40u32.to_le_bytes());
    out.extend_from_slice(&(size as i32).to_le_bytes());
    out.extend_from_slice(&((size * 2) as i32).to_le_bytes());
    out.extend_from_slice(&1u16.to_le_bytes());
    out.extend_from_slice(&32u16.to_le_bytes());
    out.extend_from_slice(&0u32.to_le_bytes());
    out.extend_from_slice(&((pixel_bytes + mask_bytes) as u32).to_le_bytes());
    out.extend_from_slice(&[0u8; 16]);
    for y in (0..size).rev() {
        for x in 0..size {
            out.extend_from_slice(&unpremultiply(image.get(x, y)).to_le_bytes());
        }
    }
    for y in (0..size).rev() {
        let mut row = vec![0u8; mask_row];
        for x in 0..size {
            if alpha(image.get(x, y)) == 0 {
                row[x / 8] |= 0x80 >> (x % 8);
            }
        }
        out.extend_from_slice(&row);
    }
    out
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

    /// The embedded artwork.
    pub fn embedded() -> Self {
        let (words, _) = EMBEDDED.as_chunks::<4>();
        let pixels = words.iter().map(|word| u32::from_le_bytes(*word)).collect();
        Self::from_pixels(EMBEDDED_SIZE, pixels).expect("the embedded artwork is square")
    }

    pub fn get(&self, x: usize, y: usize) -> Pixel {
        self.pixels[y * self.size + x]
    }

    /// Whether any pixel has a non-zero alpha.
    pub fn has_alpha(&self) -> bool {
        self.pixels.iter().any(|&p| alpha(p) != 0)
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
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fill(image: &mut Image, x0: usize, y0: usize, x1: usize, y1: usize, pixel: Pixel) {
        for y in y0..y1 {
            for x in x0..x1 {
                image.pixels[y * image.size + x] = pixel;
            }
        }
    }

    #[test]
    fn the_embedded_artwork_is_premultiplied_with_transparent_corners() {
        let image = Image::embedded();
        assert_eq!(image.size, EMBEDDED_SIZE);
        assert!(image.has_alpha());
        assert_eq!(image.get(0, 0), 0);
        assert_eq!(image.get(EMBEDDED_SIZE - 1, EMBEDDED_SIZE - 1), 0);
        assert!(image.pixels.iter().all(|&p| {
            let [a, r, g, b] = channels(p);
            r <= a && g <= a && b <= a
        }));
        let opaque_pixels = image.pixels.iter().filter(|&&p| alpha(p) == 255).count();
        assert!(opaque_pixels > EMBEDDED_SIZE * EMBEDDED_SIZE / 4);
    }

    #[test]
    fn resampling_averages_areas_and_keeps_alpha() {
        let mut source = Image::blank(4);
        fill(&mut source, 0, 0, 2, 4, opaque(200, 0, 0));
        fill(&mut source, 2, 0, 4, 4, opaque(0, 0, 100));
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
    fn the_artwork_survives_scaling_to_tray_sizes() {
        let image = Image::embedded();
        for size in [16usize, 20, 24, 32, 48] {
            let scaled = image.resample(size);
            assert_eq!(scaled.size, size);
            assert_eq!(scaled.get(0, 0), 0);
            assert!(scaled.pixels.iter().filter(|&&p| alpha(p) == 255).count() > size * size / 4);
        }
    }

    #[test]
    fn unpremultiplication_undoes_premultiplication() {
        assert_eq!(unpremultiply(0x8080_8000), 0x80ff_ff00);
        assert_eq!(unpremultiply(opaque(10, 20, 30)), opaque(10, 20, 30));
        assert_eq!(unpremultiply(0), 0);
        for &straight in &[0x40c0_8020u32, 0x01ff_0000, 0xfe12_3456, 0x7f00_00ff] {
            let back = unpremultiply(premultiply(straight));
            let [a, r, g, b] = channels(back);
            let [ea, er, eg, eb] = channels(straight);
            assert_eq!(a, ea);
            let tolerance = 255 / ea.max(1) + 1;
            assert!(r.abs_diff(er) <= tolerance && g.abs_diff(eg) <= tolerance && b.abs_diff(eb) <= tolerance);
        }
    }

    #[test]
    fn the_icon_file_has_the_expected_layout() {
        let mut image = Image::blank(8);
        image.pixels[0] = opaque(1, 2, 3);
        image.pixels[63] = 0x8040_2010;
        let bytes = ico_bytes(&image);
        assert_eq!(&bytes[..6], &[0, 0, 1, 0, 1, 0]);
        assert_eq!(&bytes[6..10], &[8, 8, 0, 0]);
        assert_eq!(&bytes[10..14], &[1, 0, 32, 0]);
        let entry_bytes = 40 + 8 * 8 * 4 + 4 * 8;
        assert_eq!(&bytes[14..18], &(entry_bytes as u32).to_le_bytes());
        assert_eq!(&bytes[18..22], &22u32.to_le_bytes());
        assert_eq!(&bytes[22..26], &40u32.to_le_bytes());
        assert_eq!(&bytes[26..30], &8i32.to_le_bytes());
        assert_eq!(&bytes[30..34], &16i32.to_le_bytes());
        assert_eq!(bytes.len(), 22 + entry_bytes);
        let pixels = &bytes[62..62 + 256];
        let last_row_first = &pixels[..4];
        assert_eq!(last_row_first, &0u32.to_le_bytes(), "rows are stored bottom-up");
        let first_row_first = &pixels[7 * 32..7 * 32 + 4];
        assert_eq!(first_row_first, &opaque(1, 2, 3).to_le_bytes());
        let bottom_right = &pixels[28..32];
        assert_eq!(bottom_right, &unpremultiply(0x8040_2010).to_le_bytes());
        let mask = &bytes[62 + 256..];
        assert_eq!(mask.len(), 32);
        assert_eq!(mask[0], 0b1111_1110, "bottom row: only the last pixel is opaque");
        assert_eq!(mask[28], 0b0111_1111, "top row: only the first pixel is opaque");
        let full = ico_bytes(&Image::embedded());
        assert_eq!(full.len(), 22 + 40 + 64 * 64 * 4 + 8 * 64);
    }

    #[test]
    fn images_need_a_square_number_of_pixels() {
        assert!(Image::from_pixels(2, vec![0; 4]).is_some());
        assert!(Image::from_pixels(2, vec![0; 3]).is_none());
    }
}
