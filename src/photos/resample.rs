// Triangle sampling adapted from image 0.25.10, src/imageops/sample.rs.
// Licensed under the MIT license, reproduced below.
//
// Permission is hereby granted, free of charge, to any person obtaining a copy
// of this software and associated documentation files (the "Software"), to deal
// in the Software without restriction, including without limitation the rights
// to use, copy, modify, merge, publish, distribute, sublicense, and/or sell copies
// of the Software, and to permit persons to whom the Software is furnished to do
// so, subject to the following conditions:
//
// The above copyright notice and this permission notice shall be included in all
// copies or substantial portions of the Software.
//
// THE SOFTWARE IS PROVIDED "AS IS", WITHOUT WARRANTY OF ANY KIND, EXPRESS OR
// IMPLIED, INCLUDING BUT NOT LIMITED TO THE WARRANTIES OF MERCHANTABILITY,
// FITNESS FOR A PARTICULAR PURPOSE AND NONINFRINGEMENT. IN NO EVENT SHALL THE
// AUTHORS OR COPYRIGHT HOLDERS BE LIABLE FOR ANY CLAIM, DAMAGES OR OTHER
// LIABILITY, WHETHER IN AN ACTION OF CONTRACT, TORT OR OTHERWISE, ARISING FROM,
// OUT OF OR IN CONNECTION WITH THE SOFTWARE OR THE USE OR OTHER DEALINGS IN THE
// SOFTWARE.

use image::RgbImage;

struct Weights {
    start: u32,
    values: Vec<f32>,
}

fn weights(source: u32, scaled: u32, output: u32) -> Weights {
    let ratio = source as f32 / scaled as f32;
    let support = ratio.max(1.0);
    let center = (output as f32 + 0.5) * ratio;
    let start = ((center - support).floor() as i64).clamp(0, i64::from(source) - 1) as u32;
    let end =
        ((center + support).ceil() as i64).clamp(i64::from(start) + 1, i64::from(source)) as u32;
    let mut values: Vec<_> = (start..end)
        .map(|i| {
            let distance = ((i as f32 - (center - 0.5)) / support).abs();
            if distance < 1.0 {
                1.0 - distance
            } else {
                0.0
            }
        })
        .collect();
    let sum: f32 = values.iter().sum();
    for weight in &mut values {
        *weight /= sum;
    }
    Weights { start, values }
}

