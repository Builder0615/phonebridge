//! MirrorAdapter：UxPlay AirPlay sidecar 的无界面封装。
//!
//! UxPlay 只负责 AirPlay 会话、解密和视频接收；画面出口按宿主平台选择：
//! macOS 读取 UxPlay 内置 GStreamer 直接输出的 RGBA，Windows 读取 UxPlay
//! 的 RTP 外部输出并由随应用分发的 ffmpeg 解码为 RGBA。用户可见的窗口始终
//! 是 Tauri canvas，UxPlay 不创建原生镜像窗口。

use std::io::{BufRead, BufReader, Read};
#[cfg(not(target_os = "macos"))]
use std::net::UdpSocket;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
#[cfg(not(target_os = "macos"))]
use std::sync::atomic::AtomicU8;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::Sender;
use std::sync::{Arc, Mutex};
use std::time::Duration;

#[cfg(target_os = "macos")]
use std::fs::File;
#[cfg(target_os = "macos")]
use std::os::unix::io::{AsRawFd, FromRawFd};
#[cfg(target_os = "macos")]
use std::os::unix::process::CommandExt;

use super::frame_bridge::{FrameSink, RgbaFrame};
use super::AdapterError;

const IOS_OUTPUT_WIDTH: u32 = 480;
const IOS_OUTPUT_HEIGHT: u32 = 1040;
#[cfg(not(target_os = "macos"))]
const MAX_DECODER_LOG_LINES: usize = 24;

/// 镜像子进程事件（结构化，语义稳定）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MirrorEvent {
    /// 解析到视频尺寸（只表示流元数据，不代表已经收到画面）。
    Metadata { width: u32, height: u32 },
    /// AirPlay 镜像会话已经建立；首帧可能还在解码器中等待。
    ///
    /// AirPlay 的 RTSP 控制连接和视频首帧不是同一个时刻到达。不能把
    /// “已连接”继续伪装成“等待手机连接”，否则面板状态会一直错误，
    /// 也无法区分是手机没有连接还是视频解码链路没有产出帧。
    MirrorConnected,
    /// 解码器已经产出第一帧。
    FirstFrame,
    /// 子进程或视频接收链路意外退出。
    Crashed {
        source: &'static str,
        exit_code: Option<i32>,
    },
    /// 脱敏后的日志行。
    LogLine(String),
}

/// 镜像适配器接口。
pub trait MirrorAdapter: Send + Sync {
    fn name(&self) -> &'static str;
    /// 启动镜像接收（异步：事件经 tx 上报）。失败返回可理解错误。
    fn start(&mut self, tx: Sender<MirrorEvent>) -> Result<(), AdapterError>;
    /// 停止镜像并回收子进程。
    fn stop(&mut self) -> Result<(), AdapterError>;
    /// 最近解析到的视频尺寸。
    fn metadata(&self) -> Option<(u32, u32)>;
    /// 绑定统一 RGBA 帧出口；默认实现供没有主动推帧的适配器兼容。
    fn attach_sink(&mut self, _sink: Arc<dyn FrameSink>) {}
}

/// UxPlay 启动配置（白名单，本模块内部生成）。
pub struct UxPlayConfig {
    /// AirPlay 服务名（显示在 iPhone 屏幕镜像列表）。
    pub service_name: String,
    /// AirPlay 控制端口（UxPlay 的 -p 参数）。
    pub port: u16,
}

impl Default for UxPlayConfig {
    fn default() -> Self {
        Self {
            service_name: crate::APP_NAME.into(),
            port: 6000,
        }
    }
}

/// 解析二进制路径：资源目录优先，其次环境变量和系统安装位置。
fn resolve_uxplay_path(app_resources_dir: &Path) -> Result<PathBuf, String> {
    for bundled in uxplay_path_candidates(app_resources_dir) {
        if bundled.is_file() {
            return Ok(bundled);
        }
    }
    if let Ok(path) = std::env::var("PHONEBRIDGE_UXPLAY_PATH") {
        let path = PathBuf::from(path.trim());
        if path.is_file() {
            return Ok(path);
        }
    }
    super::adb::resolve_tool(app_resources_dir, "uxplay", "PHONEBRIDGE_UXPLAY_PATH")
        .map_err(|e| e.to_string())
}

/// 返回当前包和开发态中可能出现的 UxPlay 路径。
///
/// macOS 发布包把 UxPlay 放进 `uxplay-agent.app`，其 `LSUIElement` 属性阻止
/// GStreamer 的 macOS application wrapper 在 Dock 注册第二个前台应用。保留
/// 旧的 Contents/MacOS 路径用于升级过渡和孤儿进程回收。
fn uxplay_path_candidates(app_resources_dir: &Path) -> Vec<PathBuf> {
    let exe = if cfg!(target_os = "windows") {
        "uxplay.exe"
    } else {
        "uxplay"
    };
    let mut candidates = Vec::new();

    #[cfg(target_os = "macos")]
    {
        candidates.push(
            app_resources_dir
                .join("binaries/uxplay-agent.app/Contents/MacOS")
                .join(exe),
        );
        candidates.push(
            Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("binaries/uxplay-agent.app/Contents/MacOS")
                .join(exe),
        );
        // 旧版 Tauri externalBin 的发布位置。
        candidates.push(app_resources_dir.join("../MacOS").join(exe));
    }

    candidates.push(app_resources_dir.join("binaries").join(exe));
    candidates
}

/// 为随应用分发的 UxPlay 配置私有 GStreamer 运行时。
///
/// GStreamer 只属于 UxPlay 的 AirPlay 接收进程；macOS 还从它的内部管线读取
/// RGBA，Windows 则把 RTP 交给随包 ffmpeg。macOS 使用 dylib/插件，Windows
/// 使用 DLL/插件，二者都从应用资源目录加载，不能退回到用户机器上的
/// Homebrew、MSYS2 或系统安装。
fn configure_uxplay_runtime(command: &mut Command, uxplay_path: &Path) -> Option<PathBuf> {
    #[cfg(target_os = "macos")]
    {
        let mut candidates = Vec::new();
        let mut parent = uxplay_path.parent();
        // 同时覆盖：发布包中的 nested agent、旧版 Contents/MacOS sidecar，
        // 以及 target/debug 与 src-tauri/binaries 开发路径。
        while let Some(directory) = parent {
            candidates.push(directory.join("../Resources/binaries/gstreamer"));
            candidates.push(directory.join("../Resources/gstreamer"));
            candidates.push(directory.join("binaries/gstreamer"));
            candidates.push(directory.join("gstreamer"));
            candidates.push(directory.join("../gstreamer"));
            parent = directory.parent();
        }
        for runtime in candidates {
            let plugins = runtime.join("plugins");
            let scanner = runtime.join("libexec/gstreamer-1.0/gst-plugin-scanner");
            if !runtime.join("lib").is_dir() || !plugins.is_dir() || !scanner.is_file() {
                continue;
            }
            command
                .env("GST_PLUGIN_PATH", &plugins)
                .env("GST_PLUGIN_SYSTEM_PATH_1_0", &plugins)
                .env("GST_PLUGIN_SCANNER", &scanner)
                // 开发态 sidecar 的 @rpath 默认落在 target/debug/gstreamer，
                // 但运行时实际位于 target/debug/binaries/gstreamer；发布态
                // 也显式固定到 app 内运行时，避免依赖 Homebrew 环境。
                .env("DYLD_LIBRARY_PATH", runtime.join("lib"));
            return Some(runtime);
        }
    }

    #[cfg(target_os = "windows")]
    {
        let parent = uxplay_path.parent()?;
        let candidates = [
            parent.join("../resources/binaries/gstreamer"),
            parent.join("../Resources/binaries/gstreamer"),
            parent.join("binaries/gstreamer"),
            parent.join("gstreamer"),
        ];
        for runtime in candidates {
            let bin = runtime.join("bin");
            let plugins = runtime.join("plugins");
            let scanner_candidates = [
                runtime.join("libexec/gstreamer-1.0/gst-plugin-scanner.exe"),
                runtime.join("libexec/gstreamer-1.0/gst-plugin-scanner"),
            ];
            let Some(scanner) = scanner_candidates.iter().find(|path| path.is_file()) else {
                continue;
            };
            if !bin.is_dir() || !plugins.is_dir() {
                continue;
            }

            // GStreamer DLLs and UxPlay's MinGW runtime are colocated under
            // the app resource.  Prepend only that directory and preserve the
            // user's PATH for unrelated tools.
            let mut path_entries = vec![bin];
            if let Some(existing) = std::env::var_os("PATH") {
                path_entries.extend(std::env::split_paths(&existing));
            }
            if let Ok(path) = std::env::join_paths(path_entries) {
                command.env("PATH", path);
            }
            command
                .env("GST_PLUGIN_PATH", &plugins)
                .env("GST_PLUGIN_SYSTEM_PATH_1_0", &plugins)
                .env("GST_PLUGIN_SCANNER", scanner);
            return Some(runtime);
        }
    }

    let _ = (command, uxplay_path);
    None
}

