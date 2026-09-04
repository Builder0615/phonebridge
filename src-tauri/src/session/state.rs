//! 会话状态机（Spec §5.5）。纯逻辑模块，无 Tauri 依赖，可独立单元测试。

use serde::Serialize;

/// 会话主状态。与前端 `src/lib/types.ts` 的 `SessionStateName` 一一对应。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum SessionStateName {
    #[default]
    Idle,
    Checking,
    Ready,
    Mirroring,
    MirroringConnected,
    ControlPairing,
    ControlReady,
    Reconnecting,
    Stopping,
    Failed,
}

/// 控制子状态（主状态派生）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum ControlState {
    #[default]
    Disabled,
    Broadcasting,
    WaitingPairing,
    Connected,
    InputPaused,
}

/// 镜像子状态（主状态派生）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum MirrorState {
    #[default]
    Idle,
    Starting,
    Connecting,
    Streaming,
    Reconnecting,
    Stopped,
    Failed,
}

/// 状态机事件。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SessionEvent {
    /// 用户点击连接
    UserConnect,
    /// 依赖检查通过
    ChecksPassed,
    /// 依赖检查失败
    ChecksFailed,
    /// AirPlay 接收器已启动
    MirrorStarted,
    /// AirPlay/镜像控制会话已建立（视频首帧可能稍后到达）
    MirrorConnected,
    /// 收到第一帧
    FirstFrame,
    /// 镜像引擎崩溃/超时
    MirrorFailed,
    /// 用户启用控制
    UserEnableControl,
    /// BLE 已配对并订阅
    ControlPaired,
    /// 用户取消控制
    UserCancelControl,
    /// BLE 断开
    BleDisconnected,
    /// 网络/接收器断链
    LinkLost,
    /// 重连成功
    ReconnectOk,
    /// 重连超时
    ReconnectTimeout,
    /// 用户停止
    UserStop,
    /// 资源清理完成
    CleanupDone,
    /// 用户关闭错误
    UserDismissError,
}

/// 非法转移（用于测试与诊断）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct InvalidTransition {
    pub from: SessionStateName,
    pub event: SessionEvent,
}

#[derive(Debug, Default)]
pub struct SessionStateMachine {
    state: SessionStateName,
}

impl SessionStateMachine {
    pub fn new() -> Self {
        Self {
            state: SessionStateName::Idle,
        }
    }

    pub fn state(&self) -> SessionStateName {
        self.state
    }

    /// 执行合法转移；非法组合返回 Err 且状态不变。
    pub fn transition(
        &mut self,
        event: SessionEvent,
    ) -> Result<SessionStateName, InvalidTransition> {
        let from = self.state;
        let next = match (from, event) {
            (SessionStateName::Idle, SessionEvent::UserConnect) => SessionStateName::Checking,

            (SessionStateName::Checking, SessionEvent::ChecksPassed) => SessionStateName::Ready,
            (SessionStateName::Checking, SessionEvent::ChecksFailed) => SessionStateName::Failed,

            (SessionStateName::Ready, SessionEvent::MirrorStarted) => SessionStateName::Mirroring,

            // 用户可以在首帧到达前取消投屏。此前 Mirroring 没有 UserStop
            // 转移，导致“等待手机连接”时取消命令提前返回，UxPlay 和模拟器
            // 窗口都无法清理，下一次启动还会留下占用端口的旧实例。
            (SessionStateName::Checking, SessionEvent::UserStop)
            | (SessionStateName::Ready, SessionEvent::UserStop)
            | (SessionStateName::Mirroring, SessionEvent::UserStop)
            | (SessionStateName::Reconnecting, SessionEvent::UserStop) => {
                SessionStateName::Stopping
            }

            (SessionStateName::Mirroring, SessionEvent::MirrorConnected)
            | (SessionStateName::Mirroring, SessionEvent::FirstFrame) => {
                SessionStateName::MirroringConnected
            }
            // AirPlay 控制连接和首帧是两个独立事件；连接事件先到时，
            // 后续首帧只需保持已连接状态，不应被当成非法转移丢掉。
            (SessionStateName::MirroringConnected, SessionEvent::FirstFrame) => {
                SessionStateName::MirroringConnected
            }
            (SessionStateName::Mirroring, SessionEvent::MirrorFailed) => SessionStateName::Failed,

            (SessionStateName::MirroringConnected, SessionEvent::UserEnableControl) => {
                SessionStateName::ControlPairing
            }
            (SessionStateName::MirroringConnected, SessionEvent::LinkLost) => {
                SessionStateName::Reconnecting
            }
            (SessionStateName::MirroringConnected, SessionEvent::UserStop) => {
                SessionStateName::Stopping
            }

            (SessionStateName::ControlPairing, SessionEvent::ControlPaired) => {
                SessionStateName::ControlReady
            }
            (SessionStateName::ControlPairing, SessionEvent::UserCancelControl) => {
                SessionStateName::MirroringConnected
            }
            (SessionStateName::ControlPairing, SessionEvent::UserStop) => {
                SessionStateName::Stopping
            }

            (SessionStateName::ControlReady, SessionEvent::BleDisconnected) => {
                SessionStateName::MirroringConnected
            }
            (SessionStateName::ControlReady, SessionEvent::UserStop) => SessionStateName::Stopping,

            (SessionStateName::Reconnecting, SessionEvent::ReconnectOk) => {
                SessionStateName::MirroringConnected
            }
            (SessionStateName::Reconnecting, SessionEvent::ReconnectTimeout) => {
                SessionStateName::Failed
            }

            (SessionStateName::Stopping, SessionEvent::CleanupDone) => SessionStateName::Idle,

            (SessionStateName::Failed, SessionEvent::UserDismissError) => SessionStateName::Idle,

            _ => {
                return Err(InvalidTransition { from, event });
            }
        };
        self.state = next;
        Ok(next)
    }
}

