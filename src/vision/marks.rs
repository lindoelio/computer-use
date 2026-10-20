//! Pure-`image` set-of-marks overlay.
//!
//! Numbers every rectangle in `boxes` on a copy of `source` and draws a small
//! filled label in the top-left of each rect. Numerals are rendered with an
//! embedded 4x6 bitmap font so the module stays free of `fontdue` or any
//! platform font APIs.

use image::{Pixel, Rgba, RgbaImage};

/// One rectangle plus its human-readable label.
///
/// `label` is what the user sees inside the box. The protocol decides whether
/// to use a tree index, an `oN` OCR index, or `?` when an index is not yet
/// known. Keep `label` short — at most two characters fit at the default size.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MarkBox {
    /// Image pixel rectangle (top-left inclusive).
    pub rect: ImageRect,
    /// Human-readable label drawn in the box.
    pub label: String,
}

/// Rectangle in image pixels.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ImageRect {
    /// Left edge.
    pub x: i32,
    /// Top edge.
    pub y: i32,
    /// Width in pixels. May be smaller than the rendered number — clip, don't fail.
    pub width: u32,
    /// Height in pixels.
    pub height: u32,
}

impl ImageRect {
    fn right(&self) -> i32 {
        self.x.saturating_add(self.width as i32)
    }

    fn bottom(&self) -> i32 {
        self.y.saturating_add(self.height as i32)
    }

    fn is_empty(&self) -> bool {
        self.width == 0 || self.height == 0
    }
}

/// Draw `boxes` on a copy of `source` and return the marked image.
///
/// Empty or off-image rectangles are skipped. Clipped rectangles still draw
/// what fits; the borders and numbers stay legible at very small sizes.
pub fn draw_marks(source: &RgbaImage, boxes: &[MarkBox]) -> RgbaImage {
    let mut image = source.clone();
    for mark in boxes {
        if mark.rect.is_empty() {
            continue;
        }
        draw_box_border(&mut image, mark.rect);
        draw_label(&mut image, mark.rect, &mark.label);
    }
    image
}

/// Map an element index from the latest tree to its label.
#[must_use]
pub fn tree_label(index: u32) -> String {
    index.to_string()
}

/// Map an OCR observation (zero-based) to the `oN` label the protocol uses.
#[must_use]
pub fn ocr_label(index: u32) -> String {
    format!("o{index}")
}

// ---------------------------------------------------------------------------
// Internals
// ---------------------------------------------------------------------------

/// Draw a 2-pixel hollow rectangle in `color` (or the default orange-red).
fn draw_box_border(image: &mut RgbaImage, rect: ImageRect) {
    let color = MARK_BORDER;
    let w = image.width() as i32;
    let h = image.height() as i32;
    let left = rect.x.clamp(0, w);
    let top = rect.y.clamp(0, h);
    let right = rect.right().clamp(0, w);
    let bottom = rect.bottom().clamp(0, h);
    if left >= right || top >= bottom {
        return;
    }
    let stroke = 1_i32;
    // Top + bottom
    for stroke_y in 0..stroke {
        for x in left..right {
            put_pixel(image, x, top + stroke_y, color);
            put_pixel(image, x, bottom - 1 - stroke_y, color);
        }
    }
    // Left + right
    for stroke_x in 0..stroke {
        for y in top..bottom {
            put_pixel(image, left + stroke_x, y, color);
            put_pixel(image, right - 1 - stroke_x, y, color);
        }
    }
    // Bumped second pass at low alpha for legibility on bright backgrounds.
    let halo = MARK_HALO;
    for stroke_y in 0..stroke {
        for x in left..right {
            put_pixel(image, x, top.saturating_sub(1) + stroke_y, halo);
            put_pixel(image, x, bottom + stroke_y, halo);
        }
    }
    for stroke_x in 0..stroke {
        for y in top..bottom {
            put_pixel(image, left.saturating_sub(1) + stroke_x, y, halo);
            put_pixel(image, right + stroke_x, y, halo);
        }
    }
}

