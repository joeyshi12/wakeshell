// Status bar on each monitor, showing dwl's state from its ipc protocol.
use std::{
    collections::{HashMap, HashSet},
    rc::Rc,
    time::Duration,
};

use gtk::{gdk, glib, prelude::*};
use gtk4_layer_shell::{KeyboardMode, Layer, LayerShell};
use relm4::{RelmWidgetExt, factory::FactoryVecDeque, prelude::*};

use crate::{
    config::Config,
    dwl::{Command, Dwl, Event, OutputState},
    icon::Icon,
    monitors,
    util::{self, Events},
};

const HEIGHT: i32 = 27;

pub struct Bar {
    config: Rc<Config>,
    dwl: Option<Rc<Dwl>>,
    screens: Vec<(String, Controller<Screen>)>,
    outputs: HashMap<String, OutputState>,
    hidden: HashSet<String>,
    status: Status,
}

#[derive(Debug)]
pub enum BarMsg {
    MonitorsChanged,
    Dwl(Event),
    Tick,
    Click(String, Click),
}

impl Component for Bar {
    type Init = Rc<Config>;
    type Input = BarMsg;
    type Output = ();
    type CommandOutput = ();
    type Root = ();
    type Widgets = ();

    fn init_root() {}

    fn init(config: Rc<Config>, _: (), sender: ComponentSender<Self>) -> ComponentParts<Self> {
        let mut model = Bar {
            config,
            dwl: None,
            screens: Vec::new(),
            outputs: HashMap::new(),
            hidden: HashSet::new(),
            status: Status::read(),
        };
        model.build(&sender);
        let monitors = sender.clone();
        monitors::on_change(move || monitors.input(BarMsg::MonitorsChanged));
        let events = sender.clone();
        model.dwl = Some(Dwl::start(move |event| events.input(BarMsg::Dwl(event))));
        let tick = sender.clone();
        glib::timeout_add_local(Duration::from_secs(2), move || {
            tick.input(BarMsg::Tick);
            glib::ControlFlow::Continue
        });
        ComponentParts { model, widgets: () }
    }

    fn update(&mut self, msg: BarMsg, sender: ComponentSender<Self>, _: &()) {
        match msg {
            BarMsg::MonitorsChanged => {
                self.status = Status::read();
                self.build(&sender);
            }
            BarMsg::Tick => {
                self.status = Status::read();
                for (_, screen) in &self.screens {
                    screen.emit(ScreenMsg::Status(self.status.clone()));
                }
            }
            BarMsg::Dwl(event) => {
                let output = match event {
                    Event::ToggleVisibility(output) => {
                        if !self.hidden.remove(&output) {
                            self.hidden.insert(output.clone());
                        }
                        output
                    }
                    Event::State(state) => {
                        let output = state.output.clone();
                        self.outputs.insert(output.clone(), state);
                        output
                    }
                };
                for (_, screen) in self.screens.iter().filter(|(o, _)| *o == output) {
                    screen.emit(self.screen_state(&output));
                }
            }
            BarMsg::Click(output, click) => {
                let command = match click {
                    Click::Tag(i, 1) => Command::View(1 << i),
                    Click::Tag(i, 3) => Command::ToggleView(1 << i),
                    Click::Layout(1) => Command::ToggleLayout,
                    Click::Layout(3) => Command::SetLayout(self.config.monocle_layout),
                    Click::Title(2) => Command::Zoom,
                    Click::Status(2) => return util::spawn(&self.config.terminal),
                    _ => return,
                };
                if let Some(dwl) = &self.dwl {
                    dwl.send(&output, command);
                }
            }
        }
    }
}

impl Bar {
    fn screen_state(&self, output: &str) -> ScreenMsg {
        ScreenMsg::State { state: self.outputs.get(output).cloned(), hidden: self.hidden.contains(output) }
    }

    fn build(&mut self, sender: &ComponentSender<Self>) {
        self.screens.clear();
        for monitor in monitors::all() {
            let Some(output) = monitor.connector().map(|c| c.to_string()) else {
                continue;
            };
            let name = output.clone();
            let screen = Screen::builder()
                .launch(ScreenInit { monitor, tags: self.config.tags.clone(), status: self.status.clone() })
                .forward(sender.input_sender(), move |click| BarMsg::Click(name.clone(), click));
            screen.emit(self.screen_state(&output));
            self.screens.push((output, screen));
        }
    }
}

#[derive(Debug, Clone, Default)]
struct Status {
    load: String,
    memory: String,
    wifi: bool,
    clock: String,
}

