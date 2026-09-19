# rsi3d-harness

> **把 3D 资产变成 Agent 能「看见」、能「动手」、可度量、可回放的东西。**
>
> 一个 Agentic 3D 资产引擎 + 官方命令行。Rust 写就，可离线，可内嵌。

```
        ┌──────────────────────────────────────────┐
Agent → │  observe → edit → evaluate → rollback    │  ← 一轮 3D RSI 循环
        │  （每一次改动都是一条命令，命令进日志）      │
        └──────────────────────────────────────────┘
```

## 30 秒跑通

```bash
git clone https://github.com/rsi3d/rsi3d && cd rsi3d/rsi3d-harness
cargo build --release            # 产出两个二进制：rsi3d-harness、rsi3d

# 1. 用模板生成一个属于你自己的 3D 资产 Agent（自带 mock 引擎，可离线跑）
./target/release/rsi3d-harness scaffold new agent-app --var project=my-agent
cd my-agent && node agent.mjs --mock

# 2. 直接观察 / 编辑一个场景（不用写代码）
cd .. && ./target/release/rsi3d-harness scene show my-agent/scene.json

# 3. 看它长什么样：同时开两条流（浏览器里并排看；也能只订阅）
./target/release/rsi3d-harness serve my-agent/scene.json --port 8283 --open
#   场景流：glTF 快照+增量 → 客户端自己渲染（随便转视角，不花服务端算力）
#   图像流：服务端渲染的 PNG + 该视角的实测值（可复现，能当证据）
#   不想开浏览器：curl -N 'http://127.0.0.1:8283/stream/frame?token=<T>&view=top'
```

## 五个入口

| 入口 | 给谁用 | 命令 |
| --- | --- | --- |
| **MCP 服务** | VS Code / Cursor / Claude Code 里的 Agent（自然语言驱动） | `rsi3d-harness mcp --root .` |
| **CLI** | 人、脚本、CI | `rsi3d-harness scene show/edit/render/verify/log` |
| **转流服务** | 要在别处看见/改这个资产的人与 Agent | `rsi3d-harness serve scene.json` · 客户端 `rsi3d-harness stream <url>` |
| **脚手架** | 想要自己的 3D 资产系统 / Agent 的人 | `rsi3d-harness scaffold list/new/export` |
| **npm 包** | 不想碰 Rust 的人 | `npx @rsi3d/cli rsi3d-harness --help` |

外加 `rsi3d`（官方命令行）：发布、检索、验签、打包、驱动运行。它与引擎共用同一个 workspace，
所以 `cargo build --release` 一次就够。

### 在 VS Code 里用（MCP）

仓库根有现成的 `.vscode/mcp.json`；包装脚本 `scripts/mcp.sh` 会在缺二进制时自动构建。
打开 Chat → Agent 模式，引擎会提供九个工具：

| 工具 | 干什么 |
| --- | --- |
| `scene_open` / `scene_observe` | 打开场景；观察节点、**可编辑性**、告警、挡窗者、哈希 |
| `scene_edit` | 执行命令（**必须写 `reason`**），返回新版本号与逆命令 |
| `scene_rollback` | 回到某一版（RSI 的 best-so-far）或退回 N 步 |
| `scene_history` / `scene_diff` | 日志 + 归因表；两版精确差异 |
| `scene_render` | **直接返回观测图（PNG）** + 可见像素 + 窗前挡光带被遮挡 % |
| `scene_save` / `scene_verify` | 落盘成可重放文档；可复现性自检 |

不在 MCP 里、但同一套内核的第二个出入口是**转流**：`rsi3d-harness serve` 把当前状态同时推成
**场景流**（给客户端渲染）与**图像流**（服务端渲染，权威观测）。详见 [`docs/stream.md`](./docs/stream.md)。

Cursor / Claude Code 用同一份配置（`mcpServers` 键名）。详见 [`docs/mcp.md`](./docs/mcp.md)。

## 设计约束（改代码前先读）

1. **内核（`crates/core`）不含渲染、IO、网络、LLM**。它必须能被单测穷尽，
   且同时编译到 `wasm32` 与 native——同一个内核跑在两个壳里，行为不能有差别。
