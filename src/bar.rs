// Status bar on each monitor, showing dwl's state from its ipc protocol.
use std::{
    cell::RefCell,
    collections::{HashMap, HashSet},
    rc::Rc,
    time::Duration,
};

use gtk::{gdk, glib, prelude::*};
use gtk4_layer_shell::{KeyboardMode, Layer, LayerShell};
use crate::{
    config::Config,
    dwl::{Command, Dwl, Event, OutputState},
    icon::Icon,
    monitors, util,
};

const HEIGHT: i32 = 27;

struct Tag {
    widget: gtk::Box,
    underline: gtk::Box,
}

struct Screen {
    output: String,
    window: gtk::Window,
    left: gtk::Box,
    tags: Vec<Tag>,
    symbol: gtk::Label,
    title_box: gtk::Box,
    floating: gtk::Box,
    title: gtk::Label,
    status: gtk::Box,
}

struct Status {
    cpu: gtk::Label,
    memory: gtk::Label,
    wifi: Icon,
    clock: gtk::Label,
}

pub struct Bar {
    config: Rc<Config>,
    screens: RefCell<Vec<Screen>>,
    statuses: RefCell<Vec<Status>>,
    outputs: RefCell<HashMap<String, OutputState>>,
    hidden: RefCell<HashSet<String>>,
    dwl: RefCell<Option<Rc<Dwl>>>,
}

impl Bar {
    pub fn new(config: Rc<Config>) -> Rc<Bar> {
        let bar = Rc::new(Bar {
            config,
            screens: RefCell::new(Vec::new()),
            statuses: RefCell::new(Vec::new()),
            outputs: RefCell::new(HashMap::new()),
            hidden: RefCell::new(HashSet::new()),
            dwl: RefCell::new(None),
        });
        bar.build();
        let weak = Rc::downgrade(&bar);
        monitors::on_change(move || {
            if let Some(bar) = weak.upgrade() {
                bar.build();
            }
        });
        let weak = Rc::downgrade(&bar);
        let dwl = Dwl::start(move |event| {
            if let Some(bar) = weak.upgrade() {
                bar.handle_event(event);
            }
        });
        bar.dwl.replace(Some(dwl));

        bar.update_status();
        let weak = Rc::downgrade(&bar);
        glib::timeout_add_local(Duration::from_secs(2), move || match weak.upgrade() {
            Some(bar) => {
                bar.update_status();
                glib::ControlFlow::Continue
            }
            None => glib::ControlFlow::Break,
        });
        bar
    }

    fn send(&self, output: &str, command: Command) {
        if let Some(dwl) = self.dwl.borrow().as_ref() {
            dwl.send(output, command);
        }
    }

    fn build(self: &Rc<Self>) {
        for screen in self.screens.borrow_mut().drain(..) {
            screen.window.destroy();
        }
        self.statuses.borrow_mut().clear();
        for monitor in monitors::all() {
            let Some(output) = monitor.connector().map(|c| c.to_string()) else {
                continue;
            };
            let screen = self.build_screen(&monitor, output);
            screen.window.present();
            self.screens.borrow_mut().push(screen);
        }
        for screen in self.screens.borrow().iter() {
            self.update_screen(screen);
        }
        self.update_status();
    }

