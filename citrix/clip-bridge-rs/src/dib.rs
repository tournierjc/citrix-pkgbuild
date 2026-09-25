//! DIB <-> RGB conversion in the Citrix `_ISL_DIB` layout (pixels at 0x428),
//! plus PNG encode/decode. Port of the Python citrix-clip-bridge helpers.

pub const ISL_PIXELS: usize = 0x428;
pub const ISL_RGBQUAD: usize = 0x400;

/// 32bpp BI_RGB DIB in Citrix _ISL_DIB layout: BITMAPINFOHEADER, a 1024-byte
/// RGBQUAD slot, then bottom-up BGRA pixels.
pub fn rgb_to_isl_dib(rgb: &[u8], w: u32, h: u32) -> Vec<u8> {
    let (w, h) = (w as usize, h as usize);
    let stride = w * 4;
    let mut out = vec![0u8; ISL_PIXELS + stride * h];
    out[0..4].copy_from_slice(&40u32.to_le_bytes()); // biSize
    out[4..8].copy_from_slice(&(w as i32).to_le_bytes()); // biWidth
    out[8..12].copy_from_slice(&(h as i32).to_le_bytes()); // biHeight (bottom-up)
    out[12..14].copy_from_slice(&1u16.to_le_bytes()); // biPlanes
    out[14..16].copy_from_slice(&32u16.to_le_bytes()); // biBitCount
    // biCompression = BI_RGB (0)
    out[20..24].copy_from_slice(&((stride * h) as u32).to_le_bytes()); // biSizeImage
    for y in 0..h {
        let src = (h - 1 - y) * w * 3;
        let dst = ISL_PIXELS + y * stride;
        for (i, px) in rgb[src..src + w * 3].chunks_exact(3).enumerate() {
            out[dst + i * 4..dst + i * 4 + 3].copy_from_slice(&[px[2], px[1], px[0]]);
        }
    }
    out
}

/// Parse a DIB into top-down RGB8. Expects the Citrix layout (pixels at
/// 0x428) but falls back to header + palette offsets like the Python version.
/// Handles 24/32bpp, bottom-up and top-down (negative height).
pub fn isl_dib_to_rgb(data: &[u8]) -> Option<(Vec<u8>, u32, u32)> {
    if data.len() < 40 {
        return None;
    }
    let bi_size = u32::from_le_bytes(data[0..4].try_into().ok()?) as usize;
    let w = i32::from_le_bytes(data[4..8].try_into().ok()?);
    let h = i32::from_le_bytes(data[8..12].try_into().ok()?);
    let bpp = u16::from_le_bytes(data[14..16].try_into().ok()?);
    let comp = u32::from_le_bytes(data[16..20].try_into().ok()?);
    let clrused = u32::from_le_bytes(data[32..36].try_into().ok()?) as usize;
    if bi_size < 40 || w <= 0 || h == 0 || !matches!(bpp, 24 | 32) {
        return None;
    }
    let w = w as usize;
    let h_abs = h.unsigned_abs() as usize;
    let stride = (w * bpp as usize).div_ceil(32) * 4;
    let mut pix_off = ISL_PIXELS;
    if data.len() < pix_off + stride * h_abs {
        pix_off = bi_size + if clrused > 0 { clrused * 4 } else { 0 };
        if comp == 3 {
            pix_off += 12; // BI_BITFIELDS masks
        }
    }
    if stride == 0 || pix_off + stride * h_abs > data.len() {
        return None;
    }
    let src_bpp = (bpp / 8) as usize;
    let top_down = h < 0;
    let mut rgb = vec![0u8; w * h_abs * 3];
    for y in 0..h_abs {
        let src_y = if top_down { y } else { h_abs - 1 - y };
        let sbase = pix_off + src_y * stride;
        for (i, px) in data[sbase..sbase + w * src_bpp]
            .chunks_exact(src_bpp)
            .enumerate()
        {
            let d = y * w * 3 + i * 3;
            rgb[d..d + 3].copy_from_slice(&[px[2], px[1], px[0]]);
        }
    }
    Some((rgb, w as u32, h_abs as u32))
}

