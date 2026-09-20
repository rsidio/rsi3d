//! 契约的**门禁**：产物不许过期、镜像不许引用不存在的键、出厂场景必须合契约。
//!
//! 这些测试的价值全在"它们失败的时候"：
//! * 改了 Rust 字段而没重新生成 → [`committed_artifacts_match_the_source`] 红；
//! * 改了 Rust 字段而没改手抄的 JS → [`mirrors_only_reference_known_keys`] 红；
//! * 出厂场景里写错了键 → [`shipped_scenes_conform_to_the_contract`] 红
//!   （内核自己**不会**报错，见 `crates/contract/src/lib.rs` 的单测）。
//!
//! 换句话：内核保证"能跑"，这里保证"跑的还是你以为的那件事"。

use std::path::{Path, PathBuf};

use serde_json::Value;

use rsi3d_harness_contract::{self as contract, FindingKind};
use rsi3d_harness_core::Scene;

fn repo_root() -> PathBuf {
    // <repo>/crates/contract → <repo>
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(|p| p.parent())
        .expect("crate 应当位于 <repo>/crates/contract")
        .to_path_buf()
}

fn read(rel: &str) -> String {
    let p = repo_root().join(rel);
    std::fs::read_to_string(&p).unwrap_or_else(|e| panic!("读不到 {}：{}", p.display(), e))
}

/// 文本里是否"提到"了这个键（`name:` / `.name` / `"name"` / `name,` … 都算）。
///
/// 故意宽松：这里要证明的是"这份镜像确实在读这个键"，不是做 JS 语法分析。
fn mentions(src: &str, key: &str) -> bool {
    let is_ident = |b: u8| b.is_ascii_alphanumeric() || b == b'_' || b == b'$';
    let bytes = src.as_bytes();
    let mut from = 0;
    while let Some(pos) = src[from..].find(key) {
        let i = from + pos;
        let j = i + key.len();
        let before = i == 0 || !is_ident(bytes[i - 1]);
        let after = j >= bytes.len() || !is_ident(bytes[j]);
        if before && after {
            return true;
        }
        from = i + 1;
    }
    false
}

/// **产物新鲜度**：`contract/*.json` 必须与刚生成的一模一样。
///
/// 产物是缓存，不是真相；真相在 Rust 类型里。所以这条测试失败时的正确动作是
/// **重新生成**，而不是改产物。
#[test]
fn committed_artifacts_match_the_source() {
    let dir = repo_root().join("contract");
    let mut stale = Vec::new();
    for (name, want) in contract::artifacts() {
        let p = dir.join(name);
        match std::fs::read_to_string(&p) {
            Ok(have) if have == want => {}
            Ok(_) => stale.push(format!("{} 与 Rust 类型不一致", p.display())),
            Err(e) => stale.push(format!("{} 读不到（{}）", p.display(), e)),
        }
    }
    assert!(
        stale.is_empty(),
        "契约产物已过期：\n  {}\n重新生成：\n  cargo run -p rsi3d-harness-cli -- contract --out contract",
        stale.join("\n  ")
    );
}

