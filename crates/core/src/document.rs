//! 文档 = 场景 + 命令日志 + 版本/快照。
//!
//! 这是引擎的**唯一真相来源**。所有改动都经过 [`Document::apply`]，
//! 于是「谁在什么时候改了什么、为什么改、改坏了能不能退回去」全都有答案。
//!
//! # 三条不变量（`tests/invariants.rs` 逐条锁死）
//!
//! | 不变量 | 含义 |
//! | --- | --- |
//! | **可逆** | `apply(cmd)` 后 `apply(inverse)` 回到**逐字段相同**的状态 |
//! | **可重放** | `replay()`（= `state_at(latest)`）与实时状态逐字段相同；快照与重放结果一致 |
//! | **确定** | `scene_hash()` 稳定；日志哈希**不含墙钟与随机数**（否则没法进账本） |
//!
//! 另外游标是**推导值**，满足 `state_at(cursor()) == scene()`——`undo`/`redo` 靠它才有意义。
//!
//! # 状态变换只有一处
//!
//! [`apply_to_scene`] 是唯一的纯状态变换函数，实时执行与重放**共用它**。
//! 这让「重放 == 实时」不是靠约定，而是**由构造保证**。

use std::collections::{BTreeMap, BTreeSet, HashMap};

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::command::{Applied, AttributionRow, Command, CommandRequest, OpEntry, Restore};
use crate::error::{CoreError, Result};
use crate::scene::{normalize_node_id, Aabb, Editability, Node, Scene};
use crate::validate::{validate, Warning};

/// 文档 schema 标识。
pub const DOCUMENT_SPEC: &str = "rsi3d-document/v1";

// ---------------------------------------------------------------- diff

/// 一处变化。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Change {
    /// 节点 id 或灯光 id
    pub id: String,
    /// `aabb` / `material` / `material_params` / `intensity` / `color` …
    pub what: String,
    pub before: Value,
    pub after: Value,
}

/// 两个版本之间的差异（给 Agent 看「这一步到底改了什么」）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Diff {
    pub from: u32,
    pub to: u32,
    pub added: Vec<String>,
    pub removed: Vec<String>,
    pub changed: Vec<Change>,
}

impl Diff {
    pub fn is_empty(&self) -> bool {
        self.added.is_empty() && self.removed.is_empty() && self.changed.is_empty()
    }
}

// ---------------------------------------------------------------- Document

/// 可回放、可回滚的场景文档。
#[derive(Debug, Clone)]
pub struct Document {
    /// rev 0 的场景（重放与恢复的起点）
    initial: Scene,
    /// 当前场景
    scene: Scene,
    /// 当前版本号（0 = 初始）
    revision: u32,
    /// 命令日志（rev 从 1 开始，连续）
    oplog: Vec<OpEntry>,
    /// 快路径快照：0、每次破坏性命令**之前**、每次 checkout 的落点。
    /// 只是优化与恢复兜底——真相在 `oplog`，`snapshot == state_at` 由测试保证。
    snapshots: BTreeMap<u32, Scene>,
}

impl Document {
    /// 从初始场景新建（会做一次一致性校验）。
    pub fn new(scene: Scene) -> Result<Self> {
        scene.check()?;
        let mut snapshots = BTreeMap::new();
        snapshots.insert(0, scene.clone());
        Ok(Document {
            initial: scene.clone(),
            scene,
            revision: 0,
            oplog: Vec::new(),
            snapshots,
        })
    }

    /// 从 JSON 新建。
    pub fn from_scene_json(raw: &str) -> Result<Self> {
        Document::new(Scene::from_json(raw)?)
    }

    pub fn scene(&self) -> &Scene {
        &self.scene
    }

    pub fn revision(&self) -> u32 {
        self.revision
    }

