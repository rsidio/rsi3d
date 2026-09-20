// rsi3d-harness 浏览器客户端：两条流并排，把设计意图直接摆在屏幕上。
//
//   左：场景流（glTF 快照 + 增量）→ 本地 three.js 渲染，随便转，不花服务端算力
//   右：图像流（PNG 帧）        → 服务端渲染的权威观测，旁边挂着遮挡率等测量值
//
// 为什么两路都连：它们不是"二选一"，而是两种代价/两种用途。
// 场景流带得走（资产到了客户端），图像流带得走结论（可复现、能当证据）。

const qs = new URLSearchParams(location.search);
let TOKEN = qs.get('token') || '';
let view = qs.get('view') || 'iso-sw';

// ---------------------------------------------------------------- 能力声明
//
// 借 glow 的纪律：**能力是声明出来的，不是被假设的**（它把 `supported_extensions()`
// 放进 `HasContext` trait，于是 native 与 WebGL 两个后端都必须回答"你支持什么"）。
// 我们原来只有服务端声明自己（`welcome.geometry`），客户端能干什么全靠猜——结果
// "CDN 被拦住了"、"GPU 上下文丢了"这类事，服务端**完全看不见**。
//
// 声明走订阅 URL 的查询参数：一条 SSE 连接就是一次订阅，它是唯一天然带"连接身份"
// 的位置（POST 那条通道服务端分不清是谁发的）。
// 规则只有一条：**声明变了就断开重连并重新声明**——所以服务端的名册永远是真的，
// 不需要"更新"这种半新半旧的状态。

const AGENT = 'rsi3d-web/0.1.0';

/// WebGL 档位：同步探测一次就记住（拿不到就当没有，不猜）
let webglProbe;
function webglTier() {
  if (webglProbe === undefined) {
    webglProbe = null;
    try {
      const c = document.createElement('canvas');
      if (c.getContext('webgl2')) webglProbe = 'webgl2';
      else if (c.getContext('webgl') || c.getContext('experimental-webgl')) webglProbe = 'webgl1';
    } catch (e) { /* 探测本身也可能抛——当作没有 */ }
  }
  return gpuLost ? null : webglProbe;
}

/// GPU 上下文丢了？（驱动重置/休眠/切后台都会发生，不是异常情况）
let gpuLost = false;

/// 当下**真的**能做到什么（不是"构建里有没有这段代码"）
function caps() {
  const list = [];
  const tier = webglTier();
  if (tier) list.push(tier);
  if (three && renderer && !gpuLost) list.push('three');
  list.push('scene', 'image', 'context-loss');
  return list;
}

/// 把声明拼进订阅地址（调用点已经带了 `?token=…`）
function withDeclaration(path) {
  const c = caps().join(',');
  return `${path}&agent=${encodeURIComponent(AGENT)}&cap=${encodeURIComponent(c)}`;
}

/// 服务端从我们声明里**派生出的**档位（welcome 回声回来的）
let serverTier = '';

function paintCaps() {
  const el = $('caps');
  if (!el) return;
  const c = caps();
  el.textContent = c.length ? c.join('·') : '–';
  el.className = gpuLost ? 'bad' : '';
  el.title = '客户端自报的能力（服务端会记账）；声明变了就重连重新声明'
    + (serverTier ? `\n服务端理解的档位：${serverTier}` : '');
}

/// 图像流的显示预算：这个 pane 有多大就报多大。
/// 服务端只会**往下调**（等比缩到框内）——不会超过它自己的默认档。
/// 为什么该报：手机端不必收 480×360 的帧，4K 屏也不必被卡在这个尺寸。
function frameBudget() {
  const img = $('frame');
  let w = Math.round(img?.clientWidth || 0);
  let h = Math.round(img?.clientHeight || 0);
  // 判定给的像素上限只**缩**不放（与"limits 只缩不放"是同一条纪律）
  if (capPx && capPx[0] > 0 && capPx[1] > 0) {
    const k = Math.min(1, Math.min(capPx[0] / Math.max(w, 1), capPx[1] / Math.max(h, 1)));
    if (k < 1) { w = Math.round(w * k); h = Math.round(h * k); }
  }
  return w > 32 && h > 32 ? `${w}x${h}` : '';
}

// ---------------------------------------------------------------- 渲染能力：探测 → 上报 → 应用
//
// 这一段补的是「声明」答不了的两个问题：**这台机器胜任吗**、**不胜任该怎么办**。
//
//   探测（读公开参数 + 可选 2 秒微基准）
//     → 上报：先问平台（rsi3d.com，权威），不可达就问**自己这台服务**（离线兜底）
//     → 应用：像素预算 / 帧率 / 几何档 / 订阅哪条流
//
// 三条纪律：
// ① **声明是事实，模式是决定**——`cap` 说的仍是"浏览器能干什么"，不因为被降档而改口；
// ② **未知不等于不行**——读不到的参数不会把人判低档（只有"渲染所需的 GPU 接口未知"
//    才保守处理，那是服务端的规则）；
// ③ **拿不到判定就不挡路**——判定失败只是没有限制，不是白屏。

// three.js 就绪信号：判定**最多等它 1.2 秒**——等得到就带上微基准，
// 等不到（CDN 被拦/慢）就不带（服务端会在理由里标注"可能偏乐观"）。
// 关键：判定**不许被 CDN 挡住**——"客户端渲不了"恰恰是最需要判定的时候。
let threeReadyResolve = () => {};
const threeReady = new Promise((r) => { threeReadyResolve = r; });

let verdict = null;          // 服务端/本地给出的判定
let policyCache = null;      // /contract/render-policy.json（离线规则 + 覆盖模式时查表用）
let allowRealMesh = true;    // 几何档：判定说 aabb-proxy 就不去取 side-car
let allowAnimation = true;   // 低档就不要再跑持续动画（把 GPU 让给必要的东西）

/// 短哈希：型号默认只上报哈希（脱敏），要明文得显式 `?hw=plain`。
async function hashShort(text) {
  try {
    const buf = new TextEncoder().encode(text);
    const d = await crypto.subtle.digest('SHA-256', buf);
    return Array.from(new Uint8Array(d)).slice(0, 6).map((b) => b.toString(16).padStart(2, '0')).join('');
  } catch {
    return 'unavailable';
  }
}

