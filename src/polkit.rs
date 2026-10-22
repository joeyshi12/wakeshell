// Polkit authentication agent. Passwords go to polkit-agent-helper-1 through
// /run/polkit/agent-helper.socket, or the setuid helper where there is no socket.
use std::{
    cell::{Cell, RefCell},
    collections::{HashMap, VecDeque},
    rc::{Rc, Weak},
};

use gtk::{gdk, gio, glib, prelude::*};
use zbus::zvariant::{OwnedValue, Value};

use crate::{backdrop, util};

const AGENT_PATH: &str = "/org/wakeshell/PolicyKit1/AuthenticationAgent";
const HELPER_SOCKET: &str = "/run/polkit/agent-helper.socket";
const HELPER: &str = "/usr/lib/polkit-1/polkit-agent-helper-1";

#[derive(zbus::DBusError, Debug)]
#[zbus(prefix = "org.freedesktop.PolicyKit1.Error")]
enum AgentError {
    #[zbus(error)]
    ZBus(zbus::Error),
    Cancelled(String),
}

struct Request {
    cookie: String,
    message: String,
    uids: Vec<u32>,
    done: async_channel::Sender<bool>,
}

enum Event {
    Begin(Request),
    Cancel(String),
}

struct Agent {
    events: async_channel::Sender<Event>,
}

#[zbus::interface(name = "org.freedesktop.PolicyKit1.AuthenticationAgent")]
impl Agent {
    async fn begin_authentication(
        &self,
        _action_id: String,
        message: String,
        _icon_name: String,
        _details: HashMap<String, String>,
        cookie: String,
        identities: Vec<(String, HashMap<String, OwnedValue>)>,
    ) -> Result<(), AgentError> {
        let uids = identities
            .iter()
            .filter(|(kind, _)| kind == "unix-user")
            .filter_map(|(_, fields)| u32::try_from(fields.get("uid")?.try_clone().ok()?).ok())
            .collect();
        let (done, result) = async_channel::bounded(1);
        let request = Request { cookie, message, uids, done };
        if self.events.send(Event::Begin(request)).await.is_err() {
            return Err(AgentError::Cancelled("The agent is shutting down".into()));
        }
        match result.recv().await {
            Ok(true) => Ok(()),
            _ => Err(AgentError::Cancelled("Authentication was cancelled".into())),
        }
    }

    async fn cancel_authentication(&self, cookie: String) {
        let _ = self.events.send(Event::Cancel(cookie)).await;
    }
}

struct Dialog {
    window: gtk::Window,
    field: gtk::Box,
    input: gtk::Text,
    notice: gtk::Label,
}

struct Flow {
    request: Request,
    dialog: Dialog,
    username: String,
    helper_input: RefCell<Option<gio::OutputStream>>,
    helper: RefCell<Option<glib::Object>>,
    /// Bumped per helper session so stale ones stop reading
    generation: Cell<u32>,
    failed: Cell<bool>,
    prompt: RefCell<String>,
}

pub struct Polkit {
    on_open: Rc<dyn Fn()>,
    queue: RefCell<VecDeque<Request>>,
    flow: RefCell<Option<Rc<Flow>>>,
    _connection: RefCell<Option<zbus::Connection>>,
}

impl Polkit {
    pub fn new(on_open: Rc<dyn Fn()>) -> Rc<Polkit> {
        let polkit = Rc::new(Polkit {
            on_open,
            queue: RefCell::new(VecDeque::new()),
            flow: RefCell::new(None),
            _connection: RefCell::new(None),
        });
        let (sender, receiver) = async_channel::unbounded();
        let weak = Rc::downgrade(&polkit);
        glib::spawn_future_local(async move {
            match register(Agent { events: sender }).await {
                Ok(connection) => {
                    if let Some(polkit) = weak.upgrade() {
                        polkit._connection.replace(Some(connection));
                    }
                }
                Err(e) => {
                    eprintln!("wakeshell: the polkit agent is off: {e}");
                    return;
                }
            }
            while let Ok(event) = receiver.recv().await {
                let Some(polkit) = weak.upgrade() else { return };
                match event {
                    Event::Begin(request) => {
                        polkit.queue.borrow_mut().push_back(request);
                        polkit.next();
                    }
                    Event::Cancel(cookie) => polkit.cancel_cookie(&cookie),
                }
            }
        });
        polkit
    }

