//! 配色：按**角色 + 材质**推导，稳定且可判读。
//!
//! 目标不是好看，而是**让 Agent 一眼分得清东西**：
//! 地面/家具/装饰/结构各自一族颜色，同族内用材质名做稳定微调
//! （同一材质永远同色，所以"换个材质"这件事在图上看得出来）。

use rsi3d_harness_core::{Node, RoleKind};

use crate::raster::Rgb;

/// 角色 → 基色。
fn role_base(role: RoleKind) -> Rgb {
    match role {
        RoleKind::Furniture => [196, 170, 142], // 木/布——暖灰
        RoleKind::Floor => [128, 140, 150],     // 地毯/地面——冷灰
        RoleKind::Decor => [150, 178, 150],     // 绿植/摆件——偏绿
        RoleKind::Structure => [176, 176, 184], // 墙/柱——中性
        RoleKind::Other => [170, 170, 170],
    }
}

/// 材质名 → 稳定的明暗微调（FNV-1a，不用 `DefaultHasher`：后者跨版本不保证稳定）。
fn material_tint(material: Option<&str>) -> f64 {
    let Some(m) = material else {
        return 1.0;
    };
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for b in m.as_bytes() {
        h ^= *b as u64;
        h = h.wrapping_mul(0x0000_0100_0000_01b3);
    }
    // 0.82 ~ 1.18
    0.82 + (h % 1000) as f64 / 1000.0 * 0.36
}

/// 节点 → 最终基色。
pub fn base_color(node: &Node) -> Rgb {
    let base = role_base(node.role_kind());
    let t = material_tint(node.material.as_deref());
    [
        (base[0] as f64 * t).clamp(0.0, 255.0) as u8,
        (base[1] as f64 * t).clamp(0.0, 255.0) as u8,
        (base[2] as f64 * t).clamp(0.0, 255.0) as u8,
    ]
}

/// 人类可读的颜色名（给文本摘要用，省得模型去猜 RGB）。
pub fn color_name(rgb: Rgb) -> &'static str {
    let (r, g, b) = (rgb[0] as i32, rgb[1] as i32, rgb[2] as i32);
    if g - r > 12 && g - b > 12 {
        "偏绿"
    } else if b - r > 12 {
        "偏蓝灰"
    } else if r - b > 18 {
        "偏暖（木/布）"
    } else {
        "中性灰"
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rsi3d_harness_core::Aabb;

    fn node(role: &str, material: Option<&str>) -> Node {
        let mut n = Node::new("obj:x", role, Aabb::new([0.0; 3], [1.0; 3]).unwrap());
        n.material = material.map(|m| m.to_string());
        n
    }

    #[test]
    fn same_material_same_color() {
        assert_eq!(
            base_color(&node("furniture", Some("oak"))),
            base_color(&node("furniture", Some("oak")))
        );
    }

    #[test]
    fn different_material_tints_differently() {
        assert_ne!(
            base_color(&node("furniture", Some("oak"))),
            base_color(&node("furniture", Some("walnut")))
        );
    }

    #[test]
    fn roles_are_distinguishable() {
        let f = base_color(&node("furniture", None));
        let g = base_color(&node("floor", None));
        let d = base_color(&node("decor", None));
        assert_ne!(f, g);
        assert_ne!(g, d);
    }
}
