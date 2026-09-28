//! server.rs — MCP Streamable HTTP 服务（基于 std::net，单线程-per-conn）
//!
//! 传输层规格（dual-era：MCP 2024-11-05 legacy + 2026-07-28 modern）：
//!   - 客户端 POST 单条 JSON-RPC 请求到服务端根路径；
//!   - 服务端返回 `Content-Type: application/json` 的单条 JSON-RPC 响应；
//!   - 不实现 SSE 长连接通道（LLM 客户端按请求-响应使用工具调用足够）；
//!   - 客户端可发起 GET / 建立长连接监听服务端通知，我们不支持 —— 直接返回 405
//!     （2026-07-28 已移除 GET 流端点，405 同时符合两代规范）。
//!
//! 版本协商（2026-07-28）：modern 客户端在每个请求的 `_meta` 里带
//! `io.modelcontextprotocol/protocolVersion`，Streamable HTTP 上还必须在
//! `MCP-Protocol-Version` 请求头里带同一个值。时代判定：
//!   - 声明 modern 版本 → modern 语义：server/discover 可用、未知方法 404、
//!     通知 202 空体、头/体不一致 -32020、版本不支持 400 + -32022、
//!     每个 result 带 resultType（2026-07-28 MUST），tools/list 与
//!     server/discover 的结果另带 ttlMs / cacheScope（CacheableResult 必填）；
//!   - 声明 legacy 版本或什么都没带 → legacy 语义：initialize 握手、
//!     200 + JSON-RPC 错误体，老客户端完全不受影响。
//!
//! 安全：校验 Origin 头防 DNS rebinding（非 localhost 来源 403）。
//!
//! 关闭机制：使用 `shutdown` AtomicBool 与一个 `Mutex<Option<JoinHandle>>`
//! 保存监听线程句柄，PM_STOP 时调用 `stop()` 标记关闭、join 线程。

use std::io::{Read, Write};
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr, TcpListener, TcpStream};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::Mutex;
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use serde_json::Value;

use super::protocol;
use super::tools;
use crate::plugin::stats;

/// 全局服务句柄 —— 让 PM_STOP 能找到当前运行的服务线程。
///
/// 必须是可写的 Option：stop → start（改端口、停用再启用插件）会创建
/// 全新的 shutdown Arc 与监听线程。OnceLock 只能 set 一次，第二次 set
/// 会被静默丢弃 —— 新监听线程从此没人能停（停用插件后端口照常接受
/// 连接），线程句柄也一并泄漏。
static SERVER: Mutex<Option<ServerHandle>> = Mutex::new(None);

/// 在途连接计数：并发上限与 PM_STOP 排空共用。
static INFLIGHT: AtomicUsize = AtomicUsize::new(0);

struct ServerHandle {
    shutdown: std::sync::Arc<AtomicBool>,
    /// 自连接唤醒地址：阻塞 accept 的打断手段（stop() 往这里连一下）。
    wakeup: SocketAddr,
    thread: Mutex<Option<JoinHandle<()>>>,
}

/// 并发连接上限。MCP 客户端串行调用，正常个位数并发就到顶；上限只为
/// 挡住失控客户端把「每连接一线程」撑爆。满了直接 503，不排队。
const MAX_CONNECTIONS: usize = 64;

/// 单连接总时限：头部与 body 的每次读取都按「剩余时限」设读超时，
/// 慢速客户端不能再靠「每 29 秒发 1 字节」永久占住线程。分发阶段不受
/// 此限 —— 搜索时长由工具自己的 timeout_ms 管。
const CONNECTION_DEADLINE: Duration = Duration::from_secs(60);

/// PM_STOP 排空在途请求的上限。搜索默认超时 10s 加余量取 15s。排空只是
/// 礼貌（把已接单的请求答完），不是内存安全要求 —— query 只在主线程
/// 使用，与 PM_STOP 天然串行。
const DRAIN_TIMEOUT: Duration = Duration::from_secs(15);

