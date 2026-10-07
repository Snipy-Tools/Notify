#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod server;
mod store;
mod tray;
mod widget;

use tao::event::Event;
use tao::event_loop::{ControlFlow, EventLoopBuilder};
use tray::{Tray, UserEvent};
use widget::Widget;

fn main() {
    let event_loop = EventLoopBuilder::<UserEvent>::with_user_event().build();

    let tray = Tray::new(&event_loop);
    let pending = server::start(event_loop.create_proxy());
    let mut widget = Widget::new(&event_loop, event_loop.create_proxy(), pending);

    event_loop.run(move |event, _, control_flow| {
        *control_flow = ControlFlow::Wait;

        match event {
            Event::WindowEvent { event, .. } => widget.handle_event(&event),
            Event::UserEvent(e) => tray.handle_event(e, &mut widget, control_flow),
            _ => {}
        }

        widget.tick();
        if let (ControlFlow::Wait, Some(deadline)) = (*control_flow, widget.deadline()) {
            *control_flow = ControlFlow::WaitUntil(deadline);
        }
    });
}
