#!/usr/bin/env node
// rsi3d-harness 引擎契约的**最小参考实现**（mock）。
//
// 目的：在真引擎（WASM + WebGPU）就绪之前，你就能开发并调试自己的 Agent。
// 它只实现 4 个原语，用**确定性规则**打分，不做渲染 —— 所以 observe 的 views 为空，
// 并显式返回 degraded 说明。真引擎会在这里填上多视角快照。
//
// 用法：node mock-engine.mjs [--port 8290] [--scene scene.json]
import { createServer } from 'node:http'
import { readFileSync } from 'node:fs'
import { resolve } from 'node:path'

const argv = process.argv.slice(2)
const arg = (name, fallback) => {
  const i = argv.indexOf('--' + name)
  return i >= 0 && argv[i + 1] ? argv[i + 1] : fallback
}
const PORT = Number(arg('port', process.env.MOCK_PORT ?? 8290))
const SCENE_FILE = arg('scene', 'scene.json')

// 维度权重：改这里就改了「什么算好」。真引擎里这份权重可由业务包覆盖。
const WEIGHTS = {
  'lighting.exposure': 0.25,
  'lighting.window': 0.25,
  'layout.clearance': 0.25,
  'layout.containment': 0.1,
  'material.repeat': 0.1,
  'semantics.intent': 0.05,
}

const clamp01 = (x) => Math.max(0, Math.min(1, x))
const round4 = (x) => Math.round(x * 10000) / 10000
const clone = (x) => JSON.parse(JSON.stringify(x))
const overlap1 = (aMin, aMax, bMin, bMax) => Math.max(0, Math.min(aMax, bMax) - Math.max(aMin, bMin))
const axisGap = (aMin, aMax, bMin, bMax) => (aMax < bMin ? bMin - aMax : bMax < aMin ? aMin - bMax : 0)

// ---------------------------------------------------------------- 状态

let scene = JSON.parse(readFileSync(resolve(SCENE_FILE), 'utf8'))
let revision = 1
const oplog = []
const snapshots = new Map([[1, clone(scene)]])

const objects = () => scene.objects
const byId = (id) => scene.objects.find((o) => o.id === id)

// ---------------------------------------------------------------- 几何

/** 两包围盒的间距：分离轴上最小的正间隙；重叠则返回 0。 */
function aabbGap(a, b) {
  const gaps = [
    axisGap(a.min[0], a.max[0], b.min[0], b.max[0]),
    axisGap(a.min[1], a.max[1], b.min[1], b.max[1]),
    axisGap(a.min[2], a.max[2], b.min[2], b.max[2]),
  ].filter((v) => v > 0)
  return gaps.length ? Math.min(...gaps) : 0
}

/** 谁挡住了窗：落在窗前 bandDepth 米内的家具。 */
function windowBlockers() {
  const w = scene.window
  const band = [w.z, w.z + w.bandDepth]
  const out = []
  for (const o of objects()) {
    if (o.role === 'floor') continue
    const inBand = o.aabb.max[2] > band[0] && o.aabb.min[2] < band[1]
    const x = overlap1(o.aabb.min[0], o.aabb.max[0], w.spanX[0], w.spanX[1])
    const y = overlap1(o.aabb.min[1], o.aabb.max[1], 0, w.height)
    if (inBand && x > 0 && y > 0) {
      out.push({ id: o.id, blocked: (x * y) / ((w.spanX[1] - w.spanX[0]) * w.height) })
    }
  }
  return out
}

function roomBounds() {
  const [sx, , sz] = scene.room.size
  return { min: [-sx / 2, 0, -sz / 2], max: [sx / 2, scene.room.size[1], sz / 2] }
}

// ---------------------------------------------------------------- 打分（确定性，无模型）

function scoreExposure() {
  const sun = scene.lights.find((l) => l.id === 'sun')
  const ideal = 1.0
  return {
    score: clamp01(1 - Math.abs(sun.intensity - ideal) / 1.5),
    detail: { intensity: sun.intensity, ideal },
  }
}

function scoreWindow() {
  const blockers = windowBlockers()
  const blocked = clamp01(blockers.reduce((s, b) => s + b.blocked, 0))
  return {
    score: clamp01(1 - blocked),
    detail: {
      blocked: round4(blocked),
      blockers: blockers.map((b) => b.id),
      band: [scene.window.z, scene.window.z + scene.window.bandDepth],
    },
  }
}

function clearanceChecks() {
  return (scene.clearance_rules ?? []).map((rule) => {
    const pair = rule.pair.map(byId)
    const gap = round4(aabbGap(pair[0].aabb, pair[1].aabb))
    const dist = gap < rule.min ? rule.min - gap : gap > rule.max ? gap - rule.max : 0
    return {
      pair: rule.pair,
      gap,
      rule: { min: rule.min, max: rule.max, reason: rule.reason },
      dist: round4(dist),
      score: clamp01(1 - dist / 2),
    }
  })
}

