//! skill / plugin 的「包」：打包、解包、清单。
//!
//! 为什么要自己写 ZIP 而不引 `zip` crate：
//! - 我们只需要极小一块（store 写 + store/deflate 读），**字节级可控**才谈得上对账；
//! - **确定性**：写侧只用 store、时间戳固定、条目按名字排序 ⇒ 同输入必得同字节，
//!   于是 sha256 可以当版本锁（供应链那条）。
//! - 标准 unzip / 7z / 资源管理器都能读 store-only 的 zip，所以「自己写」不影响别人。
//!
//! 读侧要能吃别人的 zip（别人用 deflate 压的），所以 deflate 交给 flate2——
//! 它本来就在 workspace 的依赖图里（png 拉的），不新增第三方。

use anyhow::{anyhow, bail, Context, Result};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

// ---------------------------------------------------------------- CRC32（ZIP 用的是 IEEE 多项式）

fn crc32(data: &[u8]) -> u32 {
    let mut table = [0u32; 256];
    for (i, slot) in table.iter_mut().enumerate() {
        let mut c = i as u32;
        for _ in 0..8 {
            c = if c & 1 != 0 { 0xEDB8_8320 ^ (c >> 1) } else { c >> 1 };
        }
        *slot = c;
    }
    let mut crc = 0xFFFF_FFFFu32;
    for &b in data {
        crc = table[((crc ^ b as u32) & 0xFF) as usize] ^ (crc >> 8);
    }
    crc ^ 0xFFFF_FFFF
}

// ---------------------------------------------------------------- 写：store-only

/// 固定的 DOS 时间（1980-01-01 00:00）。**不写墙钟**：
/// 同内容同字节是这里的硬要求，带上当前时间就永远对不上了（与内核的日志哈希同一条纪律）。
const DOS_TIME: u16 = 0;
const DOS_DATE: u16 = 0x0021;

fn u16le(v: u16) -> [u8; 2] {
    v.to_le_bytes()
}
fn u32le(v: u32) -> [u8; 4] {
    v.to_le_bytes()
}

/// 把若干 `(路径, 字节)` 打成 zip（store）。路径用 `/` 分隔，相对路径。
pub fn zip_store(entries: &[(String, Vec<u8>)]) -> Result<Vec<u8>> {
    let mut out = Vec::new();
    let mut central = Vec::new();
    // 名字排序 ⇒ 字节确定
    let mut sorted: Vec<&(String, Vec<u8>)> = entries.iter().collect();
    sorted.sort_by(|a, b| a.0.cmp(&b.0));

    for (name, data) in &sorted {
        if name.ends_with('/') {
            bail!("目录条目不入包（我们只写入文件）: {}", name);
        }
        check_entry_name(name)?;
        let name_bytes = name.as_bytes();
        let crc = crc32(data);
        let offset = out.len() as u32;

        // 本地文件头：flag 0x0800 = 文件名是 UTF-8
        out.extend_from_slice(&u32le(0x0403_4b50));
        out.extend_from_slice(&u16le(20)); // version needed
        out.extend_from_slice(&u16le(0x0800)); // flags
        out.extend_from_slice(&u16le(0)); // method: store
        out.extend_from_slice(&u16le(DOS_TIME));
        out.extend_from_slice(&u16le(DOS_DATE));
        out.extend_from_slice(&u32le(crc));
        out.extend_from_slice(&u32le(data.len() as u32));
        out.extend_from_slice(&u32le(data.len() as u32));
        out.extend_from_slice(&u16le(name_bytes.len() as u16));
        out.extend_from_slice(&u16le(0)); // extra
        out.extend_from_slice(name_bytes);
        out.extend_from_slice(data);

        // 中央目录
        central.extend_from_slice(&u32le(0x0201_4b50));
        central.extend_from_slice(&u16le(20 | (3 << 8))); // made by: unix
        central.extend_from_slice(&u16le(20));
        central.extend_from_slice(&u16le(0x0800));
        central.extend_from_slice(&u16le(0));
        central.extend_from_slice(&u16le(DOS_TIME));
        central.extend_from_slice(&u16le(DOS_DATE));
        central.extend_from_slice(&u32le(crc));
        central.extend_from_slice(&u32le(data.len() as u32));
        central.extend_from_slice(&u32le(data.len() as u32));
        central.extend_from_slice(&u16le(name_bytes.len() as u16));
        central.extend_from_slice(&u16le(0)); // extra
        central.extend_from_slice(&u16le(0)); // comment
        central.extend_from_slice(&u16le(0)); // disk
        central.extend_from_slice(&u16le(0)); // internal attrs
        central.extend_from_slice(&u32le(0o100644 << 16)); // external attrs: -rw-r--r--
        central.extend_from_slice(&u32le(offset));
        central.extend_from_slice(name_bytes);
    }

    let cd_offset = out.len() as u32;
    let cd_size = central.len() as u32;
    out.extend_from_slice(&central);
    let count = sorted.len() as u16;

    // EOCD
    out.extend_from_slice(&u32le(0x0605_4b50));
    out.extend_from_slice(&u16le(0));
    out.extend_from_slice(&u16le(0));
    out.extend_from_slice(&u16le(count));
    out.extend_from_slice(&u16le(count));
    out.extend_from_slice(&u32le(cd_size));
    out.extend_from_slice(&u32le(cd_offset));
    out.extend_from_slice(&u16le(0));
    Ok(out)
}

