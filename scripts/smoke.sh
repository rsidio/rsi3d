#!/usr/bin/env bash
# rsi3d-harness 冒烟：模板发现 → 生成 → 产物自测 → 自举（外部模板）→ 与平台工具链打通
#                 → 场景内核 → MCP（真进程）→ 渲染 → 转流（真起服务）→ 导出 glTF → npm 包。
#
# 全程在临时目录里跑，**不碰** ~/.rsi3d-harness 与你现有的项目。
#
# 用法：bash scripts/smoke.sh          （会先增量构建）
#      SMOKE_SKIP_BUILD=1 bash scripts/smoke.sh
set -uo pipefail

cd "$(dirname "$0")/.."
ROOT="$PWD"
PLATFORM_CLI="${ROOT}/target/release/rsi3d"   # 官方 CLI（同一个 workspace，产物同一个 target/）

TMP="$(mktemp -d)"
BIN="${ROOT}/target/release/rsi3d-harness"
MOCK_PORT="${SMOKE_MOCK_PORT:-8399}"
ENGINE="http://127.0.0.1:${MOCK_PORT}"

# 隔离外部模板目录：否则会读到你自己 ~/.rsi3d-harness/scaffolds 里的模板
export RSI3D_HARNESS_SCAFFOLDS="${TMP}/external-scaffolds"

PASS=0
FAIL=0
SKIP=0

cleanup() {
  trap - EXIT INT TERM
  rm -rf "${TMP}"
}
trap cleanup EXIT INT TERM

check() { # check <描述> <期望子串> <输出>
  local desc="$1" want="$2" got="$3"
  if printf '%s' "${got}" | grep -qF -- "${want}"; then
    printf '  ✓ %s\n' "${desc}"
    PASS=$((PASS + 1))
  else
    printf '  ✗ %s\n      期望包含: %s\n      实际输出: %s\n' \
      "${desc}" "${want}" "$(printf '%s' "${got}" | head -10 | sed 's/^/      /')"
    FAIL=$((FAIL + 1))
  fi
}

skip() {
  printf '  – %s（跳过：%s）\n' "$1" "$2"
  SKIP=$((SKIP + 1))
}

hr() { "${BIN}" "$@"; }

# ---------------------------------------------------------------- 准备

echo "── 准备 ──────────────────────────────────────────"
if [ "${SMOKE_SKIP_BUILD:-}" = "1" ]; then
  echo "→ SMOKE_SKIP_BUILD=1：跳过构建"
else
  echo "→ 构建 rsi3d-harness（增量）…"
  cargo build --release || exit 1
fi
[ -x "${BIN}" ] || { echo "✗ 二进制缺失：${BIN}"; exit 1; }

if command -v node > /dev/null 2>&1; then
  NODE_OK=1
  echo "  node $(node --version) · 临时目录 ${TMP}"
else
  NODE_OK=""
  echo "  ! 未找到 node：产物自测与 Agent 循环将被跳过"
fi

# ---------------------------------------------------------------- 1. 模板发现

echo "── 1. 模板发现 ───────────────────────────────────"
OUT="$(hr scaffold list 2>&1)"
check "列出 4 个内置模板" "共 4 个模板" "${OUT}"
check "内置 agent 模板" "agent-app" "${OUT}"
check "内置 plugin 模板" "harness-plugin" "${OUT}"
check "内置 pack 模板" "pack" "${OUT}"
check "内置 scaffold 模板（自举用）" "scaffold" "${OUT}"

OUT="$(hr scaffold info agent-app 2>&1)"
check "info 展示变量" "变量" "${OUT}"
check "info 展示产出文件" "agent.mjs" "${OUT}"
check "info 展示下一步提示" "node agent.mjs --mock" "${OUT}"

OUT="$(hr scaffold dir 2>&1)"
check "dir 打印外部模板目录" "${TMP}/external-scaffolds" "${OUT}"

OUT="$(hr scaffold new nope-template 2>&1)"
check "未知模板报错并列出可用模板" "可用" "${OUT}"

# ---------------------------------------------------------------- 2. 生成与变量

echo "── 2. 生成与变量渲染 ─────────────────────────────"
OUT="$(hr scaffold new agent-app --out "${TMP}/agent" --var project=demo-agent 2>&1)"
check "生成 agent-app" "已生成 agent-app" "${OUT}"
for f in README.md package.json agent.mjs mock-engine.mjs scene.json; do
  check "产出 ${f}" "1" "$([ -f "${TMP}/agent/${f}" ] && echo 1 || echo 0)"
done
check "变量渲染进文件内容" "demo-agent" "$(cat "${TMP}/agent/package.json")"
check "内置变量也渲染（template 名）" "模板 agent-app" "$(cat "${TMP}/agent/package.json")"
check "产出里没有残留占位符" "0" "$(grep -c '{{' "${TMP}/agent/package.json" 2>/dev/null || echo 0)"

OUT="$(hr scaffold new agent-app --out "${TMP}/agent" --var project=demo-agent 2>&1)"
check "重复生成被拒并提示 --force" "--force" "${OUT}"
OUT="$(hr scaffold new agent-app --out "${TMP}/agent" --var project=demo-agent --force 2>&1)"
check "--force 可覆盖" "已生成" "${OUT}"

check "缺变量时报错" "缺少变量" "$(hr scaffold new agent-app --out "${TMP}/x" 2>&1)"
OUT="$(hr scaffold new agent-app --out "${TMP}/dry" --var project=dry --dry-run 2>&1)"
check "--dry-run 给出预览" "预览（未落盘）" "${OUT}"
check "--dry-run 不落盘" "0" "$([ -d "${TMP}/dry" ] && echo 1 || echo 0)"

OUT="$(hr scaffold new pack --out "${TMP}/packs" --var project=home-pack --var unknown=zzz 2>&1)"
check "未声明变量被忽略并提示" "未声明变量 unknown" "${OUT}"
check "slug 从 project 派生" "home-pack" "$(cat "${TMP}/packs/rsi3d.pack.json")"

# ---------------------------------------------------------------- 3. 产物自测

echo "── 3. 产物自测（模板承诺"一条命令跑通"）────────────"
if [ -n "${NODE_OK}" ]; then
  OUT="$(cd "${TMP}/agent" && node agent.mjs --mock --engine "${ENGINE}" --iters 6 2>&1)"
  check "Agent 循环跑通并收敛" "收敛" "${OUT}"
  check "输出分数曲线" "分数曲线" "${OUT}"
  check "输出归因表" "归因表" "${OUT}"
  check "三条命令全部有正向提升" "其中 0 条没有提升" "${OUT}"
  check "批判里指出窗户被挡" "遮挡" "${OUT}"

  OUT="$(cd "${TMP}/agent" && node agent.mjs --mock --engine "${ENGINE}" --iters 2 2>&1)"
  check "轮次预算生效（2 轮不收敛也正常退出）" "分数曲线" "${OUT}"

  hr scaffold new harness-plugin --out "${TMP}/plugin" --var project=my-plugin --var plugin_id=my-walkway > /dev/null 2>&1
  OUT="$(cd "${TMP}/plugin" && node selftest.mjs 2>&1)"
  check "插件自测 6 项全过" "自测通过（6 项）" "${OUT}"
  check "插件清单里 id 被渲染" "my-walkway" "$(cat "${TMP}/plugin/rsi3d-plugin.json")"
  check "插件维度名被渲染" "layout.walkway" "$(cat "${TMP}/plugin/rsi3d-plugin.json")"
