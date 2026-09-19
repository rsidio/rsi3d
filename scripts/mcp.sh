#!/usr/bin/env bash
# 给 MCP 客户端（VS Code / Cursor / Claude Code …）用的启动器。
#
# 为什么需要它：客户端只认「command + args」，而它不知道要先 cargo build，
# 也不知道工作目录应该取哪儿。这个脚本把这两件事包掉，让配置只有一行。
#
# 用法（由客户端调用，也可手动调试）：
#   rsi3d-harness/scripts/mcp.sh            # 工作目录 = 当前目录
#   RSI3D_HARNESS_ROOT=/path/to/ws scripts/mcp.sh
#
# 协议要求：stdout 只能有 MCP 报文，所以**所有**提示都往 stderr 写。
set -euo pipefail

here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
root="${here}/.."

# 允许客户端/用户覆盖工作目录（引擎只能读写这里）
workdir="${RSI3D_HARNESS_ROOT:-${PWD}}"

bin="${root}/target/release/rsi3d-harness"
if [ ! -x "${bin}" ]; then
  echo "[rsi3d-harness] 首次运行：正在构建引擎（cargo build --release）…" >&2
  if ! (cd "${root}" && cargo build --release >&2); then
    echo "[rsi3d-harness] 构建失败。请先手动执行：cd ${root} && cargo build --release" >&2
    exit 1
  fi
fi

exec "${bin}" mcp --root "${workdir}"
