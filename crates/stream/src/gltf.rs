//! USC → **glTF 2.0**。这是「场景流」的载荷格式。
//!
//! # 为什么不自造一套场景 JSON
//!
//! 因为 `core` 的场景表示当初就是**照着 glTF 2.0 同构**设计的
//! （node ↔ glTF node、层 ↔ `mesh.primitive`、我们自己的字段进 `extras`，见 `docs/core.md` §3）。
//! 所以出图/出流时应该直接给 glTF：three.js 的 `GLTFLoader.parse` 能原样吃下，
//! 不需要为「rsi3d 的场景」再写一个加载器。
//!
//! # 诚实标注几何档次
//!
//! H0 还没有真实几何（`io` 未开工），画的/推的是**包围盒代理**。
//! 所以每个节点都标 `extras.rsi3d.geometry = "aabb-proxy"`，
//! `welcome` 里也带同一个字段——**别让客户端误以为收到的是真网格**。
//! 接上 `io` 之后这里换成真的 `meshes`/`accessors`，而节点结构与 extras 不变。

use serde_json::{json, Map, Value};

use rsi3d_harness_core::{Node, Scene};
use rsi3d_harness_render::shading::base_color;

/// 当前几何档次。
pub const GEOMETRY_AABB_PROXY: &str = "aabb-proxy";
pub const GEOMETRY_MESH: &str = "mesh";

/// 一个单位立方体的三角形（12 个三角形 = 36 个顶点索引，逐面法线）。
///
/// glTF 的 `POSITION` 只存位置；法线交给客户端用 `flatShading` 或自己算法线都行，
/// 但为了让 three.js 的默认材质就能看，我们连法线一起给。
fn unit_box_primitive() -> (Vec<f64>, Vec<f64>, Vec<u32>) {
    // 八个角（-0.5..0.5），六个面各两个三角形
    let corners = [
        [-0.5, -0.5, -0.5],
        [0.5, -0.5, -0.5],
        [0.5, -0.5, 0.5],
        [-0.5, -0.5, 0.5],
        [-0.5, 0.5, -0.5],
        [0.5, 0.5, -0.5],
        [0.5, 0.5, 0.5],
        [-0.5, 0.5, 0.5],
    ];
    let faces: [([usize; 4], [f64; 3]); 6] = [
        ([0, 3, 2, 1], [0.0, -1.0, 0.0]),
        ([4, 5, 6, 7], [0.0, 1.0, 0.0]),
        ([0, 1, 5, 4], [0.0, 0.0, -1.0]),
        ([3, 7, 6, 2], [0.0, 0.0, 1.0]),
        ([0, 4, 7, 3], [-1.0, 0.0, 0.0]),
        ([1, 2, 6, 5], [1.0, 0.0, 0.0]),
    ];

    let mut positions = Vec::with_capacity(36 * 3);
    let mut normals = Vec::with_capacity(36 * 3);
    let mut indices = Vec::with_capacity(36);
    for (quad, normal) in faces {
        let tris = [[quad[0], quad[1], quad[2]], [quad[0], quad[2], quad[3]]];
        for tri in tris {
            let base = (positions.len() / 3) as u32;
            for vi in tri {
                positions.extend_from_slice(&corners[vi]);
                normals.extend_from_slice(&normal);
            }
            indices.extend_from_slice(&[base, base + 1, base + 2]);
        }
    }
    (positions, normals, indices)
}