/// **镜像门禁**：手抄的 JS 里读的每个键，都必须是派生出来的契约键。
///
/// 表是手写的（JS 那边没有类型系统，抓不出它的读法），但**每一行都被双向检查**：
/// 键既要在文件里真的被提到，也要在契约里真的存在。Rust 侧改名 → 这里红；
/// JS 侧自己发明了个键 → 这里也红。
const MIRRORS: &[(&str, &str, &[&str])] = &[
    // 浏览器客户端：SSE 增量、握手、帧、以及 glTF 节点上的 rsi3d 扩展
    (
        "crates/serve/src/client.js",
        "stream_server",
        &[
            "revision",
            "scene_hash",
            "resumed",
            "image_hash",
            "png_base64",
            "band_occlusion",
            "changes",
            "nodes_upsert",
            "nodes_remove",
            "blockers",
            "gltf",
            "view",
            "message",
            "name",
            "translation",
            "scale",
            "extras",
            "rsi3d",
            "color",
            "geometry",
            "material",
            "to",
            "height",
        ],
    ),
    // 渲染模式：客户端上报的主机参数 → 判定 → 按判定改自己的行为
    (
        "crates/serve/src/client.js",
        "render_verdict",
        &[
            "mode",
            "limits",
            "max_px",
            "max_fps",
            "geometry",
            "stream",
            "allow_animation",
            "reasons",
            "missing",
            "fallbacks",
            "title",
            "what",
            "link",
            "authority",
            "policy_version",
        ],
    ),
    (
        "crates/serve/src/client.js",
        "host_profile",
        &["agent", "form", "gpu", "cpu", "display", "bench", "privacy", "software", "max_texture"],
    ),
    (
        "crates/serve/src/client.js",
        "render_policy",
        &["version", "modes", "fallbacks", "max_px", "allow_animation"],
    ),
    (
        "crates/serve/src/client.js",
        "gltf_scene_extras",
        &["rsi3d", "window", "spanX", "z", "height", "room", "size", "window_blockers"],
    ),
    (
        "crates/serve/src/client.js",
        "scene",
        &["window", "spanX", "z", "height", "room", "size"],
    ),
    // 客户端**发出去**的命令信封：op/target/params/reason 一个都不能改名
    (
        "crates/serve/src/client.js",
        "stream_client",
        &["op", "target", "params", "reason", "token"],
    ),
    // 脚手架里的假引擎：出厂演示就是这么读场景的
    (
        "scaffolds/agent-app/files/mock-engine.mjs",
        "scene",
        &[
            "objects",
            "id",
            "role",
            "material",
            "aabb",
            "min",
            "max",
            "window",
            "spanX",
            "height",
            "z",
            "bandDepth",
            "room",
            "size",
            "lights",
            "intensity",
            "color",
            "clearance_rules",
            "pair",
            "intent_keywords",
            "word",
            "present",
            "units",
            "intent",
        ],
    ),
    // 评测插件：只读 aabb 与 role 就够了
    (
        "scaffolds/harness-plugin/files/src/evaluator.mjs",
        "scene",
        &["objects", "id", "role", "material", "aabb", "min", "max"],
    ),
];

#[test]
fn mirrors_only_reference_known_keys() {
    let mut problems = Vec::new();
    let mut checked = 0usize;
    for (file, block, keys) in MIRRORS {
        let src = read(file);
        for key in *keys {
            checked += 1;
            if !mentions(&src, key) {
                problems.push(format!("{}：表里说它读 {}，但文件里没提到", file, key));
            }
            if !contract::has_key(block, key) {
                problems.push(format!(
                    "{}：{} 不在契约块 {} 里（Rust 侧改了名？）",
                    file, key, block
                ));
            }
        }
    }
    assert!(
        problems.is_empty(),
        "镜像与契约不一致（{} 个键检查过）：\n  {}",
        checked,
        problems.join("\n  ")
    );
    assert!(checked >= 60, "镜像表太薄了（只检查了 {} 个键）", checked);
}

/// **消息类型**：服务端能发的每种 `type`，浏览器客户端都得有分支。
///
/// 客户端用的是 SSE **具名事件**（`es.addEventListener('snapshot', …)`），事件名就是消息的
/// `type` 值——所以改一个变体名，浏览器那边就会**静默忽略整条消息**，表现是「页面不动了」，
/// 最难查的那种。这里把每个类型都攼住：要么客户端处理了，要么写在这里说明为何不处理。
const UNHANDLED_BY_DESIGN: &[(&str, &str)] = &[
    ("pong", "心跳回包，客户端不需要做任何事"),
    ("error", "客户端走 EventSource 的 onerror，不在具名分支里"),
    ("bye", "服务端告别，之后 EventSource 自己重连"),
];

