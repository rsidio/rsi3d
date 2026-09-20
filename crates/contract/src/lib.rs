//! 契约的**单一出处**：从 Rust 类型「派生」出给非 Rust 消费者用的形状。
//!
//! # 为什么需要这个 crate
//!
//! 内核的 JSON 契约（`objects` / `window` / `spanX` / `bandDepth` …）现在被**手抄**在好几处
//! 非 Rust 代码里：
//!
//! | 镜像 | 读什么 |
//! | --- | --- |
//! | `crates/serve/src/client.js` | 节点 `name/translation/scale`、`extras.rsi3d.color`、`window.spanX`、增量 `nodes_upsert`、帧 `png_base64/band_occlusion`… |
//! | `scaffolds/agent-app/files/mock-engine.mjs` | `objects[].aabb`、`window.bandDepth/spanX`、`room.size`、`clearance_rules`… |
//! | `scaffolds/harness-plugin/files/src/evaluator.mjs` | `objects[].role/aabb`… |
//!
//! 改一处 Rust 字段、忘掉一处 JS，就是**静默漂移**。而最阴的一层是：
//! [`Scene`] 与 [`Node`] 都带 `#[serde(flatten)] extras`，**任何拼错的键都会被吞进 extras**——
//! `"band_depth"` 写成下划线，内核照单全收，只是那个窗带深度**悄悄变回默认值**，
//! 渲染测量跟着变，而没有任何报错。
//!
//! # 做法：从 Rust 派生，而不是手写第二份
//!
//! rspirv 从 Khronos 的 JSON grammar 生成 71k 行（见 `prd/rsi3d-harness/prior-art.md` §9），
//! 我们的同一类问题小得多，但可以用同一招，并且**让编译器当门禁**：
//!
//! 1. **全字段样例用 Rust 结构体字面量写**（[`full_scene`]）——字段改名/增删就**编译不过**；
//! 2. 键清单与 JSON Schema 由**序列化结果**派生（`serde` 的 `rename_all`/`alias`/`flatten`/
//!    `skip_serializing_if` 自动被算进去）；
//! 3. 枚举值用**穷尽 `match`** 产出（[`layer_name`] 等）——加一个变体就**编译不过**，
//!    另有单测断言它与 serde 的改名规则一致（[`crate::enums`]）；
//! 4. 产物落盘在 `contract/`，测试断言「重新生成 == 已提交」，并在消息里告诉你怎么重新生成。
//!
//! **schema 不是权威，内核才是**：真正的校验器永远是 `Scene::from_json`。这里产出的东西
//! 有两个用途——给外部作者/编辑器看的形状，以及**门禁**（抓漂移的那道）。

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use serde_json::{json, Map, Value};

use rsi3d_harness_core::{
    Aabb, ClearanceRule, Editability, Extras, IntentKeyword, LayerKind, Light, MaterialParams,
    Metrics, Node, Origin, Provenance, Room, Scene, Window,
};
use rsi3d_harness_stream::{
    scene_to_gltf, Camera, CapabilityKind, ClientMessage, ScenePatch, ServerMessage, StreamKind,
    FRAME_RENDERERS, KNOWN_CLIENT_CAPABILITIES, RENDERER_CPU_RASTER,
};

/// 产物清单（文件名 + 生成内容）。**顺序稳定**，便于对账。
pub const ARTIFACT_NAMES: [&str; 5] = [
    "keys.json",
    "scene.schema.json",
    "view.schema.json",
    "scene.example.json",
    // 渲染模式的**规则表**（平台 `/api/render/policy` 发这份；离线判定用同一份）
    "render-policy.json",
];

/// 生成全部产物。
pub fn artifacts() -> Vec<(&'static str, String)> {
    vec![
        ("keys.json", pretty(&knowledge())),
        // schema 这一份从**类型**派生（schemars），不从样例观察——观察法看不到
        // `required`、枚举变体、字段文档，也看不到样例没跑到的分支。
        // 键清单（`x-known-keys`）仍然来自样例：它喂的是镜像门禁，改来源要先过
        // `typed_schema_covers_the_observed_inventory` 那道闸。
        ("scene.schema.json", pretty(&typed_scene_schema())),
        ("view.schema.json", pretty(&typed_view_schema())),
        ("scene.example.json", pretty(&scene_example())),
        (
            "render-policy.json",
            pretty(&serde_json::to_value(rsi3d_harness_stream::render_mode::default_policy()).expect("规则表可序列化")),
        ),
    ]
}

/// 从 Rust 类型派生的 scene schema（schemars）。
///
/// 与旧的“观察法”相比多了三样东西：**`required`**（内核缺了就报错，旧 schema 里
/// 完全看不到）、**`$defs` 里的具名类型**（`anyOf`/`oneOf` 而不是一把平铺的键）、
/// 以及**字段文档**（从 Rust doc comment 来——外部作者读的就是这份）。
fn typed_scene_schema() -> Value {
    typed_schema(schemars::schema_for!(Scene), "rsi3d scene", "scene")
}

