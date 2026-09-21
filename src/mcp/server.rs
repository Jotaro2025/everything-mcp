//! server.rs — MCP Streamable HTTP 服务（基于 std::net，单线程-per-conn）
//!
//! 传输层规格（MCP 2024-11-05 Streamable HTTP）：
//!   - 客户端 POST 单条 JSON-RPC 请求到服务端根路径；
//!   - 服务端返回 `Content-Type: application/json` 的单条 JSON-RPC 响应；
//!   - 不实现 SSE 长连接通道（LLM 客户端按请求-响应使用工具调用足够）；
//!   - 客户端可发起 GET / 建立长连接监听服务端通知，我们不支持 —— 直接返回 405。
//!
//! 我们实现的请求方法集：
//!   - initialize
//!   - notifications/initialized（无响应）
//!   - tools/list
//!   - tools/call
//!   - ping
//!
//! 关闭机制：使用 `shutdown` AtomicBool 与一个 `Mutex<Option<JoinHandle>>`
//! 保存监听线程句柄，PM_STOP 时调用 `stop()` 标记关闭、join 线程。

use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Mutex, OnceLock};
use std::thread::{self, JoinHandle};
use std::time::Duration;

use serde_json::Value;

use super::protocol::{
    error_response, make_initialize_result, make_tools_list, ok_response, JsonRpcMessage,
    METHOD_NOT_FOUND, PARSE_ERROR,
};
use super::tools;

/// 全局服务句柄 —— 让 PM_STOP 能找到当前运行的服务线程。
static SERVER: OnceLock<ServerHandle> = OnceLock::new();

struct ServerHandle {
    shutdown: std::sync::Arc<AtomicBool>,
    thread: Mutex<Option<JoinHandle<()>>>,
}

/// 启动 HTTP 服务并立即返回。失败返回错误描述。
pub fn start(bind: &str, port: u16) -> Result<(), String> {
    // 已有服务在跑 —— 关闭旧的再启新的。
    if SERVER.get().is_some() {
        stop();
    }

    let addr = format!("{}:{}", bind, port);
    let listener =
        TcpListener::bind(&addr).map_err(|e| format!("bind {} failed: {}", addr, e))?;
    // Windows 上 TCPListener::bind 默认已开启 SO_EXCLUSIVEADDRUSE/SO_REUSEADDR，
    // 服务退出后立即重启不会因为 TIME_WAIT 失败。

    let shutdown = std::sync::Arc::new(AtomicBool::new(false));
    let shutdown_thread = shutdown.clone();

    let handle = thread::Builder::new()
        .name("everything-mcp-listen".into())
        .spawn(move || {
            run_listener(listener, shutdown_thread);
        })
        .map_err(|e| format!("spawn listener thread failed: {}", e))?;

    let _ = SERVER.set(ServerHandle {
        shutdown,
        thread: Mutex::new(Some(handle)),
    });
    Ok(())
}

/// 关闭服务（PM_STOP / PM_KILL 时调用）。
pub fn stop() {
    let s = match SERVER.get() {
        Some(s) => s,
        None => return,
    };
    s.shutdown.store(true, Ordering::SeqCst);

    // 唤醒监听线程 —— 我们已经把它放在非阻塞模式，关闭标志会立刻被读取。
    if let Ok(mut guard) = s.thread.lock() {
        if let Some(t) = guard.take() {
            let _ = t.join();
        }
    }
}

fn run_listener(listener: TcpListener, shutdown: std::sync::Arc<AtomicBool>) {
    // 让 accept 不阻塞 —— 通过设置 listener 本身的非阻塞模式 + 短轮询。
    // 更简单的做法是阻塞 accept + shutdown_socket，但 Windows 上没有可移植的
    // 关闭阻塞 accept 的方式，所以这里采用非阻塞 + 轮询。
    let _ = listener.set_nonblocking(true);

    while !shutdown.load(Ordering::SeqCst) {
        match listener.accept() {
            Ok((stream, peer)) => {
                // 每个连接一个工作线程；MCP 调用通常很轻，无需连接池。
                let _ = peer;
                let s_clone = shutdown.clone();
                let _ = thread::Builder::new()
                    .name("everything-mcp-conn".into())
                    .spawn(move || {
                        let _ = handle_connection(stream, s_clone);
                    });
            }
            Err(ref e)
                if e.kind() == std::io::ErrorKind::WouldBlock =>
            {
                // 没有新连接 —— 短暂休眠再试。
                thread::sleep(Duration::from_millis(50));
            }
            Err(e) => {
                crate::plugin::host::Host::debug(&format!("accept err: {}", e));
                thread::sleep(Duration::from_millis(100));
            }
        }
    }
}

