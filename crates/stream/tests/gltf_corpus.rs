//! **语料驱动的往返测试**（corpus + round-trip）。
//!
//! # 这个文件的方法论是从哪来的
//!
//! 评估 `gfx-rs/rspirv` 时抄来的做法：它不靠"手写几个样例"证明自己，而是把
//! `spirv-blobs/` 里**真实世界的 SPIR-V 二进制**当语料，逐个做
//! 「load → disassemble → 重建 → 比较」，用往返来证明表示无损。
//!
//! 我们对应的脆弱点是 **USC → glTF 这道边界**：它一头连着内核（唯一真相），
//! 另一头连着外部工具链（three.js / Blender / Fyrox）。而就在最近，这道边界上
//! 出过一个**只有第三方实现才抓得到**的 bug：`scenes[].nodes` 写成空数组——
//! 文件合法，标准加载器却读到**空场景**。我们自己的往返测试当时是绿的，
//! 因为 `from_gltf` 读的是扁平 `nodes` 池，**根本不看 `scenes[].nodes`**。
//!
//! 所以这里做两件事，缺一不可：
//! 1. **结构自洽性**：按 glTF 的引用关系逐条检查（下标范围、字节范围、场景引用、
//!    data URI 长度）——这一层能抓住"round-trip 绿但文件是空场景"这类问题；
//! 2. **往返一致**：导出的 glTF 再读回来，节点表必须与源场景逐字段相同。
//!
//! 场景语料是**程序生成的结构多样集合**（多对象/重叠/超界/退化/无窗/多灯/编辑后），
//! 不是真实资产——真实资产要等 `io` 落地。这里诚实地说清楚这个限度。

use serde_json::{json, Value};

use rsi3d_harness_core::{CommandRequest, Document, Scene};
use rsi3d_harness_stream::gltf::{scene_to_gltf, GEOMETRY_AABB_PROXY};
use rsi3d_harness_stream::session::from_gltf;

// ---------------------------------------------------------------- 语料

/// 一个语料：名字 + 场景 JSON（要能构建出文档）。
struct Corpus {
    name: &'static str,
    scene: String,
}

fn room(objects: &str, lights: &str, window: &str) -> String {
    format!(
        r##"{{
  "units": "m",
  "room": {{ "size": [4.2, 2.8, 6.0] }},
  {window}
  "objects": [ {objects} ],
  "lights": [ {lights} ]
}}"##
    )
}

fn obj(id: &str, role: &str, material: &str, min: [f64; 3], max: [f64; 3]) -> String {
    format!(
        r##"{{ "id": "{id}", "role": "{role}", "material": "{material}",
             "aabb": {{ "min": [{}, {}, {}], "max": [{}, {}, {}] }} }}"##,
        min[0], min[1], min[2], max[0], max[1], max[2]
    )
}

const WINDOW: &str = r#""window": { "wall": "north", "spanX": [-1.2, 1.2], "height": 1.6, "z": -2.9, "bandDepth": 1.5 },"#;