fn typed_view_schema() -> Value {
    typed_schema(
        schemars::schema_for!(rsi3d_harness_core::view::SceneView),
        "rsi3d scene view",
        "view",
    )
}

fn typed_schema(schema: schemars::Schema, title: &str, block_name: &str) -> Value {
    let mut v = serde_json::to_value(schema).unwrap_or_else(|_| json!({}));
    let mut known = BTreeSet::new();
    block(block_name).flatten("", &mut known);
    let known: Vec<String> = known.into_iter().collect();
    if let Some(o) = v.as_object_mut() {
        o.insert(
            "$schema".into(),
            json!("https://json-schema.org/draft/2020-12/schema"),
        );
        o.insert("title".into(), json!(title));
        o.insert("x-known-keys".into(), json!(known));
        o.insert(
            "$comment".into(),
            json!(
                "由 `#[derive(JsonSchema)]` 从 rsi3d-harness 的 Rust 类型**派生**（schemars）。\
                 `x-known-keys` 是喂给镜像门禁的键清单（仍从样例派生，两者一致性由 \
                 `typed_schema_covers_the_observed_inventory` 钉住）。\
                 重新生成：`rsi3d-harness contract --out contract`。"
            ),
        );
    }
    v
}

/// 把产物写到目录下（不存在就建）。
pub fn write_all(dir: &Path) -> Result<Vec<PathBuf>, String> {
    std::fs::create_dir_all(dir).map_err(|e| format!("建不了 {}：{}", dir.display(), e))?;
    let mut written = Vec::new();
    for (name, body) in artifacts() {
        let p = dir.join(name);
        std::fs::write(&p, body).map_err(|e| format!("写不到 {}：{}", p.display(), e))?;
        written.push(p);
    }
    Ok(written)
}

/// 能力类别的稳定字符串（契约里要用的名字）。
///
/// 穷尽 `match`：`CapabilityKind` 加变体 → 这里编译不过。
fn capability_kind_name(kind: CapabilityKind) -> &'static str {
    match kind {
        CapabilityKind::Consume => "consume",
        CapabilityKind::Render => "render",
        CapabilityKind::Downlevel => "downlevel",
        CapabilityKind::Form => "form",
        CapabilityKind::Robustness => "robustness",
    }
}

fn pretty(v: &Value) -> String {
    // 末尾补换行：POSIX 文本文件，`git diff` 也干净
    let mut s = serde_json::to_string_pretty(v).unwrap_or_else(|_| "{}".into());
    s.push('\n');
    s
}

// ---------------------------------------------------------------- 样例（编译器门禁）

/// **全字段**样例：每个字段都设了值，用结构体字面量构造。
///
/// 作用不是"好看"，而是**让编译器守住契约**：字段改名、删字段、加必填字段，
/// 这里都编译不过。键清单与 schema 都从它派生。
pub fn full_scene() -> Scene {
    Scene {
        spec: "rsi3d-scene/v1".into(),
        units: "m".into(),
        up_axis: "Y".into(),
        coordinate_system: "RUB".into(),
        source_coordinate_system: Some("RDF".into()),
        intent: Some("3 米挑高客厅，北欧风，落地窗，暖光".into()),
        room: Some(Room {
            size: [4.2, 2.8, 6.0],
            ceiling: Some(2.8),
            wall_color: Some("#f2ede6".into()),
            extras: Extras::new(),
        }),
        window: Some(Window {
            wall: Some("north".into()),
            span_x: [-1.2, 1.2],
            height: 1.6,
            z: -2.9,
            band_depth: 1.5,
            extras: Extras::new(),
        }),
        objects: vec![
            Node {
                id: "obj:sofa_01".into(),
                role: "furniture".into(),
                material: Some("fabric".into()),
                material_params: MaterialParams {
                    roughness: Some(0.85),
                    metallic: Some(0.0),
                    opacity: Some(1.0),
                },
                aabb: Aabb {
                    min: [-1.1, 0.0, -2.6],
                    max: [0.9, 0.85, -1.7],
                },
                layers: vec![LayerKind::Mesh],
                editability_cap: Some(Editability::Full),
                provenance: Provenance {
                    origin: Origin::Imported,
                    source: Some("vendor/sofa.glb".into()),
                    source_coordinate_system: Some("RDF".into()),
                    created_at_rev: Some(0),
                },
                metrics: Metrics {
                    triangles: 12_480,
                    gaussians: 0,
                    points: 0,
                    watertight: Some(true),
                },
                // 这个节点代表**导入来的资产**，所以 extras 里带 `mesh_ref`：
                // 它是契约的一部分（客户端就是靠它去 side-car 拿真网格）。
                // 除此之外 extras 一律留空——样例里随手塞一个键，那个键就会被当成
                // "契约的一部分"（键清单是从样例派生的），那是在骗人。
                extras: {
                    let mut e = Extras::new();
                    let mut rsi3d = serde_json::Map::new();
                    rsi3d.insert("geometry".into(), json!("aabb-proxy"));
                    rsi3d.insert("mesh_ref".into(), json!("sofa.mesh.glb"));
                    e.insert("rsi3d".into(), Value::Object(rsi3d));
                    e
                },
            },
            Node {
                id: "obj:rug_01".into(),
                role: "floor".into(),
                material: None,
                material_params: MaterialParams::default(),
                aabb: Aabb {
                    min: [-1.6, 0.0, 0.2],
                    max: [1.4, 0.02, 3.2],
                },
                layers: vec![LayerKind::Mesh],
                editability_cap: None,
                provenance: Provenance::default(),
                metrics: Metrics::default(),
                extras: Extras::new(),
            },
        ],
        lights: vec![
            Light {
                id: "sun".into(),
                kind: "directional".into(),
                intensity: 2.4,
                color: Some("#ffe9c9".into()),
                direction: Some([0.3, -1.0, 0.4]),
                extras: Extras::new(),
            },
            Light {
                id: "env".into(),
                kind: "hdri".into(),
                intensity: 0.6,
                color: None,
                direction: None,
                extras: Extras::new(),
            },
        ],
        clearance_rules: vec![ClearanceRule {
            pair: ["obj:sofa_01".into(), "obj:rug_01".into()],
            min: 0.4,
            max: 0.8,
            reason: "茶几应在沙发正前方 0.4–0.8m".into(),
            extras: Extras::new(),
        }],
        intent_keywords: vec![
            IntentKeyword {
                word: "落地窗".into(),
                present: true,
                note: None,
            },
            IntentKeyword {
                word: "绿植".into(),
                present: false,
                note: Some("缺一盆绿植，风格分上不去".into()),
            },
        ],
        extras: Extras::new(),
    }
}

