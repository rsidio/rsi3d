//! 规则表作为**产物**发出去时，必须与两处消费者手里的一致。
//!
//! 三个握有规则的地方：① harness 内置的那份（离线判定用）② `contract/render-policy.json`
//! （`rsi3d-harness contract --out` 写出、服务端 `/contract/render-policy.json` 直接发）
//! ③ 平台 `/api/render/policy`（读的也是 ②，平台侧另有 parity 用例）。
//!
//! ①≠② 的后果很具体：用户在网页上看到"满档"，回家离线跑却是另一档，而没人知道为什么。

#[test]
fn published_policy_artifact_matches_the_builtin_rule_table() {
    let (_, body) = rsi3d_harness_contract::artifacts()
        .into_iter()
        .find(|(n, _)| *n == "render-policy.json")
        .expect("render-policy.json 应当是契约产物之一");
    let published: serde_json::Value = serde_json::from_str(&body).unwrap();
    let builtin: serde_json::Value = serde_json::from_str(&serde_json::to_string(
        &rsi3d_harness_stream::render_mode::default_policy(),
    )
    .unwrap())
    .unwrap();
    assert_eq!(
        published, builtin,
        "发出去的规则表与内置的不一致——离线判定会和服务端判定分叉"
    );

    // 产物里该有的都还在（别让"一致"变成"一起空掉"）
    let text = body;
    for needle in ["client-full", "client-lite", "client-minimal", "frame-only", "headless"] {
        assert!(text.contains(needle), "规则表里少了 {} 档", needle);
    }
    assert!(text.contains("fallbacks"), "规则表要带得出路");
}
