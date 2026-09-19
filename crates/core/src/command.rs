//! 命令模型与线上格式。
//!
//! # 两条不显然但重要的设计
//!
//! ## 1. 逆命令是「恢复原值」，不是「参数取反」
//!
//! 直觉上 `translate +0.35` 的逆是 `translate -0.35`。但对包围盒旋转就不成立：
//! 旋转后取 AABB 会**放大**盒子，再反向旋转得到的是更大的盒子。所以内核统一采用
//!
//! > **逆命令 = 把被改动的值恢复成前置状态里记录的值**
//!
//! 这带来两个好处：① 任何命令都**精确可逆**（不依赖代数性质）；② 未来接入不可逆操作
//! （裁剪高斯、简化网格）时，逆命令的形状不用变。
//!
//! 因此 `inverse` 一律由内核从**前置状态**算出（见 `document.rs`），调用方不需要自己写逆操作。
//!
//! ## 2. 线上格式（wire）与内部模型分开
//!
//! Agent / MCP 工具 / 脚手架模板发来的是**信封**：
//!
//! ```json
//! { "op": "transform", "target": "obj:sofa_01",
//!   "params": { "translate": [0, 0, -0.35] },
//!   "reason": "拉开与茶几间距", "expect": { "layout.clearance": "+" } }
//! ```
//!
//! 内部用类型化的 [`Command`]（日志、回放、diff 都基于它）。
//! `CommandRequest` 负责两者转换，并在转换时给出**能看懂的参数错误**。

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::error::{CoreError, Result};
use crate::scene::{normalize_node_id, Aabb, MaterialParams, Node};
use crate::validate::Warning;

// ---------------------------------------------------------------- 命令

/// 一条命令。**所有状态变更都必须表达成它**，否则无法回放与归因。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case")]
pub enum Command {
    /// 摆放 / 变换（H0 的几何代理是世界空间包围盒）
    Transform {
        target: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        translate: Option<[f64; 3]>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        rotate_y_deg: Option<f64>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        scale: Option<f64>,
    },
    /// 调灯光
    SetLight {
        target: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        intensity: Option<f64>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        color: Option<String>,
    },
    /// 调材质
    SetMaterial {
        target: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        material: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        roughness: Option<f64>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        metallic: Option<f64>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        opacity: Option<f64>,
    },
    /// 删除节点（破坏性：执行前自动打快照）
    Remove { target: String },
    /// 逆命令 / 恢复（由内核生成，Agent 一般不该直接发）
    Restore(Restore),
    /// 回滚到某一版（本身可逆：逆命令 = 回到当前版）
    Checkout { rev: u32 },
}

/// 恢复类命令的载荷：记下「前置状态长什么样」。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "what", rename_all = "snake_case")]
pub enum Restore {
    /// 恢复节点包围盒（transform / 破坏性几何操作的逆）
    Bounds { target: String, aabb: Aabb },
    /// 恢复灯光
    Light {
        id: String,
        intensity: f64,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        color: Option<String>,
    },
    /// 恢复材质
    Material {
        target: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        material: Option<String>,
        #[serde(default)]
        params: MaterialParams,
    },
    /// 把整个节点放回原位置（Remove 的逆）
    Node { index: usize, node: Box<Node> },
}