/// 启动 HTTP 服务并立即返回。失败返回错误描述。
pub fn start(bind: &str, port: u16) -> Result<(), String> {
    // 已有服务在跑 —— 关闭旧的再启新的（无服务时是 no-op）。
    stop();

    let addr = format!("{}:{}", bind, port);
    let listener = TcpListener::bind(&addr).map_err(|e| format!("bind {} failed: {}", addr, e))?;
    // Windows 上 TCPListener::bind 默认已开启 SO_EXCLUSIVEADDRUSE/SO_REUSEADDR，
    // 服务退出后立即重启不会因为 TIME_WAIT 失败。

    // 唤醒地址：bind 0.0.0.0/[::] 时换成对应的 loopback，自连接才连得到。
    let wakeup = match listener.local_addr() {
        Ok(a) => match a.ip() {
            IpAddr::V4(ip) if ip.is_unspecified() => {
                SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), a.port())
            }
            IpAddr::V6(ip) if ip.is_unspecified() => {
                SocketAddr::new(IpAddr::V6(Ipv6Addr::LOCALHOST), a.port())
            }
            _ => a,
        },
        Err(_) => SocketAddr::from(([127, 0, 0, 1], port)),
    };

    let shutdown = std::sync::Arc::new(AtomicBool::new(false));
    let shutdown_thread = shutdown.clone();

    let handle = thread::Builder::new()
        .name("everything-mcp-listen".into())
        .spawn(move || {
            run_listener(listener, shutdown_thread);
        })
        .map_err(|e| format!("spawn listener thread failed: {}", e))?;

    let mut slot = SERVER.lock().unwrap_or_else(|p| p.into_inner());
    *slot = Some(ServerHandle {
        shutdown,
        wakeup,
        thread: Mutex::new(Some(handle)),
    });
    Ok(())
}

/// 关闭服务（PM_STOP / PM_KILL 时调用）。
pub fn stop() {
    // 先把句柄整体取走再 join：不持有 SERVER 锁等待线程退出，
    // stop 幂等（第二次调用拿到 None 直接返回）。
    let taken = {
        let mut slot = SERVER.lock().unwrap_or_else(|p| p.into_inner());
        slot.take()
    };
    let s = match taken {
        Some(s) => s,
        None => return,
    };
    s.shutdown.store(true, Ordering::SeqCst);

    // 打断阻塞 accept：向本机端口自连接唤醒。连不上（理论上不会发生）
    // 就放弃 join —— 监听线程会在下一个真实连接到来时自行退出，不值得
    // 为停机挂死 PM_STOP。
    let mut woken = false;
    for _ in 0..10 {
        if TcpStream::connect_timeout(&s.wakeup, Duration::from_millis(100)).is_ok() {
            woken = true;
            break;
        }
    }
    if woken {
        let joined = match s.thread.lock() {
            Ok(mut guard) => guard.take(),
            Err(p) => p.into_inner().take(),
        };
        if let Some(t) = joined {
            let _ = t.join();
        }
    } else {
        crate::plugin::host::Host::debug(
            "everything_mcp: stop() self-connect failed; listener exits on next connection",
        );
    }

    // 排空在途请求：等连接线程把已接单的请求答完（有界）。这只影响
    // 「停机时不丢已接单的请求」；query 只在主线程使用，与 PM_STOP
    // 串行，没有内存安全问题。
    let deadline = Instant::now() + DRAIN_TIMEOUT;
    while INFLIGHT.load(Ordering::SeqCst) > 0 && Instant::now() < deadline {
        thread::sleep(Duration::from_millis(20));
    }
    if INFLIGHT.load(Ordering::SeqCst) > 0 {
        crate::plugin::host::Host::debug(
            "everything_mcp: stop() drain timeout; dropping in-flight requests",
        );
    }
}

