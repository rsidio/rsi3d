//! 内嵌的前端资源：编译进二进制，服务端不依赖任何构建步骤或外部文件。
//!
//! 用 `include_str!` 而不是读磁盘：`rsi3d-harness` 是个 CLI，不能要求用户
//! "把 assets 目录放到旁边"。

/// 浏览器客户端页面。
pub const CLIENT_HTML: &str = include_str!("client.html");

/// 浏览器客户端逻辑。
pub const CLIENT_JS: &str = include_str!("client.js");
