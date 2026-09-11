// SPDX-License-Identifier: GPL-3.0-only

#[allow(dead_code)]
#[path = "../../../src/display_authority.rs"]
mod display_authority;
#[path = "../../../src/wayland/handlers/security_context.rs"]
mod security_context;
mod state;
mod witness;

use calloop::{
    EventLoop,
    timer::{TimeoutAction, Timer},
};
use smithay::wayland::socket::ListeningSocketSource;
use std::{sync::Arc, time::Duration};
use wayland_server::Display;

fn run() -> Result<(), Box<dyn std::error::Error>> {
    tracing::subscriber::set_global_default(
        tracing_subscriber::fmt()
            .without_time()
            .with_ansi(false)
            .with_max_level(tracing::Level::WARN)
            .finish(),
    )?;
    let authority = display_authority::DisplayAuthority::receive()?;
    assert_eq!(std::env::var("LANG").as_deref(), Ok("fr_CA.UTF-8"));
    assert_eq!(std::env::var("LC_TIME").as_deref(), Ok("C.UTF-8"));
    assert_eq!(std::env::var("LANGUAGE").as_deref(), Ok("fr:en"));
    let mut event_loop = EventLoop::try_new()?;
    let mut display = Display::<state::State>::new()?;
    let mut state = state::State::new(display.handle(), event_loop.handle(), authority)?;
    let socket = ListeningSocketSource::new_auto()?;
    let name = socket
        .socket_name()
        .to_str()
        .ok_or("invalid private display name")?
        .to_string();
    event_loop
        .handle()
        .insert_source(socket, |stream, _, state| {
            let authority = state.common.display_authority.as_ref().unwrap();
            let origin = authority.login_origin();
            match origin.accepts(&stream) {
                Ok(true) => {
                    let mut data = state.new_client_state();
                    data.display_origin = Some(origin);
                    if let Err(error) = state
                        .common
                        .display_handle
                        .insert_client(stream, Arc::new(data))
                    {
                        eprintln!("private Wayland login admission failed: {error}");
                        state.common.should_stop = true;
                    }
                }
                Ok(false) => eprintln!("private Wayland endpoint refused an unbound process"),
                Err(error) => eprintln!("private Wayland login identity failed: {error}"),
            }
        })?;
    event_loop.handle().insert_source(
        Timer::from_duration(Duration::from_millis(5)),
        |_, _, state| {
            if let Err(error) = display_authority::dispatch(state) {
                eprintln!("private compositor authority failed: {error}");
                state.common.should_stop = true;
            }
            TimeoutAction::ToDuration(Duration::from_millis(5))
        },
    )?;
    state
        .common
        .display_authority
        .as_ref()
        .unwrap()
        .ready(name)?;
    while !state.common.should_stop {
        event_loop.dispatch(Duration::from_millis(5), &mut state)?;
        display.dispatch_clients(&mut state)?;
        state.witness_writes()?;
        display.flush_clients()?;
    }
    state.witness.finish()?;
    Ok(())
}

fn main() {
    if let Err(error) = run() {
        eprintln!("private headless compositor: {error}");
        std::process::exit(1);
    }
}