/// 结构多样的语料。**不要删条目**：每一条都对应一类边界（见注释）。
fn corpus() -> Vec<Corpus> {
    let mut out = Vec::new();

    // 1. 空场景：只有房间与窗（没有节点可导出）
    out.push(Corpus {
        name: "只有房间与窗",
        scene: room("", "", WINDOW),
    });

    // 2. 单对象
    out.push(Corpus {
        name: "单对象",
        scene: room(
            &obj("obj:sofa_01", "furniture", "fabric", [-1.1, 0.0, -2.6], [0.9, 0.85, -1.7]),
            r#"{ "id": "sun", "kind": "directional", "intensity": 2.0, "direction": [0.3, -1.0, 0.4] }"#,
            WINDOW,
        ),
    });

    // 3. 多对象（12 个）：索引与场景引用在数量上更容易出错
    let many: Vec<String> = (0..12)
        .map(|i| {
            let x = -1.8 + (i % 4) as f64 * 1.2;
            let z = -2.2 + (i / 4) as f64 * 1.4;
            obj(
                &format!("obj:box_{:02}", i),
                "furniture",
                if i % 2 == 0 { "oak" } else { "fabric" },
                [x, 0.0, z],
                [x + 0.5, 0.4 + (i % 3) as f64 * 0.2, z + 0.5],
            )
        })
        .collect();
    out.push(Corpus {
        name: "多对象（12）",
        scene: room(&many.join(","), r#"{ "id": "sun", "kind": "directional", "intensity": 2.0 }"#, WINDOW),
    });

    // 4. 相互重叠：diff/增量容易在这里出错
    out.push(Corpus {
        name: "两两重叠",
        scene: room(
            &format!(
                "{},{}",
                obj("obj:a", "furniture", "oak", [0.0, 0.0, 0.0], [1.0, 1.0, 1.0]),
                obj("obj:b", "furniture", "fabric", [0.5, 0.5, 0.5], [1.5, 1.5, 1.5])
            ),
            "",
            WINDOW,
        ),
    });

    // 5. 超界（对象探出房间）：应当带告警但**仍可导出**
    out.push(Corpus {
        name: "对象探出房间",
        scene: room(
            &obj("obj:out", "furniture", "oak", [1.8, 0.0, 2.6], [3.4, 0.8, 4.2]),
            "",
            WINDOW,
        ),
    });

    // 6. 退化几何：极薄（0.001m）——包围盒代理下最容易露馅的一类
    out.push(Corpus {
        name: "极薄片（0.001m）",
        scene: room(
            &obj("obj:thin", "floor", "fabric", [-1.0, 0.0, -1.0], [1.0, 0.001, 1.0]),
            "",
            WINDOW,
        ),
    });

    // 7. 无窗场景：window 缺省时 bundle 不该崩（也测 extras 的形态）
    out.push(Corpus {
        name: "无窗",
        scene: r#"{
  "units": "m",
  "room": { "size": [3.0, 2.6, 3.0] },
  "objects": [ { "id": "obj:x", "role": "furniture", "material": "oak",
                 "aabb": { "min": [0.0, 0.0, 0.0], "max": [0.5, 0.5, 0.5] } } ]
}"#
        .to_string(),
    });

    // 8. 多灯光 + 点光（灯光数组的两种 kind 都要走到）
    out.push(Corpus {
        name: "多灯光（方向光 + 点光）",
        scene: room(
            &obj("obj:lamp_table", "furniture", "oak", [-0.5, 0.0, 1.0], [0.5, 0.45, 1.6]),
            r#"{ "id": "sun", "kind": "directional", "intensity": 2.4, "direction": [0.3, -1.0, 0.4] },
               { "id": "lamp", "kind": "point", "intensity": 0.8 }"#,
            WINDOW,
        ),
    });

    // 9. Unicode 标识与理由：JSON/序列化在非 ASCII 上翻车很常见
    out.push(Corpus {
        name: "Unicode 标识",
        scene: room(
            &obj("obj:沙发·一号", "furniture", "fabric", [-1.0, 0.0, -1.0], [1.0, 0.8, 1.0]),
            r#"{ "id": "主光·暖", "kind": "directional", "intensity": 2.0 }"#,
            WINDOW,
        ),
    });

    out
}

// ---------------------------------------------------------------- 断言

