// dwl's state over its ipc protocol (dwl-ipc-unstable-v2), on a Wayland connection of its own
// that is dispatched from the GLib main loop.
use std::{
    cell::RefCell,
    collections::HashMap,
    os::fd::{AsFd, AsRawFd},
    rc::Rc,
    time::Duration,
};

use gtk::glib;
use wayland_client::{
    Connection, Dispatch, EventQueue, Proxy, QueueHandle, WEnum,
    protocol::{wl_output, wl_registry},
};

use protocol::{zdwl_ipc_manager_v2, zdwl_ipc_output_v2};

#[allow(non_upper_case_globals, clippy::all)]
mod protocol {
    use wayland_client;
    use wayland_client::protocol::*;

    pub mod __interfaces {
        use wayland_client::backend as wayland_backend;
        use wayland_client::protocol::__interfaces::*;
        wayland_scanner::generate_interfaces!("protocols/dwl-ipc-unstable-v2.xml");
    }
    use self::__interfaces::*;

    wayland_scanner::generate_client_code!("protocols/dwl-ipc-unstable-v2.xml");
}

#[derive(Debug, Default, Clone)]
pub struct OutputState {
    pub output: String,
    pub selected: bool,
    pub active: u32,
    pub occupied: u32,
    pub urgent: u32,
    pub layout: u32,
    pub symbol: String,
    pub title: String,
    pub has_window: bool,
    pub floating: bool,
}

#[derive(Debug)]
pub enum Event {
    State(OutputState),
    /// dwl's togglebar binding
    ToggleVisibility(String),
}

#[derive(Debug)]
pub enum Command {
    View(u32),
    ToggleView(u32),
    SetLayout(u32),
    /// Back to the previous layout, like dwl's setlayout with no argument
    ToggleLayout,
    Zoom,
}

struct Output {
    wl_output: wl_output::WlOutput,
    ipc: Option<zdwl_ipc_output_v2::ZdwlIpcOutputV2>,
    pending: OutputState,
    current: OutputState,
    previous_layout: u32,
    received_frame: bool,
}

struct State {
    manager: Option<zdwl_ipc_manager_v2::ZdwlIpcManagerV2>,
    /// By wl_registry global name
    outputs: HashMap<u32, Output>,
    on_event: Rc<dyn Fn(Event)>,
}

struct Session {
    conn: Connection,
    queue: EventQueue<State>,
    state: State,
    source: Option<glib::SourceId>,
}

pub struct Dwl {
    session: RefCell<Option<Session>>,
    on_event: Rc<dyn Fn(Event)>,
}

impl Dwl {
    pub fn start(on_event: impl Fn(Event) + 'static) -> Rc<Dwl> {
        let dwl = Rc::new(Dwl { session: RefCell::new(None), on_event: Rc::new(on_event) });
        dwl.connect(true);
        dwl
    }

    pub fn send(&self, output: &str, command: Command) {
        let session = self.session.borrow();
        let Some(session) = session.as_ref() else { return };
        let Some((out, ipc)) = session
            .state
            .outputs
            .values()
            .find(|o| o.current.output == output)
            .and_then(|o| Some((o, o.ipc.as_ref()?)))
        else {
            return;
        };
        match command {
            Command::View(mask) => ipc.set_tags(mask, 1),
            // dwl ignores an empty tag set, like its toggleview
            Command::ToggleView(mask) if out.current.active ^ mask != 0 => {
                ipc.set_tags(out.current.active ^ mask, 0)
            }
            Command::ToggleView(_) => {}
            Command::SetLayout(index) => ipc.set_layout(index),
            Command::ToggleLayout => ipc.set_layout(out.previous_layout),
            Command::Zoom if ipc.version() >= 3 => ipc.zoom(),
            Command::Zoom => {}
        }
        let _ = session.conn.flush();
    }

    fn connect(self: &Rc<Self>, report: bool) {
        match self.open() {
            Ok(()) => {}
            Err(e) => {
                if report {
                    eprintln!("wakeshell: dwl ipc: {e}; trying again every 2 seconds");
                }
                self.retry(false);
            }
        }
    }

    fn retry(self: &Rc<Self>, report: bool) {
        if let Some(mut session) = self.session.take()
            && let Some(source) = session.source.take()
        {
            source.remove();
        }
        let weak = Rc::downgrade(self);
        glib::timeout_add_local_once(Duration::from_secs(2), move || {
            if let Some(dwl) = weak.upgrade() {
                dwl.connect(report);
            }
        });
    }

    fn open(self: &Rc<Self>) -> Result<(), String> {
        let conn = Connection::connect_to_env().map_err(|e| e.to_string())?;
        let mut queue = conn.new_event_queue();
        let qh = queue.handle();
        conn.display().get_registry(&qh, ());
        let mut state = State { manager: None, outputs: HashMap::new(), on_event: self.on_event.clone() };
        // The first roundtrip announces the globals, the second their initial state
        queue.roundtrip(&mut state).map_err(|e| e.to_string())?;
        if state.manager.is_none() {
            return Err("the compositor does not support dwl-ipc-unstable-v2".into());
        }
        queue.roundtrip(&mut state).map_err(|e| e.to_string())?;

        let fd = conn.as_fd().as_raw_fd();
        let weak = Rc::downgrade(self);
        let condition = glib::IOCondition::IN | glib::IOCondition::HUP | glib::IOCondition::ERR;
        let source = glib_unix::unix_fd_add_local(fd, condition, move |_, condition| {
            let Some(dwl) = weak.upgrade() else { return glib::ControlFlow::Break };
            if dwl.dispatch(condition) {
                glib::ControlFlow::Continue
            } else {
                // retry() removes this source, so let GLib know it is gone first
                if let Some(session) = dwl.session.borrow_mut().as_mut() {
                    session.source = None;
                }
                dwl.retry(true);
                glib::ControlFlow::Break
            }
        });
        self.session.replace(Some(Session { conn, queue, state, source: Some(source) }));
        Ok(())
    }

