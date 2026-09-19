//! 内置模板：全部用 `include_str!` 编译进二进制，**离线可用、无需下载**。
//!
//! 加模板的步骤：在 `rsi3d-harness/scaffolds/<id>/` 放 `scaffold.json` 与 `files/`，
//! 然后在这里加一个 `fn` 并登记到 [`all`]。

use crate::{Manifest, Source, Template, TemplateFile};

pub fn all() -> Vec<Template> {
    vec![agent_app(), harness_plugin(), pack(), scaffold()]
}

fn manifest_from(raw: &str) -> Manifest {
    serde_json::from_str(raw).expect("内置模板的 scaffold.json 必须是合法 JSON（改完记得跑 cargo test）")
}

fn file(path: &str, content: &str) -> TemplateFile {
    TemplateFile {
        path: path.into(),
        content: content.into(),
        exec: false,
    }
}

/// 消费侧：一个能驱动引擎的 Agent（自带 mock 引擎，可零依赖跑通）。
fn agent_app() -> Template {
    Template {
        manifest: manifest_from(include_str!("../../../scaffolds/agent-app/scaffold.json")),
        source: Source::Builtin,
        dir: None,
        files: vec![
            file(
                "README.md",
                include_str!("../../../scaffolds/agent-app/files/README.md"),
            ),
            file(
                "package.json",
                include_str!("../../../scaffolds/agent-app/files/package.json"),
            ),
            file(
                "agent.mjs",
                include_str!("../../../scaffolds/agent-app/files/agent.mjs"),
            ),
            file(
                "mock-engine.mjs",
                include_str!("../../../scaffolds/agent-app/files/mock-engine.mjs"),
            ),
            file(
                "scene.json",
                include_str!("../../../scaffolds/agent-app/files/scene.json"),
            ),
        ],
    }
}

/// 供给侧：给引擎加能力的插件（自定义评测维度 + 工具）。
fn harness_plugin() -> Template {
    Template {
        manifest: manifest_from(include_str!("../../../scaffolds/harness-plugin/scaffold.json")),
        source: Source::Builtin,
        dir: None,
        files: vec![
            file(
                "README.md",
                include_str!("../../../scaffolds/harness-plugin/files/README.md"),
            ),
            file(
                "rsi3d-plugin.json",
                include_str!("../../../scaffolds/harness-plugin/files/rsi3d-plugin.json"),
            ),
            file(
                "src/evaluator.mjs",
                include_str!("../../../scaffolds/harness-plugin/files/src/evaluator.mjs"),
            ),
            file(
                "src/tool.mjs",
                include_str!("../../../scaffolds/harness-plugin/files/src/tool.mjs"),
            ),
            file(
                "selftest.mjs",
                include_str!("../../../scaffolds/harness-plugin/files/selftest.mjs"),
            ),
            file(
                "fixtures/scene.json",
                include_str!("../../../scaffolds/harness-plugin/files/fixtures/scene.json"),
            ),
        ],
    }
}

/// 交付侧：行业业务包（对齐平台的 `rsi3d-pack/v1` 与 `rsi3d pack` 工具链）。
fn pack() -> Template {
    Template {
        manifest: manifest_from(include_str!("../../../scaffolds/pack/scaffold.json")),
        source: Source::Builtin,
        dir: None,
        files: vec![
            file(
                "README.md",
                include_str!("../../../scaffolds/pack/files/README.md"),
            ),
            file(
                "rsi3d.pack.json",
                include_str!("../../../scaffolds/pack/files/rsi3d.pack.json"),
            ),
            file(
                "scenes/living-room.json",
                include_str!("../../../scaffolds/pack/files/scenes/living-room.json"),
            ),
            file(
                "materials/material-spec.md",
                include_str!("../../../scaffolds/pack/files/materials/material-spec.md"),
            ),
            file(
                "workflow/pipeline.json",
                include_str!("../../../scaffolds/pack/files/workflow/pipeline.json"),
            ),
            file(
                "benchmark/cases.json",
                include_str!("../../../scaffolds/pack/files/benchmark/cases.json"),
            ),
        ],
    }
}

/// 自举：生成「一个外部模板」的骨架，改完放进外部模板目录即成为你自己的模板插件。
fn scaffold() -> Template {
    Template {
        manifest: manifest_from(include_str!("../../../scaffolds/scaffold/scaffold.json")),
        source: Source::Builtin,
        dir: None,
        files: vec![
            file(
                "README.md",
                include_str!("../../../scaffolds/scaffold/files/README.md"),
            ),
            file(
                "scaffold.json",
                include_str!("../../../scaffolds/scaffold/files/scaffold.json"),
            ),
            file(
                "files/README.md",
                include_str!("../../../scaffolds/scaffold/files/files/README.md"),
            ),
        ],
    }
}
