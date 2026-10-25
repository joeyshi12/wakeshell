// Volume mixer under the bar's volume item: the default output and the others to switch to, each
// app playing sound, and the microphone.
use std::{
    cell::RefCell,
    collections::{HashMap, HashSet},
    rc::Rc,
    time::{Duration, Instant},
};

use gtk::{gdk, glib, prelude::*};
use gtk4_layer_shell::LayerShell;
use relm4::{RelmWidgetExt, factory::FactoryVecDeque, prelude::*};

use crate::{
    anim::Fade,
    bar,
    icon::Icon,
    monitors,
    pipewire::{self, Audio, Device, Stream},
    util::{self, Events},
};

const WIDTH: i32 = 400;
/// Snapshots from before a drag would pull the slider back
const SETTLE: Duration = Duration::from_millis(600);

pub struct Mixer {
    shown: bool,
    audio: Audio,
    right: i32,
    writer: Rc<RefCell<Writer>>,
    devices: FactoryVecDeque<DeviceRow>,
    streams: FactoryVecDeque<StreamRow>,
    window: gtk::Window,
    fade: Option<Fade>,
}

#[derive(Debug)]
pub enum MixerMsg {
    Toggle { output: Option<String>, right: Option<i32> },
    Close,
    Audio(Audio),
    SetVolume(u32, f64),
    ToggleMute(u32),
    SetDefault(u32),
}

#[derive(Debug)]
pub struct Shown(pub bool);

#[relm4::component(pub)]
impl SimpleComponent for Mixer {
    type Init = ();
    type Input = MixerMsg;
    type Output = Shown;

    view! {
        util::overlay_window("wakeshell-mixer") -> gtk::Window {
            set_visible: false,
            on_key[sender] => move |key, _| {
                if key != gdk::Key::Escape {
                    return false;
                }
                sender.input(MixerMsg::Close);
                true
            },

            #[name = "area"]
            gtk::Box {
                set_hexpand: true,
                set_vexpand: true,

                #[name = "card"]
                gtk::Box {
                    set_orientation: gtk::Orientation::Vertical,
                    set_css_classes: &["card", "popover", "mixer"],
                    set_halign: gtk::Align::End,
                    set_valign: gtk::Align::Start,
                    set_hexpand: true,
                    set_size_request: (WIDTH, -1),
                    set_margin_top: bar::HEIGHT + 8,
                    #[watch]
                    set_margin_end: model.right,

                    gtk::Box {
                        set_spacing: 12,
                        add_css_class: "mixer-row",
                        #[watch]
                        set_class_active: ("muted-row", model.audio.sink().is_none_or(|s| s.muted)),

                        Icon::new("volume-2", 20) {
                            add_css_class: "mixer-mute",
                            set_valign: gtk::Align::Center,
                            set_cursor_from_name: Some("pointer"),
                            #[watch]
                            set_icon: volume_icon(model.audio.sink()),
                            on_own_click[sender] => move |_| sender.input(MixerMsg::ToggleMute(SINK)),
                        },
                        #[name = "sink_scale"]
                        gtk::Scale {
                            set_hexpand: true,
                            set_valign: gtk::Align::Center,
                            set_range: (0.0, 1.0),
                            set_increments: (0.05, 0.05),
                            set_cursor_from_name: Some("pointer"),
                            #[watch]
                            set_value: model.volume(model.audio.sink()),
                            connect_change_value[sender] => move |_, _, value| {
                                sender.input(MixerMsg::SetVolume(SINK, value));
                                glib::Propagation::Proceed
                            },
                        },
                        gtk::Label {
                            set_xalign: 1.0,
                            set_width_chars: 5,
                            #[watch]
                            set_label: &model.percent(model.audio.sink()),
                        },
                    },

                    #[local_ref]
                    devices -> gtk::Box {
                        set_orientation: gtk::Orientation::Vertical,
                    },

                    gtk::Box {
                        set_css_classes: &["separator-line", "mixer-gap"],
                        #[watch]
                        set_visible: !model.audio.streams.is_empty(),
                    },
                    #[local_ref]
                    streams -> gtk::Box {
                        set_orientation: gtk::Orientation::Vertical,
                    },

                    gtk::Box {
                        set_css_classes: &["separator-line", "mixer-gap"],
                        #[watch]
                        set_visible: model.audio.source().is_some(),
                    },
                    gtk::Box {
                        set_spacing: 12,
                        add_css_class: "mixer-row",
                        #[watch]
                        set_visible: model.audio.source().is_some(),
                        #[watch]
                        set_class_active: ("muted-row", model.audio.source().is_some_and(|s| s.muted)),

                        Icon::new("mic", 20) {
                            add_css_class: "mixer-mute",
                            set_valign: gtk::Align::Center,
                            set_cursor_from_name: Some("pointer"),
                            #[watch]
                            set_icon: if model.audio.source().is_some_and(|s| s.muted) { "mic-off" } else { "mic" },
                            on_own_click[sender] => move |_| sender.input(MixerMsg::ToggleMute(SOURCE)),
                        },
                        gtk::Box {
                            set_orientation: gtk::Orientation::Vertical,
                            set_hexpand: true,
                            set_valign: gtk::Align::Center,
                            set_spacing: 2,

                            gtk::Label {
                                add_css_class: "mixer-name",
                                set_xalign: 0.0,
                                set_ellipsize: gtk::pango::EllipsizeMode::End,
                                set_max_width_chars: 1,
                                #[watch]
                                set_label: model.audio.source().map_or("", |s| &s.description),
                            },
                            gtk::Scale {
                                set_range: (0.0, 1.0),
                                set_increments: (0.05, 0.05),
                                set_cursor_from_name: Some("pointer"),
                                #[watch]
                                set_value: model.volume(model.audio.source()),
                                connect_change_value[sender] => move |_, _, value| {
                                    sender.input(MixerMsg::SetVolume(SOURCE, value));
                                    glib::Propagation::Proceed
                                },
                            },
                        },
                        gtk::Label {
                            set_xalign: 1.0,
                            set_width_chars: 5,
                            #[watch]
                            set_label: &model.percent(model.audio.source()),
                        },
                    },
                },
            },
        }
    }

