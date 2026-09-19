# MCP 服务（`crates/mcp`）

> 状态：✅ **已实现**（2026-09-19）· 测试：8 单测 + 7 集成（真实常驻会话）· 冒烟：`scripts/smoke.sh` §7（23 项）
>
> 一个命令就能让 VS Code / Cursor / Claude Code 拿到引擎的工具面：`rsi3d-harness mcp --root <工作目录>`

---

## 1. 为什么先做这个，而不是先做 harness-use 内嵌

原计划（D1）是**WASM 内嵌进 harness-use**当主战场。这条路的顺序问题在于：

- 它绑在**一个**宿主的具体机制上（编译期 feature、`localStorage['hu_plugins']`、页面上下文是否与 Agent 循环相同……），
  而这些问题的答案在 harness-use 那边（比如：宿主页面能否下发 `COOP`/`COEP`；Agent 循环是否与页面同上下文），**每一条都能把开工卡住**。
- 引擎的价值在 `core`（观察 / 编辑 / 回滚 / 可复现），**与宿主形态无关**。
  先用通用接口把它接出来，价值立刻可见，而且**反过来给 harness-use 集成提供了参照物**。

所以顺序改成：**先 MCP（通用工具）→ 再 harness-use（专用宿主）**。这不是放弃 D1，是把 D1 从"第一步"降级为"主战场"。

## 2. 为什么是 MCP

因为它是**现在通用工具真正在读的那个接口**：VS Code（Copilot Chat / Agent 模式）、Cursor、Claude Code 都能连 MCP 服务，
而且平台侧 PRD 早就把「native 壳当 MCP server」列为 WASM 壳的兜底方案（WASM 壳无 stdio、无入站连接）。

协议侧只有三条硬约束，实现起来很轻：

| 约束 | 我们的做法 |
| --- | --- |
| stdio 报文按**换行分隔**，不得含内嵌换行 | 紧凑序列化 + 单行 + 写完立刻 flush（`jsonrpc::write_message`） |
| 服务器**只能**往 stdout 写合法 MCP 报文 | 所有日志走 stderr；冒烟里专门断言 `stdout` 无杂质 |
| `initialize` 要做**版本协商** | 客户端要的版本我们支持就原样回，否则回我们最新的（`2025-06-18`） |

没有引入任何 MCP SDK：协议面就这么多（JSON-RPC + 生命周期 + tools/resources），
自己写反而更好控制「错误必须是模型能读到的文本」这件事。

## 3. 暴露了什么

### 九个工具

| 工具 | 干什么 | 注解 |
| --- | --- | --- |
| `scene_open` | 打开场景/文档（或内联场景 JSON），设为当前会话 | 幂等 |
| `scene_observe` | 观察：节点表（含可编辑性）、灯光、间距实测值、挡窗者、告警、哈希 | **只读** |
| `scene_edit` | 执行一条或多条命令，返回新版本号、**逆命令**、新增告警 | 有副作用 |
| `scene_rollback` | `rev` 跳到某一版（RSI 的 best-so-far）／`undo` 退回 N 步 | 有副作用 |
| `scene_history` | 日志 + 归因表：每步是谁改的、为什么、期望什么、新增哪些告警 | 只读 |
| `scene_diff` | 两版之间精确差异 | 只读 |
| `scene_render` | **直接返回观测图（PNG）** + 每个对象的可见像素 + 窗前挡光带被遮挡 % | 只读 |
| `scene_save` | 落盘成文档（含日志，可重放对账） | 幂等 |
| `scene_verify` | 可复现性自检：重放一致 / 全版本可重放 / 往返哈希 / 快照 / 游标 | 只读 |

### 两个 resource

`rsi3d://scene`（当前场景规范化 JSON）与 `rsi3d://log`（日志 + 归因表）——
VS Code 里可以 `Add Context > MCP Resources` 直接挂进对话。

### 一段 `instructions`

`initialize` 的返回里带一段工作方式说明（先观察、每轮只改一件事、必须写 reason、变糟就回滚、收尾自检）。
这是**唯一**能一次性教会所有客户端「怎么用这个引擎」的位置，所以它写得像 runbook。

