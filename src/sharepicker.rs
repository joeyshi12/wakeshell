// Screen-share picker. `wakeshell share-chooser` passes the portal's "Monitor: ..." and
// "Window: ..." lines and waits for the chosen one (empty to cancel).
use std::{path::PathBuf, time::Duration};

use gtk::{gdk, glib, prelude::*};
use relm4::{
    RelmWidgetExt,
    factory::{FactoryVecDeque, Position, positions::GridPosition},
    prelude::*,
};

use crate::{
    config::runtime_dir,
    icon::Icon,
    ipc::Reply,
    monitors,
    util::{self, Events},
};

const TILE_WIDTH: i32 = 256;
const TILE_HEIGHT: i32 = 144;
const COLUMNS: usize = 3;

#[derive(Clone, Copy, PartialEq, Debug)]
enum Kind {
    Screen,
    Window,
}

#[derive(Clone, Debug)]
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
    captures_left: usize,
    /// Requests that came while this one was open, with their own lines. Chromium apps send
    /// several at once, and they all get the one answer.
    joined: Vec<(String, Reply)>,
}

pub struct SharePicker {
    pending: Option<Pending>,
    shown: bool,
    /// Bumped per request so stale previews are ignored
    generation: u32,
    screens: FactoryVecDeque<Tile>,
    windows: FactoryVecDeque<Tile>,
}

#[derive(Debug)]
pub enum SharePickerMsg {
    Pick(String, Reply),
    Captured { generation: u32, index: usize, ok: bool },
    Show(u32),
    Choose(String),
    Cancel,
}

#[derive(Debug)]
pub struct Shown(pub bool);

impl SharePicker {
    fn both(&self) -> bool {
        !self.screens.is_empty() && !self.windows.is_empty()
    }

    fn what(&self) -> &'static str {
        if self.both() {
            "screen or window"
        } else if self.screens.is_empty() {
            "window"
        } else {
            "screen"
        }
    }
}

#[relm4::component(pub)]
impl SimpleComponent for SharePicker {
    type Init = ();
    type Input = SharePickerMsg;
    type Output = Shown;

    view! {
        util::overlay_window("wakeshell-share-picker") -> gtk::Window {
            on_key[sender] => move |key, _| {
                if key != gdk::Key::Escape {
                    return false;
                }
                sender.input(SharePickerMsg::Cancel);
                true
            },

            #[name = "area"]
            gtk::Box {
                set_orientation: gtk::Orientation::Vertical,
                set_hexpand: true,
                set_vexpand: true,

                #[name = "card"]
                gtk::Box {
                    set_orientation: gtk::Orientation::Vertical,
                    add_css_class: "card",
                    set_halign: gtk::Align::Center,
                    set_valign: gtk::Align::Center,
                    set_vexpand: true,

                    gtk::Box {
                        add_css_class: "card-header",
                        gtk::Label {
                            set_css_classes: &["large", "bold"],
                            #[watch]
                            set_label: &format!("Choose a {} to share", model.what()),
                        },
                    },

                    gtk::ScrolledWindow {
                        set_policy: (gtk::PolicyType::Never, gtk::PolicyType::Automatic),
                        set_propagate_natural_height: true,
                        set_propagate_natural_width: true,
                        set_max_content_height: 640,

                        gtk::Box {
                            set_orientation: gtk::Orientation::Vertical,
                            set_spacing: 4,
                            set_margin_start: 20,
                            set_margin_end: 20,
                            set_margin_top: 8,
                            set_margin_bottom: 20,

                            gtk::Box {
                                set_orientation: gtk::Orientation::Vertical,
                                set_spacing: 8,
                                #[watch]
                                set_visible: !model.screens.is_empty(),
                                #[watch]
                                set_margin_top: if model.both() { 0 } else { 4 },

                                gtk::Label {
                                    set_label: "Screens",
                                    add_css_class: "muted",
                                    set_xalign: 0.0,
                                    set_margin_top: 12,
                                    #[watch]
                                    set_visible: model.both(),
                                },
                                #[local_ref]
                                screens -> gtk::Grid {
                                    set_row_spacing: 12,
                                    set_column_spacing: 12,
                                },
                            },
                            gtk::Box {
                                set_orientation: gtk::Orientation::Vertical,
                                set_spacing: 8,
                                #[watch]
                                set_visible: !model.windows.is_empty(),
                                #[watch]
                                set_margin_top: if model.both() { 0 } else { 4 },

                                gtk::Label {
                                    set_label: "Windows",
                                    add_css_class: "muted",
                                    set_xalign: 0.0,
                                    set_margin_top: 12,
                                    #[watch]
                                    set_visible: model.both(),
                                },
                                #[local_ref]
                                windows -> gtk::Grid {
                                    set_row_spacing: 12,
                                    set_column_spacing: 12,
                                },
                            },
                        },
                    },

                    gtk::Box {
                        add_css_class: "separator-line",
                    },
                    gtk::Box {
                        set_spacing: 12,
                        set_margin_all: 12,
                        set_margin_start: 20,
                        set_margin_end: 20,

                        gtk::Label {
                            add_css_class: "muted",
                            set_hexpand: true,
                            set_xalign: 0.0,
                            #[watch]
                            set_label: &format!("Click a {} to share it", model.what()),
                        },
                        gtk::Label {
                            set_label: "Cancel",
                            add_css_class: "dialog-button",
                            set_cursor_from_name: Some("pointer"),
                            on_left_click[sender] => move || sender.input(SharePickerMsg::Cancel),
                        },
                    },
                },
            },

            #[watch]
            set_visible: model.shown,
        }
    }