/// 监控一个镜像子进程并只报告一次非用户主动停止的退出。
fn spawn_process_monitor(
    child: Arc<Mutex<Option<Child>>>,
    source: &'static str,
    tx: Sender<MirrorEvent>,
    running: Arc<AtomicBool>,
) -> std::thread::JoinHandle<()> {
    std::thread::spawn(move || {
        let mut status_error_reported = false;
        while running.load(Ordering::SeqCst) {
            let status = {
                let mut guard = child.lock().unwrap();
                match guard.as_mut() {
                    Some(process) => match process.try_wait() {
                        Ok(status) => status,
                        Err(error) => {
                            if !status_error_reported {
                                let _ = tx.send(MirrorEvent::LogLine(format!(
                                    "{source}: 读取进程状态失败：{error}"
                                )));
                                status_error_reported = true;
                            }
                            None
                        }
                    },
                    None => None,
                }
            };

            if let Some(status) = status {
                // `swap(false)` 同时充当生命周期闸门：UxPlay 和 ffmpeg
                // 即使几乎同时退出，也只有第一个观察到 running=true 的
                // 监控线程能够发出 Crashed，停止流程不会被误报为崩溃。
                if running.swap(false, Ordering::SeqCst) {
                    let _ = tx.send(MirrorEvent::Crashed {
                        source,
                        exit_code: status.code(),
                    });
                }
                break;
            }
            std::thread::sleep(Duration::from_millis(30));
        }
    })
}

#[cfg(not(target_os = "macos"))]
fn allocate_udp_port() -> Result<u16, AdapterError> {
    let socket = UdpSocket::bind(("127.0.0.1", 0))
        .map_err(|e| AdapterError::Failed(format!("创建 iOS RTP 端口失败：{e}")))?;
    socket
        .local_addr()
        .map(|address| address.port())
        .map_err(|e| AdapterError::Failed(format!("读取 iOS RTP 端口失败：{e}")))
}

#[cfg(target_os = "macos")]
const GSTREAMER_FRAME_FD: i32 = 3;

/// macOS：UxPlay 视频转换/缩放子管线（`-vc` 的值），供单元测试固定契约。
///
/// 顺序约束（发病原因见 `start_gstreamer_pipe` 注释）：`videoconvert` 不能
/// 缩放，宽高 caps 必须位于 `videoscale` 之后，否则 iPhone 首帧到达时 caps
/// 协商失败，UxPlay 关闭视频 renderer，连接立即断开且永远收不到画面。
#[cfg(target_os = "macos")]
fn macos_video_converter(width: u32, height: u32) -> String {
    format!(
        "videoconvert ! video/x-raw,format=RGBA ! videoscale ! \
         video/x-raw,width={width},height={height}"
    )
}

/// 为 macOS UxPlay 的 fdsink 创建原始 RGBA 帧管道。
#[cfg(target_os = "macos")]
fn create_gstreamer_frame_pipe() -> Result<(File, File), AdapterError> {
    let mut fds = [-1_i32; 2];
    // SAFETY: `fds` 指向两个有效的 i32 槽位，生命周期覆盖系统调用。
    let result = unsafe { libc::pipe(fds.as_mut_ptr()) };
    if result != 0 {
        return Err(AdapterError::Failed(format!(
            "创建 iOS GStreamer 帧管道失败：{}",
            std::io::Error::last_os_error()
        )));
    }

    // 读端不能被 UxPlay 子进程继承，否则某些 GStreamer 子进程仍存活时，
    // Rust 端可能迟迟收不到视频管道 EOF；write 端则必须保持可继承。
    // 子进程的 pre_exec 会把 write 端复制到固定 fd 3，GStreamer `fdsink`
    // 只向这个 fd 写像素，不污染日志。
    // SAFETY: pipe 成功后 fds[1] 是当前进程拥有的有效文件描述符。
    unsafe {
        libc::fcntl(fds[0], libc::F_SETFD, libc::FD_CLOEXEC);
        libc::fcntl(fds[1], libc::F_SETFD, 0);
        Ok((File::from_raw_fd(fds[0]), File::from_raw_fd(fds[1])))
    }
}

/// 终止并回收一个可选的解码进程。
///
/// 双编码探测中，任一路先输出完整 RGBA 帧就成为活动解码器；另一条分支
/// 必须马上停止，否则错误解码器会继续占用 CPU、产生大量无效日志并增加延迟。
fn terminate_child(child: &Arc<Mutex<Option<Child>>>) {
    if let Some(mut process) = child.lock().unwrap().take() {
        let _ = process.kill();
        let _ = process.wait();
    }
}

/// 双解码器探测：未选中的 H.264/H.265 分支退出时不能误报整个镜像失败。
#[cfg(not(target_os = "macos"))]
fn spawn_decoder_monitor(
    child: Arc<Mutex<Option<Child>>>,
    codec: &'static str,
    decoder_id: u8,
    tx: Sender<MirrorEvent>,
    running: Arc<AtomicBool>,
    winner: Arc<AtomicU8>,
    alive: Arc<AtomicU8>,
) -> std::thread::JoinHandle<()> {
    std::thread::spawn(move || {
        let mut status_error_reported = false;
        while running.load(Ordering::SeqCst) {
            let (status, process_present) = {
                let mut guard = child.lock().unwrap();
                match guard.as_mut() {
                    Some(process) => match process.try_wait() {
                        Ok(status) => (status, true),
                        Err(error) => {
                            if !status_error_reported {
                                let _ = tx.send(MirrorEvent::LogLine(format!(
                                    "iOS {codec} 解码器状态读取失败：{error}"
                                )));
                                status_error_reported = true;
                            }
                            (None, true)
                        }
                    },
                    // 首帧选出后，另一分支会被 terminate_child 取走；它的
                    // monitor 随即结束，不在会话期间空转。
                    None => (None, false),
                }
            };

            if !process_present {
                break;
            }

            if let Some(status) = status {
                alive.fetch_and(!decoder_id, Ordering::SeqCst);
                let selected = winner.load(Ordering::SeqCst);
                if selected == decoder_id || (selected == 0 && alive.load(Ordering::SeqCst) == 0) {
                    if running.swap(false, Ordering::SeqCst) {
                        let _ = tx.send(MirrorEvent::Crashed {
                            source: "ios-ffmpeg",
                            exit_code: status.code(),
                        });
                    }
                } else if selected == 0 {
                    let _ = tx.send(MirrorEvent::LogLine(format!(
                        "iOS {codec} 解码探测未匹配，继续等待另一种视频编码"
                    )));
                }
                break;
            }
            std::thread::sleep(Duration::from_millis(30));
        }
    })
}

