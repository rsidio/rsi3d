//! 统一场景表示（USC）。
//!
//! # 设计约束（改这个文件前先读）
//!
//! 1. **与 glTF 2.0 同构**：`node` 对应 glTF node，「层」对应 `mesh.primitive`；
//!    3DGS 层用 Khronos 已 Ratified 的 `KHR_gaussian_splatting` 语义。
//!    我们自己的字段（`provenance` / `layers` / `metrics`）落盘时进 glTF 的 `extras`。
//! 2. **JSON 键沿用已发布的场景模板契约**（`objects` / `window` / `spanX` …）——
//!    因为 `agent-app` 与 `harness-plugin` 两个脚手架模板里的 mock 引擎和评测插件已经在读它们。
//!    改键名等于破坏已分发的产物。
//! 3. **开放 schema**：数值/枚举字段尽量用 `String` + 类型化访问器（如 `Node::role_kind()`），
//!    未知键进 `extras`。这样导入别人的场景不会因为多一个键就解析失败。
//! 4. **H0 的 `aabb` 是世界空间几何代理**：H0 没有真实几何（那是 `io` 层的事），
//!    所以节点的包围盒直接就是它的世界包围盒。接入真实几何后节点会变成
//!    glTF 语义的 `transform + local geometry`，届时需要一次迁移（这是**计划内的**破坏性变更）。

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::fmt;

use crate::error::{CoreError, Result};

/// 场景 schema 标识。
pub const SCENE_SPEC: &str = "rsi3d-scene/v1";

/// glTF `extras` 的对应物：装我们不认识也不该丢的键。
pub type Extras = BTreeMap<String, serde_json::Value>;

// ---------------------------------------------------------------- 包围盒

/// 轴对齐包围盒（H0 的几何代理）。
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct Aabb {
    pub min: [f64; 3],
    pub max: [f64; 3],
}

impl Aabb {
    /// 构造并校验（有限、min <= max）。
    pub fn new(min: [f64; 3], max: [f64; 3]) -> Result<Self> {
        let b = Aabb { min, max };
        b.check()?;
        Ok(b)
    }

    /// 校验数值有限且 min <= max。
    pub fn check(&self) -> Result<()> {
        for (i, (lo, hi)) in self.min.iter().zip(self.max.iter()).enumerate() {
            if !lo.is_finite() || !hi.is_finite() {
                return Err(CoreError::NonFinite(format!("aabb 第 {} 轴", i)));
            }
            if lo > hi {
                return Err(CoreError::InvalidArgument(format!(
                    "aabb 第 {} 轴 min > max（{} > {}）",
                    i, lo, hi
                )));
            }
        }
        Ok(())
    }

    pub fn size(&self) -> [f64; 3] {
        [
            self.max[0] - self.min[0],
            self.max[1] - self.min[1],
            self.max[2] - self.min[2],
        ]
    }

    pub fn center(&self) -> [f64; 3] {
        [
            (self.min[0] + self.max[0]) / 2.0,
            (self.min[1] + self.max[1]) / 2.0,
            (self.min[2] + self.max[2]) / 2.0,
        ]
    }

    /// 平移（原地）。
    pub fn translate(&mut self, d: [f64; 3]) {
        for i in 0..3 {
            self.min[i] += d[i];
            self.max[i] += d[i];
        }
    }

