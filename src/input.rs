use anyhow::{Context, Result, anyhow};
use enigo::{Axis, Button, Coordinate, Direction, Enigo, Key, Keyboard, Mouse, Settings};
use serde::Deserialize;
use std::{
    borrow::Cow,
    sync::mpsc::{self, Sender},
    thread,
};

use crate::model::MonitorInfo;

const MAX_TEXT_INPUT_CHARS: usize = 512;
const MAX_SHORTCUT_KEYS: usize = 4;

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
    #[serde(rename = "b")]
    B,
    #[serde(rename = "c")]
    C,
    #[serde(rename = "d")]
    D,
    #[serde(rename = "e")]
    E,
    #[serde(rename = "f")]
    F,
    #[serde(rename = "g")]
    G,
    #[serde(rename = "h")]
    H,
    #[serde(rename = "i")]
    I,
    #[serde(rename = "j")]
    J,
    #[serde(rename = "k")]
    K,
    #[serde(rename = "l")]
    L,
    #[serde(rename = "m")]
    M,
    #[serde(rename = "n")]
    N,
    #[serde(rename = "o")]
    O,
    #[serde(rename = "p")]
    P,
    #[serde(rename = "q")]
    Q,
    #[serde(rename = "r")]
    R,
    #[serde(rename = "s")]
    S,
    #[serde(rename = "t")]
    T,
    #[serde(rename = "u")]
    U,
    #[serde(rename = "v")]
    V,
    #[serde(rename = "w")]
    W,
    #[serde(rename = "x")]
    X,
    #[serde(rename = "y")]
    Y,
    #[serde(rename = "z")]
    Z,
    #[serde(rename = "f1")]
    F1,
    #[serde(rename = "f2")]
    F2,
    #[serde(rename = "f3")]
    F3,
    #[serde(rename = "f5")]
    F5,
    #[serde(rename = "f6")]
    F6,
    #[serde(rename = "f7")]
    F7,
    #[serde(rename = "f8")]
    F8,
    #[serde(rename = "f9")]
    F9,
    #[serde(rename = "f10")]
    F10,
    #[serde(rename = "f11")]
    F11,
    #[serde(rename = "f12")]
    F12,
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