#[cfg(not(target_os = "macos"))]
fn spawn_decoder_stderr_reader<R: Read + Send + 'static>(
    stderr: R,
    codec: &'static str,
    tx: Sender<MirrorEvent>,
) -> std::thread::JoinHandle<()> {
    std::thread::spawn(move || {
        let mut reader = BufReader::new(stderr);
        let mut emitted = 0;
        let mut suppression_reported = false;
        loop {
            let mut line = String::new();
            match reader.read_line(&mut line) {
                Ok(0) | Err(_) => break,
                Ok(_) => {
                    let line = sanitize_log_line(line.trim());
                    if !line.is_empty() {
                        if emitted < MAX_DECODER_LOG_LINES {
                            let _ = tx
                                .send(MirrorEvent::LogLine(format!("ios-ffmpeg[{codec}]: {line}")));
                            emitted += 1;
                        } else if !suppression_reported {
                            let _ = tx.send(MirrorEvent::LogLine(format!(
                                "ios-ffmpeg[{codec}]: 后续解码日志已省略"
                            )));
                            suppression_reported = true;
                        }
                    }
                }
            }
        }
    })
}

#[cfg(not(target_os = "macos"))]
fn spawn_decoder_frame_reader<R: Read + Send + 'static>(
    output: R,
    codec: &'static str,
    decoder_id: u8,
    running: Arc<AtomicBool>,
    winner: Arc<AtomicU8>,
    other_decoder: Arc<Mutex<Option<Child>>>,
    sink: Arc<Mutex<Option<Arc<dyn FrameSink>>>>,
    latest_frame: Arc<Mutex<Option<RgbaFrame>>>,
    tx: Sender<MirrorEvent>,
) -> std::thread::JoinHandle<()> {
    std::thread::spawn(move || {
        let frame_len = (IOS_OUTPUT_WIDTH * IOS_OUTPUT_HEIGHT * 4) as usize;
        let mut output = output;
        let mut frame = vec![0u8; frame_len];
        while running.load(Ordering::SeqCst) {
            match output.read_exact(&mut frame) {
                Ok(()) => {
                    let selected = match winner.compare_exchange(
                        0,
                        decoder_id,
                        Ordering::SeqCst,
                        Ordering::SeqCst,
                    ) {
                        Ok(_) => {
                            terminate_child(&other_decoder);
                            let _ = tx.send(MirrorEvent::FirstFrame);
                            decoder_id
                        }
                        Err(selected) => selected,
                    };
                    if selected != decoder_id {
                        continue;
                    }
                    // Transfer the read buffer into the frame instead of
                    // cloning the whole RGBA image. The next read gets a new
                    // buffer because the current frame may still be retained
                    // by the latest-frame replay or IPC backpressure queue.
                    let decoded = RgbaFrame {
                        width: IOS_OUTPUT_WIDTH,
                        height: IOS_OUTPUT_HEIGHT,
                        rgba: Arc::from(std::mem::take(&mut frame).into_boxed_slice()),
                    };
                    frame = vec![0u8; frame_len];
                    *latest_frame.lock().unwrap() = Some(decoded.clone());
                    if let Some(frame_sink) = sink.lock().unwrap().clone() {
                        frame_sink.push(&decoded);
                    }
                }
                Err(_) => {
                    if running.load(Ordering::SeqCst) && winner.load(Ordering::SeqCst) == decoder_id
                    {
                        let _ = tx.send(MirrorEvent::LogLine(format!(
                            "iOS {codec} ffmpeg 输出通道已结束"
                        )));
                    }
                    break;
                }
            }
        }
    })
}

/// macOS 内置 GStreamer 的 RGBA 帧读取器。
#[cfg(target_os = "macos")]
fn spawn_gstreamer_frame_reader<R: Read + Send + 'static>(
    output: R,
    running: Arc<AtomicBool>,
    sink: Arc<Mutex<Option<Arc<dyn FrameSink>>>>,
    latest_frame: Arc<Mutex<Option<RgbaFrame>>>,
    tx: Sender<MirrorEvent>,
) -> std::thread::JoinHandle<()> {
    std::thread::spawn(move || {
        let frame_len = (IOS_OUTPUT_WIDTH * IOS_OUTPUT_HEIGHT * 4) as usize;
        let mut output = output;
        let mut frame = vec![0u8; frame_len];
        let mut first_frame = false;
        while running.load(Ordering::SeqCst) {
            match output.read_exact(&mut frame) {
                Ok(()) => {
                    if !first_frame {
                        first_frame = true;
                        let _ = tx.send(MirrorEvent::FirstFrame);
                    }
                    // Transfer ownership of the complete read buffer to the
                    // frame; cloning roughly 2MB per frame can starve the
                    // AirPlay pipe while the WebView is catching up.
                    let decoded = RgbaFrame {
                        width: IOS_OUTPUT_WIDTH,
                        height: IOS_OUTPUT_HEIGHT,
                        rgba: Arc::from(std::mem::take(&mut frame).into_boxed_slice()),
                    };
                    frame = vec![0u8; frame_len];
                    *latest_frame.lock().unwrap() = Some(decoded.clone());
                    if let Some(frame_sink) = sink.lock().unwrap().clone() {
                        frame_sink.push(&decoded);
                    }
                }
                Err(_) => {
                    if running.load(Ordering::SeqCst) {
                        let _ = tx.send(MirrorEvent::LogLine(
                            "iOS GStreamer RGBA 输出通道已结束，尚未收到完整视频帧".into(),
                        ));
                    }
                    break;
                }
            }
        }
    })
}

struct UxPlayCleanup {
    sdp_paths: Vec<PathBuf>,
}

/// UxPlay 视频接收器（平台相关输出，但对上层统一提供 RGBA 帧）。
pub struct UxPlayMirrorAdapter {
    config: UxPlayConfig,
    child: Arc<Mutex<Option<Child>>>,
    decoder_h264: Arc<Mutex<Option<Child>>>,
    decoder_h265: Arc<Mutex<Option<Child>>>,
    workers: Mutex<Vec<std::thread::JoinHandle<()>>>,
    running: Arc<AtomicBool>,
    metadata: Arc<Mutex<Option<(u32, u32)>>>,
    binary_path: Mutex<Option<PathBuf>>,
    ffmpeg_path: Mutex<Option<PathBuf>>,
    sink: Arc<Mutex<Option<Arc<dyn FrameSink>>>>,
    /// 模拟器窗口可能在镜像接收器启动后才附加通道；保留最近一帧，避免
    /// 首帧恰好落在通道建立之前而导致窗口永久黑屏。
    latest_frame: Arc<Mutex<Option<RgbaFrame>>>,
    cleanup: Mutex<Option<UxPlayCleanup>>,
}

impl UxPlayMirrorAdapter {
    pub fn new(config: UxPlayConfig) -> Self {
        Self {
            config,
            child: Arc::new(Mutex::new(None)),
            decoder_h264: Arc::new(Mutex::new(None)),
            decoder_h265: Arc::new(Mutex::new(None)),
            workers: Mutex::new(Vec::new()),
            running: Arc::new(AtomicBool::new(false)),
            metadata: Arc::new(Mutex::new(None)),
            binary_path: Mutex::new(None),
            ffmpeg_path: Mutex::new(None),
            sink: Arc::new(Mutex::new(None)),
            latest_frame: Arc::new(Mutex::new(None)),
            cleanup: Mutex::new(None),
        }
    }

    /// 检查二进制是否已就位（打包前需完成审计与 SHA-256 校验，见 AGENTS.md）。
    pub fn check_binary(app_resources_dir: &Path) -> Result<PathBuf, String> {
        let p = resolve_uxplay_path(app_resources_dir)?;
        if p.exists() {
            Ok(p)
        } else {
            Err(format!(
                "未找到 UxPlay 可执行文件 {}。发布前需按 AGENTS.md 放置经过审计和校验的构建物并记录版本/SHA-256/许可证。",
                p.display()
            ))
        }
    }