/// 条目名校验：不许绝对路径、反斜杠、`.`/`..`/空段（zip slip）。
///
/// 目录条目（以 `/` 结尾）在 zip 里合法——**系统 zip 就会写**——所以这里允许，
/// 真正的对策在读者那边：跳过它们（它们不带数据，也不该被建出来）。
fn check_entry_name(name: &str) -> Result<()> {
    if name.is_empty() || name.starts_with('/') || name.contains('\\') {
        bail!("包里的路径不合法: {}", name);
    }
    let trimmed = name.trim_end_matches('/');
    if trimmed.is_empty() {
        bail!("包里的路径不合法（只有斜杠）: {}", name);
    }
    for seg in trimmed.split('/') {
        if seg.is_empty() || seg == "." || seg == ".." {
            bail!("包里的路径不合法（不许 . / .. / 空段）: {}", name);
        }
    }
    Ok(())
}

// ---------------------------------------------------------------- 读：store + deflate

/// 单个条目。
pub struct Entry {
    pub name: String,
    pub bytes: Vec<u8>,
}

/// 解一个 zip。只认 store 与 deflate；路径一律过 `check_entry_name`（防 zip slip）。
pub fn unzip(bytes: &[u8]) -> Result<Vec<Entry>> {
    let eocd = find_eocd(bytes).ok_or_else(|| anyhow!("不是合法的 zip（找不到 EOCD 结尾记录）"))?;
    let count = read_u16(bytes, eocd + 10)? as usize;
    let cd_offset = read_u32(bytes, eocd + 16)? as usize;
    let mut entries = Vec::with_capacity(count);
    let mut p = cd_offset;

    for _ in 0..count {
        if read_u32(bytes, p)? != 0x0201_4b50 {
            bail!("中央目录损坏（偏移 {}）", p);
        }
        let method = read_u16(bytes, p + 10)?;
        let crc_want = read_u32(bytes, p + 16)?;
        let csize = read_u32(bytes, p + 20)? as usize;
        let usize_ = read_u32(bytes, p + 24)? as usize;
        let namelen = read_u16(bytes, p + 28)? as usize;
        let extralen = read_u16(bytes, p + 30)? as usize;
        let commentlen = read_u16(bytes, p + 32)? as usize;
        let local_offset = read_u32(bytes, p + 42)? as usize;
        let name = String::from_utf8_lossy(slice(bytes, p + 46, namelen)?).to_string();
        p += 46 + namelen + extralen + commentlen;

        check_entry_name(&name)?;
        if name.ends_with('/') {
            continue; // 目录条目：系统 zip 会写，我们不建这种空目录
        }
        // 数据从**本地头**算起（本地头的 extra 长度可能与中央目录不同）
        if read_u32(bytes, local_offset)? != 0x0403_4b50 {
            bail!("本地文件头损坏（{}）", name);
        }
        let l_namelen = read_u16(bytes, local_offset + 26)? as usize;
        let l_extralen = read_u16(bytes, local_offset + 28)? as usize;
        let data_at = local_offset + 30 + l_namelen + l_extralen;
        let raw = slice(bytes, data_at, csize)?;

        let data = match method {
            0 => raw.to_vec(),
            8 => {
                use std::io::Read;
                let mut d = flate2::read::DeflateDecoder::new(raw);
                let mut out = Vec::with_capacity(usize_.max(64));
                d.read_to_end(&mut out).with_context(|| format!("解压失败: {}", name))?;
                out
            }
            m => bail!("包里 {} 用了不支持的压缩方式 {}（只支持 store/deflate）", name, m),
        };
        if data.len() != usize_ {
            bail!("{} 解出来的长度不对：期望 {}，实际 {}", name, usize_, data.len());
        }
        if crc32(&data) != crc_want {
            bail!("{} 校验和不对（包可能损坏）", name);
        }
        entries.push(Entry { name, bytes: data });
    }
    Ok(entries)
}