    /// 状态游标：**当前场景等于哪一版的状态**（满足 `state_at(cursor) == scene`）。
    ///
    /// 为什么需要它：撤销本身也进日志（append-only），所以「日志长度 − 1」并**不是**上一步。
    /// 没有游标的话，`undo()` 越过正向历史之后会在几个状态之间来回振荡，而不是继续往回走。
    ///
    /// 它是**推导值**（看日志尾巴），不单独存——真相仍然只在 `oplog` 里。
    pub fn cursor(&self) -> u32 {
        match self.oplog.last() {
            // 最后一条是 checkout：我们在它指向的那一版上
            Some(e) => match e.command {
                Command::Checkout { rev } => rev,
                _ => e.rev,
            },
            None => 0,
        }
    }

    /// 状态历史的**前沿**：线性历史里最远那一版。
    ///
    /// = `max(每条真实编辑的 rev, 每个 checkout 的目标 rev)`。
    /// 撤销/重做条目本身不推进前沿——它们指向的状态已经存在（否则 `redo` 会掉进自己的撤销条目里振荡）。
    pub fn head(&self) -> u32 {
        self.oplog
            .iter()
            .map(|e| match e.command {
                Command::Checkout { rev } => rev,
                _ => e.rev,
            })
            .max()
            .unwrap_or(0)
    }

    /// 还能撤销吗（`undo()` 失败前问一句，比捕获错误干净）。
    pub fn can_undo(&self) -> bool {
        self.cursor() > 0
    }

    /// 还能重做吗。
    pub fn can_redo(&self) -> bool {
        self.cursor() < self.head()
    }

    pub fn oplog(&self) -> &[OpEntry] {
        &self.oplog
    }

    /// 有快照的版本（升序）。
    pub fn snapshot_revisions(&self) -> Vec<u32> {
        self.snapshots.keys().cloned().collect()
    }

    /// 某版本的快照（若存过）。
    pub fn snapshot(&self, rev: u32) -> Option<&Scene> {
        self.snapshots.get(&rev)
    }

    /// 当前场景的全量告警。
    pub fn warnings(&self) -> Vec<Warning> {
        validate(&self.scene)
    }

    pub fn scene_hash(&self) -> Result<String> {
        self.scene.scene_hash()
    }

    /// 日志哈希：**剔除墙钟**后取 sha256。
    ///
    /// 为什么必须剔除：日志要进平台的 Run 账本，而账本的可比性依赖「同一条轨迹同哈希」。
    /// 带上时间戳就永远对不上。
    pub fn log_hash(&self) -> Result<String> {
        let stripped: Vec<OpEntry> = self
            .oplog
            .iter()
            .map(|e| {
                let mut c = e.clone();
                c.at_ms = None;
                c
            })
            .collect();
        let v = serde_json::to_value(&stripped)
            .map_err(|e| CoreError::Invariant(e.to_string()))?;
        let s = serde_json::to_string(&v).map_err(|e| CoreError::Invariant(e.to_string()))?;
        Ok(crate::sha256_hex(s.as_bytes()))
    }

    // ------------------------------------------------------------ 执行

    /// 执行一条**线上信封**命令（Agent / MCP / HTTP 的入口）。
    pub fn apply_request(&mut self, req: &CommandRequest) -> Result<Applied> {
        let cmd = req.parse()?;
        self.apply_with(cmd, req.reason.clone(), req.expect.clone())
    }

    /// 执行一条内部命令（无归因信息）。
    pub fn apply(&mut self, cmd: Command) -> Result<Applied> {
        self.apply_with(cmd, String::new(), None)
    }

    /// 执行一条命令，并记录归因信息。
    pub fn apply_with(
        &mut self,
        cmd: Command,
        reason: String,
        expect: Option<Value>,
    ) -> Result<Applied> {
        if cmd.is_noop_shape() {
            return Err(CoreError::NoOp(format!(
                "{} 的参数全为默认值，不会产生任何变化",
                cmd.op()
            )));
        }

        // 逆命令一律从前置状态算（见 command.rs 的说明）
        let inverse = self.compute_inverse(&cmd)?;

        // checkout 是「跳版本」，不走状态变换
        if let Command::Checkout { rev } = cmd {
            return self.checkout_with(rev, reason);
        }

        let mut next = self.scene.clone();
        apply_to_scene(&mut next, &cmd)?;
        if next == self.scene {
            return Err(CoreError::NoOp(format!(
                "{} {} 执行后场景没有变化",
                cmd.op(),
                cmd.target()
            )));
        }
        next.check()?;

        let destructive_pre = if cmd.is_destructive() {
            Some(self.scene.clone())
        } else {
            None
        };
        self.commit(cmd, inverse, next, reason, expect, destructive_pre)
    }

