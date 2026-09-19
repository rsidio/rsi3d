#!/usr/bin/env node
// 插件自测：不依赖引擎、不依赖网络，直接对 fixture 跑一遍你的实现。
// 约定：任何断言失败 → 退出码非 0（CI / 引擎装载前的门禁都看这个）。
import assert from 'node:assert/strict'
import { readFileSync } from 'node:fs'
import { fileURLToPath } from 'node:url'
import { dirname, join } from 'node:path'

import * as evaluator from './src/evaluator.mjs'
import * as tool from './src/tool.mjs'

const HERE = dirname(fileURLToPath(import.meta.url))
const scene = JSON.parse(readFileSync(join(HERE, 'fixtures/scene.json'), 'utf8'))
const clone = (x) => JSON.parse(JSON.stringify(x))

let passed = 0
const check = (name, fn) => {
  try {
    fn()
    passed += 1
    console.log(`  ✓ ${name}`)
  } catch (e) {
    console.error(`  ✗ ${name}\n      ${e.message}`)
    process.exitCode = 1
  }
}

console.log(`${evaluator.id} 自测（fixture: fixtures/scene.json）`)

check('维度 id 与清单一致', () => {
  const manifest = JSON.parse(readFileSync(join(HERE, 'rsi3d-plugin.json'), 'utf8'))
  const ids = manifest.contributes.evaluators.map((e) => e.id)
  assert.ok(ids.includes(evaluator.id), `清单里没有 ${evaluator.id}，只有 ${ids.join(', ')}`)
})

check('打分是 0..1 的有限数', () => {
  const r = evaluator.evaluate(scene)
  assert.equal(typeof r.score, 'number')
  assert.ok(Number.isFinite(r.score), 'score 必须是有限数')
  assert.ok(r.score >= 0 && r.score <= 1, `score 越界：${r.score}`)
})

check('确定性：同一场景两次结果完全相同', () => {
  const a = evaluator.evaluate(scene)
  const b = evaluator.evaluate(scene)
  assert.deepEqual(a, b, '两次结果不一致 —— 评测维度必须是纯函数')
})

check('不修改传入的场景', () => {
  const before = clone(scene)
  evaluator.evaluate(scene)
  assert.deepEqual(scene, before, 'evaluate 修改了 scene！')
})

check('贴太近的场景要扣分并给出批判', () => {
  const tight = clone(scene)
  const a = tight.objects[0]
  const b = tight.objects[1]
  // 把第二件贴到第一件旁边（0.1m）
  const shift = a.aabb.max[0] + 0.1 - b.aabb.min[0]
  b.aabb.min[0] += shift
  b.aabb.max[0] += shift
  const r = evaluator.evaluate(tight)
  assert.ok(r.score < 1, `应该扣分，实得 ${r.score}`)
  assert.ok(r.notes.length > 0, '应该给出自然语言批判')
  assert.ok(r.detail.tight.length > 0, 'detail 里应指出违规的对')
})

check('工具只产出命令，不改场景', () => {
  const before = clone(scene)
  const ids = scene.objects.filter((o) => o.role !== 'floor').map((o) => o.id)
  const out = tool.plan(scene, { ids, axis: 'x', spacing: 0.8 })
  assert.ok(Array.isArray(out.commands), 'commands 必须是数组')
  for (const c of out.commands) {
    assert.ok(c.op === 'transform' && c.target && c.params, `命令形状不对：${JSON.stringify(c)}`)
  }
  assert.deepEqual(scene, before, 'plan 修改了 scene！')
})

console.log(
  process.exitCode ? '\n✗ 自测未通过' : `\n✓ 自测通过（${passed} 项）—— 可以放进 ~/.rsi3d-harness/plugins/ 了`,
)