impl Command {
    /// 线上 op 名。
    pub fn op(&self) -> &'static str {
        match self {
            Command::Transform { .. } => "transform",
            Command::SetLight { .. } => "set_light",
            Command::SetMaterial { .. } => "set_material",
            Command::Remove { .. } => "remove",
            Command::Restore(_) => "restore",
            Command::Checkout { .. } => "checkout",
        }
    }

    /// 目标（用于归因表）。
    pub fn target(&self) -> String {
        match self {
            Command::Transform { target, .. }
            | Command::SetLight { target, .. }
            | Command::SetMaterial { target, .. }
            | Command::Remove { target } => target.clone(),
            Command::Restore(r) => match r {
                Restore::Bounds { target, .. } => target.clone(),
                Restore::Light { id, .. } => id.clone(),
                Restore::Material { target, .. } => target.clone(),
                Restore::Node { node, .. } => node.id.clone(),
            },
            Command::Checkout { rev } => format!("rev:{}", rev),
        }
    }

    /// **破坏性**命令：在 `core` 的模型里它其实可逆（逆命令带着整个节点），
    /// 但 H3 的裁剪/简化类操作无法完整还原语义，所以统一按破坏性处理并**先打快照**。
    pub fn is_destructive(&self) -> bool {
        matches!(self, Command::Remove { .. })
    }

    /// 一条命令是否什么都没改（用于拒绝空操作，防止 Agent 循环空转）。
    pub fn is_noop_shape(&self) -> bool {
        match self {
            Command::Transform {
                translate,
                rotate_y_deg,
                scale,
                ..
            } => {
                let zero = |v: &Option<[f64; 3]>| v.map(|a| a == [0.0, 0.0, 0.0]).unwrap_or(true);
                let deg = rotate_y_deg.map(|d| d == 0.0).unwrap_or(true);
                let sc = scale.map(|s| s == 1.0).unwrap_or(true);
                zero(translate) && deg && sc
            }
            Command::SetLight {
                intensity, color, ..
            } => intensity.is_none() && color.is_none(),
            Command::SetMaterial {
                material,
                roughness,
                metallic,
                opacity,
                ..
            } => {
                material.is_none()
                    && roughness.is_none()
                    && metallic.is_none()
                    && opacity.is_none()
            }
            _ => false,
        }
    }
}

// ---------------------------------------------------------------- 线上信封

/// Agent / MCP / 骨骼脚本发来的请求。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CommandRequest {
    pub op: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub target: Option<String>,
    /// 缺省视为 `{}`
    #[serde(default, skip_serializing_if = "Value::is_null")]
    pub params: Value,
    /// Agent 说它**为什么**这么做（归因表的原料）
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub reason: String,
    /// Agent 说它**期望**发生什么（归因表的另一半）
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expect: Option<Value>,
}

impl CommandRequest {
    /// 解析成类型化命令；所有参数错误都在这里给出可读信息。
    pub fn parse(&self) -> Result<Command> {
        let op = self.op.trim().to_ascii_lowercase();
        let target = self
            .target
            .as_deref()
            .map(|t| t.trim())
            .filter(|t| !t.is_empty());
        let p = Params::new(&self.params)?;

        let need_target = |for_op: &str| -> Result<String> {
            target
                .map(|t| t.to_string())
                .ok_or_else(|| CoreError::InvalidArgument(format!("{} 需要 target", for_op)))
        };

        match op.as_str() {
            "transform" => {
                let target = normalize_node_id(&need_target("transform")?);
                let translate = p.arr3("translate")?;
                let rotate_y_deg = p.f64("rotate_y_deg")?;
                let scale = p.f64("scale")?;
                if translate.is_none() && rotate_y_deg.is_none() && scale.is_none() {
                    return Err(CoreError::InvalidArgument(
                        "transform 至少要给 translate / rotate_y_deg / scale 之一".into(),
                    ));
                }
                Ok(Command::Transform {
                    target,
                    translate,
                    rotate_y_deg,
                    scale,
                })
            }
            "set_light" => {
                let target = need_target("set_light")?;
                let intensity = p.f64("intensity")?;
                let color = p.str("color")?;
                if intensity.is_none() && color.is_none() {
                    return Err(CoreError::InvalidArgument(
                        "set_light 至少要给 intensity 或 color".into(),
                    ));
                }
                Ok(Command::SetLight {
                    target,
                    intensity,
                    color,
                })
            }
            "set_material" => {
                let target = normalize_node_id(&need_target("set_material")?);
                let material = p.str("material")?;
                let roughness = p.f64("roughness")?;
                let metallic = p.f64("metallic")?;
                let opacity = p.f64("opacity")?;
                if material.is_none()
                    && roughness.is_none()
                    && metallic.is_none()
                    && opacity.is_none()
                {
                    return Err(CoreError::InvalidArgument(
                        "set_material 至少要给 material / roughness / metallic / opacity 之一"
                            .into(),
                    ));
                }
                Ok(Command::SetMaterial {
                    target,
                    material,
                    roughness,
                    metallic,
                    opacity,
                })
            }
            "remove" => Ok(Command::Remove {
                target: normalize_node_id(&need_target("remove")?),
            }),
            "checkout" => {
                let rev = p
                    .u32("rev")?
                    .or_else(|| p.u32("revision").ok().flatten())
                    .ok_or_else(|| {
                        CoreError::InvalidArgument("checkout 需要 params.rev".into())
                    })?;
                Ok(Command::Checkout { rev })
            }
            "restore" => Err(CoreError::InvalidArgument(
                "restore 是内核生成的逆命令，不接受外部直接发送".into(),
            )),
            other => Err(CoreError::InvalidArgument(format!(
                "未知 op：{}（支持 transform / set_light / set_material / remove / checkout）",
                other
            ))),
        }
    }