    /// 撤销：把状态退回游标的前一版（**新增一条 revision**，日志保持 append-only）。
    ///
    /// 语义是「回到上一版状态」，**不是**「把最后一条日志反过来」——
    /// 后者一旦越过正向历史就会振荡（因为它假设当前状态是最后一条的后置状态，
    /// 而这个假设在 `checkout` 之后不成立）。见 [`Document::cursor`]。
    pub fn undo(&mut self) -> Result<Applied> {
        let cur = self.cursor();
        if cur == 0 {
            return Err(CoreError::NothingToUndo);
        }
        let reason = format!("undo rev {} → rev {}", cur, cur - 1);
        self.checkout_with(cur - 1, reason)
    }

    /// 重做：把状态推回游标的后一版（与 `undo` 对称）。
    ///
    /// 上界是 [`Document::head`] 而不是 `revision`——日志里那些撤销条目已经不推进历史了。
    pub fn redo(&mut self) -> Result<Applied> {
        let cur = self.cursor();
        if cur >= self.head() {
            return Err(CoreError::NothingToRedo);
        }
        let reason = format!("redo rev {} → rev {}", cur, cur + 1);
        self.checkout_with(cur + 1, reason)
    }

    /// 回滚到某一版（本身也可逆：逆命令 = 回到当前版）。
    pub fn checkout(&mut self, rev: u32) -> Result<Applied> {
        self.checkout_with(rev, String::new())
    }

    fn checkout_with(&mut self, rev: u32, reason: String) -> Result<Applied> {
        if rev > self.revision {
            return Err(CoreError::RevisionNotFound(rev));
        }
        if rev == self.revision {
            return Err(CoreError::NoOp(format!("已经在 rev {}", rev)));
        }
        let target = self.state_at(rev)?;
        let inverse = Command::Checkout { rev: self.revision };
        let reason = if reason.is_empty() {
            format!("checkout rev {}", rev)
        } else {
            reason
        };
        self.snapshots.insert(rev, target.clone());
        self.commit(Command::Checkout { rev }, inverse, target, reason, None, None)
    }

    /// 提交一次状态变更：撞版本号、算新告警、打快照、写日志。
    fn commit(
        &mut self,
        command: Command,
        inverse: Command,
        next: Scene,
        reason: String,
        expect: Option<Value>,
        destructive_pre: Option<Scene>,
    ) -> Result<Applied> {
        let pre: BTreeSet<Warning> = validate(&self.scene).into_iter().collect();
        let post: BTreeSet<Warning> = validate(&next).into_iter().collect();
        // 「新告警」按**身份**（code + 涉及节点）判，不看全文：
        // 否则同一处问题只因数值变了（间距 3.9 → 2.6）就会每轮都报一次，刷屏。
        // 数值细节仍然在 `warnings()` 里拿得到。
        let pre_keys: BTreeSet<(String, Vec<String>)> = pre
            .iter()
            .map(|w| (w.code.clone(), w.nodes.clone()))
            .collect();
        let new_warnings: Vec<Warning> = post
            .into_iter()
            .filter(|w| !pre_keys.contains(&(w.code.clone(), w.nodes.clone())))
            .collect();

        let prev_rev = self.revision;
        if let Some(pre_scene) = destructive_pre {
            // 破坏性命令前先留一份：H3 的裁剪/简化无法从逆命令完整还原
            self.snapshots.insert(prev_rev, pre_scene);
        }

        self.revision = prev_rev + 1;
        self.scene = next;
        let rev = self.revision;
        self.oplog.push(OpEntry {
            rev,
            command: command.clone(),
            inverse: inverse.clone(),
            reason,
            expect,
            at_ms: None,
            new_warnings: new_warnings.clone(),
        });
        self.snapshots.insert(rev, self.scene.clone());

        Ok(Applied {
            revision: rev,
            command,
            inverse,
            new_warnings,
            scene_hash: self.scene_hash()?,
        })
    }

