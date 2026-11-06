//! server.rs — MCP HTTP 服务（基于 std::net，单线程-per-conn）
//!
//! 传输层规格（dual-era：MCP 2024-11-05 legacy + 2026-07-28 modern）：
//!   - 客户端 POST 单条 JSON-RPC 请求到服务端根路径（`/` 或 `/mcp`）；
//!   - 服务端返回 `Content-Type: application/json` 的单条 JSON-RPC 响应；
//!   - **不是**带 SSE 的 Streamable HTTP：不实现 SSE 长连接通道；
//!   - 客户端可发起 GET / 建立长连接监听服务端通知，我们不支持 —— 直接返回 405
//!     （2026-07-28 已移除 GET 流端点，405 同时符合两代规范）。
//!
//! 版本协商（2026-07-28）：modern 客户端在每个请求的 `_meta` 里带
//! `io.modelcontextprotocol/protocolVersion`，并应在 `MCP-Protocol-Version`
//! 请求头里带同一个值。时代判定：
//!   - 声明 modern 版本 → modern 语义：server/discover 可用、未知方法 404、
//!     通知 202 空体、头/体不一致 -32020、版本不支持 400 + -32022、
//!     每个 result 带 resultType（2026-07-28 MUST），tools/list 与
//!     server/discover 的结果另带 ttlMs / cacheScope（CacheableResult 必填）；
//!   - 声明 legacy 版本或什么都没带 → legacy 语义：initialize 握手、
//!     请求错误返回 200 + JSON-RPC 错误体；通知统一返回 202 空体。
//!
//! 安全：校验 Origin 头防 DNS rebinding（非 localhost 来源 403）。
//!
//! 关闭机制：使用 `shutdown` AtomicBool 与一个 `Mutex<Option<JoinHandle>>`
//! 保存监听线程句柄，PM_STOP 时调用 `stop()` 标记关闭、join 线程。

use std::io::{Read, Write};
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, Shutdown, SocketAddr, TcpListener, TcpStream};
use std::os::windows::io::AsRawSocket;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::Mutex;
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};
use windows_sys::Win32::Networking::WinSock::{WSAPoll, POLLRDNORM, WSAPOLLFD};

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

/// 全局停机标志：停止时拒绝新的宿主任务；排队和查询等待均检查此标志。
static STOPPING: AtomicBool = AtomicBool::new(false);

/// 搜索等待循环查询的停机信号。服务未运行（测试直接走 dispatch 的场景、
/// 或尚未 start）时恒为 true？不 —— 未运行时查询本身不会发生（HTTP 层
/// 是唯一入口），false 是安全默认：标志只表达「有人在关」。
pub fn shutting_down() -> bool {
    STOPPING.load(Ordering::SeqCst)
}

/// 在途连接线程的句柄注册表。连接线程绝不能活过 FreeLibrary —— 否则它
/// 从系统调用返回后执行的是已卸载的 DLL 代码，Everything.exe 直接崩
/// （与 stats.rs 刷盘线程的 join 保障是同一条不变量）。stop() join 完
/// 监听线程后必须把这里也 join 干净；已完成句柄由监听线程 join 后回收。
static CONN_THREADS: Mutex<Vec<JoinHandle<()>>> = Mutex::new(Vec::new());
static CONNECTIONS: Mutex<Vec<(usize, TcpStream)>> = Mutex::new(Vec::new());
static NEXT_CONNECTION: AtomicUsize = AtomicUsize::new(1);

thread_local! {
    // Direct dispatch/tests have no HTTP lifecycle; cancellation belongs to a request.
    static REQUEST_SHUTDOWN: std::cell::RefCell<Option<std::sync::Arc<AtomicBool>>> = const { std::cell::RefCell::new(None) };
}

pub(crate) fn request_cancelled() -> bool {
    REQUEST_SHUTDOWN.with(|slot| {
        slot.borrow()
            .as_ref()
            .is_some_and(|s| s.load(Ordering::Acquire))
    })
}

struct ConnectionGuard(usize);

impl Drop for ConnectionGuard {
    fn drop(&mut self) {
        CONNECTIONS
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .retain(|(id, _)| *id != self.0);
        INFLIGHT.fetch_sub(1, Ordering::SeqCst);
    }
}