fn find_eocd(bytes: &[u8]) -> Option<usize> {
    if bytes.len() < 22 {
        return None;
    }
    let start = bytes.len().saturating_sub(22 + 0xFFFF);
    (start..=bytes.len() - 22)
        .rev()
        .find(|&i| bytes[i..i + 4] == [0x50, 0x4b, 0x05, 0x06])
}

fn slice(bytes: &[u8], at: usize, len: usize) -> Result<&[u8]> {
    bytes
        .get(at..at + len)
        .ok_or_else(|| anyhow!("包被截断（偏移 {} 长度 {}）", at, len))
}
fn read_u16(bytes: &[u8], at: usize) -> Result<u16> {
    let s = slice(bytes, at, 2)?;
    Ok(u16::from_le_bytes([s[0], s[1]]))
}
fn read_u32(bytes: &[u8], at: usize) -> Result<u32> {
    let s = slice(bytes, at, 4)?;
    Ok(u32::from_le_bytes([s[0], s[1], s[2], s[3]]))
}

// ---------------------------------------------------------------- 清单

/// skill / plugin 包里的清单（`skill.json` / `plugin.json`）。
///
/// 它是**包内**的单一出处：平台与安装侧都读它，所以「哪些文件、入口是哪个」不会两头各写一份。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BundleManifest {
    pub spec: String,
    pub name: String,
    #[serde(default)]
    pub kind: String,
    #[serde(default)]
    pub title: String,
    #[serde(default)]
    pub summary: String,
    /// 入口文件（skill 是 `SKILL.md`）；必须在 `files` 里
    pub entry: String,
    #[serde(default)]
    pub files: Vec<String>,
    #[serde(default)]
    pub agents: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub requires: Option<serde_json::Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mcp: Option<serde_json::Value>,
}

pub const SPEC: &str = "rsi3d-bundle/v1";

impl BundleManifest {
    pub fn load(dir: &Path, kind: &str) -> Result<(Self, PathBuf)> {
        let candidates = [format!("{}.json", kind), "bundle.json".to_string()];
        for c in &candidates {
            let p = dir.join(c);
            if p.is_file() {
                let text = std::fs::read_to_string(&p)
                    .with_context(|| format!("读取清单失败: {}", p.display()))?;
                let m: BundleManifest = serde_json::from_str(&text)
                    .with_context(|| format!("清单不是合法 JSON: {}", p.display()))?;
                return Ok((m, p));
            }
        }
        bail!(
            "{} 里没有清单（要 {} 或 bundle.json）",
            dir.display(),
            candidates[0]
        )
    }

    /// 清单自洽：entry 要在 files 里、files 不能空、名字要像 slug。
    pub fn validate(&self, kind: &str) -> Result<()> {
        if self.spec != SPEC {
            bail!("清单 spec 应为 {}，实际 {}", SPEC, self.spec);
        }
        if self.name.is_empty() || !self.name.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-') {
            bail!("name 只能是小写字母/数字/连字符: {}", self.name);
        }
        if !self.kind.is_empty() && self.kind != kind {
            bail!("清单 kind={} 与 --kind {} 不一致", self.kind, kind);
        }
        if self.files.is_empty() {
            bail!("files 不能为空");
        }
        if !self.files.iter().any(|f| f == &self.entry) {
            bail!("entry（{}）必须出现在 files 里", self.entry);
        }
        Ok(())
    }
}

