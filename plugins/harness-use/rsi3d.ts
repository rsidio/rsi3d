// rsi3d 连接与 API —— 插件的自有模块（不进宿主 agent/ 目录，随插件一起放）
//
// 红线（对齐 `prd/agent-plugin.md` §2.5）：
//  1. 插件只是**客户端**：不实现 rsi3d 管理后台，重管理跳浏览器到 Web 控制台；
//  2. **不合并账号**：rsi3d 账号 ≠ harness-use 账号，这里只做「绑定」；
//  3. **凭据只存本机**：localStorage（与宿主 hu_llm / hu_platform 同一模式），
//     **永不上报给任何平台**；
//  4. **不代理数据**：3D 产物由客户侧 Runtime 直连产出，插件只拿 URL / 摘要。

export interface Rsi3dConn {
  baseUrl: string
  token: string
  email: string
}

const KEY = 'rsi3d_conn'

export const RSI3D_DEFAULTS: Rsi3dConn = {
  baseUrl: 'http://localhost:8282',
  token: '',
  email: '',
}

export function loadConn(): Rsi3dConn {
  try {
    const raw = localStorage.getItem(KEY)
    if (!raw) return { ...RSI3D_DEFAULTS }
    return { ...RSI3D_DEFAULTS, ...(JSON.parse(raw) as Partial<Rsi3dConn>) }
  } catch {
    return { ...RSI3D_DEFAULTS }
  }
}

export function saveConn(c: Rsi3dConn): void {
  localStorage.setItem(KEY, JSON.stringify(c))
}

export function clearConn(): void {
  localStorage.removeItem(KEY)
}

export function connected(c: Rsi3dConn): boolean {
  return !!(c.baseUrl.trim() && c.token.trim())
}

/** 展示用掩码：eyJh••••9f2c（凭据不回显全文） */
export function maskToken(t: string): string {
  if (!t) return ''
  if (t.length <= 10) return '••••••••'
  return `${t.slice(0, 4)}••••${t.slice(-4)}`
}

// ---------------------------------------------------------------- HTTP

export interface ArtifactView {
  id: string
  slug: string
  kind: string
  name: string
  summary: string
  version: string
  ref: string
  domain: string
}

export interface HarnessView {
  id: string
  title: string
  endpoint: string
  protocol: string
  domain: string
  hub_score: number
  verified: boolean
  ref?: string
  capabilities: string[]
}

export interface RunView {
  id: string
  status: string
  intent: string
  best_score: number
  iterations: number
  score_curve: number[]
}

async function req<T>(c: Rsi3dConn, path: string, init?: RequestInit): Promise<T> {
  const base = c.baseUrl.trim().replace(/\/+$/, '')
  const headers: Record<string, string> = { ...(init?.headers as Record<string, string> | undefined) }
  if (c.token) headers.Authorization = `Bearer ${c.token}`
  if (init?.body && typeof init.body === 'string') headers['Content-Type'] = 'application/json'
  const res = await fetch(`${base}/api${path}`, { ...init, headers })
  const text = await res.text()
  const data: unknown = text ? JSON.parse(text) : {}
  if (!res.ok) {
    // 平台错误信封是 {"error":{"code","message"}}；解析不出来就退回状态码
    const e = (data as { error?: { code?: string; message?: string } }).error
    throw new Error(e?.message ? `${e.code ?? res.status}: ${e.message}` : `HTTP ${res.status}`)
  }
  return data as T
}

/** 登录：口令只用于这一次请求，落盘的是 token */
export async function login(baseUrl: string, email: string, password: string): Promise<Rsi3dConn> {
  const tmp: Rsi3dConn = { baseUrl, token: '', email }
  const v = await req<{ token: string; user: { email: string } }>(tmp, '/auth/login', {
    method: 'POST',
    body: JSON.stringify({ email, password }),
  })
  return { baseUrl, token: v.token, email: v.user?.email ?? email }
}

export async function me(c: Rsi3dConn): Promise<{ email: string; plan: string; ns: string }> {
  const v = await req<{
    user: { email: string; plan: string }
    namespaces?: { slug: string }[]
  }>(c, '/auth/me')
  return { email: v.user?.email ?? '', plan: v.user?.plan ?? '', ns: v.namespaces?.[0]?.slug ?? '' }
}

export async function search(c: Rsi3dConn, q: string, kind: string): Promise<ArtifactView[]> {
  const p = new URLSearchParams({ limit: '20' })
  if (q) p.set('q', q)
  if (kind) p.set('kind', kind)
  const v = await req<{ artifacts: ArtifactView[] }>(c, `/artifacts?${p}`)
  return v.artifacts ?? []
}

export async function harnesses(c: Rsi3dConn, q: string): Promise<HarnessView[]> {
  const p = new URLSearchParams({ limit: '20' })
  if (q) p.set('q', q)
  const v = await req<{ harnesses: HarnessView[] }>(c, `/harnesses?${p}`)
  return v.harnesses ?? []
}

/**
 * 登记一次 Run。
 *
 * 注意：**登记 ≠ 执行**。平台只在控制面留一条记录；跑循环的是客户侧 Runtime
 * （直连 Harness 端点，3D 数据不过平台）。所以这里只登记，然后轮询状态。
 */
export async function startRun(
  c: Rsi3dConn,
  harnessId: string,
  intent: string,
): Promise<{ id: string }> {
  const v = await req<{ run: { id: string } }>(c, '/runs', {
    method: 'POST',
    body: JSON.stringify({ harnessId, intent }),
  })
  return { id: v.run.id }
}

export async function runStatus(c: Rsi3dConn, id: string): Promise<RunView> {
  const v = await req<{ run: RunView }>(c, `/runs/${id}`)
  return v.run
}

export async function recentRuns(c: Rsi3dConn): Promise<RunView[]> {
  const v = await req<{ runs: RunView[] }>(c, '/runs?limit=5')
  return v.runs ?? []
}

/** 上传字节再绑定成制品（skill/plugin 的包由 rsi3d CLI 打好，这里只负责传） */
export async function uploadAndPublish(
  c: Rsi3dConn,
  file: File,
  kind: string,
  slug: string,
  name: string,
): Promise<ArtifactView> {
  const base = c.baseUrl.trim().replace(/\/+$/, '')
  const form = new FormData()
  form.append('file', file)
  const up = await fetch(`${base}/api/uploads`, {
    method: 'POST',
    headers: { Authorization: `Bearer ${c.token}` },
    body: form,
  })
  if (!up.ok) throw new Error(`上传失败：HTTP ${up.status}`)
  const info = (await up.json()) as { url: string; sha256: string; size: number }
  const v = await req<{ artifact: ArtifactView }>(c, '/artifacts', {
    method: 'POST',
    body: JSON.stringify({
      slug,
      kind,
      name,
      version: '0.1.0',
      visibility: 'public',
      url: info.url,
      sha256: info.sha256,
      size: info.size,
    }),
  })
  return v.artifact
}

/** 分数曲线画成一行 sparkline（与平台控制台同一套字符） */
export function sparkline(points: number[]): string {
  if (points.length === 0) return ''
  const bars = '▁▂▃▄▅▆▇'
  return points
    .map((p) => {
      const i = Math.min(bars.length - 1, Math.max(0, Math.floor(p * bars.length)))
      return bars[i]
    })
    .join('')
}