/// Bilinear-downscale an RGB8 buffer so the resulting _ISL_DIB fits into
/// `max_bytes` (the X11 BIG-REQUESTS request limit). Returns the input
/// unchanged when it already fits.
pub fn downscale_to_fit(rgb: &[u8], w: u32, h: u32, max_bytes: usize) -> (Vec<u8>, u32, u32) {
    let max_pixels = max_bytes.saturating_sub(ISL_PIXELS) / 4;
    let (w, h) = (w as usize, h as usize);
    if w == 0 || h == 0 || w * h <= max_pixels {
        return (rgb.to_vec(), w as u32, h as u32);
    }
    let scale = (max_pixels as f64 / (w * h) as f64).sqrt();
    let nw = ((w as f64 * scale).floor() as usize).max(1);
    let nh = ((h as f64 * scale).floor() as usize).max(1);
    let mut out = vec![0u8; nw * nh * 3];
    for y in 0..nh {
        let sy = ((y as f64 + 0.5) / scale - 0.5).clamp(0.0, (h - 1) as f64);
        let y0 = sy.floor() as usize;
        let y1 = (y0 + 1).min(h - 1);
        let fy = sy - y0 as f64;
        for x in 0..nw {
            let sx = ((x as f64 + 0.5) / scale - 0.5).clamp(0.0, (w - 1) as f64);
            let x0 = sx.floor() as usize;
            let x1 = (x0 + 1).min(w - 1);
            let fx = sx - x0 as f64;
            for c in 0..3 {
                let p00 = rgb[(y0 * w + x0) * 3 + c] as f64;
                let p01 = rgb[(y0 * w + x1) * 3 + c] as f64;
                let p10 = rgb[(y1 * w + x0) * 3 + c] as f64;
                let p11 = rgb[(y1 * w + x1) * 3 + c] as f64;
                let top = p00 + (p01 - p00) * fx;
                let bot = p10 + (p11 - p10) * fx;
                out[(y * nw + x) * 3 + c] = (top + (bot - top) * fy).round() as u8;
            }
        }
    }
    (out, nw as u32, nh as u32)
}

/// CF_DIB layout: BITMAPINFOHEADER followed directly by pixels (no RGBQUAD slot).
pub fn as_cf_dib(isl: &[u8]) -> Vec<u8> {
    if isl.len() < ISL_PIXELS {
        return isl.to_vec();
    }
    let mut out = Vec::with_capacity(40 + isl.len() - ISL_PIXELS);
    out.extend_from_slice(&isl[..40]);
    out.extend_from_slice(&isl[ISL_PIXELS..]);
    out
}

/// Wrap a CF_DIB in a BMP file header.
pub fn wrap_bmp(cf: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(14 + cf.len());
    out.extend_from_slice(b"BM");
    out.extend_from_slice(&(14 + cf.len() as u32).to_le_bytes());
    out.extend_from_slice(&0u32.to_le_bytes()); // reserved
    out.extend_from_slice(&54u32.to_le_bytes()); // bfOffBits
    out.extend_from_slice(cf);
    out
}


/// Decode PNG/JPEG/BMP/WEBP (or anything `image` supports) into RGB8 with
/// alpha composited over white — same as the PNG path Citrix expects.
pub fn decode_image(bytes: &[u8]) -> Option<(Vec<u8>, u32, u32)> {
    if bytes.first() == Some(&0x89) && bytes.get(1..4) == Some(b"PNG") {
        return png_decode(bytes);
    }
    let img = image::load_from_memory(bytes).ok()?;
    let rgba = img.to_rgba8();
    let (w, h) = (rgba.width() as usize, rgba.height() as usize);
    let data = rgba.into_raw();
    let mut rgb = vec![0u8; w * h * 3];
    for (i, px) in data.chunks_exact(4).enumerate() {
        let a = px[3] as u16;
        for (c, &v) in px[..3].iter().enumerate() {
            rgb[i * 3 + c] = ((v as u16 * a + 255 * (255 - a)) / 255) as u8;
        }
    }
    Some((rgb, w as u32, h as u32))
}

