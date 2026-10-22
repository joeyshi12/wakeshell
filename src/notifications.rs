// Notification daemon (org.freedesktop.Notifications).
use std::{
    cell::{Cell, RefCell},
    collections::HashMap,
    rc::Rc,
    sync::atomic::{AtomicU32, Ordering},
    time::Duration,
};

use gtk::{gdk, gio, glib, prelude::*};
use gtk4_layer_shell::{Edge, KeyboardMode, Layer, LayerShell};
use zbus::zvariant::OwnedValue;

use crate::util;

const PATH: &str = "/org/freedesktop/Notifications";
const INTERFACE: &str = "org.freedesktop.Notifications";
const MAX_SHOWN: usize = 5;
const WIDTH: i32 = 360;
const SHADOW: i32 = 5;

const LOW: u8 = 0;
const CRITICAL: u8 = 2;

// Reasons in NotificationClosed
const EXPIRED: u32 = 1;
const DISMISSED: u32 = 2;
const CLOSED: u32 = 3;

struct ImageData {
    width: i32,
    height: i32,
    rowstride: i32,
    has_alpha: bool,
    data: Vec<u8>,
}

struct Notification {
    id: u32,
    app_name: String,
    app_icon: String,
    summary: String,
    body: String,
    actions: Vec<(String, String)>,
    urgency: u8,
    expire_timeout: i32,
    resident: bool,
    image_data: Option<ImageData>,
    image_path: Option<String>,
}

enum Event {
    Notify(Notification),
    Close(u32),
}

struct Server {
    events: async_channel::Sender<Event>,
    next_id: AtomicU32,
}

#[zbus::interface(name = "org.freedesktop.Notifications")]
impl Server {
    fn get_capabilities(&self) -> Vec<&'static str> {
        vec!["actions", "body", "body-markup", "icon-static", "persistence"]
    }

    fn get_server_information(&self) -> (&'static str, &'static str, &'static str, &'static str) {
        ("wakeshell", "joeyshi12", env!("CARGO_PKG_VERSION"), "1.2")
    }

    #[allow(clippy::too_many_arguments)]
    async fn notify(
        &self,
        app_name: String,
        replaces_id: u32,
        app_icon: String,
        summary: String,
        body: String,
        actions: Vec<String>,
        hints: HashMap<String, OwnedValue>,
        expire_timeout: i32,
    ) -> u32 {
        let id = if replaces_id != 0 { replaces_id } else { self.next_id.fetch_add(1, Ordering::Relaxed) };
        let hint = |name: &str| hints.get(name).and_then(|v| v.try_clone().ok());
        let urgency = hint("urgency").and_then(|v| u8::try_from(v).ok()).unwrap_or(1);
        let resident = hint("resident").and_then(|v| bool::try_from(v).ok()).unwrap_or(false);
        let image_data = ["image-data", "image_data", "icon_data"]
            .iter()
            .find_map(|name| hint(name))
            .and_then(|v| <(i32, i32, i32, bool, i32, i32, Vec<u8>)>::try_from(v).ok())
            .map(|(width, height, rowstride, has_alpha, _, _, data)| ImageData {
                width,
                height,
                rowstride,
                has_alpha,
                data,
            });
        let image_path = ["image-path", "image_path"]
            .iter()
            .find_map(|name| hint(name))
            .and_then(|v| String::try_from(v).ok())
            .filter(|s| !s.is_empty());
        let actions = actions.chunks(2).filter(|c| c.len() == 2).map(|c| (c[0].clone(), c[1].clone())).collect();
        let _ = self
            .events
            .send(Event::Notify(Notification {
                id,
                app_name,
                app_icon,
                summary,
                body,
                actions,
                urgency,
                expire_timeout,
                resident,
                image_data,
                image_path,
            }))
            .await;
        id
    }

    async fn close_notification(&self, id: u32) {
        let _ = self.events.send(Event::Close(id)).await;
    }
}

struct Popup {
    notification: Notification,
    widget: gtk::Box,
    timer: RefCell<Option<glib::SourceId>>,
    hovered: Cell<bool>,
}