    /// 绕**自身中心**沿 Y 轴旋转（角度制），取旋转后四角的包围盒（保守，只会变大）。
    ///
    /// 注意：AABB 旋转在代数上**不可逆**（旋转再反旋会得到更大的盒子），
    /// 所以命令的逆是「恢复原值」而不是「反向旋转」——见 `command.rs` 的说明。
    pub fn rotate_y_about_center(&mut self, deg: f64) {
        let c = self.center();
        let (sin, cos) = deg.to_radians().sin_cos();
        let xs = [self.min[0] - c[0], self.max[0] - c[0]];
        let zs = [self.min[2] - c[2], self.max[2] - c[2]];
        let mut min_x = f64::INFINITY;
        let mut max_x = f64::NEG_INFINITY;
        let mut min_z = f64::INFINITY;
        let mut max_z = f64::NEG_INFINITY;
        for x in xs {
            for z in zs {
                // 右手系、+Y 向上：x' = x·cos + z·sin，z' = -x·sin + z·cos
                let rx = x * cos + z * sin;
                let rz = -x * sin + z * cos;
                min_x = min_x.min(rx);
                max_x = max_x.max(rx);
                min_z = min_z.min(rz);
                max_z = max_z.max(rz);
            }
        }
        self.min = [c[0] + min_x, self.min[1], c[2] + min_z];
        self.max = [c[0] + max_x, self.max[1], c[2] + max_z];
    }

    /// 绕自身中心**等比**缩放（s 必须 > 0）。
    pub fn scale_about_center(&mut self, s: f64) -> Result<()> {
        if !s.is_finite() || s <= 0.0 {
            return Err(CoreError::InvalidArgument(format!(
                "scale 必须是正有限数，收到 {}",
                s
            )));
        }
        let c = self.center();
        let half = self.size();
        for i in 0..3 {
            let h = half[i] * s / 2.0;
            self.min[i] = c[i] - h;
            self.max[i] = c[i] + h;
        }
        Ok(())
    }

    /// self 是否完全包含 other。
    pub fn contains(&self, other: &Aabb) -> bool {
        (0..3).all(|i| self.min[i] <= other.min[i] && self.max[i] >= other.max[i])
    }

    /// 两个包围盒是否相交或接触。
    pub fn intersects(&self, other: &Aabb) -> bool {
        (0..3).all(|i| self.min[i] <= other.max[i] && self.max[i] >= other.min[i])
    }

    /// 两个包围盒的间距：分离轴上最小的正间隙；相交/接触返回 0。
    ///
    /// 与 `scaffolds/*/files/src/*.mjs` 里的同名算法保持一致（评测插件与内核口径必须相同）。
    pub fn gap(&self, other: &Aabb) -> f64 {
        let mut min_gap = f64::INFINITY;
        for i in 0..3 {
            let g = if self.max[i] < other.min[i] {
                other.min[i] - self.max[i]
            } else if other.max[i] < self.min[i] {
                self.min[i] - other.max[i]
            } else {
                0.0
            };
            if g > 0.0 {
                min_gap = min_gap.min(g);
            }
        }
        if min_gap.is_finite() {
            min_gap
        } else {
            0.0
        }
    }

    /// 序列化用：`[[min], [max]]`。
    pub fn to_array(&self) -> [[f64; 3]; 2] {
        [self.min, self.max]
    }
}

// ---------------------------------------------------------------- 节点

/// 节点在场景中的角色。用字符串存（开放 schema），用 `RoleKind` 做类型化判断。
pub const ROLE_FURNITURE: &str = "furniture";
pub const ROLE_FLOOR: &str = "floor";
pub const ROLE_DECOR: &str = "decor";
pub const ROLE_STRUCTURE: &str = "structure";

/// 角色枚举（用于判定规则，不做 schema 约束）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum RoleKind {
    Furniture,
    Floor,
    Decor,
    Structure,
    Other,
}

impl RoleKind {
    pub fn parse(raw: &str) -> Self {
        match raw.trim().to_ascii_lowercase().as_str() {
            ROLE_FURNITURE => RoleKind::Furniture,
            ROLE_FLOOR => RoleKind::Floor,
            ROLE_DECOR => RoleKind::Decor,
            ROLE_STRUCTURE => RoleKind::Structure,
            _ => RoleKind::Other,
        }
    }

    /// 是否参与「家具之间的间距/相交」这类规则（地毯这类可以踩的排除）。
    pub fn is_solid(&self) -> bool {
        matches!(self, RoleKind::Furniture | RoleKind::Structure | RoleKind::Decor)
    }
}