    pub fn active(&self) -> bool {
        self.flow.borrow().is_some()
    }

    fn next(self: &Rc<Self>) {
        if self.active() {
            return;
        }
        let Some(request) = self.queue.borrow_mut().pop_front() else { return };
        let me = unsafe { libc::getuid() };
        let uid = if request.uids.contains(&me) { Some(me) } else { request.uids.first().copied() };
        let Some(username) = uid.and_then(username) else {
            eprintln!("wakeshell: polkit asked for no user this agent can authenticate");
            let _ = request.done.try_send(false);
            return self.next();
        };
        (self.on_open)();
        let dialog = self.build_dialog(&request.message);
        let flow = Rc::new(Flow {
            request,
            dialog,
            username,
            helper_input: RefCell::new(None),
            helper: RefCell::new(None),
            generation: Cell::new(0),
            failed: Cell::new(false),
            prompt: RefCell::new(String::new()),
        });
        self.flow.replace(Some(flow.clone()));
        update(&flow);
        flow.dialog.window.present();
        start_helper(Rc::downgrade(self), flow);
    }

    fn finish(self: &Rc<Self>, success: bool) {
        if let Some(flow) = self.flow.take() {
            flow.generation.set(flow.generation.get() + 1);
            flow.helper.take();
            flow.helper_input.take();
            flow.dialog.window.destroy();
            let _ = flow.request.done.try_send(success);
        }
        self.next();
    }

    fn cancel(self: &Rc<Self>) {
        self.finish(false);
    }

    fn cancel_cookie(self: &Rc<Self>, cookie: &str) {
        let current = self.flow.borrow().as_ref().is_some_and(|f| f.request.cookie == cookie);
        if current {
            self.cancel();
        } else {
            self.queue.borrow_mut().retain(|r| {
                let cancelled = r.cookie == cookie;
                if cancelled {
                    let _ = r.done.try_send(false);
                }
                !cancelled
            });
        }
    }

    fn submit(self: &Rc<Self>) {
        let Some(flow) = self.flow.borrow().clone() else { return };
        let password = flow.dialog.input.text().to_string();
        let Some(stdin) = flow.helper_input.take() else { return };
        if password.is_empty() {
            flow.helper_input.replace(Some(stdin));
            return;
        }
        flow.dialog.input.set_text("");
        update(&flow);
        glib::spawn_future_local(async move {
            let _ = stdin.write_all_future(password + "\n", glib::Priority::DEFAULT).await;
        });
    }

    fn build_dialog(self: &Rc<Self>, message: &str) -> Dialog {
        let window = util::overlay_window("wakeshell-polkit");

        let card = gtk::Box::new(gtk::Orientation::Vertical, 0);
        card.add_css_class("card");
        card.set_halign(gtk::Align::Center);
        card.set_valign(gtk::Align::Center);
        card.set_size_request(460, -1);
        card.set_opacity(0.0);

        let header = gtk::Box::new(gtk::Orientation::Horizontal, 0);
        header.add_css_class("card-header");
        header.append(&util::label("Authentication required", &["large", "bold"]));
        card.append(&header);

        let body = gtk::Box::new(gtk::Orientation::Vertical, 12);
        body.set_margin_top(20);
        body.set_margin_bottom(20);
        body.set_margin_start(20);
        body.set_margin_end(20);
        body.append(&util::wrapping_label(message, &[]));
        let field = gtk::Box::new(gtk::Orientation::Horizontal, 0);
        field.add_css_class("field");
        let input = gtk::Text::new();
        input.set_hexpand(true);
        input.set_valign(gtk::Align::Center);
        input.set_visibility(false);
        input.set_invisible_char('•');
        field.append(&input);
        body.append(&field);
        let notice = util::wrapping_label("", &["small", "muted"]);
        notice.set_visible(false);
        body.append(&notice);
        card.append(&body);

        card.append(&util::separator());
        let buttons = gtk::Box::new(gtk::Orientation::Horizontal, 12);
        buttons.set_halign(gtk::Align::End);
        buttons.set_margin_top(14);
        buttons.set_margin_bottom(14);
        buttons.set_margin_end(20);
        let weak = Rc::downgrade(self);
        buttons.append(&util::dialog_button("Cancel", false, move || {
            if let Some(polkit) = weak.upgrade() {
                polkit.cancel();
            }
        }));
        let weak = Rc::downgrade(self);
        buttons.append(&util::dialog_button("Authenticate", true, move || {
            if let Some(polkit) = weak.upgrade() {
                polkit.submit();
            }
        }));
        card.append(&buttons);

        let shown = card.clone();
        let root = backdrop::wrap(&window, &card, move || shown.set_opacity(1.0));
        window.set_child(Some(&root));

        let weak = Rc::downgrade(self);
        util::on_click_outside(&root, &[card.clone().upcast()], move || {
            if let Some(polkit) = weak.upgrade() {
                polkit.cancel();
            }
        });
        let weak = Rc::downgrade(self);
        util::on_key(&window, move |key, _| {
            let Some(polkit) = weak.upgrade() else { return false };
            match key {
                gdk::Key::Escape => polkit.cancel(),
                gdk::Key::Return | gdk::Key::KP_Enter => polkit.submit(),
                _ => return false,
            }
            true
        });
        let weak = Rc::downgrade(self);
        input.connect_changed(move |_| {
            if let Some(flow) = weak.upgrade().and_then(|p| p.flow.borrow().clone()) {
                update(&flow);
            }
        });
        input.grab_focus();
        Dialog { window, field, input, notice }
    }
}

