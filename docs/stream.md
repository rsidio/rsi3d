# 远程渲染 / 转流（`rsi3d-harness serve` / `stream`）

把 3D 资产**转成流**送给远处的人/Agent/浏览器。这一层解决的是"资产在客户边界内，
但有人要在别处看见它、甚至改它"。

代码：`crates/stream`（协议 + 会话 + glTF，传输无关）、`crates/serve`（HTTP/SSE + 浏览器客户端
+ 命令行客户端）。词表见 `prd/glossary.md`。

---

## 1. 两条流，不是二选一

| 订阅 | 推什么 | 谁渲染 | 代价 | 性质 |
| --- | --- | --- | --- | --- |
| `/stream/scene` | glTF 2.0 快照 + 增量 | **客户端**（three.js） | 资产到了客户端 | **交互视图**，可自由转视角，不花服务端算力 |
| `/stream/frame` | PNG 帧（+ 该视角的测量值） | **服务端** | 服务端 CPU + 带宽 | **权威观测**：可复现、能当证据 |

这个区分不是花样，它直接来自内核的性质：**同状态必得同像素**（渲染确定性），
所以只有服务端渲出来的那一帧能拿去对账；场景流是给人（和模型）看的交互视图。

两条流可以同时订阅（浏览器客户端就是并排显示），因为它们回答的是两个不同问题：
"这东西长什么样、我能怎么改" vs "现在这个状态，权威结论是什么"。

---

## 2. 为什么是 SSE + POST，不是 WebSocket

两条流都是**服务端单向推**，客户端只需要偶发地发命令（改场景 / 换视角）。这个形状下 SSE 白拿三样东西：

1. **自动重连 + 断线续传**。`EventSource` 断线后自己重连，并把上次的 `id:` 作为
   `Last-Event-ID` 回传。我们把 **`id` 定义成"状态版本"**，于是续传 = 服务端算出
   「你看到第 3 版 → 现在是第 9 版」那段增量。能这么做的前提是内核的硬性质：
   **任意历史版本都可重放**（`scene verify` 校验的就是这个）。
2. **`curl -N` 就能验收**，不用装任何客户端库。
3. 反方向用普通 `POST`，天然带 4xx（`409` 带内核错误码，如 `unknown_target`）。

接口：

| 方法 | 路径 | 说明 |
| --- | --- | --- |
| GET | `/` | 内嵌的浏览器客户端（两条流并排） |
| GET | `/client.js` | 客户端脚本（`include_str!` 进二进制） |
| GET | `/healthz` | 计数/版本/哈希（**不需要令牌**） |
| GET | `/stream/scene` | 场景流（SSE） |
| GET | `/stream/frame?view=top\|front\|iso-sw\|iso-se` | 图像流（SSE） |
| GET | `/observe` | 与 MCP/CLI **同一份**观测（节点表、挡窗者、告警） |
| GET | `/snapshot.gltf` | 当前状态直接导出成 glTF（可直接喂 three.js / Blender） |
| POST | `/command` | 命令，body 就是**裸信封** `{op,target,params,reason,expect}` |
| POST | `/camera` | 换视角（每连接状态） |

`/stream/*` 支持 `?from=<rev>`（等价于 `Last-Event-ID`）、`?kind=`、`?view=`。

---

## 3. 只推变化

- 场景流：`Subscription` 记住"这个连接已知哪一版"，版本没变就**一条都不发**。
- 图像流：记住上次推出去的 `image_hash`，像素一样就不推（而且**连渲都不渲**，省 CPU），
  并用 `fps` 限速。

静止场景 = **零带宽**。这不是优化，是内核确定性的直接收益（哈希稳定 → 去重可靠）。

命令行客户端会因此"没动静"——所以 `stream` 有 `--idle`（默认 8 秒）：超时不是错误，
而是**明确告诉你"静止场景服务端不重发"**，然后正常收工。这是刻意的：把设计行为讲清楚，
比让人对着空屏幕猜好。

