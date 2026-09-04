//! HID 报告构建与键盘映射（纯逻辑，无平台依赖）。
//!
//! - 键盘：8 字节报告（modifier + reserved + 6 键），KeyDown/KeyUp 必须成对；
//! - 鼠标：相对移动报告，每条 dx/dy 不超过 int8 范围；
//! - 键位映射使用显式「物理键 → HID usage」表，不依赖 Windows 本地化显示名。

use serde::{Deserialize, Serialize};

// ---------------------------------------------------------------------------
// 修饰键位
// ---------------------------------------------------------------------------

pub const MOD_CTRL: u8 = 0x01;
pub const MOD_SHIFT: u8 = 0x02;
pub const MOD_ALT: u8 = 0x04;
pub const MOD_META: u8 = 0x08;

/// 鼠标按键位。
pub const BTN_LEFT: u8 = 0x01;
pub const BTN_RIGHT: u8 = 0x02;
pub const BTN_MIDDLE: u8 = 0x04;

// ---------------------------------------------------------------------------
// 报告结构
// ---------------------------------------------------------------------------

/// HID 键盘报告（boot keyboard：modifier + reserved + 6 个按键）。
///
/// Report Map 声明键盘输入报告为 8 字节（修饰键 + 1 个保留字节 + 6 个按键）。
/// 发送 7 字节会被 iOS 当作非法报告长度直接丢弃，表现为“蓝牙已连接但键盘
/// 无响应”。reserved 必须保持 0。
#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct KeyboardReport {
    pub modifiers: u8,
    /// 保留字节（USB 键盘布局第 2 字节），恒为 0。
    pub reserved: u8,
    pub keys: [u8; 6],
}

/// HID 相对鼠标报告。
#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct MouseReport {
    pub buttons: u8,
    pub dx: i8,
    pub dy: i8,
    pub wheel: i8,
}

/// 鼠标键位事件（前端抽象）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PointerButton {
    pub button: u8,
    pub pressed: bool,
}

impl Default for KeyboardReport {
    fn default() -> Self {
        Self {
            modifiers: 0,
            reserved: 0,
            keys: [0; 6],
        }
    }
}

impl Default for MouseReport {
    fn default() -> Self {
        Self {
            buttons: 0,
            dx: 0,
            dy: 0,
            wheel: 0,
        }
    }
}

pub fn build_keyboard_report(modifiers: u8, keys: [u8; 6]) -> KeyboardReport {
    KeyboardReport {
        modifiers,
        reserved: 0,
        keys,
    }
}

/// 按住单个键（usage）的按下/释放报告序列 —— 保证 KeyDown/KeyUp 配对。
pub fn key_down_up(usage: u8, modifiers: u8) -> [KeyboardReport; 2] {
    [
        KeyboardReport {
            modifiers,
            reserved: 0,
            keys: [usage, 0, 0, 0, 0, 0],
        },
        KeyboardReport {
            modifiers,
            reserved: 0,
            keys: [0; 6],
        },
    ]
}

pub fn build_mouse_report(buttons: u8, dx: i8, dy: i8, wheel: i8) -> MouseReport {
    MouseReport {
        buttons,
        dx,
        dy,
        wheel,
    }
}

/// int8 位移钳制（前端已按 HID_MAX_DELTA 拆分，这里做最后防线）。
pub fn clamp_delta(v: i32) -> i8 {
    v.clamp(-127, 127) as i8
}

// ---------------------------------------------------------------------------
// 键位映射（HID Usage, Keyboard/Keypad Page 0x07）
// ---------------------------------------------------------------------------

/// 字符 → (usage, 是否需要 Shift)。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HidChar {
    pub usage: u8,
    pub shift: bool,
}

/// 单字符编码结果。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EncodedChar {
    pub usage: u8,
    pub shift: bool,
}

const fn usage_for_letter(c: char) -> Option<u8> {
    match c {
        'a' => Some(0x04),
        'b' => Some(0x05),
        'c' => Some(0x06),
        'd' => Some(0x07),
        'e' => Some(0x08),
        'f' => Some(0x09),
        'g' => Some(0x0A),
        'h' => Some(0x0B),
        'i' => Some(0x0C),
        'j' => Some(0x0D),
        'k' => Some(0x0E),
        'l' => Some(0x0F),
        'm' => Some(0x10),
        'n' => Some(0x11),
        'o' => Some(0x12),
        'p' => Some(0x13),
        'q' => Some(0x14),
        'r' => Some(0x15),
        's' => Some(0x16),
        't' => Some(0x17),
        'u' => Some(0x18),
        'v' => Some(0x19),
        'w' => Some(0x1A),
        'x' => Some(0x1B),
        'y' => Some(0x1C),
        'z' => Some(0x1D),
        _ => None,
    }
}