/// 按 glTF 2.0 的**引用关系**逐条自检。
///
/// 这一层是重点：它不看"我们想导出什么"，只看"文件自不自洽"。
fn assert_gltf_self_consistent(g: &Value, corpus_name: &str) {
    // `panic!` 是 `!` 类型，所以能直接用在 `unwrap_or_else` / 三元位置里
    macro_rules! fail {
        ($($t:tt)*) => {
            panic!("[{}] glTF 自洽性不通过：{}", corpus_name, format!($($t)*))
        };
    }

    if g["asset"]["version"] != "2.0" {
        fail!("asset.version = {}", g["asset"]["version"]);
    }

    let nodes = g["nodes"].as_array().expect("nodes 必须是数组");
    let buffers = g["buffers"].as_array().expect("buffers 必须是数组");
    let buffer_views = g["bufferViews"].as_array().expect("bufferViews 必须是数组");
    let accessors = g["accessors"].as_array().expect("accessors 必须是数组");
    let meshes = g["meshes"].as_array().expect("meshes 必须是数组");

    // 场景必须引用全部节点，且下标必须在范围内
    let scenes = g["scenes"].as_array().expect("scenes 必须是数组");
    let scene_idx = g["scene"].as_u64().expect("scene 必须是下标") as usize;
    if scene_idx >= scenes.len() {
        fail!("scene 下标 {} 越界（共 {}）", scene_idx, scenes.len());
    }
    let roots: Vec<usize> = scenes[scene_idx]["nodes"]
        .as_array()
        .expect("scenes[].nodes 必须是数组")
        .iter()
        .map(|v| v.as_u64().expect("根节点必须是下标") as usize)
        .collect();
    for r in &roots {
        if *r >= nodes.len() {
            fail!("根节点下标 {} 越界（共 {} 个节点）", r, nodes.len());
        }
    }
    let mut sorted = roots.clone();
    sorted.sort_unstable();
    sorted.dedup();
    if sorted.len() != nodes.len() {
        fail!(
            "场景只引用了 {} 个节点，但节点池里有 {} 个——不引用的节点对加载器不存在",
            sorted.len(),
            nodes.len()
        );
    }

    // 每个节点：mesh 下标有效，变换可读，extras 齐全
    for (i, n) in nodes.iter().enumerate() {
        if let Some(m) = n.get("mesh") {
            if m.as_u64().unwrap_or(u64::MAX) as usize >= meshes.len() {
                fail!("节点 {} 的 mesh 下标无效", i);
            }
        }
        for key in ["translation", "scale"] {
            let a = match n[key].as_array() {
                Some(v) => v,
                None => fail!("节点 {} 缺 {}", i, key),
            };
            if a.len() != 3 || a.iter().any(|v| !v.is_number()) {
                fail!("节点 {} 的 {} 不是三个数", i, key);
            }
        }
        let ex = &n["extras"]["rsi3d"];
        if ex["geometry"] != GEOMETRY_AABB_PROXY {
            fail!("节点 {} 没有声明几何档次", i);
        }
        if ex["id"].as_str().is_none() || ex["aabb"].as_array().is_none() {
            fail!("节点 {} 的 extras.rsi3d 不完整", i);
        }
        // 变换必须与 extras 里的 aabb 一致（这是我们"代理几何"的核心不变量）
        // —— 也正是"文件合法但内容为空/错位"这类问题最藏身的地方
        let aabb = ex["aabb"].as_array().unwrap();
        let (min, max) = (aabb[0].as_array().unwrap(), aabb[1].as_array().unwrap());
        for k in 0..3 {
            let want_center = (min[k].as_f64().unwrap() + max[k].as_f64().unwrap()) / 2.0;
            let got = n["translation"][k].as_f64().unwrap();
            if (want_center - got).abs() > 1e-9 {
                fail!("节点 {} 轴 {} 的中心与 aabb 不一致", i, k);
            }
            let want_size = max[k].as_f64().unwrap() - min[k].as_f64().unwrap();
            let got_size = n["scale"][k].as_f64().unwrap();
            if (want_size - got_size).abs() > 1e-9 {
                fail!("节点 {} 轴 {} 的尺寸与 aabb 不一致", i, k);
            }
        }
    }

    // buffer / bufferView / accessor 的引用与字节范围
    let total_bytes = buffers[0]["byteLength"].as_u64().expect("buffers[0].byteLength") as usize;
    if let Some(uri) = buffers[0]["uri"].as_str() {
        let b64 = match uri.strip_prefix("data:application/octet-stream;base64,") {
            Some(v) => v,
            None => fail!("buffer 必须是内嵌 data URI（单文件交接）"),
        };
        let decoded = rsi3d_harness_stream::decode_b64(b64).expect("base64 必须能解");
        if decoded.len() != total_bytes {
            fail!(
                "buffer 声明 {} 字节，实际解出 {} 字节",
                total_bytes,
                decoded.len()
            );
        }
    }
    for (i, bv) in buffer_views.iter().enumerate() {
        if bv["buffer"].as_u64().unwrap_or(u64::MAX) as usize >= buffers.len() {
            fail!("bufferView {} 指向不存在的 buffer", i);
        }
        let off = bv["byteOffset"].as_u64().unwrap_or(0) as usize;
        let len = bv["byteLength"].as_u64().unwrap_or(0) as usize;
        if off + len > total_bytes {
            fail!(
                "bufferView {} 越出 buffer（{} + {} > {}）",
                i,
                off,
                len,
                total_bytes
            );
        }
    }
    for (i, a) in accessors.iter().enumerate() {
        if a["bufferView"].as_u64().unwrap_or(u64::MAX) as usize >= buffer_views.len() {
            fail!("accessor {} 指向不存在的 bufferView", i);
        }
        if a["count"].as_u64().unwrap_or(0) == 0 {
            fail!("accessor {} 的 count 为 0", i);
        }
    }
    for (i, m) in meshes.iter().enumerate() {
        for (j, p) in m["primitives"].as_array().unwrap().iter().enumerate() {
            for (name, idx) in p["attributes"].as_object().unwrap() {
                if idx.as_u64().unwrap_or(u64::MAX) as usize >= accessors.len() {
                    fail!(
                        "mesh {} 的 primitive {} 属性 {} 指向不存在的 accessor",
                        i,
                        j,
                        name
                    );
                }
            }
        }
    }
}

