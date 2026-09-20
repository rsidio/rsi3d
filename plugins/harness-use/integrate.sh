#!/usr/bin/env bash
# 把 RSI 3D 功能应用接进 harness-use 宿主（新增 2 个文件 + 登记 4 处）。
#
#   bash integrate.sh /path/to/harnessuse/agent            # 应用
#   bash integrate.sh /path/to/harnessuse/agent --check    # 只检查，不改
#   bash integrate.sh /path/to/harnessuse/agent --revert   # 撤销登记（保留拷贝的两个文件）
#
# 为什么写成脚本而不是让人手动改：登记分散在 4 个文件里，手改容易漏一处，
# 而漏 `VIEW_FEATURE` 的表现是「装上了但点不开」——很难查。
#
# 纪律（与仓库其它脚本一致）：
#   - 含中文的变量一律写 ${VAR}（macOS 自带 bash 3.2 会把多字节字符吞进变量名）；
#   - **先检查再改**：某处内容与预期不符就停下报错，不做模糊替换；
#   - 幂等：已经改过的位置跳过。
set -eu

HERE="$(cd "$(dirname "$0")" && pwd)"
DEST="${1:-}"
MODE="${2:-apply}"

if [ -z "${DEST}" ]; then
  echo "用法: bash integrate.sh <harnessuse/agent 目录> [--check|--revert]" >&2
  exit 2
fi
if [ ! -d "${DEST}/src" ]; then
  echo "✗ ${DEST} 看起来不是 harness-use 的 agent 目录（缺 src/）" >&2
  exit 2
fi

PLUGINS_TS="${DEST}/src/agent/plugins.ts"
CHROME_TSX="${DEST}/src/components/chrome.tsx"
APP_TSX="${DEST}/src/App.tsx"
PAGE="${DEST}/src/components/Rsi3dPage.tsx"
MODULE="${DEST}/src/agent/rsi3d.ts"

# 一处标记串就够判断「登记过没有」：这几处是同时改的
MARK="feature-rsi3d"

for f in "${PLUGINS_TS}" "${CHROME_TSX}" "${APP_TSX}"; do
  if [ ! -f "${f}" ]; then
    echo "✗ 找不到 ${f}（宿主的插件机制与预期不符，请先看 prd/agent-plugin.md §2.2）" >&2
    exit 2
  fi
done

if grep -q "${MARK}" "${PLUGINS_TS}" "${CHROME_TSX}" "${APP_TSX}" 2>/dev/null; then
  ALREADY=1
else
  ALREADY=0
fi

# ---------------------------------------------------------------- 检查：四处锚点都要在
# 注意顺序：**先检查锚点，再判断是否已登记**。这样 --check 在「已经装过」的宿主上
# 依然有意义（宿主改版后锚点会漂，门禁要能发现），而不是一句「已登记」就放行。
missing=0
need() { # need <文件> <锚点> <说明>
  if grep -qF -- "$2" "$1"; then
    echo "  ✓ $3"
  else
    echo "  ✗ $3 —— 在 $1 里找不到锚点：$2" >&2
    missing=$((missing + 1))
  fi
}
echo "检查插入锚点："
need "${PLUGINS_TS}" "kind: 'feature'," "plugins.ts 有 feature 目录条目"
need "${PLUGINS_TS}" "const MANIFEST_KEY" "plugins.ts 有安装清单键"
need "${CHROME_TSX}" "export type View" "chrome.tsx 有 View 类型"
need "${CHROME_TSX}" "export const VIEW_FEATURE" "chrome.tsx 有 VIEW_FEATURE"
need "${CHROME_TSX}" "export function enabledViews" "chrome.tsx 有 enabledViews()"
need "${APP_TSX}" "const active: View" "App.tsx 有活动视图变量"
if [ "${missing}" -gt 0 ]; then
  echo "✗ ${missing} 处锚点对不上，**没有改动任何文件**。宿主可能已经改版，请按 README 手动接入。" >&2
  exit 1
fi

if [ "${MODE}" = "--check" ]; then
  if [ "${ALREADY}" = "1" ]; then
    echo "✓ 锚点齐全（且已经登记过）"
  else
    echo "✓ 锚点齐全，可以接入（--check 不改文件）"
  fi
  exit 0
fi

if [ "${ALREADY}" = "1" ]; then
  echo "✓ 锚点齐全；这四处已经登记过（幂等跳过，不改文件）"
  exit 0
fi

# ---------------------------------------------------------------- 改：用 python 做精确插入（含中文，避开 sed 在 macOS 上的坑）
python3 - "${PLUGINS_TS}" "${CHROME_TSX}" "${APP_TSX}" "${MODE}" <<'PY'
import pathlib
import sys

plugins, chrome, app, mode = sys.argv[1:5]
revert = mode == "--revert"

ENTRY = """  {
    id: 'feature-rsi3d',
    kind: 'feature',
    name: 'RSI 3D',
    vendor: 'rsi3d',
    desc: '把 rsi3d 接进来：身份绑定、发布制品、检索 Harness 并驱动一次 Run（控制面，3D 数据不过平台）。安装后顶部出现「RSI 3D」。',
    source: 'builtin',
  },
"""


