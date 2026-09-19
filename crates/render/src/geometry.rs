//! 从场景生成可画的几何。
//!
//! H0 没有真实几何（那是 `io` 的事），所以这里把节点的 `aabb` 变成闭合盒子。
//! 这不是"凑合"：包围盒本来就是这一阶段**唯一**的几何真相，
//! 而且盒子足以让「沙发挡窗」这件事在图上**一眼可判读**。
//!
//! `io` 落地后，这里换成真实网格即可——光栅器与观测契约都不用改。

use rsi3d_harness_core::{Aabb, Scene, Window};

use crate::math::Vec3;
use crate::raster::{Rgb, Triangle};
use crate::shading::base_color;

/// 特殊 id（真实对象用 1..=N）。
pub mod ids {
    /// 背景
    pub const BACKGROUND: u32 = 0;
    /// 窗（发光面）
    pub const WINDOW: u32 = 0xFFFF_FFF0;
    /// 窗前挡光带（地面标记）
    pub const BAND: u32 = 0xFFFF_FFF1;
    /// 房间线框
    pub const LINE: u32 = 0xFFFF_FFF2;
    /// 房间地面（实心面；没有它 3D 视图里家具像浮在空中）
    pub const FLOOR: u32 = 0xFFFF_FFF3;

    /// 是不是"真实对象"（1..=N）。
    pub fn is_object(id: u32) -> bool {
        id != BACKGROUND && id < 0xFFFF_FF00
    }
}

/// 一个 AABB → 12 个三角形（每面 2 个），法线朝外。
pub fn box_triangles(aabb: &Aabb, color: Rgb, id: u32) -> Vec<Triangle> {
    let min = Vec3::from_arr(aabb.min);
    let max = Vec3::from_arr(aabb.max);
    let (x0, y0, z0) = (min.x, min.y, min.z);
    let (x1, y1, z1) = (max.x, max.y, max.z);

    // 八个角
    let c = [
        Vec3::new(x0, y0, z0),
        Vec3::new(x1, y0, z0),
        Vec3::new(x1, y0, z1),
        Vec3::new(x0, y0, z1),
        Vec3::new(x0, y1, z0),
        Vec3::new(x1, y1, z0),
        Vec3::new(x1, y1, z1),
        Vec3::new(x0, y1, z1),
    ];
    // 六个面（每个面四个角 + 外法线）
    let faces: [([usize; 4], Vec3); 6] = [
        ([0, 3, 2, 1], Vec3::new(0.0, -1.0, 0.0)), // 底
        ([4, 5, 6, 7], Vec3::new(0.0, 1.0, 0.0)),  // 顶
        ([0, 1, 5, 4], Vec3::new(0.0, 0.0, -1.0)), // 北（-Z）
        ([3, 7, 6, 2], Vec3::new(0.0, 0.0, 1.0)),  // 南（+Z）
        ([0, 4, 7, 3], Vec3::new(-1.0, 0.0, 0.0)), // 西（-X）
        ([1, 2, 6, 5], Vec3::new(1.0, 0.0, 0.0)),  // 东（+X）
    ];

    let mut out = Vec::with_capacity(12);
    for (quad, normal) in faces {
        for tri in [[quad[0], quad[1], quad[2]], [quad[0], quad[2], quad[3]]] {
            out.push(Triangle {
                a: c[tri[0]],
                b: c[tri[1]],
                c: c[tri[2]],
                normal,
                color,
                id,
                emissive: false,
            });
        }
    }
    out
}

/// 房间线框（12 条棱的端点对）。
pub fn room_edges(size: [f64; 3]) -> Vec<(Vec3, Vec3)> {
    let (sx, sy, sz) = (size[0] / 2.0, size[1] / 2.0, size[2] / 2.0);
    let c = [
        Vec3::new(-sx, 0.0, -sz),
        Vec3::new(sx, 0.0, -sz),
        Vec3::new(sx, 0.0, sz),
        Vec3::new(-sx, 0.0, sz),
        Vec3::new(-sx, sy, -sz),
        Vec3::new(sx, sy, -sz),
        Vec3::new(sx, sy, sz),
        Vec3::new(-sx, sy, sz),
    ];
    let pairs = [
        (0, 1), (1, 2), (2, 3), (3, 0), // 地面四边
        (4, 5), (5, 6), (6, 7), (7, 4), // 顶面四边
        (0, 4), (1, 5), (2, 6), (3, 7), // 四根竖棱
    ];
    pairs.iter().map(|(a, b)| (c[*a], c[*b])).collect()
}

