// Notification daemon (org.freedesktop.Notifications).
use std::{
    collections::{HashMap, HashSet},
    sync::atomic::{AtomicU32, Ordering},
    time::Duration,
};

use gtk::{gdk, gio, glib, prelude::*};
use gtk4_layer_shell::{Edge, KeyboardMode, Layer, LayerShell};
use relm4::{RelmWidgetExt, factory::FactoryVecDeque, prelude::*};
use zbus::zvariant::OwnedValue;

use crate::{
    anim,
    util::{self, Events},
};

const PATH: &str = "/org/freedesktop/Notifications";
const INTERFACE: &str = "org.freedesktop.Notifications";
const MAX_SHOWN: usize = 5;
const WIDTH: i32 = 360;
const SHADOW: i32 = 5;
const GAP: i32 = 10;

// In step with the .notification.leaving transition in style.css
const LEAVE_MS: u64 = 100;
const COLLAPSE_MS: u64 = 120;

const LOW: u8 = 0;
const CRITICAL: u8 = 2;

// Reasons in NotificationClosed
const EXPIRED: u32 = 1;
const DISMISSED: u32 = 2;
const CLOSED: u32 = 3;

#[derive(Debug)]
struct ImageData {
    width: i32,
    height: i32,
    rowstride: i32,
    has_alpha: bool,
    data: Vec<u8>,
}

#[derive(Debug)]
pub struct Notification {
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

pub struct Notifications {
    /// Newest first
    popups: FactoryVecDeque<Popup>,
    timers: HashMap<u32, glib::SourceId>,
    hovered: HashSet<u32>,
    hidden: usize,
    /// For the next popup
    serial: u64,
    connection: Option<zbus::Connection>,
}

#[derive(Debug)]
pub enum NotificationsMsg {
    Connected(zbus::Connection),
    Notify(Notification),
    Close(u32, u32),
    Expired(u32),
    Popup(PopupEvent),
    /// A closed popup has faded out: close the gap it leaves. By popup serial, since a
    /// replacement can share the notification id with the popup that is leaving.
    Collapse(u64),
    Remove(u64),
}

#[derive(Debug)]
/// By popup serial
pub enum PopupEvent {
    Clicked(u64, u32),
    Action(u64, String),
    Hover(u64, bool),
}

#[relm4::component(pub)]
impl Component for Notifications {
    type Init = ();
    type Input = NotificationsMsg;
    type Output = ();
    type CommandOutput = ();

    view! {
        util::layer_window(util::LayerOptions {
            namespace: "wakeshell-notifications",
            layer: Layer::Overlay,
            anchors: [true, false, false, true],
            keyboard: KeyboardMode::None,
            monitor: None,
            exclusive_zone: 0,
        }) -> gtk::Window {
            set_margin: (Edge::Top, 12),
            set_margin: (Edge::Right, 12),
            set_visible: false,

            // Each popup brings its own gap and shadow room, so the gap collapses with it
            gtk::Box {
                set_orientation: gtk::Orientation::Vertical,
                set_size_request: (WIDTH + SHADOW, -1),

                #[local_ref]
                popups -> gtk::Box {
                    set_orientation: gtk::Orientation::Vertical,
                },
                gtk::Label {
                    set_css_classes: &["notification-more", "small"],
                    set_halign: gtk::Align::End,
                    set_margin_end: SHADOW,
                    set_margin_bottom: SHADOW,
                    #[watch]
                    set_visible: model.hidden > 0,
                    #[watch]
                    set_label: &format!("{} more", model.hidden),
                },
            },
        }
    }

    fn init(_: (), root: Self::Root, sender: ComponentSender<Self>) -> ComponentParts<Self> {
        let model = Notifications {
            popups: FactoryVecDeque::builder()
                .launch(gtk::Box::default())
                .forward(sender.input_sender(), NotificationsMsg::Popup),
            timers: HashMap::new(),
            hovered: HashSet::new(),
            hidden: 0,
            serial: 0,
            connection: None,
        };
        let popups = model.popups.widget();
        let widgets = view_output!();

        let (events, receiver) = async_channel::unbounded();
        let server = Server { events, next_id: AtomicU32::new(1) };
        glib::spawn_future_local(async move {
            let connection = async {
                zbus::connection::Builder::session()?.name(INTERFACE)?.serve_at(PATH, server)?.build().await
            }
            .await;
            match connection {
                Ok(connection) => sender.input(NotificationsMsg::Connected(connection)),
                Err(e) => {
                    eprintln!("wakeshell: notifications are off, cannot own {INTERFACE}: {e}");
                    return;
                }
            }
            while let Ok(event) = receiver.recv().await {
                sender.input(match event {
                    Event::Notify(notification) => NotificationsMsg::Notify(notification),
                    Event::Close(id) => NotificationsMsg::Close(id, CLOSED),
                });
            }
        });
        ComponentParts { model, widgets }
    }

