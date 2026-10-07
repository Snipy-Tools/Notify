use crate::git::Commit;
use crate::journal::DayKind;
use crate::recap::{Recap, State};
use crate::widget::Widget;
use crate::windows::{EntryMsg, SettingsMsg, TodayMsg};
use tao::event_loop::{ControlFlow, EventLoop};
use tray_icon::menu::{Menu, MenuEvent, MenuId, MenuItem, Submenu};
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
    Hook(u64, String),
    Status(String),
    Gone(u64),
    Resize(f64, f64),
    Drag,
    Entry(EntryMsg),
    Hotkey,
    Commits(Vec<Commit>),
    Today(TodayMsg),
    Settings(SettingsMsg),
}

const DAY_LABELS: [(&str, DayKind); 5] = [
    ("Arbeit", DayKind::Arbeit),
    ("Schule", DayKind::Schule),
    ("ÜK", DayKind::Uek),
    ("Ferien", DayKind::Ferien),
    ("Krank", DayKind::Krank),
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
    reflect_id: MenuId,
    export_id: MenuId,
    export_last_id: MenuId,
    today_id: MenuId,
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

        let write = MenuItem::new("Eintrag jetzt schreiben", true, None);
        let reflect = MenuItem::new("Wochenreflexion schreiben", true, None);
        let export = MenuItem::new("Woche exportieren", true, None);
        let export_last = MenuItem::new("Letzte Woche exportieren", true, None);
        let entries = MenuItem::new("Heutige Einträge", true, None);
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
        menu.append(&write).expect("menu write failed to build");
        menu.append(&reflect).expect("menu reflect failed to build");
        menu.append(&today).expect("menu today failed to build");
        menu.append(&export).expect("menu export failed to build");
        menu.append(&export_last).expect("menu export last failed to build");
        menu.append(&entries).expect("menu entries failed to build");
        menu.append(&journal).expect("menu journal failed to build");
        menu.append(&settings).expect("menu settings failed to build");
        menu.append(&quit).expect("menu quit failed to build");

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
            reflect_id: reflect.id().clone(),
            export_id: export.id().clone(),
            export_last_id: export_last.id().clone(),
            today_id: entries.id().clone(),
            journal_id: journal.id().clone(),
            settings_id: settings.id().clone(),
            day_ids,
            quit_id: quit.id().clone(),
        }
    }

    pub fn handle_event(
        &self,
        event: UserEvent,
        widget: &mut Widget,
        recap: &mut Recap,
        control_flow: &mut ControlFlow,
    ) {
        match event {
            UserEvent::Menu(e) if e.id == self.quit_id => *control_flow = ControlFlow::Exit,
            UserEvent::Menu(e) if e.id == self.write_id => recap.open_checkin(),
            UserEvent::Menu(e) if e.id == self.reflect_id => recap.open_reflection(),
            UserEvent::Menu(e) if e.id == self.export_id => recap.export(false),
            UserEvent::Menu(e) if e.id == self.export_last_id => recap.export(true),
            UserEvent::Menu(e) if e.id == self.today_id => recap.open_today(),
            UserEvent::Menu(e) if e.id == self.journal_id => recap.open_journal_folder(),
            UserEvent::Menu(e) if e.id == self.settings_id => recap.open_settings(),
            UserEvent::Menu(e) => {
                if let Some(&(_, kind)) = self.day_ids.iter().find(|(id, _)| *id == e.id) {
                    recap.set_day(kind);
                }
            }
            UserEvent::Entry(msg) => recap.handle_entry(msg),
            UserEvent::Hotkey => recap.open_quick(),
            UserEvent::Commits(commits) => recap.ingest(commits),
            UserEvent::Today(msg) => recap.handle_today(msg),
            UserEvent::Settings(msg) => recap.handle_settings(msg),
            other => widget.handle_user(other),
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