## 4. 工具设计的四条原则

1. **每份结果的第一行永远是「哪份文件、哪一版、游标在哪」**。
   MCP 会话是有状态的（像 IDE 的当前文件），模型很容易忘记自己在改谁——所以不让它猜。
2. **编辑必须带 `reason`**，否则直接拒绝（连信封都不完整）。
   没有理由的改动无法归因，RSI 就退化成「随便改改」。`expect` 可选，但它进归因表。
3. **失败是「能读到的错误」，不是协议错误**。
   `isError: true` + `engine::CoreError::code()` + 完整文案 + **当前状态**。
   协议错误会被客户端吞进日志里，模型看不见，于是只能瞎猜下一步。
   失败结果里也带完整观察，因为模型需要知道「现在到底成什么样了」才能决定是改参数重试还是回滚。
4. **回滚是一等工具**。RSI 的关键动作是「回到 best-so-far」，不是"再试一次"。

另外：`scene_edit` 支持一次传多条命令，但**中途失败就停**，并如实报告「前 N 条已生效、剩下 M 条未执行」——
假装原子反而会让模型基于错误的世界观继续推理。

## 5. 安全边界

- **只读写 `--root` 之内的文件**（缺省 = 当前目录）。路径先做词法归一再去碰文件系统，`../../etc/passwd` 这类直接拒。
- **没有「执行任意命令」的能力**：工具面全是数据操作（观察 / 变换 / 回滚 / 落盘）。
  这一点是与 `mcp-for-blender` 那类方案的关键区别——它自认无鉴权，且暴露了「执行能力」而不只是数据。
- **覆盖保护**：`scene_save` 目标已存在且不是源文件时必须显式 `overwrite: true`。
- stdio 的进程边界本身就是一层隔离（由客户端拉起、随会话结束）；若将来加 HTTP transport，
  必须补一次性 token + 只绑 loopback——那时才是"网络可达"的形态。
- VS Code 还支持给 stdio 服务开沙箱（`sandboxEnabled` + `filesystem.allowWrite`），可以作为第二道闸。

## 6. 怎么接

### VS Code

仓库里已经放好 `.vscode/mcp.json`：

```json
{
  "servers": {
    "rsi3d-harness": {
      "type": "stdio",
      "command": "bash",
      "args": ["${workspaceFolder}/rsi3d-harness/scripts/mcp.sh"],
      "env": { "RSI3D_HARNESS_ROOT": "${workspaceFolder}" }
    }
  }
}
```

- 包装脚本 `scripts/mcp.sh` 负责两件事：**缺二进制时先 `cargo build --release`**（并往 stderr 提示），
  以及把工作目录传给 `--root`。所以第一次用不需要手动构建。
- 打开 Chat → Agent 模式，工具会出现在工具列表里（`Configure Tools` 里可单独开关）。
- 排查问题：`MCP: List Servers` → 选服务器 → `Show Output`（那是我们的 stderr）。

> Agent Host 会话读的是工作区 `.mcp.json` 或用户 `~/.copilot/mcp-config.json`（键名是 `mcpServers`），
> 与 `.vscode/mcp.json` 不是同一份文件——两个都要用就都写一份。

> 如果某个客户端不展开 `${workspaceFolder}`，把 `args` 里的路径换成绝对路径即可
> （已在本机用「模拟客户端」的方式验证过 `scripts/mcp.sh` 这条启动链路：
> 握手 → 工具清单 → 会话可用，工作目录正确落在工作区根）。

### Cursor / 其它

同一份配置换个键名即可：

```json
{ "mcpServers": { "rsi3d-harness": { "command": "bash",
  "args": ["/abs/path/rsi3d-harness/scripts/mcp.sh"] } } }
```

### Claude Code

```bash
claude mcp add rsi3d-harness -- bash /abs/path/rsi3d-harness/scripts/mcp.sh
```

### 手动调试

```bash
printf '%s\n' \
  '{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-06-18"}}' \
  '{"jsonrpc":"2.0","method":"notifications/initialized"}' \
  '{"jsonrpc":"2.0","id":2,"method":"tools/list"}' \
  | ./target/release/rsi3d-harness mcp --root .
```