    /// 注入二进制路径（由命令层在启动前调用）。
    pub fn set_binary_path(&mut self, path: PathBuf) {
        *self.binary_path.lock().unwrap() = Some(path);
    }

    /// 注入宿主 ffmpeg 路径；Windows RTP 只在本机回环地址上解码。
    pub fn set_ffmpeg_path(&mut self, path: PathBuf) {
        *self.ffmpeg_path.lock().unwrap() = Some(path);
    }

    fn cleanup(&self) {
        if let Some(cleanup) = self.cleanup.lock().unwrap().take() {
            for path in cleanup.sdp_paths {
                let _ = std::fs::remove_file(path);
            }
        }
    }
}

impl MirrorAdapter for UxPlayMirrorAdapter {
    fn name(&self) -> &'static str {
        "uxplay"
    }

    fn attach_sink(&mut self, sink: Arc<dyn FrameSink>) {
        *self.sink.lock().unwrap() = Some(sink.clone());
        sink.on_size(IOS_OUTPUT_WIDTH, IOS_OUTPUT_HEIGHT);
        if let Some(frame) = self.latest_frame.lock().unwrap().clone() {
            sink.push(&frame);
        }
    }

    fn start(&mut self, tx: Sender<MirrorEvent>) -> Result<(), AdapterError> {
        if self.child.lock().unwrap().is_some()
            || self.decoder_h264.lock().unwrap().is_some()
            || self.decoder_h265.lock().unwrap().is_some()
        {
            return Err(AdapterError::Busy("镜像子进程已在运行".into()));
        }
        let path = self
            .binary_path
            .lock()
            .unwrap()
            .clone()
            .ok_or_else(|| AdapterError::DependencyMissing("UxPlay 二进制路径未配置".into()))?;
        if !path.exists() {
            return Err(AdapterError::DependencyMissing(format!(
                "UxPlay 二进制缺失：{}",
                path.display()
            )));
        }

        #[cfg(target_os = "macos")]
        {
            self.start_gstreamer_pipe(path, tx)
        }
        #[cfg(not(target_os = "macos"))]
        {
            self.start_rtp_pipe(path, tx)
        }
    }

    fn stop(&mut self) -> Result<(), AdapterError> {
        self.running.store(false, Ordering::SeqCst);
        terminate_child(&self.decoder_h264);
        terminate_child(&self.decoder_h265);
        terminate_child(&self.child);
        // The frame reader uses a blocking read_exact() on the UxPlay pipe. A
        // killed child normally closes that pipe immediately, but a wrapper
        // process or an inherited descriptor can keep it open briefly. Never
        // block the Tauri command/window event path waiting for that reader;
        // finished workers are reaped here and unfinished handles are dropped
        // so they can finish asynchronously after the child has been killed.
        let workers: Vec<_> = self.workers.lock().unwrap().drain(..).collect();
        for worker in workers {
            if worker.is_finished() {
                let _ = worker.join();
            }
        }
        *self.metadata.lock().unwrap() = None;
        *self.latest_frame.lock().unwrap() = None;
        self.cleanup();
        log::info!("UxPlay adapter stopped");
        Ok(())
    }

    fn metadata(&self) -> Option<(u32, u32)> {
        *self.metadata.lock().unwrap()
    }
}

