//! PNG 编码 + base64。
//!
//! PNG 用 `png` crate（纯 Rust、无 C 依赖、可编到 wasm32）；
//! base64 自己写十来行——不想为一个编码表引依赖，也不想让它成为
//! 「MCP 返回图片」这条路上唯一挡住我们的东西。

use std::io::Write;

/// 把 RGB 原始像素编码成 PNG。
pub fn encode_rgb(width: u32, height: u32, rgb: &[u8]) -> Vec<u8> {
    let expected = (width as usize) * (height as usize) * 3;
    assert_eq!(
        rgb.len(),
        expected,
        "像素数不对：{}×{}×3 = {}，实际 {}",
        width,
        height,
        expected,
        rgb.len()
    );
    let mut out = Vec::new();
    {
        let mut enc = png::Encoder::new(&mut out, width, height);
        enc.set_color(png::ColorType::Rgb);
        enc.set_depth(png::BitDepth::Eight);
        let mut writer = enc.write_header().expect("PNG 头写入失败");
        writer.write_image_data(rgb).expect("PNG 数据写入失败");
    }
    out
}

/// 读回 PNG（测试用：验证编码-解码往返一致）。
pub fn decode_rgb(png: &[u8]) -> Option<(u32, u32, Vec<u8>)> {
    let dec = png::Decoder::new(png);
    let mut reader = dec.read_info().ok()?;
    let mut buf = vec![0; reader.output_buffer_size()];
    let info = reader.next_frame(&mut buf).ok()?;
    buf.truncate(info.buffer_size());
    Some((info.width, info.height, buf))
}

const B64: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";

/// 标准 base64（带 `=` 补齐）。
pub fn base64(data: &[u8]) -> String {
    let mut out = String::with_capacity(data.len().div_ceil(3) * 4);
    for chunk in data.chunks(3) {
        let b0 = chunk[0] as u32;
        let b1 = *chunk.get(1).unwrap_or(&0) as u32;
        let b2 = *chunk.get(2).unwrap_or(&0) as u32;
        let n = (b0 << 16) | (b1 << 8) | b2;
        out.push(B64[(n >> 18) as usize & 63] as char);
        out.push(B64[(n >> 12) as usize & 63] as char);
        if chunk.len() > 1 {
            out.push(B64[(n >> 6) as usize & 63] as char);
        } else {
            out.push('=');
        }
        if chunk.len() > 2 {
            out.push(B64[n as usize & 63] as char);
        } else {
            out.push('=');
        }
    }
    out
}

/// 写文件（顺手建目录，省得调用方到处 `create_dir_all`）。
pub fn write_file(path: &std::path::Path, bytes: &[u8]) -> std::io::Result<()> {
    if let Some(dir) = path.parent() {
        if !dir.as_os_str().is_empty() {
            std::fs::create_dir_all(dir)?;
        }
    }
    let mut f = std::fs::File::create(path)?;
    f.write_all(bytes)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn base64_matches_known_vectors() {
        assert_eq!(base64(b""), "");
        assert_eq!(base64(b"f"), "Zg==");
        assert_eq!(base64(b"fo"), "Zm8=");
        assert_eq!(base64(b"foo"), "Zm9v");
        assert_eq!(base64(b"foob"), "Zm9vYg==");
        assert_eq!(base64(b"fooba"), "Zm9vYmE=");
        assert_eq!(base64(b"foobar"), "Zm9vYmFy");
    }

    #[test]
    fn png_roundtrip_is_lossless() {
        // 3×2 的彩条
        let rgb = vec![
            255, 0, 0, 0, 255, 0, 0, 0, 255, //
            255, 255, 0, 0, 255, 255, 255, 0, 255,
        ];
        let png = encode_rgb(3, 2, &rgb);
        assert_eq!(&png[..8], b"\x89PNG\r\n\x1a\n", "PNG 魔数不对");
        let (w, h, back) = decode_rgb(&png).expect("应当能解回来");
        assert_eq!((w, h), (3, 2));
        assert_eq!(back, rgb, "解码结果必须逐字节相同");
    }

    #[test]
    fn encode_rejects_wrong_pixel_count() {
        let r = std::panic::catch_unwind(|| encode_rgb(2, 2, &[0, 0, 0]));
        assert!(r.is_err(), "像素数不对应当直接报错而不是画出一张坏图");
    }
}