    fn update(&mut self, msg: NotificationsMsg, sender: ComponentSender<Self>, root: &Self::Root) {
        match msg {
            NotificationsMsg::Connected(connection) => self.connection = Some(connection),
            NotificationsMsg::Notify(notification) => {
                let id = notification.id;
                let position = self.popups.iter().position(|p| p.notification.id == id && !p.leaving);
                self.serial += 1;
                let mut popups = self.popups.guard();
                match position {
                    Some(i) => {
                        if let Some(timer) = self.timers.remove(&id) {
                            timer.remove();
                        }
                        self.hovered.remove(&id);
                        popups.remove(i);
                        popups.insert(i, (self.serial, notification));
                    }
                    None => {
                        popups.push_front((self.serial, notification));
                    }
                }
                drop(popups);
                // A replacement can be smaller; a new popup only grows the window
                self.refresh(&sender, root, position.is_some());
            }
            NotificationsMsg::Close(id, reason) => self.close(id, reason, &sender, root),
            NotificationsMsg::Collapse(serial) => {
                if let Some(popup) = self.popups.guard().iter_mut().find(|p| p.serial == serial) {
                    popup.revealed = false;
                }
                let sender = sender.clone();
                anim::after(COLLAPSE_MS, move || sender.input(NotificationsMsg::Remove(serial)));
            }
            NotificationsMsg::Remove(serial) => {
                let position = self.popups.iter().position(|p| p.serial == serial);
                if let Some(i) = position {
                    self.popups.guard().remove(i);
                    // The last card to leave shrinks the window, so it never cuts another short
                    let leaving = self.popups.iter().any(|p| p.leaving);
                    self.refresh(&sender, root, !leaving);
                }
            }
            NotificationsMsg::Expired(id) => {
                // The timer is gone once it fires
                self.timers.remove(&id);
                self.close(id, EXPIRED, &sender, root);
            }
            // Events from a popup that is leaving are too late
            NotificationsMsg::Popup(event) => {
                let serial = match &event {
                    PopupEvent::Clicked(serial, _) | PopupEvent::Action(serial, _) | PopupEvent::Hover(serial, _) => *serial,
                };
                let Some(id) = self.popups.iter().find(|p| p.serial == serial && !p.leaving).map(|p| p.notification.id) else {
                    return;
                };
                self.popup_event(id, event, &sender, root);
            }
        }
    }
}

impl Notifications {
    fn popup_event(&mut self, id: u32, event: PopupEvent, sender: &ComponentSender<Self>, root: &gtk::Window) {
        let sender = sender.clone();
        match event {
            PopupEvent::Clicked(_, button) => match button {
                3 => {
                    let ids: Vec<u32> = self.popups.iter().filter(|p| !p.leaving).map(|p| p.notification.id).collect();
                    for id in ids {
                        self.close(id, DISMISSED, &sender, root);
                    }
                }
                1 if self.find(id).is_some_and(|n| n.actions.iter().any(|(key, _)| key == "default")) => {
                    self.invoke(id, "default", &sender, root)
                }
                1 => self.close(id, DISMISSED, &sender, root),
                _ => {}
            },
            PopupEvent::Action(_, key) => self.invoke(id, &key, &sender, root),
            PopupEvent::Hover(_, true) => {
                self.hovered.insert(id);
                if let Some(timer) = self.timers.remove(&id) {
                    timer.remove();
                }
            }
            PopupEvent::Hover(_, false) => {
                self.hovered.remove(&id);
                if self.popups.iter().any(|p| p.notification.id == id && p.shown && !p.leaving) {
                    self.start_timer(id, &sender);
                }
            }
        }
    }