    /// 从前置状态算出精确逆命令。
    fn compute_inverse(&self, cmd: &Command) -> Result<Command> {
        let s = &self.scene;
        match cmd {
            Command::Transform { target, .. } => {
                let n = s
                    .node(target)
                    .ok_or_else(|| CoreError::UnknownTarget(normalize_node_id(target)))?;
                Ok(Command::Restore(Restore::Bounds {
                    target: n.id.clone(),
                    aabb: n.aabb,
                }))
            }
            Command::SetLight { target, .. } => {
                let l = s
                    .light_index(target)
                    .map(|i| &s.lights[i])
                    .ok_or_else(|| CoreError::UnknownTarget(target.clone()))?;
                Ok(Command::Restore(Restore::Light {
                    id: l.id.clone(),
                    intensity: l.intensity,
                    color: l.color.clone(),
                }))
            }
            Command::SetMaterial { target, .. } => {
                let n = s
                    .node(target)
                    .ok_or_else(|| CoreError::UnknownTarget(normalize_node_id(target)))?;
                Ok(Command::Restore(Restore::Material {
                    target: n.id.clone(),
                    material: n.material.clone(),
                    params: n.material_params,
                }))
            }
            Command::Remove { target } => {
                let idx = s
                    .node_index(target)
                    .ok_or_else(|| CoreError::UnknownTarget(normalize_node_id(target)))?;
                Ok(Command::Restore(Restore::Node {
                    index: idx,
                    node: Box::new(s.objects[idx].clone()),
                }))
            }
            // 逆的逆：恢复成「当前值」。对 Restore::Node 就是再删一次。
            Command::Restore(r) => match r {
                Restore::Bounds { target, .. } => {
                    let n = s
                        .node(target)
                        .ok_or_else(|| CoreError::UnknownTarget(target.clone()))?;
                    Ok(Command::Restore(Restore::Bounds {
                        target: n.id.clone(),
                        aabb: n.aabb,
                    }))
                }
                Restore::Light { id, .. } => {
                    let l = s
                        .light_index(id)
                        .map(|i| &s.lights[i])
                        .ok_or_else(|| CoreError::UnknownTarget(id.clone()))?;
                    Ok(Command::Restore(Restore::Light {
                        id: l.id.clone(),
                        intensity: l.intensity,
                        color: l.color.clone(),
                    }))
                }
                Restore::Material { target, .. } => {
                    let n = s
                        .node(target)
                        .ok_or_else(|| CoreError::UnknownTarget(target.clone()))?;
                    Ok(Command::Restore(Restore::Material {
                        target: n.id.clone(),
                        material: n.material.clone(),
                        params: n.material_params,
                    }))
                }
                Restore::Node { node, .. } => Ok(Command::Remove {
                    target: node.id.clone(),
                }),
            },
            Command::Checkout { .. } => Ok(Command::Checkout { rev: self.revision }),
        }
    }

    // ------------------------------------------------------------ 回放

    /// 某个版本的状态（**从日志重放**得到，不读快照）。
    pub fn state_at(&self, rev: u32) -> Result<Scene> {
        if rev > self.revision {
            return Err(CoreError::RevisionNotFound(rev));
        }
        let mut memo: HashMap<u32, Scene> = HashMap::new();
        memo.insert(0, self.initial.clone());
        self.state_at_inner(rev, &mut memo)
    }

    /// 当前状态（应该与 `state_at(revision)` 完全相同——测试锁死）。
    pub fn replay(&self) -> Result<Scene> {
        self.state_at(self.revision)
    }

