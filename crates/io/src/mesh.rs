//! 网格与 side-car：把导入进来的三角面装进**不占场景文档**的地方。
//!
//! # 为什么几何不放进场景文档
//!
//! 场景文档（`Scene`）是我们的**契约对象**：它要进日志、进哈希、进 diff、进增量。
//! 一个机器人总成有上百万三角面——把它塞进 `Scene` 会让"每一次移动一个物体"都要
//! 搬动几十 MB 的几何，日志/哈希/增量全部跟着变重。
//!
//! 所以这里做一个**内容寻址的 side-car**：
//!
//! - 几何写到 `<文档名>.mesh.glb`（标准 glTF 2.0 二进制，**三个立方体**：three.js /
//!   Blender 都能直接读），文件名带 sha256 前缀 ⇒ 内容变了名字就变，不可能张冠李戴；
//! - 场景里的节点只写一句 `extras.rsi3d.mesh_ref = "<文件名>"` —— **裸的 extras**，
//!   内核（`Scene`）不需要为它改一个字段，也就不会污染核心不变量；
//! - side-car **不参与版本与哈希**：它是"视图"而不是"状态"。工具链说得很清楚：
//!   证据来自服务端光栅（挡光带、包围盒那套），网格只是给人看的东西。
//!
//! GLB 里的节点名**就是我们的节点 id**——于是客户端只要把 glb 读一遍、按名字建索引，
//! 就能把每个节点换成真网格，不需要再传一份 id 映射表。

use sha2::{Digest, Sha256};

/// 一个三角网格（世界坐标；索引是 `positions` 的下标）。
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Mesh {
    pub name: String,
    /// xyz 三元组展开（`[x,y,z, x,y,z, …]`）
    pub positions: Vec<f32>,
    pub indices: Vec<u32>,
    /// 读取过程中发现的、**必须告诉用户**的事实（被丢弃的孤立顶点、源格式没有单位…）。
    /// 放在这里而不是直接丢掉：导入器的沉默就是骗人。
    pub notes: Vec<String>,
}

impl Mesh {
    pub fn vertex_count(&self) -> usize {
        self.positions.len() / 3
    }

    pub fn triangle_count(&self) -> usize {
        self.indices.len() / 3
    }

    pub fn is_empty(&self) -> bool {
        self.vertex_count() == 0 || self.indices.len() < 3
    }

    /// 包围盒 `(min, max)`。空网格返回 `None`。
    pub fn aabb(&self) -> Option<([f64; 3], [f64; 3])> {
        if self.positions.is_empty() {
            return None;
        }
        let mut min = [f64::INFINITY; 3];
        let mut max = [f64::NEG_INFINITY; 3];
        for p in self.positions.chunks_exact(3) {
            for k in 0..3 {
                let v = p[k] as f64;
                if v < min[k] {
                    min[k] = v;
                }
                if v > max[k] {
                    max[k] = v;
                }
            }
        }
        Some((min, max))
    }

    /// 内容哈希：几何相同就是同一个 mesh（去重、也当 side-car 的文件名用）。
    pub fn content_hash(&self) -> String {
        let mut h = Sha256::new();
        h.update(&(self.positions.len() as u64).to_le_bytes());
        h.update(&(self.indices.len() as u64).to_le_bytes());
        for v in &self.positions {
            // 用**位模式**而不是数值：几何哈希必须是确定性的，不能受浮点格式影响
            h.update(v.to_bits().to_le_bytes());
        }
        for i in &self.indices {
            h.update(i.to_le_bytes());
        }
        hex_lower(&h.finalize())
    }
}

pub(crate) fn hex_lower(bytes: &[u8]) -> String {
    let mut s = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        s.push_str(&format!("{:02x}", b));
    }
    s
}

