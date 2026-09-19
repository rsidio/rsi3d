// 自定义评测维度：{{dimension}}
//
// 契约（引擎中立，不依赖任何渲染能力）：
//   export const id      —— 与 rsi3d-plugin.json 里 contributes.evaluators[].id 一致
//   export function evaluate(scene) -> { score, notes, detail }
//
// 纪律：
//   1. **纯函数**：不得修改传入的 scene，不得联网、不得读文件；
//   2. **确定性**：同一个 scene 必须永远得到同一个 score（否则分数曲线不可信）；
//   3. **可解释**：notes 是给人和 Agent 看的自然语言批判，别写「得分 0.62」这种废话。
//
// scene 的形状与 rsi3d-harness 的统一场景表示一致（这里只用包围盒，便于先跑起来）：
//   { "objects": [ { "id": "obj:sofa_01", "role": "furniture", "material": "fabric",
//                    "aabb": { "min": [x,y,z], "max": [x,y,z] } } ], ... }

export const id = '{{dimension}}'
export const title = '通道宽度：家具之间留不留得出走动的路'

/** 人走得过的最小宽度（米）。家居里 0.6m 是单人侧身通过的底线。 */
const MIN_WALKWAY = 0.6

const round4 = (x) => Math.round(x * 10000) / 10000
const clamp01 = (x) => Math.max(0, Math.min(1, x))
const axisGap = (aMin, aMax, bMin, bMax) => (aMax < bMin ? bMin - aMax : bMax < aMin ? aMin - bMax : 0)

function gapBetween(a, b) {
  const gaps = [
    axisGap(a.min[0], a.max[0], b.min[0], b.max[0]),
    axisGap(a.min[1], a.max[1], b.min[1], b.max[1]),
    axisGap(a.min[2], a.max[2], b.min[2], b.max[2]),
  ].filter((v) => v > 0)
  return gaps.length ? Math.min(...gaps) : 0
}

export function evaluate(scene) {
  // 只看落地家具；地毯这类 role=floor 的不参与（可以踩）
  const solids = (scene.objects ?? []).filter((o) => o.role !== 'floor')
  const pairs = []
  for (let i = 0; i < solids.length; i++) {
    for (let j = i + 1; j < solids.length; j++) {
      pairs.push({
        pair: [solids[i].id, solids[j].id],
        gap: round4(gapBetween(solids[i].aabb, solids[j].aabb)),
      })
    }
  }

  const tight = pairs.filter((p) => p.gap < MIN_WALKWAY)
  const notes = []
  for (const t of tight) {
    if (t.gap === 0) {
      notes.push(`${t.pair.join(' 与 ')} 贴在一起，完全走不过去`)
    } else {
      notes.push(`${t.pair.join(' 与 ')} 只留了 ${t.gap}m，小于 ${MIN_WALKWAY}m 的通行底线`)
    }
  }

  return {
    score: pairs.length ? clamp01((pairs.length - tight.length) / pairs.length) : 1,
    notes,
    detail: { minWalkway: MIN_WALKWAY, pairs, tight: tight.map((t) => t.pair) },
  }
}
