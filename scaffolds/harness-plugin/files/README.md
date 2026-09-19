# {{project}}

rsi3d-harness **引擎插件**（id: `{{plugin_id}}`，由 `scaffold new harness-plugin` 生成于 {{date}}）。

插件是什么：**给引擎加能力的包**。引擎自己不做的重运算（生成、重拓扑、烘焙）也好，
行业特有的判据（通道宽度、公差、做旧规范）也好，都以插件形式挂上去，而不是改引擎内核。

## 跑自测

```bash
node selftest.mjs
```

它会用 `fixtures/scene.json` 检查你的实现是否守住四条纪律：

| 纪律 | 为什么 |
| --- | --- |
| `evaluate` 是**纯函数**（不改 scene） | 改状态的地方只能有一处（引擎的 `edit`），否则回滚与归因都会失真 |
| **确定性**（同输入同分数） | 分数曲线是"改进的证据"，非确定性会让它变成噪声 |
| 分数落在 0..1 且有限 | 上层要按权重合成总分 |
| 工具**只产出命令**，不直接改场景 | 每条改动都要进 op log，才能回滚到 best-so-far |

## 贡献点（contributes）

`rsi3d-plugin.json` 声明你给引擎加了什么，引擎装载时按此核对：

| 贡献点 | 用途 | 本样板 |
| --- | --- | --- |
| `evaluators` | 新的评测维度 | `{{dimension}}`（通道宽度） |
| `tools` | 暴露给 Agent 的新工具 | `place_row`（沿线等距摆放） |
| `commands` | 新的编辑命令（op） | 留空，示例见 `src/tool.mjs` 产出的命令形状 |
| `importers` | 新资产格式的导入器 | 留空 |

## 权限

```json
"permissions": { "network": false, "fs": "read-only" }
```

默认**不联网、只读**。要联网（例如调你自己的评测服务）必须在清单里显式声明，
装载时会展示给用户确认 —— 插件不能悄悄联网。

## 安装

```bash
mkdir -p ~/.rsi3d-harness/plugins
cp -R . ~/.rsi3d-harness/plugins/{{plugin_id}}
```

引擎启动时扫描该目录（H2 起），装载失败会在日志里给出原因（清单非法 / 自测不过 / 权限超声明）。

## 上架

```bash
rsi3d publish --file rsi3d-plugin.json --kind plugin --slug {{plugin_id}} --version 0.1.0
```

发布后会带 sha256 与签名，别人 `rsi3d download @you/{{plugin_id}}` 即可取得。

## 写维度之前先问三个问题

1. **这个判据是行业知识还是主观审美？** 行业知识（通道 ≥0.6m、法兰平面度 ≤0.05mm）→ 适合做维度；主观审美 → 别硬编码成规则，交给人的 `accept`。
2. **能算出来吗？** 算不出来的东西不要做成维度，会污染分数曲线。
3. **反例是什么？** 先写一个必然扣分的 fixture（`selftest.mjs` 里那个"贴太近"的例子），再写实现。