    fn build_screen(self: &Rc<Self>, monitor: &gdk::Monitor, output: String) -> Screen {
        let window = util::layer_window(util::LayerOptions {
            namespace: "wakeshell-bar",
            layer: Layer::Top,
            anchors: [true, false, true, true],
            keyboard: KeyboardMode::None,
            monitor: Some(monitor),
            exclusive_zone: 0,
        });
        window.auto_exclusive_zone_enable();
        window.add_css_class("bar");
        window.set_default_size(-1, HEIGHT);

        let row = gtk::Box::new(gtk::Orientation::Horizontal, 0);
        row.set_size_request(-1, HEIGHT);
        window.set_child(Some(&row));

        let left = gtk::Box::new(gtk::Orientation::Horizontal, 0);
        row.append(&left);
        let weak = Rc::downgrade(self);
        let mut tags = Vec::new();
        for (i, name) in self.config.tags.iter().enumerate() {
            let widget = gtk::Box::new(gtk::Orientation::Vertical, 0);
            widget.add_css_class("tag");
            let overlay = gtk::Overlay::new();
            overlay.set_vexpand(true);
            let icon = Icon::new(name, 19);
            icon.set_halign(gtk::Align::Center);
            icon.set_valign(gtk::Align::Center);
            overlay.set_child(Some(&icon));
            let underline = gtk::Box::new(gtk::Orientation::Horizontal, 0);
            underline.add_css_class("underline");
            underline.set_valign(gtk::Align::End);
            overlay.add_overlay(&underline);
            widget.append(&overlay);
            util::hand_cursor(&widget);
            let (weak, output) = (weak.clone(), output.clone());
            util::on_click(&widget, move |button| {
                let Some(bar) = weak.upgrade() else { return };
                let mask = 1u32 << i;
                match button {
                    1 => bar.send(&output, Command::View(mask)),
                    3 => bar.send(&output, Command::ToggleView(mask)),
                    _ => {}
                }
            });
            left.append(&widget);
            tags.push(Tag { widget, underline });
        }

        let symbol = util::label("", &["layout-symbol"]);
        util::hand_cursor(&symbol);
        {
            let (weak, output) = (weak.clone(), output.clone());
            util::on_click(&symbol, move |button| {
                let Some(bar) = weak.upgrade() else { return };
                match button {
                    1 => bar.send(&output, Command::ToggleLayout),
                    3 => bar.send(&output, Command::SetLayout(bar.config.monocle_layout)),
                    _ => {}
                }
            });
        }
        left.append(&symbol);

        let title_box = gtk::Box::new(gtk::Orientation::Horizontal, 0);
        title_box.add_css_class("title");
        title_box.set_hexpand(true);
        let floating = gtk::Box::new(gtk::Orientation::Horizontal, 0);
        floating.add_css_class("floating-box");
        floating.set_valign(gtk::Align::Start);
        title_box.append(&floating);
        let title = util::label("", &[]);
        title.set_ellipsize(gtk::pango::EllipsizeMode::End);
        title.set_xalign(0.0);
        title.set_hexpand(true);
        title_box.append(&title);
        {
            let (weak, output) = (weak.clone(), output.clone());
            util::on_click(&title_box, move |button| {
                if button == 2
                    && let Some(bar) = weak.upgrade() {
                        bar.send(&output, Command::Zoom);
                    }
            });
        }
        // Keep the status on the right when there is no title
        let spacer = gtk::Box::new(gtk::Orientation::Horizontal, 0);
        spacer.set_hexpand(true);
        spacer.set_visible(false);
        row.append(&title_box);
        row.append(&spacer);
        title_box.connect_visible_notify(move |title| spacer.set_visible(!title.is_visible()));

        let status = gtk::Box::new(gtk::Orientation::Horizontal, 22);
        status.add_css_class("status");
        let group = |name: &str| {
            let group = gtk::Box::new(gtk::Orientation::Horizontal, 7);
            let icon = Icon::new(name, 17);
            icon.set_valign(gtk::Align::Center);
            group.append(&icon);
            let label = util::label("", &[]);
            group.append(&label);
            status.append(&group);
            label
        };
        let cpu = group("cpu");
        let memory = group("memory-stick");
        let wifi = Icon::new("wifi-off", 17);
        wifi.set_valign(gtk::Align::Center);
        status.append(&wifi);
        let clock = util::label("", &[]);
        status.append(&clock);
        {
            let weak = weak.clone();
            util::on_click(&status, move |button| {
                if button == 2
                    && let Some(bar) = weak.upgrade() {
                        util::spawn(&bar.config.terminal);
                    }
            });
        }
        row.append(&status);
        self.statuses.borrow_mut().push(Status { cpu, memory, wifi, clock });

        Screen { output, window, left, tags, symbol, title_box, floating, title, status }
    }

    fn update_screen(&self, screen: &Screen) {
        let outputs = self.outputs.borrow();
        let state = outputs.get(&screen.output);
        screen.window.set_visible(!self.hidden.borrow().contains(&screen.output));
        screen.left.set_visible(state.is_some());
        screen.status.set_visible(state.is_none_or(|s| s.selected));
        let Some(state) = state else {
            screen.title_box.set_visible(false);
            return;
        };
        for (i, tag) in screen.tags.iter().enumerate() {
            let bit = 1u32 << i;
            let set = |class: &str, on: bool| {
                if on {
                    tag.widget.add_css_class(class)
                } else {
                    tag.widget.remove_css_class(class)
                }
            };
            set("viewed", state.active & bit != 0);
            set("occupied", state.occupied & bit != 0);
            set("urgent", state.urgent & bit != 0);
            tag.underline.set_visible(state.active & bit != 0);
        }
        screen.symbol.set_text(&state.symbol);
        screen.title_box.set_visible(state.has_window);
        screen.floating.set_visible(state.floating);
        screen.title.set_text(&state.title);
        if state.selected {
            screen.title_box.remove_css_class("unselected");
        } else {
            screen.title_box.add_css_class("unselected");
        }
    }

    fn handle_event(&self, event: Event) {
        let output = match event {
            Event::ToggleVisibility(output) => {
                let mut hidden = self.hidden.borrow_mut();
                if !hidden.remove(&output) {
                    hidden.insert(output.clone());
                }
                output
            }
            Event::State(state) => {
                let output = state.output.clone();
                self.outputs.borrow_mut().insert(output.clone(), state);
                output
            }
        };
        for screen in self.screens.borrow().iter().filter(|s| s.output == output) {
            self.update_screen(screen);
        }
    }

    fn update_status(&self) {
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
        let memory = memory.unwrap_or_else(|| "?".to_owned());
        let wifi = wifi_connected();
        let clock = glib::DateTime::now_local()
            .and_then(|now| now.format("%a %b %d %I:%M %p"))
            .map(|s| s.to_string())
            .unwrap_or_default();
        for status in self.statuses.borrow().iter() {
            status.cpu.set_text(&load);
            status.memory.set_text(&memory);
            status.wifi.set_icon(if wifi { "wifi" } else { "wifi-off" });
            if wifi {
                status.wifi.remove_css_class("off");
            } else {
                status.wifi.add_css_class("off");
            }
            status.clock.set_text(&clock);
        }
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