/// Decode a PNG into RGB8, compositing alpha over white (matches the Python
/// GdkPixbuf composite path).
pub fn png_decode(png_bytes: &[u8]) -> Option<(Vec<u8>, u32, u32)> {
    let mut decoder = png::Decoder::new(std::io::Cursor::new(png_bytes));
    decoder.set_transformations(png::Transformations::EXPAND | png::Transformations::STRIP_16);
    let mut reader = decoder.read_info().ok()?;
    let mut buf = vec![0u8; reader.output_buffer_size()?];
    let info = reader.next_frame(&mut buf).ok()?;
    let (w, h) = (info.width as usize, info.height as usize);
    let data = &buf[..info.buffer_size()];
    let rgb: Vec<u8> = match info.color_type {
        png::ColorType::Rgb => data.to_vec(),
        png::ColorType::Rgba => {
            let mut out = vec![0u8; w * h * 3];
            for (i, px) in data.chunks_exact(4).enumerate() {
                let a = px[3] as u16;
                for (c, &v) in px[..3].iter().enumerate() {
                    out[i * 3 + c] = ((v as u16 * a + 255 * (255 - a)) / 255) as u8;
                }
            }
            out
        }
        png::ColorType::Grayscale => data.iter().flat_map(|&v| [v, v, v]).collect(),
        png::ColorType::GrayscaleAlpha => {
            let mut out = vec![0u8; w * h * 3];
            for (i, px) in data.chunks_exact(2).enumerate() {
                let a = px[1] as u16;
                let v = ((px[0] as u16 * a + 255 * (255 - a)) / 255) as u8;
                out[i * 3..i * 3 + 3].copy_from_slice(&[v, v, v]);
            }
            out
        }
        png::ColorType::Indexed => return None, // EXPAND already mapped these to RGB(A)
    };
    Some((rgb, w as u32, h as u32))
}

