//! **不变量测试**：`core` 的全部价值都压在这里。
//!
//! 这些测试刻意只用公开 API——因为不变量必须对**调用方可见的行为**成立，
//! 而不是对内部实现成立。
//!
//! 场景用的是 H0 的真实剧本（与 `scaffolds/agent-app/files/scene.json`
//! 里的同一个客厅），所以这些测试同时是「H0 场景能被内核正确处理」的证据。

use rsi3d_harness_core::{
    align_clearance, normalize_node_id, Aabb, Command, CommandRequest, CoreError, Document,
    Editability, LayerKind, Light, Node, RoleKind, Room, Scene, Window,
};
use serde_json::json;

// ---------------------------------------------------------------- 测试夹具

/// H0 的客厅：沙发初始挡在窗前，茶几离沙发 3.9m（规则要求 0.4–0.8m）。
fn h0_scene() -> Scene {
    let mut scene = Scene {
        units: "m".into(),
        intent: Some("3 米挑高客厅，北欧风，落地窗，暖光".into()),
        room: Some(Room {
            size: [4.2, 2.8, 6.0],
            ceiling: Some(3.0),
            wall_color: None,
            extras: Default::default(),
        }),
        window: Some(Window {
            wall: Some("north".into()),
            span_x: [-1.2, 1.2],
            height: 1.6,
            z: -2.9,
            band_depth: 1.5,
            extras: Default::default(),
        }),
        ..Default::default()
    };

    let mut sofa = Node::new(
        "obj:sofa_01",
        "furniture",
        Aabb::new([-1.1, 0.0, -2.6], [0.9, 0.85, -1.7]).unwrap(),
    );
    sofa.material = Some("fabric".into());

    let mut table = Node::new(
        "obj:table_01",
        "furniture",
        Aabb::new([-0.5, 0.0, 2.2], [0.5, 0.45, 2.9]).unwrap(),
    );
    table.material = Some("oak".into());

    let mut wardrobe = Node::new(
        "obj:wardrobe_01",
        "furniture",
        Aabb::new([1.3, 0.0, -2.4], [1.95, 2.2, -1.0]).unwrap(),
    );
    wardrobe.material = Some("oak".into());

    let rug = Node::new(
        "obj:rug_01",
        "floor",
        Aabb::new([-1.6, 0.0, 0.2], [1.4, 0.02, 3.2]).unwrap(),
    );

    scene.objects = vec![sofa, table, wardrobe, rug];
    scene.lights = vec![
        Light {
            id: "sun".into(),
            kind: "directional".into(),
            intensity: 2.4,
            color: Some("#ffe9c9".into()),
            direction: None,
            extras: Default::default(),
        },
        Light {
            id: "env".into(),
            kind: "hdri".into(),
            intensity: 0.6,
            color: None,
            direction: None,
            extras: Default::default(),
        },
    ];
    scene.clearance_rules = vec![rsi3d_harness_core::ClearanceRule {
        pair: ["obj:sofa_01".into(), "obj:table_01".into()],
        min: 0.4,
        max: 0.8,
        reason: "茶几应在沙发正前方 0.4–0.8m".into(),
        extras: Default::default(),
    }];
    scene.intent_keywords = vec![
        rsi3d_harness_core::IntentKeyword {
            word: "沙发".into(),
            present: true,
            note: None,
        },
        rsi3d_harness_core::IntentKeyword {
            word: "落地窗".into(),
            present: true,
            note: None,
        },
        rsi3d_harness_core::IntentKeyword {
            word: "绿植".into(),
            present: false,
            note: Some("缺一盆绿植".into()),
        },
    ];
    scene
}

fn h0_doc() -> Document {
    Document::new(h0_scene()).expect("H0 场景应当合法")
}

fn move_sofa_out_of_window() -> Command {
    Command::Transform {
        target: "sofa_01".into(),
        translate: Some([0.0, 0.0, 1.3]),
        rotate_y_deg: None,
        scale: None,
    }
}

fn warning_codes(doc: &Document) -> Vec<String> {
    doc.warnings().into_iter().map(|w| w.code).collect()
}

// ---------------------------------------------------------------- 不变量 1：可逆