/// 把一个场景的网格写成 **GLB**（glTF 2.0 二进制单文件）。
///
/// 形状与 `crates/stream/src/gltf.rs` 的导出一致（场景引用、accessor 的 `min`/`max`、
/// 4 字节对齐），区别是这里放**真三角面**而不是包围盒代理。同一个 `Mesh` 只写一份
/// （按内容哈希去重），多个节点可以引用它。
pub fn write_glb(nodes: &[(String, Mesh)]) -> Vec<u8> {
    // 去重：内容相同的几何只写一份
    let mut pool: Vec<(&Mesh, String)> = Vec::new();
    let mut seen: std::collections::BTreeMap<String, usize> = Default::default();
    for (_, m) in nodes {
        if m.is_empty() {
            continue;
        }
        let h = m.content_hash();
        if !seen.contains_key(&h) {
            seen.insert(h.clone(), pool.len());
            pool.push((m, h));
        }
    }

    // ---- 二进制缓冲：位置（f32×3）与索引（u32），各自 4 字节对齐
    let mut bin: Vec<u8> = Vec::new();
    let mut buffer_views: Vec<serde_json::Value> = Vec::new();
    let mut accessors: Vec<serde_json::Value> = Vec::new();
    let mut gltf_meshes: Vec<serde_json::Value> = Vec::new();

    for (m, hash) in &pool {
        // 位置
        while bin.len() % 4 != 0 {
            bin.push(0);
        }
        let pos_off = bin.len();
        for v in &m.positions {
            bin.extend_from_slice(&v.to_le_bytes());
        }
        let pos_len = bin.len() - pos_off;
        // 索引
        while bin.len() % 4 != 0 {
            bin.push(0);
        }
        let idx_off = bin.len();
        for i in &m.indices {
            bin.extend_from_slice(&i.to_le_bytes());
        }
        let idx_len = bin.len() - idx_off;

        let (min, max) = m.aabb().expect("非空网格一定有包围盒");
        buffer_views.push(serde_json::json!({
            "buffer": 0, "byteOffset": pos_off, "byteLength": pos_len, "target": 34962
        }));
        buffer_views.push(serde_json::json!({
            "buffer": 0, "byteOffset": idx_off, "byteLength": idx_len, "target": 34963
        }));
        accessors.push(serde_json::json!({
            "bufferView": buffer_views.len() - 2,
            "componentType": 5126, // FLOAT
            "count": m.vertex_count(),
            "type": "VEC3",
            "min": min, "max": max,
        }));
        accessors.push(serde_json::json!({
            "bufferView": buffer_views.len() - 1,
            "componentType": 5125, // UNSIGNED_INT
            "count": m.indices.len(),
            "type": "SCALAR",
        }));
        gltf_meshes.push(serde_json::json!({
            "name": hash,
            "primitives": [{
                "attributes": { "POSITION": accessors.len() - 2 },
                "indices": accessors.len() - 1,
                "mode": 4 // TRIANGLES
            }]
        }));
    }

    // ---- 节点：**名字就是我们的节点 id**（客户端按名字建索引，不用另传映射表）
    let mut gltf_nodes: Vec<serde_json::Value> = Vec::new();
    for (id, m) in nodes {
        if m.is_empty() {
            continue;
        }
        let mesh_index = seen
            .get(&m.content_hash())
            .copied()
            .expect("刚刚才把非空网格放进池子");
        gltf_nodes.push(serde_json::json!({
            "name": id,
            "mesh": mesh_index,
        }));
    }

    let roots: Vec<usize> = (0..gltf_nodes.len()).collect();
    let gltf = serde_json::json!({
        "asset": { "version": "2.0", "generator": "rsi3d-harness io" },
        "scene": 0,
        "scenes": [{ "nodes": roots }],
        "nodes": gltf_nodes,
        "meshes": gltf_meshes,
        "accessors": accessors,
        "bufferViews": buffer_views,
        "buffers": [{ "byteLength": bin.len() }],
    });

    // ---- 组装 GLB：header + JSON chunk（空格补齐）+ BIN chunk（零补齐）
    let mut json = serde_json::to_vec(&gltf).expect("glTF JSON 一定能序列化");
    while json.len() % 4 != 0 {
        json.push(b' ');
    }
    while bin.len() % 4 != 0 {
        bin.push(0);
    }
    let total = 12 + 8 + json.len() + 8 + bin.len();
    let mut out = Vec::with_capacity(total);
    out.extend_from_slice(b"glTF");
    out.extend_from_slice(&2u32.to_le_bytes());
    out.extend_from_slice(&(total as u32).to_le_bytes());
    out.extend_from_slice(&(json.len() as u32).to_le_bytes());
    out.extend_from_slice(b"JSON");
    out.extend_from_slice(&json);
    out.extend_from_slice(&(bin.len() as u32).to_le_bytes());
    out.extend_from_slice(b"BIN\0");
    out.extend_from_slice(&bin);
    out
}

