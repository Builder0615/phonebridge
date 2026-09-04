//! 帧桥接框架（Spec v1.1 §5.1 帧桥接决策）：把镜像源的视频帧解码为 RGBA，
//! 经 Tauri Channel 二进制通道推送到前端 `<canvas>` 渲染。
//!
//! 设计原则（诚实、可测试、默认无重依赖）：
//! - `FrameSink`：一个可推送 RGBA 帧的对象（Tauri Channel 实现 + 测试用内存实现）；
//! - `FrameSource`：适配器，从视频流产生 RGBA 帧；
//! - 解码用 `video-decode` feature 门控：
//!   - 默认：无 FFmpeg 依赖，提供受控的生成帧测试源与空实现（跨机可跑）；
//!   - 开启 `--features video-decode`：接入 ffmpeg-next，把 H264 流解码为 RGBA；
//! - 帧是二进制，绝不用 JSON/base64 高频传输（Spec §5.4）。

use std::sync::Arc;

use crate::integrations::AdapterError;

/// RGBA 帧：row-major 原始像素。
#[derive(Debug, Clone)]
pub struct RgbaFrame {
    pub width: u32,
    pub height: u32,
    pub rgba: Arc<[u8]>,
}

impl RgbaFrame {
    pub fn len_bytes(&self) -> usize {
        self.rgba.len()
    }
    pub fn is_empty(&self) -> bool {
        self.rgba.is_empty()
    }
}

/// 帧接收端：可被前端消费（Tauri Channel 的二进制 InvokeResponseBody）。
pub trait FrameSink: Send + Sync {
    fn push(&self, frame: &RgbaFrame);
    /// 尺寸变化时通知（前端调整 canvas）。
    fn on_size(&self, width: u32, height: u32);
    /// 前端确认已收到一帧；有背压的 sink 可据此继续发送最新帧。
    fn acknowledge(&self, _sequence: u64) {}
}

/// 可选的帧源能力标记。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DecodeStatus {
    /// 真实视频解码可用（video-decode feature 开启）
    Real,
    /// 无 FFmpeg，仅测试生成帧
    TestOnly,
    /// 源不支持主动推帧
    Unavailable,
}

/// 帧源：从镜像适配器提取帧推送到 sink。
pub trait FrameSource: Send {
    /// 返回该源当前的解码可用性（诚实告知后端能力）。
    fn decode_status(&self) -> DecodeStatus;
    /// 把 sink 绑定到源（每次会话开始调用一次）。
    fn attach_sink(&mut self, sink: Arc<dyn FrameSink>);
    /// 是否正在生成帧。
    fn is_rendering(&self) -> bool {
        false
    }
}

/// 生成一个测试用 RGBA 交彩色带帧（验证端到端管线，不伪造真实画面）。
pub fn generate_test_frame(width: u32, height: u32, t: u32) -> RgbaFrame {
    let w = width.max(1) as usize;
    let h = height.max(1) as usize;
    let mut rgba = vec![0u8; w * h * 4];
    for y in 0..h {
        for x in 0..w {
            let i = (y * w + x) * 4;
            // 彩条 + 时间戳色调变化
            let (r, g, b): (u8, u8, u8) = match (x as u32 * 8 / w as u32) % 6 {
                0 => (255, 0, 0),
                1 => (0, 255, 0),
                2 => (0, 0, 255),
                3 => (255, 255, 0),
                4 => (0, 255, 255),
                _ => (255, 0, 255),
            };
            let tint = (t % 60) as u8;
            rgba[i] = r.wrapping_add(tint);
            rgba[i + 1] = g.wrapping_add(tint);
            rgba[i + 2] = b.wrapping_add(tint);
            rgba[i + 3] = 255;
        }
    }
    RgbaFrame {
        width: w as u32,
        height: h as u32,
        rgba: Arc::from(rgba),
    }
}