#[test]
fn client_handles_every_server_message_type() {
    let src = read("crates/serve/src/client.js");
    let kinds = contract::enums()["stream_server_message"]
        .as_array()
        .expect("enums 里应当有 stream_server_message")
        .clone();
    let mut unaccounted = Vec::new();
    let mut handled = Vec::new();
    for k in &kinds {
        let name = k.as_str().unwrap();
        if mentions(&src, name) {
            handled.push(name.to_string());
        } else if !UNHANDLED_BY_DESIGN.iter().any(|(n, _)| *n == name) {
            unaccounted.push(name.to_string());
        }
    }
    assert!(
        unaccounted.is_empty(),
        "这些消息类型既没被客户端处理，也没在 UNHANDLED_BY_DESIGN 里说明：{:?}",
        unaccounted
    );
    assert!(
        handled.len() >= 4,
        "只认出了 {} 种被处理的消息，太少（客户端应该至少处理 welcome/snapshot/patch/frame）",
        handled.len()
    );
}

/// 我们自己的场景里**有意**放进 extras 的键（不是拼错）。
///
/// 为什么需要这张表：`extras` 用 serde 的 flatten 实现，**任何**键都收。所以"未知键"这个
/// 信号天生有两种读法：拼错了，或者有意扩展。从 JSON 上分不出来，只能由人声明。
/// 表里每一条都得写淸为什么——它同时是审计记录。
const DELIBERATE_EXTRAS: &[(&str, &str)] = &[
    ("title", "场景包元数据：给目录页显示用的标题（内核不关心）"),
    ("id", "场景包元数据：包内稳定 id"),
    ("notes", "作者备注，不参与任何测量"),
    ("lights[].note", "灯具上的人读备注：为什么这么打光"),
    ("lights[].file", "HDRI 资产引用（内核不做资产解析）"),
    ("objects[].note", "节点上留给 RSI 循环的待修备注"),
];

/// **出厂场景**：我们随脚手架发出去的 `scene.json` 必须 ①合契约 ②过内核 ③声明过的不许变。
///
/// 第 ③ 条是这里最狠的：把文件交给内核读进来再写回去，**文件里写过的每个键都得原值保留**。
/// 如果 `bandDepth` 拼成了 `band_depth`，内核会把它吞进 extras、把 bandDepth 悄悄用回默认值
/// ——**读起来不报错，写回去的值却变了**，这一条就会红。
///
/// 反方向不管：内核会把默认值（如节点 `layers`）补全，那是它的权利，不是文件写错。
#[test]
fn shipped_scenes_conform_to_the_contract() {
    let files = [
        "scaffolds/agent-app/files/scene.json",
        "scaffolds/harness-plugin/files/fixtures/scene.json",
        "scaffolds/pack/files/scenes/living-room.json",
    ];
    let mut problems = Vec::new();
    for rel in files {
        let text = read(rel);
        let json: Value = serde_json::from_str(&text)
            .unwrap_or_else(|e| panic!("{} 不是合法 JSON：{}", rel, e));

        // ① 契约层：未知键 / 类型不符（有意 extras 白名单外的都算问题）
        for f in contract::validate_scene_json(&json) {
            if f.kind == FindingKind::TypeMismatch {
                problems.push(format!("{}：{}", rel, f));
                continue;
            }
            if !DELIBERATE_EXTRAS.iter().any(|(p, _)| *p == f.pattern()) {
                problems.push(format!("{}：{}", rel, f));
            }
        }

        // ② 内核层：必须能读
        let scene = match Scene::from_json(&text) {
            Ok(s) => s,
            Err(e) => {
                problems.push(format!("{}：内核读不了——{}", rel, e));
                continue;
            }
        };

        // ③ 声明过的内容不许变
        let back = serde_json::to_value(&scene).expect("Scene 必须可序列化");
        if let Some(diff) = lost_content(&json, &back, "$") {
            problems.push(format!(
                "{}：过内核一趟后内容变了（{}）——多半有键被 extras 吞了",
                rel, diff
            ));
        }
    }
    assert!(
        problems.is_empty(),
        "出厂场景不合契约：\n  {}",
        problems.join("\n  ")
    );
}