const fn usage_for_digit(c: char) -> Option<u8> {
    match c {
        '1' => Some(0x1E),
        '2' => Some(0x1F),
        '3' => Some(0x20),
        '4' => Some(0x21),
        '5' => Some(0x22),
        '6' => Some(0x23),
        '7' => Some(0x24),
        '8' => Some(0x25),
        '9' => Some(0x26),
        '0' => Some(0x27),
        _ => None,
    }
}

/// 常用标点（含 Shift 组合）。
pub fn hid_char_for(c: char) -> Option<HidChar> {
    let plain = |usage| {
        Some(HidChar {
            usage,
            shift: false,
        })
    };
    let shifted = |usage| Some(HidChar { usage, shift: true });
    match c {
        'a'..='z' => plain(usage_for_letter(c)?),
        'A'..='Z' => shifted(usage_for_letter(c.to_ascii_lowercase())?),
        '0'..='9' => plain(usage_for_digit(c)?),
        ' ' => plain(0x2C),
        '-' => plain(0x2D),
        '=' => plain(0x2E),
        '[' => plain(0x2F),
        ']' => plain(0x30),
        '\\' => plain(0x31),
        ';' => plain(0x33),
        '\'' => plain(0x34),
        '`' => plain(0x35),
        ',' => plain(0x36),
        '.' => plain(0x37),
        '/' => plain(0x38),
        '!' => shifted(0x1E),
        '@' => shifted(0x1F),
        '#' => shifted(0x20),
        '$' => shifted(0x21),
        '%' => shifted(0x22),
        '^' => shifted(0x23),
        '&' => shifted(0x24),
        '*' => shifted(0x25),
        '(' => shifted(0x26),
        ')' => shifted(0x27),
        '_' => shifted(0x2D),
        '+' => shifted(0x2E),
        '{' => shifted(0x2F),
        '}' => shifted(0x30),
        '|' => shifted(0x31),
        ':' => shifted(0x33),
        '"' => shifted(0x34),
        '~' => shifted(0x35),
        '<' => shifted(0x36),
        '>' => shifted(0x37),
        '?' => shifted(0x38),
        '\n' | '\r' => plain(0x28), // Enter
        '\t' => plain(0x2B),
        _ => None,
    }
}

/// 物理键 code → usage（DOM KeyboardEvent.code 约定）。
pub fn usage_for_code(code: &str) -> Option<u8> {
    if let Some(rest) = code.strip_prefix("Key") {
        if rest.len() == 1 {
            return usage_for_letter(rest.chars().next()?.to_ascii_lowercase());
        }
        return None;
    }
    if let Some(rest) = code.strip_prefix("Digit") {
        if rest.len() == 1 {
            return usage_for_digit(rest.chars().next()?);
        }
        return None;
    }
    if let Some(n) = code.strip_prefix("F") {
        if let Ok(n) = n.parse::<u8>() {
            if (1..=12).contains(&n) {
                return Some(0x3A + n - 1);
            }
        }
        return None;
    }
    let simple = match code {
        "Enter" => 0x28,
        "Escape" => 0x29,
        "Backspace" => 0x2A,
        "Tab" => 0x2B,
        "Space" => 0x2C,
        "CapsLock" => 0x39,
        "Insert" => 0x49,
        "Delete" => 0x4C,
        "Home" => 0x4A,
        "End" => 0x4D,
        "PageUp" => 0x4B,
        "PageDown" => 0x4E,
        "ArrowUp" => 0x52,
        "ArrowDown" => 0x51,
        "ArrowRight" => 0x4F,
        "ArrowLeft" => 0x50,
        "Numpad0" => 0x62,
        "Numpad1" => 0x59,
        "Numpad2" => 0x5A,
        "Numpad3" => 0x5B,
        "Numpad4" => 0x5C,
        "Numpad5" => 0x5D,
        "Numpad6" => 0x5E,
        "Numpad7" => 0x5F,
        "Numpad8" => 0x60,
        "Numpad9" => 0x61,
        "NumpadEnter" => 0x58,
        "NumpadAdd" => 0x57,
        "NumpadSubtract" => 0x56,
        "NumpadMultiply" => 0x55,
        "NumpadDivide" => 0x54,
        "NumpadDecimal" => 0x63,
        _ => return None,
    };
    Some(simple)
}