impl UxPlayMirrorAdapter {
    #[cfg(not(target_os = "macos"))]
    fn start_rtp_pipe(
        &mut self,
        path: PathBuf,
        tx: Sender<MirrorEvent>,
    ) -> Result<(), AdapterError> {
        // Windows 使用 UxPlay RTP → 随包 ffmpeg → RGBA 管线。ffmpeg 是
        // 随应用分发的独立 sidecar，不依赖宿主机安装的 GStreamer、Homebrew
        // 或其他 Unix 专用环境；Windows 需要随包携带 UxPlay 的 GStreamer
        // DLL、插件和 scanner，由运行时显式配置 PATH。
        let ffmpeg = self
            .ffmpeg_path
            .lock()
            .unwrap()
            .clone()
            .ok_or_else(|| AdapterError::DependencyMissing("iOS 画面解码需要 ffmpeg".into()))?;
        if !ffmpeg.exists() {
            return Err(AdapterError::DependencyMissing(format!(
                "ffmpeg 二进制缺失：{}",
                ffmpeg.display()
            )));
        }

        // UxPlay 的 -h265 会向 iPhone 宣布同时支持 H.264/H.265。它只会为
        // 当前协商出的编码器发送一路 RTP；这里把同一路复制到两个本机端口，
        // 由两个无界面 ffmpeg 解码器做协议探测，避免把 H.265 错当 H.264。
        let h264_port = allocate_udp_port()?;
        let mut h265_port = allocate_udp_port()?;
        while h265_port == h264_port {
            h265_port = allocate_udp_port()?;
        }
        let h264_sdp_path =
            std::env::temp_dir().join(format!("phonebridge-uxplay-h264-{h264_port}.sdp"));
        let h265_sdp_path =
            std::env::temp_dir().join(format!("phonebridge-uxplay-h265-{h265_port}.sdp"));
        let h264_sdp = format!(
            "v=0\r\n\
             o=- 0 0 IN IP4 127.0.0.1\r\n\
             s={app_name}\r\n\
             c=IN IP4 127.0.0.1\r\n\
             t=0 0\r\n\
             m=video {h264_port} RTP/AVP 96\r\n\
             a=rtpmap:96 H264/90000\r\n\
             a=fmtp:96 packetization-mode=1\r\n",
            app_name = crate::APP_NAME
        );
        let h265_sdp = format!(
            "v=0\r\n\
             o=- 0 0 IN IP4 127.0.0.1\r\n\
             s={app_name}\r\n\
             c=IN IP4 127.0.0.1\r\n\
             t=0 0\r\n\
             m=video {h265_port} RTP/AVP 96\r\n\
             a=rtpmap:96 H265/90000\r\n",
            app_name = crate::APP_NAME
        );
        if let Err(error) = std::fs::write(&h264_sdp_path, h264_sdp) {
            return Err(AdapterError::Failed(format!(
                "创建 iOS H.264 RTP SDP 失败：{error}"
            )));
        }
        if let Err(error) = std::fs::write(&h265_sdp_path, h265_sdp) {
            let _ = std::fs::remove_file(&h264_sdp_path);
            return Err(AdapterError::Failed(format!(
                "创建 iOS H.265 RTP SDP 失败：{error}"
            )));
        }

        let filter = format!(
            "scale={IOS_OUTPUT_WIDTH}:{IOS_OUTPUT_HEIGHT}:force_original_aspect_ratio=decrease,\
             pad={IOS_OUTPUT_WIDTH}:{IOS_OUTPUT_HEIGHT}:(ow-iw)/2:(oh-ih)/2:black"
        );
        let spawn_decoder = |sdp_path: &Path| {
            Command::new(&ffmpeg)
                .args([
                    "-hide_banner",
                    "-loglevel",
                    "error",
                    "-protocol_whitelist",
                    "file,udp,rtp",
                    // 不使用 `-fflags nobuffer`：FFmpeg 的 RTP/SDP 解包器需要
                    // 极少量缓冲来组装完整访问单元；强行禁用后会收到 UDP 数据，
                    // 但不向 rawvideo 输出首帧。`low_delay` 已负责降低解码延迟。
                    "-flags",
                    "low_delay",
                    "-max_delay",
                    "0",
                    "-f",
                    "sdp",
                    // SDP demuxer 默认只等待 10 秒；AirPlay 连接由用户在
                    // iPhone 上发起，不能把“尚未投送首帧”误判为解码器崩溃。
                    "-listen_timeout",
                    "-1",
                    "-i",
                    sdp_path.to_str().unwrap_or_default(),
                    "-an",
                    "-sn",
                    "-dn",
                    "-f",
                    "rawvideo",
                    "-pix_fmt",
                    "rgba",
                    "-vf",
                    &filter,
                    "pipe:1",
                ])
                .stdin(Stdio::null())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .spawn()
        };
        let mut decoder_h264 = match spawn_decoder(&h264_sdp_path) {
            Ok(child) => child,
            Err(error) => {
                let _ = std::fs::remove_file(&h264_sdp_path);
                let _ = std::fs::remove_file(&h265_sdp_path);
                return Err(AdapterError::Failed(format!(
                    "启动 iOS H.264 ffmpeg 解码器失败：{error}"
                )));
            }
        };
        let mut decoder_h265 = match spawn_decoder(&h265_sdp_path) {
            Ok(child) => child,
            Err(error) => {
                let _ = decoder_h264.kill();
                let _ = decoder_h264.wait();
                let _ = std::fs::remove_file(&h264_sdp_path);
                let _ = std::fs::remove_file(&h265_sdp_path);
                return Err(AdapterError::Failed(format!(
                    "启动 iOS H.265 ffmpeg 解码器失败：{error}"
                )));
            }
        };
        let decoder_h264_stdout = decoder_h264
            .stdout
            .take()
            .expect("ffmpeg H.264 stdout 已 piped");
        let decoder_h264_stderr = decoder_h264
            .stderr
            .take()
            .expect("ffmpeg H.264 stderr 已 piped");
        let decoder_h265_stdout = decoder_h265
            .stdout
            .take()
            .expect("ffmpeg H.265 stdout 已 piped");
        let decoder_h265_stderr = decoder_h265
            .stderr
            .take()
            .expect("ffmpeg H.265 stderr 已 piped");

        // `-vs 0` 会让 UxPlay 关闭整个视频管线；fakesink 才是“无原生窗口但
        // 仍处理视频”的模式。`-vrtp` 将当前协商出的 H.264/H.265 RTP
        // 复制到两个本机回环端口；两个 ffmpeg 解码器中只有匹配编码的一路
        // 会产出有效首帧。官方 -vrtp 模式本身不创建第三方窗口。
        let rtp_pipeline = format!(
            "config-interval=1 ! tee name=phonebridge_rtp \
             phonebridge_rtp. ! queue ! udpsink host=127.0.0.1 port={h264_port} \
             phonebridge_rtp. ! queue ! udpsink host=127.0.0.1 port={h265_port}"
        );
        let mut uxplay_command = Command::new(&path);
        let gstreamer_runtime = configure_uxplay_runtime(&mut uxplay_command, &path);
        if cfg!(any(target_os = "macos", target_os = "windows")) && gstreamer_runtime.is_none() {
            return Err(AdapterError::DependencyMissing(
                "未找到随应用分发的 UxPlay GStreamer 运行时；请运行 pnpm sidecars:uxplay，不能依赖宿主机安装".into(),
            ));
        }
        let mut child = match uxplay_command
            .arg("-n")
            .arg(&self.config.service_name)
            // 不让 UxPlay 再追加主机名；面板提示和 iPhone 的“屏幕镜像”
            // 列表必须使用同一个稳定服务名。
            .arg("-nh")
            .arg("-p")
            .arg(self.config.port.to_string())
            // 新款 iPhone 可能协商 H.265；UxPlay 会根据实际编码自动选择
            // 对应的 renderer，RTP 两路由上面的双解码器做最终确认。
            .arg("-h265")
            // UxPlay 开启 H.265 后默认请求 4K；快投屏画布最终输出 480x1040，
            // 请求 1080p/30 足以保持清晰度，同时避免 4K 和过高帧率带来的
            // 编码、传输与双 ffmpeg 解码压力。
            .arg("-s")
            .arg("1920x1080@30")
            .arg("-fps")
            .arg("30")
            .arg("-vs")
            .arg("fakesink")
            .arg("-as")
            .arg("0")
            // UxPlay 官方建议纯镜像场景关闭时间戳同步，避免把宿主画布拖入音视频延迟。
            .arg("-vsync")
            .arg("no")
            // 新连接到来时替换可能已经失效但尚未超时的旧 AirPlay 会话。
            .arg("-nohold")
            .arg("-vrtp")
            .arg(rtp_pipeline)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .stdin(Stdio::null())
            .spawn()
        {
            Ok(child) => child,
            Err(error) => {
                let _ = decoder_h264.kill();
                let _ = decoder_h264.wait();
                let _ = decoder_h265.kill();
                let _ = decoder_h265.wait();
                let _ = std::fs::remove_file(&h264_sdp_path);
                let _ = std::fs::remove_file(&h265_sdp_path);
                return Err(AdapterError::Failed(format!("启动 UxPlay 失败：{error}")));
            }
        };
        let stdout = child.stdout.take().expect("UxPlay stdout 已 piped");
        let stderr = child.stderr.take().expect("UxPlay stderr 已 piped");

        self.running.store(true, Ordering::SeqCst);
        *self.cleanup.lock().unwrap() = Some(UxPlayCleanup {
            sdp_paths: vec![h264_sdp_path, h265_sdp_path],
        });
        *self.child.lock().unwrap() = Some(child);
        *self.decoder_h264.lock().unwrap() = Some(decoder_h264);
        *self.decoder_h265.lock().unwrap() = Some(decoder_h265);
        *self.metadata.lock().unwrap() = Some((IOS_OUTPUT_WIDTH, IOS_OUTPUT_HEIGHT));

        let mut workers = self.workers.lock().unwrap();
        let tx_stdout = tx.clone();
        let metadata = self.metadata.clone();
        workers.push(std::thread::spawn(move || {
            let reader = BufReader::new(stdout);
            for line in reader.lines().map_while(Result::ok) {
                let line = sanitize_log_line(&line);
                if is_uxplay_connected_log(&line) {
                    let _ = tx_stdout.send(MirrorEvent::MirrorConnected);
                }
                if let Some((width, height)) = parse_resolution(&line) {
                    *metadata.lock().unwrap() = Some((width, height));
                    let _ = tx_stdout.send(MirrorEvent::Metadata { width, height });
                } else {
                    let _ = tx_stdout.send(MirrorEvent::LogLine(line));
                }
            }
        }));
        let tx_stderr = tx.clone();
        workers.push(std::thread::spawn(move || {
            let mut reader = BufReader::new(stderr);
            loop {
                let mut line = String::new();
                match reader.read_line(&mut line) {
                    Ok(0) | Err(_) => break,
                    Ok(_) => {
                        let line = sanitize_log_line(line.trim());
                        if !line.is_empty() {
                            if is_uxplay_connected_log(&line) {
                                let _ = tx_stderr.send(MirrorEvent::MirrorConnected);
                            }
                            let _ = tx_stderr.send(MirrorEvent::LogLine(format!("uxplay: {line}")));
                        }
                    }
                }
            }
        }));
        workers.push(spawn_decoder_stderr_reader(
            decoder_h264_stderr,
            "h264",
            tx.clone(),
        ));
        workers.push(spawn_decoder_stderr_reader(
            decoder_h265_stderr,
            "h265",
            tx.clone(),
        ));
        let winner = Arc::new(AtomicU8::new(0));
        workers.push(spawn_decoder_frame_reader(
            decoder_h264_stdout,
            "h264",
            1,
            self.running.clone(),
            winner.clone(),
            self.decoder_h265.clone(),
            self.sink.clone(),
            self.latest_frame.clone(),
            tx.clone(),
        ));
        workers.push(spawn_decoder_frame_reader(
            decoder_h265_stdout,
            "h265",
            2,
            self.running.clone(),
            winner.clone(),
            self.decoder_h264.clone(),
            self.sink.clone(),
            self.latest_frame.clone(),
            tx.clone(),
        ));
        let decoder_alive = Arc::new(AtomicU8::from(0b11));
        workers.push(spawn_process_monitor(
            self.child.clone(),
            "uxplay",
            tx.clone(),
            self.running.clone(),
        ));
        workers.push(spawn_decoder_monitor(
            self.decoder_h264.clone(),
            "h264",
            1,
            tx.clone(),
            self.running.clone(),
            winner.clone(),
            decoder_alive.clone(),
        ));
        workers.push(spawn_decoder_monitor(
            self.decoder_h265.clone(),
            "h265",
            2,
            tx.clone(),
            self.running.clone(),
            winner,
            decoder_alive,
        ));
        drop(workers);

        let _ = tx.send(MirrorEvent::Metadata {
            width: IOS_OUTPUT_WIDTH,
            height: IOS_OUTPUT_HEIGHT,
        });
        let runtime_detail = gstreamer_runtime
            .as_deref()
            .map(|path| format!("，UxPlay 运行时 {}", path.display()))
            .unwrap_or_default();
        let _ = tx.send(MirrorEvent::LogLine(format!(
            "UxPlay 已启动为无界面接收器（H.264/H.265 RTP 127.0.0.1:{h264_port}/{h265_port} → 内置 ffmpeg RGBA {}x{}，AirPlay 端口 {}{}）",
            IOS_OUTPUT_WIDTH,
            IOS_OUTPUT_HEIGHT,
            self.config.port,
            runtime_detail
        )));
        let _ = tx.send(MirrorEvent::LogLine(format!(
            "iOS 正在等待 AirPlay 连接：请在 iPhone 控制中心打开“屏幕镜像”，选择 {}；iPhone 与此电脑必须在同一局域网（USB 仅用于识别设备）",
            crate::APP_NAME
        )));
        log::info!("UxPlay headless RTP/ffmpeg adapter started");
        Ok(())
    }

