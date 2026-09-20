//! 渲染模式：**从主机参数判出这台机器该怎么干活，以及干不了时怎么办**。
//!
//! # 这一层补的是什么洞
//!
//! 之前客户端只会声明"我有什么"（`?cap=webgl2,three`，见 [`KNOWN_CLIENT_CAPABILITIES`]），
//! 服务端据此派生一个 `render_tier`。那是**声明**，两件事它答不了：
//!
//! 1. **"胜任不了"**：声明里有 `three` 不等于跑得动——集显、4 核、1600×900 的旧笔记本
//!    也会老老实实声明 `webgl2,three`，然后一开场景流就卡成幻灯片；
//! 2. **"那该怎么办"**：降档是一件事，告诉用户**可选的出路**是另一件事。
//!
//! 所以这里加一层：
//!
//! ```text
//! 探测（浏览器读公开参数 + 2 秒微基准）
//!   → HostProfile（**默认脱敏**：硬件型号走哈希）
//!   → 判定：联网时 POST 给 rsi3d.com（authority = platform）
//!           不可达时用**同一份规则表**本地算（authority = local，并标注）
//!   → RenderVerdict：模式 + 限制（分辨率/帧率/几何档/订阅哪条流）+ 理由 + 缺什么 + 出路
//! ```
//!
//! # 三条不变量
//!
//! - **规则表是数据**（[`RenderPolicy`]）：判定结果随平台更新而更新，客户端不必重新发版；
//!   离线时用内置的那份（[`DEFAULT_POLICY`]，与平台逐字段对齐，靠 `contract/render-policy.json`
//!   与共享语料钉住）。
//! - **判定结果只能"往弱里走"**：限制是**上限**（`max_px` / `max_fps`），客户端只缩不放；
//!   这与 §11/§12 那条"limits 只缩不放"是同一条。
//! - **"不知道"不等于"不行"**：数值缺省（`0` / `None`）一律**跳过**那条规则，并记一条
//!   说明——绝不允许因为读不到某个参数就把人判成低档。唯一例外是**渲染所需的 GPU 接口
//!   完全未知**：那种情况下按最保守处理（不能拿"没测出来"当"能渲染"）。

use serde::{Deserialize, Serialize};

use crate::protocol::KNOWN_CLIENT_CAPABILITIES;

/// 渲染模式的**阶梯**（从强到弱）。
///
/// `headless` 不在强度轴上——它是**形态**（没屏幕），单独处理。
pub const RENDER_MODES: [(&str, &str); 4] = [
    ("client-full", "客户端自己渲：真网格 + 完整场景流"),
    ("client-lite", "客户端渲但降档：降像素比/限帧率（几何仍可要真网格）"),
    ("client-minimal", "只画包围盒代理、帧率很低：看得懂布局，看不清细节"),
    ("frame-only", "本机不渲：只订阅服务端图像流（服务端出图，客户端只显示）"),
];

/// 模式的强度序（越大越强）。`headless` 单独处理，不在这条轴上。
pub fn mode_rank(mode: &str) -> i32 {
    match mode {
        "headless" => -1, // 形态不同：比 frame-only 更"不交互"
        _ => RENDER_MODES
            .iter()
            .position(|(m, _)| *m == mode)
            .map(|i| (RENDER_MODES.len() - i) as i32)
            .unwrap_or(0),
    }
}

fn mode_meaning(mode: &str) -> &'static str {
    RENDER_MODES
        .iter()
        .find(|(m, _)| *m == mode)
        .map(|(_, d)| *d)
        .unwrap_or("未知模式")
}

// ---------------------------------------------------------------- 主机参数

