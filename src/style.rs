// The stylesheet: colors.css (generated from the dotfiles' palette), then style.css.
use gtk::gdk;

use crate::config::config_dir;

const DEFAULT_COLORS: &str = "
@define-color bg #0f1825;
@define-color bg_alt #253447;
@define-color accent #418aa2;
@define-color fg #e8f4f8;
@define-color fg_dim #6b8494;
@define-color fg_muted #96a3ae;
@define-color urgent #f4d06f;
@define-color negative #e86e6e;
@define-color positive #82c896;
@define-color shadow #05090f;
";

pub fn load() {
    // Automatic font rendering clips the top row of some glyphs with the GPU renderers
    if let Some(settings) = gtk::Settings::default() {
        settings.set_gtk_font_rendering(gtk::FontRendering::Manual);
    }
    let colors = std::fs::read_to_string(config_dir().join("colors.css")).unwrap_or_default();
    let css = format!("{DEFAULT_COLORS}\n{colors}\n{}", include_str!("style.css"));
    let provider = gtk::CssProvider::new();
    provider.connect_parsing_error(|_, section, error| {
        eprintln!("wakeshell: style: {section}: {error}");
    });
    provider.load_from_string(&css);
    // Above ~/.config/gtk-4.0/gtk.css
    gtk::style_context_add_provider_for_display(
        &gdk::Display::default().expect("display"),
        &provider,
        gtk::STYLE_PROVIDER_PRIORITY_USER + 10,
    );
}