/// 读**公开**的主机参数。不主动去猜、不去探测隐私（不读字体/指纹类接口）。
async function probeHost() {
  const out = {
    agent: AGENT,
    form: 'screen',
    gpu: { api: 'none', webgpu: false, max_texture: 0 },
    cpu: { cores: navigator.hardwareConcurrency || 0 },
    display: {
      viewport: [Math.round(innerWidth), Math.round(innerHeight)],
      dpr: devicePixelRatio || 1,
      refresh_hz: null,
    },
    privacy: qs.get('hw') === 'plain' ? 'plaintext' : 'hashed',
  };
  // platform：粗略的平台名（macos/windows/linux），不含版本，够判规则了
  const ua = navigator.userAgent || '';
  out.cpu.platform = /Mac/i.test(ua) ? 'macos' : /Win/i.test(ua) ? 'windows' : /Linux|X11/i.test(ua) ? 'linux' : 'unknown';
  if (navigator.deviceMemory) out.cpu.memory_gb = navigator.deviceMemory;

  // WebGPU：有就报（它比 WebGL2 更强，但我们的客户端渲染仍走 three/WebGL）
  try {
    out.gpu.webgpu = !!navigator.gpu;
  } catch { /* 没有就没有 */ }

  // WebGL 事实：接口档位 + 最大纹理 + 型号（型号默认只留哈希）
  try {
    const c = document.createElement('canvas');
    const gl2 = c.getContext('webgl2');
    const gl = gl2 || c.getContext('webgl') || c.getContext('experimental-webgl');
    if (gl) {
      out.gpu.api = gl2 ? 'webgl2' : 'webgl1';
      out.gpu.max_texture = gl.getParameter(gl.MAX_TEXTURE_SIZE) || 0;
      const dbg = gl.getExtension('WEBGL_debug_renderer_info');
      if (dbg) {
        const name = gl.getParameter(dbg.UNMASKED_RENDERER_WEBGL) || '';
        const vendor = gl.getParameter(dbg.UNMASKED_VENDOR_WEBGL) || '';
        if (name) {
          out.gpu.renderer_hash = await hashShort(name);
          if (out.privacy === 'plaintext') out.gpu.renderer = name;
          // 软件光栅：远程桌面/虚拟机/关掉硬件加速时最常见的那几种
          out.gpu.software = /swiftshader|llvmpipe|software|basic render/i.test(name);
        }
        if (vendor) out.gpu.vendor_hash = await hashShort(vendor);
      }
      gl.getExtension('WEBGL_lose_context')?.loseContext?.(); // 探完就还回去，别占着一个上下文
    }
  } catch { /* 探测失败就留 unknown，服务端会保守处理 */ }
  return out;
}

/// 微基准：**持续**帧率（不是首帧峰值）。固定 480×320 的离屏画布，
/// 画一个高多边形物体 + 一个大平面（同时压顶点与填充率），跑 `ms` 毫秒。
async function benchHost(ms = 1500) {
  if (!three) return null;
  const canvas = document.createElement('canvas');
  canvas.width = 480;
  canvas.height = 320;
  let r = null;
  let scene = null;
  try {
    r = new three.WebGLRenderer({ canvas, antialias: false, powerPreference: 'high-performance' });
  } catch {
    return null;
  }
  scene = new three.Scene();
  const cam = new three.PerspectiveCamera(50, 480 / 320, 0.1, 100);
  cam.position.set(0, 0, 6);
  const heavy = new three.Mesh(
    new three.TorusKnotGeometry(1.2, 0.42, 220, 32),
    new three.MeshStandardMaterial({ color: 0x8899aa, roughness: 0.8 })
  );
  scene.add(heavy);
  scene.add(new three.HemisphereLight(0xffffff, 0x222222, 1.2));
  const plane = new three.Mesh(
    new three.PlaneGeometry(40, 40),
    new three.MeshStandardMaterial({ color: 0x334455 })
  );
  plane.position.z = -6;
  scene.add(plane);

  const t0 = performance.now();
  let frames = 0;
  await new Promise((done) => {
    const step = () => {
      const t = performance.now() - t0;
      heavy.rotation.y += 0.05;
      heavy.rotation.x += 0.02;
      r.render(scene, cam);
      frames++;
      if (t >= ms) done();
      else requestAnimationFrame(step);
    };
    requestAnimationFrame(step);
  });
  const elapsed = performance.now() - t0;
  const tris = heavy.geometry.attributes.position.count / 3;
  const res = {
    sustained_fps: +(frames / (elapsed / 1000)).toFixed(1),
    frames,
    fillrate_mpx: +(((480 * 320 * frames) / 1e6) / (elapsed / 1000)).toFixed(1),
    triangles_mps: +((tris * frames / 1e6) / (elapsed / 1000)).toFixed(1),
    ms: Math.round(elapsed),
  };
  try { r.dispose(); } catch { /* 释放失败不影响判定 */ }
  return res;
}

/// 拿规则表（自己服务上的那份，**离线也能拿**）：只在覆盖模式 / 展示规则版本时用。
async function loadPolicy() {
  if (policyCache) return policyCache;
  try {
    const res = await fetch(`/contract/render-policy.json?token=${encodeURIComponent(TOKEN)}`);
    if (res.ok) policyCache = await res.json();
  } catch { /* 拿不到就没有 */ }
  return policyCache;
}

/// 给平台的**硬预算**（毫秒）。
///
/// 为什么要这个：实测踩过——平台域名在某些网络里既不通也不报错，就**挂着**，
/// 结果把本地兜底一起拖住，界面上永远显示"还没判定"。判定是可以离线做的，
/// 所以平台只给一小段时间：答不上来就用本机（同一份规则表），并**说出来**。
const PLATFORM_BUDGET_MS = 1200;