#[test]
fn apply_then_inverse_restores_exact_state() {
    let mut doc = h0_doc();
    let before = doc.scene().clone();

    let commands = vec![
        move_sofa_out_of_window(),
        Command::Transform {
            target: "obj:table_01".into(),
            translate: Some([0.0, 0.0, -1.8]),
            rotate_y_deg: None,
            scale: None,
        },
        Command::SetLight {
            target: "sun".into(),
            intensity: Some(1.0),
            color: Some("#fff4e0".into()),
        },
        Command::SetMaterial {
            target: "obj:sofa_01".into(),
            material: Some("linen".into()),
            roughness: Some(0.8),
            metallic: None,
            opacity: None,
        },
    ];

    for cmd in commands {
        let applied = doc.apply(cmd.clone()).expect("命令应当成功");
        assert_ne!(doc.scene(), &before, "命令应当改变了状态：{:?}", cmd);

        // 逆命令由内核从**前置状态**算出，必须精确还原
        doc.apply(applied.inverse.clone())
            .unwrap_or_else(|e| panic!("逆命令执行失败：{}（原命令 {:?}）", e, cmd));
        assert_eq!(
            doc.scene(),
            &before,
            "逆命令没有精确还原状态。原命令 {:?}，逆命令 {:?}",
            cmd,
            applied.inverse
        );
    }
}

#[test]
fn rotation_inverse_is_exact_even_though_aabb_rotation_is_lossy() {
    let mut doc = h0_doc();
    let before = doc.scene().clone();

    // 旋转在 AABB 上是**有损**的（转回来会得到更大的盒子）……
    let applied = doc
        .apply(Command::Transform {
            target: "obj:table_01".into(),
            translate: None,
            rotate_y_deg: Some(30.0),
            scale: None,
        })
        .unwrap();
    let rotated = doc.scene().node("obj:table_01").unwrap().aabb;
    assert!(rotated.size()[0] > before.objects[1].aabb.size()[0] - 1e-9);

    // ……但逆命令是「恢复原值」而不是「反向旋转」，所以仍然精确
    doc.apply(applied.inverse).unwrap();
    assert_eq!(doc.scene(), &before);
}

#[test]
fn undo_chain_walks_back_to_the_start_and_then_refuses() {
    let mut doc = h0_doc();
    let before = doc.scene().clone();

    let steps = 4;
    for _ in 0..steps / 2 {
        doc.apply(move_sofa_out_of_window()).unwrap();
        doc.apply(Command::Transform {
            target: "obj:sofa_01".into(),
            translate: Some([0.0, 0.0, -1.3]),
            rotate_y_deg: None,
            scale: None,
        })
        .unwrap();
    }
    assert_eq!(doc.revision(), steps);
    assert_eq!(doc.cursor(), steps);

    // 撤销必须**单调往回走**（不是「反转最后一条日志」——那样会振荡）
    let mut seen = Vec::new();
    for expect in (0..steps).rev() {
        let applied = doc.undo().expect("还有可撤销的步骤");
        assert_eq!(doc.cursor(), expect, "撤销游标应当走到 rev {}", expect);
        assert_eq!(&applied.command.op(), &"checkout");
        // 游标不变量：state_at(cursor) 就是当前场景
        assert_eq!(&doc.state_at(doc.cursor()).unwrap(), doc.scene());
        assert_eq!(doc.replay().unwrap(), *doc.scene());
        seen.push(doc.scene().clone());
    }
    assert_eq!(seen.last().unwrap(), &before, "撤销到底应当回到最初状态");

    // 日志保持 append-only：撤销本身也是命令
    assert_eq!(doc.revision(), steps * 2);
    assert_eq!(doc.oplog().len(), (steps * 2) as usize);
    assert_eq!(doc.oplog().last().unwrap().reason, "undo rev 1 → rev 0");

    // 到顶了：再撤销必须报错，而不是悄悄开始重做
    assert!(!doc.can_undo());
    let err = doc.undo().unwrap_err();
    assert!(matches!(err, CoreError::NothingToUndo));
    assert_eq!(err.code(), "nothing_to_undo");

    // 重做：逐版推回去
    assert!(doc.can_redo());
    for expect in 1..=steps {
        doc.redo().unwrap();
        assert_eq!(doc.cursor(), expect);
        assert_eq!(&doc.state_at(expect).unwrap(), doc.scene());
        assert_eq!(doc.replay().unwrap(), *doc.scene());
    }
    assert!(!doc.can_redo());
    let err = doc.redo().unwrap_err();
    assert!(matches!(err, CoreError::NothingToRedo));
}

