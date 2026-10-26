// Polkit authentication agent. Passwords go to polkit-agent-helper-1 through
// /run/polkit/agent-helper.socket, or the setuid helper where there is no socket.
use std::{
    cell::Cell,
    collections::{HashMap, VecDeque},
    rc::Rc,
};

use gtk::{gdk, gio, glib, prelude::*};
use relm4::{RelmWidgetExt, prelude::*};
use zbus::zvariant::{OwnedValue, Value};

use crate::{
    anim::Fade,
    backdrop,
    repaint::RepaintBox,
    util::{self, Events},
};

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

#[derive(Debug)]
pub struct Request {
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

struct Flow {
    request: Request,
    username: String,
    /// Keeps the helper's socket or process
    helper: Option<glib::Object>,
    helper_input: Option<gio::OutputStream>,
    waiting: bool,
    failed: bool,
    prompt: String,
    echo: bool,
}

pub struct Polkit {
    queue: VecDeque<Request>,
    flow: Option<Flow>,
    /// Bumped per helper session so stale ones stop reading
    generation: Rc<Cell<u32>>,
    password: gtk::EntryBuffer,
    filled: bool,
    notice: String,
    notice_error: bool,
    focus: Cell<bool>,
    _connection: Option<zbus::Connection>,
    fade: Option<Fade>,
    /// Closes at once after authenticating; cancelling fades out
    instant: bool,
}

#[derive(Debug)]
pub enum PolkitMsg {
    Connected(zbus::Connection),
    Begin(Request),
    CancelCookie(String),
    Cancel,
    Submit,
    Changed,
    Helper(u32, Helper),
}

#[derive(Debug)]
pub enum Helper {
    Started { keep: glib::Object, input: gio::OutputStream },
    Line(String),
    Failed(glib::Error),
}

#[derive(Debug)]
pub struct Shown(pub bool);

impl Polkit {
    fn placeholder(&self) -> String {
        let Some(flow) = &self.flow else { return String::new() };
        let prompt = flow.prompt.trim_end().trim_end_matches(':').trim_end();
        if !flow.waiting {
            "Checking...".into()
        } else if prompt.is_empty() {
            "Password".into()
        } else {
            prompt.into()
        }
    }

    fn failed(&self) -> bool {
        self.flow.as_ref().is_some_and(|f| f.failed) || self.notice_error
    }
}

#[relm4::component(pub)]
impl SimpleComponent for Polkit {
    type Init = ();
    type Input = PolkitMsg;
    type Output = Shown;

    view! {
        util::overlay_window("wakeshell-polkit") -> gtk::Window {
            set_visible: false,
            on_key[sender] => move |key, _| {
                let msg = match key {
                    gdk::Key::Escape => PolkitMsg::Cancel,
                    gdk::Key::Return | gdk::Key::KP_Enter => PolkitMsg::Submit,
                    _ => return false,
                };
                sender.input(msg);
                true
            },

            #[name = "backdrop"]
            #[wrap(Some)]
            set_child = &backdrop::new() -> gtk::Overlay {
                #[name = "card"]
                add_overlay = &RepaintBox {
                    set_orientation: gtk::Orientation::Vertical,
                    add_css_class: "card",
                    set_halign: gtk::Align::Center,
                    set_valign: gtk::Align::Center,
                    set_size_request: (460, -1),

                    gtk::Box {
                        add_css_class: "card-header",
                        gtk::Label {
                            set_label: "Authentication required",
                            set_css_classes: &["large", "bold"],
                        },
                    },

                    gtk::Box {
                        set_orientation: gtk::Orientation::Vertical,
                        set_spacing: 12,
                        set_margin_all: 20,

                        gtk::Label {
                            set_wrap: true,
                            set_wrap_mode: gtk::pango::WrapMode::WordChar,
                            set_xalign: 0.0,
                            set_max_width_chars: 1,
                            set_hexpand: true,
                            #[watch]
                            set_label: model.flow.as_ref().map_or("", |f| &f.request.message),
                        },
                        gtk::Box {
                            add_css_class: "field",
                            #[watch]
                            set_class_active: ("filled", model.filled && !model.failed()),
                            #[watch]
                            set_class_active: ("failed", model.failed()),

                            #[name = "input"]
                            gtk::Text {
                                set_buffer: &model.password,
                                set_hexpand: true,
                                set_valign: gtk::Align::Center,
                                set_invisible_char: '•',
                                connect_changed => PolkitMsg::Changed,
                                #[watch]
                                set_visibility: model.flow.as_ref().is_some_and(|f| f.echo),
                                #[watch]
                                set_placeholder_text: Some(&model.placeholder()),
                                #[watch]
                                set_editable: model.flow.as_ref().is_some_and(|f| f.waiting),
                            },
                        },
                        gtk::Label {
                            set_wrap: true,
                            set_wrap_mode: gtk::pango::WrapMode::WordChar,
                            set_xalign: 0.0,
                            set_max_width_chars: 1,
                            set_hexpand: true,
                            set_css_classes: &["small", "muted"],
                            #[watch]
                            set_label: &model.notice,
                            #[watch]
                            set_visible: !model.notice.is_empty(),
                            #[watch]
                            set_class_active: ("error-text", model.notice_error),
                        },
                    },

                    gtk::Box {
                        add_css_class: "separator-line",
                    },
                    gtk::Box {
                        set_spacing: 12,
                        set_halign: gtk::Align::End,
                        set_margin_top: 14,
                        set_margin_bottom: 14,
                        set_margin_end: 20,

                        gtk::Label {
                            set_label: "Cancel",
                            add_css_class: "dialog-button",
                            set_cursor_from_name: Some("pointer"),
                            on_left_click[sender] => move || sender.input(PolkitMsg::Cancel),
                        },
                        gtk::Label {
                            set_label: "Authenticate",
                            set_css_classes: &["dialog-button", "primary"],
                            set_cursor_from_name: Some("pointer"),
                            on_left_click[sender] => move || sender.input(PolkitMsg::Submit),
                        },
                    },
                },
            },

        }
    }