/// **最小**样例：只有内核会给默认值的那些字段 + 一个节点。用来推导「每个样例都带的键」。
pub fn minimal_scene() -> Scene {
    Scene {
        objects: vec![Node::new(
            "obj:x",
            "furniture",
            Aabb {
                min: [0.0, 0.0, 0.0],
                max: [1.0, 1.0, 1.0],
            },
        )],
        ..Scene::default()
    }
}

/// 示例场景的 JSON（也是产物之一：给不读 Rust 的人一份完整样板）。
///
/// 它是一份**纯粹的、合契约的场景文件**：不多一个键（不然那个键会变成"契约的一部分"），
/// 也不少一个键。来源说明放在 `keys.json` / `scene.schema.json` 的 `$comment` 里，
/// 而不是往场景里塞一个说明字段——外部作者会照抄这份文件。
pub fn scene_example() -> Value {
    serde_json::to_value(full_scene()).expect("Scene 必须可序列化")
}

/// 一条增量（`Patch.changes`）的样板：用真的 `ScenePatch` 结构体构造。
fn patch_example() -> Value {
    let gltf_node = scene_to_gltf(&full_scene())["nodes"][0].clone();
    serde_json::to_value(ScenePatch {
        nodes_upsert: vec![gltf_node],
        nodes_remove: vec!["obj:gone_01".into()],
        lights_upsert: vec![json!({ "name": "sun", "intensity": 1.1 })],
        lights_remove: vec![],
        blockers: vec!["obj:sofa_01".into()],
        edit_count: 2,
    })
    .expect("ScenePatch 必须可序列化")
}

/// 每条服务端消息的样板（**穷尽**：新加变体会在这里编译不过）。
fn server_messages() -> Vec<ServerMessage> {
    let camera = Camera {
        azimuth_deg: -45.0,
        elevation_deg: 30.0,
        distance: 0.0,
        preset: Some("iso-sw".into()),
    };
    vec![
        ServerMessage::Welcome {
            protocol: "rsi3d-stream/v1".into(),
            server: "rsi3d-harness".into(),
            server_version: "0.1.0".into(),
            kind: StreamKind::Scene,
            revision: 3,
            scene_hash: "0".repeat(64),
            camera: camera.clone(),
            geometry: "aabb-proxy".into(),
            resumed: true,
            client_agent: "rsi3d-web/0.1.0".into(),
            client_capabilities: vec!["scene".into(), "three".into(), "webgl2".into()],
            client_render_tier: "three".into(),
        },
        ServerMessage::Snapshot {
            revision: 3,
            scene_hash: "0".repeat(64),
            gltf: scene_to_gltf(&full_scene()),
        },
        ServerMessage::Patch {
            from: 2,
            to: 3,
            scene_hash: "0".repeat(64),
            changes: patch_example(),
        },
        ServerMessage::Frame {
            revision: 3,
            view: "top".into(),
            width: 480,
            height: 360,
            renderer: RENDERER_CPU_RASTER.into(),
            image_hash: "0".repeat(64),
            png_base64: "iVBORw0KGgo".into(),
            band_occlusion: Some(0.5),
        },
        ServerMessage::Pong { nonce: 7 },
        ServerMessage::Error {
            code: "unknown_target".into(),
            message: "没有这个节点".into(),
        },
        ServerMessage::Bye {
            reason: "服务停止".into(),
        },
    ]
}