2. **一切改动走命令日志**。没有 op log 的改动不允许落盘，否则分数曲线无法归因、无法回滚到 best-so-far。
3. **可复现优先于性能**。同内容必得同 `scene_hash`；日志哈希不含墙钟。
4. **渲染后端是内部实现细节**，不暴露成用户/Agent 必须理解的概念。
5. **引擎可离线**。断网也要能 observe / edit / export。

## 四条不变量（由测试钉死）

| 不变量 | 含义 |
| --- | --- |
| **可逆** | `apply(cmd)` 后 `apply(inverse)` 回到**逐字段相同**的状态（逆命令是「恢复原值」，不是参数取反） |
| **可重放** | `replay()` 与实时状态逐字段相同；快照与重放结果一致 |
| **确定** | `scene_hash()` 稳定；日志哈希不含墙钟与随机数 |
| **游标一致** | `state_at(cursor()) == scene()`（`undo`/`redo` 的前提） |

## 代码结构

```
rsi3d-harness/
├─ crates/core/        引擎内核：统一场景表示 + 命令 + 日志/游标/快照 + 告警 + 归因（纯逻辑）
├─ crates/render/      渲染层：多视角观测图 + id pass + 遮挡测量（CPU 软件光栅，确定性）
├─ crates/stream/      流协议（传输无关）：glTF 快照/增量 + 会话状态机 + 背压规则
├─ crates/serve/       转流服务端：HTTP + SSE + 内嵌浏览器客户端 + 命令行客户端
├─ crates/mcp/         MCP 服务：把内核包成通用工具（VS Code / Cursor / Claude Code）
├─ crates/scaffold/    插件式脚手架：模板发现 / 变量渲染 / 生成
├─ crates/native/      壳②/native：bin `rsi3d-harness`（scaffold + scene + mcp + serve + stream）
├─ cli/                官方命令行：bin `rsi3d`（发布 / 检索 / 验签 / 打包 / 驱动运行）
├─ packages/rsi3d-cli/ npm 包（@rsi3d/cli，两个 bin 入口：rsi3d 与 rsi3d-harness）
├─ scaffolds/          内置模板源（它们本身就是被分发出去的产物）
├─ docs/               对外文档：内核契约 / 渲染层 / 转流 / MCP 接法 / 模板格式
└─ scripts/            smoke.sh（端到端冒烟）· mcp.sh（MCP 客户端启动器）
```

## 验证

```bash
cargo test              # 126 项：内核不变量 23 · 渲染 27 · MCP 协议与真实会话 17 · 脚手架 10 · 流协议与会话 25 · 转流端到端 11 · CLI 6 · 其余
bash scripts/smoke.sh   # 144 项端到端：模板生成 → 产物自测 → 自举 → 场景内核 → MCP（真进程）→ 渲染 → 转流（真起服务）→ npm 包
```

不变量测试与冒烟都是**可对账的证据**，不是"看起来没问题"：
`scene verify` 会在跑完之后告诉你重放是否等于落盘状态、每个版本是否都能重放、往返哈希是否稳定；
`scene render` 给出的「窗前挡光带被遮挡 x%」是一个可以被核对的数字，而不是一句形容。

### 里程碑记录