fn run_listener(listener: TcpListener, shutdown: std::sync::Arc<AtomicBool>) {
    // 阻塞 accept：旧实现的非阻塞 + 50ms 轮询给每个请求平均白加 ~25ms
    // 延迟（响应是 Connection: close，每个 MCP 请求都是新连接）。停机
    // 由 stop() 向本机端口自连接唤醒。
    while !shutdown.load(Ordering::SeqCst) {
        match listener.accept() {
            Ok((mut stream, _peer)) => {
                // 唤醒连接或停机后才到达的连接：直接退出循环，套接字丢弃。
                if shutdown.load(Ordering::SeqCst) {
                    break;
                }
                // 并发上限：满了 503 拒掉，不再开线程。
                if INFLIGHT.load(Ordering::Relaxed) >= MAX_CONNECTIONS {
                    let _ = stream.write_all(
                        b"HTTP/1.1 503 Service Unavailable\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
                    );
                    continue;
                }
                stats::record_connection();
                let s_clone = shutdown.clone();
                INFLIGHT.fetch_add(1, Ordering::SeqCst);
                let spawned = thread::Builder::new()
                    .name("everything-mcp-conn".into())
                    .spawn(move || {
                        let _ = handle_connection(stream, s_clone);
                        INFLIGHT.fetch_sub(1, Ordering::SeqCst);
                    });
                if spawned.is_err() {
                    // 没起线程就得自己把计数还回去。
                    INFLIGHT.fetch_sub(1, Ordering::SeqCst);
                }
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
    // 阻塞 accept 出来的 stream 天生是阻塞模式。读写超时按连接总时限的
    // 剩余量逐次设置（见 read_bounded）。
    let deadline = Instant::now() + CONNECTION_DEADLINE;
    let _ = stream.set_write_timeout(Some(Duration::from_secs(30)));

    let req = match read_http_request(&mut stream, deadline) {
        Ok(r) => r,
        Err(e) => {
            let body = format!("HTTP parse error: {}", e);
            let _ = stream.write_all(bad_request(&body).as_bytes());
            return Ok(());
        }
    };

    if req.head.method != "POST" || (req.head.path != "/" && req.head.path != "/mcp") {
        let _ = stream.write_all(not_allowed().as_bytes());
        return Ok(());
    }

    // Origin 校验（2026-07-28 安全要求）：带 Origin 且非 localhost 的来源
    // 一律 403，挡掉浏览器页面经 DNS rebinding 打到本机端点的请求。
    // 不带 Origin 的非浏览器客户端（curl / MCP 客户端）不受影响。
    if let Some(body) = origin_error(req.head.origin.as_deref()) {
        let _ = stream.write_all(forbidden(&body).as_bytes());
        return Ok(());
    }

    // dispatch_rpc_http 在本工作线程上执行：HTTP 解析、JSON-RPC 分发、tools/list /
    // ping 都不接触 Everything DB，留在工作线程上最自然也最快。只有 tools/call 命中
    // search_in_folder 时，search_in_folder 内部会自行把 db_query_search2 marshal
    // 到 Everything 主线程；调用方无需关心。
    let (status, body) = dispatch_rpc_http(&req.body, &req.head);

    let resp = http_response(status, &body);
    let _ = stream.write_all(resp.as_bytes());
    let _ = stream.flush();
    Ok(())
}

// ====================================================================
// HTTP 解析（极简实现：只处理无分块、Content-Length 控制的 POST）
// ====================================================================

/// 一次 HTTP 请求的头部信息。
///
/// 除 method / path / Content-Length 外，还收录 2026-07-28 规范定义的
/// MCP 镜像请求头与安全校验用的 Origin。字段可空 —— legacy 客户端不带
/// 这些头，`Default` 出一个空 head 即等价于 legacy 请求。
#[derive(Debug, Default, Clone)]
pub struct RequestHead {
    pub method: String,
    pub path: String,
    pub content_length: usize,
    /// MCP-Protocol-Version 请求头（modern 客户端必带）。
    pub protocol_version: Option<String>,
    /// Origin 请求头（浏览器客户端带；用于防 DNS rebinding）。
    pub origin: Option<String>,
    /// Mcp-Method 镜像头（modern 客户端必带，值应等于请求体的 method）。
    pub mcp_method: Option<String>,
    /// Mcp-Name 镜像头（tools/call 等带 params.name 的请求带，可能 base64）。
    pub mcp_name: Option<String>,
}

struct HttpRequest {
    head: RequestHead,
    body: String,
}

/// 在 buf[from..] 里找 b"\r\n\r\n"，返回匹配起点。
fn find_head_end(buf: &[u8], from: usize) -> Option<usize> {
    if buf.len() < 4 {
        return None;
    }
    let from = from.min(buf.len() - 4);
    (from..=buf.len() - 4).find(|&i| &buf[i..i + 4] == b"\r\n\r\n")
}

/// 带连接总时限的一次读取：读超时取「剩余时限」与 30s 的较小者，到线
/// 即报错 —— 防慢速客户端用永不完结的请求把线程占到底。
fn read_bounded(
    stream: &mut TcpStream,
    chunk: &mut [u8],
    deadline: Instant,
) -> std::io::Result<usize> {
    let remain = deadline.saturating_duration_since(Instant::now());
    if remain.is_zero() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::TimedOut,
            "connection deadline exceeded",
        ));
    }
    let _ = stream.set_read_timeout(Some(remain.min(Duration::from_secs(30))));
    stream.read(chunk)
}