/// 每条客户端消息的样板（同样穷尽）。
fn client_messages() -> Vec<ClientMessage> {
    vec![
        ClientMessage::Subscribe {
            token: "t".into(),
            kind: StreamKind::Frame,
            from_revision: Some(2),
            camera: Some(Camera::default()),
        },
        ClientMessage::SetCamera {
            camera: Camera::default(),
        },
        ClientMessage::Command {
            op: "transform".into(),
            target: Some("obj:sofa_01".into()),
            params: json!({ "translate": [0, 0, 1.3] }),
            reason: "挪出挡光带".into(),
            expect: Some(json!({"rule.violated": "-"})),
        },
        ClientMessage::Ping { nonce: 7 },
    ]
}

// ---------------------------------------------------------------- 枚举（穷尽 match）

/// 层名。**穷尽 match**：`LayerKind` 加变体 → 这里编译不过。
pub fn layer_name(l: LayerKind) -> &'static str {
    match l {
        LayerKind::Mesh => "mesh",
        LayerKind::Gaussian => "gaussian",
        LayerKind::Points => "points",
        LayerKind::Light => "light",
        LayerKind::Camera => "camera",
    }
}

/// 可编辑性取值。同样穷尽。
pub fn editability_name(e: Editability) -> &'static str {
    match e {
        Editability::Full => "full",
        Editability::CropOnly => "crop-only",
        Editability::ReplaceOnly => "replace-only",
    }
}

/// 来源取值。同样穷尽。
pub fn origin_name(o: Origin) -> &'static str {
    match o {
        Origin::Unknown => "unknown",
        Origin::Imported => "imported",
        Origin::Generated => "generated",
        Origin::Edited => "edited",
    }
}

/// 枚举取值表。前三个由上面的穷尽函数产出，消息 type 直接从**构造出来的样例**里读
/// （一个字符串都不是手写的），流类型用内核自己的 `as_str()`。
pub fn enums() -> Value {
    let server_types: Vec<Value> = server_messages()
        .iter()
        .filter_map(|m| message_type(m))
        .collect();
    let client_types: Vec<Value> = client_messages()
        .iter()
        .filter_map(|m| message_type(m))
        .collect();
    json!({
        "layer": [
            layer_name(LayerKind::Mesh),
            layer_name(LayerKind::Gaussian),
            layer_name(LayerKind::Points),
            layer_name(LayerKind::Light),
            layer_name(LayerKind::Camera),
        ],
        "editability": [
            editability_name(Editability::Full),
            editability_name(Editability::CropOnly),
            editability_name(Editability::ReplaceOnly),
        ],
        "origin": [
            origin_name(Origin::Unknown),
            origin_name(Origin::Imported),
            origin_name(Origin::Generated),
            origin_name(Origin::Edited),
        ],
        "stream_kind": [StreamKind::Scene.as_str(), StreamKind::Frame.as_str()],
        "stream_server_message": server_types,
        "stream_client_message": client_types,
        // 客户端能力的词汇表：直接从单一出处（`crates/stream` 的 const）取，
        // 名字、含义、类别、依赖都带上——外部作者靠它知道"我能声明什么、它是哪一类"
        "client_capability": KNOWN_CLIENT_CAPABILITIES
            .iter()
            .map(|c| c.name.to_string())
            .collect::<Vec<_>>(),
        "client_capability_meaning": KNOWN_CLIENT_CAPABILITIES
            .iter()
            .map(|c| (c.name.to_string(), c.meaning.to_string()))
            .collect::<std::collections::BTreeMap<_, _>>(),
        // features / downlevel / limits 的分法（抄 wgpu）：`webgl1` 不是与 `webgl2`
        // 并列的能力，而是**降级档**
        "client_capability_kind": KNOWN_CLIENT_CAPABILITIES
            .iter()
            .map(|c| (c.name.to_string(), capability_kind_name(c.kind).to_string()))
            .collect::<std::collections::BTreeMap<_, _>>(),
        "client_capability_needs_any": KNOWN_CLIENT_CAPABILITIES
            .iter()
            .map(|c| (c.name.to_string(), c.needs_any.to_vec()))
            .collect::<std::collections::BTreeMap<_, _>>(),
        // 帧来源档：谁渲的、**能不能当证据**。表里不允许出现两个 true（多了就说明
        // 有人想把不可复现的东西也算成证据）
        "frame_renderer": FRAME_RENDERERS
            .iter()
            .map(|(name, evidence, meaning)| {
                json!({"name": name, "evidence": evidence, "meaning": meaning})
            })
            .collect::<Vec<_>>(),
        "note": "role 与 units 是自由字符串（内核用 RoleKind::parse 归类，不是闭集）；\
                 命令 op 名由内核的命令信封校验，这里不枚举",
    })
}

