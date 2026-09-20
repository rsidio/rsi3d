//! 给 Agent 与评测插件看的**扁平视图**（evaluator-facing view）。
//!
//! 为什么单独一层：已发布的脚手架模板里，评测插件（`harness-plugin/files/src/evaluator.mjs`）
//! 与 mock 引擎读的是 `scene.objects[].aabb / role / material`。内核内部结构会演进
//! （H3 会引入真实几何与 local/transform 的分离），但**这一层必须稳定**——
//! 它是我们与插件作者之间的契约。

use serde::{Deserialize, Serialize};

use crate::scene::{
    Aabb, ClearanceRule, Editability, IntentKeyword, LayerKind, Light, MaterialParams, Room, Scene,
    Window,
};

/// 节点在视图里的样子。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct NodeView {
    pub id: String,
    pub role: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub material: Option<String>,
    #[serde(default, skip_serializing_if = "MaterialParams::is_empty")]
    pub material_params: MaterialParams,
    /// H0 是世界空间几何代理（评测规则直接读它算间距/遮挡）
    pub aabb: Aabb,
    pub layers: Vec<LayerKind>,
    /// 该节点实际可编辑到什么程度（**诚实声明**，见 `docs/core.md` §3.2）
    pub editability: Editability,
}

/// 场景视图：评测与 Agent 观测的直接输入。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct SceneView {
    pub spec: String,
    pub units: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub intent: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub room: Option<Room>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub window: Option<Window>,
    pub objects: Vec<NodeView>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub lights: Vec<Light>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub clearance_rules: Vec<ClearanceRule>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub intent_keywords: Vec<IntentKeyword>,
    /// 落在窗前挡光带里的节点（Agent 修「挡光」时最需要的事实）
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub blockers: Vec<String>,
    /// 规模摘要（对应 `observe.summary` 的用途）
    pub object_count: usize,
    pub solid_count: usize,
}

impl Scene {
    /// 生成扁平视图。
    pub fn view(&self) -> SceneView {
        SceneView {
            spec: self.spec.clone(),
            units: self.units.clone(),
            intent: self.intent.clone(),
            room: self.room.clone(),
            window: self.window.clone(),
            objects: self
                .objects
                .iter()
                .map(|n| NodeView {
                    id: n.id.clone(),
                    role: n.role.clone(),
                    material: n.material.clone(),
                    material_params: n.material_params,
                    aabb: n.aabb,
                    layers: n.layers.clone(),
                    editability: n.editability(),
                })
                .collect(),
            lights: self.lights.clone(),
            clearance_rules: self.clearance_rules.clone(),
            intent_keywords: self.intent_keywords.clone(),
            blockers: self.window_blockers(),
            object_count: self.objects.len(),
            solid_count: self
                .objects
                .iter()
                .filter(|n| n.role_kind().is_solid())
                .count(),
        }
    }
}