/// 处理单条 HTTP 连接：读请求 → 分发 → 写响应。
///
/// 线程模型：HTTP 解析、JSON-RPC 分发、tools/list / ping 等都直接在本工作线程
/// 上执行 —— 这套流程已经在工作线程上验证过是安全的。只有 tools/call 命中
/// search_in_folder 时，search_in_folder 内部会自行把 db_query_search2 marshal
/// 到 Everything 主线程；调用方无需关心。
fn handle_connection(
    mut stream: TcpStream,
    shutdown: std::sync::Arc<AtomicBool>,
) -> std::io::Result<()> {
    if shutdown.load(Ordering::SeqCst) {
        return Ok(());
    }
    // 关键：listener 被设成 nonblocking 用于轮询 accept；这个属性会被
    // accept 出来的 stream 继承。我们必须显式恢复阻塞模式，否则下面
    // 的 stream.read() 会立刻返回 WSAEWOULDBLOCK (os error 10035)。
    let _ = stream.set_nonblocking(false);
    let _ = stream.set_read_timeout(Some(Duration::from_secs(30)));
    let _ = stream.set_write_timeout(Some(Duration::from_secs(30)));

    let req = match read_http_request(&mut stream) {
        Ok(r) => r,
        Err(e) => {
            let body = format!("HTTP parse error: {}", e);
            let _ = stream.write_all(bad_request(&body).as_bytes());
            return Ok(());
        }
    };

    if req.method != "POST" || (req.path != "/" && req.path != "/mcp") {
        let _ = stream.write_all(not_allowed().as_bytes());
        return Ok(());
    }

    // dispatch_rpc 在本工作线程上执行：HTTP 解析、JSON-RPC 分发、tools/list / ping
    // 都不接触 Everything DB，留在工作线程上最自然也最快。只有 search_in_folder
    // 里真正调用 db_query_search2 的那几行需要 marshal 到主线程 —— 由 search.rs 自行处理。
    let body = dispatch_rpc(&req.body);

    let resp = ok_http_response(&body);
    let _ = stream.write_all(resp.as_bytes());
    let _ = stream.flush();
    Ok(())
}

// ====================================================================
// HTTP 解析（极简实现：只处理无分块、Content-Length 控制的 POST）
// ====================================================================

struct HttpRequest {
    method: String,
    path: String,
    body: String,
}

/// 读取并解析一个 HTTP 请求。
/// 我们只关心 method、path、Content-Length 与 body。
fn read_http_request(stream: &mut TcpStream) -> std::io::Result<HttpRequest> {
    let mut buf = Vec::with_capacity(4096);
    let mut byte = [0u8; 1];
    loop {
        // 读取直到 "\r\n\r\n" 标志头部结束。
        let n = stream.read(&mut byte)?;
        if n == 0 {
            return Err(std::io::Error::new(
                std::io::ErrorKind::UnexpectedEof,
                "connection closed during header",
            ));
        }
        buf.push(byte[0]);
        if buf.len() >= 4 && &buf[buf.len() - 4..] == b"\r\n\r\n" {
            break;
        }
        if buf.len() > 65536 {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "header too large",
            ));
        }
    }

    let header_str = String::from_utf8_lossy(&buf);
    let mut lines = header_str.split("\r\n");
    let request_line = lines.next().ok_or_else(|| {
        std::io::Error::new(std::io::ErrorKind::InvalidData, "no request line")
    })?;
    let mut parts = request_line.split_whitespace();
    let method = parts.next().unwrap_or("").to_string();
    let path = parts.next().unwrap_or("").to_string();

    // 解析 Content-Length（不区分大小写）。
    let mut content_length: usize = 0;
    for line in lines {
        if line.is_empty() {
            break;
        }
        let lower = line.to_ascii_lowercase();
        if let Some(v) = lower.strip_prefix("content-length:") {
            if let Ok(n) = v.trim().parse::<usize>() {
                content_length = n;
            }
        }
    }

    // 读取 body。
    let mut body = String::new();
    if content_length > 0 {
        let mut remain = content_length;
        let mut chunk = [0u8; 1024];
        while remain > 0 {
            let take = remain.min(chunk.len());
            let n = stream.read(&mut chunk[..take])?;
            if n == 0 {
                break;
            }
            body.push_str(&String::from_utf8_lossy(&chunk[..n]));
            remain -= n;
        }
    }

    Ok(HttpRequest { method, path, body })
}