/// 单个节点 → glTF node。
///
/// **快照与增量共用这一个函数**——两处形状必须完全一致，否则客户端得写两套应用逻辑，
/// 而"少支持一个字段"这种错是静默的（画面看着对，数据已经错了）。
///
/// `material_index = None` 时不出 `material` 键：增量载荷不带材质表，
/// 客户端要改颜色就看 `extras.rsi3d.color`（**两种情况下都带**）。
pub fn node_to_gltf(node: &Node, material_index: Option<usize>) -> Value {
    let size = node.aabb.size();
    let center = node.aabb.center();
    let rgb = base_color(node);
    let mut n = serde_json::Map::new();
    n.insert("name".into(), json!(node.id));
    n.insert("mesh".into(), json!(0));
    if let Some(mi) = material_index {
        n.insert("material".into(), json!(mi));
    }
    n.insert("translation".into(), json!(center));
    n.insert("scale".into(), json!(size));
    let mut extras = json!({
        "id": node.id,
        "role": node.role,
        "material_name": node.material,
        "aabb": node.aabb.to_array(),
        "editability": node.editability().as_str(),
        "layers": node.layers.iter().map(|l| format!("{:?}", l).to_lowercase()).collect::<Vec<_>>(),
        "geometry": GEOMETRY_AABB_PROXY,
        // 展平后的基色：让增量载荷自包含（不必依赖快照里的材质表）
        "color": [rgb[0], rgb[1], rgb[2]],
    });
    // 导入来的资产：几何真身在 side-car 里，这里只说文件名。
    //
    // **服务端光栅依旧画包围盒代理**（证据档次没变），这只是给客户端一条"去哪拿真网格"的
    // 线索——拿不到就还是盒子，不会变成"白屏"。
    if let Some(mesh_ref) = node
        .extras
        .get("rsi3d")
        .and_then(|v| v.get("mesh_ref"))
        .and_then(|v| v.as_str())
    {
        extras["mesh_ref"] = json!(mesh_ref);
    }
    n.insert(
        "extras".into(),
        json!({ "rsi3d": extras }),
    );
    Value::Object(n)
}

/// 单个灯光 → `KHR_lights_punctual` 的 light 对象。
pub fn light_to_gltf(l: &rsi3d_harness_core::Light) -> Value {
    let dir = l.direction.map(|d| {
        let v = rsi3d_harness_render::math::Vec3::from_arr(d).normalized();
        [v.x, v.y, v.z]
    });
    json!({
        "name": l.id,
        "type": if l.kind.eq_ignore_ascii_case("directional") { "directional" } else { "point" },
        "intensity": l.intensity,
        "color": [1.0, 0.95, 0.87],
        "extras": {"rsi3d": {"id": l.id, "kind": l.kind, "direction": dir}},
    })
}

/// 从内核的 `Diff` 里分出**哪些是节点、哪些是灯光**。
///
/// 判据是字段名（`intensity`/`color` 属灯光，其余属节点）——因为
/// **只有一份 diff 实现**（内核的），这里不另起一套，只做归类。
pub fn diff_ids(d: &rsi3d_harness_core::Diff) -> (Vec<String>, Vec<String>) {
    use std::collections::BTreeSet;
    let mut nodes: BTreeSet<String> = d.added.iter().cloned().collect();
    nodes.extend(d.removed.iter().cloned());
    let mut lights: BTreeSet<String> = BTreeSet::new();
    for ch in &d.changed {
        match ch.what.as_str() {
            "intensity" | "color" => {
                lights.insert(ch.id.clone());
            }
            _ => {
                nodes.insert(ch.id.clone());
            }
        }
    }
    (nodes.into_iter().collect(), lights.into_iter().collect())
}

