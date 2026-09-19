//! JSON-RPC 2.0 的最小实现 + stdio 分帧。
//!
//! MCP 的 stdio 传输规定：报文是**换行分隔**的 JSON，且**不得含内嵌换行**；
//! 服务器**只能**往 stdout 写合法的 MCP 报文（日志一律走 stderr）。
//! 这两条硬要求决定了这里的所有取舍：紧凑序列化、单行、写完立刻 flush。

use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::io::{BufRead, Write};

// ---------------------------------------------------------------- 错误码

pub const PARSE_ERROR: i64 = -32700;
pub const INVALID_REQUEST: i64 = -32600;
pub const METHOD_NOT_FOUND: i64 = -32601;
pub const INVALID_PARAMS: i64 = -32602;
pub const INTERNAL_ERROR: i64 = -32603;

/// JSON-RPC 错误（协议层）。
#[derive(Debug, Clone, PartialEq)]
pub struct RpcError {
    pub code: i64,
    pub message: String,
    pub data: Option<Value>,
}

impl RpcError {
    pub fn new(code: i64, message: impl Into<String>) -> Self {
        RpcError {
            code,
            message: message.into(),
            data: None,
        }
    }

    pub fn invalid_params(message: impl Into<String>) -> Self {
        RpcError::new(INVALID_PARAMS, message)
    }

    pub fn method_not_found(method: &str) -> Self {
        RpcError::new(METHOD_NOT_FOUND, format!("未实现的方法：{}", method))
    }

    pub fn internal(message: impl Into<String>) -> Self {
        RpcError::new(INTERNAL_ERROR, message)
    }

    pub fn with_data(mut self, data: Value) -> Self {
        self.data = Some(data);
        self
    }

    pub fn to_json(&self) -> Value {
        let mut e = serde_json::json!({ "code": self.code, "message": self.message });
        if let Some(d) = &self.data {
            e["data"] = d.clone();
        }
        e
    }
}

// ---------------------------------------------------------------- 报文

/// 请求 id：数字、字符串，或缺失（= 通知）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum Id {
    Num(i64),
    Str(String),
}

#[derive(Debug, Clone, Deserialize)]
pub struct Request {
    #[serde(default)]
    pub jsonrpc: String,
    #[serde(default)]
    pub id: Option<Id>,
    pub method: String,
    #[serde(default)]
    pub params: Option<Value>,
}

impl Request {
    /// 是不是通知（通知**不回复**，即使报错也不回）。
    pub fn is_notification(&self) -> bool {
        self.id.is_none()
    }

    /// `params` 当对象用（缺省视为 `{}`）。
    pub fn params_obj(&self) -> Result<Value, RpcError> {
        match &self.params {
            None | Some(Value::Null) => Ok(Value::Object(Default::default())),
            Some(v @ Value::Object(_)) => Ok(v.clone()),
            Some(_) => Err(RpcError::invalid_params("params 必须是对象")),
        }
    }
}

pub fn error_response(id: Option<&Id>, err: &RpcError) -> Value {
    serde_json::json!({
        "jsonrpc": "2.0",
        "id": id,
        "error": err.to_json(),
    })
}

pub fn result_response(id: Option<&Id>, result: Value) -> Value {
    serde_json::json!({
        "jsonrpc": "2.0",
        "id": id,
        "result": result,
    })
}

pub fn notification(method: &str, params: Value) -> Value {
    serde_json::json!({ "jsonrpc": "2.0", "method": method, "params": params })
}

// ---------------------------------------------------------------- 分帧

/// 逐行读出报文。空行忽略——有些客户端会在启动时多打一个换行。
pub fn read_message(reader: &mut impl BufRead) -> std::io::Result<Option<String>> {
    loop {
        let mut line = String::new();
        let n = reader.read_line(&mut line)?;
        if n == 0 {
            return Ok(None); // EOF
        }
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        return Ok(Some(trimmed.to_string()));
    }
}

/// 写一条报文（**单行**，写完就 flush——客户端在等）。
pub fn write_message(writer: &mut impl Write, msg: &Value) -> std::io::Result<()> {
    let s = match serde_json::to_string(msg) {
        Ok(s) => s,
        // 自己构造的 Value 通常不会序列化失败；真失败了也要回一条**合法**报文，
        // 因为往 stdout 写非 MCP 内容是协议禁止的（客户端会直接断连）。
        Err(e) => serde_json::to_string(&error_response(
            None,
            &RpcError::internal(format!("序列化响应失败：{}", e)),
        ))
        .unwrap_or_else(|_| {
            r#"{"jsonrpc":"2.0","id":null,"error":{"code":-32603,"message":"serialize failed"}}"#
                .to_string()
        }),
    };
    debug_assert!(!s.contains('\n'), "MCP 报文不得含内嵌换行");
    writer.write_all(s.as_bytes())?;
    writer.write_all(b"\n")?;
    writer.flush()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn notification_has_no_id() {
        let req: Request = serde_json::from_str(r#"{"jsonrpc":"2.0","method":"ping"}"#).unwrap();
        assert!(req.is_notification());
        assert_eq!(req.params_obj().unwrap(), serde_json::json!({}));
    }

    #[test]
    fn numeric_and_string_ids_both_work() {
        let a: Request =
            serde_json::from_str(r#"{"jsonrpc":"2.0","id":3,"method":"tools/list"}"#).unwrap();
        assert_eq!(a.id, Some(Id::Num(3)));
        let b: Request =
            serde_json::from_str(r#"{"jsonrpc":"2.0","id":"abc","method":"tools/list"}"#).unwrap();
        assert_eq!(b.id, Some(Id::Str("abc".into())));
    }

    #[test]
    fn written_messages_are_single_line() {
        let mut buf = Vec::new();
        write_message(
            &mut buf,
            &result_response(Some(&Id::Num(1)), serde_json::json!({"ok": true})),
        )
        .unwrap();
        let s = String::from_utf8(buf).unwrap();
        assert_eq!(s.matches('\n').count(), 1);
        assert!(s.ends_with('\n'));
        let back: Value = serde_json::from_str(s.trim()).unwrap();
        assert_eq!(back["id"], 1);
        assert_eq!(back["result"]["ok"], true);
    }

    #[test]
    fn blank_lines_are_skipped() {
        let mut r = std::io::BufReader::new("\n\n\r\n{\"jsonrpc\":\"2.0\"}\n".as_bytes());
        assert_eq!(read_message(&mut r).unwrap().unwrap(), "{\"jsonrpc\":\"2.0\"}");
        assert!(read_message(&mut r).unwrap().is_none());
    }
}