/// 表示层：同一节点的不同承载方式（对应 glTF 同一 node 的不同 primitive）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "lowercase")]
pub enum LayerKind {
    /// 三角网格
    Mesh,
    /// 3D 高斯（`KHR_gaussian_splatting`）
    Gaussian,
    /// 点云
    Points,
    /// 光源（挂在节点上时）
    Light,
    /// 相机
    Camera,
}

/// 可编辑程度。**由层推导，不单独存**（避免两处真相）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "kebab-case")]
pub enum Editability {
    /// 可局部编辑
    Full,
    /// 只能裁剪 / 整体替换（高斯、点云的常见处境）
    CropOnly,
    /// 只能整体替换（重训产物）
    ReplaceOnly,
}

impl LayerKind {
    /// 该层天然的可编辑上限（见 `docs/core.md` §3.2）。
    pub fn editability(self) -> Editability {
        match self {
            LayerKind::Mesh | LayerKind::Light | LayerKind::Camera => Editability::Full,
            LayerKind::Gaussian | LayerKind::Points => Editability::CropOnly,
        }
    }
}

impl Editability {
    /// 与 JSON（kebab-case）**同字面**的短名，便于文本输出与日志对齐。
    pub fn as_str(self) -> &'static str {
        match self {
            Editability::Full => "full",
            Editability::CropOnly => "crop-only",
            Editability::ReplaceOnly => "replace-only",
        }
    }
}

impl fmt::Display for Editability {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// 材质数值参数（H0 只需要这几个）。
#[derive(Debug, Clone, Copy, Default, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct MaterialParams {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub roughness: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub metallic: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub opacity: Option<f64>,
}

impl MaterialParams {
    pub fn is_empty(&self) -> bool {
        self.roughness.is_none() && self.metallic.is_none() && self.opacity.is_none()
    }

    /// 值域检查（0..1 为主；roughness 允许 0..1）。
    pub fn check(&self) -> Result<()> {
        for (name, v) in [
            ("roughness", self.roughness),
            ("metallic", self.metallic),
            ("opacity", self.opacity),
        ] {
            if let Some(v) = v {
                if !v.is_finite() {
                    return Err(CoreError::NonFinite(format!("material.{}", name)));
                }
                if !(0.0..=1.0).contains(&v) {
                    return Err(CoreError::InvalidArgument(format!(
                        "material.{} 需在 0..1，收到 {}",
                        name, v
                    )));
                }
            }
        }
        Ok(())
    }
}

/// 来源：谁造的这个节点（用于归因与「换资产」判定）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "lowercase")]
pub enum Origin {
    #[default]
    Unknown,
    Imported,
    Generated,
    Edited,
}

/// 来源信息。H3 引入真实几何后，这里会带 mesh 哈希与算法版本。
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct Provenance {
    #[serde(default, skip_serializing_if = "is_unknown_origin")]
    pub origin: Origin,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source: Option<String>,
    /// 导入时的来源坐标系（PLY 常为 RDF、GLB 常为 LUF、SPZ 默认 RUB…）——转系时 SH 要用 Wigner-D 旋转。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_coordinate_system: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub created_at_rev: Option<u32>,
}

fn is_unknown_origin(o: &Origin) -> bool {
    *o == Origin::Unknown
}

/// 规模摘要（H0 只填三角面，其余留给 H3）。
#[derive(Debug, Clone, Copy, Default, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct Metrics {
    #[serde(default, skip_serializing_if = "is_zero")]
    pub triangles: u64,
    #[serde(default, skip_serializing_if = "is_zero")]
    pub gaussians: u64,
    #[serde(default, skip_serializing_if = "is_zero")]
    pub points: u64,
    /// 是否流形/水密（H3 接 `manifold` 后才有真值）
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub watertight: Option<bool>,
}

fn is_zero(v: &u64) -> bool {
    *v == 0
}

impl Metrics {
    pub fn is_empty(&self) -> bool {
        self.triangles == 0 && self.gaussians == 0 && self.points == 0 && self.watertight.is_none()
    }
}