    /// macOS 使用 UxPlay 自己的 GStreamer 管线，直接把 RGBA 帧写入匿名
    /// pipe。该路径不经过二次 H.264/H.265 编码；解码器固定为随应用分发的
    /// libav 软件解码，避免 macOS 上 decodebin 选择不可用的硬件 decoder，
    /// 同时不创建 UxPlay 原生窗口。
    #[cfg(target_os = "macos")]
    fn start_gstreamer_pipe(
        &mut self,
        path: PathBuf,
        tx: Sender<MirrorEvent>,
    ) -> Result<(), AdapterError> {
        let (frame_read, frame_write) = create_gstreamer_frame_pipe()?;
        let frame_fd = frame_write.as_raw_fd();
        let mut uxplay_command = Command::new(&path);
        let gstreamer_runtime = configure_uxplay_runtime(&mut uxplay_command, &path);
        if gstreamer_runtime.is_none() {
            return Err(AdapterError::DependencyMissing(
                "未找到随应用分发的 UxPlay GStreamer 运行时；请运行 pnpm sidecars:uxplay，不能依赖宿主机安装".into(),
            ));
        }

        // UxPlay 的 videoscale 位于 `-vc` 之后。GStreamer 的 videoconvert
        // 只做格式/色彩空间转换，**不能缩放**：把宽高 caps 直接放在它的输出侧
        // 会让首帧到来时的 caps 协商失败（"could not link videoconvert to
        // videoscale … videoconvert can't handle caps 480x1040"），UxPlay
        // 视频总线随即报错并关闭 renderer，手机端表现为「镜像刚连接上就断开」，
        // 模拟器窗口始终黑屏、主窗口停留在“等待手机连接”。正确顺序是
        // videoconvert → format caps → videoscale → 尺寸 caps：缩放后再固定
        // 输出为完整帧（RGBA WxH），Rust 端无需猜测 stride 即可按整帧读取；
        // fd 3 专用于像素，stdout/stderr 仍保留给诊断日志。
        let video_converter = macos_video_converter(IOS_OUTPUT_WIDTH, IOS_OUTPUT_HEIGHT);
        // SAFETY: `frame_fd` 是当前进程拥有的 pipe write 端。pre_exec 只在
        // 子进程中执行，把它复制到固定 fd 3 供 GStreamer fdsink 使用；父进程
        // 在 spawn 返回后立即 drop 原始 write 端。
        unsafe {
            uxplay_command.pre_exec(move || {
                if libc::dup2(frame_fd, GSTREAMER_FRAME_FD) == -1 {
                    return Err(std::io::Error::last_os_error());
                }
                if frame_fd != GSTREAMER_FRAME_FD {
                    libc::close(frame_fd);
                }
                Ok(())
            });
        }
        let mut child = match uxplay_command
            .arg("-n")
            .arg(&self.config.service_name)
            .arg("-nh")
            .arg("-p")
            .arg(self.config.port.to_string())
            .arg("-h265")
            .arg("-s")
            .arg("1920x1080@30")
            // The macOS path uses UxPlay's software decoder and forwards
            // RGBA frames through a bounded WebView channel. 30 fps keeps
            // the fdsink pipe drained and avoids AirPlay feedback timeouts.
            .arg("-fps")
            .arg("30")
            // UxPlay 的默认 decodebin 可能在 macOS 上选择不可用的硬件
            // decoder；连接建立后会表现为 `video_source Internal data
            // stream error`，随后 renderer bus 被清理，手机端的 AirPlay
            // 服务也会因为 UxPlay 崩溃而消失。-avdec 使用应用内置的
            // libav，并且同时为 H.264/H.265 renderer 选择软件解码器。
            .arg("-avdec")
            .arg("-vc")
            .arg(video_converter)
            .arg("-vs")
            // 不按 AirPlay 时间戳等待，收到完整帧后立即交给 Tauri canvas。
            .arg(format!("fdsink fd={GSTREAMER_FRAME_FD} sync=false"))
            .arg("-as")
            .arg("0")
            .arg("-vsync")
            .arg("no")
            // UxPlay 在 macOS 默认启用 -nc 以保留原生视频窗口。这里输出
            // 到 fd，不存在需要保留的窗口；恢复正常的 renderer 清理路径，
            // 防止一次失败连接留下无 bus 的 renderer。
            .arg("-nc")
            .arg("no")
            // Replace a stale AirPlay client during reconnect instead of
            // waiting for its old session to time out.
            .arg("-nohold")
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .stdin(Stdio::null())
            .spawn()
        {
            Ok(child) => child,
            Err(error) => {
                return Err(AdapterError::Failed(format!("启动 UxPlay 失败：{error}")));
            }
        };
        // The parent must not keep the write end open, otherwise the frame reader
        // cannot observe EOF after UxPlay exits.
        drop(frame_write);
        let stdout = child.stdout.take().expect("UxPlay stdout 已 piped");
        let stderr = child.stderr.take().expect("UxPlay stderr 已 piped");

        self.running.store(true, Ordering::SeqCst);
        *self.cleanup.lock().unwrap() = Some(UxPlayCleanup {
            sdp_paths: Vec::new(),
        });
        *self.child.lock().unwrap() = Some(child);
        *self.metadata.lock().unwrap() = Some((IOS_OUTPUT_WIDTH, IOS_OUTPUT_HEIGHT));

        let mut workers = self.workers.lock().unwrap();
        let tx_stdout = tx.clone();
        let metadata = self.metadata.clone();
        workers.push(std::thread::spawn(move || {
            let reader = BufReader::new(stdout);
            for line in reader.lines().map_while(Result::ok) {
                let line = sanitize_log_line(&line);
                if is_uxplay_connected_log(&line) {
                    let _ = tx_stdout.send(MirrorEvent::MirrorConnected);
                }
                if let Some((width, height)) = parse_resolution(&line) {
                    *metadata.lock().unwrap() = Some((width, height));
                    let _ = tx_stdout.send(MirrorEvent::Metadata { width, height });
                } else if !line.is_empty() {
                    let _ = tx_stdout.send(MirrorEvent::LogLine(line));
                }
            }
        }));
        let tx_stderr = tx.clone();
        workers.push(std::thread::spawn(move || {
            let mut reader = BufReader::new(stderr);
            loop {
                let mut line = String::new();
                match reader.read_line(&mut line) {
                    Ok(0) | Err(_) => break,
                    Ok(_) => {
                        let line = sanitize_log_line(line.trim());
                        if !line.is_empty() {
                            if is_uxplay_connected_log(&line) {
                                let _ = tx_stderr.send(MirrorEvent::MirrorConnected);
                            }
                            let _ = tx_stderr.send(MirrorEvent::LogLine(format!("uxplay: {line}")));
                        }
                    }
                }
            }
        }));
        workers.push(spawn_gstreamer_frame_reader(
            frame_read,
            self.running.clone(),
            self.sink.clone(),
            self.latest_frame.clone(),
            tx.clone(),
        ));
        workers.push(spawn_process_monitor(
            self.child.clone(),
            "uxplay",
            tx.clone(),
            self.running.clone(),
        ));
        drop(workers);

        let _ = tx.send(MirrorEvent::Metadata {
            width: IOS_OUTPUT_WIDTH,
            height: IOS_OUTPUT_HEIGHT,
        });
        let runtime_detail = gstreamer_runtime
            .as_deref()
            .map(|runtime| format!("，UxPlay 运行时 {}", runtime.display()))
            .unwrap_or_default();
        let _ = tx.send(MirrorEvent::LogLine(format!(
            "UxPlay 已启动为无界面接收器（内置 GStreamer RGBA {}x{}，AirPlay 端口 {}{}）",
            IOS_OUTPUT_WIDTH, IOS_OUTPUT_HEIGHT, self.config.port, runtime_detail
        )));
        let _ = tx.send(MirrorEvent::LogLine(format!(
            "iOS 正在等待 AirPlay 连接：请在 iPhone 控制中心打开“屏幕镜像”，选择 {}；iPhone 与此电脑必须在同一局域网（USB 仅用于识别设备）",
            crate::APP_NAME
        )));
        log::info!("UxPlay headless GStreamer pipe adapter started");
        Ok(())
    }
}