    /// False once the connection is gone
    fn dispatch(&self, condition: glib::IOCondition) -> bool {
        let mut session = self.session.borrow_mut();
        let Some(session) = session.as_mut() else { return false };
        if let Some(guard) = session.queue.prepare_read() {
            match guard.read() {
                Ok(_) => {}
                Err(wayland_client::backend::WaylandError::Io(e))
                    if e.kind() == std::io::ErrorKind::WouldBlock => {}
                Err(_) => return false,
            }
        }
        if session.queue.dispatch_pending(&mut session.state).is_err() {
            return false;
        }
        let _ = session.conn.flush();
        !condition.intersects(glib::IOCondition::HUP | glib::IOCondition::ERR)
    }
}

impl State {
    fn start_ipc(&mut self, global: u32, qh: &QueueHandle<State>) {
        let (Some(manager), Some(out)) = (&self.manager, self.outputs.get_mut(&global)) else { return };
        if out.ipc.is_none() {
            out.ipc = Some(manager.get_output(&out.wl_output, qh, global));
        }
    }
}

impl Dispatch<wl_registry::WlRegistry, ()> for State {
    fn event(
        state: &mut Self,
        registry: &wl_registry::WlRegistry,
        event: wl_registry::Event,
        _: &(),
        _: &Connection,
        qh: &QueueHandle<Self>,
    ) {
        match event {
            wl_registry::Event::Global { name, interface, version } => {
                if interface == zdwl_ipc_manager_v2::ZdwlIpcManagerV2::interface().name {
                    // The unmodified ipc patch announces the global twice
                    if state.manager.is_some() {
                        return;
                    }
                    state.manager = Some(registry.bind(name, version.min(3), qh, ()));
                    let globals: Vec<u32> = state.outputs.keys().copied().collect();
                    for global in globals {
                        state.start_ipc(global, qh);
                    }
                } else if interface == wl_output::WlOutput::interface().name && version >= 4 {
                    let wl_output = registry.bind(name, 4, qh, name);
                    state.outputs.insert(
                        name,
                        Output {
                            wl_output,
                            ipc: None,
                            pending: OutputState::default(),
                            current: OutputState::default(),
                            previous_layout: 0,
                            received_frame: false,
                        },
                    );
                    state.start_ipc(name, qh);
                }
            }
            wl_registry::Event::GlobalRemove { name } => {
                if let Some(out) = state.outputs.remove(&name) {
                    if let Some(ipc) = out.ipc {
                        ipc.release();
                    }
                    out.wl_output.release();
                }
            }
            _ => {}
        }
    }
}

impl Dispatch<wl_output::WlOutput, u32> for State {
    fn event(
        state: &mut Self,
        _: &wl_output::WlOutput,
        event: wl_output::Event,
        global: &u32,
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let wl_output::Event::Name { name } = event
            && let Some(out) = state.outputs.get_mut(global)
        {
            out.pending.output = name;
        }
    }
}

impl Dispatch<zdwl_ipc_manager_v2::ZdwlIpcManagerV2, ()> for State {
    fn event(
        _: &mut Self,
        _: &zdwl_ipc_manager_v2::ZdwlIpcManagerV2,
        _: zdwl_ipc_manager_v2::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
    }
}

impl Dispatch<zdwl_ipc_output_v2::ZdwlIpcOutputV2, u32> for State {
    fn event(
        state: &mut Self,
        _: &zdwl_ipc_output_v2::ZdwlIpcOutputV2,
        event: zdwl_ipc_output_v2::Event,
        global: &u32,
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        use zdwl_ipc_output_v2::Event as E;
        let Some(out) = state.outputs.get_mut(global) else { return };
        let s = &mut out.pending;
        match event {
            E::ToggleVisibility => (state.on_event)(Event::ToggleVisibility(s.output.clone())),
            E::Active { active } => s.selected = active != 0,
            E::Tag { tag, state: tag_state, clients, focused } => {
                // A bitfield, though the protocol does not declare it as one
                let bits = match tag_state {
                    WEnum::Value(v) => v as u32,
                    WEnum::Unknown(v) => v,
                };
                let bit = 1u32.checked_shl(tag).unwrap_or(0);
                let set = |mask: &mut u32, on: bool| *mask = (*mask & !bit) | if on { bit } else { 0 };
                set(&mut s.active, bits & 1 != 0);
                set(&mut s.urgent, bits & 2 != 0);
                set(&mut s.occupied, clients != 0);
                if focused != 0 {
                    s.has_window = true;
                }
            }
            E::Layout { layout } => s.layout = layout,
            E::Title { title } => s.title = title,
            E::LayoutSymbol { layout } => s.symbol = layout,
            E::Floating { is_floating } => s.floating = is_floating != 0,
            E::Frame => {
                // dwl starts each output with layouts[1] as the previous layout, or layouts[0]
                // if layouts[1] is the current one
                if !out.received_frame {
                    out.previous_layout = if out.pending.layout == 1 { 0 } else { 1 };
                } else if out.pending.layout != out.current.layout {
                    out.previous_layout = out.current.layout;
                }
                out.received_frame = true;
                out.current = out.pending.clone();
                // The tag events set it again in the next update
                out.pending.has_window = false;
                (state.on_event)(Event::State(out.current.clone()));
            }
            _ => {}
        }
    }
}
