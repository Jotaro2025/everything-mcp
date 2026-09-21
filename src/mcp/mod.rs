//! mcp/mod.rs — Model Context Protocol 服务模块
//!
//! 子模块：
//!   - `protocol`: JSON-RPC 2.0 + MCP 2024-11-05 类型
//!   - `server`:   基于 std::net 的单线程-per-connection HTTP 服务
//!   - `tools`:    暴露给 LLM 的工具实现（search_in_folder / list_folder / count）

pub mod protocol;
pub mod server;
pub mod tools;