/// 读一条消息的 `type` 标签（`#[serde(tag = "type")]`）。
fn message_type<T: serde::Serialize>(m: &T) -> Option<Value> {
    serde_json::to_value(m).ok()?.get("type").cloned()
}

// ---------------------------------------------------------------- 键树的派生

/// 键树节点：合并多个样例得到「出现过的键 + 观察到的类型」。
#[derive(Debug, Default, Clone)]
struct Keys {
    types: BTreeSet<String>,
    keys: BTreeMap<String, Keys>,
    items: Option<Box<Keys>>,
    /// 标记子树「不归我们管」：仍是真实序列化的结果，但扁平键清单不再往下展。
    /// 用在 `Snapshot.gltf`——那是**标准 glTF 2.0 文档**，形状由 glTF 规范定义，
    /// 我们只拥有它里面的 `extras.rsi3d`（另有专门的块）。
    opaque: bool,
}

impl Keys {
    fn merge(&mut self, v: &Value) {
        match v {
            Value::Null => {
                self.types.insert("null".into());
            }
            Value::Bool(_) => {
                self.types.insert("boolean".into());
            }
            Value::Number(n) => {
                self.types.insert(if n.is_f64() { "number".into() } else { "integer".into() });
            }
            Value::String(_) => {
                self.types.insert("string".into());
            }
            Value::Array(a) => {
                self.types.insert("array".into());
                let slot = self.items.get_or_insert_with(|| Box::new(Keys::default()));
                for e in a {
                    slot.merge(e);
                }
            }
            Value::Object(o) => {
                self.types.insert("object".into());
                for (k, val) in o {
                    self.keys.entry(k.clone()).or_default().merge(val);
                }
            }
        }
    }

    fn to_json(&self) -> Value {
        let types: Vec<String> = self.types.iter().cloned().collect();
        if self.opaque {
            // 不展开：这部分形状由**别的规范**定义，展开只会制造噪音与假权威
            return json!({
                "type": types,
                "$opaque": "标准 glTF 2.0 文档：形状由 glTF 规范定义，我们只拥有其中的 extras.rsi3d（见 gltf_node_extras / gltf_scene_extras 块）",
            });
        }
        let mut out = Map::new();
        if types.len() == 1 {
            out.insert("type".into(), json!(types[0]));
        } else if !types.is_empty() {
            out.insert("type".into(), json!(types));
        }
        if !self.keys.is_empty() {
            let mut m = Map::new();
            for (k, v) in &self.keys {
                m.insert(k.clone(), v.to_json());
            }
            out.insert("keys".into(), Value::Object(m));
        }
        if let Some(i) = &self.items {
            out.insert("items".into(), i.to_json());
        }
        Value::Object(out)
    }

    fn flatten(&self, prefix: &str, out: &mut BTreeSet<String>) {
        if !prefix.is_empty() {
            out.insert(prefix.to_string());
        }
        if self.opaque {
            return;
        }
        for (k, v) in &self.keys {
            out.insert(k.clone());
            let child = if prefix.is_empty() {
                k.clone()
            } else {
                format!("{}.{}", prefix, k)
            };
            out.insert(child.clone());
            v.flatten(&child, out);
        }
        if let Some(i) = &self.items {
            i.flatten(prefix, out);
        }
    }
}

fn keys_of(v: &Value) -> Keys {
    let mut k = Keys::default();
    k.merge(v);
    k
}

/// 把某个键的子树标成 opaque（见 [`Keys::opaque`]）。
fn mark_opaque(mut root: Keys, path: &[&str]) -> Keys {
    if path.is_empty() {
        root.opaque = true;
        return root;
    }
    if let Some(child) = root.keys.get_mut(path[0]) {
        let taken = std::mem::take(child);
        *child = mark_opaque(taken, &path[1..]);
    }
    root
}