    fn state_at_inner(&self, rev: u32, memo: &mut HashMap<u32, Scene>) -> Result<Scene> {
        if let Some(s) = memo.get(&rev) {
            return Ok(s.clone());
        }
        if rev == 0 {
            return Ok(self.initial.clone());
        }
        let entry = self
            .oplog
            .get(rev as usize - 1)
            .ok_or(CoreError::RevisionNotFound(rev))?;
        if entry.rev != rev {
            return Err(CoreError::Invariant(format!(
                "oplog 不连续：位置 {} 上记的是 rev {}",
                rev - 1,
                entry.rev
            )));
        }
        let scene = match &entry.command {
            // checkout 的效果 = 「回到 rev 的状态」，重放时递归求解
            Command::Checkout { rev: target } => {
                if *target > self.revision {
                    return Err(CoreError::RevisionNotFound(*target));
                }
                self.state_at_inner(*target, memo)?
            }
            other => {
                let mut s = self.state_at_inner(rev - 1, memo)?;
                apply_to_scene(&mut s, other)?;
                s
            }
        };
        memo.insert(rev, scene.clone());
        Ok(scene)
    }

    /// 两个版本之间的差异。
    pub fn diff(&self, from: u32, to: u32) -> Result<Diff> {
        let a = self.state_at(from)?;
        let b = self.state_at(to)?;

        let am: BTreeMap<&str, &Node> = a.objects.iter().map(|n| (n.id.as_str(), n)).collect();
        let bm: BTreeMap<&str, &Node> = b.objects.iter().map(|n| (n.id.as_str(), n)).collect();

        let mut added: Vec<String> = bm.keys().filter(|k| !am.contains_key(*k)).map(|s| s.to_string()).collect();
        let mut removed: Vec<String> = am.keys().filter(|k| !bm.contains_key(*k)).map(|s| s.to_string()).collect();
        added.sort();
        removed.sort();

        let mut changed: Vec<Change> = Vec::new();
        for (id, na) in &am {
            let Some(nb) = bm.get(id) else { continue };
            if na.aabb != nb.aabb {
                changed.push(Change {
                    id: id.to_string(),
                    what: "aabb".into(),
                    before: serde_json::json!(na.aabb.to_array()),
                    after: serde_json::json!(nb.aabb.to_array()),
                });
            }
            if na.material != nb.material {
                changed.push(Change {
                    id: id.to_string(),
                    what: "material".into(),
                    before: serde_json::json!(na.material),
                    after: serde_json::json!(nb.material),
                });
            }
            if na.material_params != nb.material_params {
                changed.push(Change {
                    id: id.to_string(),
                    what: "material_params".into(),
                    before: serde_json::json!(na.material_params),
                    after: serde_json::json!(nb.material_params),
                });
            }
            if na.role != nb.role {
                changed.push(Change {
                    id: id.to_string(),
                    what: "role".into(),
                    before: serde_json::json!(na.role),
                    after: serde_json::json!(nb.role),
                });
            }
        }
        for la in &a.lights {
            let Some(lb) = b.lights.iter().find(|l| l.id == la.id) else {
                continue;
            };
            if la.intensity != lb.intensity {
                changed.push(Change {
                    id: la.id.clone(),
                    what: "intensity".into(),
                    before: serde_json::json!(la.intensity),
                    after: serde_json::json!(lb.intensity),
                });
            }
            if la.color != lb.color {
                changed.push(Change {
                    id: la.id.clone(),
                    what: "color".into(),
                    before: serde_json::json!(la.color),
                    after: serde_json::json!(lb.color),
                });
            }
        }
        changed.sort_by(|x, y| (&x.id, &x.what).cmp(&(&y.id, &y.what)));

        Ok(Diff {
            from,
            to,
            added,
            removed,
            changed,
        })
    }

    /// 归因表：Agent 的判断（reason / expect）逐条列出，`Δscore` 由 `eval` 层补。
    pub fn attribution(&self) -> Vec<AttributionRow> {
        self.oplog
            .iter()
            .map(|e| AttributionRow {
                rev: e.rev,
                op: e.command.op().to_string(),
                target: e.command.target(),
                reason: e.reason.clone(),
                expect: e.expect.clone(),
            })
            .collect()
    }

    // ------------------------------------------------------------ 持久化

