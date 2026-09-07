//! ClipboardAdapter：读取 Windows 纯文本剪贴板并转换为 HID 键盘输入（FR-CLP-*）。
//!
//! 隐私约束：
//! - 只在用户显式触发（粘贴按钮 / 画布 Ctrl+V）时读取；不默认读取、不写日志、
//!   不上传、不落盘；
//! - 不读取 HTML、文件列表或图片数据；
//! - 超限或含无法编码字符时返回明确结果，绝不静默截断。
//!
//! 发送遵循 Spec §2.2：大小 → 编码能力 → 敏感提示后发送；停止控制后不再续发。

use serde::Serialize;

use super::hid_adapter::IHidController;
use super::hid_report::{key_down_up, MOD_SHIFT};
use super::text_encoder::encode_ascii_text;
use super::AdapterError;

/// 剪贴板读取接口（便于测试注入 FakeClipboard）。
pub trait ClipboardAdapter: Send + Sync {
    fn read_text(&self) -> Result<String, AdapterError>;
}

/// 系统剪贴板实现（arboard：Windows/macOS/Linux）。
pub struct SystemClipboard;

impl ClipboardAdapter for SystemClipboard {
    fn read_text(&self) -> Result<String, AdapterError> {
        let mut clip = arboard::Clipboard::new()
            .map_err(|e| AdapterError::Failed(format!("打开系统剪贴板失败: {e}")))?;
        clip.get_text()
            .map_err(|e| AdapterError::Failed(format!("读取剪贴板文本失败: {e}")))
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PasteError {
    pub code: String,
    pub message: String,
}

/// 粘贴结果（对应用户动作，一次粘贴一个结果）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PasteResult {
    pub ok: bool,
    pub char_count: usize,
    pub byte_count: usize,
    pub truncated: bool,
    pub unencodable_positions: Vec<u32>,
    pub error: Option<PasteError>,
}

impl PasteResult {
    fn rejected(code: &str, message: impl Into<String>) -> Self {
        Self {
            ok: false,
            char_count: 0,
            byte_count: 0,
            truncated: false,
            unencodable_positions: Vec::new(),
            error: Some(PasteError {
                code: code.into(),
                message: message.into(),
            }),
        }
    }
}

/// 执行粘贴：读取 → 校验 → 编码 → 发送（FakeHid 注入以便测试）。
/// `token` 是取消令牌：发送前检查序号是否与当前一致，停止/释放后不再续发。
pub fn run_paste_flow(
    clipboard: &dyn ClipboardAdapter,
    hid: &mut dyn IHidController,
    max_bytes: usize,
    token: u64,
    current_token: u64,
) -> PasteResult {
    if token != current_token {
        return PasteResult::rejected("paste_cancelled", "控制已停止或已释放输入，粘贴已取消");
    }
    let text = match clipboard.read_text() {
        Ok(t) => t,
        Err(_) => return PasteResult::rejected("clipboard_read_failed", "读取剪贴板失败"),
    };
    run_paste_text_flow(&text, hid, max_bytes, token, current_token)
}

/// 执行已经读取的纯文本粘贴。
///
/// 会话层通常需要先读取文本做设备类型/字符能力检查；复用这个入口可以
/// 保证一次用户操作只读取一次系统剪贴板，避免第二次读取失败后表现为“没有响应”。
pub fn run_paste_text_flow(
    text: &str,
    hid: &mut dyn IHidController,
    max_bytes: usize,
    token: u64,
    current_token: u64,
) -> PasteResult {
    if token != current_token {
        return PasteResult::rejected("paste_cancelled", "控制已停止或已释放输入，粘贴已取消");
    }
    if text.is_empty() {
        return PasteResult::rejected("clipboard_empty", "剪贴板中没有可粘贴的纯文本");
    }
    if text.len() > max_bytes {
        return PasteResult::rejected(
            "paste_oversized",
            format!(
                "剪贴板文本 {0} 字节超过限制 {1} 字节；MVP 不静默截断，请手动分段",
                text.len(),
                max_bytes
            ),
        );
    }

    let enc = encode_ascii_text(text);
    let positions = enc.unencodable_positions.clone();
    if !positions.is_empty() {
        return PasteResult {
            ok: false,
            char_count: enc.char_count,
            byte_count: enc.byte_count,
            truncated: false,
            unencodable_positions: positions.clone(),
            error: Some(PasteError {
                code: "unencodable_chars".into(),
                message: format!(
                    "有 {} 个字符（中文、Emoji 或其他 Unicode）无法通过当前 BLE HID 键盘布局表达，未发送；中英文混合文本请使用 iOS USB/WDA 精确控制",
                    positions.len()
                ),
            }),
        };
    }

    // 发送：每个字符 KeyDown/KeyUp 成对；发送前均检查取消令牌。
    let mut sent = 0usize;
    for c in &enc.chars {
        if token != current_token {
            return PasteResult::rejected("paste_cancelled", "粘贴已取消，已发送部分内容");
        }
        let modifiers = if c.shift { MOD_SHIFT } else { 0 };
        let pair = key_down_up(c.usage, modifiers);
        match hid.send_keyboard(&pair[0]) {
            Ok(()) => {}
            Err(e) => {
                return PasteResult {
                    ok: false,
                    char_count: enc.char_count,
                    byte_count: enc.byte_count,
                    truncated: true,
                    unencodable_positions: Vec::new(),
                    error: Some(PasteError {
                        code: e.code().into(),
                        message: format!("发送中断（已发送 {sent} 字符）：{e}"),
                    }),
                };
            }
        }
        if let Err(e) = hid.send_keyboard(&pair[1]) {
            // 释放报告失败不代表输入丢失，但必须记录。
            log::warn!("KeyUp 报告发送失败：{e}");
            let _ = hid.release_all();
        }
        sent += 1;
    }
    PasteResult {
        ok: true,
        char_count: enc.char_count,
        byte_count: enc.byte_count,
        truncated: false,
        unencodable_positions: Vec::new(),
        error: None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::integrations::hid_adapter::test_support::FakeHid;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;

    struct FakeClipboard {
        text: String,
    }
    impl FakeClipboard {
        fn new(text: &str) -> Self {
            Self {
                text: text.to_string(),
            }
        }
    }
    impl ClipboardAdapter for FakeClipboard {
        fn read_text(&self) -> Result<String, AdapterError> {
            Ok(self.text.clone())
        }
    }

    struct SingleReadClipboard {
        reads: Arc<AtomicUsize>,
    }

    impl ClipboardAdapter for SingleReadClipboard {
        fn read_text(&self) -> Result<String, AdapterError> {
            if self.reads.fetch_add(1, Ordering::SeqCst) != 0 {
                return Err(AdapterError::Failed("clipboard read more than once".into()));
            }
            Ok("paste once".into())
        }
    }

    #[test]
    fn paste_flow_reads_system_clipboard_once() {
        let reads = Arc::new(AtomicUsize::new(0));
        let clip = SingleReadClipboard {
            reads: reads.clone(),
        };
        let mut hid = FakeHid::new();
        let result = run_paste_flow(&clip, &mut hid, 32 * 1024, 1, 1);
        assert!(result.ok, "一次读取后应完成粘贴：{result:?}");
        assert_eq!(reads.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn paste_ascii_sends_paired_reports() {
        let clip = FakeClipboard::new("Ab!");
        let mut hid = FakeHid::new();
        let result = run_paste_flow(&clip, &mut hid, 32 * 1024, 1, 1);
        assert!(result.ok, "粘贴应成功：{:?}", result);
        assert_eq!(result.char_count, 3);
        // 每字符一对报告 = 6 个键盘报告
        assert_eq!(hid.keyboard_reports.len(), 6);
        // 每对报告 KeyDown 后必须跟 KeyUp（空键数组）
        for pair in hid.keyboard_reports.chunks(2) {
            assert_ne!(pair[0].keys[0], 0, "KeyDown 必须携带 usage");
            assert_eq!(pair[1].keys, [0; 6], "KeyUp 必须是空报告");
        }
    }

    #[test]
    fn shift_chars_carry_shift_modifier() {
        let clip = FakeClipboard::new("A");
        let mut hid = FakeHid::new();
        run_paste_flow(&clip, &mut hid, 32 * 1024, 1, 1);
        let down = &hid.keyboard_reports[0];
        assert_eq!(down.keys[0], 0x04);
        assert_eq!(down.modifiers, MOD_SHIFT);
    }

    #[test]
    fn unencodable_chars_are_not_sent() {
        let clip = FakeClipboard::new("中文é✅");
        let mut hid = FakeHid::new();
        let result = run_paste_flow(&clip, &mut hid, 32 * 1024, 1, 1);
        assert!(!result.ok);
        assert_eq!(result.error.as_ref().unwrap().code, "unencodable_chars");
        assert!(!result.unencodable_positions.is_empty());
        assert!(hid.keyboard_reports.is_empty(), "不得发送不可编码内容");
    }

    #[test]
    fn oversized_text_rejected_without_sending() {
        let clip = FakeClipboard::new(&"x".repeat(40 * 1024));
        let mut hid = FakeHid::new();
        let result = run_paste_flow(&clip, &mut hid, 32 * 1024, 1, 1);
        assert!(!result.ok);
        assert_eq!(result.error.as_ref().unwrap().code, "paste_oversized");
        assert!(hid.keyboard_reports.is_empty());
    }

    #[test]
    fn empty_clipboard_rejected() {
        let clip = FakeClipboard::new("");
        let mut hid = FakeHid::new();
        let result = run_paste_flow(&clip, &mut hid, 32 * 1024, 1, 1);
        assert!(!result.ok);
        assert_eq!(result.error.as_ref().unwrap().code, "clipboard_empty");
    }

    #[test]
    fn cancelled_paste_sends_nothing() {
        let clip = FakeClipboard::new("hello");
        let mut hid = FakeHid::new();
        let result = run_paste_flow(&clip, &mut hid, 32 * 1024, 7, 1);
        assert!(!result.ok);
        assert_eq!(result.error.as_ref().unwrap().code, "paste_cancelled");
        assert!(hid.keyboard_reports.is_empty());
    }
}