    fn init(_: (), root: Self::Root, sender: ComponentSender<Self>) -> ComponentParts<Self> {
        let mut model = Polkit {
            queue: VecDeque::new(),
            flow: None,
            generation: Rc::new(Cell::new(0)),
            password: gtk::EntryBuffer::default(),
            filled: false,
            notice: String::new(),
            notice_error: false,
            focus: Cell::new(false),
            _connection: None,
            fade: None,
            instant: false,
        };
        let widgets = view_output!();
        let cancel = sender.clone();
        widgets.backdrop.on_click_outside(&[widgets.card.clone().upcast()], move || cancel.input(PolkitMsg::Cancel));
        model.fade = Some(Fade::new(&root, &widgets.backdrop, false, 80));

        let (events, receiver) = async_channel::unbounded();
        glib::spawn_future_local(async move {
            match register(Agent { events }).await {
                Ok(connection) => sender.input(PolkitMsg::Connected(connection)),
                Err(e) => {
                    eprintln!("wakeshell: the polkit agent is off: {e}");
                    return;
                }
            }
            while let Ok(event) = receiver.recv().await {
                sender.input(match event {
                    Event::Begin(request) => PolkitMsg::Begin(request),
                    Event::Cancel(cookie) => PolkitMsg::CancelCookie(cookie),
                });
            }
        });
        ComponentParts { model, widgets }
    }

    fn post_view() {
        if model.focus.take() {
            input.grab_focus();
        }
    }

    fn update(&mut self, msg: PolkitMsg, sender: ComponentSender<Self>) {
        let was_active = self.flow.is_some();
        match msg {
            PolkitMsg::Connected(connection) => self._connection = Some(connection),
            PolkitMsg::Begin(request) => {
                self.queue.push_back(request);
                self.next(&sender);
            }
            PolkitMsg::CancelCookie(cookie) => {
                if self.flow.as_ref().is_some_and(|f| f.request.cookie == cookie) {
                    self.finish(false, &sender);
                } else {
                    self.queue.retain(|r| {
                        let cancelled = r.cookie == cookie;
                        if cancelled {
                            let _ = r.done.try_send(false);
                        }
                        !cancelled
                    });
                }
            }
            PolkitMsg::Cancel => self.finish(false, &sender),
            PolkitMsg::Submit => self.submit(),
            PolkitMsg::Changed => self.filled = !self.password.text().is_empty(),
            PolkitMsg::Helper(generation, event) if generation == self.generation.get() => {
                self.helper_event(event, &sender)
            }
            PolkitMsg::Helper(..) => {}
        }
        if self.flow.is_some() != was_active {
            if let Some(fade) = &self.fade {
                match (self.flow.is_some(), std::mem::take(&mut self.instant)) {
                    (true, _) => fade.show(),
                    (false, true) => fade.hide_now(),
                    (false, false) => fade.hide(),
                }
            }
            let _ = sender.output(Shown(self.flow.is_some()));
        }
    }
}

impl Polkit {
    fn next(&mut self, sender: &ComponentSender<Self>) {
        if self.flow.is_some() {
            return;
        }
        while let Some(request) = self.queue.pop_front() {
            let me = unsafe { libc::getuid() };
            let uid = if request.uids.contains(&me) { Some(me) } else { request.uids.first().copied() };
            let Some(username) = uid.and_then(username) else {
                eprintln!("wakeshell: polkit asked for no user this agent can authenticate");
                let _ = request.done.try_send(false);
                continue;
            };
            // The next dialog stays up, so its own close decides how it goes
            self.instant = false;
            self.flow = Some(Flow {
                request,
                username,
                helper: None,
                helper_input: None,
                waiting: false,
                failed: false,
                prompt: String::new(),
                echo: false,
            });
            self.password.set_text("");
            self.notice.clear();
            self.notice_error = false;
            self.focus.set(true);
            self.start_helper(sender);
            return;
        }
    }