#[test]
fn undo_is_a_first_class_reversible_step() {
    let mut doc = h0_doc();
    doc.apply(move_sofa_out_of_window()).unwrap();
    let moved = doc.scene().clone();

    // 撤销本身也有逆命令（= 重做），所以「撤销一步再撤销那一步」回到原处
    let undo = doc.undo().unwrap();
    doc.apply(undo.inverse).unwrap();
    assert_eq!(doc.scene(), &moved);
    assert_eq!(doc.replay().unwrap(), moved);
}

// ---------------------------------------------------------------- 不变量 2：可重放

#[test]
fn replay_equals_live_state() {
    let mut doc = h0_doc();
    doc.apply(move_sofa_out_of_window()).unwrap();
    doc.apply(Command::SetLight {
        target: "sun".into(),
        intensity: Some(1.0),
        color: None,
    })
    .unwrap();
    doc.apply(Command::SetMaterial {
        target: "obj:wardrobe_01".into(),
        material: Some("walnut".into()),
        roughness: None,
        metallic: None,
        opacity: Some(0.9),
    })
    .unwrap();
    doc.undo().unwrap();
    doc.apply(Command::Transform {
        target: "obj:table_01".into(),
        translate: Some([0.0, 0.0, -1.0]),
        rotate_y_deg: Some(90.0),
        scale: None,
    })
    .unwrap();

    let replayed = doc.replay().expect("重放应当成功");
    assert_eq!(
        &replayed,
        doc.scene(),
        "重放结果与实时状态不一致——说明有状态活在场景之外"
    );
    assert_eq!(replayed.scene_hash().unwrap(), doc.scene_hash().unwrap());
}

#[test]
fn checkout_jumps_and_stays_replayable() {
    let mut doc = h0_doc();
    let initial = doc.scene().clone();

    doc.apply(move_sofa_out_of_window()).unwrap(); // rev 1
    let rev1 = doc.scene().clone();
    doc.apply(Command::SetLight {
        target: "sun".into(),
        intensity: Some(0.9),
        color: None,
    })
    .unwrap(); // rev 2
    doc.apply(Command::Transform {
        target: "obj:table_01".into(),
        translate: Some([0.0, 0.0, -1.8]),
        rotate_y_deg: None,
        scale: None,
    })
    .unwrap(); // rev 3

    // 回到 rev 1
    let applied = doc.checkout(1).unwrap();
    assert_eq!(applied.revision, 4);
    assert_eq!(doc.scene(), &rev1, "checkout 应当精确回到 rev 1 的状态");
    assert_eq!(doc.state_at(1).unwrap(), rev1);
    assert_eq!(doc.replay().unwrap(), *doc.scene());

    // checkout 本身也可逆
    doc.apply(applied.inverse).unwrap();
    assert_ne!(doc.scene(), &rev1);
    assert_eq!(doc.replay().unwrap(), *doc.scene());

    // 回到 0
    doc.checkout(0).unwrap();
    assert_eq!(doc.scene(), &initial);
    assert_eq!(doc.replay().unwrap(), initial);
}

#[test]
fn snapshots_agree_with_replay() {
    let mut doc = h0_doc();
    doc.apply(move_sofa_out_of_window()).unwrap();
    doc.apply(Command::Remove {
        target: "obj:rug_01".into(),
    })
    .unwrap(); // 破坏性 → 会打快照
    doc.apply(Command::Transform {
        target: "obj:table_01".into(),
        translate: Some([0.0, 0.0, -1.8]),
        rotate_y_deg: None,
        scale: None,
    })
    .unwrap();
    doc.checkout(1).unwrap();

    let revs = doc.snapshot_revisions();
    assert!(revs.len() >= 3, "应当有多个快照点：{:?}", revs);
    for rev in revs {
        if rev == doc.revision() {
            continue; // 当前版由 scene 直接给出
        }
        let snap = doc.snapshot(rev).expect("快照应存在").clone();
        let replayed = doc.state_at(rev).unwrap();
        assert_eq!(
            snap, replayed,
            "rev {} 的快照与重放结果不一致——快照优化引入了偏差",
            rev
        );
    }
}

