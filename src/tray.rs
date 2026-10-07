use crate::widget::Widget;
use tao::event_loop::{ControlFlow, EventLoop};
use tray_icon::menu::{Menu, MenuEvent, MenuId, MenuItem};
use tray_icon::{Icon, TrayIcon, TrayIconBuilder};

const ICON_SIZE: u32 = 32;
const ACCENT: [u8; 3] = [0x3D, 0xD6, 0xB0];
const DARK: [u8; 3] = [0x16, 0x20, 0x1D];

fn load_icon() -> Icon {
    let center = (ICON_SIZE as f32 - 1.0) / 2.0;
    let mut rgba = Vec::with_capacity((ICON_SIZE * ICON_SIZE * 4) as usize);
    for y in 0..ICON_SIZE {
        for x in 0..ICON_SIZE {
            let dist = ((x as f32 - center).powi(2) + (y as f32 - center).powi(2)).sqrt();
            let [r, g, b] = if dist < 5.0 || dist >= 10.5 { ACCENT } else { DARK };
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
}

pub struct Tray {
    _icon: TrayIcon,
    quit_id: MenuId,
}

impl Tray {
    pub fn new(event_loop: &EventLoop<UserEvent>) -> Self {
        let proxy = event_loop.create_proxy();
        MenuEvent::set_event_handler(Some(move |e| {
            let _ = proxy.send_event(UserEvent::Menu(e));
        }));

        let quit = MenuItem::new("Quit", true, None);
        let menu = Menu::new();
        menu.append(&quit).expect("menu quit failed to build");

        let icon = TrayIconBuilder::new()
            .with_menu(Box::new(menu))
            .with_tooltip("Notify")
            .with_icon(load_icon())
            .build()
            .expect("icon failed to build");

        Self {
            _icon: icon,
            quit_id: quit.id().clone(),
        }
    }

    pub fn handle_event(&self, event: UserEvent, widget: &mut Widget, control_flow: &mut ControlFlow) {
        match event {
            UserEvent::Menu(e) if e.id == self.quit_id => *control_flow = ControlFlow::Exit,
            UserEvent::Menu(_) => {}
            other => widget.handle_user(other),
        }
    }
}
