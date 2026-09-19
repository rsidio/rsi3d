//! 把模型渲染成**人/Agent 能读的文本**与**稳定的 JSON**。
//!
//! # 为什么这一层在 `core` 里
//!
//! 因为 CLI 与 MCP 必须给出**同一份**观察结果。如果各写一份渲染，
//! 就会出现「命令行说间距 0.2m、Agent 那边说 0.7m」这种最难查的分歧。
//!
//! 这里的「渲染」是**文本/JSON 序列化**，与 3D 渲染无关——`core` 依然不含任何图形代码。
//! 两个壳（`crates/native` / `crates/mcp`）只负责打印或包进协议报文。

use serde_json::{json, Value};

use crate::command::{Applied, AttributionRow, CommandRequest};
use crate::document::{Diff, Document};
use crate::error::Result;
use crate::scene::{Aabb, ClearanceRule, Scene};
use crate::validate::Warning;

// ---------------------------------------------------------------- 小工具

/// 数值打印：整数不带小数点，其余两位小数。
pub fn fmt_num(v: f64) -> String {
    if (v - v.round()).abs() < 1e-9 {
        format!("{}", v.round() as i64)
    } else {
        format!("{:.2}", v)
    }
}

/// 包围盒打印：`2×0.85×0.90 @ [-1.10, 0, -2.60]`。
pub fn fmt_aabb(a: &Aabb) -> String {
    let s = a.size();
    format!(
        "{}×{}×{} @ [{}, {}, {}]",
        fmt_num(s[0]),
        fmt_num(s[1]),
        fmt_num(s[2]),
        fmt_num(a.min[0]),
        fmt_num(a.min[1]),
        fmt_num(a.min[2])
    )
}

/// 当前场景下某条间距规则的实测间距。
pub fn rule_gap(scene: &Scene, rule: &ClearanceRule) -> Option<f64> {
    let a = scene.node(&rule.pair[0])?;
    let b = scene.node(&rule.pair[1])?;
    Some(a.aabb.gap(&b.aabb))
}

/// 一组告警的紧凑文本（每条一行）。
pub fn warnings_text(warnings: &[Warning]) -> String {
    warnings
        .iter()
        .map(|w| format!("  [{}] {}  ({})", w.code, w.message, w.nodes.join(", ")))
        .collect::<Vec<_>>()
        .join("\n")
}

// ---------------------------------------------------------------- 观察

/// 场景摘要（节点表 / 灯光 / 规则 / 挡窗 / 告警 / 哈希）。
///
/// 这份文本是给**模型**读的，所以它刻意写得像一份"现场报告"而不是 JSON：
/// 模型需要一眼看出「哪里不对、哪些东西我不能动」。
pub fn scene_summary(doc: &Document) -> String {
    let scene = doc.scene();
    let mut lines = vec![format!(
        "{}  rev {}  单位 {}  上轴 {}\n房间 {}",
        scene.spec,
        doc.revision(),
        scene.units,
        scene.up_axis,
        scene
            .room
            .as_ref()
            .map(|r| format!(
                "{}×{}×{} m",
                fmt_num(r.size[0]),
                fmt_num(r.size[1]),
                fmt_num(r.size[2])
            ))
            .unwrap_or_else(|| "（未声明）".into())
    )];

    if let Some(w) = &scene.window {
        lines.push(format!(
            "  窗 {}  x[{}, {}]  高 {}  z {}  挡光带 {}m",
            w.wall.clone().unwrap_or_else(|| "?".into()),
            fmt_num(w.span_x[0]),
            fmt_num(w.span_x[1]),
            fmt_num(w.height),
            fmt_num(w.z),
            fmt_num(w.band_depth)
        ));
    }

    let solid = scene
        .objects
        .iter()
        .filter(|n| n.role_kind().is_solid())
        .count();
    lines.push(format!(
        "\n节点 {}（其中 solid {}）",
        scene.objects.len(),
        solid
    ));
    for n in &scene.objects {
        lines.push(format!(
            "  {:<18} {:<10} {:<8} {:<26} {}",
            n.id,
            n.role,
            n.material.clone().unwrap_or_else(|| "-".into()),
            fmt_aabb(&n.aabb),
            n.editability()
        ));
    }

    if !scene.lights.is_empty() {
        lines.push(format!("\n灯光 {}", scene.lights.len()));
        for l in &scene.lights {
            lines.push(format!(
                "  {:<10} {:<12} 强度 {}  {}",
                l.id,
                l.kind,
                fmt_num(l.intensity),
                l.color.clone().unwrap_or_else(|| "-".into())
            ));
        }
    }

    if !scene.clearance_rules.is_empty() {
        lines.push(format!("\n间距规则 {}", scene.clearance_rules.len()));
        for r in &scene.clearance_rules {
            let (gap, mark) = match rule_gap(scene, r) {
                Some(g) => {
                    let d = r.distance(g);
                    (
                        format!("当前 {}m", fmt_num(g)),
                        if d == 0.0 {
                            "✓".to_string()
                        } else {
                            format!("✗ 差 {}m", fmt_num(d))
                        },
                    )
                }
                None => ("目标缺失".to_string(), "✗".to_string()),
            };
            lines.push(format!(
                "  {} ↔ {}  要求 {}–{}m  {}  {}",
                r.pair[0],
                r.pair[1],
                fmt_num(r.min),
                fmt_num(r.max),
                gap,
                mark
            ));
            if !r.reason.is_empty() {
                lines.push(format!("      {}", r.reason));
            }
        }
    }

    let blockers = scene.window_blockers();
    lines.push(format!(
        "\n挡窗者 {}  {}",
        blockers.len(),
        if blockers.is_empty() {
            "✓ 通光".to_string()
        } else {
            blockers.join(", ")
        }
    ));

    let warnings = doc.warnings();
    lines.push(format!("\n告警 {}", warnings.len()));
    if !warnings.is_empty() {
        lines.push(warnings_text(&warnings));
    }
    lines.push(format!("\n场景哈希 {}", doc.scene_hash().unwrap_or_default()));
    lines.join("\n")
}