#[test]
fn long_mixed_history_stays_consistent() {
    // 确定性伪随机（LCG）驱动的一长串命令 + 撤销 + 回滚。
    // 目的不是覆盖率，而是「长时间操作后不变量还成立吗」。
    let mut doc = h0_doc();
    let mut seed: u64 = 0x5eed_1234;
    let mut next = |n: u64| {
        seed = seed.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
        (seed >> 33) % n
    };

    let targets = ["obj:sofa_01", "obj:table_01", "obj:wardrobe_01", "obj:rug_01"];
    let mut applied_count = 0;

    for i in 0..120 {
        let pick = next(10);
        let result = match pick {
            0..=4 => doc.apply(Command::Transform {
                target: targets[next(4) as usize].into(),
                translate: Some([0.0, 0.0, (next(7) as f64 - 3.0) * 0.2]),
                rotate_y_deg: None,
                scale: None,
            }),
            5..=6 => doc.apply(Command::SetLight {
                target: "sun".into(),
                intensity: Some(0.4 + (next(10) as f64) * 0.2),
                color: None,
            }),
            7 => doc.apply(Command::SetMaterial {
                target: targets[next(4) as usize].into(),
                material: Some(format!("mat{}", next(3))),
                roughness: None,
                metallic: None,
                opacity: None,
            }),
            8 => doc.undo(),
            _ => {
                if i > 0 {
                    doc.checkout(next(i as u64) as u32)
                } else {
                    doc.checkout(0)
                }
            }
        };
        if result.is_ok() {
            applied_count += 1;
        }
        // 每一步之后：重放必须等于实时状态，且游标必须指向当前状态
        assert_eq!(
            doc.replay().unwrap(),
            *doc.scene(),
            "第 {} 步之后重放与实时状态分叉了",
            i
        );
        assert_eq!(
            doc.state_at(doc.cursor()).unwrap(),
            *doc.scene(),
            "第 {} 步之后游标与状态不符（cursor={}）",
            i,
            doc.cursor()
        );
    }
    assert!(applied_count > 50, "有效步骤太少（{}），测试没跑起来", applied_count);
}

// ---------------------------------------------------------------- 不变量 3：确定

#[test]
fn scene_hash_is_content_addressed() {
    let a = h0_doc();
    let b = h0_doc();
    assert_eq!(a.scene_hash().unwrap(), b.scene_hash().unwrap());

    // 序列化往返之后哈希不变
    let json = a.to_json().unwrap();
    let restored = Document::from_json(&json).unwrap();
    assert_eq!(restored.scene_hash().unwrap(), a.scene_hash().unwrap());
    assert_eq!(restored.revision(), a.revision());

    // 改一点点，哈希必须不同
    let mut c = h0_doc();
    c.apply(move_sofa_out_of_window()).unwrap();
    assert_ne!(c.scene_hash().unwrap(), a.scene_hash().unwrap());
}

#[test]
fn log_hash_is_reproducible_across_runs() {
    let build = || {
        let mut d = h0_doc();
        d.apply_with(
            move_sofa_out_of_window(),
            "窗户被遮挡 44%，把沙发挪出窗带".into(),
            Some(json!({"lighting.window": "+"})),
        )
        .unwrap();
        d.apply_with(
            Command::SetLight {
                target: "sun".into(),
                intensity: Some(1.0),
                color: None,
            },
            "日光 2.4 过曝".into(),
            None,
        )
        .unwrap();
        d
    };

    let a = build();
    let b = build();
    assert_eq!(
        a.log_hash().unwrap(),
        b.log_hash().unwrap(),
        "同样的命令序列必须得到同样的日志哈希（否则账本没法对账）"
    );

    // 往返之后仍然一致
    let restored = Document::from_json(&a.to_json().unwrap()).unwrap();
    assert_eq!(restored.log_hash().unwrap(), a.log_hash().unwrap());
}

