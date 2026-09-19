// 自定义工具：place_row（沿一条线批量等距摆放）
//
// 契约：
//   export const id
//   export function plan(scene, args) -> { commands: [...], notes: [...] }
//
// 纪律：工具**只产出命令**，不直接改场景 —— 真正的改动一律由引擎的 edit 落盘，
// 这样每一步都进 op log、可回滚、可归因。

export const id = 'place_row'

/**
 * @param scene 当前场景（只读）
 * @param args  { ids: string[], axis: 'x'|'z', spacing: number, start?: number }
 */
export function plan(scene, args = {}) {
  const ids = args.ids ?? []
  const axis = args.axis ?? 'x'
  const spacing = Number(args.spacing ?? 0.5)
  if (!ids.length) return { commands: [], notes: ['未指定 ids，无事可做'] }

  const axisIndex = axis === 'x' ? 0 : 2
  const objects = ids.map((id) => (scene.objects ?? []).find((o) => o.id === id))
  const missing = ids.filter((id, i) => !objects[i])
  if (missing.length) return { commands: [], notes: [`场景里没有：${missing.join('、')}`] }

  // 以第一件的当前位置为起点，其余依次向后排
  const base = objects[0].aabb.min[axisIndex]
  const commands = []
  const notes = []
  for (let i = 1; i < objects.length; i++) {
    const want = base + i * spacing
    const delta = Math.round((want - objects[i].aabb.min[axisIndex]) * 10000) / 10000
    if (Math.abs(delta) < 1e-6) continue
    const translate = [0, 0, 0]
    translate[axisIndex] = delta
    commands.push({
      op: 'transform',
      target: ids[i],
      params: { translate },
      reason: `place_row：按 ${spacing}m 间距沿 ${axis} 轴排列`,
      expect: { 'layout.walkway': '+' },
    })
    notes.push(`${ids[i]} 沿 ${axis} 轴移动 ${delta}m`)
  }
  if (!commands.length) notes.push('已经符合目标间距，无需改动')
  return { commands, notes }
}
