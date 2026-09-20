// RSI 3D —— rsi3d 的 harness-use 功能应用（feature 插件）
//
// 四块（对齐 prd/agent-plugin.md §2.4）：① 身份绑定 ② 发布 ③ 检索与消费 ④ 最近 Run
//
// 红线：插件只是**客户端**。不实现 rsi3d 管理后台（用户/组织/计费），
// 重管理跳浏览器到 Web 控制台；不合并账号；凭据只存本机。
import { useEffect, useState } from 'react'
import { WindowHead } from './chrome'
import type { View } from './chrome'
import {
  clearConn,
  connected,
  harnesses,
  loadConn,
  login,
  maskToken,
  me,
  recentRuns,
  runStatus,
  saveConn,
  search,
  sparkline,
  startRun,
  uploadAndPublish,
} from '../agent/rsi3d'
import type { ArtifactView, HarnessView, Rsi3dConn, RunView } from '../agent/rsi3d'

export default function Rsi3dPage({ onNav }: { onNav: (v: View) => void }) {
  const [conn, setConn] = useState<Rsi3dConn>(() => loadConn())
  const [who, setWho] = useState<{ email: string; plan: string; ns: string } | null>(null)
  const [busy, setBusy] = useState(false)
  const [err, setErr] = useState('')
  const [note, setNote] = useState('')

  // 已绑定过就自动自检一次：让人一眼看到「还连着没有」
  useEffect(() => {
    if (connected(conn)) void refreshMe(conn)
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [])

  async function refreshMe(c: Rsi3dConn): Promise<void> {
    setBusy(true)
    setErr('')
    try {
      setWho(await me(c))
    } catch (e) {
      setErr(String(e))
      setWho(null)
    } finally {
      setBusy(false)
    }
  }

  async function doLogin(form: { baseUrl: string; email: string; password: string }): Promise<void> {
    setBusy(true)
    setErr('')
    setNote('')
    try {
      const c = await login(form.baseUrl, form.email, form.password)
      saveConn(c)
      setConn(c)
      await refreshMe(c)
      setNote('已绑定本机（凭据只存在这台机器上）')
    } catch (e) {
      setErr(String(e))
    } finally {
      setBusy(false)
    }
  }

  function unbind(): void {
    clearConn()
    setConn({ ...loadConn() })
    setWho(null)
    setNote('已解除绑定')
  }

  return (
    <div className="hu">
      <WindowHead view="rsi3d" onNav={onNav} />
      <div className="me-scroll">
        <div className="me">
          <div className="me-head">
            <div className="me-title">
              <h2>RSI 3D</h2>
              <p>
                把 rsi3d 接进来：绑定身份、发布制品、检索并驱动一次 Run。
                插件只是客户端 —— 账号不合并、凭据不出本机、3D 数据不经过任何平台。
              </p>
            </div>
            <span className="me-localkey">🔒 凭据只存本机</span>
          </div>

          {err ? <div className="result err" role="alert">{err}</div> : null}
          {note ? <div className="saved-tag">{note}</div> : null}

          <div className="me-grid">
            <IdentityCard
              conn={conn}
              who={who}
              busy={busy}
              onLogin={doLogin}
              onRefresh={() => void refreshMe(conn)}
              onUnbind={unbind}
            />
            {connected(conn) ? (
              <>
                <PublishCard conn={conn} />
                <SearchCard conn={conn} />
                <RunsCard conn={conn} />
              </>
            ) : (
              <section className="card">
                <header className="card-head">
                  <h4>还没绑定</h4>
                </header>
                <div className="empty">
                  绑定 rsi3d 账号后，发布 / 检索 / 跑 Run 三块才会出现。
                  <br />
                  没有账号？到 <a href={conn.baseUrl} target="_blank" rel="noreferrer">{conn.baseUrl}</a> 注册一个。
                </div>
              </section>
            )}
          </div>
        </div>
      </div>
    </div>
  )
}

// ---------------------------------------------------------------- ① 身份绑定

function IdentityCard({
  conn,
  who,
  busy,
  onLogin,
  onRefresh,
  onUnbind,
}: {
  conn: Rsi3dConn
  who: { email: string; plan: string; ns: string } | null
  busy: boolean
  onLogin: (f: { baseUrl: string; email: string; password: string }) => Promise<void>
  onRefresh: () => void
  onUnbind: () => void
}) {
  const [baseUrl, setBaseUrl] = useState(conn.baseUrl)
  const [email, setEmail] = useState(conn.email)
  const [password, setPassword] = useState('')

  return (
    <section className="card">
      <header className="card-head">
        <h4>① 身份绑定</h4>
        <span className="kbd-hint">口令只用于这一次登录 · 落盘的是 token</span>
      </header>

      <div className="field">
        <label>平台地址</label>
        <input value={baseUrl} onChange={(e) => setBaseUrl(e.target.value)} spellCheck={false} />
      </div>
      <div className="field">
        <label>邮箱</label>
        <input
          value={email}
          onChange={(e) => setEmail(e.target.value)}
          placeholder="you@example.com"
          spellCheck={false}
        />
      </div>
      <div className="field">
        <label>口令</label>
        <input
          type="password"
          value={password}
          onChange={(e) => setPassword(e.target.value)}
          autoComplete="current-password"
        />
      </div>

      {connected(conn) ? (
        <div className="row">
          <span className="chip">已绑定 {conn.email || who?.email || '-'}</span>
          {who ? <span className="chip">计划 {who.plan}</span> : null}
          {who?.ns ? <span className="chip">@{who.ns}</span> : null}
          <span className="harness-id" title="凭据掩码">{maskToken(conn.token)}</span>
          <div className="row-actions">
            <button className="btn ghost sm" onClick={onRefresh} disabled={busy}>
              自测连通
            </button>
            <button className="btn ghost sm" onClick={onUnbind}>
              解除绑定
            </button>
          </div>
        </div>
      ) : (
        <button
          className="btn solid full"
          disabled={busy || !baseUrl.trim() || !email.trim() || !password}
          onClick={() => void onLogin({ baseUrl, email, password })}
        >
          {busy ? '登录中…' : '绑定并登录'}
        </button>
      )}
    </section>
  )
}

// ---------------------------------------------------------------- ② 发布

function PublishCard({ conn }: { conn: Rsi3dConn }) {
  const [kind, setKind] = useState('bizpack')
  const [slug, setSlug] = useState('')
  const [name, setName] = useState('')
  const [file, setFile] = useState<File | null>(null)
  const [busy, setBusy] = useState(false)
  const [out, setOut] = useState<ArtifactView | null>(null)
  const [err, setErr] = useState('')

  async function publish(): Promise<void> {
    if (!file) return
    setBusy(true)
    setErr('')
    try {
      const s = slug.trim() || file.name.replace(/\.[^.]+$/, '').toLowerCase().replace(/[^a-z0-9_-]+/g, '-')
      setOut(await uploadAndPublish(conn, file, kind, s, name.trim() || s))
    } catch (e) {
      setErr(String(e))
    } finally {
      setBusy(false)
    }
  }

  return (
    <section className="card">
      <header className="card-head">
        <h4>② 发布制品</h4>
        <span className="kbd-hint">skill / plugin 要传 rsi3d CLI 打好的包</span>
      </header>

      <div className="field">
        <label>类型</label>
        <select value={kind} onChange={(e) => setKind(e.target.value)}>
          <option value="bizpack">业务包 bizpack</option>
          <option value="harness">Harness（能力声明）</option>
          <option value="skill">技能 skill（包）</option>
          <option value="plugin">插件 plugin（包）</option>
          <option value="benchmark">评测基准 benchmark</option>
          <option value="asset">资产 asset</option>
        </select>
      </div>
      <div className="field">
        <label>slug（留空从文件名推）</label>
        <input value={slug} onChange={(e) => setSlug(e.target.value)} spellCheck={false} />
      </div>
      <div className="field">
        <label>展示名</label>
        <input value={name} onChange={(e) => setName(e.target.value)} />
      </div>
      <div className="field">
        <label>文件</label>
        <input type="file" onChange={(e) => setFile(e.target.files?.[0] ?? null)} />
      </div>

      <button className="btn solid full" disabled={busy || !file} onClick={() => void publish()}>
        {busy ? '上传中…' : '上传并发布'}
      </button>

      {kind === 'skill' || kind === 'plugin' ? (
        <div className="file-note">
          包由 CLI 打包（确定性 zip + 清单校验）：
          <code>rsi3d publish --dir &lt;目录&gt; --kind {kind}</code>
        </div>
      ) : null}
      {err ? <div className="result err">{err}</div> : null}
      {out ? (
        <div className="result">
          已发布 <span className="harness-id">{out.ref}</span>（{out.kind} · {out.version}）
        </div>
      ) : null}
    </section>
  )
}

// ---------------------------------------------------------------- ③ 检索与消费

function SearchCard({ conn }: { conn: Rsi3dConn }) {
  const [q, setQ] = useState('')
  const [kind, setKind] = useState('')
  const [tab, setTab] = useState<'artifact' | 'harness'>('harness')
  const [arts, setArts] = useState<ArtifactView[]>([])
  const [har, setHar] = useState<HarnessView[]>([])
  const [busy, setBusy] = useState(false)
  const [err, setErr] = useState('')
  const [intent, setIntent] = useState('')
  const [picked, setPicked] = useState<HarnessView | null>(null)
  const [run, setRun] = useState<RunView | null>(null)

  async function go(): Promise<void> {
    setBusy(true)
    setErr('')
    try {
      if (tab === 'harness') setHar(await harnesses(conn, q))
      else setArts(await search(conn, q, kind))
    } catch (e) {
      setErr(String(e))
    } finally {
      setBusy(false)
    }
  }

  async function runOnce(h: HarnessView): Promise<void> {
    setErr('')
    setRun(null)
    try {
      const r = await startRun(conn, h.id, intent.trim() || '（未填意图）')
      // 登记 ≠ 执行：真正的循环在客户侧 Runtime 跑；这里只轮询控制面记录
      const poll = async (left: number): Promise<void> => {
        const v = await runStatus(conn, r.id)
        setRun(v)
        if (left > 0 && (v.status === 'running' || v.iterations === 0)) {
          window.setTimeout(() => void poll(left - 1), 3000)
        }
      }
      await poll(10)
    } catch (e) {
      setErr(String(e))
    }
  }

  return (
    <section className="card">
      <header className="card-head">
        <h4>③ 检索与消费</h4>
        <span className="kbd-hint">对 Harness 发起一次 Run（登记 + 看曲线）</span>
      </header>

      <div className="field">
        <label>搜索</label>
        <input
          value={q}
          onChange={(e) => setQ(e.target.value)}
          placeholder="例如：家居 / 文生 3D / @ns/slug"
          onKeyDown={(e) => {
            if (e.key === 'Enter') void go()
          }}
        />
      </div>
      <div className="row">
        <button className={'btn ghost sm' + (tab === 'harness' ? ' solid' : '')} onClick={() => setTab('harness')}>
          Harness
        </button>
        <button className={'btn ghost sm' + (tab === 'artifact' ? ' solid' : '')} onClick={() => setTab('artifact')}>
          制品
        </button>
        {tab === 'artifact' ? (
          <select value={kind} onChange={(e) => setKind(e.target.value)}>
            <option value="">全部类型</option>
            <option value="bizpack">bizpack</option>
            <option value="skill">skill</option>
            <option value="plugin">plugin</option>
            <option value="benchmark">benchmark</option>
          </select>
        ) : null}
        <button className="btn solid sm" onClick={() => void go()} disabled={busy}>
          {busy ? '查询中…' : '搜索'}
        </button>
      </div>

      <div className="field">
        <label>意图（发起 Run 用）</label>
        <input value={intent} onChange={(e) => setIntent(e.target.value)} placeholder="3 米挑高客厅，北欧风，落地窗，暖光" />
      </div>

      {tab === 'harness' ? (
        <div className="results">
          {har.length === 0 ? <div className="empty">还没有结果。点「搜索」。</div> : null}
          {har.map((h) => (
            <div className="result" key={h.id}>
              <div className="results-head">
                <span className="harness-id">{h.ref || h.id}</span>
                <span className="chip">{h.protocol}</span>
                <span className="chip">信誉 {h.hub_score?.toFixed?.(1) ?? h.hub_score}</span>
                {h.verified ? <span className="chip">已验证</span> : null}
                <div className="row-actions">
                  <button className="btn ghost sm" onClick={() => setPicked(h)}>
                    详情
                  </button>
                  <button className="btn solid sm" onClick={() => void runOnce(h)}>
                    跑一次
                  </button>
                </div>
              </div>
              <div className="app-desc">
                {h.title} · {h.domain} · {h.capabilities.join(' / ')}
              </div>
              {picked?.id === h.id ? <div className="harness-id">端点 {h.endpoint}</div> : null}
            </div>
          ))}
        </div>
      ) : (
        <div className="results">
          {arts.length === 0 ? <div className="empty">还没有结果。点「搜索」。</div> : null}
          {arts.map((a) => (
            <div className="result" key={a.id}>
              <div className="results-head">
                <span className="harness-id">{a.ref}</span>
                <span className="chip">{a.kind}</span>
                <span className="chip">{a.version}</span>
              </div>
              <div className="app-desc">{a.summary || a.name}</div>
            </div>
          ))}
        </div>
      )}

      {err ? <div className="result err">{err}</div> : null}
      {run ? (
        <div className="result">
          Run <span className="harness-id">{run.id}</span> · {run.status} · 最佳{' '}
          {(run.best_score ?? 0).toFixed(2)} · {run.iterations} 轮
          {run.score_curve?.length ? <div className="scorebar">{sparkline(run.score_curve)}</div> : null}
          <div className="file-note">
            登记 ≠ 执行：跑循环的是你侧的 Runtime（直连 Harness，3D 数据不过平台）。
          </div>
        </div>
      ) : null}
    </section>
  )
}

// ---------------------------------------------------------------- ④ 最近 Run

function RunsCard({ conn }: { conn: Rsi3dConn }) {
  const [runs, setRuns] = useState<RunView[]>([])
  const [err, setErr] = useState('')

  useEffect(() => {
    recentRuns(conn)
      .then(setRuns)
      .catch((e: unknown) => setErr(String(e)))
  }, [conn])

  return (
    <section className="card">
      <header className="card-head">
        <h4>④ 最近 Run</h4>
        <span className="kbd-hint">只在控制面看摘要</span>
      </header>

      {err ? <div className="result err">{err}</div> : null}
      {runs.length === 0 && !err ? <div className="empty">还没有 Run。</div> : null}
      {runs.map((r) => (
        <div className="row" key={r.id}>
          <span className="harness-id">{r.id}</span>
          <span className="chip">{r.status}</span>
          <span className="chip">最佳 {(r.best_score ?? 0).toFixed(2)}</span>
          <span className="chip">{r.iterations} 轮</span>
        </div>
      ))}

      <div className="file-note">
        要管理用户 / 组织 / 计费，请到{' '}
        <a href={conn.baseUrl} target="_blank" rel="noreferrer">
          Web 控制台
        </a>
        —— 插件不做管理后台的复制品。
      </div>
    </section>
  )
}