/// 场景 → glTF 2.0 JSON。
///
/// 约定：
/// - 所有节点共享**一个**单位立方体 mesh，靠 `scale` = 包围盒尺寸、`translation` = 中心
///   来表达代理几何（`io` 落地后换成每节点自己的 mesh）。
/// - 颜色走 `materials[].pbrMetallicRoughness.baseColorFactor`（与 `render` 的配色同源，
///   所以浏览器里看到的颜色和 `scene render` 出的图一致），同时在 `extras.rsi3d.color` 里展平一份。
/// - 房间/窗/挡光带/规则/意图都在 `extras.rsi3d` 里给全，客户端愿意画就画。
pub fn scene_to_gltf(scene: &Scene) -> Value {
    let (positions, normals, indices) = unit_box_primitive();

    // ---- 材质：按 (role, material) 去重，保证同一个材质只有一个 material 索引
    let mut materials: Vec<Value> = Vec::new();
    let mut material_index: Map<String, Value> = Map::new();
    let mut nodes: Vec<Value> = Vec::new();

    for node in &scene.objects {
        let key = format!("{}|{}", node.role, node.material.clone().unwrap_or_default());
        let mi = match material_index.get(&key) {
            Some(v) => v.as_u64().unwrap_or(0) as usize,
            None => {
                let rgb = base_color(node);
                let idx = materials.len();
                materials.push(json!({
                    "name": key,
                    "pbrMetallicRoughness": {
                        "baseColorFactor": [
                            rgb[0] as f64 / 255.0,
                            rgb[1] as f64 / 255.0,
                            rgb[2] as f64 / 255.0,
                            1.0
                        ],
                        "metallicFactor": node.material_params.metallic.unwrap_or(0.0),
                        "roughnessFactor": node.material_params.roughness.unwrap_or(0.85),
                    },
                }));
                material_index.insert(key, json!(idx));
                idx
            }
        };
        nodes.push(node_to_gltf(node, Some(mi)));
    }

    let lights: Vec<Value> = scene.lights.iter().map(light_to_gltf).collect();

    // **场景必须引用根节点**：glTF 里 `nodes` 只是"节点池"，`scenes[].nodes` 才是
    // "这个场景由哪些节点组成"。少了这一步，任何符合规范的加载器（three.js 的
    // GLTFLoader、Blender、Fyrox…）读到的都是**空场景**——文件合法，内容为零。
    // 我们所有节点都是根节点（H0 没有层级），所以就是 0..n。
    let root_nodes: Vec<usize> = (0..nodes.len()).collect();

    let mut root = Map::new();
    root.insert("asset".into(), json!({
        "version": "2.0",
        "generator": format!("rsi3d-harness/{}", env!("CARGO_PKG_VERSION")),
    }));
    root.insert("scene".into(), json!(0));
    root.insert("scenes".into(), json!([{ "name": "rsi3d", "nodes": root_nodes }]));
    root.insert("nodes".into(), Value::Array(nodes));
    root.insert("materials".into(), Value::Array(materials));

    // ---- 代理几何：一个单位立方体
    let pos_max = positions.iter().cloned().fold(f64::NEG_INFINITY, f64::max);
    let norm_max = normals.iter().cloned().fold(f64::NEG_INFINITY, f64::max);
    let idx_max = indices.iter().cloned().max().unwrap_or(0);
    root.insert("meshes".into(), json!([{
        "name": "rsi3d-unit-box",
        "primitives": [{ "attributes": {"POSITION": 0, "NORMAL": 1}, "indices": 2, "material": 0 }]
    }]));
    root.insert("accessors".into(), json!([
        {
            "bufferView": 0, "componentType": 5126, "count": positions.len() / 3, "type": "VEC3",
            "min": [-0.5, -0.5, -0.5], "max": [0.5, 0.5, 0.5],
            "extras": {"check": pos_max}
        },
        {
            "bufferView": 1, "componentType": 5126, "count": normals.len() / 3, "type": "VEC3",
            "extras": {"check": norm_max}
        },
        {
            "bufferView": 2, "componentType": 5125, "count": indices.len(), "type": "SCALAR",
            "extras": {"check": idx_max}
        }
    ]));
    root.insert("bufferViews".into(), json!([
        {"buffer": 0, "byteOffset": 0, "byteLength": positions.len() * 4, "target": 34962},
        {"buffer": 0, "byteOffset": positions.len() * 4, "byteLength": normals.len() * 4, "target": 34962},
        {"buffer": 0, "byteOffset": (positions.len() + normals.len()) * 4, "byteLength": indices.len() * 4, "target": 34963}
    ]));

    // ---- 场景级 extras：房间 / 窗 / 挡光带 / 规则 / 意图 + **完整数据**（供客户端真的画出来）
    let (pos_bytes, idx_bytes) = (positions.len() * 4, indices.len() * 4);
    let total = pos_bytes + normals.len() * 4 + idx_bytes;
    // 位置与法线用 f32 小端，索引用 u32 小端
    let mut blob: Vec<u8> = Vec::with_capacity(total);
    for v in &positions {
        blob.extend_from_slice(&(*v as f32).to_le_bytes());
    }
    for v in &normals {
        blob.extend_from_slice(&(*v as f32).to_le_bytes());
    }
    for i in &indices {
        blob.extend_from_slice(&i.to_le_bytes());
    }
    root.insert("buffers".into(), json!([{
        "byteLength": blob.len(),
        "uri": format!("data:application/octet-stream;base64,{}", rsi3d_harness_render::png::base64(&blob)),
    }]));
    root.insert("extensionsUsed".into(), json!(["KHR_lights_punctual"]));
    root.insert("extensions".into(), json!({ "KHR_lights_punctual": { "lights": lights } }));
    root.insert("extras".into(), json!({
        "rsi3d": {
            "spec": rsi3d_harness_core::SCENE_SPEC,
            "geometry": GEOMETRY_AABB_PROXY,
            "units": scene.units,
            "up_axis": scene.up_axis,
            "intent": scene.intent,
            "room": scene.room.as_ref().map(|r| json!({"size": r.size, "ceiling": r.ceiling})),
            "window": scene.window.as_ref().map(|w| json!({
                "wall": w.wall, "spanX": w.span_x, "height": w.height, "z": w.z, "bandDepth": w.band_depth
            })),
            "clearance_rules": scene.clearance_rules,
            "intent_keywords": scene.intent_keywords,
            "window_blockers": scene.window_blockers(),
        }
    }));

    Value::Object(root)
}