    fn init(_: (), root: Self::Root, sender: ComponentSender<Self>) -> ComponentParts<Self> {
        let mut model = Mixer {
            shown: false,
            audio: Audio::default(),
            right: 12,
            writer: Rc::default(),
            devices: FactoryVecDeque::builder().launch(gtk::Box::default()).forward(sender.input_sender(), |id| {
                MixerMsg::SetDefault(id)
            }),
            streams: FactoryVecDeque::builder().launch(gtk::Box::default()).forward(sender.input_sender(), |m| m),
            window: root.clone(),
            fade: None,
        };
        let devices = model.devices.widget();
        let streams = model.streams.widget();
        let widgets = view_output!();
        let close = sender.clone();
        widgets.area.on_click_outside(&[widgets.card.clone().upcast()], move || close.input(MixerMsg::Close));
        model.fade = Some(Fade::new(&root, &widgets.card, true, 60));
        ComponentParts { model, widgets }
    }

    fn update(&mut self, msg: MixerMsg, sender: ComponentSender<Self>) {
        let was_shown = self.shown;
        // Keys still reach the sliders while it fades out
        if !self.shown && matches!(msg, MixerMsg::SetVolume(..) | MixerMsg::ToggleMute(_) | MixerMsg::SetDefault(_)) {
            return;
        }
        match msg {
            MixerMsg::Toggle { .. } if self.shown => self.shown = false,
            MixerMsg::Toggle { output, right } => {
                if let Some(right) = right {
                    // Lines the card's right edge up a little past the item's
                    self.right = (right - 12).max(12);
                }
                let monitor = output.and_then(|o| monitors::all().into_iter().find(|m| m.connector().is_some_and(|c| c == o)));
                self.window.set_monitor(monitor.as_ref());
                self.shown = true;
            }
            MixerMsg::Close => self.shown = false,
            MixerMsg::Audio(audio) => self.set_audio(audio),
            MixerMsg::SetVolume(id, value) => {
                let Some(id) = self.resolve(id) else { return };
                let muted = self.device_or_stream(id).is_some_and(|(_, muted)| muted);
                Writer::set(&self.writer, id, value.clamp(0.0, 1.0), muted);
                // Show the new value straight away
                self.update_rows();
            }
            MixerMsg::ToggleMute(id) => {
                let Some(id) = self.resolve(id) else { return };
                let Some((_, muted)) = self.device_or_stream(id) else { return };
                glib::spawn_future_local(pipewire::set_mute(id, !muted));
            }
            MixerMsg::SetDefault(id) => {
                glib::spawn_future_local(pipewire::set_default(id));
            }
        }
        if self.shown != was_shown {
            if let Some(fade) = &self.fade {
                fade.set(self.shown);
            }
            let _ = sender.output(Shown(self.shown));
        }
    }
}

/// Stand-ins for the default sink and source in messages from the view
const SINK: u32 = u32::MAX;
const SOURCE: u32 = u32::MAX - 1;