/// 场景节点。
///
/// `id` 是 **stable_id**：Agent 的对话会跨越很多轮，`obj:sofa_01` 在多次 `edit` 之后
/// 必须仍然指向同一把沙发——这是 diff、归因、回滚的共同前提。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct Node {
    pub id: String,
    #[serde(default = "default_role")]
    pub role: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub material: Option<String>,
    #[serde(default, skip_serializing_if = "MaterialParams::is_empty")]
    pub material_params: MaterialParams,
    /// H0 的世界空间几何代理。
    pub aabb: Aabb,
    #[serde(default = "default_layers")]
    pub layers: Vec<LayerKind>,
    /// 由生产者**显式声明**的可编辑性上限（可选）。
    ///
    /// 存在的意义：有些资产本身就是烘焙结果（例如在世界坐标里训练出来的高斯），
    /// 它能被「整体替换」但不能被单独移动——这件事只有生产者知道，推导不出来。
    /// 最终取值 = `max(声明值, 由层推导值)`，即**取最受限的那个**。
    #[serde(rename = "editability", default, skip_serializing_if = "Option::is_none")]
    pub editability_cap: Option<Editability>,
    #[serde(default, skip_serializing_if = "is_default_provenance")]
    pub provenance: Provenance,
    #[serde(default, skip_serializing_if = "Metrics::is_empty")]
    pub metrics: Metrics,
    #[serde(flatten)]
    pub extras: Extras,
}

fn default_role() -> String {
    ROLE_FURNITURE.to_string()
}

fn default_layers() -> Vec<LayerKind> {
    vec![LayerKind::Mesh]
}

fn is_default_provenance(p: &Provenance) -> bool {
    *p == Provenance::default()
}

impl Node {
    /// 新节点（provenance 记为「本轮创建」）。
    pub fn new(id: impl Into<String>, role: &str, aabb: Aabb) -> Self {
        Node {
            id: id.into(),
            role: role.to_string(),
            material: None,
            material_params: MaterialParams::default(),
            aabb,
            layers: default_layers(),
            editability_cap: None,
            provenance: Provenance {
                origin: Origin::Imported,
                ..Default::default()
            },
            metrics: Metrics::default(),
            extras: Extras::new(),
        }
    }

    pub fn role_kind(&self) -> RoleKind {
        RoleKind::parse(&self.role)
    }

    /// 该节点实际的可编辑程度 = `max(声明值, 各层推导值里最受限的)`。
    ///
    /// 用 `max` 是因为 [`Editability`] 的排序按「受限程度」递增：
    /// `Full < CropOnly < ReplaceOnly`。两个来源取更受限的那个，才不会让某一方把门槛偷偷放宽。
    pub fn editability(&self) -> Editability {
        let derived = self
            .layers
            .iter()
            .map(|l| l.editability())
            .max()
            .unwrap_or(Editability::Full);
        match self.editability_cap {
            Some(declared) => derived.max(declared),
            None => derived,
        }
    }

    /// 是否所有数值都合法。
    pub fn check(&self) -> Result<()> {
        if self.id.trim().is_empty() {
            return Err(CoreError::InvalidArgument("节点 id 不能为空".into()));
        }
        self.aabb.check()?;
        self.material_params.check()?;
        if self.layers.is_empty() {
            return Err(CoreError::InvalidArgument(format!(
                "节点 {} 没有任何表示层",
                self.id
            )));
        }
        Ok(())
    }
}

/// 把裸名字补成节点 id（`sofa_01` → `obj:sofa_01`）。
pub fn normalize_node_id(raw: &str) -> String {
    let t = raw.trim();
    if t.contains(':') {
        t.to_string()
    } else {
        format!("obj:{}", t)
    }
}

// ---------------------------------------------------------------- 灯光 / 房间 / 规则

/// 灯光。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct Light {
    pub id: String,
    /// `directional` | `hdri` | …（开放）
    #[serde(default = "default_light_kind")]
    pub kind: String,
    #[serde(default = "default_intensity")]
    pub intensity: f64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub color: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub direction: Option<[f64; 3]>,
    #[serde(flatten)]
    pub extras: Extras,
}

