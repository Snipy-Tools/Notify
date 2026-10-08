use crate::git::Commit;
use crate::recap::{Recap, State};
use crate::settings::DayKind;
use crate::windows::{BarMsg, ReminderMsg, SettingsMsg, WeekMsg};
use tao::event_loop::{ControlFlow, EventLoop};
use tray_icon::menu::{Menu, MenuEvent, MenuId, MenuItem, PredefinedMenuItem, Submenu};
use tray_icon::{Icon, TrayIcon, TrayIconBuilder};

const ICON_SIZE: u32 = 32;
const ACCENT: [u8; 3] = [0x3D, 0xD6, 0xB0];
const WARN: [u8; 3] = [0xF2, 0xB8, 0x4B];
const DARK: [u8; 3] = [0x16, 0x20, 0x1D];

fn load_icon(state: State) -> Icon {
    let (ring, solid) = match state {
        State::Quiet => (ACCENT, false),
        State::Due => (WARN, false),
        State::Saved => (ACCENT, true),
    };
    let center = (ICON_SIZE as f32 - 1.0) / 2.0;
    let mut rgba = Vec::with_capacity((ICON_SIZE * ICON_SIZE * 4) as usize);
    for y in 0..ICON_SIZE {
        for x in 0..ICON_SIZE {
            let dist = ((x as f32 - center).powi(2) + (y as f32 - center).powi(2)).sqrt();
            let [r, g, b] = if solid || dist < 5.0 || dist >= 10.5 { ring } else { DARK };
            let a = ((15.5 - dist).clamp(0.0, 1.0) * 255.0) as u8;
            rgba.extend_from_slice(&[r, g, b, a]);
        }
    }
    Icon::from_rgba(rgba, ICON_SIZE, ICON_SIZE).expect("invalid icon data")
}

pub enum UserEvent {
    Menu(MenuEvent),
    Bar(BarMsg),
    Week(WeekMsg),
    Settings(SettingsMsg),
    Reminder(ReminderMsg),
    Hotkey,
    Commits(Vec<Commit>),
    /// Ergebnis des Datei- oder Ordnerdialogs der Einstellungen (`None`: abgebrochen)
    Picked { field: String, path: Option<String> },
}

const DAY_LABELS: [(&str, DayKind); 4] = [
    ("Arbeit", DayKind::Arbeit),
    ("Gibb", DayKind::Schule),
    ("ÜK", DayKind::Uek),
    ("Ferien", DayKind::Ferien),
];

fn tooltip(state: State) -> &'static str {
    match state {
        State::Quiet => "Notify",
        State::Due => "Notify – Eintrag fällig",
        State::Saved => "Notify – Eintrag gespeichert",
    }
}

pub struct Tray {
    icon: TrayIcon,
    state: State,
    write_id: MenuId,
    week_id: MenuId,
    export_id: MenuId,
    export_last_id: MenuId,
    journal_id: MenuId,
    settings_id: MenuId,
    day_ids: Vec<(MenuId, DayKind)>,
    quit_id: MenuId,
}

impl Tray {
    pub fn new(event_loop: &EventLoop<UserEvent>) -> Self {
        let proxy = event_loop.create_proxy();
        MenuEvent::set_event_handler(Some(move |e| {
            let _ = proxy.send_event(UserEvent::Menu(e));
        }));

        let write = MenuItem::new("Eintrag schreiben", true, None);
        let week = MenuItem::new("Woche", true, None);
        let export = MenuItem::new("Woche exportieren", true, None);
        let export_last = MenuItem::new("Letzte Woche exportieren", true, None);
        let journal = MenuItem::new("Journal-Ordner öffnen", true, None);
        let settings = MenuItem::new("Einstellungen ...", true, None);
        let today = Submenu::new("Heute ist ...", true);
        let mut day_ids = Vec::new();
        for (label, kind) in DAY_LABELS {
            let item = MenuItem::new(label, true, None);
            today.append(&item).expect("menu day failed to build");
            day_ids.push((item.id().clone(), kind));
        }
        let quit = MenuItem::new("Beenden", true, None);
        let menu = Menu::new();
        menu.append_items(&[
            &write,
            &week,
            &today,
            &PredefinedMenuItem::separator(),
            &export,
            &export_last,
            &journal,
            &settings,
            &PredefinedMenuItem::separator(),
            &quit,
        ])
        .expect("menu failed to build");

        let icon = TrayIconBuilder::new()
            .with_menu(Box::new(menu))
            .with_tooltip(tooltip(State::Quiet))
            .with_icon(load_icon(State::Quiet))
            .build()
            .expect("icon failed to build");

        Self {
            icon,
            state: State::Quiet,
            write_id: write.id().clone(),
            week_id: week.id().clone(),
            export_id: export.id().clone(),
            export_last_id: export_last.id().clone(),
            journal_id: journal.id().clone(),
            settings_id: settings.id().clone(),
            day_ids,
            quit_id: quit.id().clone(),
        }
    }

    pub fn handle_event(&self, event: UserEvent, recap: &mut Recap, control_flow: &mut ControlFlow) {
        match event {
            UserEvent::Menu(e) if e.id == self.quit_id => *control_flow = ControlFlow::Exit,
            UserEvent::Menu(e) if e.id == self.write_id => recap.open_entry(),
            UserEvent::Menu(e) if e.id == self.week_id => recap.open_week(),
            UserEvent::Menu(e) if e.id == self.export_id => recap.export(false),
            UserEvent::Menu(e) if e.id == self.export_last_id => recap.export(true),
            UserEvent::Menu(e) if e.id == self.journal_id => recap.open_journal_folder(),
            UserEvent::Menu(e) if e.id == self.settings_id => recap.open_settings(),
            UserEvent::Menu(e) => {
                if let Some(&(_, kind)) = self.day_ids.iter().find(|(id, _)| *id == e.id) {
                    recap.set_day(kind);
                }
            }
            UserEvent::Bar(msg) => recap.handle_bar(msg),
            UserEvent::Week(msg) => recap.handle_week(msg),
            UserEvent::Settings(msg) => recap.handle_settings(msg),
            UserEvent::Reminder(msg) => recap.handle_reminder(msg),
            UserEvent::Hotkey => recap.toggle_bar(),
            UserEvent::Commits(commits) => recap.ingest(commits),
            UserEvent::Picked { field, path } => recap.picked(field, path),
        }
    }

    /// Passt Icon und Tooltip an den Zustand an, falls er sich geändert hat
    pub fn sync(&mut self, state: State) {
        if state == self.state {
            return;
        }
        self.state = state;
        self.icon.set_icon(Some(load_icon(state))).ok();
        self.icon.set_tooltip(Some(tooltip(state))).ok();
    }
}