#[test]
fn log_never_contains_wall_clock() {
    let mut doc = h0_doc();
    doc.apply(move_sofa_out_of_window()).unwrap();
    let json = doc.to_json().unwrap();
    assert!(
        !json.contains("at_ms"),
        "日志里出现了墙钟字段——它会破坏可复现性（见 log_hash 的说明）"
    );
}

// ---------------------------------------------------------------- 原子性与错误

#[test]
fn failed_command_leaves_state_untouched() {
    let mut doc = h0_doc();
    doc.apply(move_sofa_out_of_window()).unwrap();
    let rev = doc.revision();
    let scene = doc.scene().clone();
    let hash = doc.scene_hash().unwrap();

    let bad = vec![
        Command::Transform {
            target: "obj:ghost".into(),
            translate: Some([1.0, 0.0, 0.0]),
            rotate_y_deg: None,
            scale: None,
        },
        Command::SetLight {
            target: "moon".into(),
            intensity: Some(1.0),
            color: None,
        },
        Command::SetMaterial {
            target: "obj:sofa_01".into(),
            material: None,
            roughness: Some(2.5), // 越界
            metallic: None,
            opacity: None,
        },
        Command::Transform {
            target: "obj:sofa_01".into(),
            translate: None,
            rotate_y_deg: None,
            scale: Some(0.0), // 非法缩放
        },
        Command::SetLight {
            target: "sun".into(),
            intensity: Some(f64::NAN),
            color: None,
        },
    ];

    for cmd in bad {
        let err = doc.apply(cmd.clone()).unwrap_err();
        assert!(
            matches!(
                err,
                CoreError::UnknownTarget(_)
                    | CoreError::InvalidArgument(_)
                    | CoreError::NonFinite(_)
            ),
            "期望参数/目标类错误，收到 {:?}",
            err
        );
        assert_eq!(doc.revision(), rev, "失败的命令不该产生新版本");
        assert_eq!(doc.scene(), &scene, "失败的命令不该改变状态");
        assert_eq!(doc.scene_hash().unwrap(), hash);
    }
}

#[test]
fn degenerate_commands_are_rejected_as_noop() {
    let mut doc = h0_doc();

    // 参数全默认
    let err = doc
        .apply(Command::Transform {
            target: "obj:sofa_01".into(),
            translate: Some([0.0, 0.0, 0.0]),
            rotate_y_deg: None,
            scale: None,
        })
        .unwrap_err();
    assert!(matches!(err, CoreError::NoOp(_)), "{:?}", err);

    // 设成同一个值 → 场景没变
    let err = doc
        .apply(Command::SetLight {
            target: "sun".into(),
            intensity: Some(2.4),
            color: None,
        })
        .unwrap_err();
    assert!(matches!(err, CoreError::NoOp(_)), "{:?}", err);

    assert_eq!(doc.revision(), 0, "空操作不该产生版本");
}

#[test]
fn bad_params_give_readable_errors() {
    let mut doc = h0_doc();
    let cases = vec![
        (json!({"op": "transform", "target": "sofa_01", "params": {"translate": [0, 0]}}), "长度 3"),
        (json!({"op": "transform", "target": "sofa_01", "params": {"translate": "left"}}), "数组"),
        (json!({"op": "transform", "params": {"translate": [1, 0, 0]}}), "需要 target"),
        (json!({"op": "set_light", "target": "sun", "params": {"intensity": "bright"}}), "数字"),
        (json!({"op": "fly", "target": "sofa_01"}), "未知 op"),
        (json!({"op": "restore", "target": "sofa_01"}), "内核生成"),
    ];
    for (payload, needle) in cases {
        let req: CommandRequest = serde_json::from_value(payload).unwrap();
        let err = doc.apply_request(&req).unwrap_err();
        let msg = format!("{}", err);
        assert!(
            msg.contains(needle),
            "错误信息应当包含「{}」，实际是：{}",
            needle,
            msg
        );
        assert_eq!(doc.revision(), 0);
    }
}

