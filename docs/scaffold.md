# 插件式脚手架（`rsi3d-harness scaffold`）

**版本**：v0.1 · **状态**：已实现（`crates/scaffold` + `crates/native`）· **日期**：2026-09-19
**相关**：本仓 [`README.md`](../README.md)（工程入口）· [`core.md`](./core.md)（内核契约）· [`mcp.md`](./mcp.md)（MCP 接法）

---

## 0. 一句话

> **让"基于 rsi3d-harness 造自己的 3D 资产系统与 Agent"这件事，从"读三天文档"变成"一条命令 + 一个能跑的东西"。**

```bash
rsi3d-harness scaffold new agent-app --var project=my-agent
cd my-agent && node agent.mjs --mock
```

上面两行结束后，你已经拥有：一个能跑的 Agent 循环、一个引擎契约的参考实现、
一条真实的分数曲线和一张归因表 —— **不需要 GPU、不需要真引擎、零依赖**。

---

## 1. 为什么需要它

引擎再好，用户的第一个小时决定他去留。空白起步要面对四道坎：

| 坎 | 脚手架怎么解决 |
| --- | --- |
| 引擎契约（4 个原语）长什么样？ | `mock-engine.mjs` 是**可运行**的参考实现，不是文档 |
| 我的 Agent 循环怎么写才对？ | `agent.mjs` 给了完整骨架：observe → decide → edit → evaluate + 归因表 |
| 行业知识怎么变成可判分的？ | `harness-plugin` 模板给一个"评测维度"的骨架与四条纪律 |
| 做完了怎么交付、怎么卖？ | `pack` 模板直接对齐平台的 `rsi3d pack build/sign/publish` |

---

## 2. 「插件式」的三层含义

这个词是刻意的，三层都成立才算插件式：

### ① 模板本身是插件

| 来源 | 位置 | 优先级 |
| --- | --- | --- |
| 内置 | 编译进二进制（`include_str!`） | 低 |
| 用户外部 | `~/.rsi3d-harness/scaffolds/<id>/`（`RSI3D_HARNESS_SCAFFOLDS` 可覆盖） | 中 |
| 显式指定 | `--scaffold-dir <DIR>` | **高** |

**同名覆盖**：外部模板 id 与内置相同时，内置版本被隐藏（`scaffold list` 标 `external`）。
所以想改内置模板的行为**不必 fork 引擎**：

```bash
rsi3d-harness scaffold export pack ./my-templates/pack
# 改 ./my-templates/pack 里的文件，放进外部模板目录即生效
```

### ② 产物本身是插件

生成的三个东西分别接进三条既有链路，都不需要新机制：

| 模板 | 产物性质 | 接进哪里 |
| --- | --- | --- |
| `agent-app` | 消费侧 Agent（4 原语的调用方） | 引擎（HTTP/MCP）+ 平台 Run 上报 |
| `harness-plugin` | 供给侧插件（评测维度 / 工具 / 导入器） | 引擎 `~/.rsi3d-harness/plugins/<id>/`（H2 起装载） |
| `pack` | 交付物（行业知识 + 场景 + 基准） | 平台 `rsi3d pack build/sign/publish` |

### ③ 模板可以自举

`scaffold` 模板**生成模板**。冒烟里验证了两层：

```
内置 scaffold 模板
   └─ 生成 → hello-tpl（一个外部模板）
        └─ 生成 → pipe（又一个模板）
             └─ 生成 → 最终产物（内层占位符被正确替换）
```

这是"生态会长出来"的机制保证：第三方模板不需要等我们发版。

---

## 3. 一个模板长什么样

```text
<id>/
├─ scaffold.json      # 清单：id / 分类 / 变量 / 生成后提示
└─ files/             # 唯一会被生成的目录
   ├─ README.md
   └─ src/xxx.mjs
```

`scaffold.json`：

```json
{
  "id": "my-template",
  "title": "一句话说明",
  "desc": "更长的说明（会出现在 scaffold list）",
  "kind": "agent",
  "vars": [
    { "key": "project", "prompt": "项目名", "default": "" },
    { "key": "slug", "prompt": "制品 slug", "default": "", "from": "project" }
  ],
  "next": ["cd {{project}}", "node agent.mjs --mock"]
}
```