struct ServerHandle {
    shutdown: std::sync::Arc<AtomicBool>,
    /// 实际绑定的地址：`port` 传 0 时由系统分配，调用方用 [`bound_port`] 查。
    local: SocketAddr,
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

/// 请求体上限。MCP 单条 JSON-RPC 请求实测在几十 KB 量级（tools/list 的
/// 响应才是大头）；1 MiB 已是宽松上界。没有它，伪造 Content-Length 的
/// 请求能把内存灌到 OOM —— panic=abort 下直接带走宿主进程。
const MAX_BODY_BYTES: usize = 1024 * 1024;

/// 启动 HTTP 服务并立即返回。失败返回错误描述。
pub fn start(bind: &str, port: u16) -> Result<(), String> {
    // 已有服务在跑 —— 关闭旧的再启新的（无服务时是 no-op）。
    stop();
    STOPPING.store(false, Ordering::SeqCst);

    let addr = format!("{}:{}", bind, port);
    let listener = TcpListener::bind(&addr).map_err(|e| format!("bind {} failed: {}", addr, e))?;
    listener
        .set_nonblocking(true)
        .map_err(|e| format!("nonblocking listener: {e}"))?;
    // Windows 上 TCPListener::bind 默认已开启 SO_EXCLUSIVEADDRUSE/SO_REUSEADDR，
    // 服务退出后立即重启不会因为 TIME_WAIT 失败。

    // 唤醒地址：bind 0.0.0.0/[::] 时换成对应的 loopback，自连接才连得到。
    let local = listener.local_addr();
    let wakeup = match &local {
        Ok(a) => match a.ip() {
            IpAddr::V4(ip) if ip.is_unspecified() => {
                SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), a.port())
            }
            IpAddr::V6(ip) if ip.is_unspecified() => {
                SocketAddr::new(IpAddr::V6(Ipv6Addr::LOCALHOST), a.port())
            }
            _ => *a,
        },
        Err(_) => SocketAddr::from(([127, 0, 0, 1], port)),
    };
    let local = local.unwrap_or(wakeup);

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
        local,
        wakeup,
        thread: Mutex::new(Some(handle)),
    });
    Ok(())
}

/// 本次启动实际绑定的端口。`start(…, 0)` 时端口由系统临时分配 —— 测试用它
/// 拿到真实端口，避免固定端口在共享环境（CI runner）里偶发被占导致 bind 失败。
pub fn bound_port() -> Option<u16> {
    let slot = SERVER.lock().unwrap_or_else(|p| p.into_inner());
    slot.as_ref().map(|s| s.local.port())
}

/// 关闭服务（PM_STOP / PM_KILL 时调用）。
///
/// 顺序：通知停止 → join 监听线程 → shutdown 在途 socket → join 工作线程。
/// socket 读写可被主动中断；文件 I/O 在系统调用之间协作取消，Windows/网络
/// 驱动中的阻塞调用及已执行的宿主 FFI 仍必须等待安全返回。
///
/// join 必须在 PM_STOP 返回前完成：之后宿主会销毁窗口、释放 db 并
/// FreeLibrary —— 任何活过这一步的连接线程都在执行已卸载的代码。
pub fn stop() {
    // 先把句柄整体取走再 join：不持有 SERVER 锁等待线程退出，
    // stop 幂等（第二次调用拿到 None 直接返回）。
    let taken = {
        let mut slot = SERVER.lock().unwrap_or_else(|p| p.into_inner());
        slot.take()
    };
    STOPPING.store(true, Ordering::SeqCst);
    if let Some(s) = taken {
        s.shutdown.store(true, Ordering::SeqCst);

        // Wake the readiness wait promptly. Even if this fails, WSAPoll has a
        // finite timeout: the listener must ALWAYS be joined before DLL unload.
        let _ = TcpStream::connect_timeout(&s.wakeup, Duration::from_millis(100));
        let joined = s.thread.lock().unwrap_or_else(|p| p.into_inner()).take();
        if let Some(t) = joined {
            let _ = t.join();
        }
    }

    // No further registrations after joining the listener. Shutdown interrupts
    // both blocked reads and writes on the worker's cloned socket handle.
    for (_, stream) in CONNECTIONS.lock().unwrap_or_else(|p| p.into_inner()).iter() {
        let _ = stream.shutdown(Shutdown::Both);
    }

    // Take ownership of every remaining worker and wait for all thread cleanup.
    let handles = {
        let mut reg = CONN_THREADS.lock().unwrap_or_else(|p| p.into_inner());
        core::mem::take(&mut *reg)
    };
    for t in handles {
        let _ = t.join();
    }
}

