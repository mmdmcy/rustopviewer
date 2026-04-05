use anyhow::{Context, Result, anyhow};
use enigo::{Direction, Enigo, Key, Keyboard, Settings};
use serde::Deserialize;
use std::{
    borrow::Cow,
    mem::size_of,
    sync::mpsc::{self, Sender},
    thread,
};
use windows::Win32::UI::{
    Input::KeyboardAndMouse::{
        INPUT, INPUT_0, INPUT_KEYBOARD, INPUT_MOUSE, KEYBD_EVENT_FLAGS, KEYBDINPUT,
        KEYEVENTF_KEYUP, KEYEVENTF_UNICODE, MOUSE_EVENT_FLAGS, MOUSEEVENTF_ABSOLUTE,
        MOUSEEVENTF_HWHEEL, MOUSEEVENTF_LEFTDOWN, MOUSEEVENTF_LEFTUP, MOUSEEVENTF_MIDDLEDOWN,
        MOUSEEVENTF_MIDDLEUP, MOUSEEVENTF_MOVE, MOUSEEVENTF_RIGHTDOWN, MOUSEEVENTF_RIGHTUP,
        MOUSEEVENTF_VIRTUALDESK, MOUSEEVENTF_WHEEL, MOUSEINPUT, SendInput, VIRTUAL_KEY,
    },
    WindowsAndMessaging::{
        GetSystemMetrics, SM_CXVIRTUALSCREEN, SM_CYVIRTUALSCREEN, SM_XVIRTUALSCREEN,
        SM_YVIRTUALSCREEN, WHEEL_DELTA,
    },
};

use crate::model::MonitorInfo;

#[derive(Debug, Clone, Copy)]
pub struct ScreenPoint {
    pub x: i32,
    pub y: i32,
}

#[derive(Debug, Clone)]
pub enum InputCommand {
    Move {
        point: ScreenPoint,
    },
    Click {
        point: ScreenPoint,
        button: RemoteMouseButton,
        count: u8,
    },
    Button {
        point: ScreenPoint,
        button: RemoteMouseButton,
        action: ButtonAction,
    },
    Scroll {
        horizontal: i32,
        vertical: i32,
    },
    Text {
        text: String,
    },
    Key {
        key: RemoteKey,
        action: KeyAction,
    },
    Shortcut {
        keys: Vec<RemoteKey>,
    },
}

#[derive(Debug, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum InputRequest {
    Move {
        x: f32,
        y: f32,
    },
    Click {
        x: f32,
        y: f32,
        button: RemoteMouseButton,
        #[serde(default = "default_click_count")]
        count: u8,
    },
    Button {
        x: f32,
        y: f32,
        button: RemoteMouseButton,
        action: ButtonAction,
    },
    Scroll {
        #[serde(default)]
        horizontal: i32,
        #[serde(default)]
        vertical: i32,
    },
    Text {
        text: String,
    },
    Key {
        key: RemoteKey,
        #[serde(default)]
        action: KeyAction,
    },
    Shortcut {
        keys: Vec<RemoteKey>,
    },
}

#[derive(Debug, Clone, Copy, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RemoteMouseButton {
    Left,
    Right,
    Middle,
}

#[derive(Debug, Clone, Copy, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ButtonAction {
    Press,
    Release,
}

#[derive(Debug, Clone, Copy, Default, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum KeyAction {
    Press,
    Release,
    #[default]
    Click,
}

#[derive(Debug, Clone, Copy, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RemoteKey {
    Enter,
    Escape,
    Tab,
    Backspace,
    Space,
    Delete,
    Home,
    End,
    PageUp,
    PageDown,
    UpArrow,
    DownArrow,
    LeftArrow,
    RightArrow,
    Control,
    Shift,
    Alt,
    #[serde(alias = "windows")]
    Meta,
    #[serde(rename = "a")]
    A,
    #[serde(rename = "c")]
    C,
    #[serde(rename = "d")]
    D,
    #[serde(rename = "l")]
    L,
    #[serde(rename = "r")]
    R,
    #[serde(rename = "v")]
    V,
    #[serde(rename = "x")]
    X,
    #[serde(rename = "f4")]
    F4,
}

