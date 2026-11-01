//! Maps key presses to actions. The only place that knows the bindings.

use crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyModifiers};

use super::app::Mode;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action {
    Quit,
    Down,
    Up,
    Top,
    Bottom,
    SwitchPane,
    StartFilter,
    FilterChar(char),
    FilterBackspace,
    FilterApply,
    FilterCancel,
    Clear,
    TogglePause,
    ToggleHelp,
    CloseHelp,
    Break(Break),
    ClearScenarios,
}

/// A scenario key.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Break {
    Status500,
    Status401,
    Status404,
    Empty,
    Delay,
    Offline,
}

pub fn action(mode: Mode, key: KeyEvent) -> Option<Action> {
    if key.kind == KeyEventKind::Release {
        return None;
    }
    if key.modifiers.contains(KeyModifiers::CONTROL) {
        return (key.code == KeyCode::Char('c')).then_some(Action::Quit);
    }
    match mode {
        Mode::Normal => normal(key.code),
        Mode::Filter => filter(key.code),
        Mode::Help => matches!(
            key.code,
            KeyCode::Esc | KeyCode::Char('?' | 'q') | KeyCode::Enter
        )
        .then_some(Action::CloseHelp),
    }
}

fn normal(code: KeyCode) -> Option<Action> {
    Some(match code {
        KeyCode::Char('q') => Action::Quit,
        KeyCode::Char('j') | KeyCode::Down => Action::Down,
        KeyCode::Char('k') | KeyCode::Up => Action::Up,
        KeyCode::Char('g') | KeyCode::Home => Action::Top,
        KeyCode::Char('G') | KeyCode::End => Action::Bottom,
        KeyCode::Tab => Action::SwitchPane,
        KeyCode::Char('/') => Action::StartFilter,
        KeyCode::Char('c') => Action::Clear,
        KeyCode::Char('p') => Action::TogglePause,
        KeyCode::Char('?') => Action::ToggleHelp,
        KeyCode::Char('5') => Action::Break(Break::Status500),
        KeyCode::Char('4') => Action::Break(Break::Status401),
        KeyCode::Char('n') => Action::Break(Break::Status404),
        KeyCode::Char('e') => Action::Break(Break::Empty),
        KeyCode::Char('d') => Action::Break(Break::Delay),
        KeyCode::Char('o') => Action::Break(Break::Offline),
        KeyCode::Char('x') => Action::ClearScenarios,
        _ => return None,
    })
}

fn filter(code: KeyCode) -> Option<Action> {
    Some(match code {
        KeyCode::Char(character) => Action::FilterChar(character),
        KeyCode::Backspace => Action::FilterBackspace,
        KeyCode::Enter => Action::FilterApply,
        KeyCode::Esc => Action::FilterCancel,
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn press(mode: Mode, code: KeyCode) -> Option<Action> {
        action(mode, KeyEvent::new(code, KeyModifiers::NONE))
    }

    #[test]
    fn typing_in_the_filter_does_not_trigger_commands() {
        assert_eq!(
            press(Mode::Filter, KeyCode::Char('q')),
            Some(Action::FilterChar('q'))
        );
    }

    #[test]
    fn ctrl_c_quits_from_any_mode() {
        for mode in [Mode::Normal, Mode::Filter, Mode::Help] {
            let key = KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL);
            assert_eq!(action(mode, key), Some(Action::Quit));
        }
    }

    #[test]
    fn help_swallows_other_keys() {
        assert_eq!(press(Mode::Help, KeyCode::Char('j')), None);
        assert_eq!(press(Mode::Help, KeyCode::Esc), Some(Action::CloseHelp));
    }
}