    /// 落盘（不含快照——快照可从日志重算，见 `snapshot == state_at` 不变量）。
    pub fn to_json(&self) -> Result<String> {
        let doc = PersistedDocument {
            spec: DOCUMENT_SPEC.to_string(),
            revision: self.revision,
            initial: self.initial.clone(),
            scene: self.scene.clone(),
            oplog: self.oplog.clone(),
        };
        serde_json::to_string_pretty(&doc).map_err(|e| CoreError::Invariant(e.to_string()))
    }

    /// 从落盘内容恢复（会校验日志连续性与版本号一致）。
    pub fn from_json(raw: &str) -> Result<Self> {
        let doc: PersistedDocument =
            serde_json::from_str(raw).map_err(|e| CoreError::BadScene(e.to_string()))?;
        if doc.spec != DOCUMENT_SPEC {
            return Err(CoreError::BadScene(format!(
                "document spec 应为 {}，收到 {}",
                DOCUMENT_SPEC, doc.spec
            )));
        }
        if doc.oplog.len() as u32 != doc.revision {
            return Err(CoreError::BadScene(format!(
                "revision（{}）与日志条数（{}）不一致",
                doc.revision,
                doc.oplog.len()
            )));
        }
        for (i, e) in doc.oplog.iter().enumerate() {
            if e.rev != i as u32 + 1 {
                return Err(CoreError::BadScene(format!(
                    "日志第 {} 条的 rev 应为 {}，实际 {}",
                    i,
                    i + 1,
                    e.rev
                )));
            }
        }
        doc.initial.check()?;
        doc.scene.check()?;
        let mut snapshots = BTreeMap::new();
        snapshots.insert(0, doc.initial.clone());
        snapshots.insert(doc.revision, doc.scene.clone());
        let restored = Document {
            initial: doc.initial,
            scene: doc.scene,
            revision: doc.revision,
            oplog: doc.oplog,
            snapshots,
        };
        // 落盘内容自检：日志是唯一真相，那么「重放出来的状态」必须等于存下来的状态。
        // 不一致说明文件被改坏了或版本不兼容——宁可拒绝加载，也不要拿一份假状态往下跑。
        let replayed = restored.state_at(restored.revision)?;
        if replayed != restored.scene {
            return Err(CoreError::BadScene(format!(
                "rev {} 的场景与日志重放结果不一致（日志哈希 {} vs 场景哈希 {}）——文件可能被改过或版本不兼容",
                restored.revision,
                restored.log_hash()?,
                restored.scene_hash()?
            )));
        }
        Ok(restored)
    }
}

/// 落盘格式。
#[derive(Debug, Clone, Serialize, Deserialize)]
struct PersistedDocument {
    spec: String,
    revision: u32,
    initial: Scene,
    scene: Scene,
    oplog: Vec<OpEntry>,
}

// ---------------------------------------------------------------- 纯状态变换

