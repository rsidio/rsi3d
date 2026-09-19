//! 无依赖小工具：hex / SHA-256 / HMAC-SHA256 / 时间 / 终端绘图。
//!
//! 这里刻意不引入 chrono、rand、sparkline 之类的库：CLI 要保持**可离线构建**。

use anyhow::{anyhow, Result};
use sha2::{Digest, Sha256};
use std::fs::File;
use std::io::Read;
use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

// ---------------- 编码 ----------------

/// 字节 → 小写十六进制。
pub fn hex_encode(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut out = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        out.push(HEX[(b >> 4) as usize] as char);
        out.push(HEX[(b & 0x0f) as usize] as char);
    }
    out
}

/// 小写十六进制 → 字节（长度为奇数或含非法字符时报错）。
pub fn hex_decode(s: &str) -> Result<Vec<u8>> {
    let s = s.trim();
    if s.len() % 2 != 0 {
        return Err(anyhow!("十六进制字符串长度必须是偶数"));
    }
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(s.len() / 2);
    for pair in bytes.chunks(2) {
        let hi = hex_val(pair[0])?;
        let lo = hex_val(pair[1])?;
        out.push((hi << 4) | lo);
    }
    Ok(out)
}

fn hex_val(c: u8) -> Result<u8> {
    match c {
        b'0'..=b'9' => Ok(c - b'0'),
        b'a'..=b'f' => Ok(c - b'a' + 10),
        b'A'..=b'F' => Ok(c - b'A' + 10),
        _ => Err(anyhow!("非法十六进制字符: {}", c as char)),
    }
}

// ---------------- 摘要与签名 ----------------

/// 内存数据的 SHA-256（十六进制）。
pub fn sha256_hex(data: &[u8]) -> String {
    let mut h = Sha256::new();
    h.update(data);
    hex_encode(&h.finalize())
}

/// 流式计算文件 SHA-256，返回 (十六进制摘要, 字节数)。
pub fn sha256_file(path: &Path) -> Result<(String, u64)> {
    let mut f = File::open(path)?;
    let mut h = Sha256::new();
    let mut buf = vec![0u8; 64 * 1024];
    let mut total: u64 = 0;
    loop {
        let n = f.read(&mut buf)?;
        if n == 0 {
            break;
        }
        h.update(&buf[..n]);
        total += n as u64;
    }
    Ok((hex_encode(&h.finalize()), total))
}

/// HMAC-SHA256（RFC 2104），十六进制。
///
/// 手写而不引 `hmac` crate：算法 20 行，换来 CLI 零新增依赖。
pub fn hmac_sha256_hex(key: &[u8], msg: &[u8]) -> String {
    const BLOCK: usize = 64;

    let mut k = if key.len() > BLOCK {
        let mut h = Sha256::new();
        h.update(key);
        h.finalize().to_vec()
    } else {
        key.to_vec()
    };
    k.resize(BLOCK, 0);

    let mut ipad = [0x36u8; BLOCK];
    let mut opad = [0x5cu8; BLOCK];
    for i in 0..BLOCK {
        ipad[i] ^= k[i];
        opad[i] ^= k[i];
    }

    let mut inner = Sha256::new();
    inner.update(&ipad);
    inner.update(msg);
    let inner_digest = inner.finalize();

    let mut outer = Sha256::new();
    outer.update(&opad);
    outer.update(&inner_digest);
    hex_encode(&outer.finalize())
}

/// 读 /dev/urandom 取 n 字节随机数（十六进制）。
pub fn rand_hex(n: usize) -> Result<String> {
    let mut f = File::open("/dev/urandom")
        .map_err(|e| anyhow!("无法读取 /dev/urandom（Windows 请用 npm 包安装 CLI）: {}", e))?;
    let mut buf = vec![0u8; n];
    f.read_exact(&mut buf)?;
    Ok(hex_encode(&buf))
}

// ---------------- 时间 ----------------

/// 当前 UTC 时间，形如 `2026-09-19T04:31:07Z`（不引 chrono）。
pub fn now_iso8601() -> String {
    let secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);
    epoch_to_iso8601(secs)
}

/// 秒级时间戳 → ISO-8601（UTC）。便于单测。
pub fn epoch_to_iso8601(secs: i64) -> String {
    let days = secs.div_euclid(86_400);
    let rem = secs.rem_euclid(86_400);
    let (y, m, d) = civil_from_days(days);
    format!(
        "{:04}-{:02}-{:02}T{:02}:{:02}:{:02}Z",
        y,
        m,
        d,
        rem / 3600,
        (rem % 3600) / 60,
        rem % 60
    )
}

/// Howard Hinnant 的 civil_from_days：自 1970-01-01 起的天数 → (年, 月, 日)。
fn civil_from_days(z: i64) -> (i64, u32, u32) {
    let z = z + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = (z - era * 146_097) as u64; // [0, 146096]
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365; // [0, 399]
    let y = yoe as i64 + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100); // [0, 365]
    let mp = (5 * doy + 2) / 153; // [0, 11]
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32; // [1, 31]
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32; // [1, 12]
    (if m <= 2 { y + 1 } else { y }, m, d)
}

// ---------------- 展示 ----------------