    fn find(&self, id: u32) -> Option<&Notification> {
        self.popups.iter().filter(|p| !p.leaving).map(|p| &p.notification).find(|n| n.id == id)
    }

    fn emit(&self, member: &'static str, body: impl serde::Serialize + zbus::zvariant::DynamicType + 'static) {
        let Some(connection) = self.connection.clone() else { return };
        glib::spawn_future_local(async move {
            let _ = connection.emit_signal(None::<&str>, PATH, INTERFACE, member, &body).await;
        });
    }

    fn close(&mut self, id: u32, reason: u32, sender: &ComponentSender<Self>, root: &gtk::Window) {
        let Some(i) = self.popups.iter().position(|p| p.notification.id == id && !p.leaving) else { return };
        if let Some(timer) = self.timers.remove(&id) {
            timer.remove();
        }
        self.hovered.remove(&id);
        self.emit("NotificationClosed", (id, reason));
        let mut popups = self.popups.guard();
        let popup = &mut popups[i];
        // A card past MAX_SHOWN is not on screen, so it just goes
        if !popup.shown {
            popups.remove(i);
            drop(popups);
            self.update_shown(sender);
            // The "more" label may have gone; the window waits if a card is still sliding out
            if !self.popups.iter().any(|p| p.leaving) {
                self.refresh(sender, root, true);
            }
            return;
        }
        // Slide out, then let the cards below move up into the gap. The window and the cards
        // past MAX_SHOWN wait for Remove, so the leaving card keeps its place until then.
        popup.leaving = true;
        let serial = popup.serial;
        drop(popups);
        let later = sender.clone();
        anim::after(LEAVE_MS, move || later.input(NotificationsMsg::Collapse(serial)));
    }

    fn invoke(&mut self, id: u32, action: &str, sender: &ComponentSender<Self>, root: &gtk::Window) {
        self.emit("ActionInvoked", (id, action.to_owned()));
        if !self.find(id).is_some_and(|n| n.resident) {
            self.close(id, DISMISSED, sender, root);
        }
    }

    fn refresh(&mut self, sender: &ComponentSender<Self>, root: &gtk::Window, shrink: bool) {
        self.update_shown(sender);
        if self.popups.is_empty() {
            return root.set_visible(false);
        }
        // Hiding and showing again makes the window shrink to fit what is left. Not otherwise, so
        // a card that is sliding out is not cut short.
        if shrink || !root.is_visible() {
            root.set_visible(false);
            root.present();
        }
    }

    fn update_shown(&mut self, sender: &ComponentSender<Self>) {
        let mut shown = Vec::new();
        // A leaving card keeps its slot until it is removed
        for (slot, popup) in self.popups.guard().iter_mut().enumerate() {
            if !popup.leaving {
                popup.shown = slot < MAX_SHOWN;
                shown.push((popup.notification.id, popup.shown));
            }
        }
        for (id, shown) in shown {
            if !shown {
                if let Some(timer) = self.timers.remove(&id) {
                    timer.remove();
                }
            } else if !self.hovered.contains(&id) {
                self.start_timer(id, sender);
            }
        }
        self.hidden = self.popups.iter().filter(|p| !p.leaving && !p.shown).count();
    }

    fn start_timer(&mut self, id: u32, sender: &ComponentSender<Self>) {
        let Some(n) = self.find(id) else { return };
        if self.timers.contains_key(&id) || n.urgency == CRITICAL {
            return;
        }
        let timeout = if n.expire_timeout > 0 {
            n.expire_timeout as u64
        } else if n.urgency == LOW {
            5000
        } else {
            10000
        };
        let sender = sender.clone();
        let source = glib::timeout_add_local_once(Duration::from_millis(timeout), move || {
            sender.input(NotificationsMsg::Expired(id))
        });
        self.timers.insert(id, source);
    }
}

struct Popup {
    serial: u64,
    notification: Notification,
    shown: bool,
    /// Closed, and fading out
    leaving: bool,
    revealed: bool,
}

fn line(text: &str, classes: &[&str]) -> gtk::Label {
    let label = gtk::Label::new(Some(text));
    label.set_css_classes(classes);
    label.set_visible(!text.is_empty());
    label.set_xalign(0.0);
    label.set_ellipsize(gtk::pango::EllipsizeMode::End);
    label.set_max_width_chars(1);
    label.set_hexpand(true);
    label
}

#[relm4::factory]
impl FactoryComponent for Popup {
    type Init = (u64, Notification);
    type Input = ();
    type Output = PopupEvent;
    type CommandOutput = ();
    type ParentWidget = gtk::Box;