    fn init(_: (), root: Self::Root, sender: ComponentSender<Self>) -> ComponentParts<Self> {
        let tiles = || {
            FactoryVecDeque::builder()
                .launch(gtk::Grid::default())
                .forward(sender.input_sender(), SharePickerMsg::Choose)
        };
        let model = SharePicker { pending: None, shown: false, generation: 0, screens: tiles(), windows: tiles() };
        let screens = model.screens.widget();
        let windows = model.windows.widget();
        let widgets = view_output!();
        let cancel = sender.clone();
        widgets.area.on_click_outside(&[widgets.card.clone().upcast()], move || cancel.input(SharePickerMsg::Cancel));
        ComponentParts { model, widgets }
    }

    fn update(&mut self, msg: SharePickerMsg, sender: ComponentSender<Self>) {
        let was_shown = self.shown;
        match msg {
            SharePickerMsg::Pick(text, reply) => self.pick(&text, reply, &sender),
            SharePickerMsg::Captured { generation, index, ok } if generation == self.generation => {
                let Some(pending) = &mut self.pending else { return };
                pending.sources[index].has_preview = ok;
                pending.captures_left -= 1;
                if pending.captures_left == 0 {
                    self.show();
                }
            }
            SharePickerMsg::Captured { .. } => {}
            SharePickerMsg::Show(generation) if generation == self.generation => self.show(),
            SharePickerMsg::Show(_) => {}
            SharePickerMsg::Choose(line) => self.finish(&line),
            SharePickerMsg::Cancel => self.finish(""),
        }
        if self.shown != was_shown {
            let _ = sender.output(Shown(self.shown));
        }
    }
}

impl SharePicker {
    fn pick(&mut self, text: &str, reply: Reply, sender: &ComponentSender<Self>) {
        if let Some(pending) = &mut self.pending {
            pending.joined.push((text.to_owned(), reply));
            return;
        }
        self.generation += 1;
        let generation = self.generation;
        let sources = parse(text, generation);
        if sources.is_empty() {
            reply.send("");
            return;
        }
        // Before the picker shows, so it is not in the previews
        for (index, source) in sources.iter().enumerate() {
            let (target, path) = (source.target.clone(), source.preview.to_string_lossy().into_owned());
            let sender = sender.clone();
            glib::spawn_future_local(async move {
                let ok = !target.is_empty() && {
                    let mut argv: Vec<&str> = vec!["grim", "-s", "0.15"];
                    argv.extend(target.iter().map(String::as_str));
                    argv.push(&path);
                    util::run(&argv).await
                };
                sender.input(SharePickerMsg::Captured { generation, index, ok });
            });
        }
        let captures_left = sources.len();
        self.pending = Some(Pending { reply, sources, captures_left, joined: Vec::new() });
        let sender = sender.clone();
        glib::timeout_add_local_once(Duration::from_secs(3), move || sender.input(SharePickerMsg::Show(generation)));
    }