---

## 4. 背压：帧可丢，增量不能丢

`crates/stream` 的 `Outbox` 是这个设计的核心：

| 消息 | 规则 | 为什么 |
| --- | --- | --- |
| 帧 | **latest-wins**，新的直接覆盖旧的 | 帧是"当前状态的一张照片"，旧帧没有价值 |
| 增量 | **不能丢**（累积语义，丢一个就永久错位） | 客户端靠它推进本地状态 |
| 积压超过预算（16） | **压缩成一次全量快照**，而不是丢数据 | 用几十 KB 换"状态一定正确" |

还有一条硬约束：**压缩时必须同一轮内把快照补上**（`needs_snapshot()` → `arm_snapshot()`），
否则中间新产生的增量会因为 `from` 比全量新而错位——客户端会拿到接不上的增量，
那是比"多花点带宽"糟得多的结果。

物理上的背压来自"写"本身：推送循环一次只发一块，写完 `flush` 才进下一轮。
客户端读得慢 → 写变慢 → 该连接的消息自然在 `Outbox` 里被合并/压缩。
不需要额外的限流器。

---

## 5. 版本号用"日志前沿"，不是游标

线上 `id` / `Patch.to` 一律是 `Document::revision()`（日志前沿），**不是** `cursor()`。因为线上版本号必须满足：

1. **单调不减**——否则 `Last-Event-ID` 会乱；
2. **`state_at(id)` 就是客户端手上那个状态**——对账的根据。

游标会因为 `checkout`（回滚）**倒退**，两条都不满足（它是给撤销/重做用的）。
用日志前沿还有个副作用是好的：**回滚也能用增量表达**（`patch` 比的是两个状态的差异），
不必笨重地重发全量。

---

## 6. 载荷形状

- **快照 = 真 glTF 2.0**：`three.js` 的 `GLTFLoader.parse` 直接能吃，不发明格式。
- **增量 = 按 id upsert + 完整对象**（不是字段级 before/after）。理由是客户端逻辑变成一句话
  「按 id 覆盖」，不会出现"少支持一个字段"这种**静默错误**。代价是每次多几十到几百字节。
- **场景级事实随增量一起发**：`blockers`（窗前挡光者）在 `ScenePatch` 里。
  挡窗者会随家具挪动而变——只更新几何不更新结论，会让画面看着对、结论已经错。
- **几何档次如实声明**：H0 还没有真网格，所以 `welcome.geometry` 和每个节点的
  `extras.rsi3d.geometry` 都写着 `aabb-proxy`。客户端不会误以为收到了真模型。

### 每一帧都要自报家门：谁渲的、能不能当证据

`frame` 消息里有一个 `renderer` 字段：

```json
{ "type": "frame", "revision": 3, "view": "top", "width": 480, "height": 360,
  "renderer": "cpu-raster/v1", "image_hash": "…", "png_base64": "…", "band_occlusion": 0.5 }
```

- `cpu-raster/v1` —— 本引擎的确定性软件光栅（**唯一能当证据的档**：同状态必得同像素）。
- 命令行客户端把它打进每帧那一行，**非证据档会当场说出来**（"非证据档：不可复现，不得用于验收"），
  浏览器客户端在图像流旁边显示 `档 cpu-raster/v1`（证据档绿色，其它红色）。

为什么这值得一个字段，而不是只写在文档里：**"一帧"这个形态是可以被别的东西灌进来的**——