/// 修饰键 code → modifier 位。
pub fn modifier_bit_for_code(code: &str) -> Option<u8> {
    match code {
        "ControlLeft" | "ControlRight" => Some(MOD_CTRL),
        "ShiftLeft" | "ShiftRight" => Some(MOD_SHIFT),
        "AltLeft" | "AltRight" => Some(MOD_ALT),
        "MetaLeft" | "MetaRight" => Some(MOD_META),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn letters_and_digits() {
        assert_eq!(
            hid_char_for('a'),
            Some(HidChar {
                usage: 0x04,
                shift: false
            })
        );
        assert_eq!(
            hid_char_for('A'),
            Some(HidChar {
                usage: 0x04,
                shift: true
            })
        );
        assert_eq!(
            hid_char_for('9'),
            Some(HidChar {
                usage: 0x26,
                shift: false
            })
        );
        assert_eq!(
            hid_char_for('0'),
            Some(HidChar {
                usage: 0x27,
                shift: false
            })
        );
    }

    #[test]
    fn punctuation_with_shift() {
        assert_eq!(
            hid_char_for('!'),
            Some(HidChar {
                usage: 0x1E,
                shift: true
            })
        );
        assert_eq!(
            hid_char_for('@'),
            Some(HidChar {
                usage: 0x1F,
                shift: true
            })
        );
        assert_eq!(
            hid_char_for('_'),
            Some(HidChar {
                usage: 0x2D,
                shift: true
            })
        );
        assert_eq!(
            hid_char_for('?'),
            Some(HidChar {
                usage: 0x38,
                shift: true
            })
        );
        assert_eq!(
            hid_char_for('.'),
            Some(HidChar {
                usage: 0x37,
                shift: false
            })
        );
    }

    #[test]
    fn newline_and_tab() {
        assert_eq!(
            hid_char_for('\n'),
            Some(HidChar {
                usage: 0x28,
                shift: false
            })
        );
        assert_eq!(
            hid_char_for('\r'),
            Some(HidChar {
                usage: 0x28,
                shift: false
            })
        );
        assert_eq!(
            hid_char_for('\t'),
            Some(HidChar {
                usage: 0x2B,
                shift: false
            })
        );
    }

    #[test]
    fn unencodable_chars() {
        assert_eq!(hid_char_for('é'), None);
        assert_eq!(hid_char_for('中'), None);
        assert_eq!(hid_char_for('😀'), None);
    }

    #[test]
    fn dom_codes_map_to_usages() {
        assert_eq!(usage_for_code("KeyA"), Some(0x04));
        assert_eq!(usage_for_code("Digit7"), Some(0x24));
        assert_eq!(usage_for_code("Enter"), Some(0x28));
        assert_eq!(usage_for_code("ArrowUp"), Some(0x52));
        assert_eq!(usage_for_code("F12"), Some(0x45));
        assert_eq!(usage_for_code("F13"), None);
        assert_eq!(usage_for_code("KeyAB"), None);
        assert_eq!(usage_for_code("Unidentified"), None);
    }

    #[test]
    fn modifier_bits() {
        assert_eq!(modifier_bit_for_code("ControlLeft"), Some(MOD_CTRL));
        assert_eq!(modifier_bit_for_code("ShiftRight"), Some(MOD_SHIFT));
        assert_eq!(modifier_bit_for_code("MetaLeft"), Some(MOD_META));
        assert_eq!(modifier_bit_for_code("KeyA"), None);
    }

    #[test]
    fn key_down_up_pairing() {
        let reports = key_down_up(0x04, MOD_SHIFT);
        assert_eq!(reports[0].keys[0], 0x04);
        assert_eq!(reports[0].modifiers, MOD_SHIFT);
        assert_eq!(reports[1].keys, [0; 6]);
        assert_eq!(reports[1].modifiers, MOD_SHIFT);
    }

    #[test]
    fn mouse_delta_clamp() {
        assert_eq!(clamp_delta(200), 127);
        assert_eq!(clamp_delta(-200), -127);
        assert_eq!(clamp_delta(42), 42);
    }

    #[test]
    fn mouse_report_bytes() {
        let r = build_mouse_report(BTN_LEFT, 10, -5, 3);
        assert_eq!(r.buttons, BTN_LEFT);
        assert_eq!(r.dx, 10);
        assert_eq!(r.dy, -5);
        assert_eq!(r.wheel, 3);
    }
}
