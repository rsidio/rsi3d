# {{project}}

基于 **rsi3d-harness** 的 3D 资产 Agent（由 `rsi3d-harness scaffold new agent-app` 生成于 {{date}}）。

这个项目回答一个问题：**Agent 怎么"看见"并"改动"一个 3D 场景，还能证明改动有用？**

## 30 秒跑通

```bash
node agent.mjs --mock
```

`--mock` 会拉起自带的 `mock-engine.mjs`（引擎契约的最小参考实现），你会看到：

- 每轮的**批判**（哪件家具挡了窗、日光多亮、间距多少）
- 每轮的**行动**（一条编辑命令）
- 结尾的**分数曲线**与**归因表**（Agent 的判断 vs 实际效果）

不需要 GPU、不需要真引擎、零依赖（只用 Node 内置模块）。

对着真引擎跑：

```bash
rsi3d-harness serve          # 另开终端，启动引擎
node agent.mjs
```

## 四个原语

引擎对 Agent 只暴露四个动词，`agent.mjs` 里的循环就是它们串起来的：

| 原语 | 在这里做什么 | 对应 RSI 阶段 |
| --- | --- | --- |
| `POST /observe` | 拿快照 + 结构化摘要 + 与上一版的 `diff` | `evaluate` 前半段 |
| `POST /evaluate` | 拿各维度分与**批判文本**（`notes`） | `evaluate` |
| `POST /edit` | 提交一条命令，拿回新 revision 与**逆命令** | `mutate` |
| `POST /export` | 导出产物与 op log（可复现） | `accept` |

> `evaluate` 返回的 `detail` 里带着**事实**（谁挡了窗、间距多少、理想区间），不是"建议"。
> 怎么改是 Agent 的决定 —— 这就是 `decide()` 存在的意义。

## 把规则换成 LLM

`decide(obs, scores)` 现在是**规则策略**（三类问题各一条修法）。换成 LLM 只需：

1. 把 `obs` 与 `scores` 序列化成 JSON 塞进 prompt；
2. 要求模型返回同一形状：

```json
{
  "op": "transform",
  "target": "obj:sofa_01",
  "params": { "translate": [0, 0, 1.5] },
  "reason": "沙发挡住落地窗，沿 +Z 挪出窗带",
  "expect": { "lighting.window": "+" }
}
```

3. `reason` 与 `expect` 一定要让模型填 —— 它们是**归因表**的原料（哪条命令真的有用）。

引擎契约不变，所以你可以在 mock 上先把 prompt 调好，再切真引擎。

## 场景与规则

`scene.json` 是一个极简客厅：沙发、茶几、衣柜、地毯 + 太阳光与环境光。里面两条业务规则值得注意：

- `clearance_rules`：茶几应在沙发正前方 0.4–0.8m —— **太远和太近都算违规**（行业知识就是这样被显式化的）；
- `intent_keywords`：意图里的「绿植」在场景里不存在 —— 所以分数**收敛也到不了 1.0**。

最后这点是本项目的态度：**收敛 ≠ 质量达标**。分数曲线只说明"改得动、能变好"，缺口仍然要写给人看。

## 下一步

- 换成真引擎后，`observe` 会返回多视角快照（现在 `views` 为空并带 `degraded` 说明）；
- 把 `scene.json` 换成你的业务场景；
- 用 `rsi3d-harness scaffold list` 看看还有哪些模板（供给侧插件、业务包）；
- 想发布你的 Agent？把它做成 `plugin` 制品：`rsi3d publish --file ./rsi3d-plugin.json --kind plugin`。