/// 一行状态摘要（rev / 游标 / 告警数 / 挡窗 / 哈希）。
pub fn status_line(doc: &Document) -> String {
    let blockers = doc.scene().window_blockers();
    format!(
        "rev {}  游标 {}  告警 {}  挡窗 {}  哈希 {}",
        doc.revision(),
        doc.cursor(),
        doc.warnings().len(),
        if blockers.is_empty() {
            "无".into()
        } else {
            blockers.join(", ")
        },
        doc.scene_hash().unwrap_or_default()
    )
}

/// 观察结果的稳定 JSON —— CLI `--json` 与 MCP `structuredContent` **同一份**。
pub fn observe_json(doc: &Document) -> Result<Value> {
    Ok(json!({
        "revision": doc.revision(),
        "cursor": doc.cursor(),
        "head": doc.head(),
        "scene_hash": doc.scene_hash()?,
        "warnings": doc.warnings(),
        "window_blockers": doc.scene().window_blockers(),
        // `view()` 就是已发布评测插件与 mock 引擎在读的那份 shape
        "view": doc.scene().view(),
    }))
}

// ---------------------------------------------------------------- 编辑

/// 一步编辑的文本报告（模型在循环里读的就是它）。
pub fn applied_text(applied: &Applied, reason: &str) -> String {
    let mut lines = vec![format!(
        "rev {}  {}  {}",
        applied.revision,
        applied.command.op(),
        applied.command.target()
    )];
    if !reason.is_empty() {
        lines.push(format!("      理由 {}", reason));
    }
    if applied.new_warnings.is_empty() {
        lines.push("      新告警 无".into());
    } else {
        for w in &applied.new_warnings {
            lines.push(format!("      新告警 [{}] {}", w.code, w.message));
        }
    }
    lines.join("\n")
}

/// 一步编辑的 JSON（含逆命令——Agent 可以据此自己做回滚）。
pub fn applied_json(applied: &Applied) -> Value {
    json!({
        "revision": applied.revision,
        "op": applied.command.op(),
        "target": applied.command.target(),
        "inverse": CommandRequest::from_command(&applied.inverse, "", None),
        "new_warnings": applied.new_warnings,
        "scene_hash": applied.scene_hash,
    })
}

// ---------------------------------------------------------------- 历史 / 差异

/// 版本历史 + 归因表。
pub fn history_text(doc: &Document) -> String {
    let mut lines = vec![format!(
        "日志 {} 条  rev {}  游标 {}  历史前沿 {}",
        doc.oplog().len(),
        doc.revision(),
        doc.cursor(),
        doc.head()
    )];
    for e in doc.oplog() {
        lines.push(format!(
            "  rev {:<4} {:<12} {:<18} {}",
            e.rev,
            e.command.op(),
            e.command.target(),
            e.reason
        ));
        if !e.new_warnings.is_empty() {
            lines.push(format!(
                "       新告警 {}",
                e.new_warnings
                    .iter()
                    .map(|w| w.code.as_str())
                    .collect::<Vec<_>>()
                    .join(", ")
            ));
        }
        if let Some(x) = &e.expect {
            lines.push(format!("       预期 {}", x));
        }
    }
    lines.join("\n")
}

/// 归因表 JSON（`Δscore` 由 `eval` 层补，这里只给 Agent 的判断）。
pub fn attribution_json(rows: &[AttributionRow]) -> Value {
    json!({ "attribution": rows })
}