| 字段 | 规则 |
| --- | --- |
| `id` | 小写 + 连字符；与外部目录名可以不同（以清单为准） |
| `kind` | `agent` / `plugin` / `pack` / `scaffold` / `other`，仅用于分类展示 |
| `vars[].default` | 为空 = **必填**，使用者必须 `--var` 提供，否则报错 |
| `vars[].from` | 从**声明在前面**的变量派生（如 slug 跟随 project）；显式提供时以显式为准 |
| `next` | 生成后打印的提示。**只打印，绝不执行**（见 §5 安全红线） |

**只有 `files/` 会被生成** —— 这条约定的作用是：模板自己的清单、说明、示例脚本不会混进产物。

---

## 4. 变量与两层渲染

### 4.1 变量来源

| 来源 | 例子 | 优先级 |
| --- | --- | --- |
| 内置变量 | `{{date}}` `{{template}}` `{{engine_version}}` | 最低（可被同名声明覆盖，不建议） |
| 模板声明默认值 | `"default": "0.1.0"` | 中 |
| 派生 | `"from": "project"` | 中（优先于 default） |
| 命令行 | `--var project=my-app` | **最高** |

未声明的 `--var` 会被忽略并提示（允许多个模板共用一套命令行参数）。
内容里出现**未声明**的 `{{变量}}` → **直接报错**，不静默留白（静默留白是最难查的一类 bug）。

### 4.2 转义与两层渲染

| 写法 | 结果 |
| --- | --- |
| `{{project}}` | **现在**替换成值 |
| `\{{project}}` | 输出 `{{project}}`（留给下一层） |

这就是 `scaffold` 模板能生成模板的关键：**写 `{{x}}` 表示"现在替换"，写 `\{{x}}` 表示"留到下一层"。**

> ⚠️ 踩过的坑：在**作为产物**的 JSON 模板里，如果写成 `\\{{project}}`（两个反斜杠），渲染后会变成 `\{{project}}`，
> 而 `\{` 在 JSON 里是**非法转义** → 产物不是合法 JSON。
> 单测里有一条专门抓它：**渲染后的 `.json` 必须能被解析**。

---

## 5. 生成流程与安全红线

```
discover（内置 → 用户目录 → --scaffold-dir）
   → find(id)
   → resolve_vars（声明 + 派生 + 内置 + 命令行；缺必填则报错）
   → plan（路径渲染 + 越界检查 + 冲突检测）      ← --dry-run 到此为止
   → render（写文件 + 恢复可执行位）
```

| 红线 | 实现 |
| --- | --- |
| **绝不执行模板里的脚本** | `next` 只打印提示；生成物里没有 postinstall 之类的东西 |
| **路径不许越界** | 渲染后的路径含 `..` 或以 `/` 开头 → 报错拒绝 |
| **不覆盖已有文件** | 任一目标已存在即整体拒绝，并列出冲突清单；`--force` 才覆盖 |
| **先规划再落盘** | `plan()` 通过后 `render()` 才写；`--dry-run` 连目录都不建 |
| **模板是文本** | 只支持 UTF-8 文本文件（二进制资产请走制品上传，不走脚手架） |

---

## 6. CLI 参考

```bash
rsi3d-harness scaffold list                      # 内置 + 外部模板
rsi3d-harness scaffold info agent-app            # 变量、产出文件、下一步
rsi3d-harness scaffold new agent-app --var project=my-agent
rsi3d-harness scaffold new pack --out ./p --var project=home-display --dry-run
rsi3d-harness scaffold export harness-plugin ./my-templates/harness-plugin
rsi3d-harness scaffold dir                       # 外部模板目录在哪
```

全局：`--scaffold-dir <DIR>`（最高优先级模板来源）、`--json`（给脚本/Agent 消费）。

全部命令都支持 `--json`，输出是稳定的结构化结果（含 `files` / `vars` / `next`），
所以**脚手架自己也是可被 Agent 调用的**。

