# CLI 速查

没有 MCP 时用这些（脚本、CI、内网）。命令信封与 MCP 收到的**逐字一致**。

```bash
rsi3d-harness --help          # 顶层
rsi3d-harness <子命令> --help  # 每个子命令都有中文说明与示例
```

## scene —— 场景内核

```bash
rsi3d-harness scene show scenes/living.json                     # 观察（只读）
rsi3d-harness scene edit scenes/living.json \
  --cmd '{"op":"transform","target":"sofa_01","params":{"translate":[0,0,1.3]},"reason":"把沙发挪出窗带"}' \
  --out living.doc.json                                          # 执行 + 落盘成文档
rsi3d-harness scene verify living.doc.json                       # 可复现性自检
rsi3d-harness scene log living.doc.json                          # 日志 + 归因表
rsi3d-harness scene render living.doc.json --out shot --views top,iso-sw   # 多视角观测图
```

- `--cmd` 可重复，按顺序执行；`--out` 落盘成**文档**（含日志，才能 verify）。
- `--json` 输出机器可读版本（脚本与 Agent 用）。观察用 `show`，别用 `log`（后者是历史，不是现状）。

## serve / stream —— 两条流

```bash
rsi3d-harness serve scenes/living.json --port 8283 --open
#   场景流：glTF 快照 + 增量 → 客户端自己渲染（随便转视角，不花服务端算力）
#   图像流：服务端渲染的 PNG + 该视角实测值（可复现，能当证据）
# 不想开浏览器：
curl -N 'http://127.0.0.1:8283/stream/frame?token=<T>&view=top'

rsi3d-harness stream http://127.0.0.1:8283 --token T --kind frame --out ./frames
```

- `--fps` 是**上限**，客户端只能要更低（只缩不放）。
- 静态资源与 `/healthz` 免令牌；**数据口一律要令牌**（页面能打开 ≠ 资产能拿走）。
- 默认只绑回环；绑 `0.0.0.0` 会告警。跨网请放 TLS 后面。

## render-mode —— 这台机器该用哪一档

```bash
rsi3d-harness render-mode                                     # 看规则表
rsi3d-harness render-mode --profile host.json                  # 判一份主机参数
rsi3d-harness render-mode --policy from-platform.json          # 用平台那份权威规则表
```

判定结果含模式、限制、理由、缺什么、出路与 `authority`（`platform` / `local`）。
平台不可达时用**同一份规则表**离线判定并标 `authority=local`——判定本来就能离线做。

## contract —— 契约产物（单一出处）

```bash
rsi3d-harness contract --out contract      # 从 Rust 类型派生键清单 / JSON Schema
rsi3d-harness contract --check             # 产物是否过期（CI 门禁用）
```

`contract/` 里的文件**勿手改**；改 Rust 类型后重新生成。

## scaffold —— 生成你自己的系统

```bash
rsi3d-harness scaffold list
rsi3d-harness scaffold info agent-app
rsi3d-harness scaffold new agent-app --var project=my-3d-agent
rsi3d-harness scaffold export harness-plugin ./my-templates/harness-plugin
```

内置模板（`agent-app` / `harness-plugin` / `pack` / `scaffold`）本身就是被分发出去的产物，
`agent-app` 自带 mock 引擎，**一条命令离线跑通**：`node agent.mjs --mock`。
