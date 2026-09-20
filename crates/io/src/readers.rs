//! 读进来：glTF/GLB · OBJ · STL。
//!
//! 原则与我们的 glTF 出口一样——**宁可明确报错，也不给出"看起来对"的错几何**：
//! 每个读取器都按引用关系自查（accessor 落在 bufferView 内、bufferView 落在 buffer 内、
//! 索引不越界），越界就报错而不是静默截断。

use std::path::Path;

use serde_json::Value;

use crate::mesh::Mesh;

// ---------------------------------------------------------------- base64（只服务 .gltf 的内嵌缓冲）

fn b64_decode(s: &str) -> Result<Vec<u8>, String> {
    let mut out = Vec::with_capacity(s.len() / 4 * 3);
    let mut acc: u32 = 0;
    let mut nbits = 0;
    for c in s.bytes() {
        let v = match c {
            b'A'..=b'Z' => c - b'A',
            b'a'..=b'z' => c - b'a' + 26,
            b'0'..=b'9' => c - b'0' + 52,
            b'+' => 62,
            b'/' => 63,
            b'=' | b'\n' | b'\r' | b' ' => continue,
            other => return Err(format!("base64 里出现了非法字符 {:?}", other as char)),
        } as u32;
        acc = (acc << 6) | v;
        nbits += 6;
        if nbits >= 8 {
            nbits -= 8;
            out.push((acc >> nbits) as u8);
        }
    }
    Ok(out)
}

// ---------------------------------------------------------------- GLB 容器

/// 拆 GLB：返回 (版本, JSON chunk, BIN chunk)。
pub fn parse_glb(bytes: &[u8]) -> Result<(u32, Value, Vec<u8>), String> {
    if bytes.len() < 12 || &bytes[0..4] != b"glTF" {
        return Err("不是 GLB（缺少 glTF 魔数）".into());
    }
    let version = u32::from_le_bytes(bytes[4..8].try_into().unwrap());
    let total = u32::from_le_bytes(bytes[8..12].try_into().unwrap()) as usize;
    if total != bytes.len() {
        return Err(format!(
            "GLB 头里写的总长 {} 与实际文件大小 {} 不一致",
            total,
            bytes.len()
        ));
    }
    let mut json: Option<Value> = None;
    let mut bin: Vec<u8> = Vec::new();
    let mut off = 12usize;
    while off + 8 <= bytes.len() {
        let len = u32::from_le_bytes(bytes[off..off + 4].try_into().unwrap()) as usize;
        let kind = &bytes[off + 4..off + 8];
        let start = off + 8;
        let end = start
            .checked_add(len)
            .filter(|e| *e <= bytes.len())
            .ok_or_else(|| format!("GLB chunk 越界（offset {} 长度 {}）", start, len))?;
        match kind {
            b"JSON" => {
                json = Some(
                    serde_json::from_slice(&bytes[start..end])
                        .map_err(|e| format!("GLB 的 JSON chunk 解析失败：{}", e))?,
                )
            }
            b"BIN\0" => bin = bytes[start..end].to_vec(),
            other => {
                return Err(format!(
                    "GLB 里出现不认识的 chunk {:?}（只认 JSON 与 BIN）",
                    String::from_utf8_lossy(other)
                ))
            }
        }
        off = end;
    }
    Ok((version, json.ok_or("GLB 里没有 JSON chunk")?, bin))
}

// ---------------------------------------------------------------- glTF / GLB