/// 探测 → 上报 → 判定。**任何一步失败都不挡路**：没有判定只是没有限制。
async function assessHost() {
  if (qs.get('no-assess') === '1') return null;   // 测试/冒烟用：保持老行为
  const platform = (qs.get('platform') || 'https://rsi3d.com').replace(/\/$/, '');
  let profile;
  try {
    profile = await probeHost();
  } catch (e) {
    note(`主机探测失败（${e.message}）：这次不做判定，按默认档跑`);
    return null;
  }
  // 微基准要 three：等一小会儿，等不到就按硬件参数判（不许无限等 CDN）
  if (!three) {
    await Promise.race([threeReady, new Promise((r) => setTimeout(r, 1200))]);
  }
  if (qs.get('bench') !== '0' && three) {
    try {
      const b = await benchHost();
      if (b) profile.bench = b;
    } catch { /* 基准失败就按硬件参数判，服务端会标注"可能偏乐观" */ }
  }

  // 显式覆盖（测试/排障用）：直接查表，不问任何人——权威标成 declared
  const forced = qs.get('mode');
  if (forced) {
    const pol = await loadPolicy();
    const rule = pol?.modes?.find((m) => m.mode === forced);
    if (rule) {
      return { mode: rule.mode, limits: rule.limits, reasons: [`显式指定 ?mode=${forced}`], missing: [], fallbacks: [], authority: 'declared', policy_version: pol.version };
    }
    note(`?mode=${forced} 在规则表里没有这一档，忽略`);
  }

  // ① 平台（权威）：它可能掌握本地看不到的信号（账号档、聚合实测）
  const why = [];   // 失败就说清楚**为什么**（不许静默：这正是"判定不可用"被忽略过一次的原因）
  try {
    const res = await fetch(`${platform}/api/render/report?src=web`, {
      method: 'POST',
      headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify(profile),
      signal: AbortSignal.timeout
        ? AbortSignal.timeout(PLATFORM_BUDGET_MS)
        : undefined,
    });
    if (res.ok) {
      const j = await res.json();
      const v = j.verdict || j;
      if (v && v.mode) {
        v.authority = v.authority || 'platform';
        return v;
      }
      why.push(`平台答的不是判定（缺 mode）`);
    } else {
      why.push(`平台 HTTP ${res.status}`);
    }
  } catch (e) {
    why.push(
      e.name === 'TimeoutError'
        ? `平台 ${PLATFORM_BUDGET_MS}ms 内没答上来`
        : `平台不可达（${e.message || e.name}）`
    );
  }

  // ② 离线兜底：问自己这台服务（它与平台用的是**同一份规则表**，只是求值在本地）
  try {
    const res = await fetch(`/capability?token=${encodeURIComponent(TOKEN)}`, {
      method: 'POST',
      headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify(profile),
    });
    if (res.ok) {
      const j = await res.json();
      if (j.verdict) {
        // 用本机判定就要说清**为什么没用平台**（不冒充权威，也不静默换口径）。
        // 单独一个字段：reasons 只上屏前三条，塞进去反而会被挤掉。
        j.verdict.authority_note = why.join('；');
        return j.verdict;
      }
      why.push('本机答的不是判定（缺 verdict）');
    } else {
      why.push(`本机 HTTP ${res.status}`);
    }
  } catch (e) {
    why.push(`本机判定失败（${e.message || e.name}）`);
  }
  // 失败要**看得见**：写进判定区（流自己的提示会覆盖 scene-note，之前就这么被盖掉过一次）
  verdictNote(`这次没有判定：${why.join('；')}。按默认档跑，不影响你继续看场景。`);
  return null;
}

/// 把判定变成**实际行为**：这是"不同主机不同调用方式"落地的地方。
async function applyVerdict(v) {
  if (!v) return;
  verdict = v;
  const lim = v.limits || {};
  allowRealMesh = lim.geometry !== 'aabb-proxy';
  allowAnimation = lim.allow_animation !== false;

  // 几何档：判定说只画代理，就把已经取回来的真网格放掉
  if (!allowRealMesh && meshData) {
    meshData = null;
    sidecarTried = true;
    sidecarNote = '判定为低档：只用包围盒代理';
    for (const [id, m] of meshes) { m.geometry = boxGeometry(); m.position.set(...(m.userData.node?.translation || [0, 0, 0])); }
    renderMeshNote();
  }

  // 像素预算：判定给的是**上限**，与客户端自己报的预算取更小者。
  // 注意：判定可能**晚于**首连（它就并行在 three 那边跑），所以已经连上的帧流要重连一次
  // 才会带上新预算——不然"像素预算收紧"就只是显示出来了而已。
  if (lim.max_px && lim.max_px[0] > 0) {
    capPx = lim.max_px;
    if (frameES) connectFrame();
  }

  // 订阅哪条流：判定说只看图像流，就把场景流关掉（并把左侧说明白）
  if (!sceneStreamWanted()) sceneStreamOff();
  paintVerdict();
}

let capPx = null; // 判定给的像素上限（与 frameBudget() 取小）

/// 判定失败/兜底说明也上屏——**别让"没有判定"看起来像"还没开始"**。
function verdictNote(text) {
  const box = $('verdict');
  if (box) {
    box.textContent = text;
    box.className = 'gpu-note';
  }
  const el = $('sc-mode');
  if (el) {
    el.textContent = '未判定';
    el.title = text;
    el.className = 'bad';
  }
}

