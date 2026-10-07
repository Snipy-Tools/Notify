use crate::server::Pending;
use crate::store;
use crate::tray::UserEvent;
use std::time::{Duration, Instant};
use tao::dpi::{LogicalSize, PhysicalPosition};
use tao::event::WindowEvent;
use tao::event_loop::{EventLoopProxy, EventLoopWindowTarget};
use tao::platform::windows::WindowBuilderExtWindows;
use tao::window::{Window, WindowBuilder};
use wry::{WebView, WebViewBuilder};

const SIZE: LogicalSize<f64> = LogicalSize::new(340.0, 52.0);
const MARGIN: f64 = 12.0;
const TASKBAR: f64 = 48.0;
const SNAP: f64 = 24.0;
const SETTLE: Duration = Duration::from_millis(500);

fn js_string(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('"');
    for c in s.chars() {
        match c {
            '\\' => out.push_str("\\\\"),
            '"' => out.push_str("\\\""),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            c if c.is_control() || c == '\u{2028}' || c == '\u{2029}' => {
                out.push_str(&format!("\\u{:04x}", c as u32));
            }
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

fn answer(pending: &Pending, id: &str, text: &str) {
    if let Ok(id) = id.parse::<u64>()
        && let Some(tx) = pending.lock().unwrap().remove(&id)
    {
        tx.send(text.to_string()).ok();
    }
}

pub struct Widget {
    window: Window,
    webview: WebView,
    save_at: Option<Instant>,
}

impl Widget {
    pub fn new(
        target: &EventLoopWindowTarget<UserEvent>,
        proxy: EventLoopProxy<UserEvent>,
        pending: Pending,
    ) -> Self {
        let window = WindowBuilder::new()
            .with_decorations(false)
            .with_transparent(true)
            .with_always_on_top(true)
            .with_resizable(false)
            .with_skip_taskbar(true)
            .with_undecorated_shadow(false)
            .with_focused(false)
            .with_inner_size(SIZE)
            .build(target)
            .expect("widget failed to build");

        let webview = WebViewBuilder::new()
            .with_transparent(true)
            .with_html(include_str!("./ui/widget.html"))
            .with_ipc_handler(move |req| {
                let msg = req.body().as_str();
                if msg == "drag" {
                    proxy.send_event(UserEvent::Drag).ok();
                } else if let Some(v) = msg.strip_prefix("size:") {
                    if let Some((w, h)) = v.split_once(':')
                        && let (Ok(w), Ok(h)) = (w.parse(), h.parse())
                    {
                        proxy.send_event(UserEvent::Resize(w, h)).ok();
                    }
                } else if let Some(v) = msg.strip_prefix("reply:") {
                    if let Some((id, text)) = v.split_once(':') {
                        answer(&pending, id, text);
                    }
                } else if let Some(id) = msg.strip_prefix("release:") {
                    answer(&pending, id, "");
                }
            })
            .build(&window)
            .expect("widget webview failed to build");

        let widget = Self { window, webview, save_at: None };
        widget.restore_position();
        widget
    }

    fn restore_position(&self) {
        let saved = store::load_pos().filter(|&(x, y)| {
            self.window.available_monitors().any(|m| {
                let p = m.position();
                let s = m.size();
                x >= p.x && y >= p.y && x < p.x + s.width as i32 - 40 && y < p.y + s.height as i32 - 40
            })
        });

        if let Some((x, y)) = saved {
            self.window.set_outer_position(PhysicalPosition::new(x, y));
        } else if let Some(monitor) = self.window.primary_monitor() {
            let scale = monitor.scale_factor();
            let pos = monitor.position();
            let mon = monitor.size();
            let win = self.window.outer_size();
            self.window.set_outer_position(PhysicalPosition::new(
                pos.x as f64 + (mon.width as f64 - win.width as f64) / 2.0,
                pos.y as f64 + MARGIN * scale,
            ));
        }
    }

    fn place(&self, snap: bool) {
        let Some(monitor) = self.window.current_monitor() else { return };
        let Ok(pos) = self.window.outer_position() else { return };
        let scale = self.window.scale_factor();
        let size = self.window.outer_size();
        let (mx, my) = (monitor.position().x as f64, monitor.position().y as f64);
        let (mw, mh) = (monitor.size().width as f64, monitor.size().height as f64);
        let (w, h) = (size.width as f64, size.height as f64);

        let mut x = (pos.x as f64).clamp(mx, (mx + mw - w).max(mx));
        let mut y = (pos.y as f64).clamp(my, (my + mh - h).max(my));

        if snap {
            let near = SNAP * scale;
            let margin = MARGIN * scale;
            let bottom = (MARGIN + TASKBAR) * scale;
            if x - mx < near {
                x = mx + margin;
            } else if mx + mw - (x + w) < near {
                x = mx + mw - w - margin;
            }
            if y - my < near {
                y = my + margin;
            } else if my + mh - (y + h) < near {
                y = my + mh - h - bottom;
            }
        }

        if x != pos.x as f64 || y != pos.y as f64 {
            self.window.set_outer_position(PhysicalPosition::new(x, y));
        }
    }

    pub fn handle_user(&mut self, event: UserEvent) {
        match event {
            UserEvent::Hook(id, body) => {
                self.webview.evaluate_script(&format!("onHook({id},{})", js_string(&body))).ok();
            }
            UserEvent::Status(body) => {
                self.webview.evaluate_script(&format!("onStatus({})", js_string(&body))).ok();
            }
            UserEvent::Gone(id) => {
                self.webview.evaluate_script(&format!("onGone({id})")).ok();
            }
            UserEvent::Resize(w, h) => {
                self.window.set_inner_size(LogicalSize::new(w, h));
                self.place(false);
            }
            UserEvent::Drag => {
                self.window.drag_window().ok();
            }
            UserEvent::Menu(_) => {}
        }
    }

    pub fn handle_event(&mut self, event: &WindowEvent) {
        if let WindowEvent::Moved(_) = event {
            self.save_at = Some(Instant::now() + SETTLE);
        }
    }

    pub fn deadline(&self) -> Option<Instant> {
        self.save_at
    }

    pub fn tick(&mut self) {
        if self.save_at.is_some_and(|t| Instant::now() >= t) {
            self.save_at = None;
            self.place(true);
            if let Ok(pos) = self.window.outer_position() {
                store::save_pos(pos.x, pos.y);
            }
        }
    }
}
