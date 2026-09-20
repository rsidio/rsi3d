<!-- 工程历史与决策记录（内部视角，不是首页） -->
> **这是工程日志，不是对外首页。** 对外介绍看 [`../README.md`](../README.md)（English）。
> 这里保留施工过程：里程碑与信号门、做过的取舍、踩过的坑、以及当时为什么那么定。
> 保留原样不改：它是记录，不是宣传。

---

<p align="center">
  <img src="assets/logo-plate.svg" alt="rsi3d" width="190">
</p>

# rsi3d

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

# 4. 拿走资产：导出标准 glTF 2.0（Blender / FyroxEd / three.js 直接能读）
./target/release/rsi3d-harness scene export my-agent/scene.json --out model.gltf

# 5. 反过来：把外部资产接进来（自己有 glTF/GLB/OBJ/STL；blend/fbx/usd 交给本机 Blender）
./target/release/rsi3d-harness scene import assets/robot.blend --out robot.scene.json
./target/release/rsi3d-harness scene import --formats   # 支持哪些、各走哪条路
#   → 同时写出几何 side-car（robot.scene.mesh.glb）：文档里只存 AABB + mesh_ref，
#     浏览器拿到 side-car 就画真网格，拿不到就画包围盒（不白屏）
```

## 五个入口

| 入口 | 给谁用 | 命令 |
| --- | --- | --- |
| **MCP 服务** | VS Code / Cursor / Claude Code 里的 Agent（自然语言驱动） | `rsi3d-harness mcp --root .` |
| **CLI** | 人、脚本、CI | `rsi3d-harness scene show/edit/render/verify/log/export/import` · `render-mode` |
| **转流服务** | 要在别处看见/改这个资产的人与 Agent | `rsi3d-harness serve scene.json` · 客户端 `rsi3d-harness stream <url>` |
| **契约产物** | 写自己工具链/插件的外部作者（键清单 + JSON Schema） | `rsi3d-harness contract --out contract` · 服务端 `/contract/scene.schema.json` |
| **脚手架** | 想要自己的 3D 资产系统 / Agent 的人 | `rsi3d-harness scaffold list/new/export` |
| **npm 包** | 不想碰 Rust 的人 | `npx @rsi3d/cli rsi3d-harness --help` |
| **Agent 技能** | 让 Agent 自己会用这套引擎（不用人盯命令行） | `rsi3d install skill/rsi3d-harness --agent claude-code` · 见 [`docs/bundle.md`](./docs/bundle.md) |

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

两条流都带**能力声明**：客户端在订阅 URL 上报 `?agent=…&cap=…&px=WxH`（`webgl2` / `three` / `headless` …），
服务端记账、**派生出档位**（`render_tier`）并在 `/healthz` 里报出来，`welcome` 会回声对账——**降级必须显式**
（取不到 CDN、GPU 上下文丢失都看得见），而不是静默变差。能力按 wgpu 的分法分成
**features / downlevel / limits** 三类，详细见 [`docs/stream.md`](./docs/stream.md) §10。

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
├─ crates/stream/      流协议（传输无关）：glTF 快照/增量 + 会话状态机 + 背压规则 + 渲染模式判定
├─ crates/serve/       转流服务端：HTTP + SSE + 内嵌浏览器客户端 + 命令行客户端
├─ crates/contract/    契约单一出处：从 Rust 类型派生键清单/JSON Schema，并门禁
手抄的镜像
├─ crates/mcp/         MCP 服务：把内核包成通用工具（VS Code / Cursor / Claude Code）
├─ crates/scaffold/    插件式脚手架：模板发现 / 变量渲染 / 生成
├─ crates/native/      壳②/native：bin `rsi3d-harness`（scaffold + scene show/edit/render/verify/log/export/import + mcp + serve + stream）
├─ cli/                官方命令行：bin `rsi3d`（发布 / 检索 / 验签 / 打包 / 驱动运行）
├─ packages/rsi3d-cli/ npm 包（@rsi3d/cli，两个 bin 入口：rsi3d 与 rsi3d-harness）
├─ scaffolds/          内置模板源（它们本身就是被分发出去的产物）
├─ contract/           派生出来的契约产物（键清单 / schema / 样板；勿手改）
├─ skills/             随工程分发的 **Agent 技能**（`rsi3d-harness`，SKILL.md + 引用资料）
├─ plugins/harness-use/ harness-use 宿主的「RSI 3D」功能应用（源码贡献，见其 README）
├─ docs/               对外文档：内核契约 / 渲染层 / 转流 / 契约 / MCP 接法 / 模板格式 / 导入 / 渲染模式 / 技能包
└─ scripts/            smoke.sh（端到端冒烟）· mcp.sh（MCP 客户端启动器）
```

