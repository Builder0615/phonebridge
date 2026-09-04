//! 帧通道：把 RGBA 帧经 Tauri Channel（二进制）推送到前端画布。
//!
//! 前端在 `attach_frame_channel` 传入 `tauri::ipc::Channel`，Rust 端封装为
//! `FrameSink`；帧以原始字节推送（`InvokeResponseBody::Raw`），不走 JSON/base64。
//! 原始帧前带有内部 `PBFR + u64 sequence` 头，前端确认后才继续发送下一帧。

use std::sync::Mutex;

use tauri::ipc::Channel;

use crate::integrations::frame_bridge::{FrameSink, RgbaFrame};

const FRAME_MAGIC: &[u8; 4] = b"PBFR";
const FRAME_HEADER_BYTES: usize = 4 + 8;

struct PendingFrame {
    sequence: u64,
    bytes: Vec<u8>,
}

struct ChannelState {
    next_sequence: u64,
    in_flight: Option<u64>,
    // Keep the latest frame in its shared RGBA allocation while a previous
    // packet is in flight. Encoding the packet here would copy the whole
    // frame even though it is going to be replaced by the next frame anyway.
    pending: Option<RgbaFrame>,
}

/// 把前端 Channel 包装成有界 FrameSink。
///
/// Tauri 大消息会为每次 send 创建一次 fetch IPC。若 WebView 短暂繁忙，
/// 无界发送会让旧帧排队，表现为高延迟。这里最多保留一帧在途和一帧最新
/// 待发送帧，前端确认收到后才继续发，始终优先实时性。
pub struct ChannelSink {
    channel: Channel<tauri::ipc::InvokeResponseBody>,
    state: Mutex<ChannelState>,
}

impl ChannelSink {
    pub fn new(channel: Channel<tauri::ipc::InvokeResponseBody>) -> Self {
        Self {
            channel,
            state: Mutex::new(ChannelState {
                next_sequence: 1,
                in_flight: None,
                pending: None,
            }),
        }
    }

    fn make_packet(state: &mut ChannelState, frame: &RgbaFrame) -> PendingFrame {
        let sequence = state.next_sequence;
        state.next_sequence = state.next_sequence.wrapping_add(1).max(1);
        let mut bytes = Vec::with_capacity(FRAME_HEADER_BYTES + frame.rgba.len());
        bytes.extend_from_slice(FRAME_MAGIC);
        bytes.extend_from_slice(&sequence.to_le_bytes());
        bytes.extend_from_slice(&frame.rgba);
        PendingFrame { sequence, bytes }
    }

    fn send_packet(&self, packet: PendingFrame) {
        if self
            .channel
            .send(tauri::ipc::InvokeResponseBody::Raw(packet.bytes))
            .is_err()
        {
            let mut state = self.state.lock().unwrap();
            if state.in_flight == Some(packet.sequence) {
                state.in_flight = None;
                state.pending = None;
            }
        }
    }

    /// 前端收到带序号帧后调用；收到确认才放行下一帧。
    pub fn acknowledge(&self, sequence: u64) {
        let next = {
            let mut state = self.state.lock().unwrap();
            if state.in_flight != Some(sequence) {
                return;
            }
            if let Some(frame) = state.pending.take() {
                let packet = Self::make_packet(&mut state, &frame);
                state.in_flight = Some(packet.sequence);
                Some(packet)
            } else {
                state.in_flight = None;
                None
            }
        };
        if let Some(packet) = next {
            self.send_packet(packet);
        }
    }
}

impl FrameSink for ChannelSink {
    fn push(&self, frame: &RgbaFrame) {
        let send_now = {
            let mut state = self.state.lock().unwrap();
            if state.in_flight.is_some() {
                // 只保留最新帧；旧帧没有显示价值，只会增加输入/画面延迟。
                // 不要在这里编码完整的 IPC packet，否则前端繁忙时每帧
                // 都会复制约 2MB RGBA 数据，可能反过来阻塞 UxPlay 管道。
                state.pending = Some(frame.clone());
                None
            } else {
                let packet = Self::make_packet(&mut state, frame);
                state.in_flight = Some(packet.sequence);
                Some(packet)
            }
        };
        if let Some(packet) = send_now {
            self.send_packet(packet);
        }
    }

    fn on_size(&self, width: u32, height: u32) {
        let payload = serde_json::json!({ "width": width, "height": height });
        let _ = self
            .channel
            .send(tauri::ipc::InvokeResponseBody::Json(payload.to_string()));
    }

    fn acknowledge(&self, sequence: u64) {
        Self::acknowledge(self, sequence);
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use super::*;

    #[test]
    fn packet_builder_adds_protocol_header_and_sequence() {
        let mut state = ChannelState {
            next_sequence: 7,
            in_flight: None,
            pending: None,
        };
        let frame = RgbaFrame {
            width: 1,
            height: 1,
            rgba: Arc::from([1_u8, 2, 3, 4].as_slice()),
        };

        let packet = ChannelSink::make_packet(&mut state, &frame);

        assert_eq!(packet.sequence, 7);
        assert_eq!(state.next_sequence, 8);
        assert_eq!(&packet.bytes[..4], b"PBFR");
        assert_eq!(
            u64::from_le_bytes(packet.bytes[4..12].try_into().unwrap()),
            7
        );
        assert_eq!(&packet.bytes[12..], &[1, 2, 3, 4]);
    }
}
