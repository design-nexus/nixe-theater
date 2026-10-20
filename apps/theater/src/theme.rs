//! Theming: the shared Nixe theme engine, given this app's stylesheet.

pub use nixe_theme::*;

use crate::prefs;

/// Install the stylesheet and the current palette, and follow Omarchy live.
pub fn install() {
    nixe_theme::install(Style {
        window_class: "theater-window",
        base_skip: &[".tag"],
        css: nixe_playback::controls::stylesheet(include_str!("style.css")),
        no_glow_css: "\n.osd-controls { box-shadow: 0 4px 16px alpha(@theme_shadow, 0.5); }\n",
        extra_colors: Some(video_colors),
        prefs: || {
            let p = prefs::get();
            ThemePrefs { mode: p.mode, theme: p.theme, glow: p.glow, reduce_motion: p.reduce_motion }
        },
        set_theme: Some(|mode, theme| {
            prefs::update(|p| {
                p.mode = mode;
                p.theme = theme;
            })
        }),
    });
}

/// The colours around and over a playing video.
fn video_colors(p: &Palette) -> String {
    format!(
        "@define-color theme_video {};\n@define-color theme_on_video {};\n",
        // Around the picture: the darkest colour the theme has.
        if p.light { &p.text } else { &p.shadow },
        // Text over the picture's dark surround.
        if p.light { &p.bg } else { &p.text },
    )
}
