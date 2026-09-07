//! 快投屏 Tauri 应用入口。

/// 用户可见的产品名称；phonebridge 仅作为内部包名和 identifier 保留。
pub const APP_NAME: &str = "快投屏";

pub mod commands;
pub mod integrations;
pub mod session;

use std::sync::Arc;

use tauri::Manager;
use tauri_plugin_dialog::DialogExt;

use session::events::TauriEventSink;
use session::manager::SessionRegistry;

/// 单实例锁端口（回环）。两个进程无法同时绑定同一地址：第二个实例启动时
/// 发现端口被占，弹窗提示并退出——避免同一台机器上 dev / 安装版 / 挂载版的
/// 快投屏并存，导致程序坞出现重复图标、AirPlay 端口与同名广播互相抢占。
const SINGLE_INSTANCE_ADDR: &str = "127.0.0.1:47777";

fn ensure_single_instance() -> Result<(), String> {
    match std::net::TcpListener::bind(SINGLE_INSTANCE_ADDR) {
        Ok(listener) => {
            // 端口句柄必须存活到进程退出，否则释放后第二个实例会绑定成功。
            Box::leak(Box::new(listener));
            Ok(())
        }
        Err(_) => Err(
            "检测到另一个「快投屏」实例正在运行；为避免程序坞出现重复图标和端口冲突，请先退出其它实例。"
                .into(),
        ),
    }
}

pub fn run() {
    env_logger::init();

    let app = tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_shell::init())
        .setup(|app| {
            if let Err(message) = ensure_single_instance() {
                let handle = app.handle().clone();
                handle
                    .dialog()
                    .message(message)
                    .title(crate::APP_NAME)
                    .kind(tauri_plugin_dialog::MessageDialogKind::Info)
                    .show(|_| {});
                // 等弹窗渲染后退出；退出路径会走 stop_all 清理（无会话可清）。
                std::thread::spawn(move || {
                    std::thread::sleep(std::time::Duration::from_secs(2));
                    handle.exit(0);
                });
                return Ok(());
            }
            let resources = app
                .path()
                .resource_dir()
                .unwrap_or_else(|_| std::env::current_dir().unwrap_or_default());
            // 应用被强杀（SIGKILL/崩溃/tauri dev 重启）时子进程会变孤儿并继续
            // 占用 AirPlay 端口段、广播同名服务；启动时先清理，避免新会话的
            // UxPlay 无法绑定端口（表现为“一直连接不上”）。
            crate::integrations::mirror_adapter::reap_stale_uxplay(&resources);
            let sink = Arc::new(TauriEventSink::new(app.handle().clone()));
            app.manage(SessionRegistry::new(sink, resources));
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            commands::capabilities::check_capabilities,
            commands::session::list_sessions,
            commands::session::start_session,
            commands::session::stop_session,
            commands::session::stop_all,
            commands::session::activate_device,
            commands::windows::open_simulator,
            commands::windows::current_simulator_session,
            commands::hid::hid_pointer_move,
            commands::hid::hid_pointer_button,
            commands::hid::hid_wheel,
            commands::hid::hid_key_stroke,
            commands::clipboard::paste_plain_text,
            commands::diagnostics::get_diagnostics,
            commands::diagnostics::export_diagnostics,
            commands::frame::attach_frame_channel,
            commands::frame::acknowledge_frame,
            commands::frame::frame_test_mode,
            commands::frame::frame_status,
            commands::usb::list_usb_devices_cmd,
            commands::usb::get_ios_control_capability,
            commands::usb::inspect_ios_wda,
            commands::usb::prepare_ios_wda,
            commands::log::get_app_logs,
            commands::log::report_frontend_error,
        ])
        .build(tauri::generate_context!())
        .expect("error while building 快投屏");

    app.run(|app, event| {
        // Tauri dev/reload 或用户直接退出时，WebView 的销毁不一定会
        // 逐个触发模拟器窗口的 close handler；在应用退出边界统一清理，
        // 防止 UxPlay/ffmpeg 变成孤儿进程并占用下一次启动的端口。
        if matches!(
            event,
            tauri::RunEvent::ExitRequested { .. } | tauri::RunEvent::Exit
        ) {
            app.state::<SessionRegistry>().stop_all();
            crate::integrations::ios_wda_setup::stop_all();
        }
    });
}