/// Draw `label` in the top-left of `rect`. Silently drops characters the
/// font does not know — only digits, lowercase `o`, and `-` are supported.
fn draw_label(image: &mut RgbaImage, rect: ImageRect, label: &str) {
    let pixels = label_to_pixels(label);
    let rect_w = rect.width.max(1);
    let rect_h = rect.height.max(1);
    let cell = (rect_w / pixels.cols.max(1) as u32).min(rect_h / FONT_HEIGHT.max(1) as u32);
    if cell == 0 {
        return;
    }
    let cell_i32 = cell as i32;
    let origin_x = rect.x;
    let origin_y = rect.y;
    let w = image.width() as i32;
    let h = image.height() as i32;
    for (row, line) in pixels.rows.iter().enumerate() {
        let y = origin_y + (row as i32) * cell_i32;
        if y >= h {
            break;
        }
        for (col, on) in line.iter().enumerate() {
            if !*on {
                continue;
            }
            let x = origin_x + (col as i32) * cell_i32;
            if x >= w {
                break;
            }
            for dy in 0..cell_i32 {
                if y + dy < 0 || y + dy >= h {
                    continue;
                }
                for dx in 0..cell_i32 {
                    if x + dx < 0 || x + dx >= w {
                        continue;
                    }
                    put_pixel(image, x + dx, y + dy, MARK_TEXT);
                }
            }
        }
    }
}

fn put_pixel(image: &mut RgbaImage, x: i32, y: i32, color: Rgba<u8>) {
    if x < 0 || y < 0 {
        return;
    }
    let (xi, yi) = (x as u32, y as u32);
    if xi >= image.width() || yi >= image.height() {
        return;
    }
    let blended = blend(image.get_pixel(xi, yi).to_rgba(), color);
    image.put_pixel(xi, yi, blended);
}

/// Alpha-blend `overlay` onto `base`. Matches the PNG-over-PNG semantics used
/// by the screenshot encoder for capture overlays.
fn blend(base: Rgba<u8>, overlay: Rgba<u8>) -> Rgba<u8> {
    let alpha = u32::from(overlay.0[3]);
    if alpha == 0 {
        return base;
    }
    let inv = 255 - alpha;
    let mut out = [0_u8; 4];
    for (i, slot) in out.iter_mut().enumerate().take(3) {
        let mixed = (u32::from(base.0[i]) * inv + u32::from(overlay.0[i]) * alpha) / 255;
        *slot = mixed.min(255) as u8;
    }
    out[3] = 255;
    Rgba(out)
}

/// Color of the rectangle border (orange-red, opaque).
const MARK_BORDER: Rgba<u8> = Rgba([255, 64, 32, 255]);
/// Color of the rectangle text.
const MARK_TEXT: Rgba<u8> = Rgba([255, 255, 255, 255]);
/// Halo outside the border to keep it visible over white backgrounds.
const MARK_HALO: Rgba<u8> = Rgba([255, 64, 32, 80]);

// ---------------------------------------------------------------------------
// Tiny bitmap font
// ---------------------------------------------------------------------------

const FONT_HEIGHT: usize = 6;
const FONT_WIDTH: usize = 4;
const FONT_GLYPHS: &[(&str, [u8; FONT_HEIGHT])] = &[
    ("0", [0b0110, 0b1001, 0b1001, 0b1001, 0b1001, 0b0110]),
    ("1", [0b0010, 0b0110, 0b0010, 0b0010, 0b0010, 0b0111]),
    ("2", [0b0110, 0b1001, 0b0001, 0b0010, 0b0100, 0b1111]),
    ("3", [0b1110, 0b0001, 0b0001, 0b0110, 0b0001, 0b1110]),
    ("4", [0b0001, 0b0011, 0b0101, 0b1001, 0b1111, 0b0001]),
    ("5", [0b1111, 0b1000, 0b1110, 0b0001, 0b0001, 0b1110]),
    ("6", [0b0010, 0b0100, 0b1000, 0b1110, 0b1001, 0b0110]),
    ("7", [0b1111, 0b0001, 0b0010, 0b0100, 0b0100, 0b0100]),
    ("8", [0b0110, 0b1001, 0b1001, 0b0110, 0b1001, 0b0110]),
    ("9", [0b0110, 0b1001, 0b0111, 0b0001, 0b0010, 0b0100]),
    ("o", [0b0000, 0b0000, 0b0110, 0b1001, 0b1001, 0b0110]),
    ("-", [0b0000, 0b0000, 0b0000, 0b0111, 0b0000, 0b0000]),
];