    view! {
        // The revealer collapses the gap a closed popup leaves, margin included
        gtk::Revealer {
            set_transition_type: gtk::RevealerTransitionType::SlideUp,
            set_transition_duration: COLLAPSE_MS as u32,
            #[watch]
            set_reveal_child: self.revealed,
            #[watch]
            set_visible: self.shown,

            #[name = "card"]
            gtk::Box {
            set_spacing: 12,
            set_css_classes: &["card", "notification"],
            set_class_active: ("low", self.notification.urgency == LOW),
            set_class_active: ("critical", self.notification.urgency == CRITICAL),
            #[watch]
            set_class_active: ("leaving", self.leaving),
            #[watch]
            set_can_target: !self.leaving,
            set_cursor_from_name: Some("pointer"),
            set_margin_end: SHADOW,
            set_margin_bottom: GAP,
            on_click[sender, serial = self.serial] => move |button| {
                let _ = sender.output(PopupEvent::Clicked(serial, button));
            },
            on_hover[sender, serial = self.serial] => move |inside| {
                let _ = sender.output(PopupEvent::Hover(serial, inside));
            },

            #[name = "text"]
            gtk::Box {
                set_orientation: gtk::Orientation::Vertical,
                set_spacing: 4,
                set_hexpand: true,

                line(&self.notification.app_name, &["app-name", "small", "bold"]),
                line(&self.notification.summary, &["bold"]),
                #[name = "body"]
                gtk::Label {
                    set_visible: !self.notification.body.is_empty(),
                    set_wrap: true,
                    set_wrap_mode: gtk::pango::WrapMode::WordChar,
                    set_xalign: 0.0,
                    set_max_width_chars: 1,
                    set_hexpand: true,
                    set_lines: 6,
                    set_ellipsize: gtk::pango::EllipsizeMode::End,
                },
                #[name = "actions"]
                gtk::FlowBox {
                    set_selection_mode: gtk::SelectionMode::None,
                    set_column_spacing: 8,
                    set_row_spacing: 8,
                    set_margin_top: 6,
                    set_max_children_per_line: 8,
                    set_visible: self.notification.actions.iter().any(|(key, _)| key != "default"),
                },
            },
            },
        }
    }

    fn init_model((serial, notification): Self::Init, _: &DynamicIndex, _: FactorySender<Self>) -> Self {
        Popup { serial, notification, shown: true, leaving: false, revealed: true }
    }

    fn init_widgets(
        &mut self,
        _index: &DynamicIndex,
        root: Self::Root,
        _returned_widget: &gtk::Widget,
        sender: FactorySender<Self>,
    ) -> Self::Widgets {
        let widgets = view_output!();
        let n = &self.notification;
        if let Some(image) = image(n) {
            image.set_pixel_size(24);
            image.set_valign(gtk::Align::Start);
            image.set_margin_top(2);
            widgets.card.prepend(&image);
        }
        match markup(&n.body) {
            Some(markup) => widgets.body.set_markup(&markup),
            None => widgets.body.set_text(&strip_tags(&n.body)),
        }
        anim::enter(widgets.card.upcast_ref());
        for (key, label) in n.actions.iter().filter(|(key, _)| key != "default") {
            let button = gtk::Label::new(Some(label));
            button.set_css_classes(&["notification-button", "small"]);
            button.set_cursor_from_name(Some("pointer"));
            let click = gtk::GestureClick::new();
            // Claim the click so the popup's own click does not also run
            click.connect_pressed(|gesture, _, _, _| {
                gesture.set_state(gtk::EventSequenceState::Claimed);
            });
            let (sender, serial, key) = (sender.clone(), self.serial, key.clone());
            click.connect_released(move |_, _, _, _| {
                let _ = sender.output(PopupEvent::Action(serial, key.clone()));
            });
            button.add_controller(click);
            widgets.actions.insert(&button, -1);
        }
        widgets
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
