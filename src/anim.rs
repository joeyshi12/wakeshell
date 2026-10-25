// Appear and disappear transitions. Enter timings live in style.css; exit times are also here,
// since the widget has to stay visible until its exit ends.
use std::{cell::Cell, rc::Rc, time::Duration};

use gtk::{glib, prelude::*};

pub struct Fade {
    /// Shown and hidden: usually the layer window
    target: gtk::Widget,
    /// Carries the .entering and .leaving classes
    styled: gtk::Widget,
    enter: bool,
    exit_ms: u64,
    generation: Rc<Cell<u32>>,
}

fn animations() -> bool {
    gtk::Settings::default().is_none_or(|s| s.is_gtk_enable_animations())
}

/// GTK_SLOWDOWN stretches GTK's own transitions, so stretch the exit timer to match
fn slowdown() -> f64 {
    std::env::var("GTK_SLOWDOWN").ok().and_then(|v| v.parse().ok()).filter(|v: &f64| *v > 0.0).unwrap_or(1.0)
}

impl Fade {
    /// `enter` false shows at once (or leaves the entrance to the widget, like the backdrop)
    pub fn new(target: &impl IsA<gtk::Widget>, styled: &impl IsA<gtk::Widget>, enter: bool, exit_ms: u64) -> Fade {
        styled.add_css_class("fade");
        Fade {
            target: target.clone().upcast(),
            styled: styled.clone().upcast(),
            enter,
            exit_ms,
            generation: Rc::new(Cell::new(0)),
        }
    }

    pub fn set(&self, shown: bool) {
        if shown { self.show() } else { self.hide() }
    }

    pub fn show(&self) {
        self.generation.set(self.generation.get() + 1);
        // Coming back while leaving just reverses the transition
        self.styled.remove_css_class("leaving");
        self.styled.set_can_target(true);
        if self.target.is_visible() {
            return;
        }
        if self.enter {
            enter(&self.styled);
        }
        self.target.set_visible(true);
    }

    pub fn hide(&self) {
        self.hide_after(self.exit_ms);
    }

    pub fn hide_now(&self) {
        self.hide_after(0);
    }

    fn hide_after(&self, ms: u64) {
        let leaving = self.styled.has_css_class("leaving");
        if !self.target.is_visible() || (leaving && ms != 0) {
            return;
        }
        let generation = self.generation.get() + 1;
        self.generation.set(generation);
        let ms = if animations() { (ms as f64 * slowdown()) as u64 } else { 0 };
        if ms == 0 {
            self.styled.remove_css_class("entering");
            self.styled.remove_css_class("leaving");
            self.target.set_visible(false);
            self.styled.set_can_target(true);
            return;
        }
        // Nothing on it can be clicked while it fades. Keys still arrive (dwl keeps an exclusive
        // layer focused until it unmaps), so components ignore them once they have closed.
        self.styled.set_can_target(false);
        self.styled.add_css_class("leaving");
        let (target, styled, latest) = (self.target.clone(), self.styled.clone(), self.generation.clone());
        glib::timeout_add_local_once(Duration::from_millis(ms), move || {
            if latest.get() == generation {
                target.set_visible(false);
                styled.remove_css_class("leaving");
                styled.set_can_target(true);
            }
        });
    }
}

/// Plays the widget's .entering transition. The first frame has to draw the .entering style, or
/// there is nothing to transition from.
pub fn enter(widget: &gtk::Widget) {
    if !animations() {
        return;
    }
    widget.add_css_class("entering");
    let frames = Cell::new(0);
    widget.add_tick_callback(move |widget, _| {
        frames.set(frames.get() + 1);
        if frames.get() < 2 {
            return glib::ControlFlow::Continue;
        }
        widget.remove_css_class("entering");
        glib::ControlFlow::Break
    });
}

/// After `ms`, stretched like GTK's own transitions; at once with animations off
pub fn after(ms: u64, f: impl FnOnce() + 'static) {
    if !animations() || ms == 0 {
        return f();
    }
    glib::timeout_add_local_once(Duration::from_millis((ms as f64 * slowdown()) as u64), f);
}