- **GPU 渲染档**：跨驱动/跨设备的浮点与光栅化差异做不到逐像素可复现；
- **从别人的进程里钩出来的画面**：比如 [`veeenu/hudhook`](https://github.com/veeenu/hudhook)
  那种（注入 DLL + hook 人家的 `Present`，Windows/Wine、dx9/11/12/opengl3）。
  那种帧**跟我们的命令日志没有任何关系**：不可复现，也无法归因到某一版状态。

这些帧不是"坏"的，但它们**不能混进证据**。所以每个档必须在
`crates/stream/src/protocol.rs` 的 `FRAME_RENDERERS` 里登记，并说清自己算不算证据；
`is_evidence_renderer()` 是唯一的判据，而 `scene verify` 那类验收只认 `cpu-raster/v1`。
契约测试钉了两条：**证据档只能有一个**（多了就说明有人想把不可复现的东西也算成证据），
且**每个档都必须在本文档里解释清楚**。

顺带说清我们对"注入式 overlay"的立场（详见 `prd/rsi3d-harness/prior-art.md` §13）：
**不做**——不往客户的进程里注入代码、不 hook 别人的渲染管线。要在客户的应用里显示我们的观测，
走它们的官方插件 API，或者用本文档的浏览器并排视图（场景流 + 图像流）。

---

## 7. 安全边界与红线

- **默认只绑 `127.0.0.1`**，且数据口**必须带一次性令牌**（`?token=` / `Authorization: Bearer` /
  `X-Rsi3d-Token`），比较是定时的。静态资源（`/`、`/client.js`）与 `/healthz` 不需要令牌——
  页面能被打开 ≠ 资产能被拿走。
- 绑到非回环地址时**大声告警**（`--bind 0.0.0.0` 会往 stderr 打三行警告）。
- **HTTP 是明文**：跨网请放在 TLS 反向代理后面。
- 命令行客户端只支持 `http://`，不假装支持 HTTPS（要 HTTPS 就用 `curl -N` 或代理）。
- **红线：本服务跑在客户的边界内**（本机 / 内网 / 客户自己的容器）。
  数据**不经过** `rsi3d-online`——数据面永远在客户手里。

---

## 8. HTTP 是最小自写的（一次真实的取舍）

先用的是 `tiny_http`，然后发现**它做不了 SSE**：

- 响应体交给 `chunked_transfer::Encoder`，它先把数据攒进自己的缓冲（到 4KB 才发一块）；
- `flush()` 只在**整个响应结束时**调一次。

结果：一次几百字节的增量永远出不去，`curl -N` 连响应头都收不到。这不是参数没调对，
而是"缓冲整个响应"和"流"在骨子里冲突。于是 `crates/serve/src/http.rs` 自己写了约 200 行：
读请求行/头/可选 body（有长度上限），回定长或分块响应，**每块写完立刻 flush**。
换来零依赖、行为可见。代价：没有 TLS / keep-alive 复用 / HTTP/2——对"本机观测口"都不需要。

HTTP/1.0 客户端会被明确拒绝（`426 http_1_1_required`）：它没有分块编码，
服务端只能把永不结束的流缓冲进内存。

---

## 9. 怎么用

```bash
# 服务端（默认只绑回环；令牌不给就随机生成并打印）
rsi3d-harness serve scene.json --port 8283 --open
#   → 打印带令牌的客户端地址，浏览器打开就是两条流并排

# 命令行看一眼（不需要浏览器）
curl -N 'http://127.0.0.1:8283/stream/frame?token=<T>&view=top' | head -c 400

# 客户端：录帧 / 导出 glTF
rsi3d-harness stream http://127.0.0.1:8283 --token <T> --kind frame --view top --out frames/
rsi3d-harness stream http://127.0.0.1:8283 --token <T> --kind scene --out out/ --limit 2

# 手动验续传：假装自己只看到 rev 0，服务端应当只补差量、不重发全量
rsi3d-harness stream http://127.0.0.1:8283 --token <T> --kind scene --from 0 --limit 2
```

浏览器客户端（`crates/serve/src/client.js`）做了三件有意的事：

1. **左边场景流 + 右边图像流**：把"两种代价/两种用途"直接摆在屏幕上；
2. three.js 从 CDN 动态 `import`，**取不到就明确降级**到图像流并说明原因（不白屏）；
3. 拖拽/滚轮只动本地相机——这正是"场景流不花服务端算力"的字面意思。

---

## 10. 客户端要声明自己能干什么（借 glow 与 wgpu 的纪律）

做法来自 [`grovesNL/glow`](https://github.com/grovesNL/glow)：**能力是声明出来的，不是被假设的**。
它把 `supported_extensions()` 放进 `HasContext` trait，于是 native 与 WebGL 两个后端都**必须**
回答"你支持什么"，调用前先查而不是先假定。

[`gfx-rs/wgpu`](https://github.com/gfx-rs/wgpu) 把这件事说得更细，我们照它的分法把声明拆成三类：

| wgpu 的叫法 | 它的实质 | 我们对应什么 |
| --- | --- | --- |
| **features**（*"Features that are not guaranteed to be supported"*） | 有没有某项可选能力 | `scene` / `image` / `three` / `context-loss` |
| **downlevel flags**（`wgpu_hal` 里每个后端存一份） | 老后端**缺**了什么 | `webgl1`——它不是与 `webgl2` 并列的能力，而是**降级档** |
| **limits**（`Limits::downlevel_webgl2_defaults()` 这类） | 数值上限 | `?px=WxH`：客户端能显示多大 |

原来我们只有服务端声明自己（`welcome.geometry = aabb-proxy`）。客户端那边是**猜**的：CDN 被拦、
GPU 上下文丢失这类事，服务端**完全看不见**——`/healthz` 只会说"有 3 条连接"。

```
GET /stream/scene?token=T&agent=rsi3d-web/0.1.0&cap=webgl2,three,scene,image,context-loss
GET /stream/frame?token=T&cap=image&px=240x180
```

| 能力名 | 类别 | 含义 | 谁声明 |
| --- | --- | --- | --- |
| `scene` | consume | 能消费场景流（glTF 快照 + 增量） | 浏览器、CLI（`--kind scene`） |
| `image` | consume | 能消费图像流（PNG 帧） | 浏览器、CLI（`--kind frame`） |
| `three` | render | 有**当下可用**的 three.js 客户端渲染路径 | 浏览器 |
| `webgl2` | render | 本机有 WebGL2（正常档） | 浏览器 |
| `webgl1` | **downlevel** | 只有 WebGL1（降级档） | 浏览器 |
| `context-loss` | robustness | 会处理 GPU 上下文丢失/恢复，而不是假装没发生 | 浏览器 |
| `headless` | form | 没有屏幕（命令行/服务端消费者） | CLI |

规则只有两条：

1. **声明走订阅 URL 的查询参数**，不走 `ClientMessage`——一条 SSE 连接就是一次订阅，
   它是唯一天然带"连接身份"的位置；POST 那条通道（`/command`）服务端分不清是谁发的。
2. **声明变了就断开重连并重新声明**。于是服务端的名册永远是真的，不需要"更新"这种
   半新半旧的状态；而且新连接带 `from=<本地版本>` ⇒ 只补差量——这正好是"GPU 上下文
   恢复后要重建场景"所需要的东西，一个机制解决两件事。

### 服务端做什么：记账、派生结论、把问题说出来

`welcome` 会**回声**（`client_agent` / `client_capabilities` / `client_render_tier`），
`/healthz` 会报出名册——**包括派生出来的结论**，而不是把一堆标志丢给运维自己拼：

```bash
curl -s http://127.0.0.1:8283/healthz | jq '.clients[] | {agent, render_tier, capabilities, frame_px, notes}'
```

| 名册字段 | 含义 |
| --- | --- |
| `render_tier` | 派生的档位：`three` / `webgl2` / `webgl1-downlevel` / `frame-only` / `headless` / `unknown` |
| `frame_px` | **实际**会给它发多大的帧（= 它的预算与服务端默认档取小） |
| `notes[]` | 声明里的问题与降级说明，分三类：`contradiction`（自相矛盾）/ `unmet`（依赖没满足）/ `degraded`（能连但画不出来） |
| `unknown[]` | 我们不认识的能力名（**不拒、但都不丢**） |

三条硬规定：

- **不认识的能力名不拒**（旧服务端 + 新客户端要能共存），但**绝不静默**：服务端打到 stderr，
  名册里的 `unknown[]` 也带着它；
- **矛盾与依赖没满足**要喊（wgpu 用 `MissingFeatures` 在建设备时就报错；我们只报不拒，
  因为连接本身是好的）；**降级不喊**，它会在名册里以 `degraded` 出现；
- 名册**只反映当下**：连上就写、断开就抹。为了让"断开"及时可信，服务端会主动探测对端是否
  已经消失（SSE 是单向的，否则要到下一次写、最长一个心跳才发现——那段时间名册在说谎）。

### 显示预算（limits）

`?px=WxH` 是**上限**：服务端只**往下调**（等比缩放到这个框内），绝不超过自己的默认档；
小到没意义会抬到下限（`MIN_FRAME_SIDE`）且不变形。目的很朴素：手机端不必收 480×360 的帧，
4K 屏也不必被卡在这个尺寸。

客户端侧的降级因此全部**可见**，而不是静默：

| 情况 | 客户端行为 | 服务端看得到什么 |
| --- | --- | --- |
| CDN 取不到 three.js（离线/被拦） | 保留图像流，场景流重新声明为**无 `three`**，界面说明原因 | `render_tier` 从 `three` 降为 `webgl2` |
| GPU 上下文丢失（驱动重置/休眠） | `preventDefault` 后重新声明为**无 GPU**；恢复时重建渲染器并重新拉全量 | `render_tier` 降为 `frame-only` |

---

## 11. 验证

```bash
cargo test -p rsi3d-harness-stream -p rsi3d-harness-serve   # 35 + 15 个用例
bash scripts/smoke.sh                                        # 含真起服务的 49 项转流/契约断言
```

`crates/serve/tests/http.rs` 是**真起 HTTP 服务、真用 socket 连**，覆盖：
无令牌被拒、命令回执、静止场景零带宽、`Last-Event-ID` 续传只补差量、非法 id 退回全量、
回滚表达成增量、HTTP/1.0 被拒、停服务后连接结束与端口释放，以及客户端声明的
**回声/名册/断开即抹/未知能力名不拒但标出来**。

`crates/stream/tests/gltf_corpus.rs` 是**语料驱动**的边界测试（做法借自 `gfx-rs/rspirv` 的
「真实 blobs + 往返」）：9 份结构多样的场景（空/单/12 对象/重叠/超界/极薄片/无窗/多灯/Unicode），
每份都要过**两层断言**：

1. **结构自洽性**——按 glTF 的引用关系逐条查（`scenes[].nodes` 必须覆盖全部节点且下标在界内、
   `mesh`/`accessor`/`bufferView` 下标有效、字节范围不越界、内嵌 base64 字节数与 `byteLength` 一致、
   节点变换与 `extras.rsi3d.aabb` **逐轴一致**）；
2. **往返一致**——导出的 glTF 读回来，节点表与源场景逐字段相同；再加上同状态两次导出字节相同。

为什么两层都要：`scenes[].nodes` 写成空数组的那个 bug **能通过往返**（读回来只认扁平 `nodes` 池），
**往返绿 ≠ 文件能用**。同一文件里还测了「编辑与回滚后导出」与「坏输入必须在进门时被拒」。

---

## 12. 还没做

- **GPU / 任意轨道相机**：图像流目前只有四个预置视角（CPU 软件光栅）。任意相机要等渲染管线升级。
- **WebSocket / WebRTC 传输**：会话层与协议层没碰 HTTP，换传输只改 `crates/serve`。
- **鉴权粒度**：现在是一把一次性令牌（本机语义）。要多人多权限得接平台的身份体系。
- **二进制帧**：现在帧是 base64 塞在 SSE 里（调试友好、`curl` 可读）。上量后应换成二进制传输。