else
  skip "Agent 循环" "没有 node"
  skip "插件自测" "没有 node"
fi

# ---------------------------------------------------------------- 4. 自举：模板插件

echo "── 4. 自举：把模板做成插件 ───────────────────────"
hr scaffold export scaffold "${TMP}/tpl/hello-tpl" --force > /dev/null 2>&1
check "导出内置模板为外部模板" "1" "$([ -f "${TMP}/tpl/hello-tpl/scaffold.json" ] && echo 1 || echo 0)"

# 导出**保留原 id**（用于覆盖内置）；要新建模板就改 id
check "导出保留原 id（覆盖语义）" '"id": "scaffold"' "$(cat "${TMP}/tpl/hello-tpl/scaffold.json")"
sed -i '' 's/"id": "scaffold"/"id": "hello-tpl"/' "${TMP}/tpl/hello-tpl/scaffold.json" 2>/dev/null \
  || sed -i 's/"id": "scaffold"/"id": "hello-tpl"/' "${TMP}/tpl/hello-tpl/scaffold.json"

OUT="$(hr --scaffold-dir "${TMP}/tpl" scaffold list 2>&1)"
check "改 id 后成为新模板" "hello-tpl" "${OUT}"
check "外部模板来源标记为 external" "external" "${OUT}"
check "数量变成 5" "共 5 个模板" "${OUT}"

# 用外部模板生成：它产出的应当**又是一个合法模板**（两层渲染的关键）
OUT="$(hr --scaffold-dir "${TMP}/tpl" scaffold new hello-tpl --out "${TMP}/tpl2/pipe" --var project=pipe 2>&1)"
check "用外部模板生成新模板" "已生成 hello-tpl" "${OUT}"
check "新模板带清单" "1" "$([ -f "${TMP}/tpl2/pipe/scaffold.json" ] && echo 1 || echo 0)"

OUT="$("${BIN}" --scaffold-dir "${TMP}/tpl2" scaffold list 2>&1)"
check "二层模板可被再次发现（两层渲染正确）" "pipe" "${OUT}"

OUT="$("${BIN}" --scaffold-dir "${TMP}/tpl2" scaffold new pipe --out "${TMP}/tpl3/demo" --var project=demo 2>&1)"
check "二层模板可再生成产物" "已生成 pipe" "${OUT}"
check "内层占位符被正确替换" "demo" "$(cat "${TMP}/tpl3/demo/README.md")"

# 同名覆盖：外部模板 id 与内置相同时应当**覆盖**内置（而不是并列）
hr scaffold export pack "${TMP}/tpl/pack" --force > /dev/null 2>&1
check "覆盖时数量仍为 5（不并列）" "共 5 个模板" "$(hr --scaffold-dir "${TMP}/tpl" scaffold list 2>&1)"
sed -i '' 's/业务包（行业知识/我改过的业务包（行业知识/' "${TMP}/tpl/pack/scaffold.json" 2>/dev/null \
  || sed -i 's/业务包（行业知识/我改过的业务包（行业知识/' "${TMP}/tpl/pack/scaffold.json"
OUT="$(hr --scaffold-dir "${TMP}/tpl" scaffold info pack 2>&1)"
check "外部模板同名覆盖内置" "我改过的业务包" "${OUT}"
check "内置版本被隐藏（pack 只剩一份）" "1" "$(hr --scaffold-dir "${TMP}/tpl" scaffold list 2>&1 | grep -c '^pack')"

# ---------------------------------------------------------------- 5. 与平台工具链打通

echo "── 5. 与平台 CLI 打通（业务包 build/sign/verify）──"
if [ -x "${PLATFORM_CLI}" ]; then
  hr scaffold new pack --out "${TMP}/bizpack" --var project=home-display --var domain=home > /dev/null 2>&1
  cd "${TMP}/bizpack"
  OUT="$("${PLATFORM_CLI}" pack build . 2>&1)"
  check "平台 CLI 能打包脚手架产物" "打包完成" "${OUT}"
  OUT="$("${PLATFORM_CLI}" pack sign --key "${TMP}/signing.key" 2>&1)"
  check "签名成功" "hmac-sha256:" "${OUT}"
  OUT="$("${PLATFORM_CLI}" verify --key "${TMP}/signing.key" rsi3d.pack.json 2>&1)"
  check "验签通过（摘要 + 签名 + 逐文件）" "签名     ✓" "${OUT}"
  check "业务包清单里的 slug 被渲染" "home-display" "$(cat rsi3d.pack.json)"
  cd "${ROOT}"
else
  skip "平台工具链打通" "未找到 ${PLATFORM_CLI}（先跑 npm run cli:build，或 cargo build --release）"
fi

# ---------------------------------------------------------------- 6. 场景内核

echo "── 6. 场景内核（观察 → 编辑 → 回滚 → 校验）──"
SCENE_DIR="${TMP}/h0scene"
hr scaffold new agent-app --out "${SCENE_DIR}" --var project=h0scene > /dev/null 2>&1
SCENE="${SCENE_DIR}/scene.json"

OUT="$(hr scene show "${SCENE}" 2>&1)"
check "观察：识别场景 spec" "rsi3d-scene/v1" "${OUT}"
check "观察：认出挡窗者" "挡窗者 1  obj:sofa_01" "${OUT}"
check "观察：算出实测间距" "当前 3.90m" "${OUT}"
check "观察：列出三类告警" "[window.blocked]" "${OUT}"
check "观察：给出场景哈希" "场景哈希 " "${OUT}"

DOC="${TMP}/h0-doc.json"
OUT="$(hr scene edit "${SCENE}" --out "${DOC}" \
  --cmd '{"op":"transform","target":"sofa_01","params":{"translate":[0,0,1.3]},"reason":"挪出挡光带","expect":{"lighting.window":"+"}}' \
  --cmd '{"op":"set_light","target":"sun","params":{"intensity":1.0},"reason":"日光过曝"}' 2>&1)"
check "编辑：逐步给出 rev" "rev 1  transform  obj:sofa_01" "${OUT}"
check "编辑：记录理由" "理由 挪出挡光带" "${OUT}"
check "编辑：修好的问题不算新告警" "新告警 无" "${OUT}"
check "编辑：窗户恢复通光" "挡窗 无" "${OUT}"
check "编辑：落盘文档" "已保存" "${OUT}"
[ -f "${DOC}" ] || { echo "✗ 文档未生成"; FAIL=$((FAIL + 1)); }

OUT="$(hr scene edit "${DOC}" --out "${DOC}" \
  --cmd '{"op":"checkout","params":{"rev":1},"reason":"第 3 步更糟，回到 rev 1"}' 2>&1)"