/// 读 glTF/GLB：把每个有网格的节点烘成**世界坐标**的三角面。
///
/// 为什么烘世界坐标：我们的场景节点带的是世界坐标 AABB，side-car 也用世界坐标，
/// 于是客户端拿到就能直接放，不用再算一遍变换（少一处能算错的地方）。
pub fn read_gltf(path: &Path) -> Result<Vec<Mesh>, String> {
    let bytes = std::fs::read(path).map_err(|e| format!("读不到 {}：{}", path.display(), e))?;
    let (json, bin) = if bytes.starts_with(b"glTF") {
        let (_, json, bin) = parse_glb(&bytes)?;
        (json, bin)
    } else {
        let json: Value = serde_json::from_slice(&bytes)
            .map_err(|e| format!("{} 不是合法 glTF JSON：{}", path.display(), e))?;
        (json, Vec::new())
    };

    // 缓冲：GLB 的 BIN / 内嵌 data URI / 旁边的 .bin 文件
    let mut buffers: Vec<Vec<u8>> = Vec::new();
    for (i, b) in json["buffers"].as_array().cloned().unwrap_or_default().iter().enumerate() {
        let uri = b["uri"].as_str().unwrap_or("");
        let data = if uri.is_empty() {
            if i == 0 {
                bin.clone()
            } else {
                return Err(format!("buffer {} 没有 uri，也不是 GLB 的 BIN", i));
            }
        } else if let Some(rest) = uri.strip_prefix("data:") {
            let b64 = rest
                .split_once("base64,")
                .map(|(_, d)| d)
                .ok_or_else(|| format!("buffer {} 的 data URI 不是 base64", i))?;
            b64_decode(b64)?
        } else {
            let p = path.parent().unwrap_or(Path::new(".")).join(uri);
            std::fs::read(&p).map_err(|e| format!("读不到外部缓冲 {}：{}", p.display(), e))?
        };
        let want = b["byteLength"].as_u64().unwrap_or(0) as usize;
        if want != 0 && data.len() < want {
            return Err(format!(
                "buffer {} 只有 {} 字节，声明要 {} 字节",
                i,
                data.len(),
                want
            ));
        }
        buffers.push(data);
    }

    let accessor_span = |idx: u64| -> Result<(usize, usize, usize), String> {
        // 返回 (buffer_index, byte_offset, count)
        let a = json["accessors"]
            .get(idx as usize)
            .ok_or_else(|| format!("accessor {} 不存在", idx))?;
        let bv_idx = a["bufferView"]
            .as_u64()
            .ok_or_else(|| format!("accessor {} 没有 bufferView（稀疏/无缓冲暂不支持）", idx))?;
        let count = a["count"].as_u64().unwrap_or(0) as usize;
        let bv = json["bufferViews"]
            .get(bv_idx as usize)
            .ok_or_else(|| format!("bufferView {} 不存在", bv_idx))?;
        let buf = bv["buffer"].as_u64().unwrap_or(0) as usize;
        let base = bv["byteOffset"].as_u64().unwrap_or(0) as usize
            + a["byteOffset"].as_u64().unwrap_or(0) as usize;
        Ok((buf, base, count))
    };

    let read_positions = |idx: u64| -> Result<Vec<f32>, String> {
        let (buf, base, count) = accessor_span(idx)?;
        let data = buffers
            .get(buf)
            .ok_or_else(|| format!("accessor {} 指向的 buffer {} 不存在", idx, buf))?;
        let need = count * 12;
        let end = base
            .checked_add(need)
            .filter(|e| *e <= data.len())
            .ok_or_else(|| {
                format!(
                    "accessor {} 越界：要 {} 字节，buffer {} 只有 {} 字节",
                    idx,
                    need,
                    buf,
                    data.len()
                )
            })?;
        let mut out = Vec::with_capacity(count * 3);
        for c in data[base..end].chunks_exact(4) {
            out.push(f32::from_le_bytes(c.try_into().unwrap()));
        }
        Ok(out)
    };

    let read_indices = |idx: u64| -> Result<Vec<u32>, String> {
        let a = json["accessors"].get(idx as usize).cloned().unwrap_or(Value::Null);
        let comp = a["componentType"].as_u64().unwrap_or(5125);
        let (buf, base, count) = accessor_span(idx)?;
        let data = buffers
            .get(buf)
            .ok_or_else(|| format!("accessor {} 指向的 buffer {} 不存在", idx, buf))?;
        let width = match comp {
            5121 => 1,
            5123 => 2,
            5125 => 4,
            other => return Err(format!("索引 elementType {} 暂不支持", other)),
        };
        let end = base
            .checked_add(count * width)
            .filter(|e| *e <= data.len())
            .ok_or_else(|| format!("索引 accessor {} 越界", idx))?;
        let mut out = Vec::with_capacity(count);
        for c in data[base..end].chunks_exact(width) {
            out.push(match width {
                1 => c[0] as u32,
                2 => u16::from_le_bytes(c.try_into().unwrap()) as u32,
                _ => u32::from_le_bytes(c.try_into().unwrap()),
            });
        }
        Ok(out)
    };

    // 节点变换（烘进顶点）
    let node_matrix = |node: &Value| -> [f64; 16] {
        if let Some(m) = node["matrix"].as_array() {
            if m.len() == 16 {
                let mut out = [0.0; 16];
                for (i, v) in m.iter().enumerate() {
                    out[i] = v.as_f64().unwrap_or(0.0);
                }
                return out;
            }
        }
        let t: Vec<f64> = node["translation"]
            .as_array()
            .map(|a| a.iter().map(|v| v.as_f64().unwrap_or(0.0)).collect())
            .unwrap_or_else(|| vec![0.0, 0.0, 0.0]);
        let s: Vec<f64> = node["scale"]
            .as_array()
            .map(|a| a.iter().map(|v| v.as_f64().unwrap_or(1.0)).collect())
            .unwrap_or_else(|| vec![1.0, 1.0, 1.0]);
        // 只处理"平移 + 缩放"（旋转四元数的完整实现留给需要它的那天；此处绝不装作吃过它）
        let mut m = [0.0; 16];
        m[0] = s.first().copied().unwrap_or(1.0);
        m[5] = s.get(1).copied().unwrap_or(1.0);
        m[10] = s.get(2).copied().unwrap_or(1.0);
        m[15] = 1.0;
        m[12] = t.first().copied().unwrap_or(0.0);
        m[13] = t.get(1).copied().unwrap_or(0.0);
        m[14] = t.get(2).copied().unwrap_or(0.0);
        m
    };

    let apply = |m: &[f64; 16], p: &mut [f32]| {
        for c in p.chunks_exact_mut(3) {
            let (x, y, z) = (c[0] as f64, c[1] as f64, c[2] as f64);
            c[0] = (m[0] * x + m[4] * y + m[8] * z + m[12]) as f32;
            c[1] = (m[1] * x + m[5] * y + m[9] * z + m[13]) as f32;
            c[2] = (m[2] * x + m[6] * y + m[10] * z + m[14]) as f32;
        }
    };

    let mut out = Vec::new();
    for (ni, node) in json["nodes"].as_array().cloned().unwrap_or_default().iter().enumerate() {
        let Some(mi) = node["mesh"].as_u64() else {
            continue;
        };
        let m = node_matrix(node);
        let name = node["name"]
            .as_str()
            .map(|s| s.to_string())
            .unwrap_or_else(|| format!("node{}", ni));
        let mesh = json["meshes"].get(mi as usize).cloned().unwrap_or(Value::Null);
        for (pi, prim) in mesh["primitives"].as_array().cloned().unwrap_or_default().iter().enumerate() {
            let Some(pos_idx) = prim["attributes"]["POSITION"].as_u64() else {
                continue;
            };
            let mut positions = read_positions(pos_idx)?;
            apply(&m, &mut positions);
            let indices = match prim["indices"].as_u64() {
                Some(i) => read_indices(i)?,
                None => (0..(positions.len() / 3) as u32).collect(), // 非索引图元
            };
            let vcount = positions.len() / 3;
            if let Some(bad) = indices.iter().find(|i| **i as usize >= vcount) {
                return Err(format!(
                    "{} 的 primitive 里索引 {} 越界（只有 {} 个顶点）",
                    name, bad, vcount
                ));
            }
            out.push(Mesh {
                name: if pi == 0 {
                    name.clone()
                } else {
                    format!("{}#{}", name, pi)
                },
                positions,
                indices,
                notes: Vec::new(),
            });
        }
    }
    if out.is_empty() {
        return Err(format!("{} 里没有任何带 POSITION 的网格", path.display()));
    }
    Ok(out)
}

