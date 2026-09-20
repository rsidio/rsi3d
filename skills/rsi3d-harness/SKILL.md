---
name: rsi3d-harness
description: '驱动 rsi3d-harness 3D 资产引擎做「观察 → 编辑 → 评测 → 回滚」的可复现循环。USE FOR: 3D 场景/资产的自然语言编辑（"把沙发挪出窗带"、"调暗日光"）、三维空间关系与光照测量（挡窗百分比、可视像素）、可回滚的批量场景改动、产出可对账的观测图与分数曲线、glTF 导入导出、RSI 迭代收敛。Keywords: 3D, scene, glTF, 建模, 场景, 资产, 灯光, 遮挡, 渲染, RSI, self-improvement, rollback, 回滚, 可复现. DO NOT USE FOR: 纯文本/代码任务；需要图像生成模型的写实渲染；点云/高斯编辑（当前只支持 crop-only 等受限操作，见 references/discipline.md）。'
argument-hint: '[场景文件或意图，例如 "scenes/living.json 把沙发挪出窗带"]'
---

# rsi3d-harness —— Agentic 3D 资产引擎

把 3D 资产变成 Agent **能看见、能动手、可度量、可回放**的东西。每一次改动都是一条带 `reason` 的命令，
命令进日志 → 可逆、可重放、可归因、可回滚到 best-so-far。

## 什么时候用它

- 用户用**自然语言**要求改一个 3D 场景（挪家具、调灯光、换材质、删掉某件东西）；
- 需要**量出来**的空间事实，而不是看图猜："沙发挡住窗户多少" ← `scene_render` 给实测百分比；
- 需要**可回滚**的多步改动（试了几版，退回到最好那一版）；
- 需要**可对账的证据**（同场景同参数 ⇒ 同像素哈希的观测图）；
- 要读/写标准 glTF，或把 BLEND/FBX/OBJ/STL 接进来。

典型触发语：「把这个场景里的…」「跑一次 RSI」「让这个资产自己改好」「导出成 glb」「这个角度挡住光了」。

## 两条驱动路径（选一条，不要混用）

| 路径 | 什么时候用 | 入口 |
| --- | --- | --- |
| **MCP 九工具** | 你在 VS Code / Cursor / Claude Code 里，能直接调工具 | `scene_open` `scene_observe` `scene_edit` `scene_rollback` `scene_history` `scene_diff` `scene_render` `scene_save` `scene_verify` |
| **CLI** | 你要跑脚本、CI，或这个环境没有 MCP | `rsi3d-harness scene …`（见 [references/cli.md](./references/cli.md)） |

MCP 不可用时先确认有没有二进制：`rsi3d-harness --version`（没有就 `cargo build --release`，
或用 npm 的 `npx @rsi3d/cli rsi3d-harness --help`）。MCP 配置见引擎仓库的 `.vscode/mcp.json`。

## 黄金路径（一次完整闭环）

```
1. scene_open     打开场景（或内联 JSON）→ 设成当前会话
2. scene_observe  看清现状：节点表 + 可编辑性 + 告警 + 挡窗者 + 哈希   ← 别跳这步
3. scene_edit     改一件事，必须写 reason；拿回新版本号与**逆命令**
4. scene_observe  看告警增减：修好的没了，新出现的就是这一步的代价
5. scene_render   要证据就出图：四视角 PNG + 挡窗实测 %
6. scene_rollback 变糟了就回到 best-so-far（不要硬撑）
7. scene_verify   收尾自检：重放一致、全版本可重放、哈希稳定
8. scene_save     落盘成文档（含完整命令日志，可重放对账）
```

CLI 等价：`scene show` → `scene edit --cmd '…' --out doc.json` → `scene render` → `scene verify doc.json`。

## 命令信封

```json
{
  "op": "transform",
  "target": "sofa_01",
  "params": { "translate": [0, 0, 1.3] },
  "reason": "沙发挡住窗户 44%，沿 +Z 挪出挡光带",
  "expect": { "lighting.window": "+" }
}
```

- `op`：`transform`（`translate` / `rotate_y_deg` / `scale`）· `set_light`（`intensity` / `color`）·
  `set_material`（`material` / `roughness` / `metallic` / `opacity`）· `remove` · `checkout`（`rev`）。
- `reason` **必填**：它进归因表，也是回看"哪一步真的起作用"的唯一线索。
- `expect` 可选但强烈建议：写清你预期什么变好，事后用 `scene_history` 核对判断对不对。
- 一次只改一件事。批量发多条命令时，中途被拒的**前面的仍然生效**——结果里会明确写哪些生效、哪些没执行。

## 七条纪律（照做，否则结果不可信）

1. **先观察再动手**。`editability` 是 `crop-only` / `replace-only` 的节点**不能** `transform`，引擎会拒绝——先看清。
2. **每次编辑必须有 `reason`**，且是具体原因（"挡住 44%，挪出窗带"），不是"优化了一下"。
3. **只缩不放**：客户端/宿主给的是上限，引擎只按更严的来。别指望参数被放大。
4. **观测图是证据**：`cpu-raster/v1` 档可复现、能当验收依据；其它档不可复现，**不得**用于验收。
5. **降级必须显式**。取不到 CDN、GPU 上下文丢失、渲染降档，都会在结论里写出来——看到就如实转述，不要静默变差。
6. **告警是导航，不是错误清单**。修好的告警不该再出现；新出现的告警是这一步的代价，要解释。
7. **不确定就回滚**。`scene_rollback` 到 best-so-far 是一等工具，不是失败。

细节：[references/discipline.md](./references/discipline.md)（四条不变量、可编辑性、证据档、常见坑）。

## References

- [references/mcp-tools.md](./references/mcp-tools.md) —— 九个工具逐个：入参、返回、失败语义
- [references/cli.md](./references/cli.md) —— CLI 速查：scene / serve / stream / render-mode / contract / scaffold
- [references/discipline.md](./references/discipline.md) —— 不变量、可编辑性、证据与降级、坑清单
- [references/gltf.md](./references/gltf.md) —— 导入导出与 side-car（`.mesh.glb`）约定