## 验证

```bash
cargo test              # 186 项：内核不变量 23 · 渲染 27 · MCP 协议与真实会话 17 · 脚手架 10 · 流协议与会话 36 · 转流端到端 16 · 契约（含 schema 两道闸）16 · 导入 17 · 渲染模式 11 · CLI 6 · 其余
bash scripts/smoke.sh   # 235 项端到端：模板生成 → 产物自测 → 自举 → 场景内核 → MCP（真进程）→ 渲染 → 转流（真起服务）→ 导出 glTF → npm 包 → 契约门禁 → 导入 → 渲染模式
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
| 2026-09-19 | **导出（`scene export`）+ 与外部引擎的边界** | ✅ 命令行导出**标准 glTF 2.0 单文件**（内嵌 base64，含 `extras.rsi3d`）。用**第三方实现**（`@gltf-transform/cli`）验收时**抓出真 bug**：`scenes[].nodes` 为空数组 → 文件合法但标准加载器读到**空场景**（`renderVertexCount: 0`）。修复后第三方实测 `renderVertexCount: 144`（36 顶点 × 4 实例）、包围盒有限；已加回归测试+冒烟断言 |
| 2026-09-19 | **语料驱动的 glTF 往返测试**（借 rspirv 的语料纪律） | ✅ 9 份结构多样语料 × 两层断言：**结构自洽性**（场景引用/下标范围/字节范围/base64 长度/变换与 aabb 逐轴一致）+ **往返一致**（读回与源场景逐字段相同）+ 确定性；另含编辑与回滚后导出、坏输入门禁。断言语料规模，防覆盖变窄 |
| 2026-09-19 | **契约单一出处**（`crates/contract` + `contract/` + `docs/contract.md`） | ✅ 键清单/schema/样板全部**从 Rust 类型派生**（样例用结构体字面量写、枚举用穷尽 `match`）——改字段就编译不过、加变体就编译不过。四道门禁实测会红：产物过期、手抄 JS 键名漂移、出厂场景 `bandDepth`→`band_depth`（**内核不报错**、校验器报错）、消息变体改名（浏览器会静默忽略整条消息）。服务端 `/contract/scene.schema.json` 与 CLI 写出**逐字节一致**；`--check` 可当 CI 门禁 |
| 2026-09-19 | **能力声明 + 降级可见**（借 `glow` 的纪律，见 `prior-art` §11） | ✅ 客户端在订阅 URL 上声明 `?agent&cap`（词汇表单一出处，未知名字**不拒但绝不静默**）；服务端记账并暴露 `/healthz.clients`；`welcome` **回声对账**；**声明变了就重连重新声明**（带 `from=` 只补差量）。真浏览器实测：`WEBGL_lose_context` 丢失 → 能力降为 `scene·image·context-loss` + 横幅，`restoreContext()` → 恢复含 `webgl2·three` 并只补差量。顺带修：空闲预连接不再被当错误刷日志、一条老测试的读竞态 |
| 2026-09-19 | **导入层 `crates/io` + `scene import`**（外部资产接进来，见 [`docs/io.md`](./docs/io.md)） | ✅ 三条路分得清清楚楚：**自己读**（glTF/GLB/OBJ/STL，纯 Rust 无依赖）· **请 Blender**（blend/fbx/usd → GLB 再读；退出码 0 不算成功，退出码与产物都要查）· **请上游导出网格**（STEP/IGES/DWG 是 B-rep，明确拒绝并给出路）。几何进**内容寻址 side-car**（`<文档名>.mesh.glb`），文档里只留 `extras.rsi3d.mesh_ref`：浏览器读到 side-car 就画**真网格**、读不到就画包围盒（不白屏），服务端光栅的**证据档次不变**。报告写清：谁解析的 / 多少东西 / 多大 / **源文件坐标系 → 场景坐标系**（转过轴就说），截断、单位异常、孤立顶点、薄片一律主动说出来。顺带修掉两个**无声** bug：所有对象共享全局顶点表会让每个对象的 AABB 变成整体 AABB（间距测量全假）、OBJ 判断"这个对象有没有东西"看错了字段。真浏览器实测：`几何 真网格 3/3`，立方体+球真的画出来了 |
| 2026-09-19 | **能力声明的形状**（借 `wgpu` 的 features/downlevel/limits 分法，见 `prior-art` §12） | ✅ `KNOWN_CLIENT_CAPABILITIES` 从元组升级成 `{name, meaning, kind, needs_any}`：`webgl1` 是**降级档**而不是与 `webgl2` 并列的能力，依赖也声明化，于是"自相矛盾 / 依赖没满足"是**算出来的**（冒烟实测会喊）；服务端名册给出**派生档位** `render_tier` 与**实际帧尺寸** `frame_px`；新增 limits：`?px=WxH` 客户端报显示上限，服务端**只缩不放**（实测 `px=240x180` → 帧真的是 240×180）。**没有**引 wgpu/naga 依赖（零着色器零 GPU；D9 不让 GPU 进入逐像素可复现的验收线） |

**导入层还没做**：真网格进服务端光栅（要动内核契约）· PLY 读取器 · FreeCAD `freecadcmd` 那条 STEP 路 · 非标准 blend 外壳（32 字节块头）的自动恢复——诊断已经做完（[`docs/io.md`](./docs/io.md) §5），但那个文件是 **Blender 4.5** 写的，本机 4.2.9 读不了，硬把版本号降下来会**段错误**，所以等 Blender 4.5+。

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
| [`docs/render-mode.md`](./docs/render-mode.md) | **渲染模式**：探测 → 判定 → 应用；规则表为什么是数据、limits 只缩不放、「未知不等于不行」、两侧判定的共享语料与隐私边界 |
| [`docs/io.md`](./docs/io.md) | **导入**：三条路（自己读 / 请 Blender / 请上游导出网格）、几何 side-car 为什么不进文档、坐标系为什么写两个字段、非标准与更新版 blend 的逐步诊断 |
| [`docs/contract.md`](./docs/contract.md) | **契约单一出处**：为什么从 Rust 类型派生、四份产物、extras 吞键的坑、八道门禁、**schema 从类型派生（schemars）与防缩水闸**、不做什么 |
| [`docs/render-mode.md`](./docs/render-mode.md) | **渲染模式**：探测 → 判定 → 应用；规则表为什么是数据、limits 只缩不放、「未知不等于不行」、两侧判定的共享语料与隐私边界 |
| [`docs/io.md`](./docs/io.md) | **导入**：三条路（自己读 / 请 Blender / 请上游导出网格）、几何 side-car 为什么不进文档、坐标系两个字段、非标准与更新版 blend 的诊断 |
| [`docs/mcp.md`](./docs/mcp.md) | **MCP 接法**：工具面设计原则、安全边界、各客户端配置 |
| [`docs/scaffold.md`](./docs/scaffold.md) | **模板格式**：三层插件含义、变量与两层渲染、安全红线 |

## 许可

Apache-2.0。