fn update(flow: &Flow) {
    let waiting = flow.helper_input.borrow().is_some();
    let dialog = &flow.dialog;
    let filled = !dialog.input.text().is_empty();
    let failed = flow.failed.get() || dialog.notice.has_css_class("error-text");
    for (class, on) in [("filled", filled && !failed), ("failed", failed)] {
        if on {
            dialog.field.add_css_class(class);
        } else {
            dialog.field.remove_css_class(class);
        }
    }
    let prompt = flow.prompt.borrow();
    let prompt = prompt.trim_end().trim_end_matches(':').trim_end();
    let placeholder = if !waiting {
        "Checking..."
    } else if prompt.is_empty() {
        "Password"
    } else {
        prompt
    };
    dialog.input.set_placeholder_text(Some(placeholder));
    dialog.input.set_editable(waiting);
}

fn set_notice(flow: &Flow, text: &str, error: bool) {
    let notice = &flow.dialog.notice;
    notice.set_text(text);
    notice.set_visible(!text.is_empty());
    if error {
        notice.add_css_class("error-text");
    } else {
        notice.remove_css_class("error-text");
    }
}

fn start_helper(polkit: Weak<Polkit>, flow: Rc<Flow>) {
    let generation = flow.generation.get() + 1;
    flow.generation.set(generation);
    glib::spawn_future_local(async move {
        let streams = match connect_helper(&flow.username).await {
            Ok(streams) => streams,
            Err(e) => {
                eprintln!("wakeshell: cannot start polkit-agent-helper-1: {e}");
                if let Some(polkit) = polkit.upgrade() {
                    polkit.cancel();
                }
                return;
            }
        };
        let (input, output, keep) = streams;
        flow.helper.replace(Some(keep));
        // The cookie goes on stdin so other processes cannot see it
        let _ = output.write_all_future(format!("{}\n", flow.request.cookie), glib::Priority::DEFAULT).await;
        let input = gio::DataInputStream::new(&input);
        loop {
            let line = input.read_line_utf8_future(glib::Priority::DEFAULT).await;
            if flow.generation.get() != generation {
                return;
            }
            let Some(polkit) = polkit.upgrade() else { return };
            let line = match line {
                Ok(Some(line)) => unescape(&line),
                _ => "FAILURE".to_owned(),
            };
            if let Some(prompt) =
                line.strip_prefix("PAM_PROMPT_ECHO_OFF ").or_else(|| line.strip_prefix("PAM_PROMPT_ECHO_ON "))
            {
                flow.dialog.input.set_visibility(line.starts_with("PAM_PROMPT_ECHO_ON "));
                flow.prompt.replace(prompt.to_owned());
                flow.helper_input.replace(Some(output.clone()));
                update(&flow);
                flow.dialog.input.grab_focus();
            } else if let Some(message) = line.strip_prefix("PAM_ERROR_MSG ") {
                set_notice(&flow, message, true);
                update(&flow);
            } else if let Some(message) = line.strip_prefix("PAM_TEXT_INFO ") {
                set_notice(&flow, message, false);
            } else if line.starts_with("SUCCESS") {
                polkit.finish(true);
                return;
            } else {
                // Ask again
                flow.helper.take();
                flow.helper_input.take();
                flow.failed.set(true);
                if !flow.dialog.notice.has_css_class("error-text") || flow.dialog.notice.text().is_empty() {
                    set_notice(&flow, "Wrong password, try again", true);
                }
                update(&flow);
                start_helper(Rc::downgrade(&polkit), flow.clone());
                return;
            }
        }
    });
}