/// 窗：贴在墙上的一个发光四边形（让"挡没挡住窗"在图上直接可判读）。
pub fn window_quad(w: &Window) -> Vec<Triangle> {
    let x0 = w.span_x[0];
    let x1 = w.span_x[1];
    let y0 = 0.0;
    let y1 = w.height;
    let z = w.z;
    // 略微朝房间内偏一点，避免与墙面线框打架
    let z = if z < 0.0 { z + 0.01 } else { z - 0.01 };
    let a = Vec3::new(x0, y0, z);
    let b = Vec3::new(x1, y0, z);
    let c = Vec3::new(x1, y1, z);
    let d = Vec3::new(x0, y1, z);
    let normal = Vec3::new(0.0, 0.0, if z < 0.0 { 1.0 } else { -1.0 });
    vec![
        Triangle { a, b, c, normal, color: [255, 244, 214], id: ids::WINDOW, emissive: true },
        Triangle { a, b: c, c: d, normal, color: [255, 244, 214], id: ids::WINDOW, emissive: true },
    ]
}

/// 房间地面：一张水平面。
///
/// 它不是装饰——**没有地面的 3D 视图里家具像浮在空中**，人（和模型）都难以判断
/// 东西是站着还是飘着。平面图里它也是"房间范围"的直接表达。
/// 注意它的 id 不是对象（所以不会污染「物体可见像素」与「挡光带遮挡」的统计）。
pub fn floor_quad(size: [f64; 3]) -> Vec<Triangle> {
    let (sx, sz) = (size[0] / 2.0, size[2] / 2.0);
    let a = Vec3::new(-sx, 0.0, -sz);
    let b = Vec3::new(sx, 0.0, -sz);
    let c = Vec3::new(sx, 0.0, sz);
    let d = Vec3::new(-sx, 0.0, sz);
    let normal = Vec3::new(0.0, 1.0, 0.0);
    // 地面要比背景明显暗一点：否则房间轮廓沋在背景里，家具像浮着
    let color = [203, 208, 214];
    vec![
        Triangle { a, b, c, normal, color, id: ids::FLOOR, emissive: true },
        Triangle { a, b: c, c: d, normal, color, id: ids::FLOOR, emissive: true },
    ]
}

/// 窗前挡光带：地面上的一个浅色矩形。
///
/// 它是**行业知识的可视化**——「窗前 1.5m 内不要放高家具」这条规则
/// 在这张图上变成一个看得见的区域，于是"挡窗 44%"这种判断可以被核对。
pub fn band_quad(w: &Window) -> Vec<Triangle> {
    let x0 = w.span_x[0];
    let x1 = w.span_x[1];
    let z0 = w.z.min(w.z + w.band_depth);
    let z1 = w.z.max(w.z + w.band_depth);
    let y = 0.004; // 抬一点避免与地面/地毯 z-fighting
    let a = Vec3::new(x0, y, z0);
    let b = Vec3::new(x1, y, z0);
    let c = Vec3::new(x1, y, z1);
    let d = Vec3::new(x0, y, z1);
    let normal = Vec3::new(0.0, 1.0, 0.0);
    vec![
        Triangle { a, b, c, normal, color: [120, 152, 178], id: ids::BAND, emissive: true },
        Triangle { a, b: c, c: d, normal, color: [120, 152, 178], id: ids::BAND, emissive: true },
    ]
}

/// 场景里所有实体（含 id 归属）。
pub fn scene_solids(scene: &Scene) -> Vec<Triangle> {
    let mut out = Vec::new();
    for (i, node) in scene.objects.iter().enumerate() {
        let color = base_color(node);
        out.extend(box_triangles(&node.aabb, color, i as u32 + 1));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn box_has_twelve_triangles_with_outward_normals() {
        let b = Aabb::new([0.0, 0.0, 0.0], [1.0, 2.0, 3.0]).unwrap();
        let tris = box_triangles(&b, [200, 200, 200], 1);
        assert_eq!(tris.len(), 12);
        // 每个面的法线必须指向盒外：中心 + 0.5*法线 应当落在面上或面外
        let center = Vec3::new(0.5, 1.0, 1.5);
        for t in &tris {
            let mid = (t.a + t.b + t.c) * (1.0 / 3.0);
            let probe = mid + t.normal * 0.001;
            let outward = (mid - center).dot(t.normal) >= 0.0;
            assert!(outward, "法线朝内了：{:?} normal={:?}", mid, t.normal);
            assert!(probe.x >= -0.01 && probe.x <= 1.01);
        }
    }

    #[test]
    fn band_covers_the_window_to_band_depth() {
        let w = Window {
            wall: Some("north".into()),
            span_x: [-1.2, 1.2],
            height: 1.6,
            z: -2.9,
            band_depth: 1.5,
            extras: Default::default(),
        };
        let band = band_quad(&w);
        let zs: Vec<f64> = band.iter().flat_map(|t| [t.a.z, t.b.z, t.c.z]).collect();
        assert!(zs.iter().cloned().fold(f64::INFINITY, f64::min) - 2.9 < 1e-9);
        assert!(zs.iter().cloned().fold(f64::NEG_INFINITY, f64::max) - 1.4 < 1e-9);
    }
}
