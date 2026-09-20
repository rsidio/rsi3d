//! 类型派生的 schema：两道闸。
//!
//! 这一层换来源的时候最容易出的事**不是崩**，而是"schema 悄悄变弱了"：
//! `required` 少写一个字段、某个具名字段从 `properties` 里消失、某个 `anyOf` 分支
//! 被抹掉——文件照样合法，外部作者照样能读，只是约束**不再约束**。
//!
//! 所以两道闸各守一边：
//!
//! 1. **不能太严**：内核自己写出来的样例、以及出厂的 `scene.json`，必须全部通过这份
//!    schema（拿**成熟校验器** `jsonschema` 验，不是拿我们自己的代码验）。`required`
//!    写错、`Option` 处理错，这里会红。
//! 2. **不能太松（防缩水）**：样例观察出来的键清单里，**具名字段**那部分必须仍然在
//!    schema 的 `properties` 里，且数量不得低于钉住的下限。让来源从"观察"切到"类型"
//!    时，这条是唯一能证明"没有少东西"的证据。
//!
//! 为什么键清单不一起换成类型派生：它喂的是 `mirrors_only_reference_known_keys` 那类
//! 镜像门禁，而那些门禁依赖清单的**写法**（嵌套路径 + 裸名）。换来源要先把写法对齐，
//! 这就是第 2 道闸在量的事——量完了再谈换。

use std::collections::BTreeSet;

use serde_json::Value;

/// 样例观察出来的键清单（`keys.json` 里那个，`block` 来自 lib）。
fn observed(block: &str) -> BTreeSet<String> {
    let ks = rsi3d_harness_contract::known_keys(block);
    ks.into_iter().collect()
}

/// 从类型派生的 schema 里能走出来的东西。
///
/// 口径要和样例派生的键清单**对齐**（见 `known_keys` 的输出）：数组不带 `[]`，
/// 且每一段的名字都单独算一个"裸名"（`objects.aabb.max` 同时给出 `aabb`、`max`）。
struct Typed {
    /// 点路径 + 裸名，合在一个集合里
    named: BTreeSet<String>,
    /// 开放对象（`additionalProperties: true`）的路径——`extras` 就落在这里
    open: Vec<String>,
}

fn resolve<'a>(schema: &'a Value, root: &'a Value) -> &'a Value {
    if let Some(r) = schema.get("$ref").and_then(|v| v.as_str()) {
        if let Some(name) = r.strip_prefix("#/$defs/") {
            if let Some(def) = root.pointer(&format!("/$defs/{}", name)) {
                return def;
            }
        }
    }
    schema
}

fn walk(schema: &Value, root: &Value, path: &str, out: &mut Typed, depth: usize) {
    if depth > 24 {
        return; // 自引用类型：走深了没意义（`$defs` 会各自展开）
    }
    let node = resolve(schema, root);
    for k in ["anyOf", "oneOf", "allOf"] {
        if let Some(arr) = node.get(k).and_then(|v| v.as_array()) {
            for b in arr {
                walk(b, root, path, out, depth + 1);
            }
        }
    }
    if let Some(items) = node.get("items") {
        walk(items, root, path, out, depth + 1);
    }
    if let Some(props) = node.get("properties").and_then(|v| v.as_object()) {
        let is_open = node
            .get("additionalProperties")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);
        if is_open {
            out.open.push(path.to_string());
        }
        for (k, v) in props {
            let p = if path.is_empty() {
                k.clone()
            } else {
                format!("{}.{}", path, k)
            };
            out.named.insert(p.clone());
            out.named.insert(k.clone()); // 裸名（与 keys.json 的口径一致）
            walk(v, root, &p, out, depth + 1);
        }
    }
}

fn typed(schema: &Value) -> Typed {
    let mut out = Typed {
        named: BTreeSet::new(),
        open: Vec::new(),
    };
    walk(schema, schema, "", &mut out, 0);
    out
}

fn schema_of(name: &str) -> Value {
    let (_, body) = rsi3d_harness_contract::artifacts()
        .into_iter()
        .find(|(n, _)| *n == name)
        .unwrap_or_else(|| panic!("没有这份产物：{}", name));
    serde_json::from_str(&body).expect("产物应当是合法 JSON")
}

// ---------------------------------------------------------------- 闸 1：不能太严