// ---------------------------------------------------------------- 可编辑性闸门

#[test]
fn editability_cap_blocks_transform_but_not_material() {
    let mut scene = h0_scene();
    // 把一个节点声明成「只能整体替换」（例如在世界坐标里烘焙出来的表示）
    scene.objects[1].layers = vec![LayerKind::Mesh];
    scene.objects[1].editability_cap = Some(Editability::ReplaceOnly);
    let mut doc = Document::new(scene).unwrap();

    let err = doc
        .apply(Command::Transform {
            target: "obj:table_01".into(),
            translate: Some([0.5, 0.0, 0.0]),
            rotate_y_deg: None,
            scale: None,
        })
        .unwrap_err();
    assert!(matches!(err, CoreError::NotEditable { .. }), "{:?}", err);
    assert_eq!(err.code(), "not_editable");

    // 换材质仍然允许（不需要移动几何）
    doc.apply(Command::SetMaterial {
        target: "obj:table_01".into(),
        material: Some("walnut".into()),
        roughness: None,
        metallic: None,
        opacity: None,
    })
    .unwrap();
}

#[test]
fn gaussian_layer_is_crop_only_yet_still_movable() {
    let mut scene = h0_scene();
    scene.objects[2].layers = vec![LayerKind::Gaussian];
    let mut doc = Document::new(scene).unwrap();

    // 推导出 CropOnly
    assert_eq!(
        doc.scene().node("obj:wardrobe_01").unwrap().editability(),
        Editability::CropOnly
    );
    // CropOnly 在 H0 不阻塞任何命令（局部几何编辑是 H3 的事）
    doc.apply(Command::Transform {
        target: "obj:wardrobe_01".into(),
        translate: Some([-0.2, 0.0, 0.0]),
        rotate_y_deg: None,
        scale: None,
    })
    .unwrap();
}

// ---------------------------------------------------------------- 告警与归因

#[test]
fn warnings_are_structured_and_only_new_ones_are_reported() {
    let mut doc = h0_doc();

    // 初始就有三个已知问题
    let codes = warning_codes(&doc);
    assert!(codes.contains(&"window.blocked".to_string()), "{:?}", codes);
    assert!(codes.contains(&"rule.violated".to_string()), "{:?}", codes);
    assert!(codes.contains(&"intent.missing".to_string()), "{:?}", codes);

    // 修掉挡窗：新告警里不该再出现 window.blocked（它本来就存在）
    let applied = doc.apply(move_sofa_out_of_window()).unwrap();
    assert!(
        !applied.new_warnings.iter().any(|w| w.code == "window.blocked"),
        "已有问题不算新告警：{:?}",
        applied.new_warnings
    );
    let codes = warning_codes(&doc);
    assert!(!codes.contains(&"window.blocked".to_string()), "{:?}", codes);

    // 制造一个新问题：把茶几推到沙发里
    let applied = doc
        .apply(Command::Transform {
            target: "obj:table_01".into(),
            translate: Some([0.0, 0.0, -3.0]),
            rotate_y_deg: None,
            scale: None,
        })
        .unwrap();
    let new_codes: Vec<&str> = applied.new_warnings.iter().map(|w| w.code.as_str()).collect();
    assert!(
        new_codes.contains(&"layout.intersect"),
        "应当报出新的相交告警，实际 {:?}",
        new_codes
    );

    // 告警结构对 Agent 友好
    let w = applied
        .new_warnings
        .iter()
        .find(|w| w.code == "layout.intersect")
        .unwrap();
    assert_eq!(w.nodes.len(), 2, "相交告警要指出是哪两件：{:?}", w);
    assert!(w.message.contains("obj:"), "{:?}", w);
}

