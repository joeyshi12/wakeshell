// App launcher. Launch counts in $XDG_STATE_HOME/wakeshell/launcher.json rank the results.
use std::{cell::Cell, collections::HashMap, rc::Rc};

use gtk::{gdk, gio, prelude::*};
use relm4::{RelmWidgetExt, factory::FactoryVecDeque, prelude::*};
use serde::{Deserialize, Serialize};

use crate::{
    config::{Config, state_dir},
    repaint::RepaintBox,
    util::{self, Events},
};

const ROW_HEIGHT: i32 = 36;
const VISIBLE_ROWS: i32 = 10;
const SEARCH_HEIGHT: i32 = 52;

#[derive(Serialize, Deserialize, Default)]
struct History {
    #[serde(default)]
    counts: HashMap<String, u32>,
}

struct App {
    id: String,
    name: String,
    extra: String,
    info: gio_unix::DesktopAppInfo,
}

pub struct Launcher {
    config: Rc<Config>,
    history: History,
    shown: bool,
    focus: Cell<bool>,
    apps: Vec<App>,
    query: gtk::EntryBuffer,
    results: Vec<usize>,
    rows: FactoryVecDeque<Row>,
    current: usize,
    scroll: gtk::Adjustment,
}

#[derive(Debug)]
pub enum LauncherMsg {
    Open { blocked: bool },
    Toggle { blocked: bool },
    Close,
    Search,
    Move(Move),
    Hover(f64),
    Launch,
}

#[derive(Debug)]
pub struct Shown(pub bool);

#[derive(Debug)]
pub enum Move {
    Down(usize),
    Up(usize),
}

fn key_action(key: gdk::Key, modifiers: gdk::ModifierType) -> Option<LauncherMsg> {
    let ctrl = modifiers.contains(gdk::ModifierType::CONTROL_MASK);
    Some(match key {
        gdk::Key::Escape => LauncherMsg::Close,
        gdk::Key::Return | gdk::Key::KP_Enter => LauncherMsg::Launch,
        gdk::Key::Down | gdk::Key::Tab => LauncherMsg::Move(Move::Down(1)),
        gdk::Key::n if ctrl => LauncherMsg::Move(Move::Down(1)),
        gdk::Key::Up | gdk::Key::ISO_Left_Tab => LauncherMsg::Move(Move::Up(1)),
        gdk::Key::p if ctrl => LauncherMsg::Move(Move::Up(1)),
        gdk::Key::Page_Down => LauncherMsg::Move(Move::Down(VISIBLE_ROWS as usize)),
        gdk::Key::Page_Up => LauncherMsg::Move(Move::Up(VISIBLE_ROWS as usize)),
        _ => return None,
    })
}

#[relm4::component(pub)]
impl SimpleComponent for Launcher {
    type Init = Rc<Config>;
    type Input = LauncherMsg;
    type Output = Shown;

    view! {
        util::overlay_window("wakeshell-launcher") -> gtk::Window {
            #[name = "area"]
            gtk::Box {
                set_orientation: gtk::Orientation::Vertical,
                set_hexpand: true,
                set_vexpand: true,

                #[name = "card"]
                RepaintBox {
                    set_orientation: gtk::Orientation::Vertical,
                    add_css_class: "card",
                    set_halign: gtk::Align::Center,
                    set_valign: gtk::Align::Center,
                    set_vexpand: true,
                    set_size_request: (600, SEARCH_HEIGHT + 1 + VISIBLE_ROWS * ROW_HEIGHT + 16),

                    gtk::Box {
                        set_spacing: 8,
                        add_css_class: "launcher-search",
                        set_size_request: (-1, SEARCH_HEIGHT - 6),

                        #[name = "input"]
                        gtk::Text {
                            set_buffer: &model.query,
                            set_placeholder_text: Some("Search..."),
                            set_hexpand: true,
                            connect_changed => LauncherMsg::Search,
                            on_key[sender] => move |key, modifiers| {
                                let action = key_action(key, modifiers);
                                let handled = action.is_some();
                                if let Some(action) = action {
                                    sender.input(action);
                                }
                                handled
                            },
                        },
                        gtk::Label {
                            set_label: "Launch",
                            add_css_class: "launcher-prompt",
                        },
                    },

                    gtk::Box {
                        add_css_class: "separator-line",
                        set_margin_start: 11,
                        set_margin_end: 11,
                    },

                    gtk::Overlay {
                        #[wrap(Some)]
                        set_child = &gtk::ScrolledWindow {
                            set_policy: (gtk::PolicyType::Never, gtk::PolicyType::External),
                            set_vadjustment: Some(&model.scroll),
                            set_size_request: (-1, VISIBLE_ROWS * ROW_HEIGHT),
                            set_margin_top: 8,
                            set_margin_start: 7,
                            set_margin_end: 7,
                            set_cursor_from_name: Some("pointer"),
                            on_pointer_moved[sender] => move |_, y| sender.input(LauncherMsg::Hover(y)),
                            on_left_click[sender] => move || sender.input(LauncherMsg::Launch),

                            #[wrap(Some)]
                            set_child = &gtk::Overlay {
                                #[local_ref]
                                #[wrap(Some)]
                                set_child = rows -> gtk::Box {
                                    set_orientation: gtk::Orientation::Vertical,
                                    set_valign: gtk::Align::Start,
                                },
                            },
                        },
                        add_overlay = &gtk::Label {
                            set_label: "No matches",
                            add_css_class: "launcher-empty",
                            set_halign: gtk::Align::Center,
                            set_valign: gtk::Align::Center,
                            set_can_target: false,
                            #[watch]
                            set_visible: model.results.is_empty(),
                        },
                    },
                },
            },

            // No transition: it opens many times a day, so it should be there at once
            #[watch]
            set_visible: model.shown,
        }
    }