/// 两个版本之间的差异。
pub fn diff_text(d: &Diff) -> String {
    if d.is_empty() {
        return format!("rev {} 与 rev {} 完全相同", d.from, d.to);
    }
    let mut lines = vec![format!("rev {} → rev {}", d.from, d.to)];
    for id in &d.added {
        lines.push(format!("  + {}", id));
    }
    for id in &d.removed {
        lines.push(format!("  - {}", id));
    }
    for c in &d.changed {
        lines.push(format!(
            "  ~ {}  {}\n      {} → {}",
            c.id, c.what, c.before, c.after
        ));
    }
    lines.join("\n")
}

// ---------------------------------------------------------------- 可复现性自检

/// `verify` 的结果：文本 + 是否通过 + 结构化数据。
pub struct VerifyReport {
    pub text: String,
    pub ok: bool,
    pub json: Value,
}

/// 对文档做一遍可复现性自检：重放 / 全版本可重放 / 往返哈希 / 快照 / 游标。
pub fn verify_report(doc: &Document, source: &str) -> Result<VerifyReport> {
    let mut lines = vec![format!("文档 {}（rev {}）", source, doc.revision())];

    let replayed = doc.replay()?;
    let same = replayed == *doc.scene();
    lines.push(format!(
        "  {} 日志重放 == 落盘状态（{} 条命令）",
        if same { "✓" } else { "✗" },
        doc.oplog().len()
    ));

    // 每个版本都要能重放出来（日志里任何一条坏掉都会在这里暴露）
    let mut unplayable = Vec::new();
    for rev in 0..=doc.revision() {
        if doc.state_at(rev).is_err() {
            unplayable.push(rev);
        }
    }
    lines.push(format!(
        "  {} 全部 {} 个版本均可重放{}",
        if unplayable.is_empty() { "✓" } else { "✗" },
        doc.revision() + 1,
        if unplayable.is_empty() {
            String::new()
        } else {
            format!("（失败：{:?}）", unplayable)
        }
    ));

    // 落盘往返：哈希必须不变（否则「同一轨迹同哈希」不成立，进不了账本）
    let roundtrip_ok = (|| -> Result<bool> {
        let again = Document::from_json(&doc.to_json()?)?;
        Ok(again.scene_hash()? == doc.scene_hash()? && again.log_hash()? == doc.log_hash()?)
    })()
    .unwrap_or(false);
    lines.push(format!(
        "  {} 落盘往返后场景哈希与日志哈希不变",
        if roundtrip_ok { "✓" } else { "✗" }
    ));

    let mut bad = Vec::new();
    let mut checked = 0;
    for rev in doc.snapshot_revisions() {
        if rev == doc.revision() {
            continue;
        }
        if let Some(snap) = doc.snapshot(rev) {
            checked += 1;
            if *snap != doc.state_at(rev)? {
                bad.push(rev);
            }
        }
    }
    lines.push(format!(
        "  {} {} 个快照点与重放一致{}",
        if bad.is_empty() { "✓" } else { "✗" },
        checked,
        if bad.is_empty() {
            String::new()
        } else {
            format!("（不一致：{:?}）", bad)
        }
    ));

    let cursor_state = doc.state_at(doc.cursor())?;
    lines.push(format!(
        "  {} 游标 {}（state_at(cursor) == 当前场景）",
        if cursor_state == *doc.scene() { "✓" } else { "✗" },
        doc.cursor()
    ));
    lines.push(format!("  · 状态历史前沿 rev {}", doc.head()));

    let mut probe = doc.clone();
    match probe.undo() {
        Ok(a) => lines.push(format!(
            "  ✓ 可撤销：rev {} → rev {}（回滚也是命令，日志 append-only）",
            a.revision,
            probe.cursor()
        )),
        Err(e) => lines.push(format!("  ✗ 撤销失败：{}", e)),
    }

    let scene_hash = doc.scene_hash()?;
    let log_hash = doc.log_hash()?;
    lines.push(format!("\n场景哈希 {}", scene_hash));
    lines.push(format!("日志哈希 {}（不含墙钟，可进账本对账）", log_hash));

    let ok = same && unplayable.is_empty() && roundtrip_ok && bad.is_empty() && cursor_state == *doc.scene();
    let json = json!({
        "source": source,
        "ok": ok,
        "revision": doc.revision(),
        "cursor": doc.cursor(),
        "head": doc.head(),
        "log_entries": doc.oplog().len(),
        "replay_matches": same,
        "unplayable_revisions": unplayable,
        "roundtrip_stable": roundtrip_ok,
        "snapshots_checked": checked,
        "snapshots_mismatched": bad,
        "scene_hash": scene_hash,
        "log_hash": log_hash,
    });

    Ok(VerifyReport {
        text: lines.join("\n"),
        ok,
        json,
    })
}