/// 判定结果上屏：档位、谁判的、为什么、缺什么、出路（**带链接**）。
function paintVerdict() {
  const el = $('sc-mode');
  if (el) {
    el.textContent = verdict ? verdict.mode : '–';
    el.title = verdict
      ? `${verdict.authority === 'platform' ? 'rsi3d.com 判定' : verdict.authority === 'declared' ? '显式指定' : '本机离线判定'}（规则表 ${verdict.policy_version}）`
      : '还没判定';
    el.className = verdict && verdict.mode !== 'client-full' ? 'bad' : '';
  }
  const box = $('verdict');
  if (!box) return;
  if (!verdict) { box.textContent = ''; return; }
  const why = verdict.reasons.slice(0, 3).map((r) => `· ${r}`).join('\n');
  const miss = verdict.missing.length ? `\n差一点就能更强：${verdict.missing.join('；')}` : '';
  const ways = verdict.fallbacks.map((f) => `→ ${f.title}：${f.what}`).join('\n');
  // 本机判定要交代"平台为什么没给答案"——不然用户会以为平台一直是这个口径
  const src = verdict.authority_note
    ? `\n· 平台没给答案（${verdict.authority_note}），本地按同一份规则表算的`
    : '';
  box.textContent =
    `渲染模式 ${verdict.mode}（${verdict.authority} · 规则表 ${verdict.policy_version}）${src}\n${why}${miss}`
    + (ways ? `\n${ways}` : '');
  box.className = 'gpu-note' + (verdict.mode === 'client-full' ? ' ok' : ' bad');
  // 出路带链接时用可点的形式（上面的 textContent 已经给了纯文本版）
  if (verdict.fallbacks.some((f) => f.link)) {
    box.innerHTML = '';
    box.append(document.createTextNode(`渲染模式 ${verdict.mode}（${verdict.authority} · 规则表 ${verdict.policy_version}）${src}\n`));
    box.append(document.createTextNode(`${why}${miss}\n`));
    for (const f of verdict.fallbacks) {
      const line = document.createElement('div');
      line.textContent = `→ ${f.title}：${f.what}`;
      if (f.link) {
        const a = document.createElement('a');
        a.href = f.link;
        a.target = '_blank';
        a.rel = 'noopener';
        a.textContent = ' 看说明';
        line.append(a);
      }
      box.append(line);
    }
  }
}

const $ = (id) => document.getElementById(id);
const hex = (s) => (s || '').slice(0, 8);

// ---------------------------------------------------------------- 令牌
if (!TOKEN) $('gate').classList.add('show');
$('token-go').onclick = () => {
  TOKEN = $('token-input').value.trim();
  if (!TOKEN) return;
  history.replaceState(null, '', '?token=' + encodeURIComponent(TOKEN));
  $('gate').classList.remove('show');
  boot();
};
$('token-input').addEventListener('keydown', (e) => { if (e.key === 'Enter') $('token-go').click(); });

// ---------------------------------------------------------------- 上行
async function post(path, body) {
  const res = await fetch(`${path}?token=${encodeURIComponent(TOKEN)}`, {
    method: 'POST',
    headers: { 'Content-Type': 'application/json' },
    body: JSON.stringify(body),
  });
  let out = {};
  try { out = await res.json(); } catch { /* 空体 */ }
  return { status: res.status, out };
}

// 命令一律带 reason：它会进内核的归因表（谁改的、为什么改）。
const ACTIONS = {
  'move-out': () => ({ op: 'transform', target: 'obj:sofa_01',
    params: { translate: [0, 0, 1.3] }, reason: '沙发挡住窗户采光，挪出挡光带' }),
  'move-back': () => ({ op: 'transform', target: 'obj:sofa_01',
    params: { translate: [0, 0, -1.3] }, reason: '挪回窗下（对照实验）' }),
  'dim': () => ({ op: 'set_light', target: 'sun', params: { intensity: 1.0 },
    reason: '主光过曝，压到 1.0' }),
  'undo': () => ({ op: 'checkout', params: { rev: Math.max(0, localRev - 1) },
    reason: '撤销一步（跳到上一版的完整状态）' }),
  'rollback': () => ({ op: 'checkout', params: { rev: 1 },
    reason: '回滚到 rev 1：只挪沙发那一版' }),
};

// ---------------------------------------------------------------- 场景流
let localRev = 0;
let patches = 0, snapshots = 0, frames = 0;
let three = null, renderer = null, scene3 = null, camera3 = null, meshes = new Map();
let windowPlane = null;

// ---- 几何 side-car（导入来的资产的真网格）------------------------------------
//
// 规矩与整条链一致：**只换"画什么"，不换"证据是什么"**。服务端光栅依旧画包围盒代理，
// 这里拿到真网格就画真网格，拿不到就还是盒子（不白屏、不静默）。
let meshData = null; // Map<节点 id, {positions, indices}>
let sidecarGeo = new Map(); // 同一个 id 只建一次 BufferGeometry
let sidecarTried = false;
let sidecarNote = null;

/// 解析 GLB（只用我们 side-car 里会出现的那一小块：一个 buffer + BIN chunk）。
function parseGlb(buf) {
  const dv = new DataView(buf);
  if (buf.byteLength < 12 || dv.getUint32(0, true) !== 0x46546c67) throw new Error('不是 GLB');
  let off = 12, json = null, binStart = -1;
  while (off + 8 <= buf.byteLength) {
    const len = dv.getUint32(off, true);
    const type = dv.getUint32(off + 4, true);
    const body = new Uint8Array(buf, off + 8, len);
    if (type === 0x4e4f534a) json = JSON.parse(new TextDecoder().decode(body));
    // accessor 里的偏移是相对 **BIN chunk** 的，不是相对文件：这里得记住绝对起点
    else if (type === 0x004e4942) binStart = off + 8;
    off += 8 + len; // chunk 长度按规范含补齐字节
    if (json && binStart >= 0) break;
  }
  if (!json || binStart < 0) throw new Error('GLB 缺 JSON 或 BIN chunk');

  const views = json.bufferViews || [];
  const accs = json.accessors || [];
  const TYPES = { SCALAR: 1, VEC2: 2, VEC3: 3, VEC4: 4 };
  const read = (ai) => {
    const a = accs[ai];
    if (!a) throw new Error('缺 accessor ' + ai);
    const v = views[a.bufferView] || {};
    const comps = TYPES[a.type] || 0;
    const start = binStart + (v.byteOffset || 0) + (a.byteOffset || 0);
    const n = a.count * comps;
    if (start + n * 4 > buf.byteLength) throw new Error('accessor ' + ai + ' 越界');
    const out = new Float32Array(n);
    for (let i = 0; i < n; i++) out[i] = dv.getFloat32(start + i * 4, true);
    return out;
  };
  const readIdx = (ai) => {
    if (ai === undefined || ai === null) return null;
    const a = accs[ai];
    const v = views[a.bufferView] || {};
    const start = binStart + (v.byteOffset || 0) + (a.byteOffset || 0);
    const out = new Uint32Array(a.count);
    for (let i = 0; i < a.count; i++) {
      if (a.componentType === 5121) out[i] = dv.getUint8(start + i);
      else if (a.componentType === 5123) out[i] = dv.getUint16(start + i * 2, true);
      else out[i] = dv.getUint32(start + i * 4, true);
    }
    return out;
  };

  const out = new Map();
  for (const node of json.nodes || []) {
    if (node.mesh === undefined || !node.name) continue;
    const prims = json.meshes?.[node.mesh]?.primitives || [];
    const pos = [], idx = [];
    let base = 0;
    for (const p of prims) {
      const pv = read(p.attributes.POSITION);
      pos.push(pv);
      const pi = readIdx(p.indices);
      if (pi) { for (const i of pi) idx.push(i + base); }
      else { for (let i = 0; i < pv.length / 3; i++) idx.push(i + base); }
      base += pv.length / 3;
    }
    if (!pos.length) continue;
    const merged = new Float32Array(base * 3);
    let at = 0;
    for (const pv of pos) { merged.set(pv, at); at += pv.length; }
    out.set(node.name, { positions: merged, indices: idx.length ? new Uint32Array(idx) : null });
  }
  return out;
}

