//! 诊断命令：一键诊断与导出诊断包（FR-DIAG-001/002）。

use serde::Serialize;
use tauri::{AppHandle, Manager};

use crate::commands::CmdResult;
use crate::integrations::diagnostics::{
    collect_diagnostics, export_diagnostics as write_diagnostics_file, DiagnosticsReport,
};
use crate::session::manager::SessionRegistry as SessionManager;

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ExportResult {
    pub ok: bool,
    pub path: Option<String>,
    pub items: Vec<String>,
    pub error: Option<String>,
}

#[tauri::command]
pub fn get_diagnostics(app: AppHandle) -> DiagnosticsReport {
    let mgr = app.state::<SessionManager>();
    collect_diagnostics(
        &app.package_info().version.to_string(),
        tauri::VERSION,
        env!("CARGO_PKG_VERSION"),
        true,
        &mgr.aggregate_error_log(),
        6000, // UxPlay 端口段基址
        vec![
            "core:default".into(),
            "dialog:default".into(),
            "shell:allow-execute(binaries/uxplay)".into(),
        ],
        mgr.resources_dir(),
    )
}

#[tauri::command]
pub fn export_diagnostics(app: AppHandle) -> CmdResult<ExportResult> {
    let mgr = app.state::<SessionManager>();
    let report = collect_diagnostics(
        &app.package_info().version.to_string(),
        tauri::VERSION,
        env!("CARGO_PKG_VERSION"),
        true,
        &mgr.aggregate_error_log(),
        6000, // UxPlay 端口段基址
        vec![
            "core:default".into(),
            "dialog:default".into(),
            "shell:allow-execute(binaries/uxplay)".into(),
        ],
        mgr.resources_dir(),
    );
    let data_dir = app
        .path()
        .app_data_dir()
        .map_err(|e| crate::commands::CommandErrorPayload::new("app_data_dir", e.to_string()))?;
    match write_diagnostics_file(&report, &data_dir) {
        Ok((path, items)) => Ok(ExportResult {
            ok: true,
            path: Some(path),
            items,
            error: None,
        }),
        Err(e) => Ok(ExportResult {
            ok: false,
            path: None,
            items: Vec::new(),
            error: Some(e.to_string()),
        }),
    }
}