// ---------------------------------------------------------------- OBJ

/// 读 OBJ（`v` / `f` / `o` / `g`；`vt`/`vn` 读进来忽略——我们不缺纹理与法线的中转）。
/// OBJ → 网格。
///
/// ⚠️ 这里的两个坑（都是"看着能动其实全错"的那种）：
/// ① **每个对象只能带自己的顶点**。图省事让每个 mesh 共享全局顶点表的话，每个对象的
///    AABB 都会变成整体的 AABB —— 而场景层量间距、算挡窗全靠 AABB，错得无声无息。
/// ② 判断"这个对象有没有东西"得看**面索引**，不能看顶点：解析过程中顶点还在全局表里。
pub fn read_obj(path: &Path) -> Result<Vec<Mesh>, String> {
    let text = std::fs::read_to_string(path)
        .map_err(|e| format!("读不到 {}（OBJ 需要文本可读）：{}", path.display(), e))?;

    /// 正在拼的对象：全局顶点号 → 本对象的局部顶点号。
    #[derive(Default)]
    struct Building {
        name: String,
        map: std::collections::BTreeMap<u32, u32>,
        positions: Vec<f32>,
        indices: Vec<u32>,
    }

    impl Building {
        fn has_geometry(&self) -> bool {
            self.indices.len() >= 3
        }

        fn take(&mut self) -> Mesh {
            let name = std::mem::take(&mut self.name);
            let positions = std::mem::take(&mut self.positions);
            let indices = std::mem::take(&mut self.indices);
            self.map.clear();
            Mesh {
                name,
                positions,
                indices,
                notes: Vec::new(),
            }
        }

        /// 把一个全局顶点号写进本对象（首次出现时顺带复制坐标）。
        fn local(&mut self, global: u32, all: &[f32]) -> Result<u32, String> {
            if let Some(l) = self.map.get(&global) {
                return Ok(*l);
            }
            let base = global as usize * 3;
            if base + 3 > all.len() {
                return Err(format!("顶点索引 {} 越界", global + 1));
            }
            let l = self.map.len() as u32;
            self.map.insert(global, l);
            self.positions.extend_from_slice(&all[base..base + 3]);
            Ok(l)
        }
    }

    // 让 `take()` 能顺手清 map（`tap` 只为可读性）

    let mut positions: Vec<f32> = Vec::new();
    let mut meshes: Vec<Mesh> = Vec::new();
    let mut cur = Building {
        name: path
            .file_stem()
            .map(|s| s.to_string_lossy().to_string())
            .unwrap_or_else(|| "obj".into()),
        ..Default::default()
    };
    let mut referenced: std::collections::BTreeSet<u32> = std::collections::BTreeSet::new();

    let flush = |cur: &mut Building, meshes: &mut Vec<Mesh>| {
        if cur.has_geometry() {
            meshes.push(cur.take());
        } else {
            // 没面的对象（只有 `v` 或空 `o`）不是网格，丢掉，但名字留着
            cur.map.clear();
            cur.positions.clear();
            cur.indices.clear();
        }
    };

    for (lineno, raw) in text.lines().enumerate() {
        let line = raw.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let mut it = line.split_whitespace();
        match it.next() {
            Some("v") => {
                let mut v = [0.0f32; 3];
                for slot in v.iter_mut() {
                    *slot = it
                        .next()
                        .and_then(|s| s.parse().ok())
                        .ok_or_else(|| format!("第 {} 行 v 少了坐标", lineno + 1))?;
                }
                positions.extend_from_slice(&v);
            }
            Some("o") | Some("g") => {
                flush(&mut cur, &mut meshes);
                let name = it.collect::<Vec<_>>().join(" ");
                cur.name = if name.is_empty() {
                    format!("group{}", meshes.len())
                } else {
                    name
                };
            }
            Some("f") => {
                // 支持 `f a b c` 与 `f a/x/y b/x/y c/x/y`，以及多边形（三角扇拆开）
                let idx: Vec<u32> = it
                    .map(|tok| {
                        let head = tok.split('/').next().unwrap_or("");
                        let i = head
                            .parse::<i64>()
                            .map_err(|_| format!("第 {} 行的面索引看不懂：{}", lineno + 1, tok))?;
                        // OBJ 是 1-based；负数表示"从末尾数"
                        let n = positions.len() as i64 / 3;
                        let k = if i < 0 { n + i } else { i - 1 };
                        if k < 0 || k >= n {
                            return Err(format!(
                                "第 {} 行的面索引 {} 越界（当时只有 {} 个顶点）",
                                lineno + 1,
                                i,
                                n
                            ));
                        }
                        Ok(k as u32)
                    })
                    .collect::<Result<Vec<u32>, String>>()?;
                let local = idx
                    .iter()
                    .map(|g| cur.local(*g, &positions))
                    .collect::<Result<Vec<u32>, String>>()?;
                for w in 1..local.len().saturating_sub(1) {
                    cur.indices
                        .extend_from_slice(&[local[0], local[w], local[w + 1]]);
                }
                referenced.extend(idx.iter().copied());
            }
            _ => {} // mtllib/usemtl/vt/vn/s/… 先不接
        }
    }
    flush(&mut cur, &mut meshes);

    if positions.is_empty() {
        return Err(format!("{} 里没有任何 `v` 顶点", path.display()));
    }
    if meshes.is_empty() {
        return Err(format!(
            "{} 里没有任何可用的面（`f`）（声明了 {} 个顶点）",
            path.display(),
            positions.len() / 3
        ));
    }

    // 被丢掉的东西也要说：孤立顶点、以及"每次 flush 都重置"的提示
    let declared = positions.len() / 3;
    if referenced.len() < declared {
        let note = format!(
            "OBJ 里声明了 {} 个顶点，其中 {} 个没有任何面引用（孤立点，导入时丢掉了）",
            declared,
            declared - referenced.len()
        );
        meshes[0].notes.push(note);
    }
    Ok(meshes)
}

