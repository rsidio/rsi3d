//! 渲染模式判定的**行为语料**：Rust 与 Go 各跑一遍同一份用例，结论必须一致。
//!
//! 为什么需要这道闸：方案是"联网时问 rsi3d.com，离线时本地算"（见 `render_mode.rs`
//! 顶部）。如果两侧的判定规则各写各的，同一台机器会得到两个档位——用户在办公室是满档、
//! 回家就降档，而没人知道为什么。规则表本身是**数据**（`crates/stream/policy/render-policy.json`），
//! 但**求值**是两段代码（Rust 与 Go），所以用同一份语料把两边钉在一起：
//! 加规则、改阈值时，要么两边一起改，要么这道闸变红。
//!
//! 语料在 `crates/stream/policy/render-policy.corpus.json`；平台侧的同名用例会读同一个文件。

use std::path::PathBuf;

use rsi3d_harness_stream::render_mode::{assess, default_policy, RenderVerdict};

fn corpus_path() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("policy/render-policy.corpus.json")
}

fn load() -> serde_json::Value {
    let text = std::fs::read_to_string(corpus_path())
        .unwrap_or_else(|e| panic!("读不到语料 {}：{}", corpus_path().display(), e));
    serde_json::from_str(&text).expect("语料应当是合法 JSON")
}

#[test]
fn every_corpus_case_gets_the_expected_mode() {
    let corpus = load();
    assert_eq!(
        corpus["spec"], "rsi3d-render-policy-corpus/v1",
        "语料格式变了就要改这个断言（别让文件悄悄换形状）"
    );
    let cases = corpus["cases"].as_array().expect("cases 应当是数组");
    assert!(cases.len() >= 6, "语料太窄（{} 例）——覆盖不住就是没闸", cases.len());

    let policy = default_policy();
    let mut checked = 0;
    for c in cases {
        let name = c["name"].as_str().unwrap_or("(无名)");
        let profile: rsi3d_harness_stream::render_mode::HostProfile =
            serde_json::from_value(c["profile"].clone())
                .unwrap_or_else(|e| panic!("{} 的 profile 解析不了：{}", name, e));
        let v: RenderVerdict = assess(&profile, &policy, "local");
        let want = &c["expect"];

        assert_eq!(
            v.mode,
            want["mode"].as_str().unwrap(),
            "{}：模式不对（理由 {:?}）",
            name,
            v.reasons
        );
        if let Some(g) = want["geometry"].as_str() {
            assert_eq!(v.limits.geometry, g, "{}：几何档不对", name);
        }
        if let Some(s) = want["stream"].as_str() {
            assert_eq!(v.limits.stream, s, "{}：订阅哪条流不对", name);
        }
        if let Some(f) = want["max_fps"].as_u64() {
            assert_eq!(v.limits.max_fps as u64, f, "{}：帧率上限不对", name);
        }
        if let Some(has) = want["has_fallbacks"].as_bool() {
            // 满档不必给出路；一旦降档就必须给
            assert_eq!(
                !v.fallbacks.is_empty(),
                has,
                "{}：降档必须给得出路，满档不必（实际 {} 条）",
                name,
                v.fallbacks.len()
            );
        }
        if let Some(needle) = want["reason_contains"].as_str() {
            assert!(
                v.reasons.iter().any(|r| r.contains(needle)),
                "{}：理由里应当提到「{}」，实际 {:?}",
                name,
                needle,
                v.reasons
            );
        }
        if let Some(needle) = want["missing_contains"].as_str() {
            assert!(
                v.missing.iter().any(|m| m.contains(needle)),
                "{}：\"缺什么\"里应当点到「{}」，实际 {:?}",
                name,
                needle,
                v.missing
            );
        }
        checked += 1;
    }
    println!("语料 {} 例全部通过（离线判定）", checked);
}