/// 客户端测到的主机事实。**默认脱敏**：型号走哈希，明文只在显式开启时带。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct HostProfile {
    /// 谁在报（`rsi3d-web/0.1.0`）
    #[serde(default)]
    pub agent: String,
    /// `screen`（有屏）或 `headless`（无屏：CLI/CI）
    #[serde(default = "default_form")]
    pub form: String,
    #[serde(default)]
    pub gpu: GpuFacts,
    #[serde(default)]
    pub cpu: CpuFacts,
    #[serde(default)]
    pub display: DisplayFacts,
    /// 微基准结果（**可选**：不跑也能判，只是判得粗，且会在 reasons 里说明）
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bench: Option<BenchResult>,
    /// 型号怎么处理的：`hashed`（默认）/ `plaintext` / `omitted`
    #[serde(default = "default_privacy")]
    pub privacy: String,
}

fn default_form() -> String {
    "screen".into()
}
fn default_privacy() -> String {
    "hashed".into()
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct GpuFacts {
    /// `webgl2` / `webgl1` / `none`；空 = **未知**
    #[serde(default)]
    pub api: String,
    #[serde(default)]
    pub webgpu: bool,
    /// 0 = 未知
    #[serde(default)]
    pub max_texture: u32,
    /// 型号的短哈希（默认只发这个）
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub renderer_hash: Option<String>,
    /// 明文型号（只有 `privacy = plaintext` 才有）
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub renderer: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub vendor_hash: Option<String>,
    /// 检测到软件光栅（SwiftShader / llvmpipe / ANGLE 软件后端）——渲得动但很慢
    #[serde(default)]
    pub software: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct CpuFacts {
    /// 0 = 未知
    #[serde(default)]
    pub cores: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub memory_gb: Option<f64>,
    #[serde(default)]
    pub platform: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct DisplayFacts {
    /// `(宽, 高)`；`(0, 0)` = 未知
    #[serde(default)]
    pub viewport: (u32, u32),
    #[serde(default)]
    pub dpr: f64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub refresh_hz: Option<u32>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct BenchResult {
    /// 持续帧率（跑满 2 秒的平均值，不是首帧峰值）
    #[serde(default)]
    pub sustained_fps: f64,
    #[serde(default)]
    pub frames: u32,
    /// 填充率（百万像素/秒）
    #[serde(default)]
    pub fillrate_mpx: f64,
    /// 几何吞吐（百万三角面/秒）
    #[serde(default)]
    pub triangles_mps: f64,
    #[serde(default)]
    pub ms: u32,
}

// ---------------------------------------------------------------- 规则表

/// 规则表：**平台拥有它，判定两侧共用**。
///
/// 为什么把规则做成数据而不是代码：判定的松紧会随实机反馈调整（比如"4 核也能渲真网格"），
/// 那种调整不该要求客户端重新发版；同时它让"平台算"和"离线算"用的是**同一份规则**，
/// 不存在两套阈值。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RenderPolicy {
    pub version: String,
    #[serde(default)]
    pub note: String,
    /// **从强到弱**排列；第一条"所有条件都满足"的胜出
    pub modes: Vec<ModeRule>,
    /// "干不了该怎么办"的建议（按模式强弱挂）
    #[serde(default)]
    pub fallbacks: Vec<Fallback>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ModeRule {
    pub mode: String,
    /// 需要其中**任意一个** GPU 接口（空 = 不限）
    #[serde(default)]
    pub require_gpu: Vec<String>,
    /// 0 = 不要求
    #[serde(default)]
    pub min_cores: u32,
    /// 0 = 不要求（没有基准数据时**跳过**这条）
    #[serde(default)]
    pub min_sustained_fps: f64,
    #[serde(default)]
    pub min_max_texture: u32,
    /// `(0, 0)` = 不要求
    #[serde(default)]
    pub min_viewport: (u32, u32),
    /// 软件光栅能不能进这一档
    #[serde(default)]
    pub allow_software: bool,
    pub limits: ModeLimits,
    #[serde(default)]
    pub why: String,
}

/// 这一档给客户端的**限制**（全是上限，只缩不放）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct ModeLimits {
    /// 像素预算 `(宽, 高)`——客户端直接当作 `?px=` 发回来
    pub max_px: (u32, u32),
    pub max_fps: u32,
    /// `real`（真网格）或 `aabb-proxy`（只要包围盒代理）
    pub geometry: String,
    /// 订阅哪条流：`scene` / `frame` / `both`
    pub stream: String,
    /// 客户端要不要跑持续动画（`frame-only` 档就不必了）
    #[serde(default)]
    pub allow_animation: bool,
}

/// 一条"干不了该怎么办"的建议。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Fallback {
    pub title: String,
    /// 具体做什么
    pub what: String,
    /// 为什么对它有用
    pub why: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub link: Option<String>,
    /// 当判定弱于/等于这个模式时给出这条建议（空 = 总是给）
    #[serde(default)]
    pub when_at_or_below: String,
}

/// 离线兜底用的内置规则表。
///
/// ⚠️ 它与平台那份**必须逐字段一致**：`contract/render-policy.json` 是这里的序列化产物，
/// 平台侧用例会拿它比对自己那份（`contract/render-policy.corpus.json` 则钉判定的**行为**）。
pub const DEFAULT_POLICY: &str = include_str!("../policy/render-policy.json");

/// 解析内置规则表（编译期就检查过 JSON 合法：见单测 `default_policy_is_valid`）。
pub fn default_policy() -> RenderPolicy {
    serde_json::from_str(DEFAULT_POLICY).expect("内置规则表应当合法")
}

// ---------------------------------------------------------------- 判定

/// 判定结果：**模式 + 限制 + 为什么 + 缺什么 + 出路**。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RenderVerdict {
    pub mode: String,
    pub limits: ModeLimits,
    /// 判定依据（人读的句子：为什么给我这一档）
    pub reasons: Vec<String>,
    /// 主机**没能满足**的东西（缺什么才更强）
    pub missing: Vec<String>,
    /// 干不了时的建议（在线方案 / 换档 / 换设备）
    pub fallbacks: Vec<Fallback>,
    /// 谁在算：`platform`（rsi3d.com）/ `local`（离线兜底）/ `declared`（客户端显式指定）
    pub authority: String,
    pub policy_version: String,
}

/// 主判定：走一遍阶梯，第一条满足的胜出。
pub fn assess(profile: &HostProfile, policy: &RenderPolicy, authority: &str) -> RenderVerdict {
    let mut reasons = Vec::new();
    let mut notes = Vec::new();

    // 无屏形态单独处理：它不是"弱"，是"没有屏幕"
    if profile.form == "headless" {
        let rule = policy
            .modes
            .iter()
            .find(|r| r.mode == "headless")
            .or_else(|| policy.modes.last());
        if let Some(r) = rule {
            reasons.push(format!("主机形态是 headless（无屏幕）：{}", r.why));
            return finish(profile, policy, r, reasons, Vec::new(), authority);
        }
    }

    let mut chosen: Option<usize> = None;
    let mut first_fail: Option<(usize, Vec<String>)> = None;
    for (i, rule) in policy.modes.iter().enumerate() {
        if rule.mode == "headless" {
            continue;
        }
        let (ok, why, miss) = fits(rule, profile, &mut notes);
        if ok {
            chosen = Some(i);
            reasons.push(format!("{}：{}", rule.mode, rule.why));
            reasons.extend(why);
            break;
        }
        if first_fail.is_none() && !miss.is_empty() {
            first_fail = Some((i, miss));
        }
    }

    let idx = chosen.unwrap_or_else(|| policy.modes.len().saturating_sub(1));
    let rule = &policy.modes[idx];
    // 缺什么：优先给"比它强一档"那道规则缺的条件（那才是"差一点就能上"的提示）
    let missing = match &first_fail {
        Some((_, miss)) if *miss != Vec::<String>::new() => miss.clone(),
        _ => Vec::new(),
    };
    reasons.append(&mut notes);
    finish(profile, policy, rule, reasons, missing, authority)
}

fn finish(
    profile: &HostProfile,
    policy: &RenderPolicy,
    rule: &ModeRule,
    mut reasons: Vec<String>,
    missing: Vec<String>,
    authority: &str,
) -> RenderVerdict {
    // 没有基准数据时要说清楚：这时的判定只看了硬件参数，可能偏乐观
    if profile.bench.is_none() && rule.mode != "headless" {
        reasons.push(
            "没有微基准数据：这一档是按**硬件参数**判的（可能偏乐观）——要更准就在客户端跑一次基准"
                .into(),
        );
    }
    if profile.privacy != "plaintext" {
        reasons.push("硬件型号只上报了哈希（默认脱敏）".into());
    }
    // **满档不给建议**：机器够用的时候给"该怎么办"是噪音。
    // 降档时：`when_at_or_below` 空的算"任何降档都给"，否则按强度比。
    let rank = mode_rank(&rule.mode);
    let strongest = policy.modes.first().map(|m| mode_rank(&m.mode)).unwrap_or(rank);
    let fallbacks: Vec<Fallback> = if rank >= strongest {
        Vec::new()
    } else {
        policy
            .fallbacks
            .iter()
            .filter(|f| {
                f.when_at_or_below.is_empty() || mode_rank(&f.when_at_or_below) >= rank
            })
            .cloned()
            .collect()
    };
    RenderVerdict {
        mode: rule.mode.clone(),
        limits: rule.limits.clone(),
        reasons,
        missing,
        fallbacks,
        authority: authority.to_string(),
        policy_version: policy.version.clone(),
    }
}

/// 这一档吃不吃得下这台机器。返回 `(满足, 命中理由, 缺什么)`。
fn fits(rule: &ModeRule, p: &HostProfile, notes: &mut Vec<String>) -> (bool, Vec<String>, Vec<String>) {
    let mut why = Vec::new();
    let mut miss = Vec::new();

    // GPU 接口：**唯一**"未知即按不行处理"的地方——渲染能力不能靠猜
    if !rule.require_gpu.is_empty() {
        let api = p.gpu.api.trim();
        if api.is_empty() {
            miss.push("GPU 接口未知（渲染能力不能靠猜）".into());
        } else if rule.require_gpu.iter().any(|r| r == api) {
            why.push(format!("GPU 接口 {} 可用", api));
        } else {
            miss.push(format!("需要 {}，本机是 {}", rule.require_gpu.join("/"), api));
        }
    }

    if rule.min_cores > 0 {
        if p.cpu.cores == 0 {
            notes.push("CPU 核心数未知：跳过这一条".into());
        } else if p.cpu.cores >= rule.min_cores {
            why.push(format!("{} 核 ≥ {}", p.cpu.cores, rule.min_cores));
        } else {
            miss.push(format!("需要 {} 核，本机 {} 核", rule.min_cores, p.cpu.cores));
        }
    }

    if rule.min_max_texture > 0 {
        if p.gpu.max_texture == 0 {
            notes.push("最大纹理尺寸未知：跳过这一条".into());
        } else if p.gpu.max_texture >= rule.min_max_texture {
            why.push(format!("最大纹理 {} ≥ {}", p.gpu.max_texture, rule.min_max_texture));
        } else {
            miss.push(format!(
                "需要最大纹理 {}，本机 {}",
                rule.min_max_texture, p.gpu.max_texture
            ));
        }
    }

    if rule.min_viewport != (0, 0) {
        if p.display.viewport == (0, 0) {
            notes.push("视口尺寸未知：跳过这一条".into());
        } else if p.display.viewport.0 >= rule.min_viewport.0
            && p.display.viewport.1 >= rule.min_viewport.1
        {
            why.push(format!(
                "视口 {}×{} ≥ {}×{}",
                p.display.viewport.0, p.display.viewport.1, rule.min_viewport.0, rule.min_viewport.1
            ));
        } else {
            miss.push(format!(
                "需要视口至少 {}×{}，本机 {}×{}",
                rule.min_viewport.0, rule.min_viewport.1, p.display.viewport.0, p.display.viewport.1
            ));
        }
    }

    if rule.min_sustained_fps > 0.0 {
        match &p.bench {
            None => notes.push("没有基准数据：帧率这条跳过（判定可能偏乐观）".into()),
            Some(b) if b.sustained_fps >= rule.min_sustained_fps => {
                why.push(format!("持续 {:.0} fps ≥ {:.0}", b.sustained_fps, rule.min_sustained_fps))
            }
            Some(b) => miss.push(format!(
                "需要持续 {:.0} fps，实测 {:.0}",
                rule.min_sustained_fps, b.sustained_fps
            )),
        }
    }

    // 软件光栅：渲得动但慢，除非这一档明确允许
    if p.gpu.software && !rule.allow_software {
        miss.push("检测到软件光栅（没有硬件加速）".into());
    }

    (miss.is_empty(), why, miss)
}

/// 人读的一句话（给 CLI / 日志 / UI 用）。
pub fn explain(v: &RenderVerdict) -> String {
    let mut s = format!(
        "渲染模式 {}（{} · 规则表 {}）\n  {}\n  限制：≤ {}×{} · ≤ {} fps · 几何 {} · 订阅 {}",
        v.mode,
        v.authority,
        v.policy_version,
        mode_meaning(&v.mode),
        v.limits.max_px.0,
        v.limits.max_px.1,
        v.limits.max_fps,
        v.limits.geometry,
        v.limits.stream
    );
    for r in &v.reasons {
        s.push_str(&format!("\n  · {}", r));
    }
    if !v.missing.is_empty() {
        s.push_str(&format!("\n  差一点就能更强：{}", v.missing.join("；")));
    }
    for f in &v.fallbacks {
        s.push_str(&format!("\n  → {}：{}（{}）", f.title, f.what, f.why));
    }
    s
}

/// 能力词汇表里"渲染相关"的那些名字（给 UI/文档用，避免手抄）。
pub fn render_capability_names() -> Vec<&'static str> {
    KNOWN_CLIENT_CAPABILITIES
        .iter()
        .filter(|c| matches!(c.kind, crate::protocol::CapabilityKind::Render | crate::protocol::CapabilityKind::Downlevel))
        .map(|c| c.name)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn strong() -> HostProfile {
        HostProfile {
            agent: "rsi3d-web/0.1.0".into(),
            gpu: GpuFacts {
                api: "webgl2".into(),
                webgpu: true,
                max_texture: 16384,
                ..Default::default()
            },
            cpu: CpuFacts {
                cores: 10,
                memory_gb: Some(16.0),
                platform: "macos".into(),
            },
            display: DisplayFacts {
                viewport: (1920, 1080),
                dpr: 2.0,
                refresh_hz: Some(60),
            },
            bench: Some(BenchResult {
                sustained_fps: 58.0,
                frames: 116,
                fillrate_mpx: 900.0,
                triangles_mps: 42.0,
                ms: 2000,
            }),
            ..Default::default()
        }
    }

    #[test]
    fn default_policy_is_valid_and_ordered() {
        let p = default_policy();
        assert!(!p.version.is_empty());
        assert!(p.modes.len() >= 3, "阶梯至少要有三档");
        // 从强到弱：rank 必须单调不增
        let ranks: Vec<i32> = p.modes.iter().map(|m| mode_rank(&m.mode)).collect();
        assert!(
            ranks.windows(2).all(|w| w[0] >= w[1]),
            "规则表必须从强到弱排列，实际：{:?}",
            ranks
        );
        assert!(p.fallbacks.iter().any(|f| !f.what.is_empty()), "要给得出路");
    }

    #[test]
    fn a_strong_machine_gets_the_strongest_mode() {
        let v = assess(&strong(), &default_policy(), "local");
        assert_eq!(v.mode, "client-full", "{:?}", v);
        assert_eq!(v.limits.geometry, "real");
        assert_eq!(v.limits.stream, "both");
        assert!(v.missing.is_empty());
        assert!(v.reasons.iter().any(|r| r.contains("webgl2")));
    }

    #[test]
    fn a_weak_machine_is_downgraded_with_reasons_and_a_way_out() {
        // 4 核 + WebGL1 + 软件光栅 + 只有 640×480：典型的瘦客户端
        let weak = HostProfile {
            agent: "rsi3d-web/0.1.0".into(),
            gpu: GpuFacts {
                api: "webgl1".into(),
                max_texture: 2048,
                software: true,
                ..Default::default()
            },
            cpu: CpuFacts {
                cores: 2,
                platform: "windows".into(),
                ..Default::default()
            },
            display: DisplayFacts {
                viewport: (640, 480),
                dpr: 1.0,
                refresh_hz: None,
            },
            bench: Some(BenchResult {
                sustained_fps: 9.0,
                frames: 18,
                ..Default::default()
            }),
            ..Default::default()
        };
        let v = assess(&weak, &default_policy(), "local");
        assert_ne!(v.mode, "client-full", "这么弱的机器不能判成满档：{:?}", v);
        assert!(mode_rank(&v.mode) < mode_rank("client-full"));
        assert!(!v.reasons.is_empty(), "降档必须给理由");
        assert!(!v.fallbacks.is_empty(), "降档必须给得出路");
        assert!(v.limits.max_px.0 <= 1280, "低档的像素预算要收紧");
        // 降档理由是"缺什么"驱动的，必须点名
        assert!(
            v.missing.iter().any(|m| m.contains("核") || m.contains("软件光栅")),
            "{:?}",
            v.missing
        );
    }

    #[test]
    fn unknown_numbers_do_not_punish_anyone() {
        // 除了 GPU 接口，其它参数全未知：不能因为"读不到"就判低档
        let unknown = HostProfile {
            agent: "rsi3d-cli/0.1.0".into(),
            gpu: GpuFacts {
                api: "webgl2".into(),
                ..Default::default()
            },
            ..Default::default()
        };
        let v = assess(&unknown, &default_policy(), "local");
        assert_eq!(v.mode, "client-full", "未知的数值参数不该把人判低：{:?}", v);
        assert!(
            v.reasons.iter().any(|r| r.contains("未知")),
            "跳过了哪几条要说出来：{:?}",
            v.reasons
        );
        // 反过来：GPU 接口未知时**必须**保守（渲染能力不能靠猜）
        let no_api = HostProfile {
            agent: "rsi3d-cli/0.1.0".into(),
            ..Default::default()
        };
        let v2 = assess(&no_api, &default_policy(), "local");
        assert_ne!(v2.mode, "client-full");
        assert!(v2.missing.iter().any(|m| m.contains("GPU 接口未知")), "{:?}", v2);
    }

    #[test]
    fn no_bench_is_flagged_not_hidden() {
        let mut p = strong();
        p.bench = None;
        let v = assess(&p, &default_policy(), "local");
        assert!(v.reasons.iter().any(|r| r.contains("偏乐观")), "{:?}", v);
    }

    #[test]
    fn headless_is_a_form_not_a_strength() {
        let h = HostProfile {
            agent: "rsi3d-harness/0.1.0".into(),
            form: "headless".into(),
            ..Default::default()
        };
        let v = assess(&h, &default_policy(), "local");
        assert_eq!(v.mode, "headless");
        assert_eq!(v.limits.stream, "none", "无屏形态不该订阅任何流");
    }

    #[test]
    fn verdict_roundtrips_through_json() {
        let v = assess(&strong(), &default_policy(), "platform");
        let text = serde_json::to_string(&v).unwrap();
        let back: RenderVerdict = serde_json::from_str(&text).unwrap();
        assert_eq!(v, back);
        // 平台算的必须能标出来（离线兜底要能分辨）
        assert!(text.contains("\"authority\":\"platform\""));
    }
}