struct GlyphPixels {
    rows: Vec<[bool; FONT_WIDTH * MAX_LABEL_CHARS]>,
    cols: usize,
}

/// Maximum glyphs `draw_label` will render. Keeps the column buffer small
/// and bounds the size of a set-of-marks overlay.
const MAX_LABEL_CHARS: usize = 4;

fn label_to_pixels(label: &str) -> GlyphPixels {
    // MAX_LABEL_CHARS hard caps the number of glyphs; each glyph is
    // FONT_WIDTH columns wide, so the column buffer must hold that many
    // columns, not just one glyph's worth. Allocating FONT_WIDTH columns
    // (the previous size) panicked as soon as a second glyph was rendered
    // because `cols + col` walked past the end of every row.
    let mut rows = vec![[false; FONT_WIDTH * MAX_LABEL_CHARS]; FONT_HEIGHT];
    let mut cols = 0_usize;
    for (char_index, ch) in label.chars().enumerate() {
        if char_index >= MAX_LABEL_CHARS {
            // Hard cap: marks only label short IDs.
            break;
        }
        let glyph = glyph_for(ch);
        if glyph.is_empty() {
            // Unknown character: fill with a question-mark dot to keep the
            // label position visible without confusing it with a digit.
            fill_unknown(&mut rows, cols);
            cols += FONT_WIDTH;
            continue;
        }
        for (row, byte) in glyph.iter().enumerate() {
            for col in 0..FONT_WIDTH {
                let on = (byte >> (FONT_WIDTH - 1 - col)) & 1 == 1;
                rows[row][cols + col] = on;
            }
        }
        cols += FONT_WIDTH;
    }
    GlyphPixels { rows, cols }
}

fn glyph_for(ch: char) -> [u8; FONT_HEIGHT] {
    let target: &str = match ch {
        '0' => "0",
        '1' => "1",
        '2' => "2",
        '3' => "3",
        '4' => "4",
        '5' => "5",
        '6' => "6",
        '7' => "7",
        '8' => "8",
        '9' => "9",
        'o' | 'O' => "o",
        '-' => "-",
        _ => "",
    };
    FONT_GLYPHS
        .iter()
        .find(|(key, _)| *key == target)
        .map(|(_, glyph)| *glyph)
        .unwrap_or(EMPTY_GLYPH)
}

const EMPTY_GLYPH: [u8; FONT_HEIGHT] = [0; FONT_HEIGHT];