/// 由主状态派生控制子状态。
pub fn derive_control_state(state: SessionStateName) -> ControlState {
    match state {
        SessionStateName::ControlPairing => ControlState::WaitingPairing,
        SessionStateName::ControlReady => ControlState::Connected,
        SessionStateName::Stopping => ControlState::Disabled,
        SessionStateName::Failed | SessionStateName::Idle => ControlState::Disabled,
        _ => ControlState::Disabled,
    }
}

/// 由主状态派生镜像子状态。
pub fn derive_mirror_state(state: SessionStateName) -> MirrorState {
    match state {
        SessionStateName::Idle => MirrorState::Idle,
        SessionStateName::Checking | SessionStateName::Ready => MirrorState::Starting,
        SessionStateName::Mirroring => MirrorState::Connecting,
        SessionStateName::MirroringConnected
        | SessionStateName::ControlPairing
        | SessionStateName::ControlReady => MirrorState::Streaming,
        SessionStateName::Reconnecting => MirrorState::Reconnecting,
        SessionStateName::Stopping => MirrorState::Stopped,
        SessionStateName::Failed => MirrorState::Failed,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run_sequence(
        machine: &mut SessionStateMachine,
        events: &[SessionEvent],
    ) -> Vec<SessionStateName> {
        events
            .iter()
            .map(|e| machine.transition(*e).expect("合法转移应成功"))
            .collect()
    }

    #[test]
    fn happy_path_full_cycle() {
        let mut m = SessionStateMachine::new();
        let states = run_sequence(
            &mut m,
            &[
                SessionEvent::UserConnect,       // Checking
                SessionEvent::ChecksPassed,      // Ready
                SessionEvent::MirrorStarted,     // Mirroring
                SessionEvent::FirstFrame,        // MirroringConnected
                SessionEvent::UserEnableControl, // ControlPairing
                SessionEvent::ControlPaired,     // ControlReady
                SessionEvent::UserStop,          // Stopping
                SessionEvent::CleanupDone,       // Idle
            ],
        );
        assert_eq!(
            states,
            vec![
                SessionStateName::Checking,
                SessionStateName::Ready,
                SessionStateName::Mirroring,
                SessionStateName::MirroringConnected,
                SessionStateName::ControlPairing,
                SessionStateName::ControlReady,
                SessionStateName::Stopping,
                SessionStateName::Idle,
            ]
        );
        assert_eq!(m.state(), SessionStateName::Idle);
    }

    #[test]
    fn control_cancel_returns_to_mirroring() {
        let mut m = SessionStateMachine::new();
        run_sequence(
            &mut m,
            &[
                SessionEvent::UserConnect,
                SessionEvent::ChecksPassed,
                SessionEvent::MirrorStarted,
                SessionEvent::FirstFrame,
                SessionEvent::UserEnableControl,
            ],
        );
        assert_eq!(m.state(), SessionStateName::ControlPairing);
        m.transition(SessionEvent::UserCancelControl).unwrap();
        assert_eq!(m.state(), SessionStateName::MirroringConnected);
    }

    #[test]
    fn ble_disconnect_keeps_mirror() {
        let mut m = SessionStateMachine::new();
        run_sequence(
            &mut m,
            &[
                SessionEvent::UserConnect,
                SessionEvent::ChecksPassed,
                SessionEvent::MirrorStarted,
                SessionEvent::FirstFrame,
                SessionEvent::UserEnableControl,
                SessionEvent::ControlPaired,
            ],
        );
        m.transition(SessionEvent::BleDisconnected).unwrap();
        assert_eq!(m.state(), SessionStateName::MirroringConnected);
    }

    #[test]
    fn reconnect_cycle() {
        let mut m = SessionStateMachine::new();
        run_sequence(
            &mut m,
            &[
                SessionEvent::UserConnect,
                SessionEvent::ChecksPassed,
                SessionEvent::MirrorStarted,
                SessionEvent::FirstFrame,
            ],
        );
        m.transition(SessionEvent::LinkLost).unwrap();
        assert_eq!(m.state(), SessionStateName::Reconnecting);
        m.transition(SessionEvent::ReconnectOk).unwrap();
        assert_eq!(m.state(), SessionStateName::MirroringConnected);

        m.transition(SessionEvent::LinkLost).unwrap();
        m.transition(SessionEvent::ReconnectTimeout).unwrap();
        assert_eq!(m.state(), SessionStateName::Failed);
        m.transition(SessionEvent::UserDismissError).unwrap();
        assert_eq!(m.state(), SessionStateName::Idle);
    }

    #[test]
    fn check_failure_goes_failed() {
        let mut m = SessionStateMachine::new();
        m.transition(SessionEvent::UserConnect).unwrap();
        m.transition(SessionEvent::ChecksFailed).unwrap();
        assert_eq!(m.state(), SessionStateName::Failed);
    }

    #[test]
    fn mirror_crash_goes_failed() {
        let mut m = SessionStateMachine::new();
        run_sequence(
            &mut m,
            &[
                SessionEvent::UserConnect,
                SessionEvent::ChecksPassed,
                SessionEvent::MirrorStarted,
            ],
        );
        m.transition(SessionEvent::MirrorFailed).unwrap();
        assert_eq!(m.state(), SessionStateName::Failed);
    }

    #[test]
    fn user_stop_is_allowed_before_first_frame() {
        let mut m = SessionStateMachine::new();
        run_sequence(
            &mut m,
            &[
                SessionEvent::UserConnect,
                SessionEvent::ChecksPassed,
                SessionEvent::MirrorStarted,
            ],
        );
        assert_eq!(m.state(), SessionStateName::Mirroring);
        m.transition(SessionEvent::UserStop).unwrap();
        assert_eq!(m.state(), SessionStateName::Stopping);
    }

    #[test]
    fn airplay_connection_can_precede_first_frame() {
        let mut m = SessionStateMachine::new();
        run_sequence(
            &mut m,
            &[
                SessionEvent::UserConnect,
                SessionEvent::ChecksPassed,
                SessionEvent::MirrorStarted,
                SessionEvent::MirrorConnected,
            ],
        );
        assert_eq!(m.state(), SessionStateName::MirroringConnected);
        m.transition(SessionEvent::FirstFrame).unwrap();
        assert_eq!(m.state(), SessionStateName::MirroringConnected);
    }

    #[test]
    fn invalid_transitions_are_rejected() {
        let mut m = SessionStateMachine::new();
        // Idle 直接 FirstFrame 非法
        let err = m.transition(SessionEvent::FirstFrame).unwrap_err();
        assert_eq!(err.from, SessionStateName::Idle);
        assert_eq!(m.state(), SessionStateName::Idle);

        // Checking 可以取消启动中的会话
        m.transition(SessionEvent::UserConnect).unwrap();
        m.transition(SessionEvent::UserStop).unwrap();
        assert_eq!(m.state(), SessionStateName::Stopping);
        m.transition(SessionEvent::CleanupDone).unwrap();
        assert_eq!(m.state(), SessionStateName::Idle);

        // Ready 不能直接 FirstFrame
        m.transition(SessionEvent::UserConnect).unwrap();
        m.transition(SessionEvent::ChecksPassed).unwrap();
        assert!(m.transition(SessionEvent::FirstFrame).is_err());
        assert_eq!(m.state(), SessionStateName::Ready);
    }

    #[test]
    fn derived_substates() {
        assert_eq!(
            derive_control_state(SessionStateName::ControlPairing),
            ControlState::WaitingPairing
        );
        assert_eq!(
            derive_control_state(SessionStateName::ControlReady),
            ControlState::Connected
        );
        assert_eq!(
            derive_control_state(SessionStateName::Idle),
            ControlState::Disabled
        );

        assert_eq!(
            derive_mirror_state(SessionStateName::Mirroring),
            MirrorState::Connecting
        );
        assert_eq!(
            derive_mirror_state(SessionStateName::MirroringConnected),
            MirrorState::Streaming
        );
        assert_eq!(
            derive_mirror_state(SessionStateName::Reconnecting),
            MirrorState::Reconnecting
        );
        assert_eq!(
            derive_mirror_state(SessionStateName::Stopping),
            MirrorState::Stopped
        );
        assert_eq!(
            derive_mirror_state(SessionStateName::Failed),
            MirrorState::Failed
        );
    }
}