/// 读取并解析一个 HTTP 请求。
/// 我们只关心 method、path、Content-Length、MCP 镜像头与 body。
///
/// 头部与 body 都按块读进字节缓冲：头部原来逐字节 read（几百字节的头部
/// 就是几百次系统调用）；body 原来每 1 KiB 块独立 `from_utf8_lossy`，
/// 跨块边界的多字节 UTF-8 序列（中文搜索词很常见）前后两半各变 U+FFFD，
/// 属于静默数据损坏 —— 现在整个 body 攒齐后一次性解码。
fn read_http_request(
    stream: &mut TcpStream,
    deadline: Instant,
) -> std::io::Result<HttpRequest> {
    let mut buf = Vec::with_capacity(4096);
    let mut chunk = [0u8; 4096];
    let mut search_from = 0usize;
    let head_end = loop {
        if let Some(pos) = find_head_end(&buf, search_from) {
            break pos;
        }
        // 下一轮从「可能被新数据补全的最早位置」开始找，避免整段重扫。
        search_from = buf.len().saturating_sub(3);
        if buf.len() > 65536 {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "header too large",
            ));
        }
        let n = read_bounded(stream, &mut chunk, deadline)?;
        if n == 0 {
            return Err(std::io::Error::new(
                std::io::ErrorKind::UnexpectedEof,
                "connection closed during header",
            ));
        }
        buf.extend_from_slice(&chunk[..n]);
    };

    let header_str = String::from_utf8_lossy(&buf[..head_end]);
    let head = parse_header(&header_str)
        .ok_or_else(|| std::io::Error::new(std::io::ErrorKind::InvalidData, "no request line"))?;

    // 读取 body：头部结束符后面可能已经带进来一段（甚至超出的）body 字节。
    let mut body_bytes = Vec::with_capacity(head.content_length.min(64 * 1024));
    body_bytes.extend_from_slice(&buf[(head_end + 4).min(buf.len())..]);
    while body_bytes.len() < head.content_length {
        let n = read_bounded(stream, &mut chunk, deadline)?;
        if n == 0 {
            // 与旧行为一致：对端中途关闭时按已收到的内容继续。
            break;
        }
        body_bytes.extend_from_slice(&chunk[..n]);
    }
    // 管道化（同一段里塞了两条请求）时丢弃超出 Content-Length 的部分，
    // 保持「一次连接只处理一条请求」的既有语义。
    body_bytes.truncate(head.content_length);

    let body = String::from_utf8_lossy(&body_bytes).into_owned();
    Ok(HttpRequest { head, body })
}

/// 解析已读完的 HTTP 头部文本，取出请求行与关注的头部字段。
///
/// `header` 是到（含）`\r\n\r\n` 为止的全部字节；首行必须是
/// `METHOD SP PATH SP VERSION`，否则返回 None。Content-Length 缺失或非法时
/// 按 0 处理（无 body 的请求）。头部字段名不区分大小写。
pub fn parse_header(header: &str) -> Option<RequestHead> {
    let mut lines = header.split("\r\n");
    let request_line = lines.next()?;
    let mut parts = request_line.split_whitespace();
    let method = parts.next()?.to_string();
    let path = parts.next()?.to_string();

    let mut head = RequestHead {
        method,
        path,
        ..RequestHead::default()
    };
    for line in lines {
        if line.is_empty() {
            break;
        }
        // split_once 取第一个冒号 —— 值里再出现冒号（如 Origin 的端口）不受影响。
        let (name, value) = match line.split_once(':') {
            Some((n, v)) => (n.trim(), v.trim()),
            None => continue,
        };
        match name.to_ascii_lowercase().as_str() {
            "content-length" => {
                if let Ok(n) = value.parse::<usize>() {
                    head.content_length = n;
                }
            }
            "mcp-protocol-version" => head.protocol_version = Some(value.to_string()),
            "origin" => head.origin = Some(value.to_string()),
            "mcp-method" => head.mcp_method = Some(value.to_string()),
            "mcp-name" => head.mcp_name = Some(value.to_string()),
            _ => {}
        }
    }
    Some(head)
}

