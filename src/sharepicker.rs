// Screen-share picker. `wakeshell share-chooser` passes the portal's "Monitor: ..." and
// "Window: ..." lines and waits for the chosen one (empty to cancel).
use std::{
    cell::{Cell, RefCell},
    path::PathBuf,
    rc::Rc,
    time::Duration,
};

use gtk::{gdk, glib, prelude::*};

use crate::{config::runtime_dir, icon::Icon, ipc::Reply, monitors, util};

const TILE_WIDTH: i32 = 256;
const TILE_HEIGHT: i32 = 144;

#[derive(Clone, Copy, PartialEq)]
enum Kind {
    Screen,
    Window,
}

#[derive(Clone)]
struct Source {
    line: String,
    kind: Kind,
    name: String,
    detail: String,
    target: Vec<String>,
    preview: PathBuf,
    has_preview: bool,
}

struct Pending {
    reply: Reply,
    sources: Vec<Source>,
}

pub struct SharePicker {
    on_open: Rc<dyn Fn()>,
    pending: RefCell<Option<Pending>>,
    window: RefCell<Option<gtk::Window>>,
    /// Bumped per request so stale previews are ignored
    generation: Cell<u32>,
}

impl SharePicker {
    pub fn new(on_open: Rc<dyn Fn()>) -> Rc<SharePicker> {
        Rc::new(SharePicker {
            on_open,
            pending: RefCell::new(None),
            window: RefCell::new(None),
            generation: Cell::new(0),
        })
    }

    pub fn shown(&self) -> bool {
        self.window.borrow().is_some()
    }

    pub fn pick(self: &Rc<Self>, text: &str, reply: Reply) {
        self.finish("");
        let generation = self.generation.get() + 1;
        self.generation.set(generation);
        let sources = parse(text, generation);
        if sources.is_empty() {
            reply.send("");
            return;
        }
        // Before the picker shows, so it is not in the previews
        let pending_captures = Rc::new(Cell::new(sources.len()));
        let captures: Vec<(usize, Vec<String>, PathBuf)> =
            sources.iter().enumerate().map(|(i, s)| (i, s.target.clone(), s.preview.clone())).collect();
        self.pending.replace(Some(Pending { reply, sources }));
        for (i, target, path) in captures {
            let weak = Rc::downgrade(self);
            let pending_captures = pending_captures.clone();
            glib::spawn_future_local(async move {
                let ok = if target.is_empty() {
                    false
                } else {
                    let path = path.to_string_lossy().into_owned();
                    let mut argv: Vec<&str> = vec!["grim", "-s", "0.15"];
                    argv.extend(target.iter().map(String::as_str));
                    argv.push(&path);
                    util::run(&argv).await
                };
                let Some(picker) = weak.upgrade() else { return };
                if picker.generation.get() != generation {
                    return;
                }
                if let Some(pending) = picker.pending.borrow_mut().as_mut() {
                    pending.sources[i].has_preview = ok;
                }
                pending_captures.set(pending_captures.get() - 1);
                if pending_captures.get() == 0 {
                    picker.show(generation);
                }
            });
        }
        let weak = Rc::downgrade(self);
        glib::timeout_add_local_once(Duration::from_secs(3), move || {
            if let Some(picker) = weak.upgrade() {
                picker.show(generation);
            }
        });
    }

    fn finish(&self, line: &str) {
        if let Some(window) = self.window.take() {
            window.destroy();
        }
        if let Some(pending) = self.pending.take() {
            for source in &pending.sources {
                let _ = std::fs::remove_file(&source.preview);
            }
            pending.reply.send(line);
        }
    }

