#!/usr/bin/env node
// {{project}} —— 一个最小 Agent：observe → decide → edit → evaluate
//
// 这就是 3D RSI 循环的最小骨架：
//   observe  = 看（快照 + 结构化摘要 + diff）
//   decide   = critique + mutate 的决策（**默认是规则策略**；换成 LLM 调用就是真 Agent）
//   edit     = 写进引擎（产生一个新 revision，带逆命令）
//   evaluate = 打分（分数曲线与归因表都出自这里）
//
// 用法：
//   node agent.mjs --mock              # 自带 mock 引擎，一条命令跑通
//   node agent.mjs                    # 对着真引擎跑（先启动 rsi3d-harness serve）
//   node agent.mjs --iters 8 --engine http://127.0.0.1:8290
import { spawn } from 'node:child_process'
import { fileURLToPath } from 'node:url'
import { dirname, join } from 'node:path'

const HERE = dirname(fileURLToPath(import.meta.url))
const argv = process.argv.slice(2)
const flag = (name, fallback) => {
  const i = argv.indexOf('--' + name)
  if (i < 0) return fallback
  const v = argv[i + 1]
  return v && !v.startsWith('--') ? v : true
}
const ENGINE = String(flag('engine', '{{engine}}'))
const ITERS = Number(flag('iters', '{{iters}}'))
const MOCK = argv.includes('--mock')
const PORT = Number(new URL(ENGINE).port || 8290)
const round4 = (x) => Math.round(x * 10000) / 10000
const sleep = (ms) => new Promise((r) => setTimeout(r, ms))

async function api(path, body) {
  const res = await fetch(ENGINE + path, {
    method: 'POST',
    headers: { 'content-type': 'application/json' },
    body: JSON.stringify(body ?? {}),
  })
  const data = await res.json()
  if (data.error) throw new Error(`[${data.error.code}] ${data.error.message}`)
  return data
}

const post = (path, body) => api(path, body)

// ---------------------------------------------------------------- decide：Agent 的脑子

/**
 * 默认策略：**规则**。三类问题的修法各自独立，便于你替换成 LLM。
 *
 * 换成 LLM 的做法：把 (obs, scores) 序列化成 JSON 塞进 prompt，
 * 让它返回同一形状的 { op, target, params, reason, expect } 即可 —— 引擎契约不变。
 */
function decide(obs, scores) {
  const d = scores.detail
  const dim = scores.dimensions

  // 1) 窗户被挡 → 把挡窗的家具挪出窗带（+Z 方向离窗更远）
  if (dim['lighting.window'] < 0.98 && d.window.blockers.length > 0) {
    const id = d.window.blockers[0]
    const o = obs.summary.objects.find((x) => x.id === id)
    const delta = round4(d.window.band[1] - o.aabb.min[2] + 0.1)
    return {
      op: 'transform',
      target: id,
      params: { translate: [0, 0, delta] },
      reason: `窗户被 ${id} 遮挡 ${(d.window.blocked * 100).toFixed(0)}%，把它沿 +Z 挪出窗带`,
      expect: { 'lighting.window': '+' },
    }
  }

  // 2) 过曝 → 把日光拉回理想强度
  if (dim['lighting.exposure'] < 0.9) {
    return {
      op: 'set_light',
      target: 'sun',
      params: { intensity: d.exposure.ideal },
      reason: `日光强度 ${d.exposure.intensity} 偏高导致过曝，拉到 ${d.exposure.ideal}`,
      expect: { 'lighting.exposure': '+' },
    }
  }

  // 3) 间距不合理 → 把「远处那件」挪到区间中点
  const bad = (d.clearance ?? []).find((c) => c.dist > 0)
  if (bad) {
    const a = obs.summary.objects.find((o) => o.id === bad.pair[0])
    const b = obs.summary.objects.find((o) => o.id === bad.pair[1])
    const far = a.aabb.min[2] > b.aabb.min[2] ? a : b
    const near = far === a ? b : a
    const ideal = Math.round(((bad.rule.min + bad.rule.max) / 2) * 100) / 100
    const dir = far.aabb.min[2] > near.aabb.min[2] ? -1 : 1
    const delta = round4(dir * (bad.gap - ideal))
    return {
      op: 'transform',
      target: far.id,
      params: { translate: [0, 0, delta] },
      reason: `${bad.pair.join(' 与 ')} 间距 ${bad.gap}m 不在 ${bad.rule.min}–${bad.rule.max}m，挪到 ${ideal}m`,
      expect: { 'layout.clearance': '+' },
    }
  }

  return null // 无计可施 → 收敛（注意：收敛 ≠ 完美）
}

