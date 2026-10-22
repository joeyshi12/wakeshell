// Lucide icons from icons/, used as masks so they take the CSS colour.
// $XDG_CONFIG_HOME/wakeshell/icons/<name>.svg overrides a built-in icon.
use std::{cell::RefCell, collections::HashMap};

use gtk::{gdk, glib, graphene, prelude::*, subclass::prelude::*};
use resvg::{tiny_skia, usvg};

include!(concat!(env!("OUT_DIR"), "/icons.rs"));

thread_local! {
    static TEXTURES: RefCell<HashMap<(String, i32), Option<gdk::Texture>>> = RefCell::new(HashMap::new());
}

fn svg_source(name: &str) -> Option<String> {
    let path = crate::config::config_dir().join("icons").join(format!("{name}.svg"));
    if let Ok(svg) = std::fs::read_to_string(path) {
        return Some(svg);
    }
    ICONS.iter().find(|(n, _)| *n == name).map(|(_, svg)| svg.to_string())
}

fn render(name: &str, px: i32) -> Option<gdk::Texture> {
    let svg = svg_source(name)?;
    let tree = usvg::Tree::from_str(&svg, &usvg::Options::default()).ok()?;
    let mut pixmap = tiny_skia::Pixmap::new(px as u32, px as u32)?;
    let size = tree.size();
    let transform =
        tiny_skia::Transform::from_scale(px as f32 / size.width(), px as f32 / size.height());
    resvg::render(&tree, transform, &mut pixmap.as_mut());
    let bytes = glib::Bytes::from_owned(pixmap.take());
    Some(
        gdk::MemoryTexture::new(
            px,
            px,
            gdk::MemoryFormat::R8g8b8a8Premultiplied,
            &bytes,
            px as usize * 4,
        )
        .upcast(),
    )
}

fn texture(name: &str, px: i32) -> Option<gdk::Texture> {
    TEXTURES.with(|cache| {
        cache
            .borrow_mut()
            .entry((name.to_owned(), px))
            .or_insert_with(|| {
                let texture = render(name, px);
                if texture.is_none() {
                    eprintln!("wakeshell: no icon named {name}");
                }
                texture
            })
            .clone()
    })
}

mod imp {
    use super::*;

    #[derive(Default)]
    pub struct Icon {
        pub name: RefCell<String>,
        pub size: std::cell::Cell<i32>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for Icon {
        const NAME: &'static str = "WakeshellIcon";
        type Type = super::Icon;
        type ParentType = gtk::Widget;

        fn class_init(klass: &mut Self::Class) {
            klass.set_css_name("icon");
        }
    }

    impl ObjectImpl for Icon {}

    impl WidgetImpl for Icon {
        fn measure(&self, _orientation: gtk::Orientation, _for_size: i32) -> (i32, i32, i32, i32) {
            let size = self.size.get();
            (size, size, -1, -1)
        }

        fn snapshot(&self, snapshot: &gtk::Snapshot) {
            let widget = self.obj();
            let size = self.size.get();
            let Some(texture) = texture(&self.name.borrow(), size * widget.scale_factor()) else {
                return;
            };
            let x = ((widget.width() - size) / 2) as f32;
            let y = ((widget.height() - size) / 2) as f32;
            let bounds = graphene::Rect::new(x, y, size as f32, size as f32);
            snapshot.push_mask(gtk::gsk::MaskMode::Alpha);
            snapshot.append_texture(&texture, &bounds);
            snapshot.pop();
            snapshot.append_color(&widget.color(), &bounds);
            snapshot.pop();
        }
    }
}

glib::wrapper! {
    pub struct Icon(ObjectSubclass<imp::Icon>) @extends gtk::Widget, @implements gtk::Accessible, gtk::Buildable, gtk::ConstraintTarget;
}

impl Icon {
    pub fn new(name: &str, size: i32) -> Icon {
        let icon: Icon = glib::Object::new();
        icon.imp().size.set(size);
        icon.set_icon(name);
        icon
    }

    pub fn set_icon(&self, name: &str) {
        if *self.imp().name.borrow() != name {
            self.imp().name.replace(name.to_owned());
            self.queue_draw();
        }
    }
}