pub struct Notifications {
    window: gtk::Window,
    column: gtk::Box,
    more: gtk::Label,
    popups: RefCell<Vec<Rc<Popup>>>,
    connection: RefCell<Option<zbus::Connection>>,
}

impl Notifications {
    pub fn new() -> Rc<Notifications> {
        let window = util::layer_window(util::LayerOptions {
            namespace: "wakeshell-notifications",
            layer: Layer::Overlay,
            anchors: [true, false, false, true],
            keyboard: KeyboardMode::None,
            monitor: None,
            exclusive_zone: 0,
        });
        window.set_margin(Edge::Top, 12);
        window.set_margin(Edge::Right, 12);
        let column = gtk::Box::new(gtk::Orientation::Vertical, 10);
        column.set_size_request(WIDTH, -1);
        column.set_margin_end(SHADOW);
        column.set_margin_bottom(SHADOW);
        let more = util::label("", &["notification-more", "small"]);
        more.set_halign(gtk::Align::End);
        window.set_child(Some(&column));

        let notifications = Rc::new(Notifications {
            window,
            column,
            more,
            popups: RefCell::new(Vec::new()),
            connection: RefCell::new(None),
        });

        let (sender, receiver) = async_channel::unbounded();
        let server = Server { events: sender, next_id: AtomicU32::new(1) };
        let weak = Rc::downgrade(&notifications);
        glib::spawn_future_local(async move {
            let connection = async {
                zbus::connection::Builder::session()?
                    .name(INTERFACE)?
                    .serve_at(PATH, server)?
                    .build()
                    .await
            }
            .await;
            match connection {
                Ok(connection) => {
                    if let Some(n) = weak.upgrade() {
                        n.connection.replace(Some(connection));
                    }
                }
                Err(e) => {
                    eprintln!("wakeshell: notifications are off, cannot own {INTERFACE}: {e}");
                    return;
                }
            }
            while let Ok(event) = receiver.recv().await {
                let Some(n) = weak.upgrade() else { return };
                match event {
                    Event::Notify(notification) => n.add(notification),
                    Event::Close(id) => n.close(id, CLOSED),
                }
            }
        });
        notifications
    }

    fn emit(&self, member: &'static str, body: impl serde::Serialize + zbus::zvariant::DynamicType + 'static) {
        let Some(connection) = self.connection.borrow().clone() else { return };
        glib::spawn_future_local(async move {
            let _ = connection.emit_signal(None::<&str>, PATH, INTERFACE, member, &body).await;
        });
    }

    fn add(self: &Rc<Self>, notification: Notification) {
        let id = notification.id;
        let popup = Rc::new(Popup {
            widget: self.build_popup(&notification),
            notification,
            timer: RefCell::new(None),
            hovered: Cell::new(false),
        });
        let mut popups = self.popups.borrow_mut();
        match popups.iter().position(|p| p.notification.id == id) {
            Some(i) => {
                if let Some(timer) = popups[i].timer.take() {
                    timer.remove();
                }
                popups[i] = popup;
            }
            None => popups.push(popup),
        }
        drop(popups);
        self.refresh();
    }

    fn close(self: &Rc<Self>, id: u32, reason: u32) {
        let removed = {
            let mut popups = self.popups.borrow_mut();
            popups.iter().position(|p| p.notification.id == id).map(|i| popups.remove(i))
        };
        if let Some(popup) = removed {
            if let Some(timer) = popup.timer.take() {
                timer.remove();
            }
            self.emit("NotificationClosed", (id, reason));
            self.refresh();
        }
    }

    fn invoke(self: &Rc<Self>, id: u32, action: &str) {
        self.emit("ActionInvoked", (id, action.to_owned()));
        let resident = self.popups.borrow().iter().any(|p| p.notification.id == id && p.notification.resident);
        if !resident {
            self.close(id, DISMISSED);
        }
    }