// ---------------------------------------------------------------- 展示

const LEVELS = ['▁', '▂', '▃', '▄', '▅', '▆', '▇', '█']
function sparkline(xs) {
  if (!xs.length) return ''
  const min = Math.min(...xs)
  const max = Math.max(...xs)
  if (max - min < 1e-9) return LEVELS[3].repeat(xs.length)
  return xs.map((x) => LEVELS[Math.min(7, Math.round(((x - min) / (max - min)) * 7))]).join('')
}

// ---------------------------------------------------------------- 主循环

async function run() {
  const curve = []
  const attribution = []
  const seen = new Set()
  let rev = 0
  let best = { total: -1, rev: 0 }
  let mock = null

  if (MOCK) mock = await startMock()

  console.log(`→ 引擎 ${ENGINE}${MOCK ? '（自带 mock）' : ''}，最多 ${ITERS} 轮`)
  try {
    for (let i = 1; i <= ITERS; i++) {
      const obs = await post('/observe', { revision: rev || undefined })
      rev = obs.revision
      const before = await post('/evaluate', { revision: rev })
      curve.push(before.total)
      if (before.total > best.total) best = { total: before.total, rev }

      const views = obs.views.length ? obs.views.length + ' 张快照' : '无快照（degraded）'
      console.log(`\n#${i}  rev ${rev} · 总分 ${before.total.toFixed(4)} · ${views}`)
      for (const note of before.notes) console.log(`    批判：${note}`)

      const cmd = decide(obs, before)
      if (!cmd) {
        console.log('\n✓ 收敛：没有可执行的改进命令（收敛 ≠ 质量达标，剩余缺口见上面的批判）')
        break
      }
      const sig = JSON.stringify([cmd.op, cmd.target, cmd.params])
      if (seen.has(sig)) {
        console.log('\n! 同一条命令重复出现，跳出循环（避免空转）')
        break
      }
      seen.add(sig)

      console.log(`    行动：${cmd.op} ${cmd.target} ${JSON.stringify(cmd.params)}`)
      const res = await post('/edit', cmd)
      if (res.validation?.warnings?.length) {
        for (const w of res.validation.warnings) console.log(`    ! 校验：${w}`)
      }
      const after = await post('/evaluate', { revision: res.revision })
      attribution.push({
        rev: res.revision,
        op: cmd.op,
        target: cmd.target,
        reason: cmd.reason,
        expect: cmd.expect,
        delta: round4(after.total - before.total),
      })
      rev = res.revision
    }
  } finally {
    if (mock) mock.kill()
  }

  // ---- 收尾：分数曲线 + 归因表 ----
  console.log('\n──────── 分数曲线 ────────')
  console.log(`  ${curve.map((v) => v.toFixed(3)).join(' → ')}`)
  console.log(`  ${sparkline(curve)}   最佳 ${best.total.toFixed(4)} @ rev ${best.rev}`)

  console.log('\n──────── 归因表（Agent 的判断 vs 实际效果）────────')
  console.log('  rev  op          target                Δscore   reason')
  for (const a of attribution) {
    const sign = a.delta > 0 ? '+' : ''
    console.log(
      `  ${String(a.rev).padEnd(4)} ${a.op.padEnd(11)} ${String(a.target).padEnd(21)} ${(sign + a.delta.toFixed(4)).padEnd(8)} ${a.reason}`,
    )
  }
  const wrong = attribution.filter((a) => a.delta <= 0).length
  console.log(
    `\n  共 ${attribution.length} 条命令，其中 ${wrong} 条没有提升（这些才是最该回看的部分）。`,
  )
  console.log('  接入真引擎：rsi3d-harness serve  →  node agent.mjs')
}

// ---------------------------------------------------------------- mock 生命周期

async function startMock() {
  const child = spawn(
    process.execPath,
    [join(HERE, 'mock-engine.mjs'), '--port', String(PORT), '--scene', join(HERE, 'scene.json')],
    { stdio: ['ignore', 'pipe', 'inherit'] },
  )
  child.stdout.on('data', (b) => process.stderr.write('[mock] ' + b))
  for (let i = 0; i < 80; i++) {
    try {
      await api('/health')
      return child
    } catch {
      await sleep(100)
    }
  }
  child.kill()
  throw new Error('mock 引擎未就绪（端口被占用？）')
}

run().catch((e) => {
  console.error('✗ ' + e.message)
  process.exit(1)
})