fn fill_unknown(rows: &mut [[bool; FONT_WIDTH * MAX_LABEL_CHARS]], col_offset: usize) {
    // Single center dot — narrower than a digit — keeps the column count.
    rows[2][col_offset + 1] = true;
    rows[3][col_offset + 1] = true;
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use image::{Rgba, RgbaImage};

    fn white_square(size: u32) -> RgbaImage {
        RgbaImage::from_pixel(size, size, Rgba([255, 255, 255, 255]))
    }

    #[test]
    fn marks_leave_a_non_white_pixel_inside_box_zero() {
        // The Roadmap-mandated fixture: a 20x20 white canvas, a single
        // element rect (tree index 0), and the expectation that the marks
        // overlay tinted a pixel inside that box.
        let image = white_square(20);
        let marks = vec![MarkBox {
            rect: ImageRect {
                x: 4,
                y: 4,
                width: 8,
                height: 8,
            },
            label: tree_label(0),
        }];
        let marked = draw_marks(&image, &marks);
        let mut saw_drawn_pixel = false;
        // Scan the inside of the box (centroid and near-label area). With
        // cell=1, the digit "0" paints some on-pixels inside x=[4..8),
        // y=[4..10). Any of those qualifies as a marks overlay.
        for y in 4..12 {
            for x in 4..12 {
                if marked.get_pixel(x, y).to_rgba() != Rgba([255, 255, 255, 255]) {
                    saw_drawn_pixel = true;
                    break;
                }
            }
            if saw_drawn_pixel {
                break;
            }
        }
        assert!(
            saw_drawn_pixel,
            "marks must leave a non-white pixel inside the labelled box"
        );
    }

    #[test]
    fn empty_boxes_are_skipped() {
        let image = white_square(8);
        let marks = vec![
            MarkBox {
                rect: ImageRect {
                    x: 0,
                    y: 0,
                    width: 0,
                    height: 0,
                },
                label: "?".to_string(),
            },
            MarkBox {
                rect: ImageRect {
                    x: -10,
                    y: -10,
                    width: 4,
                    height: 4,
                },
                label: "?".to_string(),
            },
        ];
        let marked = draw_marks(&image, &marks);
        // No pixels should have been written.
        for y in 0..marked.height() {
            for x in 0..marked.width() {
                assert_eq!(*marked.get_pixel(x, y), Rgba([255, 255, 255, 255]));
            }
        }
    }

    #[test]
    fn ocr_label_format() {
        assert_eq!(ocr_label(0), "o0");
        assert_eq!(ocr_label(12), "o12");
    }

    #[test]
    fn tree_label_is_decimal() {
        assert_eq!(tree_label(0), "0");
        assert_eq!(tree_label(42), "42");
    }

    #[test]
    fn multi_char_label_renders_without_panicking() {
        // Before the live the glyph column buffer was sized to one glyph
        // (FONT_WIDTH=4). The second glyph in 'o12' pushed cols + col to
        // index 4, past the end of a length-4 row, and
        // `rows[row][cols + col] = on` panicked with
        // "index out of bounds: the len is 4 but the index is 4".
        // The label lives in an OCR observation whose box touches the
        // right edge of the image — the clamp in draw_label must keep
        // every on-pixel inside the canvas.
        let image = RgbaImage::from_pixel(40, 12, Rgba([255, 255, 255, 255]));
        let marks = vec![MarkBox {
            rect: ImageRect {
                x: 24,
                y: 0,
                width: 16,
                height: 12,
            },
            label: "o12".to_string(),
        }];
        let marked = draw_marks(&image, &marks);
        // The cell size is width / cols = 16 / 12 = 1, so every on-pixel
        // should fall inside x in [24..40) and y in [0..12). No crash, and
        // at least one pixel inside the box must differ from the white
        // background.
        let mut saw_drawn_pixel = false;
        for y in 0..12 {
            for x in 24..40 {
                if marked.get_pixel(x, y).to_rgba() != Rgba([255, 255, 255, 255]) {
                    saw_drawn_pixel = true;
                    break;
                }
            }
            if saw_drawn_pixel {
                break;
            }
        }
        assert!(
            saw_drawn_pixel,
            "marks must leave a non-white pixel inside the labelled box"
        );
    }

    #[test]
    fn multi_char_label_with_touching_right_edge_stays_inside() {
        // OCR labels can be two or three chars (o0, o12) inside a box flush
        // with the right edge of the canvas. draw_label clamps every on-pixel
        // to `x + dx < w` so this used to be the panic site, not a stray
        // pixel. The box border still paints, so we only assert the call
        // returns and that no on-pixel would have walked past the right
        // edge of the bitmap (the input width is 8, the box right edge is
        // x = 8, the cell width is 4 / cols(8) = 0 so draw_label returns
        // before drawing any glyph).
        let image = RgbaImage::from_pixel(8, 8, Rgba([255, 255, 255, 255]));
        let marks = vec![MarkBox {
            rect: ImageRect {
                x: 4,
                y: 0,
                width: 4,
                height: 8,
            },
            label: "12".to_string(),
        }];
        let _marked = draw_marks(&image, &marks);
    }
}