    fn refresh(self: &Rc<Self>) {
        while let Some(child) = self.column.first_child() {
            self.column.remove(&child);
        }
        let popups = self.popups.borrow();
        for (i, popup) in popups.iter().rev().enumerate() {
            if i < MAX_SHOWN {
                self.column.append(&popup.widget);
                if !popup.hovered.get() {
                    self.start_timer(popup);
                }
            } else if let Some(timer) = popup.timer.take() {
                timer.remove();
            }
        }
        let hidden = popups.len().saturating_sub(MAX_SHOWN);
        if hidden > 0 {
            self.more.set_text(&format!("{hidden} more"));
            self.column.append(&self.more);
        }
        let any = !popups.is_empty();
        drop(popups);
        if any {
            // Hiding and showing again makes the window shrink to fit what is left
            self.window.set_visible(false);
            self.window.present();
        } else {
            self.window.set_visible(false);
        }
    }

    fn start_timer(self: &Rc<Self>, popup: &Rc<Popup>) {
        if popup.timer.borrow().is_some() {
            return;
        }
        let n = &popup.notification;
        let timeout = if n.urgency == CRITICAL {
            return;
        } else if n.expire_timeout > 0 {
            n.expire_timeout as u64
        } else if n.urgency == LOW {
            5000
        } else {
            10000
        };
        let weak = Rc::downgrade(self);
        let id = n.id;
        let weak_popup = Rc::downgrade(popup);
        let source = glib::timeout_add_local_once(Duration::from_millis(timeout), move || {
            if let Some(popup) = weak_popup.upgrade() {
                popup.timer.take();
            }
            if let Some(n) = weak.upgrade() {
                n.close(id, EXPIRED);
            }
        });
        popup.timer.replace(Some(source));
    }

    fn build_popup(self: &Rc<Self>, n: &Notification) -> gtk::Box {
        let card = gtk::Box::new(gtk::Orientation::Horizontal, 12);
        card.add_css_class("card");
        card.add_css_class("notification");
        match n.urgency {
            LOW => card.add_css_class("low"),
            CRITICAL => card.add_css_class("critical"),
            _ => {}
        }
        util::hand_cursor(&card);

        if let Some(image) = image(n) {
            image.set_pixel_size(24);
            image.set_valign(gtk::Align::Start);
            image.set_margin_top(2);
            card.append(&image);
        }

        let text = gtk::Box::new(gtk::Orientation::Vertical, 4);
        text.set_hexpand(true);
        let line = |s: &str, classes: &[&str]| {
            let label = util::label(s, classes);
            label.set_xalign(0.0);
            label.set_ellipsize(gtk::pango::EllipsizeMode::End);
            label.set_max_width_chars(1);
            label.set_hexpand(true);
            label
        };
        if !n.app_name.is_empty() {
            text.append(&line(&n.app_name, &["app-name", "small", "bold"]));
        }
        if !n.summary.is_empty() {
            text.append(&line(&n.summary, &["bold"]));
        }
        if !n.body.is_empty() {
            let body = util::wrapping_label("", &[]);
            body.set_lines(6);
            body.set_ellipsize(gtk::pango::EllipsizeMode::End);
            match markup(&n.body) {
                Some(markup) => body.set_markup(&markup),
                None => body.set_text(&strip_tags(&n.body)),
            }
            text.append(&body);
        }
        let buttons: Vec<_> = n.actions.iter().filter(|(key, _)| key != "default").cloned().collect();
        if !buttons.is_empty() {
            let row = gtk::FlowBox::new();
            row.set_selection_mode(gtk::SelectionMode::None);
            row.set_column_spacing(8);
            row.set_row_spacing(8);
            row.set_margin_top(6);
            row.set_max_children_per_line(8);
            for (key, label) in buttons {
                let button = util::label(&label, &["notification-button", "small"]);
                util::hand_cursor(&button);
                let weak = Rc::downgrade(self);
                let id = n.id;
                let click = gtk::GestureClick::new();
                // Claim the click so the popup's own click does not also run
                click.connect_pressed(|gesture, _, _, _| {
                    gesture.set_state(gtk::EventSequenceState::Claimed);
                });
                click.connect_released(move |_, _, _, _| {
                    if let Some(n) = weak.upgrade() {
                        n.invoke(id, &key);
                    }
                });
                button.add_controller(click);
                row.insert(&button, -1);
            }
            text.append(&row);
        }
        card.append(&text);

        let id = n.id;
        let has_default = n.actions.iter().any(|(key, _)| key == "default");
        let weak = Rc::downgrade(self);
        util::on_click(&card, move |button| {
            let Some(n) = weak.upgrade() else { return };
            match button {
                3 => {
                    let ids: Vec<u32> = n.popups.borrow().iter().map(|p| p.notification.id).collect();
                    for id in ids {
                        n.close(id, DISMISSED);
                    }
                }
                1 if has_default => n.invoke(id, "default"),
                1 => n.close(id, DISMISSED),
                _ => {}
            }
        });

        let hover = gtk::EventControllerMotion::new();
        let weak = Rc::downgrade(self);
        hover.connect_enter(move |_, _, _| {
            let Some(n) = weak.upgrade() else { return };
            if let Some(popup) = n.find(id) {
                popup.hovered.set(true);
                if let Some(timer) = popup.timer.take() {
                    timer.remove();
                }
            }
        });
        let weak = Rc::downgrade(self);
        hover.connect_leave(move |_| {
            let Some(n) = weak.upgrade() else { return };
            if let Some(popup) = n.find(id) {
                popup.hovered.set(false);
                n.start_timer(&popup);
            }
        });
        card.add_controller(hover);
        card
    }