/// 各"块"的键树。块名是给人和测试用的稳定标识。
fn blocks() -> BTreeMap<&'static str, Keys> {
    let mut m = BTreeMap::new();

    let full = serde_json::to_value(full_scene()).unwrap();
    let minimal = serde_json::to_value(minimal_scene()).unwrap();
    m.insert("scene", {
        let mut k = keys_of(&full);
        k.merge(&minimal);
        k
    });
    m.insert("view", keys_of(&serde_json::to_value(full_scene().view()).unwrap()));

    let mut server = Keys::default();
    for msg in server_messages() {
        server.merge(&serde_json::to_value(&msg).unwrap());
    }
    // Snapshot 里嵌的 glTF 文档是标准 glTF 2.0，不展开（我们只拥有 extras.rsi3d）
    m.insert("stream_server", mark_opaque(server, &["gltf"]));

    let mut client = Keys::default();
    for msg in client_messages() {
        client.merge(&serde_json::to_value(&msg).unwrap());
    }
    m.insert("stream_client", client);

    // 渲染模式：三份形状都是**跨语言**的（浏览器探测 / 平台或本地判定 / 规则表），
    // 所以都进契约。样例故意用一台**弱机**：满档时 `fallbacks` 是空的，
    // 用满档当样例会让出路那几个字段（title/what/link/when_at_or_below）漏掉。
    let host_sample = rsi3d_harness_stream::render_mode::HostProfile {
        agent: "rsi3d-web/0.1.0".into(),
        form: "screen".into(),
        gpu: rsi3d_harness_stream::render_mode::GpuFacts {
            api: "webgl1".into(),
            webgpu: true,
            max_texture: 2048,
            renderer_hash: Some("a1b2c3d4e5f6".into()),
            // 明文型号是**显式开启**才有的字段（`?hw=plain`），样例里带上，
            // 免得它变成"契约里看不见的键"
            renderer: Some("Example GPU".into()),
            vendor_hash: Some("f6e5d4c3b2a1".into()),
            software: true,
        },
        cpu: rsi3d_harness_stream::render_mode::CpuFacts {
            cores: 2,
            memory_gb: Some(8.0),
            platform: "windows".into(),
        },
        display: rsi3d_harness_stream::render_mode::DisplayFacts {
            viewport: (1280, 720),
            dpr: 1.0,
            refresh_hz: Some(60),
        },
        bench: Some(rsi3d_harness_stream::render_mode::BenchResult {
            sustained_fps: 12.0,
            frames: 24,
            fillrate_mpx: 18.0,
            triangles_mps: 1.5,
            ms: 2000,
        }),
        privacy: "plaintext".into(),
    };
    let policy_sample = rsi3d_harness_stream::render_mode::default_policy();
    let verdict_sample = rsi3d_harness_stream::render_mode::assess(&host_sample, &policy_sample, "platform");
    m.insert(
        "host_profile",
        keys_of(&serde_json::to_value(&host_sample).unwrap()),
    );
    m.insert(
        "render_policy",
        keys_of(&serde_json::to_value(&policy_sample).unwrap()),
    );
    m.insert(
        "render_verdict",
        keys_of(&serde_json::to_value(&verdict_sample).unwrap()),
    );

    let gltf = scene_to_gltf(&full_scene());
    // 节点 extras 有两副面孔：手搭场景没有 `mesh_ref`，**导入来的**节点有。
    // 清单必须两份都覆盖，否则 mirrored 的客户端一提到 mesh_ref 就会被判为"引用未知键"。
    let mut node_extras = keys_of(&gltf["nodes"][0]["extras"]);
    if let Some(nodes) = gltf["nodes"].as_array() {
        for n in nodes {
            if n.pointer("/extras/rsi3d/mesh_ref").is_some() {
                node_extras.merge(&n["extras"]);
            }
        }
    }
    m.insert("gltf_node_extras", node_extras);
    m.insert("gltf_scene_extras", keys_of(&gltf["extras"]));
    m.insert("gltf_patch_changes", keys_of(&patch_example()));

    m
}

fn block(name: &str) -> Keys {
    blocks()
        .remove(name)
        .unwrap_or_else(|| panic!("没有这个契约块：{}", name))
}

/// 键清单（产物 `keys.json` 的内容）。
pub fn knowledge() -> Value {
    let b = blocks();
    let mut out = Map::new();
    out.insert("spec".into(), json!("rsi3d-contract/v1"));
    out.insert(
        "derived_from".into(),
        json!([
            "crates/core/src/scene.rs（Scene / Node / Window / Room / Light / ClearanceRule…）",
            "crates/core/src/view.rs（SceneView：评测插件读的那份形状）",
            "crates/stream/src/protocol.rs（ServerMessage / ClientMessage）",
            "crates/stream/src/gltf.rs（extras.rsi3d 与增量载荷）",
        ]),
    );
    out.insert(
        "note".into(),
        json!(
            "这份清单**由 Rust 类型派生**（全字段样例用结构体字面量写，枚举用穷尽 match），\
             不是手写的第二份真相。`keys` 里带点号的是嵌套路径，裸名的是任意层级的键。\
             ⚠️ Scene/Node/glTF 节点都带 serde flatten 的 extras：**拼错的键会被内核吞进 extras \
             而不报错**（例如把 bandDepth 写成 band_depth，窗带深度会悄悄回落默认值）——\
             这正是这份清单要拦住的事。schema 不是权威，内核（Scene::from_json）才是。"
        ),
    );
    out.insert("enums".into(), enums());

    // "每个样例都有"的键（信息性，不是规范性要求：内核几乎每个字段都有默认值）
    let full = serde_json::to_value(full_scene()).unwrap();
    let minimal = serde_json::to_value(minimal_scene()).unwrap();
    let both: BTreeSet<String> = top_keys(&full)
        .intersection(&top_keys(&minimal))
        .cloned()
        .collect();
    out.insert(
        "present_in_both_rust_examples".into(),
        json!({
            "scene": both.iter().cloned().collect::<Vec<_>>(),
            "note": "内核给几乎每个字段都设了默认值，所以这里不是「必需键」，只是「我们自己的样例都带的键」",
        }),
    );

    let mut block_map = Map::new();
    for (name, k) in &b {
        block_map.insert((*name).to_string(), k.to_json());
    }
    out.insert("blocks".into(), Value::Object(block_map));
    Value::Object(out)
}