/// 真实解码入口（feature 门控）。默认编译为 Disabled 占位，不链接 FFmpeg。
/// 开启 `--features video-decode` 时通过 ffmpeg CLI 子进程解码（详见 ffmpeg_bridge）。
#[derive(Debug)]
pub struct VideoDecoder {
    pub(crate) inner: VideoDecoderInner,
}

#[derive(Debug)]
#[allow(dead_code)] // Disabled 变体仅在非 feature 默认构建下不构造（decode 分支仍引用）
pub(crate) enum VideoDecoderInner {
    Disabled,
    #[cfg(feature = "video-decode")]
    Ffmpeg(crate::integrations::ffmpeg_bridge::FFmpegVideoDecoder),
}

impl VideoDecoder {
    /// 尝试创建解码器；feature 未开启或无系统 ffmpeg 时返回可理解错误，绝不 panic。
    pub fn try_create(_width: u32, _height: u32) -> Result<Self, AdapterError> {
        #[cfg(feature = "video-decode")]
        {
            let d = crate::integrations::ffmpeg_bridge::FFmpegVideoDecoder::new(_width, _height)?;
            return Ok(Self {
                inner: VideoDecoderInner::Ffmpeg(d),
            });
        }
        #[cfg(not(feature = "video-decode"))]
        {
            Err(AdapterError::PendingRealDeviceValidation(
                "视频解码（FFmpeg）未启用：默认构建不含 FFmpeg 依赖。用 --features video-decode 启用后即可把镜像流解码为画面。".into(),
            ))
        }
    }

    /// 解码一段 H264 到 RGBA 帧；未启用或无输入返回 None/Err。
    #[allow(unused_variables)]
    pub fn decode(&self, h264: &[u8], w: u32, h: u32) -> Result<Option<RgbaFrame>, AdapterError> {
        match &self.inner {
            VideoDecoderInner::Disabled => Ok(None),
            #[cfg(feature = "video-decode")]
            VideoDecoderInner::Ffmpeg(d) => d.decode_h264(h264, w, h),
        }
    }
}

/// 帧桥接运行时：持有解码器状态与 sink 绑定（由 SessionManager 拥有）。
pub struct FrameHub {
    pub sink: Option<Arc<dyn FrameSink>>,
    pub decode: DecodeStatus,
}

impl FrameHub {
    pub fn new(sink: Option<Arc<dyn FrameSink>>) -> Self {
        Self {
            sink,
            decode: DecodeStatus::Unavailable,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_frame_dimensions_and_rgba_len() {
        let f = generate_test_frame(320, 240, 7);
        assert_eq!(f.width, 320);
        assert_eq!(f.height, 240);
        assert_eq!(f.rgba.len(), 320 * 240 * 4);
        assert_eq!(f.rgba[3], 255); // alpha
    }

    #[test]
    fn zero_size_is_guarded_to_non_empty() {
        // 0 尺寸被保护为 1x1，避免除零/空缓冲区；帧不应为空。
        let f = generate_test_frame(0, 0, 0);
        assert_eq!(f.width, 1);
        assert_eq!(f.height, 1);
        assert_eq!(f.rgba.len(), 4);
    }

    #[test]
    fn decode_status_default_unavailable_without_feature() {
        // 默认构建（无 video-decode）下解码器不可用，但不 panic。
        let _ = VideoDecoder::try_create(1920, 1080);
    }

    struct MemSink {
        frames: std::sync::Mutex<Vec<RgbaFrame>>,
    }
    impl FrameSink for MemSink {
        fn push(&self, frame: &RgbaFrame) {
            self.frames.lock().unwrap().push(frame.clone());
        }
        fn on_size(&self, _w: u32, _h: u32) {}
    }

    #[test]
    fn frame_sink_pushes() {
        let sink = Arc::new(MemSink {
            frames: std::sync::Mutex::new(Vec::new()),
        });
        let f = generate_test_frame(16, 16, 1);
        sink.push(&f);
        assert_eq!(sink.frames.lock().unwrap().len(), 1);
    }
}
