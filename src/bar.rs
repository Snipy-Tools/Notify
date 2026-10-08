use crate::layout::{self, Dir, GAP, MARGIN, PANEL_H, PANEL_W, PILL_H, PILL_W, Rect};
use crate::settings::Aufklappen;
use crate::store;
use crate::tray::UserEvent;
use crate::windows::{Popup, parse_bar_msg};
use std::time::{Duration, Instant};
use tao::event::WindowEvent;
use tao::event_loop::{EventLoopProxy, EventLoopWindowTarget};
use tao::window::WindowId;

const SETTLE: Duration = Duration::from_millis(500);

/// Die Leiste besteht aus zwei getrennten, rahmenlosen Fenstern: der Pille (immer da) und dem Panel
/// (nur ausgeklappt), 24 px auseinander. Die Pille merkt sich Position und Zustand.
pub struct Bar {
    pill: Popup,
    panel: Popup,
    visible: bool,
    collapsed: bool,
    /// Wohin das Panel gerade aufgeklappt ist; `None` solange es zu ist
    dir: Option<Dir>,
    pref: Aufklappen,
    save_at: Option<Instant>,
}

impl Bar {
    pub fn new(
        target: &EventLoopWindowTarget<UserEvent>,
        proxy: EventLoopProxy<UserEvent>,
        pref: Aufklappen,
    ) -> Result<Self, String> {
        let state = store::load_bar();
        let pill_proxy = proxy.clone();
        let pill = Popup::new(
            target,
            "Notify",
            include_str!("./ui/pill.html"),
            (PILL_W, PILL_H),
            PILL_H / 2.0,
            move |body| {
                if let Some(msg) = parse_bar_msg(&body) {
                    pill_proxy.send_event(UserEvent::Bar(msg)).ok();
                }
            },
        )?;
        let panel = Popup::new(
            target,
            "Notify – Tag",
            include_str!("./ui/panel.html"),
            (PANEL_W, PANEL_H),
            20.0,
            move |body| {
                if let Some(msg) = parse_bar_msg(&body) {
                    proxy.send_event(UserEvent::Bar(msg)).ok();
                }
            },
        )?;
        let mut bar = Self { pill, panel, visible: false, collapsed: state.collapsed, dir: None, pref, save_at: None };
        bar.restore_position(state.pos);
        Ok(bar)
    }

    pub fn owns(&self, id: WindowId) -> bool {
        id == self.pill.id() || id == self.panel.id()
    }

    pub fn is_visible(&self) -> bool {
        self.visible
    }

    pub fn is_collapsed(&self) -> bool {
        self.collapsed
    }

    /// Hat Pille oder Panel den Fokus?
    pub fn is_focused(&self) -> bool {
        self.visible && (self.pill.is_foreground() || self.panel.is_foreground())
    }

    fn gap(&self) -> i32 {
        (GAP * self.pill.scale()).round() as i32
    }

    /// Wohin das Panel klappt: jetzt, falls schon offen, sonst wohin es bei der jetzigen Position klappen würde
    pub fn direction(&self) -> Dir {
        if let Some(dir) = self.dir {
            return dir;
        }
        match (self.pill.rect(), self.pill.work_area()) {
            (Some(pill), Some(work)) => layout::resolve_dir(self.pref, pill, work),
            _ => Dir::Down,
        }
    }

    /// Pille und (falls offen) Panel zusammen; daran verankert sich das Einstellungs-Popover
    pub fn group_rect(&self) -> Option<Rect> {
        let (x, y) = self.pill.position()?;
        Some(layout::group_rect((x, y), self.pill.size_px(), self.panel.size_px(), self.gap(), self.dir))
    }

    pub fn work_area(&self) -> Option<Rect> {
        self.pill.work_area()
    }

    /// Zeigt die Leiste, ohne dass sie den Fokus nimmt
    pub fn show_quietly(&mut self) {
        self.visible = true;
        self.pill.show(false);
        if !self.collapsed {
            self.open_panel(false);
        }
    }

    /// Zeigt die Leiste ausgeklappt und gibt dem Panel den Fokus (Hotkey, Menüpunkt)
    pub fn show_focused(&mut self) {
        self.visible = true;
        self.collapsed = false;
        self.pill.show(false);
        self.open_panel(true);
        self.save();
    }

