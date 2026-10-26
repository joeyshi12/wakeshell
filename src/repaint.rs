// A box GTK redraws whole when anything in it changes. GTK sizes text by unhinted glyphs but
// draws them hinted, so a text change can leave stale or missing dots a pixel above the text.
use gtk::{gdk, glib, graphene, prelude::*, subclass::prelude::*};

mod imp {
    use super::*;

    #[derive(Default)]
    pub struct RepaintBox;

    #[glib::object_subclass]
    impl ObjectSubclass for RepaintBox {
        const NAME: &'static str = "WakeshellRepaintBox";
        type Type = super::RepaintBox;
        type ParentType = gtk::Box;
    }

    impl ObjectImpl for RepaintBox {}

    impl WidgetImpl for RepaintBox {
        fn snapshot(&self, snapshot: &gtk::Snapshot) {
            self.parent_snapshot(snapshot);
            let widget = self.obj();
            // A new texture never matches the last frame's, so GTK damages the whole box
            let blank = gdk::MemoryTexture::new(
                1,
                1,
                gdk::MemoryFormat::B8g8r8a8Premultiplied,
                &glib::Bytes::from_static(&[0; 4]),
                4,
            );
            let bounds = graphene::Rect::new(0.0, 0.0, widget.width() as f32, widget.height() as f32);
            snapshot.append_texture(&blank, &bounds);
        }
    }

    impl BoxImpl for RepaintBox {}
}

glib::wrapper! {
    pub struct RepaintBox(ObjectSubclass<imp::RepaintBox>)
        @extends gtk::Box, gtk::Widget,
        @implements gtk::Accessible, gtk::Buildable, gtk::ConstraintTarget, gtk::Orientable;
}

impl Default for RepaintBox {
    fn default() -> RepaintBox {
        glib::Object::new()
    }
}

impl relm4::ContainerChild for RepaintBox {
    type Child = gtk::Widget;
}

impl relm4::RelmContainerExt for RepaintBox {
    fn container_add(&self, widget: &impl AsRef<gtk::Widget>) {
        self.append(widget.as_ref());
    }
}
