<p align="center">
  <img src="assets/logo-plate.svg" alt="rsi3d" width="190">
</p>

<h1 align="center">rsi3d-harness</h1>

<p align="center"><b>An agentic 3D asset engine: give an agent eyes, hands, and a score.</b></p>

<p align="center">
  <!-- CI 是动态徽章：它说的是「上一抰到底绿不绿」，不是某个时刻的数字。
       测试数量会腐，所以那个数字只写在 Development 那一节里。 -->
  <img alt="CI" src="https://github.com/rsidio/rsi3d/actions/workflows/ci.yml/badge.svg?branch=main">
  <img alt="License" src="https://img.shields.io/badge/license-Apache--2.0-blue">
  <img alt="Offline" src="https://img.shields.io/badge/offline-first-54e6c4">
</p>

---

Agents can write code. What they cannot do is **look at a 3D scene, tell whether their change made it
better, and undo it when it didn't.** `rsi3d-harness` is a Rust engine that hands them exactly those
three abilities, plus a command log that makes the whole trajectory reproducible.

```
   observe ──▶ edit ──▶ evaluate ──▶ rollback?
     ▲                                 │
     └─────────────────────────────────┘

   (see)     (change)   (measure)    (undo)
   every step is a command; every command is logged
```

It runs offline. It needs no GPU. It produces the same bytes for the same input, so what an agent
measured is what you can re-check.

## What it is good at

| | |
| --- | --- |
| **Edits you can undo** | Every change is a command with a `reason`, and the engine returns the **inverse command** (restoring the original values, not negating the arguments). `checkout` any earlier revision in one call. |
| **Measurements, not vibes** | "The sofa blocks 44% of the window" is a number the engine computed from pixels, not a caption. Same scene + same parameters ⇒ same pixel hash. |
| **A score curve** | An agent's loop is only as good as its feedback signal. You get per-iteration scores, a best-so-far, and an attribution table saying which step actually helped. |
| **Observations an agent can see** | `scene_render` returns a PNG directly, so the model looks at the asset instead of guessing from JSON. |
| **Portable assets** | Standard glTF 2.0 in and out. Blender, three.js, Fyrox and friends can read what you export. |
| **No cloud in the loop** | Assets never leave the machine. The engine is a local library/binary; there is nothing to sign up for. |

## Quickstart

```bash
git clone https://github.com/rsidio/rsi3d.git && cd rsi3d
cargo build --release          # produces two binaries: rsi3d-harness and rsi3d

# 1. Generate a working agent (ships with a mock engine, needs no GPU)
./target/release/rsi3d-harness scaffold new agent-app --var project=demo-agent
cd demo-agent && node agent.mjs --mock
```