/// Encode RGB8 as PNG.
pub fn png_encode(rgb: &[u8], w: u32, h: u32) -> Option<Vec<u8>> {
    let mut out = Vec::new();
    {
        let mut encoder = png::Encoder::new(&mut out, w, h);
        encoder.set_color(png::ColorType::Rgb);
        encoder.set_depth(png::BitDepth::Eight);
        let mut writer = encoder.write_header().ok()?;
        writer.write_image_data(rgb).ok()?;
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rgb_roundtrip() {
        let (w, h) = (5u32, 3u32);
        let rgb: Vec<u8> = (0..w * h * 3).map(|i| (i * 37 + 11) as u8).collect();
        let dib = rgb_to_isl_dib(&rgb, w, h);
        assert_eq!(dib.len(), ISL_PIXELS + (w * 4 * h) as usize);
        assert_eq!(u32::from_le_bytes(dib[0..4].try_into().unwrap()), 40);
        assert_eq!(i32::from_le_bytes(dib[4..8].try_into().unwrap()), w as i32);
        assert_eq!(i32::from_le_bytes(dib[8..12].try_into().unwrap()), h as i32);
        assert_eq!(u16::from_le_bytes(dib[14..16].try_into().unwrap()), 32);
        let (back, bw, bh) = isl_dib_to_rgb(&dib).expect("parse");
        assert_eq!((bw, bh), (w, h));
        assert_eq!(back, rgb);
    }

    #[test]
    fn top_down_24bpp() {
        let (w, h) = (4usize, 2usize);
        let stride = (w * 24).div_ceil(32) * 4;
        let mut data = vec![0u8; ISL_PIXELS + stride * h];
        data[0..4].copy_from_slice(&40u32.to_le_bytes());
        data[4..8].copy_from_slice(&(w as i32).to_le_bytes());
        data[8..12].copy_from_slice(&(-(h as i32)).to_le_bytes()); // top-down
        data[14..16].copy_from_slice(&24u16.to_le_bytes());
        for i in 0..w * h {
            let base = ISL_PIXELS + (i / w) * stride + (i % w) * 3;
            data[base..base + 3].copy_from_slice(&[(i * 3) as u8, (i * 3 + 1) as u8, (i * 3 + 2) as u8]);
        }
        let (rgb, rw, rh) = isl_dib_to_rgb(&data).expect("parse");
        assert_eq!((rw, rh), (w as u32, h as u32));
        // BGR -> RGB, first top-down row first
        assert_eq!(&rgb[0..3], &[2u8, 1, 0]);
    }

    #[test]
    fn cf_dib_strips_rgbquad() {
        let rgb = vec![7u8; 2 * 2 * 3];
        let dib = rgb_to_isl_dib(&rgb, 2, 2);
        let cf = as_cf_dib(&dib);
        assert_eq!(cf.len(), 40 + 2 * 2 * 4);
        assert_eq!(u32::from_le_bytes(cf[0..4].try_into().unwrap()), 40);
        let bmp = wrap_bmp(&cf);
        assert_eq!(&bmp[0..2], b"BM");
        assert_eq!(u32::from_le_bytes(bmp[2..6].try_into().unwrap()) as usize, bmp.len());
        assert_eq!(u32::from_le_bytes(bmp[10..14].try_into().unwrap()), 54);
    }

    #[test]
    fn png_roundtrip_and_alpha() {
        let (w, h) = (8u32, 4u32);
        let rgb: Vec<u8> = (0..w * h * 3).map(|i| (i * 13 + 5) as u8).collect();
        let png = png_encode(&rgb, w, h).expect("encode");
        let (back, bw, bh) = png_decode(&png).expect("decode");
        assert_eq!((bw, bh), (w, h));
        assert_eq!(back, rgb);
    }

    #[test]
    fn downscale_fits_limit() {
        // 4K RGB frame, X11-ish limit of 16_777_148 bytes.
        let (w, h) = (3840u32, 2160u32);
        let rgb = vec![128u8; (w * h * 3) as usize];
        let max = 16_777_148usize;
        let (_out, nw, nh) = downscale_to_fit(&rgb, w, h, max);
        assert!((nw, nh) != (w, h));
        assert!(ISL_PIXELS + (nw * nh * 4) as usize <= max);
        // aspect ratio roughly preserved
        let ar_in = w as f64 / h as f64;
        let ar_out = nw as f64 / nh as f64;
        assert!((ar_in - ar_out).abs() < 0.01);
        // small images pass through untouched
        let small = vec![1u8; 4 * 4 * 3];
        let (s, sw, sh) = downscale_to_fit(&small, 4, 4, max);
        assert_eq!((sw, sh), (4, 4));
        assert_eq!(s, small);
    }

    #[test]
    fn decode_image_png() {
        let (w, h) = (2u32, 2u32);
        let rgb = vec![10u8, 20, 30, 40, 50, 60, 70, 80, 90, 100, 110, 120];
        let png = png_encode(&rgb, w, h).unwrap();
        let (back, bw, bh) = decode_image(&png).expect("png via decode_image");
        assert_eq!((bw, bh), (w, h));
        assert_eq!(back, rgb);
    }

    #[test]
    fn rejects_garbage() {
        assert!(isl_dib_to_rgb(b"short").is_none());
        let mut hdr = vec![0u8; 40];
        hdr[0..4].copy_from_slice(&40u32.to_le_bytes());
        hdr[4..8].copy_from_slice(&100i32.to_le_bytes());
        hdr[8..12].copy_from_slice(&100i32.to_le_bytes());
        hdr[14..16].copy_from_slice(&32u16.to_le_bytes());
        assert!(isl_dib_to_rgb(&hdr).is_none()); // claims pixels that are absent
    }
}
