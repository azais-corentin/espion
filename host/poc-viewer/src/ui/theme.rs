//! The window's look: colours, type and spacing, and their installation into egui.
//!
//! The picture is greyscale, so the chrome is neutral grey and colour means one thing: a stage
//! of the pipeline ([`hue`]). A stage has the same hue in the timing diagram, on the settings
//! that act on it and on its counters. Red ([`FAULT`]) is for faults and refusals only.
//!
//! Type is IBM Plex (`assets/fonts`, SIL Open Font License 1.1): Plex Sans for words, Plex
//! Mono for every number that changes, so digits keep their place.

use std::sync::Arc;

use eframe::egui::{
    self, Color32, CornerRadius, FontData, FontDefinitions, FontFamily, FontId, Margin, Stroke,
    TextStyle, Vec2,
};
use poc_viewer::timing::Stage;

/// The window behind everything.
pub const BG: Color32 = Color32::from_rgb(0x14, 0x15, 0x17);
/// Bars, the rail, the drawer.
pub const PANEL: Color32 = Color32::from_rgb(0x1b, 0x1d, 0x20);
/// Controls at rest.
pub const RAISED: Color32 = Color32::from_rgb(0x26, 0x29, 0x2e);
/// Controls under the pointer.
pub const HOVER: Color32 = Color32::from_rgb(0x30, 0x34, 0x3a);
/// The chosen part of a control without a stage.
pub const CHOSEN: Color32 = Color32::from_rgb(0x4a, 0x4f, 0x57);
/// Hairlines.
pub const LINE: Color32 = Color32::from_rgb(0x33, 0x36, 0x3b);
/// Text.
pub const TEXT: Color32 = Color32::from_rgb(0xe8, 0xe8, 0xea);
/// Names, units, captions.
pub const MUTED: Color32 = Color32::from_rgb(0x9a, 0x9d, 0xa4);
/// Disabled text, grid lines' labels.
pub const FAINT: Color32 = Color32::from_rgb(0x6c, 0x70, 0x78);
/// Behind the picture.
pub const WELL: Color32 = Color32::from_rgb(0x0a, 0x0a, 0x0b);
/// Faults and refusals.
pub const FAULT: Color32 = Color32::from_rgb(0xff, 0x5a, 0x5f);
/// Behind a fault's text.
pub const FAULT_FILL: Color32 = Color32::from_rgb(0x3a, 0x1c, 0x1f);

/// Text sizes, points.
pub const SMALL: f32 = 11.5;
pub const BODY: f32 = 13.0;
pub const FIGURE: f32 = 15.0;
pub const HERO: f32 = 28.0;

/// Height of a control, points.
pub const CONTROL: f32 = 26.0;
/// Corner radius of controls.
pub const RADIUS: CornerRadius = CornerRadius::same(6);
/// Corner radius of bars and small marks.
pub const RADIUS_SMALL: CornerRadius = CornerRadius::same(3);
/// Padding of the panels.
pub const PAD: f32 = 14.0;
/// Space between rows.
pub const GAP: f32 = 8.0;

/// The colours of one stage: a dark fill, a bright line, and text that reads on the fill.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Hue {
    pub fill: Color32,
    pub line: Color32,
    pub text: Color32,
}

const fn rgb(v: u32) -> Color32 {
    Color32::from_rgb((v >> 16) as u8, (v >> 8) as u8, v as u8)
}

/// For controls and marks that belong to no stage.
pub const NEUTRAL: Hue = Hue {
    fill: CHOSEN,
    line: rgb(0xc9ccd2),
    text: TEXT,
};

/// Waits between stages.
pub const WAIT: Hue = Hue {
    fill: rgb(0x23252a),
    line: rgb(0x5c6068),
    text: MUTED,
};

/// The hue of a stage.
pub const fn hue(stage: Stage) -> Hue {
    let (fill, line, text) = match stage {
        Stage::Exposure => (0x5f4a12, 0xd9a826, 0xf5d98a),
        Stage::Readout => (0x0e5346, 0x35b597, 0x8fe6cf),
        Stage::Encode => (0x3b358f, 0x7f77ea, 0xc4c0ff),
        Stage::Send => (0x73301a, 0xe0764a, 0xffb695),
        Stage::Decode => (0x1c4a73, 0x4a9fe0, 0xaed6fb),
        Stage::Draw => (0x6a274d, 0xd0609c, 0xf4aed3),
    };
    Hue {
        fill: rgb(fill),
        line: rgb(line),
        text: rgb(text),
    }
}

const SEMIBOLD: &str = "plex-sans-semibold";
const MONO_MEDIUM: &str = "plex-mono-medium";

/// Plex Sans.
pub fn sans(size: f32) -> FontId {
    FontId::new(size, FontFamily::Proportional)
}