    pub fn hide(&mut self) {
        self.pill.hide();
        self.panel.hide();
        self.visible = false;
        self.dir = None;
    }

    pub fn script_pill(&self, js: &str) {
        self.pill.script(js);
    }

    pub fn script_panel(&self, js: &str) {
        self.panel.script(js);
    }

    pub fn focus_panel_input(&self) {
        self.panel.focus_webview();
        self.panel.script("focusInput()");
    }

    pub fn drag(&self) {
        self.pill.drag();
    }

    /// Klappt das Panel ein oder aus. Die Pille bleibt stehen, ausser das Panel passt sonst nicht auf den Monitor.
    pub fn set_collapsed(&mut self, collapsed: bool) {
        self.collapsed = collapsed;
        if collapsed {
            self.panel.hide();
            self.dir = None;
        } else if self.visible {
            self.open_panel(false);
        }
        self.save();
    }

    /// Ändert die bevorzugte Richtung; ein offenes Panel klappt sofort um
    pub fn set_pref(&mut self, pref: Aufklappen) {
        if pref == self.pref {
            return;
        }
        self.pref = pref;
        if self.visible && self.dir.is_some() {
            self.dir = None;
            self.open_panel(false);
            self.save();
        }
    }

    /// Stellt das Panel neben die Pille (mit Spalt) und zeigt es
    fn open_panel(&mut self, activate: bool) {
        if self.dir.is_none() {
            let (Some(pos), Some(work)) = (self.pill.position(), self.pill.work_area()) else {
                self.panel.show(activate);
                return;
            };
            let placement = layout::expand(self.pref, pos, self.pill.size_px(), self.panel.size_px(), self.gap(), work);
            if placement.pill_moved(pos) {
                self.pill.move_to(placement.pill.0, placement.pill.1);
            }
            self.panel.move_to(placement.panel.0, placement.panel.1);
            self.dir = Some(placement.dir);
        }
        self.panel.show(activate);
    }

    /// Das Panel folgt der Pille, wenn sie verschoben wird
    fn follow_pill(&self, pill: (i32, i32)) {
        if let Some(dir) = self.dir {
            let (x, y) = layout::panel_origin(pill, self.pill.size_px(), self.panel.size_px(), self.gap(), dir);
            self.panel.move_to(x, y);
        }
    }

    fn save(&self) {
        if let Some((x, y)) = self.pill.position() {
            store::save_bar(x, y, self.collapsed);
        }
    }

    fn restore_position(&mut self, saved: Option<(i32, i32)>) {
        let on_screen = |(x, y): (i32, i32)| self.pill.on_any_monitor(x, y, 40);
        if let Some((x, y)) = saved.filter(|&p| on_screen(p)) {
            self.pill.move_to(x, y);
        } else if let Some(work) = self.pill.work_area() {
            let scale = self.pill.scale();
            let (w, h) = self.pill.size_px();
            let margin = (MARGIN * scale).round() as i32;
            self.pill.move_to(work.right() - w - margin, work.bottom() - h - margin);
        }
        self.place(false);
    }

    /// Hält die ganze Leiste auf dem Bildschirm; `snap` rastet zusätzlich am Rand ein
    fn place(&self, snap: bool) {
        let (Some(pos), Some(work)) = (self.pill.position(), self.pill.work_area()) else { return };
        let new = layout::settle_pill(
            pos,
            self.pill.size_px(),
            self.panel.size_px(),
            self.gap(),
            self.dir,
            work,
            self.pill.scale(),
            snap,
        );
        if new != pos {
            self.pill.move_to(new.0, new.1);
        }
        self.follow_pill(new);
    }

    pub fn handle_event(&mut self, id: WindowId, event: &WindowEvent) {
        self.pill.handle_event(id, event);
        self.panel.handle_event(id, event);
        if id == self.pill.id() && let WindowEvent::Moved(pos) = event {
            self.follow_pill((pos.x, pos.y));
            self.save_at = Some(Instant::now() + SETTLE);
        }
    }

    pub fn deadline(&self) -> Option<Instant> {
        self.save_at
    }

    /// Nach dem Verschieben: auf den Bildschirm holen, einrasten, speichern. `true`, wenn das passiert ist.
    pub fn tick(&mut self) -> bool {
        if self.save_at.is_some_and(|t| Instant::now() >= t) {
            self.save_at = None;
            self.place(true);
            self.save();
            return true;
        }
        false
    }
}
