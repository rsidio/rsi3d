//! 相机与视角。
//!
//! H0 的观测契约要的是**固定、可复现**的多视角，而不是"随便飞一飞"。
//! 所以视角是**从房间包围盒推导**出来的（不存相机参数）：同一个场景永远得到同四张图。

use serde::{Deserialize, Serialize};

use rsi3d_harness_core::{Aabb, Scene};

use crate::math::{Mat4, Vec3};

/// 预置视角。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ViewKind {
    /// 俯视（正交）= 平面布置图。+X 向右、+Z 向下（**北在上**），最适合判读"挡不挡窗"。
    Top,
    /// 正立面（正交）：从 +Z 看向 -Z，看到的是"朝着窗户"的那面。
    Front,
    /// 西南 45° 斜视（透视）。
    IsoSw,
    /// 东南 45° 斜视（透视）。
    IsoSe,
}

impl ViewKind {
    pub fn as_str(self) -> &'static str {
        match self {
            ViewKind::Top => "top",
            ViewKind::Front => "front",
            ViewKind::IsoSw => "iso-sw",
            ViewKind::IsoSe => "iso-se",
        }
    }

    pub fn parse(raw: &str) -> Option<Self> {
        match raw.trim().to_ascii_lowercase().as_str() {
            "top" | "plan" => Some(ViewKind::Top),
            "front" | "elevation" => Some(ViewKind::Front),
            "iso-sw" | "iso" | "sw" => Some(ViewKind::IsoSw),
            "iso-se" | "se" => Some(ViewKind::IsoSe),
            _ => None,
        }
    }

    /// 四个标准视角（观测契约里的"4 张快照"）。
    pub fn all() -> [ViewKind; 4] {
        [ViewKind::Top, ViewKind::Front, ViewKind::IsoSw, ViewKind::IsoSe]
    }

    pub fn is_orthographic(self) -> bool {
        matches!(self, ViewKind::Top | ViewKind::Front)
    }
}

/// 一台可立即用于光栅化的相机（视图 × 投影已合并）。
#[derive(Debug, Clone, Copy)]
pub struct Camera {
    pub kind: ViewKind,
    pub view_proj: Mat4,
    /// 相机位置（世界系）——着色时算视线方向要用
    pub eye: Vec3,
}

impl Camera {
    /// 从场景包围盒推导该视角的相机。
    ///
    /// 刻意让相机**退到包围盒之外**：这样所有几何都在近平面之前，
    /// 光栅器就不需要处理"三角形跨越近平面"这种要写裁剪的情况。
    pub fn for_scene(scene: &Scene, kind: ViewKind, aspect: f64) -> Camera {
        let (min, max) = scene_extent(scene);
        let center = (min + max) * 0.5;
        let size = max - min;
        let radius = (size.length() * 0.5).max(0.5);

        match kind {
            ViewKind::Top => {
                let eye = Vec3::new(center.x, max.y + radius * 2.0 + 1.0, center.z);
                let view = Mat4::look_at(eye, Vec3::new(center.x, center.y, center.z), Vec3::new(0.0, 0.0, -1.0));
                // 紧贴内容取景：先算两个方向各需多少，再取能同时满足的那个（保长宽比，不拉伸）
                let need_x = size.x * 0.5 * 1.15 + 0.15;
                let need_y = size.z * 0.5 * 1.15 + 0.15; // 竖直方向要盖住 z 跨度
                let half_y = need_y.max(need_x / aspect);
                let half_x = half_y * aspect;
                let proj = Mat4::orthographic(-half_x, half_x, -half_y, half_y, 0.1, radius * 6.0 + 10.0);
                Camera { kind, view_proj: proj.mul(&view), eye }
            }
            ViewKind::Front => {
                let eye = Vec3::new(center.x, center.y, max.z + radius * 2.0 + 1.0);
                let view = Mat4::look_at(eye, Vec3::new(center.x, center.y, center.z), Vec3::new(0.0, 1.0, 0.0));
                let need_x = size.x * 0.5 * 1.15 + 0.15;
                let need_y = size.y * 0.5 * 1.15 + 0.15;
                let half_y = need_y.max(need_x / aspect);
                let half_x = half_y * aspect;
                let proj = Mat4::orthographic(-half_x, half_x, -half_y, half_y, 0.1, radius * 6.0 + 10.0);
                Camera { kind, view_proj: proj.mul(&view), eye }
            }
            ViewKind::IsoSw | ViewKind::IsoSe => {
                // 45° 方位、30° 仰角；距离按「包围球正好落进竖直视场」算。
                // 之前用固定倍数（radius*3.4）是拍脑袋的，结果场景只占画面一小块。
                let sign = if kind == ViewKind::IsoSw { -1.0 } else { 1.0 };
                let fov_deg = 45.0_f64;
                let fov = fov_deg.to_radians();
                let dist = radius / (fov / 2.0).sin() * 1.12;
                let az = 45.0_f64.to_radians();
                let el = 30.0_f64.to_radians();
                let eye = Vec3::new(
                    center.x + sign * dist * az.cos() * el.cos(),
                    center.y + dist * el.sin(),
                    center.z + dist * az.sin() * el.cos(),
                );
                let view = Mat4::look_at(eye, center, Vec3::new(0.0, 1.0, 0.0));
                let proj = Mat4::perspective(fov_deg, aspect, 0.05, dist * 3.0 + radius * 4.0);
                Camera { kind, view_proj: proj.mul(&view), eye }
            }
        }
    }
}

/// 场景在世界系里的包围盒：优先用房间，没有房间就退化成所有节点的并集。
pub fn scene_extent(scene: &Scene) -> (Vec3, Vec3) {
    if let Some(room) = &scene.room {
        let b: Aabb = room.bounds();
        return (Vec3::from_arr(b.min), Vec3::from_arr(b.max));
    }
    let mut min = Vec3::splat(f64::INFINITY);
    let mut max = Vec3::splat(f64::NEG_INFINITY);
    for n in &scene.objects {
        min = min.min(Vec3::from_arr(n.aabb.min));
        max = max.max(Vec3::from_arr(n.aabb.max));
    }
    if !min.x.is_finite() {
        // 空场景：给一个 1m 的默认体量，免得相机退化
        return (Vec3::new(-0.5, 0.0, -0.5), Vec3::new(0.5, 1.0, 0.5));
    }
    (min, max)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn view_kind_roundtrip() {
        for k in ViewKind::all() {
            assert_eq!(ViewKind::parse(k.as_str()), Some(k));
        }
        assert_eq!(ViewKind::parse("plan"), Some(ViewKind::Top));
        assert_eq!(ViewKind::parse("nope"), None);
    }

    #[test]
    fn empty_scene_still_gives_a_camera() {
        let c = Camera::for_scene(&Scene::default(), ViewKind::IsoSw, 16.0 / 9.0);
        assert!(c.eye.y > 0.0);
    }
}
