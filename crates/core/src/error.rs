//! 内核错误：**每个错误都带机器可读的 `code()`**，便于 HTTP/MCP 层直接映射成
//! `{"error": {"code", "message"}}`（与平台侧错误信封一致）。

use std::fmt;

/// 内核错误。
#[derive(Debug, Clone, PartialEq)]
pub enum CoreError {
    /// 参数缺了、类型不对、或者数值非法
    InvalidArgument(String),
    /// 找不到目标（对象 / 灯光）
    UnknownTarget(String),
    /// 目标存在，但当前不允许这类编辑（例如高斯层只能 crop/replace）
    NotEditable { target: String, reason: String },
    /// 命令没有任何效果 —— 拒绝执行，避免在循环里空转
    NoOp(String),
    /// 非法数值（NaN / Inf）
    NonFinite(String),
    /// 请求的 revision 不存在或超出当前版本
    RevisionNotFound(u32),
    /// 已经在最初版本，无可撤销
    NothingToUndo,
    /// 已经在最新版本，无可重做
    NothingToRedo,
    /// 场景 JSON 解析失败
    BadScene(String),
    /// 内部一致性被破坏（理论上不该发生；出现即 bug）
    Invariant(String),
}

impl CoreError {
    /// 机器可读错误码（snake_case）。
    pub fn code(&self) -> &'static str {
        match self {
            CoreError::InvalidArgument(_) => "invalid_argument",
            CoreError::UnknownTarget(_) => "unknown_target",
            CoreError::NotEditable { .. } => "not_editable",
            CoreError::NoOp(_) => "no_op",
            CoreError::NonFinite(_) => "non_finite",
            CoreError::RevisionNotFound(_) => "revision_not_found",
            CoreError::NothingToUndo => "nothing_to_undo",
            CoreError::NothingToRedo => "nothing_to_redo",
            CoreError::BadScene(_) => "bad_scene",
            CoreError::Invariant(_) => "invariant_violated",
        }
    }
}

impl fmt::Display for CoreError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            CoreError::InvalidArgument(m) => write!(f, "参数非法：{}", m),
            CoreError::UnknownTarget(t) => write!(f, "找不到目标：{}", t),
            CoreError::NotEditable { target, reason } => {
                write!(f, "{} 不可这样编辑：{}", target, reason)
            }
            CoreError::NoOp(m) => write!(f, "空操作：{}", m),
            CoreError::NonFinite(m) => write!(f, "数值非法（NaN/Inf）：{}", m),
            CoreError::RevisionNotFound(r) => write!(f, "没有 revision {}", r),
            CoreError::NothingToUndo => write!(f, "已经在最初版本，无可撤销"),
            CoreError::NothingToRedo => write!(f, "已经在最新版本，无可重做"),
            CoreError::BadScene(m) => write!(f, "场景解析失败：{}", m),
            CoreError::Invariant(m) => write!(f, "内部不变量被破坏（这是 bug）：{}", m),
        }
    }
}

impl std::error::Error for CoreError {}

/// crate 内统一 Result。
pub type Result<T> = std::result::Result<T, CoreError>;