impl Drop for UxPlayMirrorAdapter {
    fn drop(&mut self) {
        let _ = self.stop();
    }
}

/// 从 UxPlay 日志行解析分辨率（形如 1920x1080、800 x 600）。
fn parse_resolution(line: &str) -> Option<(u32, u32)> {
    let digits: Vec<&str> = line
        .split(|c: char| !c.is_ascii_digit())
        .filter(|s| !s.is_empty())
        .collect();
    if digits.len() < 2 {
        return None;
    }
    let w = digits[0].parse::<u32>().ok()?;
    let h = digits[1].parse::<u32>().ok()?;
    if w >= 320 && h >= 320 && w <= 8000 && h <= 8000 {
        Some((w, h))
    } else {
        None
    }
}

/// 判断 UxPlay 输出中是否已经出现 AirPlay/RAOP 会话活动。
///
/// UxPlay 没有专门的结构化“connected”输出，且不同版本把连接信息写到
/// stdout 或 stderr。只使用能证明客户端已经进入 RAOP/视频会话的标记；
/// “server started”“waiting”之类的启动日志不能触发该事件。
fn is_uxplay_connected_log(line: &str) -> bool {
    let lower = line.to_ascii_lowercase();
    lower.contains("raop active connection")
        || (lower.contains("new request") && lower.contains("type raop"))
        || lower.contains("raop_handler_feedback")
        || lower.contains("post /feedback")
        || lower.contains("received video streaming performance info packet")
        || lower.contains("begin streaming to gstreamer video pipeline")
}

/// 日志行脱敏：隐藏 MAC/IP 等网络身份并限制长度。
fn sanitize_log_line(line: &str) -> String {
    let mut s = line
        .split_whitespace()
        .map(redact_log_token)
        .collect::<Vec<_>>()
        .join(" ");
    if s.len() > 512 {
        let mut end = 512;
        while end > 0 && !s.is_char_boundary(end) {
            end -= 1;
        }
        s.truncate(end);
        s.push_str("…(截断)");
    }
    s
}

fn redact_log_token(token: &str) -> String {
    let leading = token
        .char_indices()
        .find(|(_, c)| !c.is_ascii_punctuation())
        .map(|(index, _)| index)
        .unwrap_or(token.len());
    let trailing = token
        .char_indices()
        .rev()
        .find(|(_, c)| !c.is_ascii_punctuation())
        .map(|(index, c)| index + c.len_utf8())
        .unwrap_or(leading);
    let core = &token[leading..trailing];
    if looks_like_mac(core) || looks_like_ipv4(core) {
        format!("{}[地址已脱敏]{}", &token[..leading], &token[trailing..])
    } else {
        token.to_string()
    }
}

fn looks_like_mac(value: &str) -> bool {
    let parts: Vec<&str> = value.split(':').collect();
    parts.len() == 6
        && parts
            .iter()
            .all(|part| part.len() == 2 && part.bytes().all(|byte| byte.is_ascii_hexdigit()))
}

fn looks_like_ipv4(value: &str) -> bool {
    let parts: Vec<&str> = value.split('.').collect();
    parts.len() == 4
        && parts.iter().all(|part| {
            !part.is_empty()
                && part.len() <= 3
                && part.parse::<u8>().is_ok()
                && part.bytes().all(|byte| byte.is_ascii_digit())
        })
}

/// 供命令层记录的依赖状态：返回 (present, version, detail)。
pub fn uxplay_package_state(app_resources_dir: &Path) -> (bool, Option<String>, String) {
    let version_path = app_resources_dir.join("binaries").join("VERSION");
    let version = std::fs::read_to_string(&version_path)
        .ok()
        .map(|s| s.trim().to_string());
    match resolve_uxplay_path(app_resources_dir) {
        Ok(path) if path.exists() => {
            let version = version.unwrap_or_else(|| "未登记（需审计+记录版本）".into());
            (true, Some(version), path.display().to_string())
        }
        Ok(path) => (false, None, format!("缺失：{}", path.display())),
        Err(error) => (false, None, error),
    }
}

/// 清理上一次异常退出遗留的 UxPlay 孤儿实例（仅 macOS）。
///
/// 应用被强杀（SIGKILL、崩溃、`tauri dev` 重启）时不会走到 lib.rs 的
/// `ExitRequested` 清理路径，UxPlay 子进程会变成孤儿：继续占用 AirPlay
/// 端口段（TCP/UDP n..n+2）、继续以同名广播「快投屏」。后果是——新会话的
/// UxPlay 无法绑定端口而退出（“一直连接不上”），而手机一旦连上，被接走的是
/// 孤儿接收器（其帧管道已随旧应用关闭），连接立刻断开。
///
/// 只匹配「本应用分发的 uxplay 可执行文件 + 快投屏服务名」的进程，不误杀
/// 第三方 UxPlay。并发运行两个应用实例时后者会清掉前者（两者本就不能共存
/// 于同一端口段），属可接受的最后写入者胜出语义。
#[cfg(target_os = "macos")]
pub fn reap_stale_uxplay(app_resources_dir: &Path) {
    let paths = uxplay_path_candidates(app_resources_dir);
    if paths.is_empty() {
        return;
    }
    let Ok(output) = std::process::Command::new("ps")
        .args(["-axo", "pid=,command="])
        .output()
    else {
        return;
    };
    let Ok(text) = String::from_utf8(output.stdout) else {
        return;
    };
    let current = std::process::id();
    let victims: Vec<i32> = text
        .lines()
        .filter_map(|line| {
            paths
                .iter()
                .find_map(|path| parse_orphan_line(line, path, current))
        })
        .collect();
    if victims.is_empty() {
        return;
    }
    log::info!("reaping {} stale uxplay instance(s)", victims.len());
    for &pid in &victims {
        // SAFETY: pid 来自本进程扫描出的、匹配本应用 uxplay 的存活进程。
        unsafe {
            libc::kill(pid, libc::SIGTERM);
        }
    }
    // 给 SIGTERM 一次优雅退出机会（UxPlay 会自行停止 RAOP server），
    // 超时仍未退出才强杀。
    std::thread::sleep(Duration::from_millis(400));
    for &pid in &victims {
        // kill(pid, 0) 仅检查进程是否存在（不发送信号）。
        unsafe {
            if libc::kill(pid, 0) == 0 {
                libc::kill(pid, libc::SIGKILL);
            }
        }
    }
}

