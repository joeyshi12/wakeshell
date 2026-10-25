// The stylesheet. Colours are the --wakenight-* variables from ~/.config/gtk-4.0/gtk.css, with the
// built-in palette underneath for when that file does not set them.
use gtk::gdk;

const FALLBACK_COLORS: &str = ":root {
    --wakenight-bg: #0f1825;
    --wakenight-bg-alt: #253447;
    --wakenight-accent: #418aa2;
    --wakenight-fg: #e8f4f8;
    --wakenight-fg-dim: #6b8494;
    --wakenight-fg-muted: #96a3ae;
    --wakenight-urgent: #f4d06f;
    --wakenight-shadow: #05090f;
}";

pub fn load() {
    // Automatic font rendering clips the top row of some glyphs with the GPU renderers
    if let Some(settings) = gtk::Settings::default() {
        settings.set_gtk_font_rendering(gtk::FontRendering::Manual);
    }
    let display = gdk::Display::default().expect("display");
    // Below ~/.config/gtk-4.0/gtk.css, so its variables win
    add(&display, FALLBACK_COLORS, gtk::STYLE_PROVIDER_PRIORITY_FALLBACK);
    // Above it, so themes do not restyle the shell
    add(&display, include_str!("style.css"), gtk::STYLE_PROVIDER_PRIORITY_USER + 10);
}

fn add(display: &gdk::Display, css: &str, priority: u32) {
    let provider = gtk::CssProvider::new();
    provider.connect_parsing_error(|_, section, error| {
        eprintln!("wakeshell: style: {section}: {error}");
    });
    provider.load_from_string(css);
    gtk::style_context_add_provider_for_display(display, &provider, priority);
}