/// 把内核的 `Diff` 变成客户端能直接用的增量（**不另造 delta 格式**）。
pub fn diff_to_payload(d: &rsi3d_harness_core::Diff) -> Value {
    json!({
        "added": d.added,
        "removed": d.removed,
        "changed": d.changed,
        // 顺序与范围都给全，客户端可以据此判断是否需要重新拉全量
        "empty": d.is_empty(),
    })
}

/// 节点级摘要（给日志与测试断言用；不含几何）。
pub fn node_summary(node: &Node) -> Value {
    json!({
        "id": node.id,
        "role": node.role,
        "aabb": node.aabb.to_array(),
        "editability": node.editability().as_str(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scene() -> Scene {
        Scene::from_json(
            r##"{
              "room": { "size": [4.2, 2.8, 6.0] },
              "window": { "wall": "north", "spanX": [-1.2, 1.2], "height": 1.6, "z": -2.9, "bandDepth": 1.5 },
              "objects": [
                { "id": "obj:sofa_01", "role": "furniture", "material": "fabric",
                  "aabb": { "min": [-1.1, 0.0, -2.6], "max": [0.9, 0.85, -1.7] } },
                { "id": "obj:table_01", "role": "furniture", "material": "oak",
                  "aabb": { "min": [-0.5, 0.0, 2.2], "max": [0.5, 0.45, 2.9] } }
              ],
              "lights": [ { "id": "sun", "kind": "directional", "intensity": 2.0, "direction": [0.3,-1.0,0.4] } ],
              "clearance_rules": [ { "pair": ["obj:sofa_01","obj:table_01"], "min": 0.4, "max": 0.8, "reason": "茶几" } ]
            }"##,
        )
        .unwrap()
    }

    #[test]
    fn gltf_has_the_required_shape() {
        let g = scene_to_gltf(&scene());
        assert_eq!(g["asset"]["version"], "2.0", "必须是 glTF 2.0");
        assert!(g["nodes"].as_array().unwrap().len() == 2);
        assert!(g["meshes"][0]["primitives"][0]["attributes"]["POSITION"].is_number());
        assert!(g["accessors"].as_array().unwrap().len() == 3);
        assert!(g["bufferViews"].as_array().unwrap().len() == 3);
        assert!(g["buffers"][0]["uri"].as_str().unwrap().starts_with("data:application/octet-stream;base64,"));
        // 灯光走 Khronos 扩展（不自造）
        assert!(g["extensionsUsed"].as_array().unwrap().contains(&json!("KHR_lights_punctual")));
        assert_eq!(g["extensions"]["KHR_lights_punctual"]["lights"][0]["name"], "sun");
    }

    /// **场景必须真的把节点挂上**。
    ///
    /// 这个用例是被一次外部验收逼出来的：`nodes` 是"节点池"，`scenes[].nodes` 才是
    /// "这个场景由哪些节点组成"。只填节点池的话文件**完全合法**，但任何标准加载器
    /// （three.js GLTFLoader / Blender / Fyrox）读到的都是**空场景**——
    /// 而只断言 `nodes` 的测试是看不出来的（`gltf-transform inspect` 会报
    /// `renderVertexCount: 0` + 包围盒 `Infinity`）。
    #[test]
    fn the_scene_actually_references_every_node() {
        let g = scene_to_gltf(&scene());
        let pool = g["nodes"].as_array().unwrap().len();
        let roots: Vec<u64> = g["scenes"][0]["nodes"]
            .as_array()
            .expect("scenes[0].nodes 必须是数组")
            .iter()
            .map(|v| v.as_u64().unwrap())
            .collect();
        assert_eq!(roots.len(), pool, "每个节点都该是场景的根（H0 没有层级）");
        assert_eq!(roots, (0..pool as u64).collect::<Vec<_>>(), "根节点索引要覆盖全部节点");
        assert_eq!(g["scene"], json!(0), "默认场景下标要指到那个非空场景");
    }

    #[test]
    fn node_transform_matches_the_aabb() {
        let s = scene();
        let g = scene_to_gltf(&s);
        let n = &g["nodes"][0];
        let node = &s.objects[0];
        let center = node.aabb.center();
        let size = node.aabb.size();
        assert_eq!(n["translation"], json!(center));
        assert_eq!(n["scale"], json!(size));
        assert_eq!(n["extras"]["rsi3d"]["aabb"], json!(node.aabb.to_array()));
    }

    #[test]
    fn geometry_grade_is_declared_loud_and_clear() {
        // H0 是包围盒代理：必须标注，别让客户端以为收到真网格
        let g = scene_to_gltf(&scene());
        assert_eq!(g["extras"]["rsi3d"]["geometry"], GEOMETRY_AABB_PROXY);
        assert_eq!(g["nodes"][0]["extras"]["rsi3d"]["geometry"], GEOMETRY_AABB_PROXY);
    }

    #[test]
    fn base64_blob_length_matches_the_buffer_views() {
        let g = scene_to_gltf(&scene());
        let uri = g["buffers"][0]["uri"].as_str().unwrap();
        let b64 = uri.trim_start_matches("data:application/octet-stream;base64,");
        let decoded = b64.len() / 4 * 3 - b64.matches('=').count(); // 粗略反推（无填充时精确）
        let declared = g["buffers"][0]["byteLength"].as_u64().unwrap() as usize;
        assert!(
            decoded.abs_diff(declared) <= 2,
            "base64 长度与 byteLength 对不上：{} vs {}",
            decoded,
            declared
        );
    }

    #[test]
    fn lights_without_direction_do_not_crash() {
        let mut s = scene();
        s.lights[0].direction = None;
        let g = scene_to_gltf(&s);
        assert!(g["extensions"]["KHR_lights_punctual"]["lights"][0]["extras"]["rsi3d"]["direction"].is_null());
    }
}
