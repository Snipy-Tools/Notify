#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod docx;
mod export;
mod git;
mod journal;
mod log;
mod recap;
mod server;
mod settings;
mod store;
mod system;
mod timer;
mod today;
mod tray;
mod widget;
mod windows;

use recap::Recap;
use tao::event::Event;
use tao::event_loop::{ControlFlow, EventLoopBuilder};
use tray::{Tray, UserEvent};
use widget::Widget;

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if let Some(code) = export::cli(&args) {
        std::process::exit(code);
    }

    let event_loop = EventLoopBuilder::<UserEvent>::with_user_event().build();

    let mut tray = Tray::new(&event_loop);
    let pending = server::start(event_loop.create_proxy());
    let mut widget = Widget::new(&event_loop, event_loop.create_proxy(), pending);
    let mut recap = match Recap::new(&event_loop, event_loop.create_proxy()) {
        Ok(recap) => recap,
        Err(e) => {
            log::error(&e);
            std::process::exit(1);
        }
    };

    event_loop.run(move |event, _, control_flow| {
        *control_flow = ControlFlow::Wait;

        match event {
            Event::WindowEvent { window_id, event, .. } => {
                widget.handle_event(window_id, &event);
                recap.handle_window_event(window_id, &event);
            }
            Event::UserEvent(e) => tray.handle_event(e, &mut widget, &mut recap, control_flow),
            _ => {}
        }

        widget.tick();
        recap.tick();
        tray.sync(recap.state());
        let deadline = [widget.deadline(), recap.deadline()].into_iter().flatten().min();
        if let (ControlFlow::Wait, Some(deadline)) = (*control_flow, deadline) {
            *control_flow = ControlFlow::WaitUntil(deadline);
        }
    });
}