    fn show(&mut self) {
        let Some(pending) = &self.pending else { return };
        if self.shown {
            return;
        }
        let mut screens = self.screens.guard();
        let mut windows = self.windows.guard();
        screens.clear();
        windows.clear();
        for source in &pending.sources {
            match source.kind {
                Kind::Screen => screens.push_back(source.clone()),
                Kind::Window => windows.push_back(source.clone()),
            };
        }
        drop((screens, windows));
        self.shown = true;
    }

    fn finish(&mut self, line: &str) {
        self.shown = false;
        if let Some(pending) = self.pending.take() {
            for source in &pending.sources {
                let _ = std::fs::remove_file(&source.preview);
            }
            pending.reply.send(line);
            for (text, reply) in pending.joined {
                // Its own line for the same output or window, in case a title changed meanwhile
                let key = source_key(line);
                let own = text.lines().find(|l| *l == line).or_else(|| text.lines().find(|l| key.is_some() && source_key(l) == key));
                reply.send(own.unwrap_or(""));
            }
        }
    }
}

/// The output name or window identifier a portal line names
fn source_key(line: &str) -> Option<&str> {
    if let Some(rest) = line.strip_prefix("Monitor: ") {
        return rest.split(' ').next().filter(|n| !n.is_empty());
    }
    let rest = line.strip_prefix("Window: ")?.strip_suffix(')')?;
    let id = &rest[rest.rfind(" (")? + 2..];
    (!id.is_empty() && !id.contains(['(', ')'])).then_some(id)
}

struct Tile {
    source: Source,
    texture: Option<gdk::Texture>,
}

impl Position<GridPosition, DynamicIndex> for Tile {
    fn position(&self, index: &DynamicIndex) -> GridPosition {
        let i = index.current_index();
        GridPosition { column: (i % COLUMNS) as i32, row: (i / COLUMNS) as i32, width: 1, height: 1 }
    }
}

#[relm4::factory]
impl FactoryComponent for Tile {
    type Init = Source;
    type Input = ();
    type Output = String;
    type CommandOutput = ();
    type ParentWidget = gtk::Grid;

    view! {
        gtk::Box {
            set_orientation: gtk::Orientation::Vertical,
            set_spacing: 6,
            add_css_class: "share-tile",
            set_size_request: (TILE_WIDTH + 24, -1),
            // Labels inside ask to expand; keep the tiles at their own size
            set_hexpand: false,
            set_vexpand: false,
            set_cursor_from_name: Some("pointer"),
            on_left_click[sender, line = self.source.line.clone()] => move || {
                let _ = sender.output(line.clone());
            },

            gtk::Box {
                set_orientation: gtk::Orientation::Vertical,
                add_css_class: "share-preview",
                set_size_request: (TILE_WIDTH, TILE_HEIGHT),
                set_overflow: gtk::Overflow::Hidden,

                gtk::Picture {
                    set_visible: self.texture.is_some(),
                    set_paintable: self.texture.as_ref(),
                    set_content_fit: gtk::ContentFit::Contain,
                    set_can_shrink: true,
                    set_size_request: (TILE_WIDTH, TILE_HEIGHT),
                },
                Icon::new(if self.source.kind == Kind::Screen { "monitor" } else { "app-window" }, 40) {
                    set_visible: self.texture.is_none(),
                    set_vexpand: true,
                    set_valign: gtk::Align::Center,
                },
            },
            gtk::Label {
                set_label: &self.source.name,
                set_ellipsize: gtk::pango::EllipsizeMode::End,
                set_max_width_chars: 1,
                set_hexpand: true,
                set_margin_top: 2,
            },
            gtk::Label {
                set_visible: !self.source.detail.is_empty(),
                set_label: &self.source.detail,
                set_css_classes: &["small", "muted"],
                set_ellipsize: gtk::pango::EllipsizeMode::End,
                set_max_width_chars: 1,
            },
        }
    }

    fn init_model(source: Source, _: &DynamicIndex, _: FactorySender<Self>) -> Self {
        let texture = source.has_preview.then(|| gdk::Texture::from_filename(&source.preview).ok()).flatten();
        Tile { source, texture }
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