/// 把一个目录打成确定性 zip：清单校验 → 收集字节 → 打包。
///
/// 返回 `(zip 字节, 清单)`。
pub fn pack_dir(dir: &Path, kind: &str) -> Result<(Vec<u8>, BundleManifest)> {
    let (manifest, _) = BundleManifest::load(dir, kind)?;
    manifest.validate(kind)?;

    let mut found = BTreeMap::new();
    walk(dir, dir, &mut found)?;
    let manifest_file = format!("{}.json", kind);
    let manifest_file = if dir.join(&manifest_file).is_file() {
        manifest_file
    } else {
        "bundle.json".to_string()
    };

    // 清单里声明的每个文件都必须真的在
    let mut entries: Vec<(String, Vec<u8>)> = Vec::new();
    for rel in &manifest.files {
        let bytes = found
            .get(rel)
            .ok_or_else(|| anyhow!("清单声明了 {}，但目录里没有这个文件", rel))?;
        entries.push((rel.clone(), bytes.clone()));
    }
    // 未声明的文件：不算错，但要**说出来**——静默丢文件是最难查的那种包
    let undeclared: Vec<&String> = found
        .keys()
        .filter(|k| !manifest.files.contains(k) && **k != manifest_file)
        .collect();
    if !undeclared.is_empty() {
        bail!(
            "目录里有清单没声明的文件（要么加进 files，要么删掉）：{}",
            undeclared
                .iter()
                .map(|s| s.as_str())
                .collect::<Vec<_>>()
                .join(", ")
        );
    }
    entries.push((manifest_file.clone(), std::fs::read(dir.join(&manifest_file))?));

    Ok((zip_store(&entries)?, manifest))
}

fn walk(root: &Path, at: &Path, out: &mut BTreeMap<String, Vec<u8>>) -> Result<()> {
    for e in std::fs::read_dir(at).with_context(|| format!("读取目录失败: {}", at.display()))? {
        let e = e?;
        let p = e.path();
        let name = e.file_name().to_string_lossy().to_string();
        if name.starts_with('.') {
            continue; // 隐藏文件不入包（.DS_Store / .git 这类）
        }
        let meta = std::fs::symlink_metadata(&p)?;
        if meta.file_type().is_symlink() {
            // 软链不入包：解包侧要么跟着跳到包外，要么变成普通文件——两头都不好解释
            continue;
        }
        if meta.is_dir() {
            walk(root, &p, out)?;
        } else {
            let rel = p
                .strip_prefix(root)
                .expect("子路径")
                .to_string_lossy()
                .replace('\\', "/");
            out.insert(rel, std::fs::read(&p)?);
        }
    }
    Ok(())
}