pub fn spawn_input_worker() -> Result<Sender<InputCommand>> {
    let (tx, rx) = mpsc::channel::<InputCommand>();

    thread::spawn(move || {
        let mut enigo = match Enigo::new(&Settings::default()) {
            Ok(enigo) => enigo,
            Err(err) => {
                tracing::error!(error = %err, "Failed to initialize the input injector");
                return;
            }
        };

        while let Ok(command) = rx.recv() {
            if let Err(err) = execute_command(&mut enigo, command) {
                tracing::warn!(error = %err, "Failed to execute an input command");
            }
        }
    });

    Ok(tx)
}

pub fn command_from_request(request: InputRequest, monitor: &MonitorInfo) -> Result<InputCommand> {
    Ok(match request {
        InputRequest::Move { x, y } => InputCommand::Move {
            point: normalize_point(monitor, x, y)?,
        },
        InputRequest::Click {
            x,
            y,
            button,
            count,
        } => InputCommand::Click {
            point: normalize_point(monitor, x, y)?,
            button,
            count: count.max(1),
        },
        InputRequest::Button {
            x,
            y,
            button,
            action,
        } => InputCommand::Button {
            point: normalize_point(monitor, x, y)?,
            button,
            action,
        },
        InputRequest::Scroll {
            horizontal,
            vertical,
        } => InputCommand::Scroll {
            horizontal,
            vertical,
        },
        InputRequest::Text { text } => InputCommand::Text { text },
        InputRequest::Key { key, action } => InputCommand::Key { key, action },
        InputRequest::Shortcut { keys } => InputCommand::Shortcut { keys },
    })
}

fn execute_command(enigo: &mut Enigo, command: InputCommand) -> Result<()> {
    match command {
        InputCommand::Move { point } => move_mouse_absolute(point),
        InputCommand::Click {
            point,
            button,
            count,
        } => {
            move_mouse_absolute(point)?;
            for _ in 0..count {
                mouse_button(button, true)?;
                mouse_button(button, false)?;
            }
            Ok(())
        }
        InputCommand::Button {
            point,
            button,
            action,
        } => {
            move_mouse_absolute(point)?;
            mouse_button(button, matches!(action, ButtonAction::Press))
        }
        InputCommand::Scroll {
            horizontal,
            vertical,
        } => {
            if horizontal != 0 {
                mouse_scroll(horizontal, false)?;
            }

            if vertical != 0 {
                mouse_scroll(vertical, true)?;
            }

            Ok(())
        }
        InputCommand::Text { text } => inject_text(enigo, &text),
        InputCommand::Key { key, action } => {
            enigo
                .key(to_enigo_key(key), to_enigo_direction(action))
                .context("failed to inject keyboard input")?;
            Ok(())
        }
        InputCommand::Shortcut { keys } => run_shortcut(enigo, &keys),
    }
}

fn inject_text(enigo: &mut Enigo, text: &str) -> Result<()> {
    let normalized = normalize_text_for_injection(text);

    for character in normalized.chars() {
        match character {
            '\n' => enigo
                .key(Key::Return, Direction::Click)
                .context("failed to inject a line break")?,
            '\t' => enigo
                .key(Key::Tab, Direction::Click)
                .context("failed to inject a tab")?,
            '\0' => return Err(anyhow!("text input contained a null byte")),
            _ => send_unicode_char(character)
                .with_context(|| format!("failed to inject character {character:?}"))?,
        }
    }

    Ok(())
}

fn run_shortcut(enigo: &mut Enigo, keys: &[RemoteKey]) -> Result<()> {
    let mut pressed = Vec::with_capacity(keys.len());

    for key in keys {
        let keycode = to_enigo_key(*key);
        enigo
            .key(keycode, Direction::Press)
            .with_context(|| format!("failed to press shortcut key {key:?}"))?;
        pressed.push(keycode);
    }

    for keycode in pressed.into_iter().rev() {
        enigo
            .key(keycode, Direction::Release)
            .context("failed to release a shortcut key")?;
    }

    Ok(())
}