/// 分数曲线 → 迷你图（▁▂▃▄▅▆▇█）。
pub fn sparkline(xs: &[f64]) -> String {
    const LEVELS: [char; 8] = ['▁', '▂', '▃', '▄', '▅', '▆', '▇', '█'];
    if xs.is_empty() {
        return String::new();
    }
    let min = xs.iter().cloned().fold(f64::INFINITY, f64::min);
    let max = xs.iter().cloned().fold(f64::NEG_INFINITY, f64::max);
    let span = max - min;
    xs.iter()
        .map(|v| {
            if span <= f64::EPSILON {
                return LEVELS[3];
            }
            let t = ((v - min) / span * 7.0).round() as usize;
            LEVELS[t.min(7)]
        })
        .collect()
}

/// 字节数 → 人类可读。
pub fn human_size(n: u64) -> String {
    const UNITS: [&str; 4] = ["B", "KB", "MB", "GB"];
    let mut v = n as f64;
    let mut i = 0;
    while v >= 1024.0 && i < UNITS.len() - 1 {
        v /= 1024.0;
        i += 1;
    }
    if i == 0 {
        format!("{} {}", n, UNITS[0])
    } else {
        format!("{:.1} {}", v, UNITS[i])
    }
}

/// 截断长文本（按字符计，避免切断 UTF-8）。
pub fn ellipsis(s: &str, max_chars: usize) -> String {
    let s = s.trim();
    let mut out = String::new();
    for (i, ch) in s.chars().enumerate() {
        if i >= max_chars {
            out.push('…');
            return out;
        }
        if ch == '\n' {
            out.push(' ');
            continue;
        }
        out.push(ch);
    }
    out
}

/// 百分号编码查询参数（不引入 url crate）。
pub fn urlencode(s: &str) -> String {
    const KEEP: &[u8] = b"-_.~";
    let mut out = String::with_capacity(s.len());
    for b in s.as_bytes() {
        if b.is_ascii_alphanumeric() || KEEP.contains(b) {
            out.push(*b as char);
        } else {
            out.push_str(&format!("%{:02X}", b));
        }
    }
    out
}

/// 依据后缀猜 MIME（上传时用）。
pub fn guess_mime(filename: &str) -> &'static str {
    let lower = filename.to_ascii_lowercase();
    let ext = lower.rsplit('.').next().unwrap_or("");
    match ext {
        "json" => "application/json",
        "md" | "markdown" => "text/markdown; charset=utf-8",
        "txt" => "text/plain; charset=utf-8",
        "html" | "htm" => "text/html; charset=utf-8",
        "css" => "text/css; charset=utf-8",
        "js" => "application/javascript",
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "webp" => "image/webp",
        "svg" => "image/svg+xml",
        "zip" => "application/zip",
        "gz" | "tgz" => "application/gzip",
        "tar" => "application/x-tar",
        "glb" => "model/gltf-binary",
        "gltf" => "model/gltf+json",
        "obj" => "model/obj",
        "ply" => "application/octet-stream",
        "yaml" | "yml" => "application/yaml",
        _ => "application/octet-stream",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sha256_known_vectors() {
        assert_eq!(
            sha256_hex(b""),
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
        );
        assert_eq!(
            sha256_hex(b"abc"),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
        // 长度跨过 64 字节块边界
        assert_eq!(
            sha256_hex(b"abcdbcdecdefdefgefghfghighijhijkijkljklmklmnlmnomnopnopq"),
            "248d6a61d20638b8e5c026930c3e6039a33ce45964ff2167f6ecedd419db06c1"
        );
    }

    #[test]
    fn hmac_rfc4231_style_vector() {
        // RFC 4231 测试向量 2
        assert_eq!(
            hmac_sha256_hex(b"Jefe", b"what do ya want for nothing?"),
            "5bdcc146bf60754e6a042426089575c75a003f089d2739839dec58b964ec3843"
        );
    }

    #[test]
    fn hex_roundtrip() {
        let bytes = [0u8, 1, 15, 16, 127, 255];
        let hex = hex_encode(&bytes);
        assert_eq!(hex, "00010f107fff");
        assert_eq!(hex_decode(&hex).unwrap(), bytes.to_vec());
        assert!(hex_decode("0f0").is_err());
        assert!(hex_decode("zz").is_err());
    }

    #[test]
    fn time_and_draw() {
        assert_eq!(epoch_to_iso8601(0), "1970-01-01T00:00:00Z");
        assert_eq!(epoch_to_iso8601(1_700_000_000), "2023-11-14T22:13:20Z");
        assert_eq!(sparkline(&[]), "");
        assert_eq!(sparkline(&[0.5]).chars().count(), 1);
        assert_eq!(sparkline(&[0.0, 1.0]).chars().count(), 2);
        assert_eq!(human_size(0), "0 B");
        assert_eq!(human_size(2048), "2.0 KB");
        assert_eq!(ellipsis("中文很长的一段话", 3), "中文很…");
    }

    #[test]
    fn url_encoding() {
        assert_eq!(urlencode("home-display_2.0"), "home-display_2.0");
        assert_eq!(urlencode("北欧 风&x=1"), "%E5%8C%97%E6%AC%A7%20%E9%A3%8E%26x%3D1");
    }

    #[test]
    fn mime_by_extension() {
        assert_eq!(guess_mime("a.PNG"), "image/png");
        assert_eq!(guess_mime("pack.json"), "application/json");
        assert_eq!(guess_mime("SKILL.md"), "text/markdown; charset=utf-8");
        assert_eq!(guess_mime("noext"), "application/octet-stream");
    }
}