#[test]
fn align_clearance_fixes_gap_without_reblocking_window() {
    let mut doc = h0_doc();
    doc.apply(move_sofa_out_of_window()).unwrap();

    let rule_before = doc.scene().clearance_rules[0].clone();
    let gap_before = {
        let a = doc.scene().node(&rule_before.pair[0]).unwrap();
        let b = doc.scene().node(&rule_before.pair[1]).unwrap();
        a.aabb.gap(&b.aabb)
    };
    assert!(rule_before.distance(gap_before) > 0.0, "夹具应当先违规");

    let cmd = align_clearance(doc.scene(), 0).unwrap().expect("应当需要调整");
    // 应该挪的是「离房间中心更远」的茶几，而不是刚挪好的沙发
    assert_eq!(cmd.target(), "obj:table_01", "不该挪沙发（会把窗户又挡住）");
    doc.apply(cmd).unwrap();

    let a = doc.scene().node(&rule_before.pair[0]).unwrap();
    let b = doc.scene().node(&rule_before.pair[1]).unwrap();
    let gap_after = a.aabb.gap(&b.aabb);
    assert_eq!(rule_before.distance(gap_after), 0.0, "间距应当落进区间");

    // 窗户仍然是通的
    assert!(doc.scene().window_blockers().is_empty(), "别把窗口又堵上");
    // 再调一次就没有动作了
    assert!(align_clearance(doc.scene(), 0).unwrap().is_none());
}

#[test]
fn attribution_rows_capture_agent_intent() {
    let mut doc = h0_doc();
    let req: CommandRequest = serde_json::from_value(json!({
        "op": "transform",
        "target": "sofa_01",
        "params": { "translate": [0, 0, 1.3] },
        "reason": "窗户被遮挡 44%，把沙发挪出窗带",
        "expect": { "lighting.window": "+" }
    }))
    .unwrap();
    doc.apply_request(&req).unwrap();

    let rows = doc.attribution();
    assert_eq!(rows.len(), 1);
    let row = &rows[0];
    assert_eq!(row.rev, 1);
    assert_eq!(row.op, "transform");
    assert_eq!(row.target, "obj:sofa_01", "目标应当被规范化");
    assert!(row.reason.contains("遮挡"));
    assert_eq!(row.expect.as_ref().unwrap()["lighting.window"], "+");

    // 日志里也留着（可审计）
    assert_eq!(doc.oplog()[0].reason, row.reason);
}

// ---------------------------------------------------------------- 视图与 diff

#[test]
fn view_keeps_the_published_plugin_contract() {
    let doc = h0_doc();
    let view = doc.scene().view();
    let v = serde_json::to_value(&view).unwrap();

    // 这些键是已发布的评测插件（harness-plugin 模板）与 mock 引擎在读的
    assert!(v["objects"][0]["id"].is_string());
    assert!(v["objects"][0]["role"].is_string());
    assert!(v["objects"][0]["material"].is_string());
    assert!(v["objects"][0]["aabb"]["min"].is_array());
    assert!(v["objects"][0]["aabb"]["max"].is_array());
    assert!(v["room"]["size"].is_array());
    assert!(v["window"]["spanX"].is_array(), "键名必须是 spanX");
    assert!(v["window"]["bandDepth"].is_number(), "键名必须是 bandDepth");
    assert!(v["clearance_rules"][0]["pair"].is_array());
    assert!(v["clearance_rules"][0]["min"].is_number());
    assert!(v["clearance_rules"][0]["reason"].is_string());
    assert!(v["intent_keywords"][0]["word"].is_string());

    // 我们额外提供的：可编辑性与挡窗事实
    assert!(v["objects"][0]["editability"].is_string());
    assert_eq!(v["blockers"][0], "obj:sofa_01");
    assert_eq!(v["solid_count"], 3, "地毯不算 solid");
}

#[test]
fn diff_reports_what_changed() {
    let mut doc = h0_doc();
    doc.apply(move_sofa_out_of_window()).unwrap();
    doc.apply(Command::SetLight {
        target: "sun".into(),
        intensity: Some(1.0),
        color: None,
    })
    .unwrap();
    doc.apply(Command::Remove {
        target: "obj:rug_01".into(),
    })
    .unwrap();

    let d = doc.diff(0, 3).unwrap();
    assert_eq!(d.from, 0);
    assert_eq!(d.to, 3);
    assert_eq!(d.removed, vec!["obj:rug_01".to_string()]);
    assert!(d.added.is_empty());
    assert!(d
        .changed
        .iter()
        .any(|c| c.id == "obj:sofa_01" && c.what == "aabb"));
    assert!(d
        .changed
        .iter()
        .any(|c| c.id == "sun" && c.what == "intensity"));
    assert!(!d.is_empty());

    // 自己与自己比是空的
    assert!(doc.diff(3, 3).unwrap().is_empty());
}