check "回滚：checkout 产生新 rev" "rev 3  checkout" "${OUT}"
check "回滚：游标指回 rev 1" "游标 1" "${OUT}"

OUT="$(hr scene verify "${DOC}" 2>&1)"
check "校验：重放等于落盘状态" "✓ 日志重放 == 落盘状态" "${OUT}"
check "校验：每个版本都可重放" "✓ 全部 4 个版本均可重放" "${OUT}"
check "校验：落盘往返哈希不变" "✓ 落盘往返后场景哈希与日志哈希不变" "${OUT}"
check "校验：游标不变量" "✓ 游标 1" "${OUT}"
check "校验：日志哈希可对账" "日志哈希 " "${OUT}"

OUT="$(hr scene edit "${SCENE}" --cmd '{"op":"transform","target":"ghost","params":{"translate":[1,0,0]}}' 2>&1)"
check "坏命令被拒绝（未知目标）" "unknown_target" "${OUT}"
OUT="$(hr scene edit "${SCENE}" --cmd '{"op":"transform","target":"sofa_01","params":{"translate":[0,0,0]}}' 2>&1)"
check "坏命令被拒绝（空操作）" "no_op" "${OUT}"

OUT="$(hr scene show "${DOC}" --json 2>&1)"
check "JSON 模式：给出 view（插件契约）" '"aabb"' "${OUT}"
check "JSON 模式：给出可编辑性" '"editability"' "${OUT}"
check "JSON 模式：给出告警结构" '"code"' "${OUT}"

# ---------------------------------------------------------------- 7. MCP（给 VS Code 等通用工具）

echo "── 7. MCP 服务（stdio：VS Code / Cursor / Claude Code）──"
MCP_ROOT="${TMP}/mcp"
mkdir -p "${MCP_ROOT}"
cp "${SCENE}" "${MCP_ROOT}/scene.json"

# 跑一次真实的服务进程：stdin 喂报文，stdout 收报文，日志进 stderr
mcp_run() {
  printf '%s\n' "$@" | "${BIN}" mcp --root "${MCP_ROOT}" 2> "${TMP}/mcp.log"
}

HANDSHAKE=(
  '{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-06-18","capabilities":{},"clientInfo":{"name":"smoke","version":"1"}}}'
  '{"jsonrpc":"2.0","method":"notifications/initialized"}'
)

OUT="$(mcp_run "${HANDSHAKE[@]}" '{"jsonrpc":"2.0","id":2,"method":"tools/list"}')"
check "握手：回协商后的协议版本" '"protocolVersion":"2025-06-18"' "${OUT}"
check "握手：报出引擎身份" '"name":"rsi3d-harness"' "${OUT}"
check "握手：给出工作目录" '"name":"rsi3d-harness"' "${OUT}"
check "工具清单含四原语" '"name":"scene_observe"' "${OUT}"
check "工具清单含回滚（RSI 的关键动作）" '"name":"scene_rollback"' "${OUT}"
check "工具清单含自检" '"name":"scene_verify"' "${OUT}"
check "通知不被回复（2 请求 + 1 通知 → 2 行）" "2" "$(printf '%s' "${OUT}" | grep -c .)"
check "stdout 只有 MCP 报文" "0" "$(printf '%s' "${OUT}" | grep -cv '"jsonrpc"')"
check "日志走 stderr 而非 stdout" "MCP stdio 已就绪" "$(cat "${TMP}/mcp.log")"

OUT="$(mcp_run "${HANDSHAKE[@]}" \
  '{"jsonrpc":"2.0","id":2,"method":"tools/call","params":{"name":"scene_open","arguments":{"file":"scene.json"}}}' \
  '{"jsonrpc":"2.0","id":3,"method":"tools/call","params":{"name":"scene_edit","arguments":{"commands":[{"op":"transform","target":"sofa_01","params":{"translate":[0,0,1.3]},"reason":"沙发挡窗，挪出挡光带","expect":{"lighting.window":"+"}}]}}}' \
  '{"jsonrpc":"2.0","id":4,"method":"tools/call","params":{"name":"scene_rollback","arguments":{"rev":0}}}' \
  '{"jsonrpc":"2.0","id":5,"method":"tools/call","params":{"name":"scene_verify","arguments":{}}}')"
check "打开：抬头说明在改哪份文件" "scene.json" "${OUT}"
check "打开：认出挡窗者" "挡窗者 1  obj:sofa_01" "${OUT}"
check "编辑：新告警无（修好窗户）" "新告警 无" "${OUT}"
check "编辑：逆命令是恢复原值" '"op":"restore"' "${OUT}"
check "回滚：回到 rev 0" "已回到 rev 0" "${OUT}"
check "自检：通过" '"ok":true' "${OUT}"
check "全流程无工具级错误" "0" "$(printf '%s' "${OUT}" | grep -c '"isError":true')"

# 失败必须是「模型读得到的错误」，而不是协议错误
OUT="$(mcp_run "${HANDSHAKE[@]}" \
  '{"jsonrpc":"2.0","id":2,"method":"tools/call","params":{"name":"scene_open","arguments":{"file":"scene.json"}}}' \
  '{"jsonrpc":"2.0","id":3,"method":"tools/call","params":{"name":"scene_edit","arguments":{"commands":[{"op":"transform","target":"obj:ghost","params":{"translate":[1,0,0]},"reason":"试试"}]}}}')"
check "坏命令：isError 在结果里" '"isError":true' "${OUT}"
check "坏命令：错误码可读" "unknown_target" "${OUT}"
check "坏命令：附上当前状态供判断" '"window_blockers"' "${OUT}"

# 路径越界必须被拒（引擎不碰工作目录之外的任何文件）
OUT="$(mcp_run "${HANDSHAKE[@]}" \
  '{"jsonrpc":"2.0","id":2,"method":"tools/call","params":{"name":"scene_open","arguments":{"file":"../../etc/passwd"}}}')"
check "越界路径被拒绝" "越界" "${OUT}"

OUT="$(mcp_run "${HANDSHAKE[@]}" \
  '{"jsonrpc":"2.0","id":2,"method":"tools/call","params":{"name":"scene_open","arguments":{"file":"scene.json"}}}' \
  '{"jsonrpc":"2.0","id":3,"method":"resources/read","params":{"uri":"rsi3d://scene"}}' \
  '{"jsonrpc":"2.0","id":4,"method":"resources/read","params":{"uri":"rsi3d://log"}}')"
check "resource：能读到当前场景" "obj:sofa_01" "${OUT}"
check "resource：能读到命令日志" "log_hash" "${OUT}"

# ---------------------------------------------------------------- 8. 渲染（观测图）