## 7. 一次真实会话长什么样（实测）

```text
场景 /tmp/h0demo/scene.json  ·  rev 1  ·  游标 1  ·  历史前沿 1  ·  未保存

rev 1  transform  obj:sofa_01
      理由 沙发挡窗，挪出挡光带
      新告警 无

当前全部告警：
  [intent.missing] 意图里的「绿植」在场景里不存在  ()
  [rule.violated] obj:sofa_01 与 obj:table_01 间距 2.60m，应在 0.40–0.80m（茶几应在沙发正前方 0.4–0.8m…）

挡窗者：无（窗前通光）

可回滚到 rev 0–0（当前 rev 1）。
```

注意这段文本是**给模型读的**：它一眼能看出「上一步修好了什么（新告警无、挡窗者无）」、
「还剩什么问题（间距还没调）」、「能退到哪（rev 0）」。结构化数据在同一条结果的 `structuredContent` 里，
文本与结构化**同源**（都由 `core::report` 生成），所以两者不可能说不一样的话。

## 8. 与 harness-use 路线的关系

| | 现在（MCP） | 之后（WASM 内嵌 harness-use） |
| --- | --- | --- |
| 宿主 | 任何支持 MCP 的工具 | harness-use 页面 |
| 壳 | `crates/mcp`（stdio） | `crates/wasm`（wasm-bindgen） |
| 渲染 | ✅ `scene_render`（CPU 软件光栅，返回 `image/png`） | 三级降级渲染（GPU 性能档） |
| 共享 | **`crates/core` + `crates/report` 完全共用** | 同左 |

两条路的**内核是同一个**，所以先做 MCP 不会产生要丢掉的代码；`report` 抽到 `core` 也正是为了这件事——
CLI、MCP、将来的 WASM 壳给出**同一份**观察结果，而不是三份各自漂移的渲染。

## 9. 还没做

- **`eval` 相关工具**：评分维度（`scene_evaluate`）还没接到工具面上。
  现在有规则告警（`scene_observe`）与遮挡测量（`scene_render`），但"分数"还没有——
  那是让 RSI 循环真正闭环的最后一块（归因表的 `Δscore` 也等它）。
- **prompts**：`prompts/list` 现在回空列表。可以把「家居摆场一轮 RSI」做成模板，
  但 `instructions` 已经覆盖了主要引导，先不着急。
- **HTTP transport**：目前只有 stdio。要远程/多客户端时才做，且必须带鉴权。
- **插件工具**：`scaffold` 模板里那种"自定义评测维度"还没有接到工具面上（要等 `eval`）。
- **撤销/重做的独立工具**：`scene_rollback` 的 `undo` 已经覆盖；`redo` 由 `core` 提供但没暴露
  （避免工具面变大，模型也容易误用）。

## 10. 踩坑（都是真的）

1. **测试脚手架必须是常驻会话**。第一版集成测试每次调用都起一个新的 `serve()`，于是"打开场景"和"编辑场景"
   落在两个进程里，全部报 `unknown_target`。真实客户端是长连接——所以测试改成
   **后台线程 + 双向管道**（`tests/session.rs` 里的 `Client`），这才测到真东西。
2. **通知不能被回复**。测试里断言"响应的 id 必须等于请求的 id"，
   这样一旦误回了通知，立刻就会被抓到（而不是在客户端那里表现为莫名的协议错乱）。
3. **`r#"..."#` 与 JSON 里的 `"#ffe9c9"` 冲突**（`"#` 会提前闭合原始字符串）。要用 `r##"..."##`。
   这条坑在 `core` 里踩过一次，写测试时又踩了一次——现在两个文件里都有注释。
4. **别拿 `--json` 的顺序猜**：MCP 报文是紧凑 JSON 且**键按字母序**（`{"id":…,"jsonrpc":…}`），
   冒烟脚本里按 `^{"jsonrpc"` 去 grep 会全部落空。
5. **冒烟里 grep 结构化字段要留意转义**：嵌在 `text` 字段里的 JSON 引号是 `\"`，
   所以断言应该匹配 `obj:sofa_01` 这种**值**，而不是 `objects` 这种**键名**。