impl Status {
    fn read() -> Status {
        let load = std::fs::read_to_string("/proc/loadavg")
            .ok()
            .and_then(|s| s.split_whitespace().next()?.parse::<f64>().ok())
            .map_or("?".to_owned(), |load| format!("{load:.2}"));
        let memory = std::fs::read_to_string("/proc/meminfo").ok().and_then(|text| {
            let field = |name: &str| -> Option<f64> {
                text.lines().find(|l| l.starts_with(name))?.split_whitespace().nth(1)?.parse().ok()
            };
            Some(format!("{:.1}G", (field("MemTotal:")? - field("MemAvailable:")?) / 1048576.0))
        });
        let clock = glib::DateTime::now_local()
            .and_then(|now| now.format("%a %b %d %I:%M %p"))
            .map(|s| s.to_string())
            .unwrap_or_default();
        Status { load, memory: memory.unwrap_or_else(|| "?".to_owned()), wifi: wifi_connected(), clock }
    }
}

#[derive(Debug)]
pub enum Click {
    Tag(usize, u32),
    Layout(u32),
    Title(u32),
    Status(u32),
}

struct Screen {
    window: gtk::Window,
    state: Option<OutputState>,
    title_changed: bool,
    hidden: bool,
    status: Status,
    tags: FactoryVecDeque<Tag>,
}

struct ScreenInit {
    monitor: gdk::Monitor,
    tags: Vec<String>,
    status: Status,
}

#[derive(Debug)]
enum ScreenMsg {
    State { state: Option<OutputState>, hidden: bool },
    Status(Status),
}

impl Screen {
    fn has_window(&self) -> bool {
        self.state.as_ref().is_some_and(|s| s.has_window)
    }
}

#[relm4::component]
impl SimpleComponent for Screen {
    type Init = ScreenInit;
    type Input = ScreenMsg;
    type Output = Click;

    view! {
        util::layer_window(util::LayerOptions {
            namespace: "wakeshell-bar",
            layer: Layer::Top,
            anchors: [true, false, true, true],
            keyboard: KeyboardMode::None,
            monitor: None,
            exclusive_zone: 0,
        }) -> gtk::Window {
            auto_exclusive_zone_enable: (),
            add_css_class: "bar",
            set_default_size: (-1, HEIGHT),

            gtk::Box {
                set_size_request: (-1, HEIGHT),

                gtk::Box {
                    #[watch]
                    set_visible: model.state.is_some(),

                    #[local_ref]
                    tags -> gtk::Box {},

                    gtk::Label {
                        add_css_class: "layout-symbol",
                        set_cursor_from_name: Some("pointer"),
                        #[watch]
                        set_label: model.state.as_ref().map_or("", |s| &s.symbol),
                        on_click[sender] => move |button| {
                            let _ = sender.output(Click::Layout(button));
                        },
                    },
                },

                gtk::Box {
                    add_css_class: "title",
                    set_hexpand: true,
                    #[watch]
                    set_visible: model.has_window(),
                    #[watch]
                    set_class_active: ("unselected", !model.state.as_ref().is_some_and(|s| s.selected)),
                    on_click[sender] => move |button| {
                        let _ = sender.output(Click::Title(button));
                    },

                    gtk::Box {
                        add_css_class: "floating-box",
                        set_valign: gtk::Align::Start,
                        #[watch]
                        set_visible: model.state.as_ref().is_some_and(|s| s.floating),
                    },
                    gtk::Overlay {
                        set_hexpand: true,
                        #[wrap(Some)]
                        set_child = &gtk::Label {
                            set_ellipsize: gtk::pango::EllipsizeMode::End,
                            set_xalign: 0.0,
                            #[watch]
                            set_label: model.state.as_ref().map_or("", |s| &s.title),
                        },
                        // Accent tips draw outside the text's damage; redrawing this repaints them
                        #[name = "repaint"]
                        add_overlay = &gtk::DrawingArea {
                            set_can_target: false,
                            set_draw_func: |_, _, _, _| {},
                        },
                    },
                },

                // Keeps the status on the right when there is no title
                gtk::Box {
                    set_hexpand: true,
                    #[watch]
                    set_visible: !model.has_window(),
                },

                gtk::Box {
                    set_spacing: 17,
                    add_css_class: "status",
                    #[watch]
                    set_visible: model.state.as_ref().is_none_or(|s| s.selected),
                    on_click[sender] => move |button| {
                        let _ = sender.output(Click::Status(button));
                    },

                    gtk::Box {
                        set_spacing: 7,
                        Icon::new("cpu", 17) {
                            set_valign: gtk::Align::Center,
                        },
                        gtk::Label {
                            #[watch]
                            set_label: &model.status.load,
                        },
                    },
                    gtk::Box {
                        set_spacing: 7,
                        Icon::new("memory-stick", 17) {
                            set_valign: gtk::Align::Center,
                        },
                        gtk::Label {
                            #[watch]
                            set_label: &model.status.memory,
                        },
                    },
                    Icon::new("wifi-off", 17) {
                        set_valign: gtk::Align::Center,
                        #[watch]
                        set_icon: if model.status.wifi { "wifi" } else { "wifi-off" },
                        #[watch]
                        set_class_active: ("off", !model.status.wifi),
                    },
                    gtk::Label {
                        #[watch]
                        set_label: &model.status.clock,
                    },
                },
            },

            #[watch]
            set_visible: !model.hidden,
        }
    }