// ====================================================================
// JSON-RPC 分发
// ====================================================================

fn dispatch_rpc(body: &str) -> String {
    // 可能有批量和单条两种；MCP 客户端实际只发单条，这里也只处理单条。
    let parsed: JsonRpcMessage = match serde_json::from_str(body) {
        Ok(m) => m,
        Err(_) => {
            let resp = error_response(&None, PARSE_ERROR, "parse error");
            return serde_json::to_string(&resp).unwrap_or_else(|_| "{}".into());
        }
    };

    // 是否是通知（无 id）—— 通知无响应内容，但我们仍返回一个 204 风格的空响应体。
    let is_notification = parsed.id.is_none();
    let id = parsed.id.clone();
    let method = parsed.method.as_deref().unwrap_or("");
    let params = parsed.params.clone().unwrap_or(Value::Null);

    let resp = match method {
        "initialize" => ok_response(&id, make_initialize_result(&params)),
        "notifications/initialized" => {
            // 通知 —— 不返回任何内容。
            return "{\"jsonrpc\":\"2.0\",\"id\":null}".to_string();
        }
        "ping" => ok_response(&id, serde_json::json!({})),
        "tools/list" => ok_response(&id, make_tools_list()),
        "tools/call" => {
            let name = params
                .get("name")
                .and_then(Value::as_str)
                .unwrap_or("");
            let args = params.get("arguments").cloned().unwrap_or(Value::Null);
            match tools::dispatch(name, &args) {
                Ok((content, is_error)) => {
                    let mut result = serde_json::json!(content);
                    if is_error {
                        result["isError"] = serde_json::Value::Bool(true);
                    }
                    ok_response(&id, result)
                }
                Err((code, msg)) => error_response(&id, code, &msg),
            }
        }
        _ => {
            let _ = is_notification;
            error_response(&id, METHOD_NOT_FOUND, &format!("method '{}' not found", method))
        }
    };

    serde_json::to_string(&resp).unwrap_or_else(|_| "{\"error\":\"encode failed\"}".into())
}

// ====================================================================
// HTTP 响应构造
// ====================================================================

fn ok_http_response(body: &str) -> String {
    let mut s = String::with_capacity(body.len() + 256);
    s.push_str("HTTP/1.1 200 OK\r\n");
    s.push_str("Content-Type: application/json\r\n");
    s.push_str(&format!("Content-Length: {}\r\n", body.len()));
    s.push_str("Access-Control-Allow-Origin: *\r\n");
    s.push_str("Access-Control-Allow-Methods: POST, OPTIONS\r\n");
    s.push_str("Access-Control-Allow-Headers: Content-Type, Accept\r\n");
    s.push_str("Connection: close\r\n");
    s.push_str("\r\n");
    s.push_str(body);
    s
}

fn bad_request(reason: &str) -> String {
    let body = format!("{{\"error\":\"{}\"}}", reason.replace('"', "\\\""));
    format!(
        "HTTP/1.1 400 Bad Request\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
        body.len(),
        body
    )
}

fn not_allowed() -> String {
    "HTTP/1.1 405 Method Not Allowed\r\nAllow: POST\r\nContent-Length: 0\r\nConnection: close\r\n\r\n".into()
}