    fn finish(&mut self, success: bool, sender: &ComponentSender<Self>) {
        if let Some(flow) = self.flow.take() {
            self.instant = success;
            self.generation.set(self.generation.get() + 1);
            let _ = flow.request.done.try_send(success);
        }
        self.next(sender);
    }

    fn submit(&mut self) {
        let Some(flow) = &mut self.flow else { return };
        let password = self.password.text().to_string();
        if !flow.waiting || password.is_empty() {
            return;
        }
        let Some(stdin) = flow.helper_input.clone() else { return };
        flow.waiting = false;
        self.password.set_text("");
        glib::spawn_future_local(async move {
            let _ = stdin.write_all_future(password + "\n", glib::Priority::DEFAULT).await;
        });
    }

    fn set_notice(&mut self, text: &str, error: bool) {
        self.notice = text.to_owned();
        self.notice_error = error;
    }

    fn helper_event(&mut self, event: Helper, sender: &ComponentSender<Self>) {
        let Some(flow) = &mut self.flow else { return };
        let line = match event {
            Helper::Started { keep, input } => {
                flow.helper = Some(keep);
                flow.helper_input = Some(input);
                return;
            }
            Helper::Failed(e) => {
                eprintln!("wakeshell: cannot start polkit-agent-helper-1: {e}");
                return self.finish(false, sender);
            }
            Helper::Line(line) => line,
        };
        if let Some(prompt) =
            line.strip_prefix("PAM_PROMPT_ECHO_OFF ").or_else(|| line.strip_prefix("PAM_PROMPT_ECHO_ON "))
        {
            flow.echo = line.starts_with("PAM_PROMPT_ECHO_ON ");
            flow.prompt = prompt.to_owned();
            flow.waiting = true;
            self.focus.set(true);
        } else if let Some(message) = line.strip_prefix("PAM_ERROR_MSG ") {
            self.set_notice(message, true);
        } else if let Some(message) = line.strip_prefix("PAM_TEXT_INFO ") {
            self.set_notice(message, false);
        } else if line.starts_with("SUCCESS") {
            self.finish(true, sender);
        } else {
            // Ask again
            flow.helper = None;
            flow.helper_input = None;
            flow.waiting = false;
            flow.failed = true;
            if !self.notice_error || self.notice.is_empty() {
                self.set_notice("Wrong password, try again", true);
            }
            self.start_helper(sender);
        }
    }

    fn start_helper(&mut self, sender: &ComponentSender<Self>) {
        let Some(flow) = &self.flow else { return };
        let generation = self.generation.get() + 1;
        self.generation.set(generation);
        let latest = self.generation.clone();
        let (username, cookie) = (flow.username.clone(), flow.request.cookie.clone());
        let sender = sender.input_sender().clone();
        glib::spawn_future_local(async move {
            let send = |event| sender.send(PolkitMsg::Helper(generation, event)).is_ok();
            let (input, output, keep) = match connect_helper(&username).await {
                Ok(streams) => streams,
                Err(e) => {
                    send(Helper::Failed(e));
                    return;
                }
            };
            send(Helper::Started { keep, input: output.clone() });
            // The cookie goes on stdin so other processes cannot see it
            let _ = output.write_all_future(format!("{cookie}\n"), glib::Priority::DEFAULT).await;
            let input = gio::DataInputStream::new(&input);
            loop {
                let line = input.read_line_utf8_future(glib::Priority::DEFAULT).await;
                if latest.get() != generation {
                    return;
                }
                let line = match line {
                    Ok(Some(line)) => unescape(&line),
                    _ => "FAILURE".to_owned(),
                };
                let done = !line.starts_with("PAM_");
                if !send(Helper::Line(line)) || done {
                    return;
                }
            }
        });
    }
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