    fn init(init: ScreenInit, root: Self::Root, sender: ComponentSender<Self>) -> ComponentParts<Self> {
        let mut tags = FactoryVecDeque::builder()
            .launch(gtk::Box::default())
            .forward(sender.output_sender(), |(i, button)| Click::Tag(i, button));
        for (i, name) in init.tags.into_iter().enumerate() {
            tags.guard().push_back((i, name));
        }
        let model = Screen { window: root.clone(), state: None, title_changed: false, hidden: false, status: init.status, tags };
        let tags = model.tags.widget();
        root.set_monitor(Some(&init.monitor));
        let widgets = view_output!();
        ComponentParts { model, widgets }
    }

    fn post_view() {
        if model.title_changed {
            repaint.queue_draw();
        }
    }

    // After the last update, so nothing touches the window once it is gone
    fn shutdown(&mut self, _: &mut Self::Widgets, _: relm4::Sender<Self::Output>) {
        self.window.destroy();
    }

    fn update(&mut self, msg: ScreenMsg, _sender: ComponentSender<Self>) {
        match msg {
            ScreenMsg::State { state, hidden } => {
                self.hidden = hidden;
                let mut tags = self.tags.guard();
                for (i, tag) in tags.iter_mut().enumerate() {
                    let bit = 1u32 << i;
                    let state = state.as_ref();
                    tag.viewed = state.is_some_and(|s| s.active & bit != 0);
                    tag.occupied = state.is_some_and(|s| s.occupied & bit != 0);
                    tag.urgent = state.is_some_and(|s| s.urgent & bit != 0);
                }
                drop(tags);
                self.title_changed = self.state.as_ref().map(|s| &s.title) != state.as_ref().map(|s| &s.title);
                self.state = state;
            }
            ScreenMsg::Status(status) => {
                self.title_changed = false;
                self.status = status;
            }
        }
    }
}

#[derive(Debug)]
struct Tag {
    index: usize,
    icon: String,
    viewed: bool,
    occupied: bool,
    urgent: bool,
}

#[relm4::factory]
impl FactoryComponent for Tag {
    type Init = (usize, String);
    type Input = ();
    type Output = (usize, u32);
    type CommandOutput = ();
    type ParentWidget = gtk::Box;

    view! {
        gtk::Box {
            set_orientation: gtk::Orientation::Vertical,
            add_css_class: "tag",
            #[watch]
            set_class_active: ("viewed", self.viewed),
            #[watch]
            set_class_active: ("occupied", self.occupied),
            #[watch]
            set_class_active: ("urgent", self.urgent),
            set_cursor_from_name: Some("pointer"),
            on_click[sender, index = self.index] => move |button| {
                let _ = sender.output((index, button));
            },

            gtk::Overlay {
                set_vexpand: true,
                #[wrap(Some)]
                set_child = &Icon::new(&self.icon, 19) {
                    set_halign: gtk::Align::Center,
                    set_valign: gtk::Align::Center,
                },
                add_overlay = &gtk::Box {
                    add_css_class: "underline",
                    set_valign: gtk::Align::End,
                    #[watch]
                    set_visible: self.viewed,
                },
            },
        }
    }

    fn init_model((index, icon): Self::Init, _: &DynamicIndex, _: FactorySender<Self>) -> Self {
        Tag { index, icon, viewed: false, occupied: false, urgent: false }
    }
}

fn wifi_connected() -> bool {
    let Ok(entries) = std::fs::read_dir("/sys/class/net") else {
        return false;
    };
    let mut names: Vec<String> = entries
        .flatten()
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .filter(|n| n.starts_with("wl"))
        .collect();
    names.sort();
    names.first().is_some_and(|name| {
        std::fs::read_to_string(format!("/sys/class/net/{name}/operstate")).is_ok_and(|s| s.trim() == "up")
    })
}