fn run_listener(listener: TcpListener, shutdown: std::sync::Arc<AtomicBool>) {
    // Wait for readiness rather than sleeping between accepts. A finite poll
    // also guarantees shutdown if the self-connect wakeup fails.
    while !shutdown.load(Ordering::SeqCst) {
        let mut fd = WSAPOLLFD {
            fd: listener.as_raw_socket() as _,
            events: POLLRDNORM,
            revents: 0,
        };
        let ready = unsafe { WSAPoll(&mut fd, 1, 200) };
        if ready == 0 {
            continue;
        }
        if ready < 0 {
            crate::plugin::host::Host::debug("everything_mcp: listener poll failed");
            break;
        }
        match listener.accept() {
            Ok((mut stream, _peer)) => {
                // 唤醒连接或停机后才到达的连接：直接退出循环，套接字丢弃。
                if shutdown.load(Ordering::SeqCst) {
                    break;
                }
                if stream.set_nonblocking(false).is_err() {
                    continue;
                }
                // 并发上限：满了 503 拒掉，不再开线程。
                if INFLIGHT.load(Ordering::Relaxed) >= MAX_CONNECTIONS {
                    let _ = stream.set_write_timeout(Some(Duration::from_millis(200)));
                    let _ = stream.write_all(
                        b"HTTP/1.1 503 Service Unavailable\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
                    );
                    continue;
                }
                stats::record_connection();
                let Ok(control) = stream.try_clone() else {
                    continue;
                };
                let id = NEXT_CONNECTION.fetch_add(1, Ordering::Relaxed);
                CONNECTIONS
                    .lock()
                    .unwrap_or_else(|p| p.into_inner())
                    .push((id, control));
                INFLIGHT.fetch_add(1, Ordering::SeqCst);
                let s_clone = shutdown.clone();
                let spawned = thread::Builder::new()
                    .name("everything-mcp-conn".into())
                    .spawn(move || {
                        let _guard = ConnectionGuard(id);
                        REQUEST_SHUTDOWN.with(|slot| *slot.borrow_mut() = Some(s_clone.clone()));
                        let _ = handle_connection(stream, s_clone);
                    });
                match spawned {
                    Ok(handle) => {
                        let mut reg = CONN_THREADS.lock().unwrap_or_else(|p| p.into_inner());
                        let mut i = 0;
                        while i < reg.len() {
                            if reg[i].is_finished() {
                                // is_finished may precede the last thread cleanup;
                                // join even completed threads before discarding handles.
                                let _ = reg.swap_remove(i).join();
                            } else {
                                i += 1;
                            }
                        }
                        reg.push(handle);
                    }
                    Err(_) => {
                        drop(ConnectionGuard(id));
                    }
                }
            }
            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {}
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

    let (req_head, leftover) = match read_http_head(&mut stream, deadline) {
        Ok(r) => r,
        Err(e) => {
            let body = format!("HTTP parse error: {}", e);
            let _ = stream.write_all(bad_request(&body).as_bytes());
            return Ok(());
        }
    };

    // 防线顺序（2026-10 审计修复）：方法 / 路径 / Origin 的校验必须发生在
    // 读取 body 之前 —— 否则不可信来源可以先用超大 body 耗尽资源，再等
    // 校验被拒，防线形同虚设。读 body 需要的信息只有 Content-Length。
    if req_head.method == "OPTIONS" {
        // 浏览器对带 JSON 的 POST 必发预检。按 Origin 白名单应答：
        // 放行的来源回 204 + 精确回显，其余 403 —— 预检结果与实际
        // POST 的命运一致，不出现「预检过了、请求被拒」的裂缝。
        let body = match req_head.origin.as_deref() {
            Some(o) if origin_allowed(o) => preflight_response(o.trim()),
            _ => forbidden(r#"{"error":"origin not allowed"}"#),
        };
        let _ = stream.write_all(body.as_bytes());
        return Ok(());
    }
    if req_head.method != "POST" || (req_head.path != "/" && req_head.path != "/mcp") {
        let _ = stream.write_all(with_cors(not_allowed(), req_head.origin.as_deref()).as_bytes());
        return Ok(());
    }
    if let Some(body) = origin_error(req_head.origin.as_deref()) {
        let _ = stream.write_all(forbidden(&body).as_bytes());
        return Ok(());
    }

    let req = match read_http_body(&mut stream, &req_head, leftover, deadline) {
        Ok(r) => r,
        Err(e) => {
            let body = format!("HTTP body error: {}", e);
            let _ = stream
                .write_all(with_cors(bad_request(&body), req_head.origin.as_deref()).as_bytes());
            return Ok(());
        }
    };

    // dispatch_rpc_http 在本工作线程上执行：HTTP 解析、JSON-RPC 分发、tools/list /
    // ping 都不接触 Everything DB，留在工作线程上最自然也最快。只有 tools/call 命中
    // search_in_folder 时，search_in_folder 内部会自行把 db_query_search2 marshal
    // 到 Everything 主线程；调用方无需关心。
    let (status, body) = dispatch_rpc_http(&req.body, &req_head);

    let resp = with_cors(http_response(status, &body), req_head.origin.as_deref());
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

/// 已完整收到的请求。head 在读取前的校验阶段用过；进入分发时只需要
/// body（head 由 handle_connection 持有的 req_head 携带）。
struct HttpRequest {
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
    loop {
        let remain = deadline.saturating_duration_since(Instant::now());
        if remain.is_zero() || request_cancelled() {
            return Err(std::io::Error::new(
                std::io::ErrorKind::TimedOut,
                "connection cancelled or deadline exceeded",
            ));
        }
        stream.set_read_timeout(Some(remain.min(Duration::from_millis(200))))?;
        match stream.read(chunk) {
            Err(e)
                if matches!(
                    e.kind(),
                    std::io::ErrorKind::WouldBlock
                        | std::io::ErrorKind::TimedOut
                        | std::io::ErrorKind::Interrupted
                ) =>
            {
                continue
            }
            result => return result,
        }
    }
}

/// 读取并解析一个 HTTP 请求头（不含 body）。
///
/// body 由 [`read_http_body`] 在方法/Origin 校验通过后单独读取 ——
/// 校验必须发生在读 body 之前，两者分开才能保证这个顺序。
///
/// 头部按块读进字节缓冲：头部原来逐字节 read（几百字节的头部
/// 就是几百次系统调用）。
fn read_http_head(
    stream: &mut TcpStream,
    deadline: Instant,
) -> std::io::Result<(RequestHead, Vec<u8>)> {
    let mut buf = Vec::with_capacity(4096);
    let mut chunk = [0u8; 4096];
    let mut search_from = 0usize;
    let head_end = loop {
        if let Some(pos) = find_head_end(&buf, search_from) {
            if pos > 65536 {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::InvalidData,
                    "header too large",
                ));
            }
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

    let header_str = std::str::from_utf8(&buf[..head_end])
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
    let head = parse_header(header_str)
        .ok_or_else(|| std::io::Error::new(std::io::ErrorKind::InvalidData, "no request line"))?;
    // Content-Length 在头部阶段就拦：声明超过上限的请求直接拒绝，绝不
    // 进入读 body 循环 —— 否则读校验顺序颠倒，资源先被消耗、Origin/方法
    // 检查在后，防线形同虚设。
    if head.content_length > MAX_BODY_BYTES {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "body too large",
        ));
    }
    // 头部结束符之后可能已经带进来一段 body 字节（整条请求常在一个 TCP
    // 段里到齐）—— 交给 read_http_body，否则这些字节就被丢了。
    let leftover = buf[(head_end + 4).min(buf.len())..].to_vec();
    Ok((head, leftover))
}

/// 校验通过后读取请求体。
///
/// `leftover` 是 [`read_http_head`] 里头部结束符之后已经读到的字节
/// （整条请求常在一个 TCP 段里到齐）；不足的部分从流上继续读。
fn read_http_body(
    stream: &mut TcpStream,
    head: &RequestHead,
    leftover: Vec<u8>,
    deadline: Instant,
) -> std::io::Result<HttpRequest> {
    let mut chunk = [0u8; 4096];
    // Content-Length 已在 read_http_head 里验证过 <= MAX_BODY_BYTES。
    let mut body_bytes = leftover;
    while body_bytes.len() < head.content_length {
        let take = chunk.len().min(head.content_length - body_bytes.len());
        let n = read_bounded(stream, &mut chunk[..take], deadline)?;
        if n == 0 {
            return Err(std::io::Error::new(
                std::io::ErrorKind::UnexpectedEof,
                "incomplete request body",
            ));
        }
        body_bytes.extend_from_slice(&chunk[..n]);
    }
    // 管道化（同一段里塞了两条请求）时丢弃超出 Content-Length 的部分，
    // 保持「一次连接只处理一条请求」的既有语义。
    body_bytes.truncate(head.content_length);

    let body = String::from_utf8(body_bytes)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
    Ok(HttpRequest { body })
}

/// 解析已读完的 HTTP 头部文本，取出请求行与关注的头部字段。
///
/// `header` 是到（含）`\r\n\r\n` 为止的全部字节；首行必须是
/// `METHOD SP PATH SP VERSION`，否则返回 None。Content-Length
/// 缺失按 0 处理；非法值、冲突长度、Transfer-Encoding 均拒绝。头名不区分大小写。
pub fn parse_header(header: &str) -> Option<RequestHead> {
    let mut lines = header.split("\r\n");
    let request_line = lines.next()?;
    let mut parts = request_line.split(' ');
    let method = parts.next()?;
    let path = parts.next()?;
    let version = parts.next()?;
    if parts.next().is_some()
        || !matches!(version, "HTTP/1.0" | "HTTP/1.1")
        || method.is_empty()
        || !method.bytes().all(|b| b.is_ascii_uppercase())
        || !path.starts_with('/')
        || path
            .bytes()
            .any(|b| b.is_ascii_whitespace() || b.is_ascii_control())
    {
        return None;
    }
    let mut head = RequestHead {
        method: method.into(),
        path: path.into(),
        ..RequestHead::default()
    };
    let mut length = None;
    for line in lines {
        if line.is_empty() {
            break;
        }
        let (name, value) = line.split_once(':')?;
        if name.is_empty()
            || !name
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b"!#$%&'*+-.^_`|~".contains(&b))
        {
            return None;
        }
        let value = value.trim_matches([' ', '\t']);
        if value.chars().any(|c| c.is_control() && c != '\t') {
            return None;
        }
        let slot = match name.to_ascii_lowercase().as_str() {
            "content-length" => {
                if value.is_empty() || !value.bytes().all(|b| b.is_ascii_digit()) {
                    return None;
                }
                let n = value.parse::<usize>().ok()?;
                if length.is_some_and(|previous| previous != n) {
                    return None;
                }
                length = Some(n);
                head.content_length = n;
                continue;
            }
            // Chunked requests are unsupported; never interpret them as CL bodies.
            "transfer-encoding" => return None,
            "mcp-protocol-version" => &mut head.protocol_version,
            "origin" => &mut head.origin,
            "mcp-method" => &mut head.mcp_method,
            "mcp-name" => &mut head.mcp_name,
            _ => continue,
        };
        if slot.is_some() {
            return None;
        }
        *slot = Some(value.to_string());
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
///
/// 两种时代接受通知时均返回 202 空体，不生成 JSON-RPC 响应。
pub fn dispatch_rpc_http(body: &str, head: &RequestHead) -> (u16, String) {
    stats::record_request();
    let error_status = if head.protocol_version.is_some() {
        400
    } else {
        200
    };
    let envelope: Value = match serde_json::from_str(body) {
        Ok(value) => value,
        Err(_) => {
            return (
                error_status,
                encode(&protocol::error_response(
                    &None,
                    protocol::PARSE_ERROR,
                    "parse error",
                )),
            )
        }
    };
    let valid_id = envelope
        .get("id")
        .is_none_or(|id| id.is_null() || id.is_string() || id.is_number());
    let valid = envelope.is_object()
        && envelope.get("jsonrpc").and_then(Value::as_str) == Some("2.0")
        && envelope.get("method").is_some_and(Value::is_string)
        && envelope
            .get("params")
            .is_none_or(|p| p.is_object() || p.is_array())
        && envelope.get("result").is_none()
        && envelope.get("error").is_none()
        && valid_id;
    if !valid {
        return (
            error_status,
            encode(&protocol::error_response(
                &None,
                protocol::INVALID_REQUEST,
                "invalid JSON-RPC 2.0 request",
            )),
        );
    }
    // Preserve explicit null IDs: only an ABSENT id denotes a notification.
    let id = envelope.get("id").cloned();
    let method = envelope["method"].as_str().unwrap_or("");
    let params = envelope.get("params").unwrap_or(&Value::Null);
    let is_notification = id.is_none();

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

    // 两种协议的通知均不返回 JSON-RPC 响应。旧版的 200 + {jsonrpc, id:null}
    // 不是合法响应，会使 Codex/rmcp 在 initialized 阶段反序列化失败并断开。
    if is_notification {
        return (202, String::new());
    }

    let resp = match method {
        // discover 对两个时代都应答 —— modern 客户端靠它探测时代与版本，
        // 应答一个版本全带的 discover 是最友好的双时代行为。
        "server/discover" => protocol::ok_response(&id, protocol::make_discover_result()),
        "initialize" => protocol::ok_response(&id, protocol::make_initialize_result(params)),
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
            if name.is_empty() || (!args.is_null() && !args.is_object()) {
                return (
                    200,
                    encode(&protocol::error_response(
                        &id,
                        protocol::INVALID_PARAMS,
                        "tools/call requires name and an arguments object",
                    )),
                );
            }
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
            // （通知在上面的 is_notification 早返回里统一 202 空体。）
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
/// 放行：缺失 / 空（非浏览器客户端）、localhost / 127.0.0.1 / [::1] 来源
/// （本机 MCP 客户端与网页工具的正常形态）。其余一律拒绝。
///
/// `null` 同样拒绝（2026-10 审计修复）：沙箱 iframe、file:// 页面与某些
/// 重定向场景的 Origin 都是 `null` —— 远程攻击页恰好能用 sandbox iframe
/// 造出这个值穿过校验。本机浏览器内 MCP 工具运行在 http(s)://localhost
/// 下，不会发 `null`。
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
    let rest = match o
        .strip_prefix("http://")
        .or_else(|| o.strip_prefix("https://"))
    {
        Some(r) => r,
        None => return false,
    };
    // Origins contain only a scheme and authority; reject paths/userinfo and
    // malformed ports instead of normalizing a non-origin URL into the allowlist.
    if rest.contains(['/', '@', '?', '#']) || rest.chars().any(char::is_whitespace) {
        return false;
    }
    let port = if let Some(tail) = rest.strip_prefix("[::1]") {
        tail
    } else {
        rest.split_once(':').map_or("", |(_, p)| p)
    };
    if rest.starts_with("[::1]") {
        if !port.is_empty()
            && !port.strip_prefix(':').is_some_and(|p| {
                !p.is_empty() && p.bytes().all(|b| b.is_ascii_digit()) && p.parse::<u16>().is_ok()
            })
        {
            return false;
        }
    } else if rest.contains(':')
        && (port.is_empty()
            || !port.bytes().all(|b| b.is_ascii_digit())
            || port.parse::<u16>().is_err())
    {
        return false;
    }
    // The authority is now validated.
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

/// 构造任意状态码的 HTTP 响应。
///
/// Actual responses use with_cors only after an allowlisted Origin is parsed.
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
    s.push_str("Connection: close\r\n");
    s.push_str("\r\n");
    s.push_str(body);
    s
}

fn with_cors(mut response: String, origin: Option<&str>) -> String {
    if let Some(origin) = origin.filter(|o| origin_allowed(o)) {
        if let Some(end) = response.find("\r\n\r\n") {
            response.insert_str(
                end + 2,
                &format!(
                    "Access-Control-Allow-Origin: {}\r\nVary: Origin\r\n",
                    origin.trim()
                ),
            );
        }
    }
    response
}

/// OPTIONS 预检的 204 应答。`origin` 已通过白名单校验（调用方负责），
/// CORS 头精确回显它 —— 任何页面（无论多远）都拿不到可读响应。
fn preflight_response(origin: &str) -> String {
    let mut s = String::with_capacity(320);
    s.push_str("HTTP/1.1 204 No Content\r\n");
    s.push_str(&format!("Access-Control-Allow-Origin: {}\r\n", origin));
    s.push_str("Vary: Origin\r\n");
    s.push_str("Access-Control-Allow-Methods: POST, OPTIONS\r\n");
    s.push_str(
        "Access-Control-Allow-Headers: Content-Type, Accept, MCP-Protocol-Version, Mcp-Method, Mcp-Name\r\n",
    );
    s.push_str("Access-Control-Max-Age: 600\r\n");
    s.push_str("Connection: close\r\n");
    s.push_str("\r\n");
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
    let body = serde_json::json!({"error": reason}).to_string();
    format!(
        "HTTP/1.1 400 Bad Request\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
        body.len(),
        body
    )
}

pub fn not_allowed() -> String {
    "HTTP/1.1 405 Method Not Allowed\r\nAllow: POST\r\nContent-Length: 0\r\nConnection: close\r\n\r\n".into()
}