fn default_light_kind() -> String {
    "directional".to_string()
}

fn default_intensity() -> f64 {
    1.0
}

/// 窗（H0 单窗；与已发布模板的 `window` 键一致）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "camelCase")]
pub struct Window {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub wall: Option<String>,
    /// 窗在 X 轴上的跨度 [x0, x1]
    pub span_x: [f64; 2],
    pub height: f64,
    /// 窗所在平面的 Z 坐标
    pub z: f64,
    /// 窗前多少米内算「挡光带」
    #[serde(default = "default_band_depth")]
    pub band_depth: f64,
    #[serde(flatten)]
    pub extras: Extras,
}

fn default_band_depth() -> f64 {
    1.5
}

/// 房间。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "camelCase")]
pub struct Room {
    pub size: [f64; 3],
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ceiling: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub wall_color: Option<String>,
    #[serde(flatten)]
    pub extras: Extras,
}

impl Room {
    /// 以原点为中心、地面 y=0 的房间包围盒。
    pub fn bounds(&self) -> Aabb {
        let [sx, sy, sz] = self.size;
        Aabb {
            min: [-sx / 2.0, 0.0, -sz / 2.0],
            max: [sx / 2.0, sy, sz / 2.0],
        }
    }
}

/// 间距规则：**行业知识被显式化的地方**（太远和太近都算违规）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct ClearanceRule {
    pub pair: [String; 2],
    pub min: f64,
    pub max: f64,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub reason: String,
    #[serde(flatten)]
    pub extras: Extras,
}

impl ClearanceRule {
    /// 间距到合法区间的距离（区间内为 0）。
    pub fn distance(&self, gap: f64) -> f64 {
        if gap < self.min {
            self.min - gap
        } else if gap > self.max {
            gap - self.max
        } else {
            0.0
        }
    }

    /// 区间中点（Agent 修间距时的目标值）。
    pub fn ideal(&self) -> f64 {
        (self.min + self.max) / 2.0
    }
}

/// 意图关键词核查（H0 的语义维度）
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct IntentKeyword {
    pub word: String,
    #[serde(default)]
    pub present: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
}

// ---------------------------------------------------------------- 场景

/// 场景 = 一棵节点树 + 环境 + 行业规则。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct Scene {
    #[serde(default = "default_spec")]
    pub spec: String,
    #[serde(default = "default_units")]
    pub units: String,
    #[serde(default = "default_up_axis")]
    pub up_axis: String,
    /// 内部统一坐标系（glTF 约定：右手、+Y 向上）。导入时必须归一化到这里。
    #[serde(default = "default_coordinate_system")]
    pub coordinate_system: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_coordinate_system: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub intent: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub room: Option<Room>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub window: Option<Window>,
    /// 节点列表。JSON 键沿用 `objects`（已发布模板的契约），也接受 `nodes` 作为别名。
    #[serde(default, alias = "nodes")]
    pub objects: Vec<Node>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub lights: Vec<Light>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub clearance_rules: Vec<ClearanceRule>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub intent_keywords: Vec<IntentKeyword>,
    #[serde(flatten)]
    pub extras: Extras,
}

fn default_spec() -> String {
    SCENE_SPEC.to_string()
}
fn default_units() -> String {
    "m".to_string()
}
fn default_up_axis() -> String {
    "Y".to_string()
}
fn default_coordinate_system() -> String {
    // glTF 约定：Right-handed, Up = +Y, Back = +Z
    "RUB".to_string()
}

impl Default for Scene {
    fn default() -> Self {
        Scene {
            spec: default_spec(),
            units: default_units(),
            up_axis: default_up_axis(),
            coordinate_system: default_coordinate_system(),
            source_coordinate_system: None,
            intent: None,
            room: None,
            window: None,
            objects: Vec::new(),
            lights: Vec::new(),
            clearance_rules: Vec::new(),
            intent_keywords: Vec::new(),
            extras: Extras::new(),
        }
    }
}