    fn init(config: Rc<Config>, root: Self::Root, sender: ComponentSender<Self>) -> ComponentParts<Self> {
        let history = std::fs::read_to_string(state_dir().join("launcher.json"))
            .ok()
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or_default();
        let model = Launcher {
            config,
            history,
            shown: false,
            focus: Cell::new(false),
            apps: Vec::new(),
            query: gtk::EntryBuffer::default(),
            results: Vec::new(),
            rows: FactoryVecDeque::builder().launch(gtk::Box::default()).detach(),
            current: 0,
            scroll: gtk::Adjustment::default(),
        };
        let rows = model.rows.widget();
        let widgets = view_output!();
        let close = sender.clone();
        widgets.area.on_click_outside(&[widgets.card.clone().upcast()], move || close.input(LauncherMsg::Close));
        ComponentParts { model, widgets }
    }

    fn post_view() {
        if model.focus.take() {
            input.grab_focus();
        }
    }

    fn update(&mut self, msg: LauncherMsg, sender: ComponentSender<Self>) {
        let was_shown = self.shown;
        match msg {
            LauncherMsg::Open { blocked: false } if !self.shown => self.open(),
            LauncherMsg::Open { .. } => {}
            LauncherMsg::Toggle { .. } if self.shown => self.shown = false,
            LauncherMsg::Toggle { blocked: false } => self.open(),
            LauncherMsg::Toggle { .. } => {}
            LauncherMsg::Close => self.shown = false,
            LauncherMsg::Search if self.shown => self.search(),
            LauncherMsg::Search => {}
            LauncherMsg::Move(Move::Down(n)) => {
                self.set_current((self.current + n).min(self.results.len().saturating_sub(1)))
            }
            LauncherMsg::Move(Move::Up(n)) => self.set_current(self.current.saturating_sub(n)),
            LauncherMsg::Hover(y) => {
                let index = ((y + self.scroll.value()) / ROW_HEIGHT as f64).floor();
                if index >= 0.0 && (index as usize) < self.results.len() {
                    self.set_current(index as usize);
                }
            }
            LauncherMsg::Launch => self.launch_current(),
        }
        if self.shown != was_shown {
            let _ = sender.output(Shown(self.shown));
        }
    }
}

impl Launcher {
    fn open(&mut self) {
        self.apps = load_apps();
        self.query.set_text("");
        self.shown = true;
        self.focus.set(true);
        self.search();
    }

    fn set_current(&mut self, index: usize) {
        let mut rows = self.rows.guard();
        if let Some(row) = rows.get_mut(self.current) {
            row.current = false;
        }
        self.current = index;
        let Some(row) = rows.get_mut(index) else { return };
        row.current = true;
        let top = (index as i32 * ROW_HEIGHT) as f64;
        let bottom = top + ROW_HEIGHT as f64;
        let page = (VISIBLE_ROWS * ROW_HEIGHT) as f64;
        if top < self.scroll.value() {
            self.scroll.set_value(top);
        } else if bottom > self.scroll.value() + page {
            self.scroll.set_value(bottom - page);
        }
    }