/// 解析 `ps -axo pid=,command=` 的一行：行首为 pid，命令匹配本应用分发的
/// uxplay 路径并且带快投屏服务名，且不是当前进程时返回该 pid。
#[cfg(target_os = "macos")]
fn parse_orphan_line(line: &str, uxplay_path: &Path, ignore_pid: u32) -> Option<i32> {
    let trimmed = line.trim();
    let pid_end = trimmed.find(char::is_whitespace)?;
    let pid: u32 = trimmed[..pid_end].parse().ok()?;
    if pid == ignore_pid {
        return None;
    }
    let command = trimmed[pid_end..].trim_start();
    if !command.starts_with(uxplay_path.to_str().unwrap_or_default()) {
        return None;
    }
    if !command.contains(&format!(" -n {}", crate::APP_NAME)) {
        return None;
    }
    Some(pid as i32)
}

/// 非 macOS：暂不实现孤儿清理。Windows 上按镜像名 `taskkill /IM uxplay.exe`
/// 可能误杀用户自行安装的 UxPlay，故仅依赖 `ExitRequested` 清理路径；
/// 发布前按需用 tasklist + 可执行文件路径校验后再 kill。
#[cfg(not(target_os = "macos"))]
pub fn reap_stale_uxplay(_app_resources_dir: &Path) {}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Default)]
    struct TestSink {
        frames: std::sync::Mutex<Vec<RgbaFrame>>,
        sizes: std::sync::Mutex<Vec<(u32, u32)>>,
    }

    impl FrameSink for TestSink {
        fn push(&self, frame: &RgbaFrame) {
            self.frames.lock().unwrap().push(frame.clone());
        }

        fn on_size(&self, width: u32, height: u32) {
            self.sizes.lock().unwrap().push((width, height));
        }
    }

    #[test]
    fn parses_resolution() {
        assert_eq!(parse_resolution("video  1920x1080 @60"), Some((1920, 1080)));
        assert_eq!(parse_resolution("800 x 600"), Some((800, 600)));
        assert_eq!(parse_resolution("no resolution here"), None);
        assert_eq!(parse_resolution("120x80"), None);
    }

    #[test]
    fn recognizes_airplay_connection_logs_without_matching_waiting_logs() {
        assert!(is_uxplay_connected_log(
            "new request, connection 0, socket 19 type RAOP"
        ));
        assert!(is_uxplay_connected_log("POST /feedback RTSP/1.0"));
        assert!(is_uxplay_connected_log(
            "Received video streaming performance info packet"
        ));
        assert!(!is_uxplay_connected_log("iOS 正在等待 AirPlay 连接"));
        assert!(!is_uxplay_connected_log(
            "register_dnssd: advertised AirPlay service"
        ));
    }

    #[test]
    fn sanitize_truncates_long_lines() {
        let long = "x".repeat(2000);
        let out = sanitize_log_line(&long);
        assert!(out.len() <= 512 + 32);
        assert!(out.contains("截断"));
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn orphan_line_parsing_matches_only_our_uxplay() {
        let path = std::path::Path::new("/app/Resources/binaries/uxplay");
        let ours = format!(
            "12345 /app/Resources/binaries/uxplay -n {} -nh -p 6000 -h265 -s 1920x1080@30",
            crate::APP_NAME
        );
        // 匹配本应用分发的 uxplay + 快投屏服务名
        assert_eq!(parse_orphan_line(&ours, path, 999), Some(12345));
        // 当前进程忽略
        assert_eq!(parse_orphan_line(&ours, path, 12345), None);
        // 第三方 uxplay（不同路径）
        assert_eq!(
            parse_orphan_line("23456 /usr/local/bin/uxplay -n OtherSvc -nh", path, 999),
            None
        );
        // 本路径但无快投屏服务名（可能是其它用途/早期版本参数）
        assert_eq!(
            parse_orphan_line(
                "34567 /app/Resources/binaries/uxplay -nh -p 6000",
                path,
                999
            ),
            None
        );
        // 垃圾行 / 空行
        assert_eq!(parse_orphan_line("not a ps line", path, 999), None);
        assert_eq!(parse_orphan_line("", path, 999), None);
    }

    #[test]
    fn sanitize_redacts_network_identity() {
        assert_eq!(
            sanitize_log_line("client 01:23:45:67:89:ab 192.168.1.10"),
            "client [地址已脱敏] [地址已脱敏]"
        );
        assert_eq!(sanitize_log_line("time 06:08:37.827"), "time 06:08:37.827");
    }

    #[test]
    fn config_defaults() {
        let config = UxPlayConfig::default();
        assert_eq!(config.service_name, crate::APP_NAME);
        assert_eq!(config.port, 6000);
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn macos_converter_scales_before_fixing_size() {
        let s = macos_video_converter(IOS_OUTPUT_WIDTH, IOS_OUTPUT_HEIGHT);

        // videoconvert 只转格式/色彩空间，不能缩放；宽高 caps 必须出现在
        // videoscale 之后，否则首帧 caps 协商失败、连接立即断开。
        let scale_at = s.find("videoscale").expect("缺少 videoscale");
        let size_at = s.find("width=").expect("缺少输出尺寸 caps");
        assert!(
            scale_at < size_at,
            "尺寸 caps 不得位于 videoscale 之前：{s}"
        );

        // 格式与输出尺寸必须与 Rust 端整帧读取的契约一致。
        assert!(s.contains("format=RGBA"));
        assert!(s.contains(&format!(
            "width={IOS_OUTPUT_WIDTH},height={IOS_OUTPUT_HEIGHT}"
        )));

        // 回归保护：旧写法在 videoconvert 输出侧直接强制 480x1040，
        // videoconvert 无法产生产出该尺寸，属于历史发病写法。
        assert!(s.starts_with("videoconvert ! video/x-raw,format=RGBA ! videoscale"));
        assert!(!s.contains("format=RGBA,width="));
    }

    #[test]
    fn attach_sink_replays_latest_frame() {
        let mut adapter = UxPlayMirrorAdapter::new(UxPlayConfig::default());
        let frame = RgbaFrame {
            width: IOS_OUTPUT_WIDTH,
            height: IOS_OUTPUT_HEIGHT,
            rgba: std::sync::Arc::from(vec![255, 0, 0, 255]),
        };
        *adapter.latest_frame.lock().unwrap() = Some(frame.clone());

        let sink = std::sync::Arc::new(TestSink::default());
        adapter.attach_sink(sink.clone());

        assert_eq!(
            sink.sizes.lock().unwrap().as_slice(),
            &[(IOS_OUTPUT_WIDTH, IOS_OUTPUT_HEIGHT)]
        );
        let frames = sink.frames.lock().unwrap();
        assert_eq!(frames.len(), 1);
        assert_eq!(frames[0].width, frame.width);
        assert_eq!(frames[0].height, frame.height);
        assert_eq!(frames[0].rgba.as_ref(), frame.rgba.as_ref());
    }
}