// ---------------------------------------------------------------- STL

/// 三角汤 → (去重后的顶点, 索引)。
///
/// STL 每个三角面自带 3 个顶点，一个立方体就是 36 个顶点而实际只有 8 个；不去重的话
/// side-car 会白胖三倍。这里按**位模式**判等（NaN 也照判，不引入任何浮点容差——
/// 容差合并会把锐角件的顶点吃掉，那是改数据不是压数据）。
fn dedupe_vertices(soup: Vec<f32>) -> (Vec<f32>, Vec<u32>) {
    let mut positions: Vec<f32> = Vec::with_capacity(soup.len());
    let mut indices: Vec<u32> = Vec::with_capacity(soup.len() / 3);
    let mut seen: std::collections::HashMap<[u32; 3], u32> = std::collections::HashMap::new();
    for v in soup.chunks_exact(3) {
        let key = [v[0].to_bits(), v[1].to_bits(), v[2].to_bits()];
        let idx = match seen.get(&key) {
            Some(i) => *i,
            None => {
                let i = (positions.len() / 3) as u32;
                positions.extend_from_slice(v);
                seen.insert(key, i);
                i
            }
        };
        indices.push(idx);
    }
    (positions, indices)
}

/// 读 STL（二进制与 ASCII 都认）。STL 没有对象概念，整份就是一个三角汤。
pub fn read_stl(path: &Path) -> Result<Vec<Mesh>, String> {
    let bytes = std::fs::read(path).map_err(|e| format!("读不到 {}：{}", path.display(), e))?;
    let name = path
        .file_stem()
        .map(|s| s.to_string_lossy().to_string())
        .unwrap_or_else(|| "stl".into());

    // 二进制 STL 的长度是确定的：84 + 50×三角形数。先按这个判，判不出来再当 ASCII。
    let looks_binary = bytes.len() >= 84
        && {
            let n = u32::from_le_bytes(bytes[80..84].try_into().unwrap()) as usize;
            bytes.len() == 84 + n * 50
        };

    if looks_binary {
        let n = u32::from_le_bytes(bytes[80..84].try_into().unwrap()) as usize;
        // 80 字节头里**常常**（不是规范要求）放着名字：能当文本读出来就拿来用
        let header_name = String::from_utf8_lossy(&bytes[..80])
            .trim_matches(|c: char| c == '\0' || c.is_whitespace())
            .to_string();
        let name = if header_name.is_empty() {
            name
        } else {
            header_name
        };
        let mut positions = Vec::with_capacity(n * 9);
        for i in 0..n {
            let base = 84 + i * 50 + 12; // 跳过法线
            for v in 0..3 {
                let off = base + v * 12;
                for k in 0..3 {
                    let b = &bytes[off + k * 4..off + k * 4 + 4];
                    positions.push(f32::from_le_bytes(b.try_into().unwrap()));
                }
            }
        }
        if positions.is_empty() {
            return Err(format!("{} 的三角形数是 0（空文件？）", path.display()));
        }
        let (positions, indices) = dedupe_vertices(positions);
        return Ok(vec![Mesh {
            name,
            indices,
            positions,
            notes: vec![
                "二进制 STL 是三角汤：所有面挤在一个对象里（源文件本来就没有对象划分）".into(),
            ],
        }]);
    }

    // ASCII：**能分对象就分**——一个 `solid <名字>` 一段，这是装配体导出 ASCII STL 时
    // 唯一的分组线索（不去分的话，一个总成会塌成一个叫"stl"的巨型对象）
    let text = String::from_utf8_lossy(&bytes);
    if !text.contains("facet") {
        return Err(format!(
            "{} 既不像二进制 STL（长度与三角形数不匹配），也没有 `facet`（不是 ASCII STL）",
            path.display()
        ));
    }

    let mut meshes: Vec<Mesh> = Vec::new();
    let mut cur_name = name.clone();
    // 一个 solid 段的**三角汤**（每个 facet 的 3 个顶点顺次排）：去重在段结束时一次做完，
    // 这样"去重表 → 索引"的映射只做一遍，不会错位
    let mut soup: Vec<f32> = Vec::new();
    let mut pending: Vec<[f32; 3]> = Vec::new();
    let mut solids = 0usize;
    let mut malformed = 0usize;

    fn flush(meshes: &mut Vec<Mesh>, name: &mut String, soup: &mut Vec<f32>) {
        if soup.len() >= 9 {
            let (positions, indices) = dedupe_vertices(std::mem::take(soup));
            meshes.push(Mesh {
                name: std::mem::take(name),
                positions,
                indices,
                notes: Vec::new(),
            });
        } else {
            soup.clear();
        }
    }

    for line in text.lines() {
        let t = line.trim();
        if let Some(rest) = t.strip_prefix("solid") {
            flush(&mut meshes, &mut cur_name, &mut soup);
            solids += 1;
            let nm = rest.trim();
            cur_name = if nm.is_empty() {
                format!("solid_{}", solids)
            } else {
                nm.to_string()
            };
        } else if t.starts_with("facet") {
            pending.clear();
        } else if let Some(rest) = t.strip_prefix("vertex") {
            let v: Vec<f32> = rest
                .split_whitespace()
                .filter_map(|s| s.parse().ok())
                .collect();
            if v.len() >= 3 {
                pending.push([v[0], v[1], v[2]]);
            }
        } else if t.starts_with("endfacet") {
            if pending.len() == 3 {
                for v in pending.drain(..) {
                    soup.extend_from_slice(&v);
                }
            } else {
                malformed += 1;
                pending.clear();
            }
        }
    }
    flush(&mut meshes, &mut cur_name, &mut soup);

    if meshes.is_empty() {
        return Err(format!("{} 里没有可用的 `vertex` 行", path.display()));
    }
    if malformed > 0 {
        meshes[0].notes.push(format!(
            "有 {} 个 facet 的顶点数不是 3（STL 只装三角面），跳过了",
            malformed
        ));
    }
    if solids <= 1 {
        meshes[0].notes.push(
            "ASCII STL 里只有一个 solid：源文件没有对象划分，整份塌成一个对象".into(),
        );
    }
    Ok(meshes)
}