// ====================================================================
// JSON-RPC 分发（时代感知）
// ====================================================================

/// 序列化 JSON-RPC 响应（失败时退回一个最小错误体，绝不返回空）。
fn encode(resp: &protocol::JsonRpcMessage) -> String {
    serde_json::to_string(resp).unwrap_or_else(|_| r#"{"error":"encode failed"}"#.into())
}

/// legacy 兼容入口：不带任何版本信息 → 按 legacy 语义分发，只返回响应体。
///
/// 保留这个签名是为了不破坏既有调用方与测试；新代码应使用
/// [`dispatch_rpc_http`] 拿到 HTTP 状态码。
pub fn dispatch_rpc(body: &str) -> String {
    dispatch_rpc_http(body, &RequestHead::default()).1
}

/// 时代感知的 JSON-RPC 分发。返回 `(HTTP 状态码, 响应体)`。
///
/// 时代由请求声明的协议版本决定（`MCP-Protocol-Version` 头优先，其次
/// `_meta` 里的版本键；两者都在但不一致 → -32020）：
///   - modern（2026-07-28）：server/discover、未知方法 404、通知 202 空体；
///   - legacy（2024-11-05 或未声明）：initialize 握手、200 + JSON-RPC 错误体。
pub fn dispatch_rpc_http(body: &str, head: &RequestHead) -> (u16, String) {
    stats::record_request();
    // 可能有批量和单条两种；MCP 客户端实际只发单条，这里也只处理单条。
    let parsed: protocol::JsonRpcMessage = match serde_json::from_str(body) {
        Ok(m) => m,
        Err(_) => {
            let resp = protocol::error_response(&None, protocol::PARSE_ERROR, "parse error");
            // modern 语义：请求没进 JSON-RPC 层，按 400 拒绝；legacy 保持 200。
            let status = if head.protocol_version.is_some() {
                400
            } else {
                200
            };
            return (status, encode(&resp));
        }
    };

    let id = parsed.id.clone();
    let method = parsed.method.as_deref().unwrap_or("");
    let params = parsed.params.as_ref().unwrap_or(&Value::Null);
    let is_notification = parsed.id.is_none();

    // ---- 版本协商 ----
    let meta_version = protocol::requested_version_from_meta(params);
    let declared = match (&head.protocol_version, &meta_version) {
        (Some(h), Some(m)) if h != m => {
            let resp = protocol::header_mismatch_error(
                &id,
                &format!(
                    "MCP-Protocol-Version header '{}' does not match _meta protocolVersion '{}'",
                    h, m
                ),
            );
            return (400, encode(&resp));
        }
        (Some(h), _) => Some(h.clone()),
        (None, m) => m.clone(),
    };
    if let Some(v) = &declared {
        if !protocol::SUPPORTED_PROTOCOL_VERSIONS.contains(&v.as_str()) {
            let resp = protocol::unsupported_version_error(&id, v);
            return (400, encode(&resp));
        }
    }
    let modern = declared
        .as_deref()
        .map(protocol::is_modern_version)
        .unwrap_or(false);

    // ---- modern 时代的镜像请求头校验（头/体不一致 → -32020）----
    // 规范要求 modern 客户端带 Mcp-Method / Mcp-Name 镜像头；这里只在头实际
    // 存在时校验一致性，缺失时放行 —— 双时代端点要对不完整的实现保持宽容。
    if modern {
        if let Some(hm) = &head.mcp_method {
            if hm != method {
                let resp = protocol::header_mismatch_error(
                    &id,
                    &format!(
                        "Mcp-Method header '{}' does not match body method '{}'",
                        hm, method
                    ),
                );
                return (400, encode(&resp));
            }
        }
        if let Some(hn) = &head.mcp_name {
            let decoded = decode_mcp_name(hn);
            let body_name = params.get("name").and_then(Value::as_str).unwrap_or("");
            if decoded.as_deref() != Some(body_name) {
                let resp = protocol::header_mismatch_error(
                    &id,
                    &format!(
                        "Mcp-Name header '{}' does not match body name '{}'",
                        hn, body_name
                    ),
                );
                return (400, encode(&resp));
            }
        }
    }

    // modern：通知（无 id）一律 202 空体 —— 规范规定接受通知时不得返回响应体。
    if modern && is_notification {
        return (202, String::new());
    }

    let resp = match method {
        // discover 对两个时代都应答 —— modern 客户端靠它探测时代与版本，
        // 应答一个版本全带的 discover 是最友好的双时代行为。
        "server/discover" => protocol::ok_response(&id, protocol::make_discover_result()),
        "initialize" => protocol::ok_response(&id, protocol::make_initialize_result(params)),
        "notifications/initialized" => {
            // legacy：通知不返回业务内容，回一个 id 为 null 的空响应（200）。
            // modern 客户端走上面的 202 空体分支，到不了这里。
            return (200, r#"{"jsonrpc":"2.0","id":null}"#.to_string());
        }
        "ping" => protocol::ok_response(
            &id,
            protocol::with_result_type(serde_json::json!({}), modern),
        ),
        "tools/list" => {
            // ListToolsResult 继承 CacheableResult：modern 时代除 resultType 外
            // 还必须带 ttlMs / cacheScope，缺失会被客户端判为无效结果。
            // 全局搜索档位影响 search_everywhere 的描述前缀与注解（形状不变）。
            let result = protocol::with_result_type(
                protocol::make_tools_list(crate::options::global_search_mode()),
                modern,
            );
            let result = protocol::with_cache_control(result, modern, protocol::TOOLS_LIST_TTL_MS);
            protocol::ok_response(&id, result)
        }
        "tools/call" => {
            let name = params.get("name").and_then(Value::as_str).unwrap_or("");
            let args = params.get("arguments").unwrap_or(&Value::Null);
            let idx = stats::tool_index(name);
            let t0 = std::time::Instant::now();
            let resp = match tools::dispatch(name, args) {
                Ok((content, is_error)) => {
                    let mut result = serde_json::json!(content);
                    if is_error {
                        result["isError"] = serde_json::Value::Bool(true);
                    }
                    protocol::ok_response(&id, protocol::with_result_type(result, modern))
                }
                Err((code, msg)) => protocol::error_response(&id, code, &msg),
            };
            // 三类结果都要记：Ok((_,false))=ok、Ok((_,true))/Err(_)=err。
            // 响应只序列化一次：埋点字节数按最终响应算，返回时直接复用。
            let ok = stats::dispatch_ok_flag(&resp).unwrap_or(false);
            let encoded = encode(&resp);
            stats::record_call(
                idx,
                ok,
                t0.elapsed().as_millis() as u64,
                encoded.len() as u64,
            );
            return (200, encoded);
        }
        _ => {
            // modern：未知方法 404 + JSON-RPC 错误体；legacy：保持 200。
            let resp = protocol::error_response(
                &id,
                protocol::METHOD_NOT_FOUND,
                &format!("method '{}' not found", method),
            );
            let status = if modern { 404 } else { 200 };
            return (status, encode(&resp));
        }
    };

    (200, encode(&resp))
}

// ====================================================================
// Origin 校验（防 DNS rebinding）
// ====================================================================

/// 校验 Origin 请求头。返回 `Some(错误响应体)` 表示应拒绝（403）。
///
/// 放行：缺失 / 空（非浏览器客户端）、`null`（沙箱 iframe 等不透明来源）、
/// localhost / 127.0.0.1 / [::1] 来源（本机 MCP 客户端与网页工具的正常形态）。
/// 其余一律拒绝 —— 远程网站经 DNS rebinding 打到本机端点的场景。
pub fn origin_error(origin: Option<&str>) -> Option<String> {
    let origin = origin?;
    if origin.trim().is_empty() || origin_allowed(origin) {
        return None;
    }
    let resp = protocol::error_response(&None, protocol::INVALID_REQUEST, "origin not allowed");
    Some(encode(&resp))
}

/// Origin 是否属于本机来源。
fn origin_allowed(origin: &str) -> bool {
    let o = origin.trim();
    if o.eq_ignore_ascii_case("null") {
        return true;
    }
    let rest = match o
        .strip_prefix("http://")
        .or_else(|| o.strip_prefix("https://"))
    {
        Some(r) => r,
        None => return false,
    };
    // 取 authority（去掉路径），再去掉 userinfo。
    let authority = rest.split('/').next().unwrap_or("");
    let host_port = authority.rsplit('@').next().unwrap_or(authority);
    // IPv6 字面量带方括号：[::1]:8080。
    let host = if let Some(end) = host_port.find(']') {
        &host_port[..=end]
    } else {
        host_port.split(':').next().unwrap_or(host_port)
    };
    let host = host.trim_matches(|c| c == '[' || c == ']');
    host.eq_ignore_ascii_case("localhost") || host == "127.0.0.1" || host == "::1"
}

// ====================================================================
// Mcp-Name 镜像头解码（base64 sentinel）
// ====================================================================

/// 解析 Mcp-Name 请求头值：base64 sentinel（`=?base64?…?=`）先解码，
/// 其余原样返回。无法解码时返回 None（调用方按不匹配处理）。
fn decode_mcp_name(value: &str) -> Option<String> {
    if let Some(inner) = value
        .strip_prefix("=?base64?")
        .and_then(|v| v.strip_suffix("?="))
    {
        return base64_decode(inner).and_then(|b| String::from_utf8(b).ok());
    }
    Some(value.to_string())
}

/// 标准 base64 解码（带填充，容忍换行）。仅供 sentinel 解析使用。
fn base64_decode(s: &str) -> Option<Vec<u8>> {
    fn val(c: u8) -> Option<u8> {
        match c {
            b'A'..=b'Z' => Some(c - b'A'),
            b'a'..=b'z' => Some(c - b'a' + 26),
            b'0'..=b'9' => Some(c - b'0' + 52),
            b'+' => Some(62),
            b'/' => Some(63),
            _ => None,
        }
    }
    let bytes: Vec<u8> = s.bytes().filter(|&b| b != b'\r' && b != b'\n').collect();
    if !bytes.len().is_multiple_of(4) {
        return None;
    }
    let mut out = Vec::with_capacity(bytes.len() / 4 * 3);
    for chunk in bytes.chunks(4) {
        let mut n = 0u32;
        let mut pad = 0;
        for (i, &c) in chunk.iter().enumerate() {
            if c == b'=' {
                if i < 2 {
                    return None; // 填充只能出现在末尾
                }
                pad += 1;
                continue;
            }
            if pad > 0 {
                return None; // 填充后还有数据
            }
            n |= (val(c)? as u32) << (18 - 6 * i);
        }
        out.push((n >> 16) as u8);
        if pad < 2 {
            out.push((n >> 8) as u8);
        }
        if pad < 1 {
            out.push(n as u8);
        }
    }
    Some(out)
}

// ====================================================================
// HTTP 响应构造
// ====================================================================

/// 构造任意状态码的 HTTP 响应（CORS 头照旧放开，方便浏览器内 MCP 工具）。
pub fn http_response(status: u16, body: &str) -> String {
    let reason = match status {
        200 => "OK",
        202 => "Accepted",
        400 => "Bad Request",
        403 => "Forbidden",
        404 => "Not Found",
        405 => "Method Not Allowed",
        _ => "OK",
    };
    let mut s = String::with_capacity(body.len() + 256);
    s.push_str(&format!("HTTP/1.1 {} {}\r\n", status, reason));
    s.push_str("Content-Type: application/json\r\n");
    s.push_str(&format!("Content-Length: {}\r\n", body.len()));
    s.push_str("Access-Control-Allow-Origin: *\r\n");
    s.push_str("Access-Control-Allow-Methods: POST, OPTIONS\r\n");
    s.push_str(
        "Access-Control-Allow-Headers: Content-Type, Accept, MCP-Protocol-Version, Mcp-Method, Mcp-Name\r\n",
    );
    s.push_str("Connection: close\r\n");
    s.push_str("\r\n");
    s.push_str(body);
    s
}

/// 200 + JSON 响应体。
pub fn ok_http_response(body: &str) -> String {
    http_response(200, body)
}

/// 403 + JSON 响应体（Origin 校验拒绝时用）。
pub fn forbidden(body: &str) -> String {
    http_response(403, body)
}

pub fn bad_request(reason: &str) -> String {
    let body = format!("{{\"error\":\"{}\"}}", reason.replace('"', "\\\""));
    format!(
        "HTTP/1.1 400 Bad Request\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
        body.len(),
        body
    )
}

pub fn not_allowed() -> String {
    "HTTP/1.1 405 Method Not Allowed\r\nAllow: POST\r\nContent-Length: 0\r\nConnection: close\r\n\r\n".into()
}