/// 找出"原件里写了、写回去却变了样"的第一处。
///
/// 只查一个方向：内核**多**补出默认值不管（那是它的权利），
/// 但**原件写过的键与值必须原样还在**。
fn lost_content(original: &Value, back: &Value, path: &str) -> Option<String> {
    match (original, back) {
        (Value::Object(x), Value::Object(y)) => {
            for (k, vx) in x {
                match y.get(k) {
                    Some(vy) => {
                        if let Some(d) = lost_content(vx, vy, &format!("{}.{}", path, k)) {
                            return Some(d);
                        }
                    }
                    None => {
                        return Some(format!(
                            "{}：这个键过内核后不见了（被吞进 extras 却没写回来？）",
                            join_path(path, k)
                        ))
                    }
                }
            }
            None
        }
        (Value::Array(x), Value::Array(y)) => {
            if x.len() != y.len() {
                return Some(format!("{}：数组长度 {} → {}", path, x.len(), y.len()));
            }
            for (i, (vx, vy)) in x.iter().zip(y).enumerate() {
                if let Some(d) = lost_content(vx, vy, &format!("{}[{}]", path, i)) {
                    return Some(d);
                }
            }
            None
        }
        _ if original == back => None,
        _ => Some(format!(
            "{}：值被换掉了（{} → {}）——典型症状是键拼错了，内核用回了默认值",
            path, original, back
        )),
    }
}

fn join_path(path: &str, key: &str) -> String {
    if path == "$" {
        key.to_string()
    } else {
        format!("{}.{}", path, key)
    }
}

/// **能力词汇表**：客户端声明的东西必须有单一出处（`crates/stream` 的 const），
/// 而且每个名字都得在对外文档里写清楚——否则运维在 `/healthz` 里看到 `webgl1`
/// 之类的字，却没人知道它意味着什么。
///
/// 表是手写的（JS 与 markdown 没有类型系统），但每一行都被双向检查。
const CAPABILITY_USERS: &[(&str, &[&str])] = &[
    // 浏览器客户端：它声明的必须是词汇表里的名字
    (
        "crates/serve/src/client.js",
        &["scene", "image", "three", "webgl2", "webgl1", "context-loss"],
    ),
    // 命令行客户端：没有屏幕，只能消费流
    (
        "crates/native/src/serve_cmd.rs",
        &["headless", "scene", "image"],
    ),
];

#[test]
fn capability_vocabulary_is_declared_everywhere_that_matters() {
    let vocab: Vec<String> = contract::enums()["client_capability"]
        .as_array()
        .expect("enums 里应当有 client_capability")
        .iter()
        .map(|v| v.as_str().unwrap().to_string())
        .collect();
    assert!(vocab.contains(&"webgl2".to_string()), "{:?}", vocab);

    let mut problems = Vec::new();
    for (file, declared) in CAPABILITY_USERS {
        let src = read(file);
        for name in *declared {
            if !vocab.contains(&name.to_string()) {
                problems.push(format!("{} 声明了词汇表里没有的能力名 {}", file, name));
            }
            if !mentions(&src, name) {
                problems.push(format!("{} 的表里说它声明 {}，但文件里找不到", file, name));
            }
        }
    }

    // 文档必须解释**每一个**能力名（词汇表变了、文档没跟上 → 红）
    let docs = read("docs/stream.md");
    for name in &vocab {
        if !docs.contains(name.as_str()) {
            problems.push(format!("docs/stream.md 没解释能力名「{}」", name));
        }
    }

    assert!(
        problems.is_empty(),
        "能力词汇表与使用处不一致：\n  {}",
        problems.join("\n  ")
    );
}