/// 去把 side-car 取回来（只在真的有节点声明了 `mesh_ref` 时才叫）。
async function loadSidecar() {
  if (sidecarTried) return;
  if (!three) return; // three 还没就绪：先不记"试过了"，等下一帧
  sidecarTried = true;
  try {
    const res = await fetch(`/mesh.glb?token=${encodeURIComponent(TOKEN)}`);
    if (!res.ok) throw new Error('HTTP ' + res.status);
    meshData = parseGlb(await res.arrayBuffer());
    sidecarNote = meshData.size ? '真网格 ' + meshData.size + ' 个' : 'side-car 里没有节点';
    refreshGeometry(); // 快照可能**先到**：到货后得回去把已经画上去的盒子换掉
  } catch (e) {
    sidecarNote = '真网格不可用（' + e.message + '）';
  }
  const el = $('mesh-note');
  if (el) el.textContent = sidecarNote || '–';
  renderMeshNote();
}

/// 几何状态：**真网格几个 / 一共几个**。
///
/// 写成比例而不是一句话，是因为"部分成功"才是这里的常态：side-car 里没有这个节点、
/// 或者 side-car 根本没到，都会退回包围盒代理——不写清楚就只能靠猜。
function renderMeshNote() {
  const el = $('mesh-note');
  if (!el) return;
  const total = meshes.size;
  const real = [...meshes.values()].filter((m) => m.userData.real).length;
  if (real > 0) el.textContent = `真网格 ${real}/${total}`;
  else if (sidecarTried) el.textContent = sidecarNote || '–';
}

function geometryFor(id) {
  if (!meshData || !three) return null;
  if (sidecarGeo.has(id)) return sidecarGeo.get(id);
  const d = meshData.get(id);
  if (!d) return null;
  const g = new three.BufferGeometry();
  g.setAttribute('position', new three.Float32BufferAttribute(d.positions, 3));
  if (d.indices) g.setIndex(new three.BufferAttribute(d.indices, 1));
  g.computeVertexNormals();
  sidecarGeo.set(id, g);
  return g;
}
let room = [4.2, 2.8, 6.0];
let orbit = { az: -0.9, el: 0.45, dist: 8 };

async function loadThree() {
  // 从 CDN 取；取不到（离线/被拦）就**明确降级**到图像流（而不是白屏）。
  // 一定要带超时：CDN 卡住时不能把整个客户端拖死。
  const timeout = new Promise((_, reject) => setTimeout(() => reject(new Error('timeout')), 4000));
  try {
    const mod = await Promise.race([
      import('https://unpkg.com/three@0.160.0/build/three.module.js'),
      timeout,
    ]);
    return mod;
  } catch (e) {
    console.warn('three.js 不可用：', e.message);
    return null;
  }
}

function cameraFromOrbit() {
  const d = orbit.dist;
  const x = d * Math.cos(orbit.el) * Math.cos(orbit.az);
  const y = d * Math.sin(orbit.el);
  const z = d * Math.cos(orbit.el) * Math.sin(orbit.az);
  camera3.position.set(x, y, z);
  camera3.lookAt(0, 1.0, 0);
}

function colorOf(node) {
  const c = node?.extras?.rsi3d?.color;
  if (Array.isArray(c) && c.length === 3) {
    return (c[0] << 16) | (c[1] << 8) | c[2];
  }
  return 0x9aa4b2;
}

function upsertNode(node) {
  if (!three || !scene3) return;
  const id = node.name;
  let mesh = meshes.get(id);
  if (!mesh) {
    mesh = new three.Mesh(
      boxGeometry(),
      new three.MeshStandardMaterial({ color: colorOf(node), roughness: 0.85, metalness: 0 })
    );
    scene3.add(mesh);
    meshes.set(id, mesh);
  }
  mesh.userData.node = node;
  mesh.userData.ref = node?.extras?.rsi3d?.mesh_ref || null;
  if (mesh.userData.ref && allowRealMesh) loadSidecar(); // 懒加载：没导入资产、或被判定为低档，就不发这一趟请求
  applyGeometry(id, mesh);
  mesh.material.color.setHex(colorOf(node));
  renderMeshNote();
}

/// 一个盒子几何体就够（之前每个节点都新建一个，白占显存）。
let boxGeo = null;
function boxGeometry() {
  if (!boxGeo) boxGeo = new three.BoxGeometry(1, 1, 1);
  return boxGeo;
}

/// 决定这个节点画**真网格**还是**包围盒代理**。
///
/// 两者只能二选一，不允许"一半真一半假"：真网格的坐标已经是世界坐标（导出时烘过），
/// 所以不能再乘 AABB；代理盒则相反，靠 AABB 拉成盒子。
function applyGeometry(id, mesh) {
  const node = mesh.userData.node;
  const real = mesh.userData.ref ? geometryFor(id) : null;
  if (real) {
    if (mesh.geometry !== real) mesh.geometry = real;
    mesh.position.set(0, 0, 0);
    mesh.scale.set(1, 1, 1);
  } else {
    if (mesh.userData.real) mesh.geometry = boxGeometry();
    const [x, y, z] = node.translation;
    const [sx, sy, sz] = node.scale;
    mesh.position.set(x, y, z);
    mesh.scale.set(sx, sy, sz);
  }
  mesh.userData.real = !!real;
}

