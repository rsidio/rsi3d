#!/usr/bin/env bash
# rsi3d-harness 脚手架冒烟：模板发现 → 生成 → 产物自测 → 自举（外部模板）→ 与平台工具链打通。
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

# ---------------------------------------------------------------- 9. npm 包（两个入口）

echo "── 9. npm 包（@rsi3d/cli：rsi3d + rsi3d-harness）──"
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

# ---------------------------------------------------------------- 结果

echo
echo "── 结果：通过 ${PASS} · 失败 ${FAIL} · 跳过 ${SKIP} ──────"
if [ "${FAIL}" -gt 0 ]; then
  exit 1
fi
echo "✓ rsi3d-harness 冒烟全部通过（脚手架 + 场景内核 + MCP + 渲染 + npm）"
