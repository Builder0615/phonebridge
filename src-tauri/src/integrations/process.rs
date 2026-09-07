//! 受控的宿主侧子进程启动。
//!
//! Tauri 在 Windows 上通常以 GUI 子系统运行，但 `std::process::Command`
//! 启动控制台程序时仍可能创建一个短暂的控制台窗口。设备轮询会周期性
//! 调用 adb/idevice_id，这会表现为命令行窗口反复闪烁。所有外部工具都从
//! 这里创建，Windows 使用 CREATE_NO_WINDOW；其它平台保持标准行为。

use std::ffi::OsStr;
use std::process::Command;

/// 创建一个不会在 Windows 桌面上显示控制台窗口的子进程命令。
pub(crate) fn hidden_command(program: impl AsRef<OsStr>) -> Command {
    #[cfg(target_os = "windows")]
    {
        use std::os::windows::process::CommandExt;

        let mut command = Command::new(program);
        // CREATE_NO_WINDOW = 0x08000000。它适用于 console subsystem 的
        // exe（adb、idevice_id、iproxy、ffmpeg 等），不会改变 stdout/stderr
        // 的管道行为，也不会把输出写入用户的控制台。
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        command.creation_flags(CREATE_NO_WINDOW);
        command
    }

    #[cfg(not(target_os = "windows"))]
    {
        Command::new(program)
    }
}
