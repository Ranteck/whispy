//! Context menu construction and menu item action mapping.

use ldtray::{Menu, MenuId, MenuItem};

use crate::history::{Transcript, preview};
use crate::settings::Language;
use crate::view::{Kind, header, toggle_label};

const PREVIEW_MAX_CHARS: usize = 42;

const ID_DISABLED: u32 = 0;
const ID_TOGGLE: u32 = 1;
const ID_CANCEL: u32 = 2;
const ID_OPEN_LOG: u32 = 3;
const ID_LANG_ES: u32 = 4;
const ID_LANG_EN: u32 = 5;
const ID_LANG_AUTO: u32 = 6;
const ID_EDIT_CONFIG: u32 = 7;
const ID_RESTART_DAEMON: u32 = 8;
const ID_QUIT: u32 = 9;
const ID_COPY_BASE: u32 = 100;

/// Action triggered by clicking an enabled tray menu item.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action {
    /// Start or stop dictation.
    Toggle,
    /// Cancel the current dictation and discard audio.
    Cancel,
    /// Copy the transcript at the given history index.
    Copy(usize),
    /// Open `transcripts.jsonl` with the system handler.
    OpenLog,
    /// Switch `stt.language` in `config.toml` and restart the daemon.
    SetLanguage(Language),
    /// Open `config.toml` with the system handler.
    EditConfig,
    /// Restart `whispy-daemon.service`.
    RestartDaemon,
    /// Restore audio mute state and exit the tray.
    Quit,
}

/// Build the context menu for the current state, recent transcripts, and active language.
pub fn build(kind: Kind, history: &[Transcript], language: Language) -> Menu {
    Menu::new()
        .item(MenuItem::button(ID_DISABLED, header(kind)).enabled(false))
        .item(MenuItem::separator())
        .item(MenuItem::button(ID_TOGGLE, toggle_label(kind)))
        .item(MenuItem::button(ID_CANCEL, "Cancel dictation"))
        .item(MenuItem::separator())
        .item(MenuItem::submenu(
            format!("History ({})", history.len()),
            history_items(history),
        ))
        .item(MenuItem::separator())
        .item(MenuItem::submenu(
            format!("Language: {}", language.name()),
            language_items(language),
        ))
        .item(MenuItem::button(
            ID_EDIT_CONFIG,
            "Edit configuration (config.toml)",
        ))
        .item(MenuItem::button(
            ID_RESTART_DAEMON,
            "Restart daemon (whispy-daemon)",
        ))
        .item(MenuItem::separator())
        .item(MenuItem::button(ID_QUIT, "Quit"))
}

fn history_items(history: &[Transcript]) -> Vec<MenuItem> {
    if history.is_empty() {
        return vec![MenuItem::button(ID_DISABLED, "(No recent recordings)").enabled(false)];
    }

    let mut items = Vec::with_capacity(history.len() + 2);
    for (index, transcript) in history.iter().enumerate() {
        if let Some(id) = u32::try_from(index)
            .ok()
            .and_then(|offset| ID_COPY_BASE.checked_add(offset))
        {
            items.push(MenuItem::button(
                id,
                preview(&transcript.text, PREVIEW_MAX_CHARS),
            ));
        }
    }
    items.push(MenuItem::separator());
    items.push(MenuItem::button(
        ID_OPEN_LOG,
        "Open full log (transcripts.jsonl)",
    ));
    items
}

fn language_items(current: Language) -> [MenuItem; 3] {
    [
        (ID_LANG_ES, Language::Es),
        (ID_LANG_EN, Language::En),
        (ID_LANG_AUTO, Language::Auto),
    ]
    .map(|(id, lang)| MenuItem::checkbox(id, lang.item_label(), lang == current))
}

