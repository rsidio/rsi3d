//! 场景规则校验：产出**结构化告警**。
//!
//! 告警与错误的区别：
//! - **错误**（`CoreError`）→ 命令被拒绝，状态不变；
//! - **告警**（`Warning`）→ 命令照做，但把「现在哪里不对」告诉 Agent。
//!
//! Agent 需要能试错，所以默认不阻塞；但告警必须**可比较、可排序、可哈希**，
//! 这样 `Document` 才能给出「本次**新引入**的告警」而不是每次都刷一屏老问题。

use serde::{Deserialize, Serialize};

use crate::scene::{RoleKind, Scene};

/// 一条场景告警。
///
/// 字段顺序即排序顺序（`code` → `nodes` → `message`），保证输出确定。
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct Warning {
    /// 机器可读码（如 `layout.intersect`）
    pub code: String,
    /// 涉及的节点/灯光 id（有序）
    pub nodes: Vec<String>,
    /// 给人看的说明（数字已格式化进文本，便于比较）
    pub message: String,
}

impl Warning {
    fn new(code: &str, nodes: Vec<String>, message: String) -> Self {
        Warning {
            code: code.to_string(),
            nodes,
            message,
        }
    }
}

/// 允许的灯光强度区间（家居场景的经验区间，超出即告警）。
const LIGHT_INTENSITY_RANGE: (f64, f64) = (0.0, 5.0);
/// 家具「悬空」判定阈值（米）。超过即认为没落地。
const FLOATING_THRESHOLD: f64 = 0.02;

/// 全量校验，输出**已排序去重**的告警列表。
pub fn validate(scene: &Scene) -> Vec<Warning> {
    let mut out: Vec<Warning> = Vec::new();

    // 1. 越界 / 悬空
    if let Some(room) = &scene.room {
        let bounds = room.bounds();
        for n in &scene.objects {
            if n.role_kind() == RoleKind::Floor {
                continue;
            }
            if !bounds.contains(&n.aabb) {
                out.push(Warning::new(
                    "bounds.out_of_room",
                    vec![n.id.clone()],
                    format!(
                        "{} 超出房间范围（在 x/z 上至少要落在 ±{:.2}m / ±{:.2}m 内）",
                        n.id,
                        room.size[0] / 2.0,
                        room.size[2] / 2.0
                    ),
                ));
            }
        }
    }
    for n in &scene.objects {
        if n.role_kind() == RoleKind::Furniture && n.aabb.min[1] > FLOATING_THRESHOLD {
            out.push(Warning::new(
                "bounds.floating",
                vec![n.id.clone()],
                format!("{} 离地 {:.3}m，看起来没落地", n.id, n.aabb.min[1]),
            ));
        }
    }

    // 2. 家具相交
    let solids: Vec<&crate::scene::Node> = scene
        .objects
        .iter()
        .filter(|n| n.role_kind().is_solid())
        .collect();
    for i in 0..solids.len() {
        for j in (i + 1)..solids.len() {
            if solids[i].aabb.intersects(&solids[j].aabb) {
                let (a, b) = ordered_pair(&solids[i].id, &solids[j].id);
                out.push(Warning::new(
                    "layout.intersect",
                    vec![a.clone(), b.clone()],
                    format!("{} 与 {} 相交/贴在一起", a, b),
                ));
            }
        }
    }

    // 3. 间距规则（行业知识被显式化的地方）
    for rule in &scene.clearance_rules {
        let (Some(a), Some(b)) = (scene.node(&rule.pair[0]), scene.node(&rule.pair[1])) else {
            let (x, y) = ordered_pair(&rule.pair[0], &rule.pair[1]);
            out.push(Warning::new(
                "rule.dangling",
                vec![x, y],
                format!(
                    "规则引用了不存在的节点：{} / {}",
                    rule.pair[0], rule.pair[1]
                ),
            ));
            continue;
        };
        let gap = a.aabb.gap(&b.aabb);
        let dist = rule.distance(gap);
        if dist > 0.0 {
            let (x, y) = ordered_pair(&a.id, &b.id);
            out.push(Warning::new(
                "rule.violated",
                vec![x.clone(), y.clone()],
                format!(
                    "{} 与 {} 间距 {:.2}m，应在 {:.2}–{:.2}m（{}）",
                    x, y, gap, rule.min, rule.max, rule.reason
                ),
            ));
        }
    }

    // 4. 挡窗
    for id in scene.window_blockers() {
        out.push(Warning::new(
            "window.blocked",
            vec![id.clone()],
            format!("{} 落在窗前挡光带里", id),
        ));
    }

    // 5. 灯光强度
    for l in &scene.lights {
        if !(LIGHT_INTENSITY_RANGE.0..=LIGHT_INTENSITY_RANGE.1).contains(&l.intensity) {
            out.push(Warning::new(
                "light.out_of_range",
                vec![l.id.clone()],
                format!(
                    "灯光 {} 强度 {:.2}，建议落在 {:.0}–{:.0}",
                    l.id, l.intensity, LIGHT_INTENSITY_RANGE.0, LIGHT_INTENSITY_RANGE.1
                ),
            ));
        }
    }

    // 6. 意图缺项（收敛 ≠ 质量达标的证据来源）
    for kw in &scene.intent_keywords {
        if !kw.present {
            out.push(Warning::new(
                "intent.missing",
                Vec::new(),
                format!("意图里的「{}」在场景里不存在", kw.word),
            ));
        }
    }

    out.sort();
    out.dedup();
    out
}

/// 稳定排序的两个 id（告警的可比较性要求与顺序无关）。
fn ordered_pair(a: &str, b: &str) -> (String, String) {
    if a <= b {
        (a.to_string(), b.to_string())
    } else {
        (b.to_string(), a.to_string())
    }
}
