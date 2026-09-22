//! mcp/mod.rs — Model Context Protocol 服务模块
//!
//! 子模块：
//!   - `protocol`: JSON-RPC 2.0 + MCP 双时代（2024-11-05 legacy / 2026-07-28 modern）类型
//!   - `server`:   基于 std::net 的单线程-per-connection HTTP 服务，按请求协商协议版本
//!   - `tools`:    暴露给 LLM 的工具实现（search_in_folder / list_folder / count）
//!   - `validate`: 工具入参校验与路径规范化（folder 规范化、pattern 校验）

pub mod protocol;
pub mod server;
pub mod tools;
pub mod validate;