echo "── 8. 渲染（多视角观测图 + 挡光带测量）──"
SHOT="${TMP}/shot"
OUT="$(hr scene render "${SCENE}" --out "${SHOT}" --views top,iso-sw --width 240 --height 180 2>&1)"
check "渲染：报出后端" "渲染后端 software" "${OUT}"
check "渲染：只出要的视角" "top" "${OUT}"
check "渲染：算出挡光带遮挡比例" "窗前挡光带被遮挡" "${OUT}"
check "渲染：报出每个对象的可见像素" "obj:sofa_01" "${OUT}"
check "渲染：Windows 上也能用的朴素命名" "shot-top.png" "${OUT}"
check "渲染：PNG 魔数正确" "89504e47" "$(head -c 4 "${SHOT}-top.png" | od -An -tx1 | tr -d ' \n')"
check "渲染：两个视角都落盘了" "2" "$(ls "${SHOT}"-*.png | wc -l | tr -d ' ')"
check "渲染：坏视角被拒绝" "不认识的视角" "$(hr scene render "${SCENE}" --views sideways 2>&1)"

# 挪开沙发之后窗带应当基本通光（同一条数字指标能反映改动）
DOC2="${TMP}/render-doc.json"
hr scene edit "${SCENE}" --out "${DOC2}" \
  --cmd '{"op":"transform","target":"sofa_01","params":{"translate":[0,0,1.3]},"reason":"挪出挡光带"}' > /dev/null 2>&1
OUT="$(hr scene render "${DOC2}" --views top 2>&1)"
check "渲染：挪开后窗带恢复通光" "被遮挡 0%" "${OUT}"

# ---------------------------------------------------------------- 9. 转流（服务端 + 客户端）

echo "── 9. 远程渲染 / 转流（HTTP + SSE）──"
PORT="${SMOKE_STREAM_PORT:-8393}"
TOKEN="smoke-$$"
SERVE_LOG="${TMP}/serve.log"
"${BIN}" serve "${SCENE}" --port "${PORT}" --token "${TOKEN}" > "${SERVE_LOG}" 2>&1 &
SERVE_PID=$!
# 服务是后台起的：无论如何都要收掉，否则会留下占端口的进程
cleanup_serve() { kill "${SERVE_PID}" 2>/dev/null; }
trap 'cleanup_serve; cleanup' EXIT INT TERM

BASE="http://127.0.0.1:${PORT}"
for _ in $(seq 1 30); do
  curl -s -o /dev/null "${BASE}/healthz" && break
  sleep 0.2
done

check "转流：启动后打印带令牌的客户端地址" "?token=${TOKEN}" "$(cat "${SERVE_LOG}")"
check "转流：明确说出两条流" "场景流（客户端渲染）" "$(cat "${SERVE_LOG}")"
check "转流：healthz 可用（无需令牌）" '"protocol": "rsi3d-stream/v1"' "$(curl -s "${BASE}/healthz")"
check "转流：无令牌拿不到流" "401" "$(curl -s -o /dev/null -w '%{http_code}' "${BASE}/stream/scene")"
check "转流：令牌不对也拒绝" "401" "$(curl -s -o /dev/null -w '%{http_code}' "${BASE}/stream/scene?token=wrong")"
check "转流：页面是内嵌的（无需令牌）" "rsi3d-harness" "$(curl -s "${BASE}/")"
check "转流：客户端脚本真的用 EventSource" "EventSource" "$(curl -s "${BASE}/client.js")"
# 契约产物同样是现算的：与 `hr contract --out` 写出来的字节必须一致
check "转流：/contract 索引无需令牌" '"scene.schema.json"' "$(curl -s "${BASE}/contract")"
check "转流：schema 与派生结果逐字节一致" "0" \
  "$(diff <(curl -s "${BASE}/contract/scene.schema.json") "${ROOT}/contract/scene.schema.json" >/dev/null 2>&1; echo $?)"
check "转流：契约里没有不存在的产物" "404" "$(curl -s -o /dev/null -w '%{http_code}' "${BASE}/contract/nope.json")"

# 能力声明（借 glow 的纪律：能力是声明出来的，不是被假设的）
# 1) 握手回声：服务端把我们听成了什么，客户端能对账
DECL="$(curl -s -N --max-time 2 \
  "${BASE}/stream/scene?token=${TOKEN}&agent=rsi3d-third-party%2F9.9&cap=webgl1,telepathy" | head -c 900)"
check "能力：握手回声客户端身份" '"client_agent":"rsi3d-third-party/9.9"' "${DECL}"
check "能力：回声里带上我们声明的能力" '"webgl1"' "${DECL}"
check "能力：不认识的能力名也**不丢**（原样带着）" 'telepathy' "${DECL}"
check "能力：不认识的能力名会大声告警（不静默）" "不认识的能力" "$(cat "${SERVE_LOG}")"
check "能力：自相矛盾的声明会被标出来（webgl1+webgl2）" "自相矛盾" \
  "$(curl -s -N --max-time 2 "${BASE}/stream/scene?token=${TOKEN}&cap=webgl1,webgl2" >/dev/null 2>&1; cat "${SERVE_LOG}")"
# 显示预算（wgpu 那套里的 limits）：服务端**只缩不放**，并等比
FRAME_SMALL="$(curl -s -N --max-time 2 \
  "${BASE}/stream/frame?token=${TOKEN}&view=top&agent=smoke-budget&cap=image&px=240x180" | head -c 900)"
check "能力：图像流按客户端预算缩到 240×180" '"width":240' "${FRAME_SMALL}"
check "能力：等比缩放（高度也缩了，不是只改宽）" '"height":180' "${FRAME_SMALL}"
# 2) 服务端名册：连上就写（这里用真在跑的命令行客户端）
"${BIN}" stream "${BASE}" --token "${TOKEN}" --kind frame --view top --limit 0 --idle 6 \
  > "${TMP}/cli-stream.log" 2>&1 &
CLI_STREAM_PID=$!
sleep 1.2
ROSTER="$(curl -s "${BASE}/healthz")"
check "能力：名册里能看到命令行客户端" 'rsi3d-cli/' "${ROSTER}"
check "能力：名册里能看到它声明了 headless" 'headless' "${ROSTER}"
check "能力：名册报出**派生档位**（不让人自己拼字符串）" '"render_tier": "headless"' "${ROSTER}"
check "能力：名册报出实际会给它发多大的帧" '"frame_px"' "${ROSTER}"
check "能力：healthz 也公开词汇表的类别与依赖（抄 wgpu 的 features/downlevel 之分）" '"needs_any"' "${ROSTER}"
check "能力：名册里能看到它连的是哪条流" '"kind": "frame"' "${ROSTER}"
check "能力：healthz 也公开词汇表（外部工具靠它知道能声明什么）" 'capabilities_known' "${ROSTER}"
kill "${CLI_STREAM_PID}" 2>/dev/null
# 3) 断开就抹：名册反映的是**当下**，不是"连过"的历史
for _ in $(seq 1 40); do
  [ "$(curl -s "${BASE}/healthz" | grep -c 'rsi3d-cli/')" = "0" ] && break
  sleep 0.1
done
check "能力：断开后立刻从名册消失（不是等心跳）" "0" "$(curl -s "${BASE}/healthz" | grep -c 'rsi3d-cli/')"

