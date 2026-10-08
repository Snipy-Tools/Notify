#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod bar;
mod docx;
mod export;
mod git;
mod hotkey;
mod journal;
mod layout;
mod log;
mod recap;
mod reminder;
mod settings;
mod store;
mod system;
mod timer;
mod tray;
mod view;
mod windows;

use recap::Recap;
use tao::event::Event;
use tao::event_loop::{ControlFlow, EventLoopBuilder};
use tray::{Tray, UserEvent};

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if let Some(code) = export::cli(&args) {
        std::process::exit(code);
    }
    // Eine zweite Instanz beendet sich still
    if !system::acquire_single_instance() {
        return;
    }

    let event_loop = EventLoopBuilder::<UserEvent>::with_user_event().build();

    let mut tray = Tray::new(&event_loop);
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
            Event::WindowEvent { window_id, event, .. } => recap.handle_window_event(window_id, &event),
            Event::UserEvent(e) => tray.handle_event(e, &mut recap, control_flow),
            _ => {}
        }

        recap.tick();
        tray.sync(recap.state());
        if let (ControlFlow::Wait, Some(deadline)) = (*control_flow, recap.deadline()) {
            *control_flow = ControlFlow::WaitUntil(deadline);
        }
    });
}
