# @rsi3d/cli

> npm entry point for the rsi3d command line and the `rsi3d-harness` 3D asset engine.
> One install, two commands.

```bash
npx @rsi3d/cli rsi3d-harness --help     # the engine
npx @rsi3d/cli rsi3d --help             # the platform client
```

Or install it globally:

```bash
npm i -g @rsi3d/cli
rsi3d-harness scene show scene.json
```

## What the two commands do

| Command | Purpose |
| --- | --- |
| `rsi3d-harness` | **Engine**: `mcp` (expose the engine's tools to VS Code / Cursor / Claude Code) · `scene` (observe / edit / rollback / render / verify / export / import) · `serve` + `stream` (live scene + image streams) · `scaffold` (generate your own 3D-asset agent or plugin) · `contract` · `render-mode` |
| `rsi3d` | **Platform client**: publish / search / download / verify / pack / drive a Run / install skills |

Using it from VS Code (`.vscode/mcp.json`):

```json
{
  "servers": {
    "rsi3d-harness": {
      "type": "stdio",
      "command": "npx",
      "args": ["-y", "@rsi3d/cli", "rsi3d-harness", "mcp", "--root", "${workspaceFolder}"]
    }
  }
}
```

Cursor and Claude Code use the same fields under an `mcpServers` key. The engine also ships an
[Agent Skill](https://github.com/rsidio/rsi3d) that teaches a model the tools and the discipline:
`rsi3d install skill/rsi3d-harness --agent claude-code`.

## Where the binaries come from

This package **ships a small launcher, not the binaries.** `bin/launcher.js` resolves them in this
order — local first, remote last, and it never silently switches source:

1. `RSI3D_BIN` / `RSI3D_HARNESS_BIN` (explicit; a broken path fails loudly instead of falling back)
2. a build in the repo (`target/release/<name>`) — the single-repo development case
3. `~/.rsi3d/bin/<name>` (previously installed)
4. `vendor/<platform>-<arch>/<name>` inside the package
5. download from `RSI3D_RELEASE_BASE` (GitHub Releases by default) into `~/.rsi3d/bin/`, verified
   against `checksums.txt`

Why not bundle the binaries: two binaries times a platform matrix would make **everyone** pay for
everyone else's platform. Prebuilt artifacts as npm `optionalDependencies`
(`@rsi3d/cli-<platform>-<arch>`) are the next step.

## Just want the source

```bash
git clone https://github.com/rsidio/rsi3d.git && cd rsi3d
cargo build --release        # both binaries, one target/ directory
./target/release/rsi3d-harness --help
```

The engine is a Rust workspace with no required system dependencies beyond a toolchain; the scene
kernel, the renderer and the stream server all work offline.

## License

Apache-2.0.