#[test]
fn diff_after_remove_and_restore_is_symmetric() {
    let mut doc = h0_doc();
    doc.apply(Command::Remove {
        target: "obj:rug_01".into(),
    })
    .unwrap();
    let applied = doc
        .apply(Command::Restore(rsi3d_harness_core::Restore::Node {
            index: 3,
            node: Box::new(h0_scene().objects[3].clone()),
        }))
        .unwrap();
    assert_eq!(applied.revision, 2);
    assert_eq!(doc.scene(), &h0_scene(), "恢复后应当与原始场景一致");

    // 节点顺序也恢复了（稳定顺序影响 diff 与序列化哈希）
    assert_eq!(doc.scene().objects[3].id, "obj:rug_01");
}

// ---------------------------------------------------------------- 场景解析兼容性

#[test]
fn parses_the_shipped_scaffold_scene_json() {
    // 与 `scaffolds/agent-app/files/scene.json` 同形（键名一致就应当能读进来）
    let raw = r##"{
      "units": "m",
      "intent": "3 米挑高客厅，北欧风，落地窗，暖光，用于电商详情页主图",
      "room": { "size": [4.2, 2.8, 6.0] },
      "window": { "wall": "north", "spanX": [-1.2, 1.2], "height": 1.6, "z": -2.9, "bandDepth": 1.5 },
      "objects": [
        { "id": "obj:sofa_01", "role": "furniture", "material": "fabric",
          "aabb": { "min": [-1.1, 0.0, -2.6], "max": [0.9, 0.85, -1.7] } },
        { "id": "obj:table_01", "role": "furniture", "material": "oak",
          "aabb": { "min": [-0.5, 0.0, 2.2], "max": [0.5, 0.45, 2.9] } },
        { "id": "obj:rug_01", "role": "floor", "material": "fabric",
          "aabb": { "min": [-1.6, 0.0, 0.2], "max": [1.4, 0.02, 3.2] } }
      ],
      "lights": [
        { "id": "sun", "kind": "directional", "intensity": 2.4, "color": "#ffe9c9" }
      ],
      "clearance_rules": [
        { "pair": ["obj:sofa_01", "obj:table_01"], "min": 0.4, "max": 0.8,
          "reason": "茶几应在沙发正前方 0.4–0.8m" }
      ],
      "intent_keywords": [ { "word": "沙发", "present": true } ]
    }"##;

    let scene = Scene::from_json(raw).expect("已分发的场景 JSON 必须能解析");
    assert_eq!(scene.objects.len(), 3);
    assert_eq!(scene.window.as_ref().unwrap().band_depth, 1.5);
    assert_eq!(scene.window_blockers(), vec!["obj:sofa_01".to_string()]);
    assert_eq!(scene.objects[0].role_kind(), RoleKind::Furniture);
    assert_eq!(normalize_node_id("sofa_01"), scene.objects[0].id);

    // 未知键不丢（进 extras），也不报错
    let with_extra = r##"{
      "units": "m",
      "unknown_future_key": {"a": 1},
      "objects": [ { "id": "obj:x", "role": "furniture", "note": "来自未来",
        "aabb": { "min": [0,0,0], "max": [1,1,1] } } ]
    }"##;
    let s2 = Scene::from_json(with_extra).expect("未知键应当被容忍");
    assert_eq!(s2.extras.len(), 1);
    assert!(s2.objects[0].extras.contains_key("note"));
    let round = s2.canonical_json().unwrap();
    assert!(round.contains("unknown_future_key"), "extras 必须能原样落盘");
    assert!(round.contains("来自未来"));

    // 报错要能定位问题
    let bad = r##"{ "objects": [ { "id": "obj:x", "aabb": { "min": [1,0,0], "max": [0,1,1] } } ] }"##;
    let err = Scene::from_json(bad).unwrap_err();
    assert!(format!("{}", err).contains("min > max"), "{}", err);
}