fn top_keys(v: &Value) -> BTreeSet<String> {
    v.as_object()
        .map(|o| o.keys().cloned().collect())
        .unwrap_or_default()
}

/// 某个契约块里有没有这个键（裸名或路径）。**镜像门禁**就靠它。
pub fn has_key(block_name: &str, key: &str) -> bool {
    let mut set = BTreeSet::new();
    block(block_name).flatten("", &mut set);
    set.contains(key)
}

/// 列出某个契约块的全部已知键（排序后）。
pub fn known_keys(block_name: &str) -> Vec<String> {
    let mut set = BTreeSet::new();
    block(block_name).flatten("", &mut set);
    set.into_iter().collect()
}

// ---------------------------------------------------------------- 校验器（门禁用）

/// 校验器报出来的一条问题。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Finding {
    /// 带下标的路径，如 `$.lights[1].note`（给人看的）
    pub path: String,
    /// 键名
    pub key: String,
    pub kind: FindingKind,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FindingKind {
    /// 不在契约键清单里。
    ///
    /// ⚠️ 这**不一定**是错误：所有带 `#[serde(flatten)] extras` 的层都"合法地"接受任意键。
    /// 它只说明一件事——内核认不出这个键，会原样收进 extras。所以要么是拼错了，
    /// 要么是有意放的扩展字段；这两者从 JSON 上分不出来，得由人（或白名单）判断。
    UnknownKey,
    /// 类型与契约不符（同一个键在别处是对象、这里是字符串这类）
    TypeMismatch,
}

impl Finding {
    /// 把 `$.lights[1].note` 归一成 `lights[].note`：白名单按键型匹配，
    /// 不然每个数组元素都得单独列一条。
    pub fn pattern(&self) -> String {
        let mut out = String::new();
        let mut bracket = false;
        for ch in self.path.chars() {
            match ch {
                '[' => {
                    bracket = true;
                    out.push('[');
                }
                ']' => {
                    bracket = false;
                    out.push(']');
                }
                c if bracket && c.is_ascii_digit() => {}
                c => out.push(c),
            }
        }
        out.trim_start_matches("$.").to_string()
    }
}

impl std::fmt::Display for Finding {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self.kind {
            FindingKind::UnknownKey => write!(
                f,
                "{}：未知键（不在契约里，多半是拼错了；内核会把它吞进 extras）",
                self.path
            ),
            FindingKind::TypeMismatch => write!(f, "{}：类型与契约不符", self.path),
        }
    }
}

/// 按派生出来的键清单检查一份场景 JSON。
///
/// 判据只有两条（**故意不假装比内核更严**）：未知键、类型不符。
pub fn validate_scene_json(v: &Value) -> Vec<Finding> {
    let mut out = Vec::new();
    check_against(
        &keys_of(&serde_json::to_value(full_scene()).unwrap()),
        v,
        "$",
        &mut out,
    );
    out
}

fn check_against(schema: &Keys, v: &Value, path: &str, out: &mut Vec<Finding>) {
    let ty = json_type(v);
    if !schema.types.is_empty() && !schema.types.contains(ty) {
        // integer / number 视作同类
        let compatible = (ty == "integer" && schema.types.contains("number"))
            || (ty == "number" && schema.types.contains("integer"))
            || (ty == "null" && schema.types.contains("object")); // 可选对象可以显式给 null
        if !compatible {
            out.push(Finding {
                path: path.to_string(),
                key: path.rsplit('.').next().unwrap_or(path).to_string(),
                kind: FindingKind::TypeMismatch,
            });
        }
    }
    match v {
        Value::Object(o) => {
            for (k, val) in o {
                match schema.keys.get(k) {
                    Some(child) => check_against(child, val, &format!("{}.{}", path, k), out),
                    None => out.push(Finding {
                        path: if path == "$" {
                            k.clone()
                        } else {
                            format!("{}.{}", path, k)
                        },
                        key: k.clone(),
                        kind: FindingKind::UnknownKey,
                    }),
                }
            }
        }
        Value::Array(a) => {
            if let Some(items) = &schema.items {
                for (i, e) in a.iter().enumerate() {
                    check_against(items, e, &format!("{}[{}]", path, i), out);
                }
            }
        }
        _ => {}
    }
}