/// side-car 到货后调用：把已经画上去的盒子换成真网格。
function refreshGeometry() {
  for (const [id, mesh] of meshes) applyGeometry(id, mesh);
}

function removeNode(id) {
  const mesh = meshes.get(id);
  if (mesh) { scene3.remove(mesh); meshes.delete(id); }
}

function buildScene() {
  if (!three || renderer) return;
  const canvas = $('c');
  renderer = new three.WebGLRenderer({ canvas, antialias: true, alpha: false });
  watchGpu(canvas);
  renderer.setPixelRatio(Math.min(devicePixelRatio, 2));
  renderer.setSize(canvas.clientWidth, canvas.clientHeight, false);
  scene3 = new three.Scene();
  scene3.background = new three.Color(0x0e1116);
  camera3 = new three.PerspectiveCamera(45, canvas.clientWidth / canvas.clientHeight, 0.1, 200);

  scene3.add(new three.HemisphereLight(0xdfe8f5, 0x2a3038, 1.1));
  const dir = new three.DirectionalLight(0xfff0dd, 1.4);
  dir.position.set(3, 6, -4);
  scene3.add(dir);

  // 地面网格：给"房间"一个可读的空间参照
  scene3.add(new three.GridHelper(12, 24, 0x2b3340, 0x1c222a));
  cameraFromOrbit();
  if (!buildScene.rafStarted) {
    // 上下文恢复会重建渲染器——别让 render loop 叠起来
    buildScene.rafStarted = true;
    renderLoop();
  }

  // 本地轨道：拖拽/滚轮只动本地相机——这正是"场景流不花服务端算力"的意思
  let dragging = false, last = [0, 0];
  canvas.addEventListener('pointerdown', (e) => { dragging = true; last = [e.clientX, e.clientY]; });
  addEventListener('pointerup', () => { dragging = false; });
  addEventListener('pointermove', (e) => {
    if (!dragging) return;
    orbit.az -= (e.clientX - last[0]) * 0.008;
    orbit.el = Math.max(-0.2, Math.min(1.4, orbit.el + (e.clientY - last[1]) * 0.006));
    last = [e.clientX, e.clientY];
    cameraFromOrbit();
  });
  canvas.addEventListener('wheel', (e) => {
    e.preventDefault();
    orbit.dist = Math.max(2, Math.min(40, orbit.dist * (1 + Math.sign(e.deltaY) * 0.1)));
    cameraFromOrbit();
  }, { passive: false });

  addEventListener('resize', () => {
    if (!renderer) return;
    renderer.setSize(canvas.clientWidth, canvas.clientHeight, false);
    camera3.aspect = canvas.clientWidth / canvas.clientHeight;
    camera3.updateProjectionMatrix();
  });
}

function renderLoop() {
  // 低档（判定说 `allow_animation: false`）时不跑满帧：降到 ~5 fps 的重绘，
  // 交互仍由拖拽/命令各自触发一次 `paint()` 立刻响应。
  // 这不是"省电"的玄学——是**把 GPU 让给必要的东西**：低档机器上跑满帧
  // 会把浏览器的合成器一起拖垮，连服务端图像流都会跟着卡。
  const period = allowAnimation ? 0 : 200;
  if (period) {
    const now = performance.now();
    if (!renderLoop.last || now - renderLoop.last >= period) {
      renderLoop.last = now;
      if (renderer && scene3 && camera3) renderer.render(scene3, camera3);
    }
  } else {
    if (renderer && scene3 && camera3) renderer.render(scene3, camera3);
  }
  requestAnimationFrame(renderLoop);
}

/// GPU 上下文丢失/恢复的观察者（只装一次）。
///
/// 上下文丢失**真的会发生**（驱动重置、笔记本休眠、移动端切后台）：glow 专门有
/// `CONTEXT_LOST` 这个错误码，egui 还把它翻成人话打在屏幕上。我们原来什么都不做——
/// 画布会永远黑着，而服务端一无所知。
function watchGpu(canvas) {
  if (watchGpu.installed) return;
  watchGpu.installed = true;

  canvas.addEventListener('webglcontextlost', (e) => {
    // 不 preventDefault，浏览器就永远不会发 restored —— 这一步是必须的，不是礼仪
    e.preventDefault();
    gpuLost = true;
    gpuNote('GPU 上下文丢失（驱动重置/休眠？）：场景流已重新声明为“无 GPU”，' +
      '恢复后会自动重建并重新拉全量；这段时间看右侧图像流。', 'bad');
    redeclareScene();
  });

  canvas.addEventListener('webglcontextrestored', () => {
    gpuLost = false;
    gpuNote('GPU 上下文已恢复：重建渲染器，并重新拉一份全量场景。', 'ok');
    // 重建：three.js 的渲染器/场景/网格都绑在丢失的那个上下文上
    renderer = null; scene3 = null; windowPlane = null; meshes = new Map();
    buildScene();
    // 数据没丢（快照一直留着）：先把本地重建出来，全量到了再对一次账
    if (lastGltf) applySnapshot(lastGltf);
    redeclareScene();
  });
}

function gpuNote(text, cls) {
  const el = $('gpu-note');
  if (!el) return;
  el.textContent = text || '';
  el.className = 'gpu-note' + (cls ? ' ' + cls : '');
}

/// 能力变了 → 断开重连并**重新声明**（两条连接都重声明：它们是同一个客户端的）。
///
/// 顺带的好处：新连接不带 `Last-Event-ID` ⇒ 服务端发全量 —— 这正好是"上下文恢复后
/// 要重建场景"所需要的东西，一个机制解决两件事。
function redeclareScene() {
  paintCaps();
  if (!TOKEN) return;
  connectScene();
  // 图像流也带上同一份声明，否则名册里会看到一个客户端两套说法
  if (frameES) connectFrame();
}