    fn show(self: &Rc<Self>, generation: u32) {
        if self.generation.get() != generation || self.shown() {
            return;
        }
        let Some(sources) = self.pending.borrow().as_ref().map(|p| p.sources.clone()) else { return };
        (self.on_open)();
        let window = util::overlay_window("wakeshell-share-picker");
        let screens: Vec<&Source> = sources.iter().filter(|s| s.kind == Kind::Screen).collect();
        let windows: Vec<&Source> = sources.iter().filter(|s| s.kind == Kind::Window).collect();
        let both = !screens.is_empty() && !windows.is_empty();
        let what = if both {
            "screen or window"
        } else if screens.is_empty() {
            "window"
        } else {
            "screen"
        };

        let card = gtk::Box::new(gtk::Orientation::Vertical, 0);
        card.add_css_class("card");
        card.set_halign(gtk::Align::Center);
        card.set_valign(gtk::Align::Center);

        let header = gtk::Box::new(gtk::Orientation::Horizontal, 0);
        header.add_css_class("card-header");
        header.append(&util::label(&format!("Choose a {what} to share"), &["large", "bold"]));
        card.append(&header);

        let sections = gtk::Box::new(gtk::Orientation::Vertical, 4);
        sections.set_margin_start(20);
        sections.set_margin_end(20);
        sections.set_margin_top(8);
        sections.set_margin_bottom(20);
        for (label, items) in [("Screens", &screens), ("Windows", &windows)] {
            if items.is_empty() {
                continue;
            }
            let section = gtk::Box::new(gtk::Orientation::Vertical, 8);
            if both {
                let title = util::label(label, &["muted"]);
                title.set_xalign(0.0);
                title.set_margin_top(12);
                section.append(&title);
            } else {
                section.set_margin_top(4);
            }
            let grid = gtk::Grid::new();
            grid.set_row_spacing(12);
            grid.set_column_spacing(12);
            for (i, source) in items.iter().enumerate() {
                let tile = self.tile(source);
                grid.attach(&tile, (i % 3) as i32, (i / 3) as i32, 1, 1);
            }
            section.append(&grid);
            sections.append(&section);
        }
        let scrolled = gtk::ScrolledWindow::new();
        scrolled.set_policy(gtk::PolicyType::Never, gtk::PolicyType::Automatic);
        scrolled.set_propagate_natural_height(true);
        scrolled.set_propagate_natural_width(true);
        scrolled.set_max_content_height(640);
        scrolled.set_child(Some(&sections));
        card.append(&scrolled);

        card.append(&util::separator());
        let footer = gtk::Box::new(gtk::Orientation::Horizontal, 12);
        footer.set_margin_top(12);
        footer.set_margin_bottom(12);
        footer.set_margin_start(20);
        footer.set_margin_end(20);
        let hint = util::label(&format!("Click a {what} to share it"), &["muted"]);
        hint.set_hexpand(true);
        hint.set_xalign(0.0);
        footer.append(&hint);
        let weak = Rc::downgrade(self);
        footer.append(&util::dialog_button("Cancel", false, move || {
            if let Some(picker) = weak.upgrade() {
                picker.finish("");
            }
        }));
        card.append(&footer);

        let root = gtk::Box::new(gtk::Orientation::Vertical, 0);
        root.set_hexpand(true);
        root.set_vexpand(true);
        card.set_vexpand(true);
        root.append(&card);
        window.set_child(Some(&root));
        let weak = Rc::downgrade(self);
        util::on_click_outside(&root, &[card.clone().upcast()], move || {
            if let Some(picker) = weak.upgrade() {
                picker.finish("");
            }
        });
        let weak = Rc::downgrade(self);
        util::on_key(&window, move |key, _| {
            if key == gdk::Key::Escape {
                if let Some(picker) = weak.upgrade() {
                    picker.finish("");
                }
                return true;
            }
            false
        });
        window.present();
        self.window.replace(Some(window));
    }