impl Mixer {
    fn resolve(&self, id: u32) -> Option<u32> {
        match id {
            SINK => self.audio.default_sink,
            SOURCE => self.audio.default_source,
            id => Some(id),
        }
    }

    fn device_or_stream(&self, id: u32) -> Option<(f64, bool)> {
        let devices = self.audio.sinks.iter().chain(&self.audio.sources).map(|d| (d.id, d.volume, d.muted));
        let streams = self.audio.streams.iter().map(|s| (s.id, s.volume, s.muted));
        devices.chain(streams).find(|(i, _, _)| *i == id).map(|(_, v, m)| (v, m))
    }

    /// What the slider shows: the user's value while their change settles
    fn shown_volume(&self, id: u32, actual: f64) -> f64 {
        self.writer.borrow().recent(id).unwrap_or(actual)
    }

    fn volume(&self, device: Option<&Device>) -> f64 {
        device.map_or(0.0, |d| self.shown_volume(d.id, d.volume))
    }

    fn percent(&self, device: Option<&Device>) -> String {
        match device {
            Some(d) if d.muted => "Muted".into(),
            Some(d) => format!("{}%", (self.shown_volume(d.id, d.volume) * 100.0).round()),
            None => String::new(),
        }
    }

    fn set_audio(&mut self, audio: Audio) {
        self.audio = audio;
        // Rows stay put when others come and go, so a drag on one carries on. This relies on
        // pipewire::state() listing devices and streams by id.
        let mut devices = self.devices.guard();
        let ids: Vec<u32> = self.audio.sinks.iter().map(|d| d.id).collect();
        for i in (0..devices.len()).rev() {
            if !ids.contains(&devices[i].device.id) {
                devices.remove(i);
            }
        }
        for (i, device) in self.audio.sinks.iter().enumerate() {
            if devices.get(i).map(|r| r.device.id) != Some(device.id) {
                devices.insert(i, device.clone());
            }
        }
        drop(devices);
        let mut rows = self.streams.guard();
        let ids: Vec<u32> = self.audio.streams.iter().map(|s| s.id).collect();
        for i in (0..rows.len()).rev() {
            if !ids.contains(&rows[i].stream.id) {
                rows.remove(i);
            }
        }
        for (i, stream) in self.audio.streams.iter().enumerate() {
            if rows.get(i).map(|r| r.stream.id) != Some(stream.id) {
                rows.insert(i, stream.clone());
            }
        }
        drop(rows);
        self.update_rows();
    }

    fn update_rows(&mut self) {
        let default = self.audio.default_sink;
        for (row, device) in self.devices.guard().iter_mut().zip(&self.audio.sinks) {
            row.device = device.clone();
            row.current = Some(device.id) == default;
        }
        let shown: Vec<f64> = self.audio.streams.iter().map(|s| self.shown_volume(s.id, s.volume)).collect();
        for ((row, stream), volume) in self.streams.guard().iter_mut().zip(&self.audio.streams).zip(shown) {
            row.stream = stream.clone();
            row.volume = volume;
        }
    }
}

fn volume_icon(device: Option<&Device>) -> &'static str {
    device.map_or("volume-x", |d| pipewire::volume_icon(d.volume, d.muted))
}

/// Runs one wpctl per node at a time, always sending the newest value, so dragging does not
/// queue up a backlog of calls
#[derive(Default)]
struct Writer {
    wanted: HashMap<u32, f64>,
    busy: HashSet<u32>,
    touched: HashMap<u32, (Instant, f64)>,
    /// What wpctl was last told, as it was told
    sent: HashMap<u32, String>,
}

impl Writer {
    fn set(writer: &Rc<RefCell<Writer>>, id: u32, volume: f64, unmute: bool) {
        {
            let mut w = writer.borrow_mut();
            // A drag reports the same position many times in a row
            let repeat = w.touched.get(&id).is_some_and(|(at, v)| {
                at.elapsed() < Duration::from_millis(100) && format!("{v:.2}") == format!("{volume:.2}")
            });
            if repeat {
                return;
            }
            w.wanted.insert(id, volume);
            w.touched.insert(id, (Instant::now(), volume));
            if !w.busy.insert(id) {
                return;
            }
        }
        let writer = writer.clone();
        glib::spawn_future_local(async move {
            if unmute {
                pipewire::set_mute(id, false).await;
            }
            loop {
                let next = writer.borrow_mut().wanted.remove(&id);
                let Some(volume) = next else { break };
                let text = format!("{volume:.2}");
                if writer.borrow_mut().sent.insert(id, text.clone()).as_ref() == Some(&text) {
                    continue;
                }
                pipewire::set_volume(id, volume).await;
            }
            // Only within one run: afterwards the volume may have moved some other way
            let mut w = writer.borrow_mut();
            w.busy.remove(&id);
            w.sent.remove(&id);
        });
    }