/// 窗（只画位置：房间是一层壳，看得见朝向就够）。
/// 注意数据来源：**快照的 extras**，不是 welcome（welcome 里没有场景元数据）。
function drawWindow(ex) {
  if (!three || !scene3 || !ex || !ex.window) return;
  const win = ex.window;
  if (!Array.isArray(win.spanX)) return;
  if (windowPlane) { scene3.remove(windowPlane); windowPlane = null; }
  const w = Math.abs(win.spanX[1] - win.spanX[0]);
  const h = win.height || 1.6;
  windowPlane = new three.Mesh(
    new three.PlaneGeometry(w, h),
    new three.MeshBasicMaterial({ color: 0x8ecdf7, transparent: true, opacity: 0.55,
      side: three.DoubleSide })
  );
  windowPlane.position.set((win.spanX[0] + win.spanX[1]) / 2, h / 2, win.z);
  scene3.add(windowPlane);
}

function applySnapshot(gltf) {
  // 一直留着最近一份快照：上下文丢了之后要靠它把本地重建出来（数据在，丢的只是显示）
  lastGltf = gltf;
  // three 还没就绪（正在加载或加载失败）：先存着，等能画了再画。
  // **不能因此丢掉数据**——流已经收到了，丢的就只是显示。
  if (!three || !scene3) {
    pendingSnapshot = gltf;
    return;
  }
  for (const id of [...meshes.keys()]) removeNode(id);
  for (const n of gltf.nodes || []) upsertNode(n);
  const ex = gltf?.extras?.rsi3d;
  if (Array.isArray(ex?.room?.size)) room = ex.room.size;
  drawWindow(ex);
  if (ex && Array.isArray(ex.window_blockers)) {
    blockers = ex.window_blockers;
  }
  orbit.dist = Math.min(orbit.dist, Math.max(6, Math.max(...room) * 1.6));
  cameraFromOrbit();
}

function applyPatch(changes) {
  for (const id of changes.nodes_remove || []) removeNode(id);
  for (const n of changes.nodes_upsert || []) upsertNode(n);
  // 挡窗者这类**场景级事实**随增量一起更新（不能只看几何）
  if (Array.isArray(changes.blockers)) blockers = changes.blockers;
}

let sceneES = null;

/// 判定说"本机不渲"时，左侧的场景流**就不该订**——判定的意义就在这里。
///
/// 这个分支是被实测抓出来的：首连之后 `redeclareScene()` 会**无条件**重连一次，
/// 把判定刚做的"关掉场景流"又打开回来，而且流自己的提示还盖掉了判定写下的说明。
function sceneStreamOff() {
  if (sceneES) { sceneES.close(); sceneES = null; }
  const dot = $('scene-dot');
  if (dot) dot.className = 'dot off';
  const el = $('scene-note');
  if (el && verdict) {
    el.textContent =
      `判定为 ${verdict.mode}：本机不渲，左侧不再订阅场景流——右侧服务端渲染的帧才是权威观测`;
  }
}

/// 判定要求不订场景流？（`stream=frame` 只看帧，`stream=none` 什么都不订）
function sceneStreamWanted() {
  const s = verdict?.limits?.stream;
  return !(s === 'frame' || s === 'none');
}

function connectScene() {
  // 重连前先关旧的：能力变了/上下文丢了的时候，这里是同一条路径
  if (sceneES) { sceneES.close(); sceneES = null; }
  if (!sceneStreamWanted()) { sceneStreamOff(); return; }
  // 重新声明时带上 `from=本地版本`：我们**已经知道**到那一版，不必重发全量。
  // （上下文丢失后也一样成立：本地状态没丢，丢的只是 GPU 那一份。）
  // 注意判据是「有没有收到过快照」而不是 `localRev > 0`——rev 0 是合法状态。
  const from = haveSnapshot ? `&from=${localRev}` : '';
  // 这一段连接声明的是什么，回声就按它**对账**（不是按"现在"的能力）
  const declared = caps();
  const es = new EventSource(withDeclaration(
    `/stream/scene?token=${encodeURIComponent(TOKEN)}${from}`
  ));
  es.declaredCaps = declared;
  sceneES = es;
  const dot = $('scene-dot');
  es.onopen = () => { dot.className = 'dot on'; };
  es.onerror = () => {
    // EventSource 会自己重连，并带上 Last-Event-ID —— 服务端据此发"差的那一段"
    dot.className = 'dot err';
    note('连接中断，正在自动重连（会带 Last-Event-ID 续传）…');
  };
  es.addEventListener('welcome', (ev) => {
    const w = JSON.parse(ev.data);
    serverRev = w.revision;
    buildScene();
    // 服务端派生的档位：回声回来就显示，客户端不必自己猜
    serverTier = w.client_render_tier || '';
    // **对账**：服务端把这条连接听成了什么？回声对不上就是它没听懂（比如旧服务端
    // 不认 `cap`），这种事必须看得见，不能静默
    const echoed = (w.client_capabilities || []).join(',');
    if (w.client_agent && echoed !== es.declaredCaps.join(',')) {
      note(`注意：服务端记录的声明与这条连接发出的不一致（服务端：${w.client_agent} [${echoed || '空'}]）——它可能不理解能力声明`);
    } else if (w.resumed) {
      note(`已续传：你看到 rev ${localRev}，服务端在 rev ${w.revision}，只补差量`);
    } else {
      note(`几何档次：${w.geometry}（**文档与服务端光栅**里就是这个；客户端如能读到 side-car 会画真网格，看顶部「几何」）`);
    }
    paintCaps();
    paint();
  });

  es.addEventListener('snapshot', (ev) => {
    const m = JSON.parse(ev.data);
    applySnapshot(m.gltf);
    localRev = m.revision;
    haveSnapshot = true;
    snapshots++;
    $('snapshots').textContent = snapshots;
    paint();
  });
  es.addEventListener('patch', (ev) => {
    const m = JSON.parse(ev.data);
    applyPatch(m.changes || {});
    localRev = m.to;
    patches++;
    $('patches').textContent = patches;
    paint();
  });
  return es;
}