/// Plex Sans SemiBold: headings and the chosen option.
pub fn strong(size: f32) -> FontId {
    FontId::new(size, FontFamily::Name(SEMIBOLD.into()))
}

/// Plex Mono: numbers.
pub fn mono(size: f32) -> FontId {
    FontId::new(size, FontFamily::Monospace)
}

/// Plex Mono Medium: the figures that lead.
pub fn mono_strong(size: f32) -> FontId {
    FontId::new(size, FontFamily::Name(MONO_MEDIUM.into()))
}

/// `c` at `alpha` (0..=1) of its opacity.
pub fn fade(c: Color32, alpha: f32) -> Color32 {
    c.gamma_multiply(alpha)
}

/// Installs the fonts and the style. egui's own fonts stay behind Plex for the glyphs it
/// lacks.
pub fn install(ctx: &egui::Context) {
    let mut fonts = FontDefinitions::default();
    let files: [(&str, &'static [u8]); 4] = [
        (
            "plex-sans",
            include_bytes!("../../assets/fonts/IBMPlexSans-Regular.ttf"),
        ),
        (
            SEMIBOLD,
            include_bytes!("../../assets/fonts/IBMPlexSans-SemiBold.ttf"),
        ),
        (
            "plex-mono",
            include_bytes!("../../assets/fonts/IBMPlexMono-Regular.ttf"),
        ),
        (
            MONO_MEDIUM,
            include_bytes!("../../assets/fonts/IBMPlexMono-Medium.ttf"),
        ),
    ];
    for (name, bytes) in files {
        fonts
            .font_data
            .insert(name.to_owned(), Arc::new(FontData::from_static(bytes)));
    }
    let fallback = |fonts: &FontDefinitions, family: &FontFamily| {
        fonts.families.get(family).cloned().unwrap_or_default()
    };
    let sans_fallback = fallback(&fonts, &FontFamily::Proportional);
    let mono_fallback = fallback(&fonts, &FontFamily::Monospace);
    for (family, first, rest) in [
        (FontFamily::Proportional, "plex-sans", &sans_fallback),
        (FontFamily::Name(SEMIBOLD.into()), SEMIBOLD, &sans_fallback),
        (FontFamily::Monospace, "plex-mono", &mono_fallback),
        (
            FontFamily::Name(MONO_MEDIUM.into()),
            MONO_MEDIUM,
            &mono_fallback,
        ),
    ] {
        let mut list = vec![first.to_owned()];
        list.extend(rest.iter().cloned());
        fonts.families.insert(family, list);
    }
    ctx.set_fonts(fonts);

    ctx.all_styles_mut(|style| {
        style.text_styles = [
            (TextStyle::Small, sans(SMALL)),
            (TextStyle::Body, sans(BODY)),
            (TextStyle::Button, sans(BODY)),
            (TextStyle::Monospace, mono(BODY)),
            (TextStyle::Heading, strong(FIGURE)),
        ]
        .into();
        style.drag_value_text_style = TextStyle::Monospace;
        let s = &mut style.spacing;
        s.item_spacing = Vec2::new(GAP, GAP);
        s.button_padding = Vec2::new(8.0, 4.0);
        s.interact_size = Vec2::new(40.0, CONTROL);
        s.window_margin = Margin::same(10);
        s.menu_margin = Margin::same(8);
        s.scroll.bar_width = 6.0;
        s.scroll.floating = true;

        let v = &mut style.visuals;
        v.dark_mode = true;
        v.override_text_color = Some(TEXT);
        v.panel_fill = PANEL;
        v.window_fill = RAISED;
        v.window_stroke = Stroke::new(1.0, LINE);
        v.window_corner_radius = RADIUS;
        v.menu_corner_radius = RADIUS;
        v.extreme_bg_color = BG;
        v.faint_bg_color = RAISED;
        v.error_fg_color = FAULT;
        v.warn_fg_color = TEXT;
        v.hyperlink_color = TEXT;
        v.selection.bg_fill = CHOSEN;
        v.selection.stroke = Stroke::new(1.0, TEXT);
        let w = &mut v.widgets;
        for (state, fill, line, text) in [
            (&mut w.noninteractive, PANEL, LINE, TEXT),
            (&mut w.inactive, RAISED, LINE, TEXT),
            (&mut w.hovered, HOVER, MUTED, TEXT),
            (&mut w.active, CHOSEN, TEXT, TEXT),
            (&mut w.open, HOVER, MUTED, TEXT),
        ] {
            state.bg_fill = fill;
            state.weak_bg_fill = fill;
            state.bg_stroke = Stroke::new(1.0, line);
            state.fg_stroke = Stroke::new(1.0, text);
            state.corner_radius = RADIUS;
            state.expansion = 0.0;
        }
    });
}