/// Map a clicked [`MenuId`] to its corresponding [`Action`].
pub fn action(id: MenuId) -> Option<Action> {
    match id.0 {
        ID_TOGGLE => Some(Action::Toggle),
        ID_CANCEL => Some(Action::Cancel),
        ID_OPEN_LOG => Some(Action::OpenLog),
        ID_LANG_ES => Some(Action::SetLanguage(Language::Es)),
        ID_LANG_EN => Some(Action::SetLanguage(Language::En)),
        ID_LANG_AUTO => Some(Action::SetLanguage(Language::Auto)),
        ID_EDIT_CONFIG => Some(Action::EditConfig),
        ID_RESTART_DAEMON => Some(Action::RestartDaemon),
        ID_QUIT => Some(Action::Quit),
        raw if raw >= ID_COPY_BASE => usize::try_from(raw - ID_COPY_BASE).ok().map(Action::Copy),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_history() -> Vec<Transcript> {
        vec![
            Transcript {
                text: "ñ".repeat(50),
                timestamp: 20.0,
            },
            Transcript {
                text: "hello   world\nfrom whispy".to_owned(),
                timestamp: 10.0,
            },
        ]
    }

    #[test]
    fn idle_menu_order() {
        let history = sample_history();
        let menu = build(Kind::Idle, &history, Language::Auto);
        let items = menu.items();

        let top_level: Vec<Option<&str>> = items
            .iter()
            .map(|item| match item {
                MenuItem::Button { label, .. } | MenuItem::Submenu { label, .. } => {
                    Some(label.as_str())
                }
                MenuItem::Separator => None,
            })
            .collect();

        assert_eq!(
            top_level,
            vec![
                Some("Whispy: ✓ Ready to dictate"),
                None,
                Some("Start dictation"),
                Some("Cancel dictation"),
                None,
                Some("History (2)"),
                None,
                Some("Language: Automatic"),
                Some("Edit configuration (config.toml)"),
                Some("Restart daemon (whispy-daemon)"),
                None,
                Some("Quit"),
            ]
        );

        match &items[0] {
            MenuItem::Button {
                label,
                enabled,
                checked,
                ..
            } => {
                assert_eq!(label, header(Kind::Idle));
                assert!(!enabled);
                assert_eq!(*checked, None);
            }
            other => panic!("expected disabled header button, got {other:?}"),
        }

        let recording_menu = build(Kind::Recording, &history, Language::Auto);
        match &recording_menu.items()[2] {
            MenuItem::Button { label, enabled, .. } => {
                assert_eq!(label, toggle_label(Kind::Recording));
                assert!(*enabled);
            }
            other => panic!("expected toggle button, got {other:?}"),
        }
    }

    #[test]
    fn history_submenu() {
        let history = sample_history();
        let menu = build(Kind::Idle, &history, Language::Auto);
        let Some(MenuItem::Submenu {
            label,
            enabled,
            items,
        }) = menu.items().get(5)
        else {
            panic!("expected History submenu at index 5");
        };
        assert_eq!(label, "History (2)");
        assert!(*enabled);
        assert_eq!(items.len(), 4);

        match &items[0] {
            MenuItem::Button {
                label,
                enabled,
                checked,
                ..
            } => {
                assert_eq!(label, &preview(&history[0].text, 42));
                assert_eq!(label.chars().count(), 42);
                assert!(*enabled);
                assert_eq!(*checked, None);
            }
            other => panic!("expected first transcript button, got {other:?}"),
        }
        match &items[1] {
            MenuItem::Button {
                label,
                enabled,
                checked,
                ..
            } => {
                assert_eq!(label, "hello world from whispy");
                assert!(*enabled);
                assert_eq!(*checked, None);
            }
            other => panic!("expected second transcript button, got {other:?}"),
        }
        assert!(matches!(items[2], MenuItem::Separator));
        match &items[3] {
            MenuItem::Button {
                label,
                enabled,
                checked,
                ..
            } => {
                assert_eq!(label, "Open full log (transcripts.jsonl)");
                assert!(*enabled);
                assert_eq!(*checked, None);
            }
            other => panic!("expected open full log button, got {other:?}"),
        }

        let empty_menu = build(Kind::Idle, &[], Language::Auto);
        let Some(MenuItem::Submenu {
            label: empty_label,
            enabled: empty_enabled,
            items: empty_items,
        }) = empty_menu.items().get(5)
        else {
            panic!("expected empty History submenu at index 5");
        };
        assert_eq!(empty_label, "History (0)");
        assert!(*empty_enabled);
        assert_eq!(empty_items.len(), 1);
        match &empty_items[0] {
            MenuItem::Button {
                id,
                label,
                enabled,
                checked,
            } => {
                assert_eq!(label, "(No recent recordings)");
                assert!(!enabled);
                assert_eq!(*checked, None);
                assert_eq!(action(*id), None);
            }
            other => panic!("expected disabled placeholder button, got {other:?}"),
        }
    }

    #[test]
    fn language_submenu_checks_current() {
        for current in [Language::Es, Language::En, Language::Auto] {
            let menu = build(Kind::Idle, &[], current);
            let Some(MenuItem::Submenu {
                label,
                enabled,
                items,
            }) = menu.items().get(7)
            else {
                panic!("expected Language submenu at index 7");
            };
            assert_eq!(label, &format!("Language: {}", current.name()));
            assert!(*enabled);

            let entries: Vec<(&str, bool, Option<bool>)> = items
                .iter()
                .map(|item| match item {
                    MenuItem::Button {
                        label,
                        enabled,
                        checked,
                        ..
                    } => (label.as_str(), *enabled, *checked),
                    other => panic!("expected language checkbox button, got {other:?}"),
                })
                .collect();

            assert_eq!(
                entries,
                vec![
                    ("Spanish (es)", true, Some(current == Language::Es)),
                    ("English (en)", true, Some(current == Language::En)),
                    ("Automatic (auto)", true, Some(current == Language::Auto)),
                ]
            );
        }
    }

    #[test]
    fn every_clickable_item_maps_to_an_action() {
        fn collect_buttons(items: &[MenuItem], out: &mut Vec<(MenuId, String, bool)>) {
            for item in items {
                match item {
                    MenuItem::Button {
                        id, label, enabled, ..
                    } => out.push((*id, label.clone(), *enabled)),
                    MenuItem::Submenu { items, .. } => collect_buttons(items, out),
                    MenuItem::Separator => {}
                }
            }
        }

        let history = sample_history();
        let menu = build(Kind::Idle, &history, Language::Auto);
        let mut buttons = Vec::new();
        collect_buttons(menu.items(), &mut buttons);

        let enabled_actions: Vec<Option<Action>> = buttons
            .iter()
            .filter(|(_, _, enabled)| *enabled)
            .map(|(id, _, _)| action(*id))
            .collect();

        assert_eq!(
            enabled_actions,
            vec![
                Some(Action::Toggle),
                Some(Action::Cancel),
                Some(Action::Copy(0)),
                Some(Action::Copy(1)),
                Some(Action::OpenLog),
                Some(Action::SetLanguage(Language::Es)),
                Some(Action::SetLanguage(Language::En)),
                Some(Action::SetLanguage(Language::Auto)),
                Some(Action::EditConfig),
                Some(Action::RestartDaemon),
                Some(Action::Quit),
            ]
        );

        for (id, label, enabled) in &buttons {
            if !enabled {
                assert_eq!(
                    action(*id),
                    None,
                    "disabled item {label:?} should not map to an action"
                );
            }
        }
    }
}