def patch(path: str, apply_fn, undo_fn):
    p = pathlib.Path(path)
    s = p.read_text()
    if revert:
        new = undo_fn(s)
        if new == s:
            print(f"  – {p.name} 没有可撤销的改动")
            return
    else:
        new = apply_fn(s)
    if new == s:
        print(f"  – {p.name} 无变化")
        return
    p.write_text(new)
    print(f"  ✓ {p.name}")


# ① FEATURE_CATALOG 加一条（插在数组开头之后，即第一条之后）
def add_entry(s: str) -> str:
    anchor = "export const FEATURE_CATALOG: PluginMeta[] = [\n"
    i = s.index(anchor) + len(anchor)
    return s[:i] + ENTRY + s[i:]


def del_entry(s: str) -> str:
    return s.replace(ENTRY, "")


# ② chrome.tsx：View / VIEW_META / VIEW_FEATURE / enabledViews 四处
def chrome_add(s: str) -> str:
    s = s.replace(
        "export type View = 'agent' | 'create' | 'apps' | 'me'",
        "export type View = 'agent' | 'create' | 'apps' | 'me' | 'rsi3d'",
        1,
    )
    s = s.replace(
        "  me: { label: '个人中心', title: '个人中心 · 资料与偏好' },",
        "  me: { label: '个人中心', title: '个人中心 · 资料与偏好' },\n"
        "  rsi3d: { label: 'RSI 3D', title: 'rsi3d · 3D 资产的自动生成与自进化' },",
        1,
    )
    s = s.replace(
        "  me: 'feature-me',\n}",
        "  me: 'feature-me',\n  rsi3d: 'feature-rsi3d',\n}",
        1,
    )
    s = s.replace(
        "  if (isFeatureOn(VIEW_FEATURE.me as string)) out.push('me')\n  return out",
        "  if (isFeatureOn(VIEW_FEATURE.me as string)) out.push('me')\n"
        "  if (isFeatureOn(VIEW_FEATURE.rsi3d as string)) out.push('rsi3d')\n  return out",
        1,
    )
    return s


def chrome_del(s: str) -> str:
    s = s.replace("export type View = 'agent' | 'create' | 'apps' | 'me' | 'rsi3d'",
                  "export type View = 'agent' | 'create' | 'apps' | 'me'", 1)
    s = s.replace("\n  rsi3d: { label: 'RSI 3D', title: 'rsi3d · 3D 资产的自动生成与自进化' },", "", 1)
    s = s.replace("\n  rsi3d: 'feature-rsi3d',", "", 1)
    s = s.replace("\n  if (isFeatureOn(VIEW_FEATURE.rsi3d as string)) out.push('rsi3d')", "", 1)
    return s


# ③ App.tsx：import + 渲染分支
def app_add(s: str) -> str:
    s = s.replace(
        "import MePage from './components/MePage'",
        "import MePage from './components/MePage'\nimport Rsi3dPage from './components/Rsi3dPage'",
        1,
    )
    anchor = "      {active === 'me' && viewAllowed('me') && ("
    i = s.index(anchor)
    # 找到该分支的结束（下一个同级 `)}` 行）
    j = s.index("      )}\n", i) + len("      )}\n")
    block = (
        "      {active === 'rsi3d' && viewAllowed('rsi3d') && (\n"
        "        <div className=\"hu-view on\">\n"
        "          <Rsi3dPage onNav={setView} />\n"
        "        </div>\n"
        "      )}\n"
    )
    return s[:j] + block + s[j:]


def app_del(s: str) -> str:
    s = s.replace("\nimport Rsi3dPage from './components/Rsi3dPage'", "", 1)
    s = s.replace(
        "      {active === 'rsi3d' && viewAllowed('rsi3d') && (\n"
        "        <div className=\"hu-view on\">\n"
        "          <Rsi3dPage onNav={setView} />\n"
        "        </div>\n"
        "      )}\n",
        "",
        1,
    )
    return s


patch(plugins, add_entry, del_entry)
patch(chrome, chrome_add, chrome_del)
patch(app, app_add, app_del)
PY

# ---------------------------------------------------------------- 拷两个新文件（--revert 时保留）
if [ "${MODE}" != "--revert" ]; then
  cp "${HERE}/rsi3d.ts" "${MODULE}"
  cp "${HERE}/Rsi3dPage.tsx" "${PAGE}"
  echo "  ✓ 拷贝 Rsi3dPage.tsx · rsi3d.ts"
fi

echo
if [ "${MODE}" = "--revert" ]; then
  echo "✓ 已撤销登记（两个新文件保留，可自行删除）"
else
  echo "✓ 接入完成。下一步："
  echo "    cd ${DEST} && npx tsc --noEmit     # 宿主是 strict + noUnusedLocals"
  echo "  然后在应用中心安装「RSI 3D」——顶部才会出现该页面。"
fi