/// **唯一的**状态变换函数：实时执行与重放共用它。
///
/// 这是「重放 == 实时」由构造保证的关键——如果这里有两份实现，迟早会漂移。
/// 注意：`Checkout` 不在这里处理（它是「跳版本」，需要文档级上下文）。
pub fn apply_to_scene(scene: &mut Scene, cmd: &Command) -> Result<()> {
    match cmd {
        Command::Transform {
            target,
            translate,
            rotate_y_deg,
            scale,
        } => {
            let idx = scene
                .node_index(target)
                .ok_or_else(|| CoreError::UnknownTarget(normalize_node_id(target)))?;
            // 可编辑性检查：烘焙在世界坐标里的表示不能单独移动
            ensure_transformable(&scene.objects[idx])?;

            if let Some(t) = translate {
                for (i, v) in t.iter().enumerate() {
                    if !v.is_finite() {
                        return Err(CoreError::NonFinite(format!("translate[{}]", i)));
                    }
                }
            }
            if let Some(deg) = rotate_y_deg {
                if !deg.is_finite() {
                    return Err(CoreError::NonFinite("rotate_y_deg".into()));
                }
            }

            let node = &mut scene.objects[idx];
            if let Some(t) = translate {
                node.aabb.translate(*t);
            }
            if let Some(deg) = rotate_y_deg {
                node.aabb.rotate_y_about_center(*deg);
            }
            if let Some(s) = scale {
                node.aabb.scale_about_center(*s)?;
            }
            node.aabb.check()?;
            Ok(())
        }

        Command::SetLight {
            target,
            intensity,
            color,
        } => {
            let idx = scene
                .light_index(target)
                .ok_or_else(|| CoreError::UnknownTarget(target.clone()))?;
            let light = &mut scene.lights[idx];
            if let Some(v) = intensity {
                if !v.is_finite() {
                    return Err(CoreError::NonFinite("intensity".into()));
                }
                if *v < 0.0 {
                    return Err(CoreError::InvalidArgument(format!(
                        "intensity 不能为负，收到 {}",
                        v
                    )));
                }
                light.intensity = *v;
            }
            if let Some(c) = color {
                let c = c.trim();
                if c.is_empty() {
                    return Err(CoreError::InvalidArgument("color 不能为空字符串".into()));
                }
                light.color = Some(c.to_string());
            }
            Ok(())
        }

        Command::SetMaterial {
            target,
            material,
            roughness,
            metallic,
            opacity,
        } => {
            let idx = scene
                .node_index(target)
                .ok_or_else(|| CoreError::UnknownTarget(normalize_node_id(target)))?;
            let node = &mut scene.objects[idx];
            if let Some(m) = material {
                let m = m.trim();
                if m.is_empty() {
                    return Err(CoreError::InvalidArgument("material 不能为空字符串".into()));
                }
                node.material = Some(m.to_string());
            }
            let mut p = node.material_params;
            if let Some(v) = roughness {
                p.roughness = Some(*v);
            }
            if let Some(v) = metallic {
                p.metallic = Some(*v);
            }
            if let Some(v) = opacity {
                p.opacity = Some(*v);
            }
            p.check()?;
            node.material_params = p;
            Ok(())
        }

        Command::Remove { target } => {
            let idx = scene
                .node_index(target)
                .ok_or_else(|| CoreError::UnknownTarget(normalize_node_id(target)))?;
            scene.objects.remove(idx);
            Ok(())
        }

        Command::Restore(r) => {
            match r {
                Restore::Bounds { target, aabb } => {
                    let idx = scene
                        .node_index(target)
                        .ok_or_else(|| CoreError::UnknownTarget(target.clone()))?;
                    aabb.check()?;
                    scene.objects[idx].aabb = *aabb;
                }
                Restore::Light {
                    id,
                    intensity,
                    color,
                } => {
                    let idx = scene
                        .light_index(id)
                        .ok_or_else(|| CoreError::UnknownTarget(id.clone()))?;
                    if !intensity.is_finite() {
                        return Err(CoreError::NonFinite("restore.intensity".into()));
                    }
                    scene.lights[idx].intensity = *intensity;
                    scene.lights[idx].color = color.clone();
                }
                Restore::Material {
                    target,
                    material,
                    params,
                } => {
                    let idx = scene
                        .node_index(target)
                        .ok_or_else(|| CoreError::UnknownTarget(target.clone()))?;
                    params.check()?;
                    scene.objects[idx].material = material.clone();
                    scene.objects[idx].material_params = *params;
                }
                Restore::Node { index, node } => {
                    if scene.node_index(&node.id).is_some() {
                        return Err(CoreError::InvalidArgument(format!(
                            "{} 已存在，无法恢复（会导致 stable_id 重复）",
                            node.id
                        )));
                    }
                    node.check()?;
                    let at = (*index).min(scene.objects.len());
                    scene.objects.insert(at, (**node).clone());
                }
            }
            Ok(())
        }

        Command::Checkout { .. } => Err(CoreError::Invariant(
            "Checkout 必须由 Document 处理（它是跳版本，不是状态变换）".into(),
        )),
    }
}

/// 变换前的可编辑性闸门。
fn ensure_transformable(node: &Node) -> Result<()> {
    if node.editability() == Editability::ReplaceOnly {
        return Err(CoreError::NotEditable {
            target: node.id.clone(),
            reason: "该表示是烘焙结果（世界坐标已固定），不能单独移动；请整体替换".into(),
        });
    }
    Ok(())
}

