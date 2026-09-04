//! 会话命令：多设备连接/断开/激活（Spec v1.2 §5.4）。

use serde::{Deserialize, Serialize};
use tauri::{AppHandle, Manager};

use crate::commands::CmdResult;
use crate::integrations::hid_adapter::HidStatus;
use crate::session::device::DeviceKind;
use crate::session::manager::{SessionPreferences, SessionRegistry, SessionStateView};

/// start_session 参数。
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StartSessionRequest {
    pub kind: String, // "iphone" | "android"
    pub id: String,   // "iphone:<udid>" | "android:<serial>"
    #[serde(default)]
    pub prefs: Option<SessionPreferences>,
}

/// 会话信息（面板列表）。
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionInfo {
    pub id: String,
    pub kind: String,
    pub active: bool,
    pub state: SessionStateView,
    pub hid: HidStatus,
    pub mirror_size: Option<(u32, u32)>,
}

#[tauri::command]
pub fn list_sessions(app: AppHandle) -> Vec<SessionInfo> {
    let reg = app.state::<SessionRegistry>();
    let active = reg.active();
    reg.list()
        .into_iter()
        .map(|(id, kind, state)| SessionInfo {
            id: id.clone(),
            kind: kind.as_str().into(),
            active: active.as_deref() == Some(id.as_str()),
            state,
            hid: reg.get(&id).map(|s| s.hid_status()).unwrap_or_default(),
            mirror_size: reg.get(&id).and_then(|s| s.mirror_metadata()),
        })
        .collect()
}

#[tauri::command]
pub fn start_session(app: AppHandle, request: StartSessionRequest) -> CmdResult<()> {
    let kind = DeviceKind::from_str(&request.kind)
        .ok_or_else(|| crate::commands::CommandErrorPayload::new("invalid_kind", "未知设备类型"))?;
    app.state::<SessionRegistry>()
        .start_session(kind, request.id, request.prefs)
        .map_err(Into::into)
}

/// 断开指定设备会话（异步：清理与子进程回收不占用主线程，避免
/// 关闭模拟器/取消投屏时 App 卡死；窗口销毁由 windows::close_simulator
/// 异步投递到主线程，先返回命令响应再拆窗）。
#[tauri::command]
pub async fn stop_session(app: AppHandle, id: String) -> CmdResult<()> {
    // 先记住停止结果，但无论子进程清理是否返回错误，都必须关闭对应
    // 模拟器窗口；否则“取消投屏”会留下黑色空窗，且用户无法再次打开同一会话。
    // SessionRegistry::stop_session 会同步停止 BLE/镜像子进程。即使各适配器
    // 已尽量做到非阻塞，也不能把它放在 Tauri async runtime 的执行线程上，
    // 否则某个原生回调或异常子进程仍可能拖住命令分发，表现为点击后 App 卡死。
    let stop_id = id.clone();
    let app_for_stop = app.clone();
    let result = tauri::async_runtime::spawn_blocking(move || {
        app_for_stop
            .state::<SessionRegistry>()
            .stop_session(&stop_id)
    })
    .await
    .map_err(|error| {
        crate::commands::CommandErrorPayload::new(
            "stop_session",
            format!("停止投屏任务失败：{error}"),
        )
    })?;
    // 取消投屏 = 同时关闭对应模拟器窗口（非阻塞，主线程异步销毁）
    crate::commands::windows::close_simulator(&app, &id);
    result.map_err(Into::into)
}

#[tauri::command]
pub async fn stop_all(app: AppHandle) -> CmdResult<()> {
    let ids: Vec<String> = app
        .state::<SessionRegistry>()
        .list()
        .into_iter()
        .map(|(id, _, _)| id)
        .collect();
    let app_for_stop = app.clone();
    tauri::async_runtime::spawn_blocking(move || {
        app_for_stop.state::<SessionRegistry>().stop_all();
    })
    .await
    .map_err(|error| {
        crate::commands::CommandErrorPayload::new("stop_all", format!("停止投屏任务失败：{error}"))
    })?;
    for id in ids {
        // 全部断开也必须清理所有无装饰模拟器窗口（异步销毁）。
        crate::commands::windows::close_simulator(&app, &id);
    }
    Ok(())
}

#[tauri::command]
pub async fn activate_device(app: AppHandle, id: String) -> CmdResult<()> {
    let reg = app.state::<SessionRegistry>();
    reg.activate(&id)?;
    crate::commands::windows::sync_simulator_windows(&app, &id)?;
    Ok(())
}

#[tauri::command]
pub async fn start_control(app: AppHandle, id: String, prefs: SessionPreferences) -> CmdResult<()> {
    let session = app
        .state::<SessionRegistry>()
        .get(&id)
        .ok_or_else(|| crate::commands::CommandErrorPayload::new("no_session", "会话不存在"))?;
    tauri::async_runtime::spawn_blocking(move || session.start_control(Some(prefs)))
        .await
        .map_err(|error| {
            crate::commands::CommandErrorPayload::new(
                "start_control",
                format!("启动控制任务失败：{error}"),
            )
        })?
        .map_err(Into::into)
}

#[tauri::command]
pub async fn stop_control(app: AppHandle, id: String) -> CmdResult<()> {
    let session = app
        .state::<SessionRegistry>()
        .get(&id)
        .ok_or_else(|| crate::commands::CommandErrorPayload::new("no_session", "会话不存在"))?;
    tauri::async_runtime::spawn_blocking(move || session.stop_control())
        .await
        .map_err(|error| {
            crate::commands::CommandErrorPayload::new(
                "stop_control",
                format!("停止控制任务失败：{error}"),
            )
        })?
        .map_err(Into::into)
}

#[tauri::command]
pub async fn release_all_input(app: AppHandle, id: String) -> CmdResult<()> {
    let session = app.state::<SessionRegistry>().get(&id);
    tauri::async_runtime::spawn_blocking(move || {
        if let Some(session) = session {
            session.release_all_input();
        }
    })
    .await
    .map_err(|error| {
        crate::commands::CommandErrorPayload::new(
            "release_all_input",
            format!("释放输入任务失败：{error}"),
        )
    })?;
    Ok(())
}