You should see the loop close on its own (output abbreviated; the CLI speaks Chinese today — see
[Status](#status)):

```
──────── 分数曲线 ────────
  0.323 → 0.458 → 0.692 → 0.917
  ▁▃▅█   最佳 0.9167 @ rev 4

──────── 归因表（Agent 的判断 vs 实际效果）────────
  rev  op          target        Δscore   reason
  2    transform   obj:sofa_01   +0.1356  窗户被 obj:sofa_01 遮挡 44%，把它沿 +Z 挪出窗带
  3    set_light   sun           +0.2334  日光强度 2.4 偏高导致过曝，拉到 1
  4    transform   obj:table_01  +0.2250  obj:sofa_01 与 obj:table_01 间距 2.6m 不在 0.4–0.8m，挪到 0.6m
```

That is the whole point of the engine in one screen: a **score curve** (did it get better?) and an
**attribution table** (which step actually helped, and what the agent believed at the time).

Then look at the same scene yourself:

```bash
cd .. && ./target/release/rsi3d-harness scene show demo-agent/scene.json   # read-only: nodes, warnings, clearances
./target/release/rsi3d-harness scene render demo-agent/scene.json --out shot   # 4 viewpoints as PNG
./target/release/rsi3d-harness scene export demo-agent/scene.json --out model.gltf
```

No Rust toolchain? The npm package provides both commands (it ships a small launcher and fetches the
binary for your platform on install or first use):

```bash
npx @rsi3d/cli rsi3d-harness scene show scene.json
```

## The four primitives

| Primitive | What it does |
| --- | --- |
| **observe** | Node table (with per-node **editability**), lights, measured clearances, which object blocks the window and by how much, all warnings, and a content-addressed `scene_hash`. |
| **edit** | Applies a command and returns the new revision, its **inverse**, and any *new* warnings. Commands need a `reason`; `expect` lets you state what you think will improve and check that later. |
| **evaluate** | Scores the current state on rule-based dimensions (window occlusion, exposure, clearance, out-of-room, intersections…). Deterministic — the same revision always scores the same. |
| **export** | Standard glTF 2.0, with our own metadata under `extras.rsi3d`. Also the op log, so the whole session is replayable. |

A command is a plain JSON envelope — identical whether it comes from the CLI, the MCP tools or the
stream server:

```json
{
  "op": "transform",
  "target": "sofa_01",
  "params": { "translate": [0, 0, 1.3] },
  "reason": "window is 44% blocked by the sofa — move it out of the light band",
  "expect": { "lighting.window": "+" }
}
```

`op` is one of `transform` · `set_light` · `set_material` · `remove` · `checkout`. Undo is not an op:
the engine computes it.

## Entry points

| Entry point | For | Command |
| --- | --- | --- |
| **MCP server** (stdio) | Agents inside VS Code / Cursor / Claude Code / any MCP client | `rsi3d-harness mcp --root .` |
| **CLI** | People, scripts, CI | `rsi3d-harness scene show/edit/render/verify/log/export/import` · `render-mode` · `contract` · `scaffold` |
| **Stream server** | Anyone who needs to *see* the asset elsewhere (browser, another machine) | `rsi3d-harness serve scene.json` — scene stream (client-side three.js) + image stream (server-rendered PNG) |
| **Contract artifacts** | People writing their own tooling/plugins | `rsi3d-harness contract --out contract` |
| **Scaffolds** | People who want their own 3D-asset agent or plugin | `rsi3d-harness scaffold list/new` |
| **Agent skill** | Shipping the whole thing to an agent in one command | `rsi3d install skill/rsi3d-harness --agent claude-code` |

The second binary, `rsi3d`, is a client for an [rsi3d](https://rsi3d.com) platform instance
(registry, runs, signatures). The engine does not need it; a self-hosted platform is a separate
component and is not part of this repository.

## Drive it from an agent (MCP)

```jsonc
// .vscode/mcp.json — VS Code and Agent Host read this shape.
// Cursor and Claude Code use the same fields under an `mcpServers` key instead; see docs/mcp.md.
{
  "servers": {
    "rsi3d-harness": {
      "type": "stdio",
      "command": "bash",
      "args": ["${workspaceFolder}/scripts/mcp.sh"],
      "env": { "RSI3D_HARNESS_ROOT": "${workspaceFolder}" }
    }
  }
}
```

Nine tools, all sharing one kernel with the CLI — so the warnings and numbers an agent sees are the
same ones you see:

| Tool | What it does |
| --- | --- |
| `scene_open` | Open a scene, a saved document, or inline scene JSON as the session. |
| `scene_observe` | Read-only snapshot: nodes, editability, lights, clearances, blockers, warnings, hash. |
| `scene_edit` | Apply one or more commands; returns revision, **inverse**, new warnings. |
| `scene_rollback` | Jump to a revision (best-so-far) or undo N steps. Rolling back is itself logged, so it is undoable. |
| `scene_history` | The log **plus an attribution table**: who changed what, why, and what it cost. |
| `scene_diff` | Exact difference between two revisions. |
| `scene_render` | Returns a PNG plus per-object visible pixels and window-band occlusion. |
| `scene_save` | Persist as a document (log included) — the artifact you can replay and audit. |
| `scene_verify` | Self-check: does replay equal the saved state, is every revision replayable, is the hash stable. |

Writes are confined to `--root`. Failures come back as `isError: true` with an error code **and the
current state**, because a model that cannot see what went wrong cannot fix it.

## CLI

```bash
# Look at a scene, change it, keep the log
rsi3d-harness scene show scenes/living.json
rsi3d-harness scene edit scenes/living.json \
  --cmd '{"op":"transform","target":"sofa_01","params":{"translate":[0,0,1.3]},"reason":"out of the light band"}' \
  --out living.doc.json
rsi3d-harness scene verify living.doc.json     # replay == saved state? all revisions replayable?
rsi3d-harness scene render living.doc.json --out shot --views top,iso-sw

# See it from anywhere: two streams at once
rsi3d-harness serve scenes/living.json --port 8283 --open
rsi3d-harness stream http://127.0.0.1:8283 --token T --kind frame --out ./frames

# Which tier should this machine render at? (same rules table online and offline)
rsi3d-harness render-mode --profile host.json

# Bring in an outside asset / hand one out
rsi3d-harness scene import assets/robot.blend --out robot.scene.json
rsi3d-harness scene import --formats
rsi3d-harness scene export living.doc.json --out model.gltf
```

`--json` on most commands makes the output machine-readable. Everything works offline; `serve` binds
to loopback by default and requires a one-time token for the data endpoints.

## Reproducibility guarantees

These four are enforced by tests, not by convention, and they are what makes the numbers usable as
evidence:

| Invariant | Meaning |
| --- | --- |
| **Reversible** | `apply(cmd)` then `apply(inverse)` returns field-for-field to the previous state. The inverse restores recorded values; it is not the arguments negated (rotation on an AABB is not invertible that way). |
| **Replayable** | `replay()` equals the live state field-for-field. A snapshot is only an optimisation, and saved documents are checked against replay on load. |
| **Deterministic** | `scene_hash()` is content-addressed, and the log hash excludes wall-clock time — so an identical trajectory always hashes identically and can be reconciled by a third party. |
| **Cursor-consistent** | `state_at(cursor()) == scene()`, which is what makes `undo`/`redo` and `checkout` trustworthy. |

Corollary: **a change that is not in the op log is not allowed to reach disk.** Editing the scene JSON
by hand breaks attribution and rollback, which is why the engine refuses to treat it as an edit.

## Observations are evidence

`scene_render` produces multi-view PNGs from a **CPU software rasterizer**, and each frame declares
its renderer tier (`cpu-raster/v1`). Same scene, same parameters ⇒ same pixels, on the same platform
and build. `checkout` an old revision and the pixels come back exactly.

That tier is the only one marked as *evidence*: a GPU or screen-capture tier, if added, must declare
itself and may not pretend to be `cpu-raster/v1`, because those pixels cannot be reproduced for
verification. Cross-platform byte-identical output is not promised (libm `sin`/`cos` differ); for
cross-platform comparisons use the integer id-pass statistics and rule metrics instead.

Measurements that come with the images — visible pixels per object, and the share of the window's
light band that is occluded — are computed from the same raster pass, so they cost nothing extra.

## Interop: glTF in, glTF out

- **Out:** standard glTF 2.0, single file, with our metadata in `extras.rsi3d` (which conformant
  loaders are allowed to ignore). Blender, three.js and FyroxEd read it directly.
- **In:** glTF / GLB / OBJ / STL are parsed by the engine itself. BLEND / FBX / USD are converted via
  a local Blender installation (`--blender`, or `RSI3D_BLENDER`); `.step`/`.iges` are B-rep and get a
  clear refusal with a way forward instead of a broken import.
- Imports also write a geometry side-car (`<scene>.mesh.glb`); the document keeps only bounds plus a
  content-addressed `mesh_ref`. A viewer with the side-car draws the real mesh, one without it draws
  bounding proxies — degraded, but never a blank screen.
- Units and axes are never guessed: `--unit-scale` states the source unit, and both the source
  convention and the scene's own convention are recorded.

## Client render modes

Not every machine can render the same thing, and "has WebGL2" does not mean "has the headroom".
`render-mode` reads host facts, returns a tier (`client-full`, `client-lite`, `client-minimal`,
`frame-only`, `headless`) with concrete limits, what is missing, and what to do about it. Limits
are **shrink-only**: a client asking for more gets the stricter value.

The rules table is data and there is one authoritative copy online (`GET /api/render/policy`); when
the platform is unreachable the same table is evaluated locally and the verdict says
`authority=local`. A downgrade is always stated — never silent. See [`docs/render-mode.md`](docs/render-mode.md).

## Ship it to an agent as a skill

```bash
rsi3d publish --dir skills/rsi3d-harness --kind skill       # pack + validate + upload + publish
rsi3d install skill/rsi3d-harness --agent claude-code        # → ~/.claude/skills/rsi3d-harness/
```

A skill/plugin is a **directory with a manifest** (`skill.json`) that becomes a deterministic zip:
store-only, fixed timestamps, entries sorted, so an identical directory always produces identical
bytes and the sha256 can be used as a version lock. Publishing validates the manifest against the
directory and the platform unpacks and re-checks it. Format: [`docs/bundle.md`](docs/bundle.md).

The skill in this repository ([`skills/rsi3d-harness/`](skills/rsi3d-harness)) teaches an agent the
nine tools, the CLI, and the discipline — including which edits are irreversible.

## Design constraints

If you want to change the engine, these are the rules the codebase is built around:

1. **The kernel contains no rendering, IO, networking or LLM calls.** `crates/core` must stay
   exhaustively testable and compile to `wasm32` as well as native. The same kernel runs inside every
   shell, and behaviour must not differ between them.
2. **Every change goes through the command log.** No logged command, no saved change — otherwise the
   score curve cannot be attributed and you cannot roll back to best-so-far.
3. **Reproducibility beats performance.** Identical content must produce identical hashes; log hashes
   must not contain wall-clock time.
4. **The renderer is an implementation detail.** Users and agents should never have to know which
   backend produced an observation — only which tier it belongs to.
5. **The engine works offline.** Observe, edit, render and export must all work with no network.

## Repository layout

```
rsi3d-harness/
├─ crates/core/        kernel: scene model + commands + log/cursor/snapshots + warnings + attribution
├─ crates/render/      observation images: multi-view software raster, id pass, occlusion measurement
├─ crates/stream/      stream protocol (transport-agnostic): glTF snapshots/deltas + sessions + render tiers
├─ crates/serve/       stream server: HTTP + SSE + embedded browser client + CLI client
├─ crates/contract/    single source of contract artifacts: key lists / JSON Schema derived from Rust types
├─ crates/mcp/         MCP server: the kernel as nine tools for any MCP client
├─ crates/scaffold/    plugin-style scaffolding: template discovery, variable rendering, generation
├─ crates/io/          geometry import: glTF/GLB/OBJ/STL read in-process, Blender bridge for the rest
├─ crates/native/      native shell: the `rsi3d-harness` binary
├─ cli/                `rsi3d` binary: platform client (publish / search / verify / pack / run)
├─ packages/rsi3d-cli/ npm package (`@rsi3d/cli`: both commands behind one launcher)
├─ skills/             Agent skill shipped with the project
├─ plugins/            Plugins for other hosts (e.g. harness-use)
├─ scaffolds/          built-in templates (they are themselves distributed artifacts)
├─ contract/           derived artifacts — do not hand-edit
├─ docs/               kernel / renderer / stream / contract / MCP / scaffold / import / render modes / bundles
└─ scripts/            smoke.sh (end-to-end) · mcp.sh (MCP launcher)
```

## Development

```bash
cargo test              # 193 tests: kernel invariants, renderer, MCP protocol, scaffolding,
                        # stream sessions, contract gates, import, render tiers, CLI
bash scripts/smoke.sh   # 241 end-to-end checks: scaffold → kernel → MCP (real process) →
                        # render → stream (real server) → glTF export → npm → contract → import
```

Both run in CI (`.github/workflows/ci.yml`); the counts above are for the current tree.

Both are meant to be *evidence*, not a green light: `scene verify` tells you after the fact whether
replay equals the saved state, whether every revision can be replayed, whether hashes survive a
round-trip, and whether the cursor is self-consistent.

Working on formats? Our own tests cannot prove interoperability — anything we expose to the outside
(glTF, bundle format, stream protocol) has to be checked against a **third-party implementation**
once (`npx @gltf-transform/cli inspect model.gltf`, or the system `unzip` for bundles).

The test suite has only been run on macOS so far; the kernel is platform-independent but Windows is
untested, which is also why CI runs on macOS only for now.

## Documentation

| Doc | Contents |
| --- | --- |
| [`docs/core.md`](docs/core.md) | Kernel contract, the four invariants and how they are pinned by tests |
| [`docs/render.md`](docs/render.md) | Observation renderer, id pass, occlusion measurement, determinism boundaries |
| [`docs/stream.md`](docs/stream.md) | Scene/image streams, sessions, backpressure, client capability declarations |
| [`docs/mcp.md`](docs/mcp.md) | MCP server: transport rules, tools, failure semantics, client wiring |
| [`docs/contract.md`](docs/contract.md) | How contract artifacts are derived and gated |
| [`docs/io.md`](docs/io.md) | Importing geometry (including the Blender path and its pitfalls) |
| [`docs/scaffold.md`](docs/scaffold.md) | Template format and how to publish your own |
| [`docs/render-mode.md`](docs/render-mode.md) | Tier rules, the shared rules table, privacy of host data |
| [`docs/bundle.md`](docs/bundle.md) | Skill/plugin bundle format (`rsi3d-bundle/v1`) |
| [`docs/history.md`](docs/history.md) | Engineering log: milestones, trade-offs, mistakes (Chinese) |

The `docs/` pages are written in Chinese; this README and the code comments you will hit most are in
English. Translations are welcome.

## Status

Early, and honest about it:

- **Not yet callable as a hosted harness.** The engine has no inbound "run this for me over HTTP" entry
  point yet; that side is planned. Today it is driven by an MCP client, the CLI, or your own code.
- **No GPU backend.** Observations come from the CPU rasterizer on purpose (reproducibility), which
  caps resolution and scene size. A GPU tier is future work and will have to declare itself.
- **Gaussian splats and point clouds** are modelled and carry restricted editability (`crop-only`),
  but the observation renderer draws their bounding proxies today.
- **Interfaces will move.** The crate layout and the command envelope are stable; the stream protocol
  and contract artifacts are still gaining fields.

The CLI's human-readable output is Chinese today (structured output via `--json` is
language-neutral); translations are welcome.

Issues and pull requests are welcome. If you are integrating an engine into an agent, the most useful
thing you can report is a case where the attribution table disagreed with what you saw.

## License

Apache-2.0 — see [`LICENSE`](LICENSE).

Built by the [rsi3d](https://rsi3d.com) project. The engine is the reference implementation of the
"harness" contract that the platform registers: a capability boundary that is called over HTTP or MCP
and reports iteration summaries back. The platform is a separate product; this repository is
self-contained and useful on its own.