fn normalize_point(monitor: &MonitorInfo, x: f32, y: f32) -> Result<ScreenPoint> {
    if !x.is_finite() || !y.is_finite() {
        return Err(anyhow!("pointer coordinates must be finite values"));
    }

    let clamped_x = x.clamp(0.0, 1.0);
    let clamped_y = y.clamp(0.0, 1.0);

    Ok(ScreenPoint {
        x: monitor.x + ((monitor.width.saturating_sub(1) as f32) * clamped_x).round() as i32,
        y: monitor.y + ((monitor.height.saturating_sub(1) as f32) * clamped_y).round() as i32,
    })
}

fn move_mouse_absolute(point: ScreenPoint) -> Result<()> {
    let (x, y) = map_to_virtual_desktop(point)?;
    send_mouse(
        MOUSEEVENTF_MOVE | MOUSEEVENTF_ABSOLUTE | MOUSEEVENTF_VIRTUALDESK,
        x,
        y,
        0,
    )
}

fn mouse_button(button: RemoteMouseButton, pressed: bool) -> Result<()> {
    let flags = match (button, pressed) {
        (RemoteMouseButton::Left, true) => MOUSEEVENTF_LEFTDOWN,
        (RemoteMouseButton::Left, false) => MOUSEEVENTF_LEFTUP,
        (RemoteMouseButton::Right, true) => MOUSEEVENTF_RIGHTDOWN,
        (RemoteMouseButton::Right, false) => MOUSEEVENTF_RIGHTUP,
        (RemoteMouseButton::Middle, true) => MOUSEEVENTF_MIDDLEDOWN,
        (RemoteMouseButton::Middle, false) => MOUSEEVENTF_MIDDLEUP,
    };

    send_mouse(flags, 0, 0, 0)
}

fn mouse_scroll(amount: i32, vertical: bool) -> Result<()> {
    let flags = if vertical {
        MOUSEEVENTF_WHEEL
    } else {
        MOUSEEVENTF_HWHEEL
    };

    let scaled_amount = if vertical {
        amount.saturating_mul(-(WHEEL_DELTA as i32))
    } else {
        amount.saturating_mul(WHEEL_DELTA as i32)
    };

    send_mouse(flags, 0, 0, scaled_amount)
}

fn normalize_text_for_injection(text: &str) -> Cow<'_, str> {
    if text.contains('\r') {
        Cow::Owned(text.replace("\r\n", "\n").replace('\r', "\n"))
    } else {
        Cow::Borrowed(text)
    }
}

fn send_unicode_char(character: char) -> Result<()> {
    let mut buffer = [0; 2];

    for &utf16_unit in character.encode_utf16(&mut buffer).iter() {
        let inputs = [
            keyboard_input(KEYEVENTF_UNICODE, VIRTUAL_KEY(0), utf16_unit),
            keyboard_input(
                KEYEVENTF_UNICODE | KEYEVENTF_KEYUP,
                VIRTUAL_KEY(0),
                utf16_unit,
            ),
        ];

        send_inputs(&inputs)?;
    }

    Ok(())
}

fn keyboard_input(flags: KEYBD_EVENT_FLAGS, vk: VIRTUAL_KEY, scan: u16) -> INPUT {
    INPUT {
        r#type: INPUT_KEYBOARD,
        Anonymous: INPUT_0 {
            ki: KEYBDINPUT {
                wVk: vk,
                wScan: scan,
                dwFlags: flags,
                time: 0,
                dwExtraInfo: 0,
            },
        },
    }
}

fn send_mouse(flags: MOUSE_EVENT_FLAGS, dx: i32, dy: i32, mouse_data: i32) -> Result<()> {
    let input = INPUT {
        r#type: INPUT_MOUSE,
        Anonymous: INPUT_0 {
            mi: MOUSEINPUT {
                dx,
                dy,
                mouseData: mouse_data as u32,
                dwFlags: flags,
                time: 0,
                dwExtraInfo: 0,
            },
        },
    };

    send_inputs(&[input])
}