/// Center-crop a Triangle resize without allocating the discarded pixels.
/// `scaled` is the caller's cover size. All dimensions must be nonzero,
/// `scaled` must contain `target`, and `output` must hold exactly `w*h*3` bytes.
/// Sampling retains the full source filter support, including pixels outside
/// the crop, and image's rounding order.
pub(super) fn cover_into(
    image: &RgbImage,
    scaled: (u32, u32),
    target: (u32, u32),
    output: &mut [u8],
) {
    let (w, h) = target;
    let (sw, sh) = scaled;
    assert_eq!(output.len(), w as usize * h as usize * 3);
    let (x, y) = ((sw - w) / 2, (sh - h) / 2);
    if scaled == image.dimensions() {
        for (row, dest) in output.chunks_exact_mut(w as usize * 3).enumerate() {
            let start = ((y as usize + row) * sw as usize + x as usize) * 3;
            dest.copy_from_slice(&image.as_raw()[start..start + dest.len()]);
        }
        return;
    }
    let horizontal: Vec<_> = (x..x + w).map(|x| weights(image.width(), sw, x)).collect();
    let first = horizontal.first().unwrap().start;
    let last = horizontal.last().unwrap();
    let end = last.start + last.values.len() as u32;
    let mut row = vec![[0.0_f32; 3]; (end - first) as usize];
    for (out_y, dest) in output.chunks_exact_mut(w as usize * 3).enumerate() {
        row.fill([0.0; 3]);
        let vertical = weights(image.height(), sh, y + out_y as u32);
        for (offset, weight) in vertical.values.iter().enumerate() {
            let source = image.as_raw();
            let start =
                ((vertical.start as usize + offset) * image.width() as usize + first as usize) * 3;
            for (pixel, input) in row.iter_mut().zip(source[start..].chunks_exact(3)) {
                for channel in 0..3 {
                    pixel[channel] += f32::from(input[channel]) * weight;
                }
            }
        }
        for (out_x, horizontal) in horizontal.iter().enumerate() {
            let mut pixel = [0.0_f32; 3];
            let start = (horizontal.start - first) as usize;
            for (input, weight) in row[start..].iter().zip(&horizontal.values) {
                for channel in 0..3 {
                    pixel[channel] += input[channel] * weight;
                }
            }
            dest[out_x * 3..out_x * 3 + 3]
                .copy_from_slice(&pixel.map(|v| v.clamp(0.0, 255.0).round() as u8));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use image::{
        imageops::{crop_imm, resize, FilterType},
        Rgb,
    };

    fn fit(image: &RgbImage, target: (u32, u32)) -> RgbImage {
        let scale =
            (target.0 as f32 / image.width() as f32).max(target.1 as f32 / image.height() as f32);
        let scaled = (
            (image.width() as f32 * scale).ceil() as u32,
            (image.height() as f32 * scale).ceil() as u32,
        );
        let mut output = RgbImage::new(target.0, target.1);
        cover_into(image, scaled, target, output.as_mut());
        output
    }

    #[test]
    fn unscaled_center_crop_keeps_the_left_pixel_when_the_margin_is_odd() {
        let image = RgbImage::from_fn(5, 2, |x, y| Rgb([x as u8, y as u8, 0]));
        let actual = fit(&image, (2, 2));
        assert_eq!(actual.into_raw(), [1, 0, 0, 2, 0, 0, 1, 1, 0, 2, 1, 0]);
    }

    #[test]
    fn downsampling_averages_edges_and_includes_filter_support_outside_the_crop() {
        let image = RgbImage::from_fn(8, 4, |x, _| Rgb([if x < 4 { 0 } else { 255 }, 0, 0]));
        let actual = fit(&image, (2, 2));
        assert_eq!(
            actual.into_raw(),
            [32, 0, 0, 223, 0, 0, 32, 0, 0, 223, 0, 0]
        );
        let image = RgbImage::from_fn(2, 2, |x, _| Rgb([x as u8 * 255; 3]));
        assert_eq!(fit(&image, (1, 1)).into_raw(), [128; 3]);
    }

    #[test]
    fn oriented_images_keep_the_expected_center_and_pixel_order() {
        use image::{metadata::Orientation::*, DynamicImage};
        let source = RgbImage::from_fn(5, 3, |x, y| Rgb([(x + 10 * y) as u8; 3]));
        for (orientation, target, expected) in [
            (NoTransforms, (2, 3), [1, 2, 11, 12, 21, 22]),
            (FlipHorizontal, (2, 3), [3, 2, 13, 12, 23, 22]),
            (FlipVertical, (2, 3), [21, 22, 11, 12, 1, 2]),
            (Rotate180, (2, 3), [23, 22, 13, 12, 3, 2]),
            (Rotate90, (3, 2), [21, 11, 1, 22, 12, 2]),
            (Rotate270, (3, 2), [3, 13, 23, 2, 12, 22]),
            (Rotate90FlipH, (3, 2), [1, 11, 21, 2, 12, 22]),
            (Rotate270FlipH, (3, 2), [23, 13, 3, 22, 12, 2]),
        ] {
            let mut image = DynamicImage::ImageRgb8(source.clone());
            image.apply_orientation(orientation);
            let actual = fit(&image.into_rgb8(), target);
            assert_eq!(actual.pixels().map(|p| p[0]).collect::<Vec<_>>(), expected);
        }
    }

    #[test]
    fn all_cover_geometries_match_image_triangle_exactly() {
        for (iw, ih) in [
            (1, 1),
            (1, 7),
            (7, 1),
            (2, 3),
            (3, 2),
            (8, 4),
            (17, 31),
            (32, 11),
            (151, 113),
        ] {
            let image = RgbImage::from_fn(iw, ih, |x, y| {
                Rgb([
                    ((x * 73 + y * 151) % 256) as u8,
                    if (x + y) % 2 == 0 { 0 } else { 255 },
                    ((x * 7 + y * 19) % 256) as u8,
                ])
            });
            for (w, h) in [
                (1, 1),
                (1, 13),
                (13, 1),
                (2, 2),
                (3, 7),
                (7, 3),
                (12, 12),
                (29, 17),
                (17, 29),
            ] {
                let scale = (w as f32 / iw as f32).max(h as f32 / ih as f32);
                let (sw, sh) = (
                    (iw as f32 * scale).ceil() as u32,
                    (ih as f32 * scale).ceil() as u32,
                );
                let reference = resize(&image, sw, sh, FilterType::Triangle);
                let reference = crop_imm(&reference, (sw - w) / 2, (sh - h) / 2, w, h).to_image();
                assert_eq!(fit(&image, (w, h)), reference, "{iw}x{ih} -> {w}x{h}");
            }
        }
    }
}