/// **帧来源档**：每一帧都要能说清"谁渲的、能不能当证据"，且这段话必须在对外文档里
/// 写明白——否则客户端看到 `renderer: "..."` 也不知道该不该拿它去对账。
#[test]
fn frame_renderers_are_evidence_tagged_and_documented() {
    let grades = contract::enums()["frame_renderer"]
        .as_array()
        .expect("enums 里应当有 frame_renderer")
        .clone();
    assert!(!grades.is_empty(), "至少得有一个来源档");

    let evidence: Vec<&serde_json::Value> = grades
        .iter()
        .filter(|g| g["evidence"] == serde_json::json!(true))
        .collect();
    assert_eq!(evidence.len(), 1, "证据档只能有一个：{:?}", grades);

    let docs = read("docs/stream.md");
    let mut problems = Vec::new();
    for g in &grades {
        let name = g["name"].as_str().unwrap();
        if !docs.contains(name) {
            problems.push(format!("docs/stream.md 没解释帧来源档「{}」", name));
        }
    }
    assert!(
        problems.is_empty(),
        "帧来源档与文档不一致：\n  {}",
        problems.join("\n  ")
    );
}

/// **样板文件本身得是能用的**：它能被内核读、能被契约校验器通过、写回去不丢东西。
///
/// 这一条防的是"示范文件自己就是错的"——外部作者会照抄它。
#[test]
fn scene_example_is_accepted_by_the_kernel() {
    let example = contract::scene_example();
    let text = serde_json::to_string(&example).unwrap();
    let scene = Scene::from_json(&text).expect("样板必须能被内核读进去");
    let back = serde_json::to_value(&scene).unwrap();

    assert!(
        lost_content(&example, &back, "$").is_none(),
        "样板过内核一趟就变了：{:?}",
        lost_content(&example, &back, "$")
    );
    assert!(
        contract::validate_scene_json(&example).is_empty(),
        "样板自己没过契约校验：{:?}",
        contract::validate_scene_json(&example)
    );
}

// ---------------------------------------------------------------- 跨语言镜像（Go）

/// Go 那边的判定结构是**手抄的镜像**（不同语言，没有编译期门禁），所以用文本门禁。
///
/// 为什么值得单独一条：`policy_version` 拼成 `policyVersion`、`max_px` 拼成 `maxPx`，
/// 两边都编译得过、测试也可能过——但浏览器拿到的 verdict 里那个字段**是空的**，
/// 于是"像素预算"永远不生效。这类漂移只能靠点名检查。
#[test]
fn go_side_mirrors_the_same_field_names() {
    let repo = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("..")
        .join("rsi3d-online/server/internal/render/render.go");
    let Ok(src) = std::fs::read_to_string(&repo) else {
        println!(
            "跳过：读不到 {}（平台不在此容器里？跨工程对齐另有平台侧用例）",
            repo.display()
        );
        return;
    };
    // 判定与规则的**跨语言字段名**：少一个，客户端就会静默拿到空字段
    let required = [
        "mode",
        "limits",
        "max_px",
        "max_fps",
        "geometry",
        "stream",
        "allow_animation",
        "reasons",
        "missing",
        "fallbacks",
        "authority",
        "policy_version",
        "require_gpu",
        "min_cores",
        "min_sustained_fps",
        "min_max_texture",
        "min_viewport",
        "allow_software",
        "when_at_or_below",
    ];
    let mut missing = Vec::new();
    for key in required {
        if !src.contains(&format!("json:\"{}\"", key)) {
            missing.push(key);
        }
    }
    assert!(
        missing.is_empty(),
        "平台侧 render.go 少了这些 json 标签：{:?}\n（字段名对不上，客户端会拿到空值）",
        missing
    );
}