fn send_inputs(inputs: &[INPUT]) -> Result<()> {
    let expected = u32::try_from(inputs.len()).context("too many input events to inject")?;
    let sent = unsafe { SendInput(inputs, size_of::<INPUT>() as i32) };

    if sent == expected {
        Ok(())
    } else {
        Err(anyhow!("SendInput sent {sent} of {expected} events"))
    }
}

fn map_to_virtual_desktop(point: ScreenPoint) -> Result<(i32, i32)> {
    let left = unsafe { GetSystemMetrics(SM_XVIRTUALSCREEN) };
    let top = unsafe { GetSystemMetrics(SM_YVIRTUALSCREEN) };
    let width = unsafe { GetSystemMetrics(SM_CXVIRTUALSCREEN) };
    let height = unsafe { GetSystemMetrics(SM_CYVIRTUALSCREEN) };

    if width <= 1 || height <= 1 {
        return Err(anyhow!("failed to determine the virtual desktop bounds"));
    }

    let max_x = i64::from(width - 1);
    let max_y = i64::from(height - 1);
    let relative_x = i64::from((point.x - left).clamp(0, width - 1));
    let relative_y = i64::from((point.y - top).clamp(0, height - 1));

    let mapped_x = ((relative_x * 65535) + (max_x / 2)) / max_x;
    let mapped_y = ((relative_y * 65535) + (max_y / 2)) / max_y;

    Ok((mapped_x as i32, mapped_y as i32))
}

fn to_enigo_direction(action: KeyAction) -> Direction {
    match action {
        KeyAction::Press => Direction::Press,
        KeyAction::Release => Direction::Release,
        KeyAction::Click => Direction::Click,
    }
}

fn to_enigo_key(key: RemoteKey) -> Key {
    match key {
        RemoteKey::Enter => Key::Return,
        RemoteKey::Escape => Key::Escape,
        RemoteKey::Tab => Key::Tab,
        RemoteKey::Backspace => Key::Backspace,
        RemoteKey::Space => Key::Space,
        RemoteKey::Delete => Key::Delete,
        RemoteKey::Home => Key::Home,
        RemoteKey::End => Key::End,
        RemoteKey::PageUp => Key::PageUp,
        RemoteKey::PageDown => Key::PageDown,
        RemoteKey::UpArrow => Key::UpArrow,
        RemoteKey::DownArrow => Key::DownArrow,
        RemoteKey::LeftArrow => Key::LeftArrow,
        RemoteKey::RightArrow => Key::RightArrow,
        RemoteKey::Control => Key::Control,
        RemoteKey::Shift => Key::Shift,
        RemoteKey::Alt => Key::Alt,
        RemoteKey::Meta => Key::Meta,
        RemoteKey::A => Key::A,
        RemoteKey::C => Key::C,
        RemoteKey::D => Key::D,
        RemoteKey::L => Key::L,
        RemoteKey::R => Key::R,
        RemoteKey::V => Key::V,
        RemoteKey::X => Key::X,
        RemoteKey::F4 => Key::F4,
    }
}

fn default_click_count() -> u8 {
    1
}

#[cfg(test)]
mod tests {
    use super::{normalize_point, normalize_text_for_injection};
    use crate::model::MonitorInfo;

    #[test]
    fn normalized_pointer_coordinates_map_into_monitor_bounds() {
        let monitor = MonitorInfo {
            id: 1,
            name: "Test".to_string(),
            x: 100,
            y: 200,
            width: 1600,
            height: 900,
            is_primary: true,
        };

        let top_left = normalize_point(&monitor, 0.0, 0.0).unwrap();
        let bottom_right = normalize_point(&monitor, 1.0, 1.0).unwrap();

        assert_eq!(top_left.x, 100);
        assert_eq!(top_left.y, 200);
        assert_eq!(bottom_right.x, 1699);
        assert_eq!(bottom_right.y, 1099);
    }

    #[test]
    fn carriage_returns_are_normalized_before_text_injection() {
        let normalized = normalize_text_for_injection("hello\r\nworld\rfrom\nrov");

        assert_eq!(normalized, "hello\nworld\nfrom\nrov");
    }
}