    /// 从内部命令造出信封（日志 → 线上形态的回程）。
    pub fn from_command(cmd: &Command, reason: &str, expect: Option<&Value>) -> Self {
        let target = match cmd {
            Command::Restore(Restore::Node { node, .. }) => Some(node.id.clone()),
            other => Some(other.target()),
        };
        CommandRequest {
            op: cmd.op().to_string(),
            target,
            params: command_params(cmd),
            reason: reason.to_string(),
            expect: expect.cloned(),
        }
    }
}

/// 把命令的载荷摊平成 `params` 对象（与 Agent 发来的形状一致）。
fn command_params(cmd: &Command) -> Value {
    match cmd {
        Command::Transform {
            translate,
            rotate_y_deg,
            scale,
            ..
        } => {
            let mut m = serde_json::Map::new();
            if let Some(t) = translate {
                m.insert("translate".into(), serde_json::json!(t));
            }
            if let Some(d) = rotate_y_deg {
                m.insert("rotate_y_deg".into(), serde_json::json!(d));
            }
            if let Some(s) = scale {
                m.insert("scale".into(), serde_json::json!(s));
            }
            Value::Object(m)
        }
        Command::SetLight {
            intensity, color, ..
        } => {
            let mut m = serde_json::Map::new();
            if let Some(v) = intensity {
                m.insert("intensity".into(), serde_json::json!(v));
            }
            if let Some(c) = color {
                m.insert("color".into(), serde_json::json!(c));
            }
            Value::Object(m)
        }
        Command::SetMaterial {
            material,
            roughness,
            metallic,
            opacity,
            ..
        } => {
            let mut m = serde_json::Map::new();
            if let Some(v) = material {
                m.insert("material".into(), serde_json::json!(v));
            }
            if let Some(v) = roughness {
                m.insert("roughness".into(), serde_json::json!(v));
            }
            if let Some(v) = metallic {
                m.insert("metallic".into(), serde_json::json!(v));
            }
            if let Some(v) = opacity {
                m.insert("opacity".into(), serde_json::json!(v));
            }
            Value::Object(m)
        }
        Command::Checkout { rev } => serde_json::json!({ "rev": rev }),
        Command::Remove { .. } => Value::Object(serde_json::Map::new()),
        Command::Restore(r) => serde_json::json!(r),
    }
}

// ---------------------------------------------------------------- 参数读取

/// 带类型检查的参数读取器（错误信息面向 Agent，而不是面向 Rust 开发者）。
pub struct Params<'a> {
    obj: Option<&'a serde_json::Map<String, Value>>,
}

impl<'a> Params<'a> {
    pub fn new(v: &'a Value) -> Result<Self> {
        match v {
            Value::Null => Ok(Params { obj: None }),
            Value::Object(m) => Ok(Params { obj: Some(m) }),
            other => Err(CoreError::InvalidArgument(format!(
                "params 必须是对象，收到 {}",
                type_name(other)
            ))),
        }
    }

