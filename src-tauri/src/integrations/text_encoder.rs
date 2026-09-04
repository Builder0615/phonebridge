//! 文本编码：把纯文本转为可经 HID 键盘表达的字符序列（Spec §6.2、FR-CLP-005）。
//!
//! 规则：
//! - 只编码通过当前 HID 键盘布局可表达的字符（字母、数字、常用标点、空格、
//!   回车 → Enter、Tab → Tab）；
//! - 无法表达的字符（含中文、Emoji、其它 Unicode）必须返回失败位置，
//!   绝不静默丢失；
//! - 位置采用 UTF-16 索引，与前端 JavaScript 字符串索引一致。

use serde::Serialize;

use super::hid_report::{hid_char_for, EncodedChar};

/// 文本编码结果。
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TextEncoding {
    /// 可表达字符（顺序保持原文本顺序）。
    pub chars: Vec<EncodedChar>,
    /// 无法表达字符的 UTF-16 位置（递增）。
    pub unencodable_positions: Vec<u32>,
    /// UTF-8 字节数。
    pub byte_count: usize,
    /// UTF-16 字符数（与前端 length 语义一致）。
    pub char_count: usize,
}

/// 编码纯文本；不可表达字符记入 unencodable_positions。
pub fn encode_ascii_text(text: &str) -> TextEncoding {
    let mut chars = Vec::with_capacity(text.len());
    let mut unencodable_positions = Vec::new();
    let mut utf16_index: u32 = 0;

    for c in text.chars() {
        match hid_char_for(c) {
            Some(h) => {
                let char_len = c.len_utf16() as u32;
                // 单个 HID 字符对应的可能不止一个 UTF-16 单元（如代理对），
                // 但能被 hid_char_for 编码的都是单单元字符。
                debug_assert_eq!(char_len, 1);
                chars.push(EncodedChar {
                    usage: h.usage,
                    shift: h.shift,
                });
                utf16_index += char_len;
            }
            None => {
                unencodable_positions.push(utf16_index);
                utf16_index += c.len_utf16() as u32;
            }
        }
    }

    TextEncoding {
        chars,
        unencodable_positions,
        byte_count: text.len(),
        char_count: utf16_index as usize,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ascii_roundtrip_is_fully_encodable() {
        let enc = encode_ascii_text("Hello, World! 123");
        assert!(enc.unencodable_positions.is_empty());
        assert_eq!(enc.chars.len(), 17);
        assert_eq!(enc.char_count, 17);
        assert_eq!(enc.byte_count, "Hello, World! 123".len());
    }

    #[test]
    fn chinese_and_emoji_are_unencodable_with_positions() {
        let text = "a中文😀b";
        let enc = encode_ascii_text(text);
        // '中' 是第 1 个 UTF-16 单元，'文' 第 2，'😀'(代理对) 第 3、4
        assert_eq!(enc.unencodable_positions, vec![1, 2, 3]);
        assert_eq!(enc.char_count, 6); // a 中 文 😀 b → 1+1+1+2+1
        assert_eq!(enc.chars.len(), 2); // 只有 a 和 b 可编码
    }

    #[test]
    fn newline_and_tab_are_encodable() {
        let enc = encode_ascii_text("a\nb\tc");
        assert!(enc.unencodable_positions.is_empty());
        assert_eq!(enc.chars.len(), 5);
    }

    #[test]
    fn shift_chars_are_marked() {
        let enc = encode_ascii_text("A!");
        assert_eq!(
            enc.chars,
            vec![
                EncodedChar {
                    usage: 0x04,
                    shift: true
                },
                EncodedChar {
                    usage: 0x1E,
                    shift: true
                },
            ]
        );
    }

    #[test]
    fn empty_text() {
        let enc = encode_ascii_text("");
        assert!(enc.chars.is_empty());
        assert!(enc.unencodable_positions.is_empty());
        assert_eq!(enc.char_count, 0);
    }
}