    fn tile(self: &Rc<Self>, source: &Source) -> gtk::Box {
        let tile = gtk::Box::new(gtk::Orientation::Vertical, 6);
        tile.add_css_class("share-tile");
        tile.set_size_request(TILE_WIDTH + 24, -1);
        // Labels inside ask to expand; keep the tiles at their own size
        tile.set_hexpand(false);
        tile.set_vexpand(false);
        util::hand_cursor(&tile);

        let preview = gtk::Box::new(gtk::Orientation::Vertical, 0);
        preview.add_css_class("share-preview");
        preview.set_size_request(TILE_WIDTH, TILE_HEIGHT);
        preview.set_overflow(gtk::Overflow::Hidden);
        let texture = source.has_preview.then(|| gdk::Texture::from_filename(&source.preview).ok()).flatten();
        match texture {
            Some(texture) => {
                let picture = gtk::Picture::for_paintable(&texture);
                picture.set_content_fit(gtk::ContentFit::Contain);
                picture.set_can_shrink(true);
                picture.set_size_request(TILE_WIDTH, TILE_HEIGHT);
                preview.append(&picture);
            }
            None => {
                let icon = Icon::new(if source.kind == Kind::Screen { "monitor" } else { "app-window" }, 40);
                icon.set_vexpand(true);
                icon.set_valign(gtk::Align::Center);
                preview.append(&icon);
            }
        }
        tile.append(&preview);

        let name = util::label(&source.name, &[]);
        name.set_ellipsize(gtk::pango::EllipsizeMode::End);
        name.set_max_width_chars(1);
        name.set_hexpand(true);
        name.set_margin_top(2);
        tile.append(&name);
        if !source.detail.is_empty() {
            let detail = util::label(&source.detail, &["small", "muted"]);
            detail.set_ellipsize(gtk::pango::EllipsizeMode::End);
            detail.set_max_width_chars(1);
            tile.append(&detail);
        }

        let weak = Rc::downgrade(self);
        let line = source.line.clone();
        util::on_click(&tile, move |button| {
            if button == 1
                && let Some(picker) = weak.upgrade() {
                    picker.finish(&line);
                }
        });
        tile
    }
}

fn parse(text: &str, generation: u32) -> Vec<Source> {
    let monitors = monitors::all();
    let mut sources = Vec::new();
    for line in text.lines() {
        if let Some(rest) = line.strip_prefix("Monitor: ") {
            let name = rest.split(' ').next().unwrap_or("").to_owned();
            let size = monitors.iter().find(|m| m.connector().is_some_and(|c| c == name)).map(|m| {
                let geometry = m.geometry();
                let scale = m.scale();
                format!("{}×{}", (geometry.width() as f64 * scale).round(), (geometry.height() as f64 * scale).round())
            });
            sources.push(Source {
                line: line.to_owned(),
                kind: Kind::Screen,
                name: "Entire screen".into(),
                detail: size.map_or(name.clone(), |size| format!("{name}, {size}")),
                target: vec!["-o".into(), name],
                preview: PathBuf::new(),
                has_preview: false,
            });
        } else if let Some(rest) = line.strip_prefix("Window: ") {
            // The title may contain parentheses itself
            let parsed = rest.strip_suffix(')').and_then(|r| {
                let open = r.rfind(" (")?;
                let id = &r[open + 2..];
                (!id.contains(['(', ')'])).then(|| (r[..open].trim().to_owned(), id.to_owned()))
            });
            let (title, target) = match parsed {
                Some((title, id)) => (title, vec!["-T".to_owned(), id]),
                None => (rest.trim().to_owned(), Vec::new()),
            };
            sources.push(Source {
                line: line.to_owned(),
                kind: Kind::Window,
                name: if title.is_empty() { "Untitled window".into() } else { title },
                detail: String::new(),
                target,
                preview: PathBuf::new(),
                has_preview: false,
            });
        }
    }
    sources.sort_by_key(|s| s.kind == Kind::Window);
    let dir = runtime_dir();
    for (i, source) in sources.iter_mut().enumerate() {
        source.preview = dir.join(format!("share-{generation}-{i}.png"));
    }
    sources
}