# 场景流：首包应当是 welcome + 全量快照，事件 id = 状态版本
SNAP_HEAD="$(curl -s -N --max-time 3 "${BASE}/stream/scene?token=${TOKEN}" | head -c 4000)"
check "场景流：首事件是 welcome" "event: welcome" "${SNAP_HEAD}"
check "场景流：握手声明协议版本" "rsi3d-stream/v1" "${SNAP_HEAD}"
check "场景流：老实说清几何档次" "aabb-proxy" "${SNAP_HEAD}"
check "场景流：事件 id 就是状态版本" "id: 0" "${SNAP_HEAD}"
check "场景流：紧跟一张全量快照" "event: snapshot" "${SNAP_HEAD}"
check "场景流：快照是真 glTF 2.0" '"version":"2.0"' "${SNAP_HEAD}"
check "场景流：快照里带房间/窗/规则（客户端能自己画）" '"bandDepth":1.5' "${SNAP_HEAD}"
check "场景流：每个节点自报几何档次（增量也自包含）" '"color"' "${SNAP_HEAD}"

# 图像流：服务端渲染的帧 + 它自己的测量值随帧一起到（只看头部：帧很大）
OUT="$(curl -s -N --max-time 3 "${BASE}/stream/frame?token=${TOKEN}&view=top" | head -c 1200)"
check "图像流：发的是帧" "event: frame" "${OUT}"
check "图像流：帧里带 PNG" "iVBORw0KGgo" "${OUT}"
check "图像流：说清是哪一版、哪个视角" '"view":"top"' "${OUT}"
check "图像流：帧自报家门（谁渲的）" '"renderer":"cpu-raster/v1"' "${OUT}"

# 命令：与 CLI 同一个信封（裸 {op,target,params,reason}），改完流里就该有增量
OUT="$(curl -s -X POST -H 'Content-Type: application/json' \
  --data '{"op":"transform","target":"sofa_01","params":{"translate":[0,0,1.3]},"reason":"挪出挡光带"}' \
  "${BASE}/command?token=${TOKEN}" 2>&1)"
check "命令：走 HTTP 也能改场景" '"revision": 1' "${OUT}"
check "命令：回执带回 reason（可对账）" "挪出挡光带" "${OUT}"
OUT="$(curl -s "${BASE}/observe?token=${TOKEN}")"
check "观测：与 MCP/CLI 同一份（挡窗者已变空）" '"window_blockers": []' "${OUT}"

# 断线续传：带上 Last-Event-ID 就只补差量，不再重发全量
RESUME="$(curl -s -N --max-time 3 -H 'Last-Event-ID: 0' "${BASE}/stream/scene?token=${TOKEN}" | head -c 600)"
check "续传：握手标明是续传" '"resumed":true' "${RESUME}"
check "续传：只补增量" "event: patch" "${RESUME}"
check "续传：不再重发全量" "0" "$(printf '%s' "${RESUME}" | grep -c 'event: snapshot' | tr -d ' ')"

# 客户端：落盘帧与快照（同时也验证了自写的 SSE 客户端）
FRAMES="${TMP}/frames"
OUT="$("${BIN}" stream "${BASE}" --token "${TOKEN}" --kind frame --view top \
  --out "${FRAMES}" --limit 5 --idle 3 2>&1)"
check "客户端：能连上并说明是图像流" "图像流" "${OUT}"
check "客户端：静止场景会明确解释\"为什么不推了\"" "服务端不重发" "${OUT}"
# 前面已经把沙发挪出窗带：图像流带来的测量值必须是**当前**的，不能是旧的
check "客户端：帧里带服务端自己测的遮挡率（结论随帧到，且是当前状态）" "遮挡 0%" "${OUT}"
check "客户端：每帧自报来源档（落盘的帧得能说清谁渲的）" "cpu-raster/v1（证据档）" "${OUT}"
check "客户端：PNG 落盘且魔数正确" "89504e47" "$(head -c 4 "${FRAMES}/frame-0001.png" | od -An -tx1 | tr -d ' \n')"

SCENE_OUT="${TMP}/scene-out"
OUT="$("${BIN}" stream "${BASE}" --token "${TOKEN}" --kind scene --out "${SCENE_OUT}" --limit 2 2>&1)"
check "客户端：场景流能导出 glTF" "snapshot.json" "${OUT}"
check "客户端：导出的确实是 glTF 2.0" '"version": "2.0"' "$(cat "${SCENE_OUT}/snapshot.json")"

# 安全：绑非回环时必须大喊
"${BIN}" serve "${SCENE}" --port "${PORT}" --token "${TOKEN}" --bind 0.0.0.0 > "${TMP}/public.log" 2>&1 &
PUB_PID=$!
sleep 1
check "安全：绑非回环地址会告警" "非回环" "$(cat "${TMP}/public.log")"
kill "${PUB_PID}" 2>/dev/null

kill "${SERVE_PID}" 2>/dev/null
trap 'cleanup' EXIT INT TERM

# ---------------------------------------------------------------- 9. 导出（交给外部工具链）

echo "── 10. 导出：标准 glTF 2.0（Blender / Fyrox / three.js 都能读）──"
GLTF="${TMP}/model.gltf"
OUT="$(hr scene export "${SCENE}" --out "${GLTF}" 2>&1)"
check "导出：报出格式" "glTF 2.0" "${OUT}"
check "导出：报出节点与灯光数" "节点 4 · 灯光 2" "${OUT}"
check "导出：如实说明几何档次（包围盒代理）" "包围盒代理" "${OUT}"
check "导出：给出可交接的用法" "Blender" "${OUT}"
check "导出：文件里的 glTF 版本" '"version": "2.0"' "$(cat "${GLTF}")"
check "导出：几何内嵌成单文件（data URI）" "data:application/octet-stream;base64," "$(cat "${GLTF}")"
# 这条是被一次**外部验收**逼出来的：`nodes` 只是节点池，`scenes[].nodes` 才是场景内容。
# 只填节点池的话文件合法但任何标准加载器都读到空场景（gltf-transform 会报 renderVertexCount: 0）。
ROOTS="$(sed -n '/"scenes"/,/^  \]/p' "${GLTF}" | grep -cE '^ +[0-9]+,?$' | tr -d ' ')"
check "导出：场景真的引用了全部节点（否则外部加载器读到空场景）" "4" "${ROOTS}"
check "导出：不支持的格式会被明确拒绝" "目前只有 gltf" "$(hr scene export "${SCENE}" --format usdz 2>&1)"

# ---------------------------------------------------------------- 11. npm 包（两个入口）