/// 解包到 `dest`（`dest` 会按需创建）。返回写出来的相对路径。
///
/// 安全：条目名先过白名单（无 `..`/绝对/反斜杠），最终路径再确认落在 `dest` 之内。
pub fn unpack(zip_bytes: &[u8], dest: &Path) -> Result<Vec<String>> {
    let entries = unzip(zip_bytes)?;
    if entries.is_empty() {
        bail!("包里没有文件");
    }
    std::fs::create_dir_all(dest).with_context(|| format!("创建目录失败: {}", dest.display()))?;
    let root = dest.canonicalize().unwrap_or_else(|_| dest.to_path_buf());
    let mut written = Vec::new();

    for e in entries {
        let target = dest.join(&e.name);
        let mut probe = target.clone();
        while let Some(parent) = probe.parent() {
            if parent.exists() {
                let c = parent.canonicalize().unwrap_or_else(|_| parent.to_path_buf());
                if !c.starts_with(&root) {
                    bail!("包里的路径想跑到目标目录外面去: {}", e.name);
                }
                break;
            }
            probe = parent.to_path_buf();
        }
        if let Some(parent) = target.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(&target, &e.bytes)
            .with_context(|| format!("写文件失败: {}", target.display()))?;
        written.push(e.name);
    }
    Ok(written)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("rsi3d-bundle-test-{}-{}", name, std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn crc32_matches_known_vectors() {
        assert_eq!(crc32(b""), 0x0000_0000);
        assert_eq!(crc32(b"123456789"), 0xCBF4_3926);
        assert_eq!(crc32(b"hello"), 0x3610_A686);
    }

    #[test]
    fn round_trip_and_deterministic() {
        let entries = vec![
            ("b.txt".to_string(), b"second".to_vec()),
            ("a.txt".to_string(), b"first".to_vec()),
        ];
        let z1 = zip_store(&entries).unwrap();
        let z2 = zip_store(&entries.iter().rev().cloned().collect::<Vec<_>>()).unwrap();
        assert_eq!(z1, z2, "条目顺序不该影响字节（要能当版本锁）");

        let back = unzip(&z1).unwrap();
        assert_eq!(back.len(), 2);
        assert_eq!(back[0].name, "a.txt");
        assert_eq!(back[0].bytes, b"first");
    }

    #[test]
    fn rejects_zip_slip() {
        // 手工造一个 ../evil.txt 的条目
        let evil = vec![("../evil.txt".to_string(), b"x".to_vec())];
        let err = zip_store(&evil).unwrap_err().to_string();
        assert!(err.contains("不合法"), "{}", err);
    }

    #[test]
    fn reads_empty_and_bad_input_gracefully() {
        assert!(unzip(b"not a zip").is_err());
        assert!(unzip(&[]).is_err());
    }

    #[test]
    fn reads_foreign_zip_with_directory_entries() {
        // 夹具：系统 zip / python zipfile 打出来的包会带**目录条目**（`references/`）。
        // 这是实测抓到的：我们的读者原来把「以 / 结尾」当非法路径，于是别人的包一律读不了。
        // 夹具是 python zipfile（store）现生成的 230 字节，就在下面。
        let hex: String = [
            "504b0304140000000000000021000000000000000000000000000b0000007265666572656e636573",
            "2f504b030414000000000043bf345dff9b1c7d04000000040000000f0000007265666572656e6365",
            "732f612e6d647265660a504b01021403140000000000000021000000000000000000000000000b00",
            "00000000000000001000ed41000000007265666572656e6365732f504b0102140314000000000043",
            "bf345dff9b1c7d04000000040000000f00000000000000000000008001290000007265666572656e",
            "6365732f612e6d64504b05060000000002000200760000005a0000000000",
        ]
        .concat();
        let bytes: Vec<u8> = (0..hex.len() / 2)
            .map(|i| u8::from_str_radix(&hex[i * 2..i * 2 + 2], 16).unwrap())
            .collect();
        let entries = unzip(&bytes).expect("别人的包要能读");
        assert_eq!(entries.len(), 1, "目录条目应被跳过，只剩文件");
        assert_eq!(entries[0].name, "references/a.md");
        assert_eq!(entries[0].bytes, b"ref\n");
    }

    #[test]
    fn pack_dir_rejects_undeclared_files() {
        let dir = tmp("undeclared");
        std::fs::write(dir.join("SKILL.md"), "# hi\n").unwrap();
        std::fs::write(dir.join("extra.md"), "x\n").unwrap();
        std::fs::write(
            dir.join("skill.json"),
            r#"{"spec":"rsi3d-bundle/v1","name":"demo","kind":"skill","entry":"SKILL.md","files":["SKILL.md"]}"#,
        )
        .unwrap();
        let err = pack_dir(&dir, "skill").unwrap_err().to_string();
        assert!(err.contains("没声明的文件"), "{}", err);
    }

    #[test]
    fn pack_dir_round_trips_a_real_skill() {
        let dir = tmp("real");
        std::fs::create_dir_all(dir.join("references")).unwrap();
        std::fs::write(dir.join("SKILL.md"), "# demo\n").unwrap();
        std::fs::write(dir.join("references/a.md"), "ref\n").unwrap();
        std::fs::write(
            dir.join("skill.json"),
            r#"{"spec":"rsi3d-bundle/v1","name":"demo","kind":"skill","entry":"SKILL.md","files":["SKILL.md","references/a.md"]}"#,
        )
        .unwrap();
        let (zip, m) = pack_dir(&dir, "skill").unwrap();
        assert_eq!(m.name, "demo");
        let out = tmp("real-out");
        let names = unpack(&zip, &out).unwrap();
        assert_eq!(names.len(), 3);
        assert_eq!(std::fs::read_to_string(out.join("references/a.md")).unwrap(), "ref\n");
    }
}