/// side-car 文件名：内容寻址，内容变了名字就变，不可能张冠李戴。
pub fn sidecar_name(glb: &[u8]) -> String {
    let mut h = Sha256::new();
    h.update(glb);
    format!("{}.mesh.glb", &hex_lower(&h.finalize())[..16])
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cube(size: f32) -> Mesh {
        let s = size;
        let mut positions = Vec::new();
        for &z in &[-s, s] {
            for &y in &[-s, s] {
                for &x in &[-s, s] {
                    positions.extend_from_slice(&[x, y, z]);
                }
            }
        }
        Mesh {
            name: "cube".into(),
            positions,
            indices: vec![0, 1, 3, 0, 3, 2, 4, 6, 7, 4, 7, 5],
            notes: Vec::new(),
        }
    }

    #[test]
    fn aabb_covers_all_vertices() {
        let (min, max) = cube(2.0).aabb().unwrap();
        assert_eq!(min, [-2.0, -2.0, -2.0]);
        assert_eq!(max, [2.0, 2.0, 2.0]);
    }

    #[test]
    fn identical_geometry_hashes_the_same_and_dedupes() {
        assert_eq!(cube(1.0).content_hash(), cube(1.0).content_hash());
        assert_ne!(cube(1.0).content_hash(), cube(2.0).content_hash());

        let glb = write_glb(&[
            ("a".into(), cube(1.0)),
            ("b".into(), cube(1.0)), // 与 a 同几何
            ("c".into(), cube(2.0)),
        ]);
        // 读回来只应有 2 个 mesh、3 个节点
        let (_, json, _) = crate::readers::parse_glb(&glb).unwrap();
        assert_eq!(json["meshes"].as_array().unwrap().len(), 2);
        assert_eq!(json["nodes"].as_array().unwrap().len(), 3);
        // 节点名 = 我们的 id（客户端据此建索引）
        assert_eq!(json["nodes"][0]["name"], "a");
        assert_eq!(json["scenes"][0]["nodes"].as_array().unwrap().len(), 3);
    }

    #[test]
    fn glb_is_wellformed_and_padded() {
        let glb = write_glb(&[("a".into(), cube(1.0))]);
        assert_eq!(&glb[0..4], b"glTF");
        assert_eq!(u32::from_le_bytes(glb[4..8].try_into().unwrap()), 2);
        assert_eq!(
            u32::from_le_bytes(glb[8..12].try_into().unwrap()) as usize,
            glb.len(),
            "GLB 头里写的总长必须与实际一致"
        );
        let json_len = u32::from_le_bytes(glb[12..16].try_into().unwrap()) as usize;
        assert_eq!(json_len % 4, 0, "JSON chunk 必须 4 字节对齐");
        assert_eq!(&glb[16..20], b"JSON");
        let bin_off = 20 + json_len;
        let bin_len = u32::from_le_bytes(glb[bin_off..bin_off + 4].try_into().unwrap()) as usize;
        assert_eq!(bin_len % 4, 0, "BIN chunk 必须 4 字节对齐");
        assert_eq!(&glb[bin_off + 4..bin_off + 8], b"BIN\0");
        assert_eq!(bin_off + 8 + bin_len, glb.len());
    }
}