---

## 7. 四个内置模板

| 模板 | 解决什么 | 一条命令验证 |
| --- | --- | --- |
| `agent-app` | 我的 Agent 怎么驱动引擎 | `node agent.mjs --mock` → 分数曲线 + 归因表 |
| `harness-plugin` | 我的行业判据怎么变成评测维度 | `node selftest.mjs` → 6 项纪律检查 |
| `pack` | 我怎么交付行业知识并卖出去 | `rsi3d pack build/sign/verify` |
| `scaffold` | 我怎么做一个自己的模板 | `scaffold list` 里出现它 |

### 7.1 `agent-app` 里最值得看的两个设计

1. **`decide()` 是唯一需要你改的地方** —— 它是规则策略，换成 LLM 只改这一个函数，引擎契约不变；
   prompt 里要模型返回的字段（`op/target/params/reason/expect`）与规则策略**完全同形**。
2. **收敛 ≠ 质量达标** —— 场景里刻意留了「意图里的绿植不存在」，所以分数最高只到 0.97。
   循环会在"提不出新命令"时停下并如实报告剩余缺口，而不是假装成功。

### 7.2 `harness-plugin` 里的四条纪律

| 纪律 | 违反的后果 |
| --- | --- |
| `evaluate` 是纯函数（不改场景） | 回滚与归因都会失真 |
| 确定性（同输入同分数） | 分数曲线从"证据"退化成"噪声" |
| 分数 ∈ [0,1] 且有限 | 上层按权重合成总分时会崩 |
| 工具**只产出命令**，不直接改场景 | 绕过 op log，无法回滚到 best-so-far |

`selftest.mjs` 把这四条都写成断言，并且**先写反例**（"把两件家具贴到一起，应当扣分"）。

---

## 8. 与平台的关系

| 你要做的事 | 用哪个工具 |
| --- | --- |
| 造 3D 资产系统 / Agent 的骨架 | `rsi3d-harness scaffold`（本功能） |
| 打包、签名、发布、验签 | `rsi3d`（平台 CLI，`pack` 工具链） |
| 登记 Run、看分数曲线 | `rsi3d run` / 平台的 Run 账本 |

两者边界清楚：**脚手架只管"生成文件"，不联网、不上传、不执行**。
`pack` 模板生成的 `rsi3d.pack.json` 直接就是平台 CLI 的输入 —— 冒烟里已实测打通（build → sign → verify 全绿）。

---

## 9. 里程碑

| 阶段 | 内容 | 状态 |
| --- | --- | --- |
| S0 | 内置 4 模板 + 变量系统 + 外部模板发现 + 自举 + 冒烟 | ✅ 已完成（当时 52 项冒烟 / 10 项单测；现已并入 96 项统一冒烟） |
| S1 | 引擎 `serve` 落地后：`agent-app` 生成的 Agent 直连真引擎出图 | 待 H0 引擎 |
| S2 | 引擎装载 `harness-plugin` 产物（`~/.rsi3d-harness/plugins/`） | 待 H2 |
| S3 | **从 Registry 拉模板**：`rsi3d-harness scaffold add @ns/slug`（模板作为制品分发 + 验签） | 规划 |
| S4 | 模板版本与兼容声明（`engine: ">=0.2"`），装载时校验 | 规划 |

> S3 是"插件式"真正闭环的一步：模板变成可上架的 `plugin` 制品，
> 那时的分发机制与 `rsi3d install @ns/slug` 完全一致（含 sha256 校验）。

---

## 10. 推迟与已知限制

- 不支持二进制文件与符号链接（模板只处理 UTF-8 文本）；
- 没有交互式问答（`--var` 或默认值，不做 prompt 交互）——便于脚本与 Agent 调用；
- 变量只做纯文本替换，**没有条件/循环**（要分支就写两份模板，或让模板自带生成脚本 —— 但那脚本由使用者自己跑）；
- 不做模板依赖（模板 A 引用模板 B）——等 S3 有真实需求再说。