// ---------------------------------------------------------------- 图像流
let frameES = null;
function connectFrame() {
  if (frameES) { frameES.close(); frameES = null; }
  const budget = frameBudget();
  frameES = new EventSource(withDeclaration(
    `/stream/frame?token=${encodeURIComponent(TOKEN)}&view=${encodeURIComponent(view)}`
      + (budget ? `&px=${budget}` : '')
      // 判定给的帧率上限：服务端只缩不放（要更高也拿不到）
      + (verdict?.limits?.max_fps ? `&fps=${verdict.limits.max_fps}` : '')
  ));
  const dot = $('frame-dot');
  frameES.onopen = () => { dot.className = 'dot on'; };
  frameES.onerror = () => { dot.className = 'dot err'; };
  frameES.addEventListener('frame', (ev) => {
    const m = JSON.parse(ev.data);
    $('frame').src = 'data:image/png;base64,' + m.png_base64;
    frames++;
    $('f-view').textContent = m.view;
    $('f-rev').textContent = m.revision;
    $('f-hash').textContent = hex(m.image_hash);
    // 这帧是谁渲的：证据档必须自报家门（GPU 档/钩出来的画面不得冒充）
    $('f-render').textContent = m.renderer || '–';
    $('f-render').className = m.renderer === 'cpu-raster/v1' ? 'ok' : 'bad';
    $('f-count').textContent = frames;
    $('f-occ').textContent = m.band_occlusion == null
      ? '–' : (m.band_occlusion * 100).toFixed(1) + '%';
    $('f-occ').className = m.band_occlusion > 0.2 ? 'bad' : 'ok';
    serverRev = m.revision;
    paint();
  });
}

// ---------------------------------------------------------------- 显示
let serverRev = 0;
/// 收到过快照吗？（rev 0 也是合法状态，不能用 `localRev > 0` 当判据）
let haveSnapshot = false;
function paint() {
  $('rev').textContent = `${localRev} / ${serverRev}`;
  const behind = localRev !== serverRev;
  $('rev').className = behind ? 'warn' : '';
  $('hash').textContent = hex(lastHash);
  paintCaps();
}

let lastHash = '';
/// 最近一份 glTF 快照：上下文丢失/恢复后要靠它本地重建（数据在，丢的只是显示）
let lastGltf = null;
let blockers = [];
/// three 未就绪时暂存的快照（就绪后补画）
let pendingSnapshot = null;
/// three.js 彻底不可用？——那是要一直显示在屏幕上的事实，不能被后续文案盖掉
let threeFailed = false;

function note(text) {
  if (threeFailed) return; // 已经明确降级了，别再糊上别的字
  $('scene-note').textContent = text;
}
setInterval(async () => {
  try {
    const r = await fetch('/healthz');
    const h = await r.json();
    lastHash = h.scene_hash;
    $('conns').textContent = `连接 ${h.connections} · 已发 ${h.messages_sent} · 帧 ${h.frames_sent}`;
    // 服务端名册（它记账的结果）：鼠标停上去能看到每个连接是谁、声称能干什么。
    // 这是"能力声明"真正派上用场的地方——之前服务端只知道"有几条连接"。
    const roster = (h.clients || []).map((c) =>
      `${c.agent} [${(c.capabilities || []).join(',') || '未声明'}] ${c.kind}` +
      ((c.unknown || []).length ? ` ⚠未知能力:${c.unknown.join(',')}` : '')).join('\n');
    $('conns').title = roster ? `服务端记录：\n${roster}` : '服务端记录：（无）';
    if (localRev === 0) localRev = h.revision;
    $('rev').textContent = `${localRev === h.revision ? '' : localRev + ' / '}${h.revision}`;
    $('hash').textContent = hex(h.scene_hash);
  } catch { /* 服务停了 */ }
  try {
    // 浏览器里看到的告警必须和 Agent 看到的一致（同一个 observe）
    const r = await fetch(`/observe?token=${encodeURIComponent(TOKEN)}`);
    const o = await r.json();
    const n = (o.warnings || []).length;
    $('alerts').textContent = n ? `告警 ${n}` : '无告警';
    $('alerts').className = 'stat ' + (n ? 'warn' : 'ok');
    const bl = o.window_blockers || [];
    $('scene-note').title = bl.length ? `挡窗者：${bl.join(', ')}` : '无挡窗者';
  } catch { /* ignore */ }
}, 2000);

// ---------------------------------------------------------------- 启动
document.querySelectorAll('[data-view]').forEach((b) => {
  b.onclick = () => {
    view = b.dataset.view;
    connectFrame();
  };
});

for (const [id, mk] of Object.entries(ACTIONS)) {
  $(id).onclick = async () => {
    const { status, out } = await post('/command', mk());
    if (status >= 400) {
      $('err').textContent = `${out.error || status}: ${out.message || ''}`;
      $('err').className = 'stat bad';
    } else {
      $('err').textContent = '';
    }
  };
}

async function boot() {
  // 顺序很重要：**先把两条流接上**，再去拿渲染库。
  // 流是本服务自己的东西（永远在），three.js 是 CDN 上的第三方（可能拉不到、可能很慢）；
  // 把后者放到前面 await，等于让第三方决定我们能不能工作。
  connectScene();
  connectFrame();

  // 判定：探测 → 上报（平台优先，离线兜底）→ 应用。
  // **与 three 并行**：microbench 会等 three 一小会儿（见 assessHost），但判定本身
  // 绝不被 CDN 挡住——最需要判定的场合恰好是"客户端渲染根本起不来"。
  const assessed = assessHost().then((v) => (applyVerdict(v), v));
  three = await loadThree();
  threeReadyResolve();
  await assessed;
  if (three) {
    buildScene();
    if (pendingSnapshot) {
      applySnapshot(pendingSnapshot);
      pendingSnapshot = null;
    }
    // 现在才真的能渲染——把这件事**重新声明**给服务端。
    // （首连时 three.js 还没到，那版声明里不能凭空写上 `three`。）
    redeclareScene();
  } else {
    threeFailed = true;
    $('scene-note').textContent =
      'three.js 没能加载（离线/CDN 不可达）：场景流仍在接收，但左侧无法渲染——请看右侧服务端渲染的图像流';
    // 把降级**告诉服务端**：否则运维在服务端只能看到"有三条连接"，
    // 却不知道其中一条根本无法渲染
    redeclareScene();
  }
}

if (TOKEN) boot();