    fn raw(&self, key: &str) -> Option<&'a Value> {
        self.obj.and_then(|m| m.get(key))
    }

    pub fn f64(&self, key: &str) -> Result<Option<f64>> {
        match self.raw(key) {
            None | Some(Value::Null) => Ok(None),
            Some(Value::Number(n)) => {
                let v = n.as_f64().ok_or_else(|| {
                    CoreError::InvalidArgument(format!("params.{} 不是合法数字", key))
                })?;
                if !v.is_finite() {
                    return Err(CoreError::NonFinite(format!("params.{}", key)));
                }
                Ok(Some(v))
            }
            Some(other) => Err(CoreError::InvalidArgument(format!(
                "params.{} 应为数字，收到 {}",
                key,
                type_name(other)
            ))),
        }
    }

    pub fn u32(&self, key: &str) -> Result<Option<u32>> {
        match self.raw(key) {
            None | Some(Value::Null) => Ok(None),
            Some(Value::Number(n)) => n
                .as_u64()
                .and_then(|v| u32::try_from(v).ok())
                .map(Some)
                .ok_or_else(|| {
                    CoreError::InvalidArgument(format!("params.{} 应为非负整数", key))
                }),
            Some(other) => Err(CoreError::InvalidArgument(format!(
                "params.{} 应为整数，收到 {}",
                key,
                type_name(other)
            ))),
        }
    }

    pub fn str(&self, key: &str) -> Result<Option<String>> {
        match self.raw(key) {
            None | Some(Value::Null) => Ok(None),
            Some(Value::String(s)) => Ok(Some(s.clone())),
            Some(other) => Err(CoreError::InvalidArgument(format!(
                "params.{} 应为字符串，收到 {}",
                key,
                type_name(other)
            ))),
        }
    }

    pub fn arr3(&self, key: &str) -> Result<Option<[f64; 3]>> {
        let Some(v) = self.raw(key).filter(|v| !v.is_null()) else {
            return Ok(None);
        };
        let arr = v.as_array().ok_or_else(|| {
            CoreError::InvalidArgument(format!("params.{} 应为长度 3 的数组", key))
        })?;
        if arr.len() != 3 {
            return Err(CoreError::InvalidArgument(format!(
                "params.{} 应为长度 3 的数组，收到长度 {}",
                key,
                arr.len()
            )));
        }
        let mut out = [0.0_f64; 3];
        for (i, item) in arr.iter().enumerate() {
            let n = item.as_f64().ok_or_else(|| {
                CoreError::InvalidArgument(format!("params.{}[{}] 不是数字", key, i))
            })?;
            if !n.is_finite() {
                return Err(CoreError::NonFinite(format!("params.{}[{}]", key, i)));
            }
            out[i] = n;
        }
        Ok(Some(out))
    }
}

fn type_name(v: &Value) -> &'static str {
    match v {
        Value::Null => "null",
        Value::Bool(_) => "布尔值",
        Value::Number(_) => "数字",
        Value::String(_) => "字符串",
        Value::Array(_) => "数组",
        Value::Object(_) => "对象",
    }
}

// ---------------------------------------------------------------- 执行结果与日志

/// 一次成功执行的摘要。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Applied {
    pub revision: u32,
    pub command: Command,
    /// 逆命令（由内核从前置状态算出）
    pub inverse: Command,
    /// **本次新引入**的告警（不是全量告警）
    pub new_warnings: Vec<Warning>,
    pub scene_hash: String,
}

/// 命令日志里的一条。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct OpEntry {
    pub rev: u32,
    pub command: Command,
    pub inverse: Command,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub reason: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expect: Option<Value>,
    /// 墙钟（可选）。**不参与日志哈希**：日志必须可复现，不能含时间与随机数。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub at_ms: Option<i64>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub new_warnings: Vec<Warning>,
}

impl OpEntry {
    /// 一行摘要（给人看的）。
    pub fn summary(&self) -> String {
        format!(
            "rev {} · {} {}",
            self.rev,
            self.command.op(),
            self.command.target()
        )
    }
}

/// 归因表的一行：**Agent 的判断 vs 实际效果**。
/// `delta` 由 `eval` 层填（内核不认识分数），内核只保证 `reason/expect` 被如实记下来。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AttributionRow {
    pub rev: u32,
    pub op: String,
    pub target: String,
    pub reason: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expect: Option<Value>,
}