impl Scene {
    /// 从 JSON 解析（校验基本一致性）。
    pub fn from_json(raw: &str) -> Result<Self> {
        let scene: Scene =
            serde_json::from_str(raw).map_err(|e| CoreError::BadScene(e.to_string()))?;
        scene.check()?;
        Ok(scene)
    }

    /// 基本一致性校验。
    pub fn check(&self) -> Result<()> {
        if self.spec != SCENE_SPEC {
            return Err(CoreError::BadScene(format!(
                "spec 应为 {}，收到 {}",
                SCENE_SPEC, self.spec
            )));
        }
        let mut seen: Vec<&str> = Vec::new();
        for n in &self.objects {
            n.check()?;
            if seen.contains(&n.id.as_str()) {
                return Err(CoreError::InvalidArgument(format!(
                    "节点 id 重复：{}（stable_id 必须唯一）",
                    n.id
                )));
            }
            seen.push(&n.id);
        }
        let mut lids: Vec<&str> = Vec::new();
        for l in &self.lights {
            if !l.intensity.is_finite() {
                return Err(CoreError::NonFinite(format!("灯光 {} 的 intensity", l.id)));
            }
            if lids.contains(&l.id.as_str()) {
                return Err(CoreError::InvalidArgument(format!("灯光 id 重复：{}", l.id)));
            }
            lids.push(&l.id);
        }
        for r in &self.clearance_rules {
            if !r.min.is_finite() || !r.max.is_finite() {
                return Err(CoreError::NonFinite("clearance rule 的 min/max".into()));
            }
            if r.min > r.max {
                return Err(CoreError::InvalidArgument(format!(
                    "规则 {}–{} 的 min > max",
                    r.pair[0], r.pair[1]
                )));
            }
        }
        Ok(())
    }

    /// 规范化 JSON（键排序、紧凑），**用作哈希与 diff 的输入**。
    pub fn canonical_json(&self) -> Result<String> {
        // serde_json 的 Map 默认是 BTreeMap → 键有序；经 to_value 再序列化可摆脱字段声明顺序
        let v = serde_json::to_value(self).map_err(|e| CoreError::Invariant(e.to_string()))?;
        serde_json::to_string(&v).map_err(|e| CoreError::Invariant(e.to_string()))
    }

    /// 场景哈希（sha256 hex）。同内容必同哈希——这是「可复现」的地基。
    pub fn scene_hash(&self) -> Result<String> {
        Ok(crate::sha256_hex(self.canonical_json()?.as_bytes()))
    }

    /// 节点下标（支持裸名补 `obj:` 前缀）。
    pub fn node_index(&self, target: &str) -> Option<usize> {
        let id = normalize_node_id(target);
        self.objects.iter().position(|n| n.id == id)
    }

    pub fn node(&self, target: &str) -> Option<&Node> {
        self.node_index(target).map(|i| &self.objects[i])
    }

    /// 灯光下标。
    pub fn light_index(&self, target: &str) -> Option<usize> {
        let t = target.trim();
        self.lights.iter().position(|l| l.id == t)
    }

    /// 落在窗前挡光带里的节点 id（有序，稳定）。
    pub fn window_blockers(&self) -> Vec<String> {
        let Some(w) = &self.window else {
            return Vec::new();
        };
        let band = [w.z, w.z + w.band_depth];
        let mut out = Vec::new();
        for n in &self.objects {
            if !n.role_kind().is_solid() {
                continue;
            }
            let a = n.aabb;
            let in_band = a.max[2] > band[0] && a.min[2] < band[1];
            let x_overlap =
                (a.max[0].min(w.span_x[1]) - a.min[0].max(w.span_x[0])).max(0.0);
            let y_overlap = (a.max[1].min(w.height) - a.min[1].max(0.0)).max(0.0);
            if in_band && x_overlap > 0.0 && y_overlap > 0.0 {
                out.push(n.id.clone());
            }
        }
        out
    }
}