/// 源场景 → (id, center, size) 表，用来比对往返结果。
fn source_nodes(s: &Scene) -> Vec<(String, [f64; 3], [f64; 3])> {
    let mut v: Vec<_> = s
        .objects
        .iter()
        .map(|n| (n.id.clone(), n.aabb.center(), n.aabb.size()))
        .collect();
    v.sort_by(|a, b| a.0.cmp(&b.0));
    v
}

// ---------------------------------------------------------------- 用例

#[test]
fn every_corpus_scene_exports_a_self_consistent_gltf() {
    let mut nodes_seen = 0usize;
    let mut scenes_seen = 0usize;

    for c in corpus() {
        let scene = Scene::from_json(&c.scene)
            .unwrap_or_else(|e| panic!("[{}] 语料本身不合法：{}", c.name, e));
        let g = scene_to_gltf(&scene);

        assert_gltf_self_consistent(&g, c.name);

        // 往返：读回来的节点表必须与源场景一致（逐字段）
        let back = from_gltf(&g).unwrap_or_else(|e| panic!("[{}] 读回失败：{}", c.name, e));
        let mut got: Vec<(String, [f64; 3], [f64; 3])> = back
            .nodes
            .values()
            .map(|n| (n.name.clone(), n.translation, n.scale))
            .collect();
        got.sort_by(|a, b| a.0.cmp(&b.0));
        assert_eq!(
            source_nodes(&scene),
            got,
            "[{}] 往返后节点表与源场景不一致",
            c.name
        );

        // 确定性：同状态必须导出同一份字节（与内核"同内容同哈希"同一条纪律）
        let again = serde_json::to_string(&scene_to_gltf(&scene)).unwrap();
        let once = serde_json::to_string(&g).unwrap();
        assert_eq!(once, again, "[{}] 两次导出结果不同", c.name);

        scenes_seen += 1;
        nodes_seen += scene.objects.len();
    }

    // 语料不许悄悄缩水（缩水 = 覆盖变窄 = 这类断言会退化成摆设）
    assert!(scenes_seen >= 9, "语料太少了：{}", scenes_seen);
    assert!(nodes_seen >= 20, "语料节点太少了：{}", nodes_seen);
}