/// 便捷构造：变换命令。
pub fn transform(target: &str, translate: [f64; 3]) -> Command {
    Command::Transform {
        target: normalize_node_id(target),
        translate: Some(translate),
        rotate_y_deg: None,
        scale: None,
    }
}

/// 便捷构造：材质命令（只改名字）。
pub fn set_material_name(target: &str, material: &str) -> Command {
    Command::SetMaterial {
        target: normalize_node_id(target),
        material: Some(material.to_string()),
        roughness: None,
        metallic: None,
        opacity: None,
    }
}

/// 便捷构造：灯光强度命令。
pub fn set_light_intensity(target: &str, intensity: f64) -> Command {
    Command::SetLight {
        target: target.to_string(),
        intensity: Some(intensity),
        color: None,
    }
}

/// 便捷构造：把两个节点的间距调到规则区间中点（Agent 最常用的动作之一）。
///
/// **挑哪一件挪**：取「离房间中心更远」的那件（通常是靠墙/靠外侧的），
/// 挪它不容易破坏中心区布局、也不容易把刚修好的挡窗问题又搞回来。
///
/// 返回 `None` 表示当前不需要调整（间距已在区间内）。
pub fn align_clearance(scene: &Scene, rule_index: usize) -> Result<Option<Command>> {
    let rule = scene
        .clearance_rules
        .get(rule_index)
        .ok_or_else(|| CoreError::InvalidArgument(format!("没有第 {} 条间距规则", rule_index)))?;
    let a = scene
        .node(&rule.pair[0])
        .ok_or_else(|| CoreError::UnknownTarget(rule.pair[0].clone()))?;
    let b = scene
        .node(&rule.pair[1])
        .ok_or_else(|| CoreError::UnknownTarget(rule.pair[1].clone()))?;

    let gap = a.aabb.gap(&b.aabb);
    if rule.distance(gap) == 0.0 {
        return Ok(None);
    }
    let Some((axis, sign_ab)) = separation_axis(&a.aabb, &b.aabb) else {
        return Err(CoreError::InvalidArgument(format!(
            "{} 与 {} 在当前轴上分不开，无法按间距规则调整",
            a.id, b.id
        )));
    };

    let room_center = scene
        .room
        .as_ref()
        .map(|r| r.bounds().center())
        .unwrap_or([0.0; 3]);
    let dist = |c: [f64; 3]| -> f64 {
        ((c[0] - room_center[0]).powi(2)
            + (c[1] - room_center[1]).powi(2)
            + (c[2] - room_center[2]).powi(2))
            .sqrt()
    };
    let mover_is_a = dist(a.aabb.center()) >= dist(b.aabb.center());
    let (mover_id, mover_sign) = if mover_is_a {
        (a.id.clone(), sign_ab)
    } else {
        (b.id.clone(), -sign_ab)
    };

    // mover 在 + 方向就往 - 方向挪，差多少补多少
    let mut t = [0.0_f64; 3];
    t[axis] = -mover_sign * (gap - rule.ideal());
    Ok(Some(transform(&mover_id, t)))
}

/// 找分离轴与该轴上 `a` 相对 `b` 的方向（+1 = a 在正方向）。
///
/// 多轴都分离时取**间隙最小**的那根——与 [`Aabb::gap`] 的口径一致。
fn separation_axis(a: &Aabb, b: &Aabb) -> Option<(usize, f64)> {
    let mut best: Option<(usize, f64, f64)> = None;
    for i in 0..3 {
        let gap = if a.max[i] < b.min[i] {
            b.min[i] - a.max[i]
        } else if b.max[i] < a.min[i] {
            a.min[i] - b.max[i]
        } else {
            0.0
        };
        if gap <= 0.0 {
            continue;
        }
        let sign = if a.center()[i] >= b.center()[i] {
            1.0
        } else {
            -1.0
        };
        let better = match best {
            Some((_, cur, _)) => gap < cur,
            None => true,
        };
        if better {
            best = Some((i, gap, sign));
        }
    }
    best.map(|(i, _, s)| (i, s))
}

