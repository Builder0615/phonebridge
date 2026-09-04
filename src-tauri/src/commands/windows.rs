//! 模拟器窗口管理（Spec v1.2 §4.2、§5.1）。
//!
//! - 每台已连接设备一个模拟器窗口（label `simulator-<hex(id)>`），加载独立的
//!   `simulator.html` 入口，只渲染对应设备画布，不加载主窗口面板；
//! - 多台设备窗口可以同时显示：activate/open 时只 show/focus 目标，不隐藏其它窗口；
//! - 模拟器使用系统原生标题栏以支持拖动和关闭，但禁用最大化；关闭动作回到
//!   与主窗口相同的会话清理路径。

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use tauri::{
    AppHandle, Manager, WebviewUrl, WebviewWindow, WebviewWindowBuilder, Window, WindowEvent,
};

use crate::session::manager::SessionRegistry;

/// 窗口 label 前缀。
pub const SIMULATOR_PREFIX: &str = "simulator-";

/// 在主线程事件循环上执行窗口操作并等待结果。
///
/// Tauri 的 `WebviewWindowBuilder.build()` / `WebviewWindow::destroy()` 必须在
/// 主线程执行；而命令可能运行在异步线程池（async 命令）上。直接在主线程同步
/// 命令里 destroy 会占住事件循环并等待自身完成——macOS 上表现为「关闭模拟器
/// 窗口后 App 卡死」。
fn run_on_main_thread<T: Send + 'static>(
    app: &AppHandle,
    f: impl FnOnce() -> T + Send + 'static,
) -> Result<T, String> {
    let (tx, rx) = std::sync::mpsc::sync_channel::<Result<T, String>>(1);
    app.run_on_main_thread(move || {
        let _ = tx.send(Ok(f()));
    })
    .map_err(|error| format!("投递到主线程失败：{error}"))?;
    rx.recv()
        .map_err(|error| format!("主线程结果接收失败：{error}"))?
}

/// 非阻塞地把窗口销毁投递到主线程。调用方（async 命令）会先返回，
/// IPC 响应因此可以先于销毁发出，避免「在响应还没发出前就销毁承载响应的
/// WebView」造成的挂起。
fn destroy_on_main_thread(app: &AppHandle, window: WebviewWindow) {
    let app = app.clone();
    std::thread::spawn(move || {
        // 短暂延迟：让本命令的 IPC 响应先进入发送队列，再拆掉 WebView。
        std::thread::sleep(std::time::Duration::from_millis(120));
        let _ = app.run_on_main_thread(move || {
            let _ = window.destroy();
        });
    });
}

/// 把 sessionId 的 UTF-8 字节编码成十六进制窗口 label，确保 label 安全且可逆。
pub fn simulator_label(session_id: &str) -> String {
    let encoded: String = session_id
        .as_bytes()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect();
    format!("{SIMULATOR_PREFIX}{encoded}")
}

/// 从模拟器窗口 label 解码会话 id。
///
/// 这是前端路由的最终兜底：创建窗口时由 Rust 生成 label，因此即使开发服务器
/// 没有正确保留 URL 查询参数，模拟器 WebView 也不会误渲染主窗口面板。
pub fn session_id_from_simulator_label(label: &str) -> Option<String> {
    let hex = label.strip_prefix(SIMULATOR_PREFIX)?;
    if hex.is_empty() || hex.len() % 2 != 0 || !hex.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return None;
    }

    let mut bytes = Vec::with_capacity(hex.len() / 2);
    for pair in hex.as_bytes().chunks_exact(2) {
        let text = std::str::from_utf8(pair).ok()?;
        bytes.push(u8::from_str_radix(text, 16).ok()?);
    }

    let session_id = String::from_utf8(bytes).ok()?;
    (!session_id.is_empty()).then_some(session_id)
}