#[test]
fn edits_and_rollbacks_still_export_consistently() {
    // 编辑之后的世界同样要能交接；而且**回滚的语义必须在导出里也成立**
    let base = corpus()
        .into_iter()
        .find(|c| c.name == "多对象（12）")
        .unwrap()
        .scene;
    let mut doc = Document::from_scene_json(&base).unwrap();

    let run = |doc: &mut Document, op: &str, target: &str, params: Value| {
        doc.apply_request(&CommandRequest {
            op: op.to_string(),
            target: if target.is_empty() {
                None
            } else {
                Some(target.to_string())
            },
            params,
            reason: format!("语料：{}", op),
            expect: None,
        })
        .unwrap_or_else(|e| panic!("{} 被拒：{}", op, e));
    };

    run(&mut doc, "transform", "obj:box_00", json!({"translate": [0.0, 0.0, 1.2]}));
    run(&mut doc, "remove", "obj:box_05", json!({}));
    run(&mut doc, "set_light", "sun", json!({"intensity": 1.1}));
    run(&mut doc, "transform", "obj:box_11", json!({"scale": 1.5}));

    // 1) 编辑后的状态：删掉的对象必须不在文件里，节点数与当前状态一致
    let edited = scene_to_gltf(doc.scene());
    assert_gltf_self_consistent(&edited, "编辑后");
    let edited_names = node_names(&edited);
    assert!(
        !edited_names.contains(&"obj:box_05".to_string()),
        "删掉的节点还在导出里"
    );
    assert_eq!(edited_names.len(), doc.scene().objects.len());

    // 2) 回滚到 rev 1：**回滚 = 状态恢复**，所以被删的对象必须回来。
    //    （这条最初被我写反了：我以为"删掉的就不该出现"，而回滚恰恰是反着来的。）
    run(&mut doc, "checkout", "", json!({"rev": 1}));
    let rolled = scene_to_gltf(doc.scene());
    assert_gltf_self_consistent(&rolled, "回滚到 rev 1 后");
    let rolled_names = node_names(&rolled);
    assert!(
        rolled_names.contains(&"obj:box_05".to_string()),
        "回滚之后被删的节点应当恢复"
    );
    assert_eq!(rolled_names.len(), doc.scene().objects.len());
    // 两次导出必须不同：导出如果没跟着状态走，上面两条断言会同时为真而毫无意义
    assert_ne!(
        serde_json::to_string(&edited).unwrap(),
        serde_json::to_string(&rolled).unwrap(),
        "回滚前后导出了同一份文件——导出没跟着状态走"
    );

    // 3) 回滚之后再编辑：状态继续推进（这也测了旋转走 AABB 的路径）
    run(&mut doc, "transform", "obj:box_03", json!({"rotate_y_deg": 30.0}));
    let after = scene_to_gltf(doc.scene());
    assert_gltf_self_consistent(&after, "回滚后再编辑");
    assert_eq!(node_names(&after).len(), doc.scene().objects.len());
    assert_eq!(
        after["extras"]["rsi3d"]["window_blockers"],
        json!(doc.scene().window_blockers())
    );
}

fn node_names(g: &Value) -> Vec<String> {
    g["nodes"]
        .as_array()
        .unwrap()
        .iter()
        .map(|n| n["name"].as_str().unwrap().to_string())
        .collect()
}

#[test]
fn the_gate_rejects_structurally_broken_scenes() {
    // 语料只覆盖"合法输入"；这里补一句反面：坏输入必须在**进门时**被拒，
    // 而不是被导出成一份"看起来对"的文件。
    for (why, bad) in [
        ("缺 aabb", r#"{"room":{"size":[3,3,3]},"objects":[{"id":"a","role":"furniture"}]}"#),
        ("min > max", r#"{"room":{"size":[3,3,3]},"objects":[{"id":"a","role":"furniture","aabb":{"min":[1,1,1],"max":[0,0,0]}}]}"#),
        ("非有限数", r#"{"room":{"size":[3,3,3]},"objects":[{"id":"a","role":"furniture","aabb":{"min":[0,0,0],"max":[1,"NaN",1]}}]}"#),
    ] {
        assert!(
            Scene::from_json(bad).is_err(),
            "坏输入（{}）竟然被接受了",
            why
        );
    }
}