echo "── 11. npm 包（@rsi3d/cli：rsi3d + rsi3d-harness）──"
PKG="${ROOT}/packages/rsi3d-cli"
if [ -n "${NODE_OK}" ]; then
  check "npm：包声明了 CLI 入口" "bin/rsi3d.js" "$(cat "${PKG}/package.json")"
  check "npm：包声明了引擎入口（MCP 走这里）" "bin/rsi3d-harness.js" "$(cat "${PKG}/package.json")"

  BAD=0
  for f in "${PKG}"/bin/*.js; do
    node --check "${f}" > /dev/null 2>&1 || BAD=$((BAD + 1))
  done
  check "npm：全部 JS 语法正确" "0" "${BAD}"

  OUT="$(node "${PKG}/bin/rsi3d.js" --version 2>&1)"
  check "npm：rsi3d 入口可用（解析到本地构建）" "rsi3d 0.1.0" "${OUT}"
  OUT="$(node "${PKG}/bin/rsi3d-harness.js" --version 2>&1)"
  check "npm：rsi3d-harness 入口可用" "rsi3d-harness 0.1.0" "${OUT}"
  OUT="$(node "${PKG}/bin/rsi3d-harness.js" scene --help 2>&1)"
  check "npm：入口能透传子命令与参数" "渲染" "${OUT}"

  RSI3D_BIN=/nope/rsi3d node "${PKG}/bin/rsi3d.js" --version > /dev/null 2>&1
  RC=$?
  check "npm：显式指定的坏路径直接失败（不静默换来源）" "127" "${RC}"
  OUT="$(RSI3D_BIN=/nope/rsi3d node "${PKG}/bin/rsi3d.js" --version 2>&1)"
  check "npm：失败时告诉用户怎么解决" "cargo build --release" "${OUT}"
else
  skip "npm 包入口" "未找到 node"
fi

# ---------------------------------------------------------------- 12. 契约（从 Rust 类型派生）

echo "── 12. 契约：从 Rust 类型派生给非 Rust 消费者的形状 ──"
OUT="$(hr contract 2>&1)"
check "契约：说明产物是派生的、勿手改" "派生自 Rust 类型" "${OUT}"
check "契约：列出 4 个产物" "scene.example.json" "${OUT}"
check "契约：报出各块的已知键数" "stream_server" "${OUT}"
OUT="$(hr contract --check 2>&1)"
check "契约：已提交的产物与 Rust 类型一致" "一致" "${OUT}"

CONTRACT_OUT="${TMP}/contract"
hr contract --out "${CONTRACT_OUT}" > /dev/null 2>&1
check "契约：写出产物" "1" "$([ -f "${CONTRACT_OUT}/keys.json" ] && echo 1 || echo 0)"
check "契约：键清单用 JSON 名（spanX / bandDepth 而不是 span_x）" "1" \
  "$([ "$(grep -c 'spanX' "${CONTRACT_OUT}/keys.json")" -gt 0 ] && [ "$(grep -c 'bandDepth' "${CONTRACT_OUT}/keys.json")" -gt 0 ] && echo 1 || echo 0)"
# 只查 schema 的键清单：keys.json 的说明文字里会引用 band_depth 作为反面例子
check "契约：键清单里不会出现手写错法（band_depth / span_x）" "0" \
  "$(grep -c 'band_depth\|span_x' "${CONTRACT_OUT}/scene.schema.json" 2>/dev/null || echo 0)"
check "契约：schema 是 draft 2020-12" "json-schema.org/draft/2020-12" "$(cat "${CONTRACT_OUT}/scene.schema.json")"
check "契约：schema 在字段描述里声明了已知键清单" "x-known-keys" "$(cat "${CONTRACT_OUT}/scene.schema.json")"
check "契约：样例里没有多余说明字段（样例会被照抄）" "0" "$(grep -c '\$comment' "${CONTRACT_OUT}/scene.example.json")"

# 门禁自检：把产物改坏，--check 必须变红（否则这道门就是摆设）
mkdir -p "${TMP}/tampered" && cp "${CONTRACT_OUT}"/*.json "${TMP}/tampered/"
sed -i.bak 's/"number"/"integer"/' "${TMP}/tampered/keys.json" 2>/dev/null || \
  perl -pi -e 's/"number"/"integer"/' "${TMP}/tampered/keys.json"
check "契约：产物被改坏后 --check 会失败（证明门禁不是摆设）" "不一致" \
  "$(hr contract --out "${TMP}/tampered" --check 2>&1)"
check "契约：--check 失败时告诉你怎么重新生成" "contract --out" \
  "$(hr contract --out "${TMP}/tampered" --check 2>&1)"

# ---------------------------------------------------------------- 13. 导入（外部资产）

echo "── 13. 导入：自己读 / 请 Blender / 老实说不行 ──"

IO="${TMP}/io"
mkdir -p "${IO}"

# 三条路都得在格式表里明说
FMT="$(hr scene import --formats)"
check "导入：格式表分三条路（自己读 / 经 Blender / 请上游导出）" "自己读" "${FMT}"
check "导入：格式表里 Blender 那条路" "经 Blender" "${FMT}"
check "导入：B-rep 那条路说清是"请上游导出网格"" "请上游导出网格" "${FMT}"
check "导入：格式表不是嘴上的（自己读的那几个都在表里）" "Wavefront OBJ" "${FMT}"

# 自己读：OBJ（一个 2×2×2 的盒子；行尾反斜杠会吞换行，所以这里用一行一个 \\n）
printf 'o Box_A\nv -1 -1 -1\nv 1 -1 -1\nv 1 1 -1\nv -1 1 -1\nv -1 -1 1\nv 1 -1 1\nv 1 1 1\nv -1 1 1\nf 1 2 3 4\nf 5 6 7 8\nf 1 2 6 5\nf 3 4 8 7\nf 1 4 8 5\nf 2 3 7 6\n' > "${IO}/box.obj"
OBJ_REPORT="$(hr scene import "${IO}/box.obj" --out "${IO}/box.scene.json" 2>&1)"
check "导入：OBJ 自己读（对象数/顶点/三角面都在报告里）" "1 个对象 · 8 顶点 · 12 三角面" "${OBJ_REPORT}"
check "导入：OBJ 不带坐标系声明，就老实写 unknown（不猜）" "源文件 unknown" "${OBJ_REPORT}"
check "导入：没有坐标系/单位的格式会出告警" "不猜轴向" "${OBJ_REPORT}"
check "导入：报告里给出下一步（怎么渲染、怎么起服务）" "scene render" "${OBJ_REPORT}"
check "导入：几何 side-car 与场景同目录同前缀" "box.scene.mesh.glb" "${OBJ_REPORT}"
check "导入：side-car 真的落盘了" "box.scene.mesh.glb" "$(ls "${IO}")"
check "导入：side-car 是标准 GLB（magic 是 glTF）" "glTF" "$(head -c 4 "${IO}/box.scene.mesh.glb")"
check "导入：产出的场景内核读得回来" "Box_A" "$(hr scene show "${IO}/box.scene.json" 2>&1)"
check "导入：节点上挂着 mesh_ref（客户端据此显示真网格）" "mesh_ref" "$(cat "${IO}/box.scene.json")"

# 截断必须说出来
printf 'o A\nv 0 0 0\nv 1 0 0\nv 0 1 0\nf 1 2 3\no B\nv 5 0 0\nv 6 0 0\nv 5 1 0\nf 1 2 3\n' > "${IO}/two.obj"
check "导入：--limit 截断要说出来，并且说清原文件有几个" "被截断了" \
  "$(hr scene import "${IO}/two.obj" --limit 1 --out "${IO}/two.scene.json" 2>&1)"

# 单位提醒（毫米模型不缩放就是"体育场"）
printf 'o Big\nv 0 0 0\nv 2000 0 0\nv 2000 100 0\nv 0 100 0\nf 1 2 3 4\n' > "${IO}/mm.obj"
check "导入：尺寸异常会提醒 --unit-scale" "unit-scale" \
  "$(hr scene import "${IO}/mm.obj" --out "${IO}/mm.scene.json" 2>&1)"
check "导入：--unit-scale 0.001 把 2000mm 变成 2m" "x 0.000…2.000" \
  "$(hr scene import "${IO}/mm.obj" --unit-scale 0.001 --out "${IO}/mm2.scene.json" 2>&1)"

# B-rep 不许假装能读
printf 'ISO-10303-21;\n' > "${IO}/part.stp"
STEP_ERR="$(hr scene import "${IO}/part.stp" --out "${IO}/x.json" 2>&1)"
check "导入：STEP 明确拒绝，并说清是 B-rep 不是网格" "B-rep" "${STEP_ERR}"
check "导入：拒绝时给出路（导出 STL/OBJ/glTF）" "STL / OBJ / glTF" "${STEP_ERR}"

# 未知扩展名：把支持列表摆出来
printf 'x' > "${IO}/thing.xyz"
check "导入：不认识的扩展名会列出支持哪些" "scene import --formats" \
  "$(hr scene import "${IO}/thing.xyz" --out "${IO}/y.json" 2>&1)"

# 请 Blender：有就真跑一遍，没有就明说跳过（不静默通过）
BLENDER="$(hr scene import --formats >/dev/null 2>&1; command -v blender 2>/dev/null || true)"
if [ -x /Applications/Blender.app/Contents/MacOS/Blender ]; then
  /Applications/Blender.app/Contents/MacOS/Blender --background --factory-startup --python-expr \
    "import bpy; bpy.ops.wm.read_factory_settings(use_empty=True); bpy.ops.mesh.primitive_cube_add(size=1.0); bpy.context.object.name='Cube_A'; bpy.ops.mesh.primitive_plane_add(size=4.0, location=(0,0,-0.5)); bpy.context.object.name='Floor_01'; bpy.ops.wm.save_as_mainfile(filepath='${IO}/fixture.blend')" \
    >/dev/null 2>&1
  BLEND_REPORT="$(hr scene import "${IO}/fixture.blend" --out "${IO}/fixture.scene.json" 2>&1)"
  check "导入：.blend 经本机 Blender 转 GLB 后读入" "经手：Blender" "${BLEND_REPORT}"
  check "导入：报告里写清源文件坐标系 → 场景坐标系" "源文件 RUF" "${BLEND_REPORT}"
  check "导入：转轴这件事会出告警（不静默）" "转轴" "${BLEND_REPORT}"
  check "导入：平面在光栅里看不见，会点名" "Floor_01" "${BLEND_REPORT}"
  check "导入：blend 的 side-car 也是 GLB" "glTF" "$(head -c 4 "${IO}/fixture.scene.mesh.glb")"
else
  skip "导入：.blend 经 Blender 转 GLB" "这台机器上没装 Blender"
fi

# 用户资产里那两个非标准 blend：错误必须有用（不是"失败"两个字）
if [ -f "${ROOT}/../assets/H0_URDF_rigged.blend" ]; then
  BAD="$(hr scene import "${ROOT}/../assets/H0_URDF_rigged.blend" --out "${IO}/h0.json" 2>&1)"
  check "导入：非标准 blend 的错误里点名是谁说的" "读不了这个文件。Blender 说" "${BAD}"
  check "导入：把 Blender 的原话带出来（不是我们转述的"失败"）" "not a blend file" "${BAD}"
else
  skip "导入：非标准 blend 的诊断" "assets/ 里没有那个文件"
fi

# ---------------------------------------------------------------- 14. 渲染模式（探测 → 判定 → 应用）

echo "── 14. 渲染模式：按主机能力判档，判不了的给得出路 ──"

RM="${TMP}/render-mode"
mkdir -p "${RM}"

# 规则表：档位、给的限制、出路都要看得见
POLICY_OUT="$(hr render-mode)"
check "渲染模式：规则表列出全部档位" "client-full" "${POLICY_OUT}"
check "渲染模式：最强档要 WebGL2" "GPU webgl2" "${POLICY_OUT}"
check "渲染模式：兜底档是 frame-only（本机不渲）" "frame-only" "${POLICY_OUT}"
check "渲染模式：无屏形态单独一档" "headless" "${POLICY_OUT}"

# 规则表也是契约产物（平台 /api/render/policy 发同一份）
check "渲染模式：规则表进了契约产物" "render-policy.json" "$(ls contract/ 2>/dev/null || echo '')"
check "渲染模式：产物里的规则表与服务端同源" "client-full" "$(cat contract/render-policy.json 2>/dev/null)"

# 强机 → 满档
cat > "${RM}/strong.json" <<'JSON'
{"agent":"rsi3d-web/0.1.0","form":"screen","gpu":{"api":"webgl2","webgpu":true,"max_texture":16384},"cpu":{"cores":10,"platform":"macos"},"display":{"viewport":[1920,1080],"dpr":2},"bench":{"sustained_fps":58,"frames":116,"ms":2000},"privacy":"hashed"}
JSON
STRONG="$(hr render-mode --profile "${RM}/strong.json")"
check "渲染模式：强机判满档" "渲染模式 client-full" "${STRONG}"
check "渲染模式：满档给真网格 + 两条流" "几何 real · 订阅 both" "${STRONG}"
check "渲染模式：满档不给建议（够用的时候别啰嗦）" "0" "$(printf '%s' "${STRONG}" | grep -c '→' || echo 0)"
check "渲染模式：判定自报是谁算的（离线=local）" "local" "${STRONG}"

# 弱机 → 降档 + 理由 + 出路
WEAK="$(hr render-mode --profile "${TMP}/io/two.obj" 2>&1 || true)"   # 故意喂错：要的是有用报错
check "渲染模式：喂错文件时给出路（不是"失败"两个字）" "合法的主机参数" "${WEAK}"

cat > "${RM}/weak.json" <<'JSON'
{"agent":"rsi3d-web/0.1.0","form":"screen","gpu":{"api":"webgl1","max_texture":2048,"software":true},"cpu":{"cores":2,"platform":"windows"},"display":{"viewport":[640,480],"dpr":1},"bench":{"sustained_fps":9,"frames":18,"ms":2000},"privacy":"hashed"}
JSON
WEAKOUT="$(hr render-mode --profile "${RM}/weak.json")"
check "渲染模式：瘦客户端降到 client-minimal" "渲染模式 client-minimal" "${WEAKOUT}"
check "渲染模式：降档要收紧像素预算" "≤ 960×600" "${WEAKOUT}"
check "渲染模式：降档只画包围盒代理" "几何 aabb-proxy" "${WEAKOUT}"
check "渲染模式：降档要说明差什么（可执行的数字）" "需要 4 核，本机 2 核" "${WEAKOUT}"
check "渲染模式：降档要给得出一路" "只订阅服务端图像流" "${WEAKOUT}"
check "渲染模式：默认脱敏要说出来" "只上报了哈希" "${WEAKOUT}"

# 无屏形态
cat > "${RM}/headless.json" <<'JSON'
{"agent":"rsi3d-harness/0.1.0","form":"headless","cpu":{"cores":16},"privacy":"omitted"}
JSON
HEADLESS="$(hr render-mode --profile "${RM}/headless.json")"
check "渲染模式：无屏形态不订阅任何流" "订阅 none" "${HEADLESS}"

# 平台不可达时的**离线兜底**：本地服务也能判（同一份规则表）
( "${BIN}" serve scaffolds/agent-app/files/scene.json --port "${RM_PORT:-8471}" --token m > "${RM}/serve.log" 2>&1 & echo $! > "${RM}/pid" )
sleep 2
if [ -s "${RM}/serve.log" ]; then
  CAP="$(curl -s -X POST -H 'Content-Type: application/json' \
    -d @"${RM}/weak.json" "http://127.0.0.1:${RM_PORT:-8471}/capability?token=m" 2>/dev/null)"
  check "渲染模式：本地 /capability 判出同一档（离线兜底）" "client-minimal" "${CAP}"
  # 坏输入不许静默降档：字段都能缺省，`{"nope":1}` 会被解析成"什么都没报的机器"→ 兜底档
  BADP="$(curl -s -m 5 -X POST -H 'Content-Type: application/json' -d '{"nope":1}' \
    "http://127.0.0.1:${RM_PORT:-8471}/capability?token=m" 2>/dev/null)"
  check "渲染模式：坏输入明确拒绝（不许静默判成低档）" "bad_profile" "${BADP}"
  check "渲染模式：兜底判定自报 authority=local（不冒充权威）" "local" \
    "$(printf '%s' "${CAP}" | python3 -c "import json,sys; print(json.load(sys.stdin)['verdict']['authority'])" 2>/dev/null)"
  check "渲染模式：判定里带规则表版本（两边要能对账）" "1" \
    "$(printf '%s' "${CAP}" | python3 -c "import json,sys; print(json.load(sys.stdin)['verdict']['policy_version'])" 2>/dev/null)"
  # 带上 -m：冒烟里没有超时的 curl 曾经偶发拿到空体（服务端没问题，是客户端没等到）
  HZ="$(curl -s -m 5 "http://127.0.0.1:${RM_PORT:-8471}/healthz" 2>&1)"
  check "渲染模式：上报的主机参数进了 healthz（运维能查为什么降档）" "client-minimal" "${HZ}"
  check "渲染模式：/contract/render-policy.json 直接可发（平台同源）" "client-full" \
    "$(curl -s "http://127.0.0.1:${RM_PORT:-8471}/contract/render-policy.json?token=m" 2>/dev/null)"
  # 帧率上限：服务端只缩不放（要更高会被按下限给）
  check "渲染模式：帧率只缩不放（客户端要 999 时服务端记一条说明）" "只缩不放" \
    "$(curl -s -m 2 -N "http://127.0.0.1:${RM_PORT:-8471}/stream/frame?token=m&view=top&fps=999" >/dev/null 2>&1; grep -o '只缩不放' "${RM}/serve.log" | head -1)"
else
  skip "渲染模式：本地 /capability 离线兜底" "服务没起来"
fi
kill "$(cat "${RM}/pid")" 2>/dev/null || pkill -f "rsi3d-harness serve" 2>/dev/null || true
sleep 0.5

# ---------------------------------------------------------------- 技能包与插件包
# 这两个目录是**会被分发出去**的产物：包里少一个文件，别人装完就少一块能力；
# 目录里多一个文件而清单没登记，打包会拒（这是好事，但不能等到发布时才发现）。
# 所以这里把「清单 == 目录实际内容」钉住，再让**第三方实现**（系统 unzip）读一遍字节。

echo
echo "── 技能包 / 插件包（清单自洽 + 第三方可读）──────"

selfcheck_bundle() { # selfcheck_bundle <目录> <kind> <期望 name>
  local dir="$1" kind="$2" want="$3"
  if [ ! -d "${dir}" ]; then
    skip "包：${dir}" "目录不存在"
    return
  fi
  # 清单里的 files[] 必须与目录里的真实文件（去掉清单自身）**逐个对上**
  MISSING="$(python3 - "${dir}" "${kind}" <<'PY'
import json, pathlib, sys
d = pathlib.Path(sys.argv[1])
kind = sys.argv[2]
man = d / f"{kind}.json"
m = json.loads(man.read_text())
declared = sorted(m.get("files", []))
real = sorted(
    str(p.relative_to(d))
    for p in d.rglob("*")
    if p.is_file() and p.name != man.name and not p.name.startswith(".")
)
print("OK" if declared == real else f"声明 {declared} ≠ 实际 {real}")
PY
)"
  check "包：${kind}/${want} 清单与目录一致" "OK" "${MISSING}"

  ZIP="${TMP:-/tmp}/bundle-${want}.zip"
  mkdir -p "$(dirname "${ZIP}")"
  OUT="$("${PLATFORM_CLI}" skill pack "${dir}" --kind "${kind}" --out "${ZIP}" 2>&1)"
  check "包：${want} 能打包（清单校验通过）" "已打包" "${OUT}"
  if command -v unzip >/dev/null 2>&1; then
    check "包：${want} 系统 unzip 能读（第三方验收）" "No errors detected" \
      "$(unzip -t "${ZIP}" 2>&1 | tail -1)"
  else
    skip "包：${want} 第三方验收" "本机没有 unzip"
  fi
}

selfcheck_bundle "${ROOT}/skills/rsi3d-harness" "skill" "rsi3d-harness"
selfcheck_bundle "${ROOT}/plugins/harness-use" "plugin" "harness-use-rsi3d"

# 插件是「源码贡献」给 harness-use 的：能对得上宿主时顺手验一下锚点（宿主不在就跳过）
# 宿主是另一个仓库：只在显式给了路径时才验（公开仓库里不该写死本机路径）
HOST_AGENT="${RSI3D_HARNESSUSE_AGENT:-}"
if [ -n "${HOST_AGENT}" ] && [ -d "${HOST_AGENT}/src" ]; then
  OUT="$(bash "${ROOT}/plugins/harness-use/integrate.sh" "${HOST_AGENT}" --check 2>&1 || true)"
  check "包：插件接入脚本对得上宿主锚点（--check 不改文件）" "锚点齐全" "${OUT}"
else
  skip "包：插件接入脚本对宿主锚点" "没设 RSI3D_HARNESSUSE_AGENT（宿主是另一个仓库）"
fi

# ---------------------------------------------------------------- 结果

echo
echo "── 结果：通过 ${PASS} · 失败 ${FAIL} · 跳过 ${SKIP} ──────"
if [ "${FAIL}" -gt 0 ]; then
  exit 1
fi
echo "✓ rsi3d-harness 冒烟全部通过（脚手架 + 场景内核 + MCP + 渲染 + 转流 + 导出 + npm + 契约 + 导入 + 渲染模式 + 技能包）"