| 日期 | 项 | 结果 |
| --- | --- | --- |
| 2026-09-19 | **插件式脚手架**（`scaffold list/info/new/export/dir`） | ✅ `cargo test` 10/10；冒烟：4 模板生成、变量渲染、`--dry-run`/`--force`/冲突检测、产物自测（Agent 循环收敛 0.32→0.92、插件 6 项）、**两层自举**（模板生成模板再生成产物）、与官方 CLI 打通（build→sign→verify） |
| 2026-09-19 | **内核 `crates/core`**（USC + 命令 + 日志/游标/快照 + 告警 + 归因） | ✅ 不变量逐条钉死：**可逆**（逆命令恢复原值，而非参数取反）、**可重放**（`replay == 实时`、快照 == 重放、120 步混排压力）、**确定**（`scene_hash` 内容寻址、日志哈希不含墙钟）、**游标一致**（`state_at(cursor) == scene`） |
| 2026-09-19 | **可复现性自检**（`scene verify`） | ✅ 实测一轮 H0 客厅：挪沙发 → 调灯光 → 挪茶几（反而变糟） → `checkout` 回 best-so-far；`verify` 全绿：重放 == 落盘状态 · 全部版本可重放 · 落盘往返哈希不变 · 快照与重放一致 · 游标不变量 |
| 2026-09-19 | **MCP 服务 `crates/mcp`**（stdio） | ✅ 九个工具 + 两个 resource + `instructions` 工作流引导；stdout 严格只有协议报文（专门断言）；失败是 `isError:true` + 错误码 + **当前状态**（模型读得到才能改对）；写路径限在 `--root` 内，`../../etc/passwd` 被拒 |
| 2026-09-19 | **渲染层 `crates/render`**（软件光栅） | ✅ ① **Agent 能看见**——`scene_render` 返回 `image/png`；② **可计算**——「窗前挡光带被遮挡 50%」是量出来的数字；③ **可当证据**——同场景同参数同像素哈希，`checkout` 后四视角逐像素回到原样。两处几何事实写进了测试：水平面在正立面退化、垂直面在俯视图退化 |
| 2026-09-19 | **官方 CLI 并入同一 workspace**（D8） | ✅ 一次 `cargo build --release` 出两个二进制（`rsi3d-harness` + `rsi3d`）共用 `target/`，零告警；CLI 的 6 项单测随迁 |
| 2026-09-19 | **npm 包 `packages/rsi3d-cli`**（两个 bin 入口） | ✅ 包体 5.9 kB / 7 文件（不带二进制，按需取）；`RSI3D_BIN` 指坏路径时**直接失败**而不是静默换来源 |
| 2026-09-19 | **远程渲染 / 转流**（`crates/stream` + `crates/serve`） | ✅ 同时提供两条流：**场景流**（glTF 快照+增量 → 客户端 three.js，随便转不花服务端算力）与**图像流**（服务端渲染 PNG + 该视角实测值）；`EventSource` 断线重连自带 `Last-Event-ID`，**续传只补差量、不重发全量**；静止场景**零带宽**；背压“帧可丢、增量不可丢，过载压缩成全量” |

**路线调整（D7）**：原计划先做 WASM 内嵌 harness-use，现改为**先接通用工具（MCP / VS Code）**。
理由是 WASM 内嵌绑定在单一宿主的具体机制上，而它的阻塞问题都在对方那边；引擎的价值在内核，与宿主形态无关。
两条路共用 `crates/core`，所以先做 MCP 不会产生要丢掉的代码。

**内核实现时被测试逼出来的一个真 bug**：`undo()` 最初实现为「反转最后一条日志」。
因为撤销本身也进日志，「当前状态 == 最后一条的后置状态」这个假设在 `checkout` 之后不成立，
于是撤销越过正向历史后会在几个状态之间**来回振荡**。修正为**游标语义**（`undo` → `cursor-1`，
`redo` 上界是**历史前沿**而非日志长度，否则会掉进自己的撤销条目里）。

## 文档

| 文档 | 内容 |
| --- | --- |
| [`docs/core.md`](./docs/core.md) | **内核契约**：统一场景表示、命令与逆命令、日志/游标/快照、告警码表、错误码表 |
| [`docs/render.md`](./docs/render.md) | **渲染层**：为什么软件光栅、四个视角与朝向敏感、id pass 与遮挡测量、确定性的边界 |
| [`docs/stream.md`](./docs/stream.md) | **远程渲染 / 转流**：两条流的分工、SSE+POST 而非 WebSocket、只推变化、背压规则、断线续传、安全边界与红线 |
| [`docs/mcp.md`](./docs/mcp.md) | **MCP 接法**：工具面设计原则、安全边界、各客户端配置 |
| [`docs/scaffold.md`](./docs/scaffold.md) | **模板格式**：三层插件含义、变量与两层渲染、安全红线 |

## 许可

Apache-2.0。