/// 打开（或显示）某个会话的模拟器窗口；不会影响其它设备的窗口。
pub fn open_simulator_inner(app: &AppHandle, session_id: &str) -> Result<(), String> {
    let reg = app.state::<SessionRegistry>();
    reg.get(session_id)
        .ok_or_else(|| format!("会话不存在：{session_id}"))?;

    let label = simulator_label(session_id);

    if let Some(w) = app.get_webview_window(&label) {
        let _ = w.show();
        let _ = w.set_focus();
        return Ok(());
    }
    // 把同一份安全的 hex id 放进独立入口 URL；即使 URL 查询参数没有保留，
    // simulator-main 也会通过 current_simulator_session 从 Rust 获取它。
    let encoded_id = label
        .strip_prefix(SIMULATOR_PREFIX)
        .ok_or_else(|| "模拟器窗口 label 生成失败".to_string())?;
    let url = format!("simulator.html?simulator={encoded_id}");
    // 创建窗口必须发生在主线程事件循环中（否则 macOS 可能把窗口当独立
    // “应用”处理，出现第二个 Dock 图标）；命令线程在这里同步等待创建结果。
    let app_for_main = app.clone();
    let label_for_main = label.clone();
    let url_for_main = url.clone();
    let session_id_for_main = session_id.to_string();
    run_on_main_thread(app, move || {
        let window = WebviewWindowBuilder::new(
            &app_for_main,
            &label_for_main,
            WebviewUrl::App(url_for_main.into()),
        )
        .title(session_id_for_main.clone())
        .inner_size(420.0, 860.0)
        .min_inner_size(320.0, 560.0)
        // 使用系统原生标题栏，用户可以直接拖动窗口；允许关闭但禁用最大化。
        .decorations(true)
        .closable(true)
        .maximizable(false)
        .resizable(true)
        .build()
        .map_err(|e| format!("创建模拟器窗口失败: {e}"))?;

        // 关闭事件在 Rust 原生窗口层处理。模拟器 WebView 不再通过
        // `preventDefault() -> invoke(stop_session)` 拦截关闭，否则 IPC 或
        // 子进程清理异常时会让标题栏按钮永久无响应。
        let close_started = Arc::new(AtomicBool::new(false));
        let app_for_close = app_for_main.clone();
        let label_for_close = label_for_main.clone();
        let session_id_for_close = session_id_for_main.clone();
        window.on_window_event(move |event| {
            let WindowEvent::CloseRequested { api, .. } = event else {
                return;
            };
            api.prevent_close();
            if close_started.swap(true, Ordering::AcqRel) {
                return;
            }

            let app = app_for_close.clone();
            let label = label_for_close.clone();
            let session_id = session_id_for_close.clone();
            std::thread::spawn(move || {
                if let Err(error) = app.state::<SessionRegistry>().stop_session(&session_id) {
                    log::error!("关闭模拟器窗口时停止会话失败（{}）：{}", session_id, error);
                }

                let Some(window) = app.get_webview_window(&label) else {
                    return;
                };
                let _ = app.run_on_main_thread(move || {
                    let _ = window.destroy();
                });
            });
        });
        Ok(())
    })?
}

/// Tauri 命令入口：打开某设备的模拟器窗口（激活窗口由 activate_device 统一管理）。
/// async：窗口创建由 open_simulator_inner 投递到主线程执行，命令线程不阻塞主循环。
#[tauri::command]
pub async fn open_simulator(app: AppHandle, id: String) -> Result<(), String> {
    open_simulator_inner(&app, &id)
}

/// 返回当前 WebView 是否是模拟器窗口及其会话 id。
///
/// 前端在没有从 URL 得到路由提示时调用该命令。主窗口返回 `None`，
/// 模拟器窗口返回创建它时编码进 label 的会话 id。
#[tauri::command]
pub fn current_simulator_session(window: Window) -> Option<String> {
    session_id_from_simulator_label(window.label())
}

/// 激活切换后同步窗口显示：确保目标窗口存在、可见并聚焦，不影响其它模拟器窗口。
pub fn sync_simulator_windows(
    app: &AppHandle,
    active_id: &str,
) -> Result<(), crate::integrations::AdapterError> {
    let reg = app.state::<SessionRegistry>();
    if reg.get(active_id).is_some() {
        open_simulator_inner(app, active_id).map_err(crate::integrations::AdapterError::Failed)?;
    }
    Ok(())
}

/// 强制销毁某个模拟器窗口（主窗口或模拟器标题栏关闭会话时调用）。
///
/// 使用 destroy 而不是 close，绕过模拟器窗口关闭事件，避免清理流程递归触发。
/// 销毁必须发生在主线程，且不能在承载 IPC 响应的同步命令里立即执行；
/// 这里异步投递，命令先返回、响应先发出，再拆窗口（修复「关闭模拟器后
/// App 卡死」）。
pub fn close_simulator(app: &AppHandle, session_id: &str) {
    let label = simulator_label(session_id);
    if let Some(window) = app.get_webview_window(&label) {
        destroy_on_main_thread(app, window);
    }
}

/// 供测试/调用的 window_label 派生验证。
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn label_encoding_is_reversible_and_safe() {
        assert_eq!(
            simulator_label("iphone:85C76618-C200-5A6B-A692-8704B3DE3467"),
            "simulator-6970686f6e653a38354337363631382d433230302d354136422d413639322d383730344233444533343637"
        );
        assert_eq!(
            simulator_label("android:AB/CD ef"),
            "simulator-616e64726f69643a41422f4344206566"
        );
        assert!(simulator_label("x").starts_with(SIMULATOR_PREFIX));
    }

    #[test]
    fn session_id_decodes_only_valid_simulator_labels() {
        let id = "android:AB/CD ef";
        assert_eq!(
            session_id_from_simulator_label(&simulator_label(id)),
            Some(id.to_string())
        );
        assert_eq!(session_id_from_simulator_label("main"), None);
        assert_eq!(session_id_from_simulator_label("simulator-"), None);
        assert_eq!(session_id_from_simulator_label("simulator-0"), None);
        assert_eq!(session_id_from_simulator_label("simulator-zz"), None);
        assert_eq!(session_id_from_simulator_label("simulator-c3"), None);
    }
}
