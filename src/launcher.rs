// App launcher. Launch counts in $XDG_STATE_HOME/wakeshell/launcher.json rank the results.
use std::{
    cell::{Cell, RefCell},
    collections::HashMap,
    rc::Rc,
};

use gtk::{gdk, gio, prelude::*};
use serde::{Deserialize, Serialize};

use crate::{
    config::{Config, state_dir},
    util,
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

struct Ui {
    window: gtk::Window,
    input: gtk::Text,
    scrolled: gtk::ScrolledWindow,
    list: gtk::Box,
    empty: gtk::Label,
    apps: Vec<App>,
    results: RefCell<Vec<usize>>,
    rows: RefCell<Vec<gtk::Box>>,
    current: Cell<usize>,
}

pub struct Launcher {
    config: Rc<Config>,
    blocked: Rc<dyn Fn() -> bool>,
    ui: RefCell<Option<Rc<Ui>>>,
    history: RefCell<History>,
}

impl Launcher {
    pub fn new(config: Rc<Config>, blocked: Rc<dyn Fn() -> bool>) -> Rc<Launcher> {
        let history = std::fs::read_to_string(state_dir().join("launcher.json"))
            .ok()
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or_default();
        Rc::new(Launcher { config, blocked, ui: RefCell::new(None), history: RefCell::new(history) })
    }

    pub fn toggle(self: &Rc<Self>) {
        if self.ui.borrow().is_some() {
            self.close();
        } else {
            self.open();
        }
    }

    pub fn close(&self) {
        if let Some(ui) = self.ui.take() {
            ui.window.destroy();
        }
    }

    pub fn open(self: &Rc<Self>) {
        if self.ui.borrow().is_some() || (self.blocked)() {
            return;
        }
        let window = util::overlay_window("wakeshell-launcher");

        let card = gtk::Box::new(gtk::Orientation::Vertical, 0);
        card.add_css_class("card");
        card.set_halign(gtk::Align::Center);
        card.set_valign(gtk::Align::Center);
        card.set_size_request(600, SEARCH_HEIGHT + 1 + VISIBLE_ROWS * ROW_HEIGHT + 16);

        let search = gtk::Box::new(gtk::Orientation::Horizontal, 8);
        search.add_css_class("launcher-search");
        search.set_size_request(-1, SEARCH_HEIGHT - 6);
        let input = gtk::Text::new();
        input.set_placeholder_text(Some("Search..."));
        input.set_hexpand(true);
        search.append(&input);
        search.append(&util::label("Launch", &["launcher-prompt"]));
        card.append(&search);

        let separator = util::separator();
        separator.set_margin_start(11);
        separator.set_margin_end(11);
        card.append(&separator);

        let scrolled = gtk::ScrolledWindow::new();
        scrolled.set_policy(gtk::PolicyType::Never, gtk::PolicyType::External);
        scrolled.set_size_request(-1, VISIBLE_ROWS * ROW_HEIGHT);
        scrolled.set_margin_top(8);
        scrolled.set_margin_start(7);
        scrolled.set_margin_end(7);
        let stack = gtk::Overlay::new();
        let list = gtk::Box::new(gtk::Orientation::Vertical, 0);
        list.set_valign(gtk::Align::Start);
        stack.set_child(Some(&list));
        scrolled.set_child(Some(&stack));
        let empty = util::label("No matches", &["launcher-empty"]);
        let list_area = gtk::Overlay::new();
        list_area.set_child(Some(&scrolled));
        list_area.add_overlay(&empty);
        empty.set_halign(gtk::Align::Center);
        empty.set_valign(gtk::Align::Center);
        empty.set_can_target(false);
        card.append(&list_area);

        let root = gtk::Box::new(gtk::Orientation::Vertical, 0);
        root.append(&card);
        root.set_hexpand(true);
        root.set_vexpand(true);
        card.set_vexpand(true);
        window.set_child(Some(&root));

        let ui = Rc::new(Ui {
            window: window.clone(),
            input: input.clone(),
            scrolled: scrolled.clone(),
            list,
            empty,
            apps: load_apps(),
            results: RefCell::new(Vec::new()),
            rows: RefCell::new(Vec::new()),
            current: Cell::new(0),
        });
        self.ui.replace(Some(ui.clone()));
        self.search();

        let weak = Rc::downgrade(self);
        util::on_click_outside(&root, &[card.clone().upcast()], move || {
            if let Some(launcher) = weak.upgrade() {
                launcher.close();
            }
        });

        let weak = Rc::downgrade(self);
        input.connect_changed(move |_| {
            if let Some(launcher) = weak.upgrade() {
                launcher.search();
            }
        });

        let weak = Rc::downgrade(self);
        util::on_key(&input, move |key, modifiers| {
            let Some(launcher) = weak.upgrade() else { return false };
            let ctrl = modifiers.contains(gdk::ModifierType::CONTROL_MASK);
            let n = launcher.result_count();
            let current = launcher.current();
            match key {
                gdk::Key::Escape => launcher.close(),
                gdk::Key::Return | gdk::Key::KP_Enter => launcher.launch_current(),
                gdk::Key::Down | gdk::Key::Tab => launcher.set_current((current + 1).min(n.saturating_sub(1))),
                gdk::Key::n if ctrl => launcher.set_current((current + 1).min(n.saturating_sub(1))),
                gdk::Key::Up | gdk::Key::ISO_Left_Tab => launcher.set_current(current.saturating_sub(1)),
                gdk::Key::p if ctrl => launcher.set_current(current.saturating_sub(1)),
                gdk::Key::Page_Down => {
                    launcher.set_current((current + VISIBLE_ROWS as usize).min(n.saturating_sub(1)))
                }
                gdk::Key::Page_Up => launcher.set_current(current.saturating_sub(VISIBLE_ROWS as usize)),
                _ => return false,
            }
            true
        });

        let weak = Rc::downgrade(self);
        util::on_pointer_moved(&scrolled, move |_, y| {
            if let Some(launcher) = weak.upgrade()
                && let Some(index) = launcher.row_at(y) {
                    launcher.set_current(index);
                }
        });
        let weak = Rc::downgrade(self);
        util::on_click(&scrolled, move |button| {
            let Some(launcher) = weak.upgrade() else { return };
            if button == 1 {
                launcher.launch_current();
            }
        });
        util::hand_cursor(&scrolled);

        window.present();
        input.grab_focus();
    }

    fn result_count(&self) -> usize {
        self.ui.borrow().as_ref().map_or(0, |ui| ui.results.borrow().len())
    }

    fn current(&self) -> usize {
        self.ui.borrow().as_ref().map_or(0, |ui| ui.current.get())
    }

    fn row_at(&self, y: f64) -> Option<usize> {
        let ui = self.ui.borrow().clone()?;
        let y = y + ui.scrolled.vadjustment().value();
        let index = (y / ROW_HEIGHT as f64).floor();
        (index >= 0.0 && (index as usize) < ui.results.borrow().len()).then_some(index as usize)
    }

    fn set_current(&self, index: usize) {
        let Some(ui) = self.ui.borrow().clone() else { return };
        let rows = ui.rows.borrow();
        if let Some(row) = rows.get(ui.current.get()) {
            row.remove_css_class("current");
        }
        ui.current.set(index);
        let Some(row) = rows.get(index) else { return };
        row.add_css_class("current");
        let adjustment = ui.scrolled.vadjustment();
        let top = (index as i32 * ROW_HEIGHT) as f64;
        let bottom = top + ROW_HEIGHT as f64;
        let page = (VISIBLE_ROWS * ROW_HEIGHT) as f64;
        if top < adjustment.value() {
            adjustment.set_value(top);
        } else if bottom > adjustment.value() + page {
            adjustment.set_value(bottom - page);
        }
    }

    fn search(&self) {
        let Some(ui) = self.ui.borrow().clone() else { return };
        let query = ui.input.text().trim().to_lowercase();
        let counts = &self.history.borrow().counts;
        let count = |app: &App| counts.get(&app.id).copied().unwrap_or(0);
        let mut results: Vec<(u8, usize)> = ui
            .apps
            .iter()
            .enumerate()
            .filter_map(|(i, app)| {
                let rank = if query.is_empty() { Some(0) } else { match_rank(app, &query) };
                rank.map(|rank| (rank, i))
            })
            .collect();
        results.sort_by(|&(rank_a, a), &(rank_b, b)| {
            let (a, b) = (&ui.apps[a], &ui.apps[b]);
            rank_a
                .cmp(&rank_b)
                .then(count(b).cmp(&count(a)))
                .then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase()))
        });

        while let Some(child) = ui.list.first_child() {
            ui.list.remove(&child);
        }
        let mut rows = Vec::new();
        for &(_, i) in &results {
            let app = &ui.apps[i];
            let row = gtk::Box::new(gtk::Orientation::Horizontal, 12);
            row.add_css_class("launcher-row");
            row.set_size_request(-1, ROW_HEIGHT);
            let image = match app.info.icon() {
                Some(icon) => gtk::Image::from_gicon(&icon),
                None => gtk::Image::from_icon_name("application-x-executable"),
            };
            image.set_pixel_size(22);
            row.append(&image);
            let name = util::label(&app.name, &[]);
            name.set_ellipsize(gtk::pango::EllipsizeMode::End);
            name.set_xalign(0.0);
            name.set_hexpand(true);
            row.append(&name);
            ui.list.append(&row);
            rows.push(row);
        }
        ui.empty.set_visible(rows.is_empty());
        ui.results.replace(results.into_iter().map(|(_, i)| i).collect());
        ui.rows.replace(rows);
        ui.current.set(0);
        ui.scrolled.vadjustment().set_value(0.0);
        self.set_current(0);
    }

    fn launch_current(&self) {
        let Some(ui) = self.ui.borrow().clone() else { return };
        let Some(&index) = ui.results.borrow().get(ui.current.get()) else { return };
        let app = &ui.apps[index];
        {
            let mut history = self.history.borrow_mut();
            *history.counts.entry(app.id.clone()).or_default() += 1;
            let dir = state_dir();
            let _ = std::fs::create_dir_all(&dir);
            if let Ok(json) = serde_json::to_string(&*history) {
                let _ = std::fs::write(dir.join("launcher.json"), json);
            }
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
        self.close();
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
