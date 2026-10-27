//! The OpenPen icon (blue ring with a dot), drawn in code at any size.
//!
//! Shared by the app (window + tray icon) and `build.rs`, which bakes it into
//! the exe as a multi-size .ico so Explorer shows it too.

const COLOR: [u8; 3] = [90, 150, 255];

/// Straight-alpha RGBA pixels, `size` x `size`, anti-aliased by 4x4 supersampling.
pub fn rgba(size: u32) -> Vec<u8> {
    // Geometry in a 32-unit design space: ring 11..15 around the center, dot radius 4.5.
    let unit = size as f32 / 32.0;
    let center = size as f32 / 2.0;
    const SS: u32 = 4;
    let mut out = Vec::with_capacity((size * size * 4) as usize);
    for y in 0..size {
        for x in 0..size {
            let mut hits = 0;
            for sy in 0..SS {
                for sx in 0..SS {
                    let px = x as f32 + (sx as f32 + 0.5) / SS as f32 - center;
                    let py = y as f32 + (sy as f32 + 0.5) / SS as f32 - center;
                    let r = (px * px + py * py).sqrt() / unit;
                    if (11.0..15.0).contains(&r) || r < 4.5 {
                        hits += 1;
                    }
                }
            }
            let alpha = (hits * 255 / (SS * SS)) as u8;
            out.extend_from_slice(&[COLOR[0], COLOR[1], COLOR[2], alpha]);
        }
    }
    out
}

/// A Windows .ico file containing the icon at each of `sizes` (max 256).
#[allow(dead_code)] // only used by build.rs
pub fn ico(sizes: &[u32]) -> Vec<u8> {
    let images: Vec<Vec<u8>> = sizes.iter().map(|&s| bmp_entry(s)).collect();
    let mut file = Vec::new();
    // ICONDIR: reserved, type 1 = icon, image count.
    file.extend_from_slice(&0u16.to_le_bytes());
    file.extend_from_slice(&1u16.to_le_bytes());
    file.extend_from_slice(&(sizes.len() as u16).to_le_bytes());
    let mut offset = 6 + 16 * sizes.len() as u32;
    for (&size, image) in sizes.iter().zip(&images) {
        // ICONDIRENTRY: width/height (0 means 256), palette, reserved, planes, bpp, bytes, offset.
        let dim = if size >= 256 { 0 } else { size as u8 };
        file.extend_from_slice(&[dim, dim, 0, 0]);
        file.extend_from_slice(&1u16.to_le_bytes());
        file.extend_from_slice(&32u16.to_le_bytes());
        file.extend_from_slice(&(image.len() as u32).to_le_bytes());
        file.extend_from_slice(&offset.to_le_bytes());
        offset += image.len() as u32;
    }
    for image in images {
        file.extend(image);
    }
    file
}

/// One 32-bit BMP icon image: header, bottom-up BGRA pixels, then an empty AND mask.
fn bmp_entry(size: u32) -> Vec<u8> {
    let pixels = rgba(size);
    let mask_row = size.div_ceil(32) * 4;
    let mut out = Vec::new();
    // BITMAPINFOHEADER; height is doubled because it covers pixels + mask.
    for v in [40u32, size, size * 2] {
        out.extend_from_slice(&v.to_le_bytes());
    }
    out.extend_from_slice(&1u16.to_le_bytes());
    out.extend_from_slice(&32u16.to_le_bytes());
    out.extend_from_slice(&[0u8; 24]);
    for y in (0..size).rev() {
        for x in 0..size {
            let i = ((y * size + x) * 4) as usize;
            out.extend_from_slice(&[pixels[i + 2], pixels[i + 1], pixels[i], pixels[i + 3]]);
        }
    }
    out.extend(std::iter::repeat_n(0u8, (mask_row * size) as usize));
    out
}