    fn recent(&self, id: u32) -> Option<f64> {
        self.touched.get(&id).filter(|(at, _)| at.elapsed() < SETTLE || self.busy.contains(&id)).map(|(_, v)| *v)
    }
}

struct DeviceRow {
    device: Device,
    current: bool,
}

#[relm4::factory]
impl FactoryComponent for DeviceRow {
    type Init = Device;
    type Input = ();
    type Output = u32;
    type CommandOutput = ();
    type ParentWidget = gtk::Box;

    view! {
        gtk::Box {
            set_spacing: 12,
            add_css_class: "mixer-device",
            set_cursor_from_name: Some("pointer"),
            #[watch]
            set_class_active: ("current", self.current),
            on_left_click[sender, id = self.device.id] => move || {
                let _ = sender.output(id);
            },

            Icon::new("speaker", 18) {
                add_css_class: "device-icon",
                set_valign: gtk::Align::Center,
                #[watch]
                set_icon: self.device.icon,
            },
            gtk::Label {
                set_hexpand: true,
                set_xalign: 0.0,
                set_ellipsize: gtk::pango::EllipsizeMode::End,
                set_max_width_chars: 1,
                #[watch]
                set_label: &self.device.description,
            },
            Icon::new("check", 16) {
                add_css_class: "check",
                set_valign: gtk::Align::Center,
                #[watch]
                set_visible: self.current,
            },
        }
    }

    fn init_model(device: Device, _: &DynamicIndex, _: FactorySender<Self>) -> Self {
        DeviceRow { device, current: false }
    }
}

struct StreamRow {
    stream: Stream,
    volume: f64,
}

#[relm4::factory]
impl FactoryComponent for StreamRow {
    type Init = Stream;
    type Input = ();
    type Output = MixerMsg;
    type CommandOutput = ();
    type ParentWidget = gtk::Box;

    view! {
        gtk::Box {
            set_spacing: 12,
            add_css_class: "mixer-row",
            #[watch]
            set_class_active: ("muted-row", self.stream.muted),

            gtk::Image {
                add_css_class: "mixer-mute",
                set_pixel_size: 22,
                set_valign: gtk::Align::Center,
                set_cursor_from_name: Some("pointer"),
                set_icon_name: Some(&app_icon(&self.stream.icon)),
                #[watch]
                set_opacity: if self.stream.muted { 0.5 } else { 1.0 },
                on_own_click[sender, id = self.stream.id] => move |_| {
                    let _ = sender.output(MixerMsg::ToggleMute(id));
                },
            },
            gtk::Box {
                set_orientation: gtk::Orientation::Vertical,
                set_hexpand: true,
                set_valign: gtk::Align::Center,
                set_spacing: 2,

                gtk::Label {
                    add_css_class: "mixer-name",
                    set_xalign: 0.0,
                    set_ellipsize: gtk::pango::EllipsizeMode::End,
                    set_max_width_chars: 1,
                    #[watch]
                    set_label: &self.stream.app,
                },
                gtk::Scale {
                    set_range: (0.0, 1.0),
                    set_increments: (0.05, 0.05),
                    set_cursor_from_name: Some("pointer"),
                    #[watch]
                    set_value: self.volume,
                    connect_change_value[sender, id = self.stream.id] => move |_, _, value| {
                        let _ = sender.output(MixerMsg::SetVolume(id, value));
                        glib::Propagation::Proceed
                    },
                },
            },
            gtk::Label {
                set_xalign: 1.0,
                set_width_chars: 5,
                #[watch]
                set_label: &if self.stream.muted { "Muted".to_owned() } else { format!("{}%", (self.volume * 100.0).round()) },
            },
        }
    }

    fn init_model(stream: Stream, _: &DynamicIndex, _: FactorySender<Self>) -> Self {
        let volume = stream.volume;
        StreamRow { stream, volume }
    }
}

/// The app's themed icon, or a generic one when the theme has none by that name
fn app_icon(name: &str) -> String {
    let theme = gdk::Display::default().map(|d| gtk::IconTheme::for_display(&d));
    let lower = name.to_lowercase();
    [name, lower.as_str()]
        .into_iter()
        .find(|n| !n.is_empty() && theme.as_ref().is_some_and(|t| t.has_icon(n)))
        .unwrap_or("application-x-executable")
        .to_owned()
}