function scoreClearance() {
  const checks = clearanceChecks()
  return { score: checks.reduce((m, c) => Math.min(m, c.score), 1), detail: { checks } }
}

function scoreContainment() {
  const { min, max } = roomBounds()
  const solids = objects().filter((o) => o.role !== 'floor')
  const bad = solids.filter(
    (o) =>
      o.aabb.min[0] < min[0] || o.aabb.max[0] > max[0] || o.aabb.min[2] < min[2] || o.aabb.max[2] > max[2],
  )
  return {
    score: solids.length ? clamp01((solids.length - bad.length) / solids.length) : 1,
    detail: { violations: bad.map((o) => o.id) },
  }
}

function scoreMaterialRepeat() {
  const mats = objects()
    .map((o) => o.material)
    .filter(Boolean)
  const unique = [...new Set(mats)]
  const dupes = mats.length - unique.length
  return {
    score: mats.length > 1 ? clamp01(1 - dupes / (mats.length - 1)) : 1,
    detail: { materials: unique, duplicates: dupes },
  }
}

function scoreIntent() {
  const kws = scene.intent_keywords ?? []
  const hit = kws.filter((k) => k.present).length
  return {
    score: kws.length ? clamp01(hit / kws.length) : 1,
    detail: { missing: kws.filter((k) => !k.present).map((k) => k.word) },
  }
}

function evaluate() {
  const parts = {
    'lighting.exposure': scoreExposure(),
    'lighting.window': scoreWindow(),
    'layout.clearance': scoreClearance(),
    'layout.containment': scoreContainment(),
    'material.repeat': scoreMaterialRepeat(),
    'semantics.intent': scoreIntent(),
  }
  const dimensions = {}
  let total = 0
  for (const [key, part] of Object.entries(parts)) {
    dimensions[key] = round4(part.score)
    total += (WEIGHTS[key] ?? 0) * part.score
  }

  const notes = []
  const win = parts['lighting.window'].detail
  if (win.blocked > 0) notes.push(`窗户被 ${win.blockers.join('、')} 遮挡 ${(win.blocked * 100).toFixed(0)}%`)
  const exp = parts['lighting.exposure'].detail
  if (parts['lighting.exposure'].score < 0.9) {
    notes.push(`日光强度 ${exp.intensity}，理想 ${exp.ideal}（画面过曝）`)
  }
  for (const c of parts['layout.clearance'].detail.checks) {
    if (c.dist > 0) {
      notes.push(`${c.pair.join(' 与 ')} 间距 ${c.gap}m，应在 ${c.rule.min}–${c.rule.max}m（${c.rule.reason}）`)
    }
  }
  for (const m of parts['semantics.intent'].detail.missing) notes.push(`意图里的「${m}」在场景里不存在`)

  return {
    revision,
    total: round4(total),
    dimensions,
    detail: {
      exposure: exp,
      window: win,
      clearance: parts['layout.clearance'].detail.checks,
      containment: parts['layout.containment'].detail,
      material: parts['material.repeat'].detail,
      intent: parts['semantics.intent'].detail,
    },
    notes,
  }
}

// ---------------------------------------------------------------- 观测

function diffAgainst(prev) {
  if (!prev) return { vs: null, added: [], changed: [], removed: [] }
  const now = new Map(objects().map((o) => [o.id, o]))
  const then = new Map(prev.objects.map((o) => [o.id, o]))
  const changed = []
  for (const [id, o] of now) {
    const p = then.get(id)
    if (p && JSON.stringify(p.aabb) !== JSON.stringify(o.aabb)) {
      changed.push({ id, what: 'aabb', delta: o.aabb.min.map((v, i) => round4(v - p.aabb.min[i])) })
    }
  }
  return {
    vs: revision - 1,
    added: [...now.keys()].filter((k) => !then.has(k)),
    changed,
    removed: [...then.keys()].filter((k) => !now.has(k)),
  }
}

function observe() {
  const solids = objects().filter((o) => o.role !== 'floor')
  return {
    session: 'mock',
    revision,
    engine: { kind: 'mock', renderer: 'none' },
    degraded: { reason: 'mock-engine 不做渲染，views 为空；真引擎会返回多视角快照' },
    views: [],
    summary: {
      units: scene.units,
      intent: scene.intent,
      room: scene.room,
      window: scene.window,
      objects: objects().map((o) => ({ id: o.id, role: o.role, material: o.material, aabb: o.aabb })),
      lights: scene.lights,
      clearance_rules: scene.clearance_rules ?? [],
      blockers: windowBlockers().map((b) => b.id),
      // mock 不算真几何，用占位值说明「这里会有规模摘要」
      triangles: solids.length * 12000,
    },
    diff: diffAgainst(revision > 1 ? snapshots.get(revision - 1) : null),
    editability: [{ layer: 'mock', level: 'full', note: 'mock 直接改 JSON 场景；真引擎按层声明可编辑性' }],
    cost: { observe_ms: 1, vram_mb: 0, estimate_next_edit_ms: 1 },
  }
}

