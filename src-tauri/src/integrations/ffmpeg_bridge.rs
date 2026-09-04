//! FFmpeg 解码桥接（`video-decode` feature 门控）。
//!
//! 仅在启用 `--features video-decode` 时编译真实解码逻辑，链接系统 FFmpeg
//! （libavcodec 等）。默认构建不含此模块的 FFmpeg 依赖，保证 `cargo check/test`
//! 在无 FFmpeg 的机器上可跑（AGENTS.md 验证命令通用性）。
//!
//! 这里用受控的子进程调用系统 `ffmpeg` CLI 把编码流解码为原始 RGBA——
//! 与现有子进程模式一致（白名单参数、不拼接用户输入），比静态链接 ffmpeg-next
//! 更便于跨机部署，且避免 crates.io 上 ffmpeg-next 的构建系统依赖。

use std::io::Read;
use std::process::{Command, Stdio};

use serde::Serialize;

use super::frame_bridge::RgbaFrame;
use super::AdapterError;

/// 检查系统是否可调用 ffmpeg。
pub fn ffmpeg_available() -> bool {
    Command::new("ffmpeg")
        .arg("-version")
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

/// FFmpeg CLI 解码器：将裸 H264（或任意编码流）解码为原始 RGBA。
#[derive(Debug)]
pub struct FFmpegVideoDecoder {
    width: u32,
    height: u32,
}

impl FFmpegVideoDecoder {
    /// 确认系统 ffmpeg 可用。
    pub fn ensure_available() -> Result<(), AdapterError> {
        if ffmpeg_available() {
            Ok(())
        } else {
            Err(AdapterError::DependencyMissing(
                "启用 video-decode 需要系统 ffmpeg 可执行文件（PATH 中）。".into(),
            ))
        }
    }

    /// 创建解码器。dimensions 可选：提供后可在流解析失败时有兜底。
    pub fn new(width: u32, height: u32) -> Result<Self, AdapterError> {
        Self::ensure_available()?;
        Ok(Self { width, height })
    }

    /// 将一段 H264 编码数据解码为 RGBA 帧。
    /// stdin 输入编码流，stdout 输出 rawvideo rgba。白名单参数，ffmpeg 路径固定。
    pub fn decode_h264(
        &self,
        h264: &[u8],
        w: u32,
        h: u32,
    ) -> Result<Option<RgbaFrame>, AdapterError> {
        if h264.is_empty() {
            return Ok(None);
        }
        let width = if w > 0 { w } else { self.width };
        let height = if h > 0 { h } else { self.height };
        if width == 0 || height == 0 {
            return Ok(None);
        }

        let mut cmd = Command::new("ffmpeg");
        cmd.args([
            "-loglevel",
            "error",
            "-f",
            "h264",
            "-i",
            "pipe:0",
            "-f",
            "rawvideo",
            "-pix_fmt",
            "rgba",
            "-vf",
            // 缩放到目标尺寸，避免与元数据不一致
            &format!("scale={width}:{height}"),
            "pipe:1",
        ])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());

        let mut child = cmd
            .spawn()
            .map_err(|e| AdapterError::Failed(format!("启动 ffmpeg 失败: {e}")))?;
        {
            let mut stdin = child.stdin.take().expect("stdin piped");
            use std::io::Write;
            let _ = stdin.write_all(h264);
        }
        let mut rgbs: Vec<u8> = Vec::with_capacity((width * height * 4) as usize);
        if let Some(ref mut stdout) = child.stdout {
            stdout
                .read_to_end(&mut rgbs)
                .map_err(|e| AdapterError::Failed(format!("读取解码帧失败: {e}")))?;
        }
        let _ = child.wait();

        let expected = (width * height * 4) as usize;
        if rgbs.len() < expected {
            // 数据不足：可能只解码了部分帧或失败，不返回残缺帧。
            return Ok(None);
        }
        rgbs.truncate(expected);
        Ok(Some(RgbaFrame {
            width,
            height,
            rgba: rgbs.into(),
        }))
    }
}

/// 诊断：FFmpeg 版本信息（脱敏）。
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FfmpegInfo {
    pub available: bool,
    pub version: Option<String>,
}

pub fn ffmpeg_info() -> FfmpegInfo {
    if !ffmpeg_available() {
        return FfmpegInfo {
            available: false,
            version: None,
        };
    }
    let out = Command::new("ffmpeg")
        .arg("-version")
        .output()
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        .unwrap_or_default();
    let first = out.lines().next().map(|s| s.to_string());
    FfmpegInfo {
        available: true,
        version: first,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ffmpeg_detect_does_not_panic() {
        // 无论系统是否安装 ffmpeg，检查都不应 panic。
        let _ = ffmpeg_available();
        let _ = ffmpeg_info();
    }

    #[test]
    fn decode_empty_yields_none() {
        if let Ok(d) = FFmpegVideoDecoder::new(64, 64) {
            assert!(d.decode_h264(&[], 64, 64).unwrap_or(None).is_none());
        }
    }
}