fn json_type(v: &Value) -> &'static str {
    match v {
        Value::Null => "null",
        Value::Bool(_) => "boolean",
        Value::Number(n) => {
            if n.is_f64() {
                "number"
            } else {
                "integer"
            }
        }
        Value::String(_) => "string",
        Value::Array(_) => "array",
        Value::Object(_) => "object",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn enum_names_match_serde_renaming() {
        // 手写的字符串必须与 serde 的 rename_all 规则一致（否则产物会骗人）
        for l in [
            LayerKind::Mesh,
            LayerKind::Gaussian,
            LayerKind::Points,
            LayerKind::Light,
            LayerKind::Camera,
        ] {
            assert_eq!(serde_json::to_value(l).unwrap(), json!(layer_name(l)));
        }
        for e in [
            Editability::Full,
            Editability::CropOnly,
            Editability::ReplaceOnly,
        ] {
            assert_eq!(serde_json::to_value(e).unwrap(), json!(editability_name(e)));
        }
        for o in [
            Origin::Unknown,
            Origin::Imported,
            Origin::Generated,
            Origin::Edited,
        ] {
            assert_eq!(serde_json::to_value(o).unwrap(), json!(origin_name(o)));
        }
    }

    #[test]
    fn key_tree_uses_json_names_not_rust_names() {
        // 最典型的坑：Rust 叫 span_x，线上叫 spanX（rename_all = camelCase）
        assert!(has_key("scene", "window.spanX"), "键清单里应当有 camelCase 的 spanX");
        assert!(!has_key("scene", "window.span_x"), "不该出现 Rust 字段名");
        assert!(has_key("scene", "window.bandDepth"));
        assert!(has_key("scene", "objects"), "JSON 键是 objects（nodes 是别名）");
        // 增量载荷与帧
        assert!(has_key("stream_server", "changes.nodes_upsert"));
        assert!(has_key("stream_server", "band_occlusion"));
        assert!(has_key("stream_server", "png_base64"));
        // 插件读的那份 view
        assert!(has_key("view", "objects"));
        assert!(has_key("view", "blockers"));
        assert!(has_key("view", "object_count"));
    }

    #[test]
    fn message_types_are_derived_not_guessed() {
        // 消息 type 值直接从构造出来的样例里读；这里把它钉成"期望是什么"
        let e = enums();
        assert_eq!(
            e["stream_server_message"],
            json!(["welcome", "snapshot", "patch", "frame", "pong", "error", "bye"])
        );
        assert_eq!(
            e["stream_client_message"],
            json!(["subscribe", "set_camera", "command", "ping"])
        );
        assert_eq!(e["stream_kind"], json!(["scene", "frame"]));
    }

    #[test]
    fn opaque_subtrees_are_not_expanded() {
        // 内嵌的 glTF 文档是标准 glTF 2.0，不该混进我们的键清单
        assert!(has_key("stream_server", "gltf"), "gltf 这个键本身要在");
        assert!(
            !has_key("stream_server", "accessors"),
            "glTF 内部键不该出现在我们的清单里"
        );
        // 但 glTF 里**属于我们**的部分另有块
        assert!(has_key("gltf_node_extras", "rsi3d.aabb"));
        assert!(has_key("gltf_node_extras", "rsi3d.geometry"));
        assert!(has_key("gltf_scene_extras", "rsi3d.window.spanX"));
        assert!(has_key("gltf_scene_extras", "rsi3d.window_blockers"));
    }

    #[test]
    fn validator_catches_typos_and_type_mismatch() {
        let good = serde_json::to_value(full_scene()).unwrap();
        assert!(
            validate_scene_json(&good).is_empty(),
            "{:?}",
            validate_scene_json(&good)
        );

        // 拼错一个键：内核**照收**（进 extras），只有这里会报
        let mut typo = good.clone();
        typo["window"]["band_depth"] = json!(1.5);
        let found = validate_scene_json(&typo);
        assert_eq!(found.len(), 1, "{:?}", found);
        assert_eq!(found[0].key, "band_depth");
        assert_eq!(found[0].pattern(), "window.band_depth");
        assert_eq!(found[0].kind, FindingKind::UnknownKey);
        // 而且内核确实不报错 —— 这就是"静默漂移"，必须由契约门禁兜住
        let as_text = serde_json::to_string(&typo).unwrap();
        assert!(
            Scene::from_json(&as_text).is_ok(),
            "内核确实会接受拼错的键（所以这门禁不是多余的）"
        );

        // 类型不符
        let mut wrong = good.clone();
        wrong["objects"] = json!("不是数组");
        assert!(validate_scene_json(&wrong)
            .iter()
            .any(|f| f.kind == FindingKind::TypeMismatch));
    }

    #[test]
    fn findings_normalize_array_indices() {
        // 白名单按键型匹配，所以下标必须被消掉
        let f = Finding {
            path: "$.lights[12].note".into(),
            key: "note".into(),
            kind: FindingKind::UnknownKey,
        };
        assert_eq!(f.pattern(), "lights[].note");
        assert!(format!("{}", f).contains("$.lights[12].note"));
    }
}
