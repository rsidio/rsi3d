//! # rsi3d-harness 内核
//!
//! **引擎的唯一真相来源**：Agent 的每次改动都是一条命令，命令进日志，日志能重放出状态。
//!
//! 这个 crate **不含渲染、不含 IO、不含网络、不含 LLM**——所以它能被单测穷尽，
//! 也能同时编译到 `wasm32` 与 native（两条约束：内核不依赖宿主；同一个内核跑在两个壳里不漂移）。
//!
//! ## 三条不变量（`tests/invariants.rs` 逐条锁死）
//!
//! | 不变量 | 含义 | 为什么重要 |
//! | --- | --- | --- |
//! | **可逆** | `apply(cmd)` 后 `apply(inverse)` 回到**逐字段相同**的状态 | RSI 要能回到 best-so-far |
//! | **可重放** | `replay()` 与实时状态逐字段相同；快照与重放结果一致 | 分数曲线的每一步都能归因 |
//! | **确定** | `scene_hash()` 稳定；日志哈希不含墙钟与随机数 | 日志要进平台账本，必须可比 |
//!
//! ## 五分钟上手
//!
//! ```
//! use rsi3d_harness_core::{Aabb, Command, Document, Node, Scene};
//!
//! let mut scene = Scene::default();
//! scene.objects.push(Node::new(
//!     "obj:sofa_01",
//!     "furniture",
//!     Aabb::new([-1.1, 0.0, -2.6], [0.9, 0.85, -1.7]).unwrap(),
//! ));
//! let mut doc = Document::new(scene).unwrap();
//!
//! // 挪开 1.3m
//! let applied = doc.apply(Command::Transform {
//!     target: "sofa_01".into(),          // 裸名会自动补 obj: 前缀
//!     translate: Some([0.0, 0.0, 1.3]),
//!     rotate_y_deg: None,
//!     scale: None,
//! });
//! let applied = applied.unwrap();
//! assert_eq!(applied.revision, 1);
//!
//! // 撤销：内核从前置状态算出的精确逆命令
//! doc.undo().unwrap();
//! let back = doc.scene().node("sofa_01").unwrap();
//! assert_eq!(back.aabb.min, [-1.1, 0.0, -2.6]);
//! ```
//!
//! ## 与 glTF 的关系
//!
//! USC 刻意与 glTF 2.0 同构（node ↔ glTF node，「层」↔ `mesh.primitive`），
//! 3DGS 层用 Khronos 已 Ratified 的 `KHR_gaussian_splatting` 语义。
//! 我们自己的字段（`provenance` / `layers` / `metrics`）落盘时进 glTF 的 `extras`。
//!
//! 选这个方向的依据：Khronos 的 `KHR_gaussian_splatting` 已经 Ratified，
//! 所以 3DGS 有现成的标准化落点——**不自造格式**。
//!
//! 详见 [`docs/core.md`](../../docs/core.md)。

pub mod command;
pub mod document;
pub mod error;
pub mod report;
pub mod scene;
pub mod validate;
pub mod view;

pub use command::{Applied, AttributionRow, Command, CommandRequest, OpEntry, Params, Restore};
pub use document::{
    align_clearance, apply_to_scene, set_light_intensity, set_material_name, transform, Change,
    Diff, Document, DOCUMENT_SPEC,
};
pub use error::{CoreError, Result};
pub use report::{
    applied_json, applied_text, attribution_json, diff_text, history_text, observe_json,
    scene_summary, status_line, verify_report, VerifyReport,
};
pub use scene::{
    normalize_node_id, Aabb, ClearanceRule, Editability, Extras, IntentKeyword, LayerKind, Light,
    MaterialParams, Metrics, Node, Origin, Provenance, RoleKind, Room, Scene, Window, SCENE_SPEC,
};
pub use validate::{validate, Warning};
pub use view::{NodeView, SceneView};

use sha2::{Digest, Sha256};

/// sha256 → 小写十六进制（与平台侧制品校验和**同口径**，便于跨组件比对）。
pub fn sha256_hex(bytes: &[u8]) -> String {
    let mut h = Sha256::new();
    h.update(bytes);
    h.finalize()
        .iter()
        .map(|b| format!("{:02x}", b))
        .collect()
}

#[cfg(test)]
mod unit_tests {
    use super::*;

    #[test]
    fn aabb_gap_matches_plugin_semantics() {
        let a = Aabb::new([0.0, 0.0, 0.0], [1.0, 1.0, 1.0]).unwrap();
        let b = Aabb::new([0.0, 0.0, 2.0], [1.0, 1.0, 3.0]).unwrap();
        // 在 z 上分开 1.0
        assert!((a.gap(&b) - 1.0).abs() < 1e-9);
        // 相交（同 x/y 区间，z 重叠）
        let c = Aabb::new([0.5, 0.5, 0.5], [1.5, 1.5, 1.5]).unwrap();
        assert_eq!(a.gap(&c), 0.0);
        assert!(a.intersects(&c));
        // 斜向分离取最小间隙
        let d = Aabb::new([2.0, 2.0, 3.0], [3.0, 3.0, 4.0]).unwrap();
        assert!((a.gap(&d) - 1.0).abs() < 1e-9);
    }

    #[test]
    fn rotate_about_center_keeps_center() {
        let mut a = Aabb::new([0.0, 0.0, 0.0], [2.0, 1.0, 2.0]).unwrap();
        let c0 = a.center();
        a.rotate_y_about_center(30.0);
        let c1 = a.center();
        // 中心只在浮点尾差范围内保持（不做精确相等——组合算术本来就会有尾差）
        for i in 0..3 {
            assert!(
                (c0[i] - c1[i]).abs() < 1e-9,
                "第 {} 轴中心漂移了：{:?} → {:?}",
                i,
                c0,
                c1
            );
        }
        // 30° 旋转后 xz 范围必然变大（保守包围）
        assert!(a.size()[0] > 2.0 - 1e-9);
        assert!(a.size()[2] > 2.0 - 1e-9);
    }

    #[test]
    fn scale_rejects_non_positive() {
        let mut a = Aabb::new([0.0, 0.0, 0.0], [1.0, 1.0, 1.0]).unwrap();
        assert!(a.scale_about_center(0.0).is_err());
        assert!(a.scale_about_center(-1.0).is_err());
        a.scale_about_center(2.0).unwrap();
        assert_eq!(a.size(), [2.0, 2.0, 2.0]);
        assert_eq!(a.center(), [0.5, 0.5, 0.5]);
    }

    #[test]
    fn node_id_normalization() {
        assert_eq!(normalize_node_id("sofa_01"), "obj:sofa_01");
        assert_eq!(normalize_node_id(" obj:sofa_01 "), "obj:sofa_01");
        assert_eq!(normalize_node_id("light:sun"), "light:sun");
    }

    #[test]
    fn editability_derives_from_layers() {
        let mut n = Node::new("obj:x", "furniture", Aabb::new([0.0; 3], [1.0; 3]).unwrap());
        assert_eq!(n.editability(), Editability::Full); // 默认 mesh
        n.layers = vec![LayerKind::Mesh, LayerKind::Gaussian];
        assert_eq!(n.editability(), Editability::CropOnly); // 取最受限
        n.layers = vec![LayerKind::Gaussian];
        assert_eq!(n.editability(), Editability::CropOnly);
    }

    #[test]
    fn sha256_matches_known_vector() {
        assert_eq!(
            sha256_hex(b"abc"),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
    }
}