    fn search(&mut self) {
        let query = self.query.text().trim().to_lowercase();
        let counts = &self.history.counts;
        let count = |app: &App| counts.get(&app.id).copied().unwrap_or(0);
        let mut results: Vec<(u8, usize)> = self
            .apps
            .iter()
            .enumerate()
            .filter_map(|(i, app)| {
                let rank = if query.is_empty() { Some(0) } else { match_rank(app, &query) };
                rank.map(|rank| (rank, i))
            })
            .collect();
        results.sort_by(|&(rank_a, a), &(rank_b, b)| {
            let (a, b) = (&self.apps[a], &self.apps[b]);
            rank_a
                .cmp(&rank_b)
                .then(count(b).cmp(&count(a)))
                .then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase()))
        });
        self.results = results.into_iter().map(|(_, i)| i).collect();

        let mut rows = self.rows.guard();
        rows.clear();
        for &i in &self.results {
            let app = &self.apps[i];
            let icon = app.info.icon().unwrap_or_else(|| gio::ThemedIcon::new("application-x-executable").upcast());
            rows.push_back((app.name.clone(), icon));
        }
        drop(rows);
        self.current = 0;
        self.scroll.set_value(0.0);
        self.set_current(0);
    }

    fn launch_current(&mut self) {
        let Some(&index) = self.results.get(self.current) else { return };
        let app = &self.apps[index];
        *self.history.counts.entry(app.id.clone()).or_default() += 1;
        let dir = state_dir();
        let _ = std::fs::create_dir_all(&dir);
        if let Ok(json) = serde_json::to_string(&self.history) {
            let _ = std::fs::write(dir.join("launcher.json"), json);
        }
        if self.config.launch_prefix.is_empty() {
            if let Err(e) = app.info.launch(&[], None::<&gio::AppLaunchContext>) {
                eprintln!("wakeshell: cannot launch {}: {e}", app.id);
            }
        } else {
            let mut argv = self.config.launch_prefix.clone();
            argv.push(format!("{}.desktop", app.id));
            util::spawn(&argv);
        }
        self.shown = false;
    }
}

struct Row {
    name: String,
    icon: gio::Icon,
    current: bool,
}

#[relm4::factory]
impl FactoryComponent for Row {
    type Init = (String, gio::Icon);
    type Input = ();
    type Output = ();
    type CommandOutput = ();
    type ParentWidget = gtk::Box;

    view! {
        gtk::Box {
            set_spacing: 12,
            add_css_class: "launcher-row",
            set_size_request: (-1, ROW_HEIGHT),
            #[watch]
            set_class_active: ("current", self.current),

            gtk::Image {
                set_from_gicon: &self.icon,
                set_pixel_size: 22,
            },
            gtk::Label {
                set_label: &self.name,
                set_ellipsize: gtk::pango::EllipsizeMode::End,
                set_xalign: 0.0,
                set_hexpand: true,
            },
        }
    }

    fn init_model((name, icon): Self::Init, _: &DynamicIndex, _: FactorySender<Self>) -> Self {
        Row { name, icon, current: false }
    }
}

fn load_apps() -> Vec<App> {
    gio::AppInfo::all()
        .into_iter()
        .filter(|info| info.should_show())
        .filter_map(|info| info.downcast::<gio_unix::DesktopAppInfo>().ok())
        .filter_map(|info| {
            let id = info.id()?.to_string();
            let id = id.strip_suffix(".desktop").unwrap_or(&id).to_owned();
            let program = info
                .commandline()
                .and_then(|c| c.to_str().and_then(|s| s.split(' ').next()).map(str::to_owned))
                .unwrap_or_default();
            let extra = [
                info.generic_name().map(|s| s.to_string()).unwrap_or_default(),
                info.keywords().iter().map(|k| k.to_string()).collect::<Vec<_>>().join(" "),
                info.description().map(|s| s.to_string()).unwrap_or_default(),
                program,
            ]
            .join(" ")
            .to_lowercase();
            Some(App { id, name: info.name().to_string(), extra, info })
        })
        .collect()
}

/// Lower is better
fn match_rank(app: &App, query: &str) -> Option<u8> {
    let name = app.name.to_lowercase();
    if name == query {
        return Some(0);
    }
    if name.starts_with(query) {
        return Some(1);
    }
    if name.split(|c: char| c.is_whitespace() || "-_.".contains(c)).any(|word| word.starts_with(query)) {
        return Some(2);
    }
    if name.contains(query) {
        return Some(3);
    }
    let mut wanted = query.chars().peekable();
    for c in name.chars() {
        if wanted.peek() == Some(&c) {
            wanted.next();
        }
        if wanted.peek().is_none() {
            return Some(4);
        }
    }
    app.extra.contains(query).then_some(5)
}