    fn find(&self, id: u32) -> Option<Rc<Popup>> {
        self.popups.borrow().iter().find(|p| p.notification.id == id).cloned()
    }
}

fn image(n: &Notification) -> Option<gtk::Image> {
    if let Some(data) = &n.image_data {
        let (format, channels) =
            if data.has_alpha { (gdk::MemoryFormat::R8g8b8a8, 4) } else { (gdk::MemoryFormat::R8g8b8, 3) };
        let needed = data.rowstride as usize * (data.height.max(1) as usize - 1) + data.width as usize * channels;
        if data.width > 0 && data.height > 0 && data.data.len() >= needed {
            let bytes = glib::Bytes::from(&data.data[..]);
            let texture = gdk::MemoryTexture::new(data.width, data.height, format, &bytes, data.rowstride as usize);
            return Some(gtk::Image::from_paintable(Some(&texture)));
        }
    }
    let source = n.image_path.as_deref().unwrap_or(&n.app_icon);
    if source.is_empty() {
        return None;
    }
    let path = source.strip_prefix("file://").unwrap_or(source);
    if path.starts_with('/') {
        let file = gio::File::for_path(path);
        return gdk::Texture::from_file(&file).ok().map(|t| gtk::Image::from_paintable(Some(&t)));
    }
    let theme = gtk::IconTheme::for_display(&gdk::Display::default()?);
    theme.has_icon(path).then(|| gtk::Image::from_icon_name(path))
}

/// Keeps <b>, <i> and <u>, turns <br> into a newline and drops other tags
fn markup(body: &str) -> Option<String> {
    let mut out = String::new();
    let mut rest = body;
    while let Some(start) = rest.find('<') {
        out.push_str(&rest[..start]);
        let Some(end) = rest[start..].find('>') else {
            out.push_str(&rest[start..].replace('<', "&lt;"));
            rest = "";
            break;
        };
        let tag = &rest[start + 1..start + end];
        let name = tag.trim_start_matches('/').split(|c: char| c.is_whitespace() || c == '/').next().unwrap_or("");
        match name.to_ascii_lowercase().as_str() {
            "b" | "i" | "u" => {
                out.push('<');
                out.push_str(&tag.to_ascii_lowercase());
                out.push('>');
            }
            "br" => out.push('\n'),
            _ => {}
        }
        rest = &rest[start + end + 1..];
    }
    out.push_str(rest);
    gtk::pango::parse_markup(&out, '\0').is_ok().then_some(out)
}

fn strip_tags(body: &str) -> String {
    let mut out = String::new();
    let mut in_tag = false;
    for c in body.chars() {
        match c {
            '<' => in_tag = true,
            '>' if in_tag => in_tag = false,
            _ if !in_tag => out.push(c),
            _ => {}
        }
    }
    out
}