// ---------------------------------------------------------------- 编辑

function validateObject(o) {
  const { min, max } = roomBounds()
  const warnings = []
  if (o.aabb.min[0] < min[0] || o.aabb.max[0] > max[0] || o.aabb.min[2] < min[2] || o.aabb.max[2] > max[2]) {
    warnings.push(`${o.id} 超出房间范围`)
  }
  for (const other of objects()) {
    if (other.id === o.id || other.role === 'floor') continue
    if (aabbGap(o.aabb, other.aabb) === 0) warnings.push(`${o.id} 与 ${other.id} 相交`)
  }
  return { ok: true, warnings }
}

const OPS = {
  transform(target, params) {
    const o = byId(target)
    if (!o) throw new Error(`找不到对象 ${target}`)
    const d = params.translate ?? [0, 0, 0]
    o.aabb.min = o.aabb.min.map((v, i) => round4(v + d[i]))
    o.aabb.max = o.aabb.max.map((v, i) => round4(v + d[i]))
    return {
      inverse: { op: 'transform', target, params: { translate: d.map((v) => -v) } },
      validation: validateObject(o),
    }
  },
  set_light(target, params) {
    const l = scene.lights.find((x) => x.id === target)
    if (!l) throw new Error(`找不到灯光 ${target}`)
    const inverse = { op: 'set_light', target, params: {} }
    if (params.intensity !== undefined) {
      inverse.params.intensity = l.intensity
      l.intensity = round4(params.intensity)
    }
    if (params.color !== undefined) {
      inverse.params.color = l.color
      l.color = params.color
    }
    return { inverse, validation: { ok: true, warnings: [] } }
  },
  set_material(target, params) {
    const o = byId(target)
    if (!o) throw new Error(`找不到对象 ${target}`)
    const inverse = { op: 'set_material', target, params: {} }
    if (params.material !== undefined) {
      inverse.params.material = o.material
      o.material = params.material
    }
    return { inverse, validation: { ok: true, warnings: [] } }
  },
}

function applyEdit(body) {
  const { op, target, params = {}, reason = '', expect = null } = body ?? {}
  const impl = OPS[op]
  if (!impl) {
    return { error: { code: 'unknown_op', message: `不支持的 op：${op}（支持 ${Object.keys(OPS).join(' / ')}）` } }
  }
  let result
  try {
    result = impl(target, params)
  } catch (e) {
    return { error: { code: 'invalid_target', message: e.message } }
  }
  revision += 1
  snapshots.set(revision, clone(scene))
  oplog.push({ rev: revision, op, target, params, reason, expect, inverse: result.inverse, at: new Date().toISOString() })
  return {
    revision,
    applied: { op, target, params },
    inverse: result.inverse,
    validation: result.validation,
  }
}

function checkout(rev) {
  const snap = snapshots.get(Number(rev))
  if (!snap) return { error: { code: 'not_found', message: `没有 revision ${rev} 的快照` } }
  revision += 1
  scene = clone(snap)
  snapshots.set(revision, clone(scene))
  oplog.push({ rev: revision, op: 'checkout', target: `rev:${rev}`, params: {}, reason: '回滚', at: new Date().toISOString() })
  return { revision, checkedOut: Number(rev) }
}

// ---------------------------------------------------------------- HTTP

const ROUTES = {
  '/health': () => ({ ok: true, engine: 'mock', revision }),
  '/observe': () => observe(),
  '/evaluate': () => evaluate(),
  '/edit': (body) => applyEdit(body),
  '/checkout': (body) => checkout(body?.revision),
  '/oplog': () => ({ oplog }),
  '/export': () => ({ revision, scene: clone(scene), oplog, format: 'json' }),
}

const server = createServer((req, res) => {
  const send = (code, payload) => {
    const text = JSON.stringify(payload, null, 2)
    res.writeHead(code, { 'content-type': 'application/json; charset=utf-8', 'content-length': Buffer.byteLength(text) })
    res.end(text)
  }
  const handler = ROUTES[req.url]
  if (!handler) return send(404, { error: { code: 'not_found', message: `未知路径 ${req.url}` } })

  let raw = ''
  req.on('data', (c) => (raw += c))
  req.on('end', () => {
    let body = {}
    if (raw.trim()) {
      try {
        body = JSON.parse(raw)
      } catch {
        return send(400, { error: { code: 'bad_json', message: '请求体不是合法 JSON' } })
      }
    }
    try {
      send(200, handler(body))
    } catch (e) {
      send(500, { error: { code: 'engine_error', message: String(e?.message ?? e) } })
    }
  })
})

server.listen(PORT, '127.0.0.1', () => {
  const solids = objects().filter((o) => o.role !== 'floor').length
  console.log(`mock 引擎就绪 http://127.0.0.1:${PORT}  （场景 ${SCENE_FILE}，${solids} 件家具，rev ${revision}）`)
  console.log('这是契约参考实现，不是真引擎：views 为空、不做渲染。')
})