pub fn command_from_request(
    request: InputRequest,
    monitor: Option<&MonitorInfo>,
) -> Result<InputCommand> {
    Ok(match request {
        InputRequest::Move { x, y } => InputCommand::Move {
            point: normalize_request_point(monitor, x, y)?,
        },
        InputRequest::Click {
            x,
            y,
            button,
            count,
        } => InputCommand::Click {
            point: normalize_request_point(monitor, x, y)?,
            button,
            count: count.max(1),
        },
        InputRequest::Button {
            x,
            y,
            button,
            action,
        } => InputCommand::Button {
            point: normalize_request_point(monitor, x, y)?,
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
        InputRequest::Text { text } => {
            if text.chars().count() > MAX_TEXT_INPUT_CHARS {
                return Err(anyhow!(
                    "text input is limited to {MAX_TEXT_INPUT_CHARS} characters per request"
                ));
            }
            InputCommand::Text { text }
        }
        InputRequest::Key { key, action } => InputCommand::Key { key, action },
        InputRequest::Shortcut { keys } => {
            if keys.is_empty() {
                return Err(anyhow!("shortcut requests must include at least one key"));
            }
            if keys.len() > MAX_SHORTCUT_KEYS {
                return Err(anyhow!(
                    "shortcut requests are limited to {MAX_SHORTCUT_KEYS} keys"
                ));
            }
            InputCommand::Shortcut { keys }
        }
    })
}

fn normalize_request_point(monitor: Option<&MonitorInfo>, x: f32, y: f32) -> Result<ScreenPoint> {
    let monitor = monitor.ok_or_else(|| anyhow!("pointer input requires an active monitor"))?;
    normalize_point(monitor, x, y)
}

fn execute_command(enigo: &mut Enigo, command: InputCommand) -> Result<()> {
    match command {
        InputCommand::Move { point } => move_mouse_absolute(enigo, point),
        InputCommand::Click {
            point,
            button,
            count,
        } => {
            move_mouse_absolute(enigo, point)?;
            for _ in 0..count {
                mouse_button(enigo, button, Direction::Click)?;
            }
            Ok(())
        }
        InputCommand::Button {
            point,
            button,
            action,
        } => {
            move_mouse_absolute(enigo, point)?;
            mouse_button(enigo, button, to_enigo_direction(action))
        }
        InputCommand::Scroll {
            horizontal,
            vertical,
        } => {
            if horizontal != 0 {
                mouse_scroll(enigo, horizontal, Axis::Horizontal)?;
            }

            if vertical != 0 {
                mouse_scroll(enigo, vertical, Axis::Vertical)?;
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
            _ => enigo
                .key(Key::Unicode(character), Direction::Click)
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

fn move_mouse_absolute(enigo: &mut Enigo, point: ScreenPoint) -> Result<()> {
    enigo
        .move_mouse(point.x, point.y, Coordinate::Abs)
        .context("failed to move the mouse cursor")
}

fn mouse_button(enigo: &mut Enigo, button: RemoteMouseButton, direction: Direction) -> Result<()> {
    enigo
        .button(to_enigo_button(button), direction)
        .context("failed to inject mouse button input")
}

fn mouse_scroll(enigo: &mut Enigo, amount: i32, axis: Axis) -> Result<()> {
    enigo
        .scroll(amount, axis)
        .context("failed to inject mouse wheel input")
}

fn normalize_text_for_injection(text: &str) -> Cow<'_, str> {
    if text.contains('\r') {
        Cow::Owned(text.replace("\r\n", "\n").replace('\r', "\n"))
    } else {
        Cow::Borrowed(text)
    }
}

fn to_enigo_direction(action: impl IntoEnigoDirection) -> Direction {
    action.into_direction()
}

fn to_enigo_button(button: RemoteMouseButton) -> Button {
    match button {
        RemoteMouseButton::Left => Button::Left,
        RemoteMouseButton::Right => Button::Right,
        RemoteMouseButton::Middle => Button::Middle,
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
        RemoteKey::A => Key::Unicode('a'),
        RemoteKey::B => Key::Unicode('b'),
        RemoteKey::C => Key::Unicode('c'),
        RemoteKey::D => Key::Unicode('d'),
        RemoteKey::E => Key::Unicode('e'),
        RemoteKey::F => Key::Unicode('f'),
        RemoteKey::G => Key::Unicode('g'),
        RemoteKey::H => Key::Unicode('h'),
        RemoteKey::I => Key::Unicode('i'),
        RemoteKey::J => Key::Unicode('j'),
        RemoteKey::K => Key::Unicode('k'),
        RemoteKey::L => Key::Unicode('l'),
        RemoteKey::M => Key::Unicode('m'),
        RemoteKey::N => Key::Unicode('n'),
        RemoteKey::O => Key::Unicode('o'),
        RemoteKey::P => Key::Unicode('p'),
        RemoteKey::Q => Key::Unicode('q'),
        RemoteKey::R => Key::Unicode('r'),
        RemoteKey::S => Key::Unicode('s'),
        RemoteKey::T => Key::Unicode('t'),
        RemoteKey::U => Key::Unicode('u'),
        RemoteKey::V => Key::Unicode('v'),
        RemoteKey::W => Key::Unicode('w'),
        RemoteKey::X => Key::Unicode('x'),
        RemoteKey::Y => Key::Unicode('y'),
        RemoteKey::Z => Key::Unicode('z'),
        RemoteKey::F1 => Key::F1,
        RemoteKey::F2 => Key::F2,
        RemoteKey::F3 => Key::F3,
        RemoteKey::F4 => Key::F4,
        RemoteKey::F5 => Key::F5,
        RemoteKey::F6 => Key::F6,
        RemoteKey::F7 => Key::F7,
        RemoteKey::F8 => Key::F8,
        RemoteKey::F9 => Key::F9,
        RemoteKey::F10 => Key::F10,
        RemoteKey::F11 => Key::F11,
        RemoteKey::F12 => Key::F12,
    }
}

trait IntoEnigoDirection {
    fn into_direction(self) -> Direction;
}

impl IntoEnigoDirection for KeyAction {
    fn into_direction(self) -> Direction {
        match self {
            KeyAction::Press => Direction::Press,
            KeyAction::Release => Direction::Release,
            KeyAction::Click => Direction::Click,
        }
    }
}

impl IntoEnigoDirection for ButtonAction {
    fn into_direction(self) -> Direction {
        match self {
            ButtonAction::Press => Direction::Press,
            ButtonAction::Release => Direction::Release,
        }
    }
}

fn default_click_count() -> u8 {
    1
}

#[cfg(test)]
mod tests {
    use super::{
        InputCommand, InputRequest, command_from_request, normalize_point,
        normalize_text_for_injection,
    };
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

    #[test]
    fn text_requests_do_not_require_a_monitor() {
        let command = command_from_request(
            InputRequest::Text {
                text: "hello".to_string(),
            },
            None,
        )
        .unwrap();

        assert!(matches!(
            command,
            InputCommand::Text { text } if text == "hello"
        ));
    }

    #[test]
    fn pointer_requests_require_a_monitor() {
        let error = command_from_request(InputRequest::Move { x: 0.5, y: 0.5 }, None)
            .unwrap_err()
            .to_string();

        assert_eq!(error, "pointer input requires an active monitor");
    }
}