/// 内核自己写出来的东西，必须全部通过这份 schema。
///
/// 用成熟的 `jsonschema`（和 Tauri / Apollo Router 用的是同一个）来验，不用我们自己的
/// 代码验——自己写的校验器验自己派生的 schema，等于自己给自己判卷。
#[test]
fn typed_schema_accepts_everything_the_kernel_writes() {
    let scene_schema = schema_of("scene.schema.json");
    let validator = jsonschema::validator_for(&scene_schema).expect("schema 本身得合法");

    let mut checked = 0;
    let mut cases: Vec<(String, Value)> = vec![
        (
            "full_scene()".into(),
            serde_json::to_value(rsi3d_harness_contract::full_scene()).unwrap(),
        ),
        (
            "minimal_scene()".into(),
            serde_json::to_value(rsi3d_harness_contract::minimal_scene()).unwrap(),
        ),
        ("scene.example.json".into(), schema_of("scene.example.json")),
    ];
    // 出厂场景：把脚手架里所有 scene.json 都过一遍（它们是真会被发出去的文件）
    for entry in walk_json(&std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../scaffolds"))
    {
        if entry.file_name().and_then(|s| s.to_str()) == Some("scene.json") {
            let text = std::fs::read_to_string(&entry).unwrap();
            cases.push((
                entry.display().to_string(),
                serde_json::from_str(&text).unwrap(),
            ));
        }
    }

    let mut bad = Vec::new();
    for (name, instance) in &cases {
        checked += 1;
        let errs: Vec<String> = validator
            .iter_errors(instance)
            .map(|e| format!("{} at {}", e, e.instance_path()))
            .collect();
        if !errs.is_empty() {
            bad.push(format!("{}：{:?}", name, errs));
        }
    }
    assert!(bad.is_empty(), "schema 比内核严了（这些是内核自己写的）：{:#?}", bad);
    assert!(checked >= 4, "只验了 {} 份，样本太窄", checked);
    println!("闸 1：{} 份内核产物全部通过类型派生的 schema", checked);
}

fn walk_json(dir: &std::path::Path) -> Vec<std::path::PathBuf> {
    let mut out = Vec::new();
    let Ok(rd) = std::fs::read_dir(dir) else {
        return out;
    };
    for e in rd.flatten() {
        let p = e.path();
        if p.is_dir() {
            out.extend(walk_json(&p));
        } else {
            out.push(p);
        }
    }
    out
}

// ---------------------------------------------------------------- 闸 2：防缩水

/// 样例观察出来的**具名字段**，必须仍然是 schema 里的具名属性，且数量不得低于下限。
///
/// 落在开放对象（`extras`）下面的键**不算**具名——那是有意开放的：`extras` 用 serde
/// flatten 实现，schema 天生列不出来。所以这门闸只数"能被列出来"的那部分。
#[test]
fn typed_schema_does_not_shrink_the_observed_named_fields() {
    // 下限：写死一个数，缩水就红。加字段不会红（上限不封），掉字段会。
    const NAMED_FLOOR: usize = 85;

    let mut report = Vec::new();
    for (block, artifact) in [("scene", "scene.schema.json"), ("view", "view.schema.json")] {
        let t = typed(&schema_of(artifact));
        let obs = observed(block);

        // 第一遍：点路径。落在开放对象下的算 extras（extras 用 serde flatten，
        // schema 天生列不出来——那是**有意**开放的，不是漏了）。
        let mut named = 0;
        let mut extras_keys: Vec<String> = Vec::new();
        let mut unexplained = Vec::new();
        for k in &obs {
            if t.named.contains(k) {
                named += 1;
            } else if t
                .open
                .iter()
                .any(|p| k == p || k.starts_with(&format!("{}.", p)))
            {
                extras_keys.push(k.clone());
            } else {
                unexplained.push(k.clone());
            }
        }
        // 第二遍：裸名。它只要出现在某个 extras 路径的任何一段上，就算 extras。
        let segs: BTreeSet<&str> = extras_keys
            .iter()
            .flat_map(|k| k.split('.'))
            .collect();
        let extra_names: Vec<String> = obs
            .iter()
            .filter(|k| !k.contains('.') && segs.contains(k.as_str()))
            .cloned()
            .collect();
        // 这些裸名是第一遍判成"讲不通"的，第二遍把它们认领回 extras
        unexplained.retain(|k| !extra_names.contains(k));
        let extra_count = extras_keys.len() + extra_names.len();

        report.push(format!(
            "{}：观察 {} 键 → 具名 {} · extras（开放对象下）{} · 讲不通 {}",
            block,
            obs.len(),
            named,
            extra_count,
            unexplained.len()
        ));
        assert!(
            unexplained.is_empty(),
            "{} 里有讲不通的键（既不在 schema 里，也不在任何开放对象下）：{:#?}",
            block,
            unexplained
        );
        if block == "scene" {
            assert!(
                named >= NAMED_FLOOR,
                "scene 的具名字段从 {} 掉到 {}（下限 {}）——schema 缩水了",
                NAMED_FLOOR,
                named,
                NAMED_FLOOR
            );
        }
    }
    for line in &report {
        println!("闸 2：{}", line);
    }
}

/// 顺带钉住：schema 里该有的**约束**真的在（不然这份 schema 就只是一张属性表）。
#[test]
fn typed_schema_actually_constrains() {
    let s = schema_of("scene.schema.json");
    let text = serde_json::to_string(&s).unwrap();
    assert!(text.contains("\"$defs\""), "具名类型应当出现在 $defs 里");
    assert!(text.contains("\"required\""), "应当有 required（观察法给不出这个）");
    assert!(text.contains("\"default\""), "serde 默认值应当变成 schema 的 default");
    assert!(
        text.contains("\"oneOf\"") || text.contains("\"anyOf\""),
        "枚举/可选应当表达成 oneOf/anyOf"
    );
    // 字段文档来自 Rust doc comment：外部作者读 schema 时看的就是它
    assert!(
        text.contains("场景 = 一棵节点树"),
        "Rust doc comment 应当出现在 description 里"
    );
    assert!(s.get("x-known-keys").is_some(), "键清单标记不能丢");
    assert_eq!(
        s.get("$schema").and_then(|v| v.as_str()),
        Some("https://json-schema.org/draft/2020-12/schema")
    );
}