async fn connect_helper(username: &str) -> Result<(gio::InputStream, gio::OutputStream, glib::Object), glib::Error> {
    if std::path::Path::new(HELPER_SOCKET).exists() {
        let address = gio::UnixSocketAddress::new(std::path::Path::new(HELPER_SOCKET));
        let connection = gio::SocketClient::new().connect_future(&address).await?;
        let output = connection.output_stream();
        output.write_all_future(format!("{username}\n"), glib::Priority::DEFAULT).await.map_err(|(_, e)| e)?;
        Ok((connection.input_stream(), output, connection.upcast()))
    } else {
        let process = gio::Subprocess::newv(
            &[HELPER.as_ref(), username.as_ref()],
            gio::SubprocessFlags::STDIN_PIPE | gio::SubprocessFlags::STDOUT_PIPE,
        )?;
        let input = process.stdout_pipe().unwrap();
        let output = process.stdin_pipe().unwrap();
        Ok((input, output, process.upcast()))
    }
}

/// Undoes g_strescape, which the helper applies to each line
fn unescape(line: &str) -> String {
    let mut out = Vec::new();
    let bytes = line.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'\\' && i + 1 < bytes.len() {
            i += 1;
            let c = bytes[i];
            match c {
                b'0'..=b'7' => {
                    let mut value = 0u32;
                    let mut n = 0;
                    while n < 3 && i < bytes.len() && (b'0'..=b'7').contains(&bytes[i]) {
                        value = value * 8 + (bytes[i] - b'0') as u32;
                        i += 1;
                        n += 1;
                    }
                    out.push(value as u8);
                    continue;
                }
                b'n' => out.push(b'\n'),
                b't' => out.push(b'\t'),
                b'r' => out.push(b'\r'),
                b'b' => out.push(8),
                b'f' => out.push(12),
                b'v' => out.push(11),
                _ => out.push(c),
            }
        } else {
            out.push(bytes[i]);
        }
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

fn username(uid: u32) -> Option<String> {
    unsafe {
        let passwd = libc::getpwuid(uid);
        if passwd.is_null() {
            return None;
        }
        Some(std::ffi::CStr::from_ptr((*passwd).pw_name).to_string_lossy().into_owned())
    }
}

async fn register(agent: Agent) -> zbus::Result<zbus::Connection> {
    let connection = zbus::Connection::system().await?;
    connection.object_server().at(AGENT_PATH, agent).await?;
    // A systemd user service is outside any session, so fall back to the graphical one
    let reply = connection
        .call_method(
            Some("org.freedesktop.login1"),
            "/org/freedesktop/login1/session/auto",
            Some("org.freedesktop.DBus.Properties"),
            "Get",
            &("org.freedesktop.login1.Session", "Id"),
        )
        .await?;
    let session: OwnedValue = reply.body().deserialize()?;
    let session = String::try_from(session)?;
    let mut subject = ("unix-session", HashMap::from([("session-id", Value::from(session))]));
    // Testing only: serve one process, like `pkttyagent --process`
    if let Some(pid) = std::env::var("WAKESHELL_POLKIT_PID").ok().and_then(|p| p.parse::<u32>().ok()) {
        let start_time: u64 = std::fs::read_to_string(format!("/proc/{pid}/stat"))
            .ok()
            .and_then(|s| s.rsplit_once(')')?.1.split_whitespace().nth(19)?.parse().ok())
            .unwrap_or(0);
        subject = (
            "unix-process",
            HashMap::from([("pid", Value::from(pid)), ("start-time", Value::from(start_time))]),
        );
    }
    let locale = std::env::var("LANG").unwrap_or_else(|_| "C".into());
    connection
        .call_method(
            Some("org.freedesktop.PolicyKit1"),
            "/org/freedesktop/PolicyKit1/Authority",
            Some("org.freedesktop.PolicyKit1.Authority"),
            "RegisterAuthenticationAgent",
            &(subject, locale, AGENT_PATH),
        )
        .await?;
    Ok(connection)
}
