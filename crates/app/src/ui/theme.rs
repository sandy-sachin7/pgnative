//! Design tokens + dark/light themes — per AGENTS.md §31.
//!
//! The dated stock look came from returning `Visuals::dark()` verbatim.
//! This module builds custom [`Visuals`] from an explicit token set:
//! layered fills (app → panel → elevated → input), 1px borders, flat
//! widget fills, accent selection, and a rounded window shadow.

use egui::{Color32, CornerRadius, FontFamily, FontId, Margin, Shadow, Stroke, TextStyle, Visuals};

/// Full design token set. `bg`/`fg`/`accent` are kept as aliases of the
/// primary tokens for compatibility; new code should use the layered names.
#[derive(Debug, Clone)]
pub struct Theme {
    /// Farthest background (behind panels, window gutters).
    pub app_bg: Color32,
    /// Panel surfaces (side bars, top bar, cards).
    pub panel: Color32,
    /// Popups, menus, modal windows, completion list.
    pub elevated: Color32,
    /// Text inputs, editor surface.
    pub input: Color32,
    /// 1px borders, separators, inactive widget outlines.
    pub border: Color32,
    /// Primary text.
    pub text_primary: Color32,
    /// Secondary text (labels, footers, placeholders).
    pub text_secondary: Color32,
    /// Faint text (counts, hints, disabled).
    pub text_faint: Color32,
    /// Primary accent (links, active states, primary buttons).
    pub accent: Color32,
    /// Accent hover/pressed.
    pub accent_hover: Color32,
    /// Translucent accent for text selection.
    pub accent_dim: Color32,
    /// Success / in-transaction.
    pub success: Color32,
    /// Warning.
    pub warn: Color32,
    /// Errors / failed transaction.
    pub danger: Color32,
    /// Legacy alias of [`Theme::app_bg`].
    pub bg: Color32,
    /// Legacy alias of [`Theme::text_primary`].
    pub fg: Color32,
    /// Corner radius for small widgets (buttons, inputs).
    pub radius_sm: u8,
    /// Corner radius for cards and popups.
    pub radius_md: u8,
    /// Corner radius for windows and modals.
    pub radius_lg: u8,
    /// Which side of the toggle we are on.
    pub is_dark: bool,
}

impl Theme {
    /// Dark-first default theme: layered slate fills, blue accent.
    #[must_use]
    pub fn dark() -> Self {
        let text_primary = Color32::from_rgb(226, 232, 240);
        let accent = Color32::from_rgb(96, 165, 250);
        let app_bg = Color32::from_rgb(13, 16, 22);
        Self {
            app_bg,
            panel: Color32::from_rgb(19, 24, 33),
            elevated: Color32::from_rgb(27, 34, 46),
            input: Color32::from_rgb(15, 19, 27),
            border: Color32::from_rgb(48, 58, 76),
            text_primary,
            text_secondary: Color32::from_rgb(148, 163, 184),
            text_faint: Color32::from_rgb(100, 116, 139),
            accent,
            accent_hover: Color32::from_rgb(125, 184, 252),
            accent_dim: Color32::from_rgba_unmultiplied(96, 165, 250, 48),
            success: Color32::from_rgb(52, 211, 153),
            warn: Color32::from_rgb(251, 191, 36),
            danger: Color32::from_rgb(248, 113, 113),
            bg: app_bg,
            fg: text_primary,
            radius_sm: 4,
            radius_md: 6,
            radius_lg: 10,
            is_dark: true,
        }
    }

    /// Light theme mirroring the same token structure.
    #[must_use]
    pub fn light() -> Self {
        let text_primary = Color32::from_rgb(23, 32, 44);
        let accent = Color32::from_rgb(37, 99, 235);
        let app_bg = Color32::from_rgb(241, 245, 249);
        Self {
            app_bg,
            panel: Color32::WHITE,
            elevated: Color32::WHITE,
            input: Color32::from_rgb(248, 250, 252),
            border: Color32::from_rgb(203, 213, 225),
            text_primary,
            text_secondary: Color32::from_rgb(71, 85, 105),
            text_faint: Color32::from_rgb(148, 163, 184),
            accent,
            accent_hover: Color32::from_rgb(29, 78, 216),
            accent_dim: Color32::from_rgba_unmultiplied(37, 99, 235, 36),
            success: Color32::from_rgb(5, 150, 105),
            warn: Color32::from_rgb(217, 119, 6),
            danger: Color32::from_rgb(220, 38, 38),
            bg: app_bg,
            fg: text_primary,
            radius_sm: 4,
            radius_md: 6,
            radius_lg: 10,
            is_dark: false,
        }
    }

    /// Build custom visuals from the token set (never stock).
    #[must_use]
    pub fn visuals(&self) -> Visuals {
        let mut v = if self.is_dark {
            Visuals::dark()
        } else {
            Visuals::light()
        };
        v.dark_mode = self.is_dark;
        v.override_text_color = Some(self.text_primary);
        v.window_fill = self.elevated;
        v.panel_fill = self.panel;
        v.extreme_bg_color = self.app_bg;
        v.code_bg_color = self.input;
        v.faint_bg_color = self.app_bg;
        v.hyperlink_color = self.accent;
        v.error_fg_color = self.danger;
        v.warn_fg_color = self.warn;
        v.window_stroke = Stroke::new(1.0, self.border);
        v.window_corner_radius = CornerRadius::same(self.radius_lg);
        v.window_shadow = Shadow {
            offset: [0, 10],
            blur: 28,
            spread: 0,
            color: Color32::from_black_alpha(110),
        };
        v.popup_shadow = Shadow {
            offset: [0, 6],
            blur: 20,
            spread: 0,
            color: Color32::from_black_alpha(90),
        };
        v.menu_corner_radius = CornerRadius::same(self.radius_md);
        v.selection = egui::style::Selection {
            bg_fill: self.accent_dim,
            stroke: Stroke::new(1.0, self.accent),
        };
        let widget_fill = self.elevated;
        let widget_hover = self.input;
        let sm = CornerRadius::same(self.radius_sm);
        let border = Stroke::new(1.0, self.border);
        let text = |c: Color32| Stroke::new(1.0, c);
        v.widgets.noninteractive = egui::style::WidgetVisuals {
            bg_fill: self.panel,
            weak_bg_fill: self.panel,
            bg_stroke: Stroke::NONE,
            corner_radius: sm,
            fg_stroke: text(self.text_secondary),
            expansion: 0.0,
        };
        v.widgets.inactive = egui::style::WidgetVisuals {
            bg_fill: widget_fill,
            weak_bg_fill: widget_fill,
            bg_stroke: border,
            corner_radius: sm,
            fg_stroke: text(self.text_primary),
            expansion: 0.0,
        };
        v.widgets.hovered = egui::style::WidgetVisuals {
            bg_fill: widget_hover,
            weak_bg_fill: widget_hover,
            bg_stroke: Stroke::new(1.0, self.accent),
            corner_radius: sm,
            fg_stroke: text(self.text_primary),
            expansion: 1.0,
        };
        v.widgets.active = egui::style::WidgetVisuals {
            bg_fill: self.accent_dim,
            weak_bg_fill: self.accent_dim,
            bg_stroke: Stroke::new(1.0, self.accent),
            corner_radius: sm,
            fg_stroke: text(self.text_primary),
            expansion: 1.0,
        };
        v.widgets.open = egui::style::WidgetVisuals {
            bg_fill: widget_hover,
            weak_bg_fill: widget_hover,
            bg_stroke: Stroke::new(1.0, self.accent),
            corner_radius: sm,
            fg_stroke: text(self.text_primary),
            expansion: 0.0,
        };
        v
    }

    /// Install visuals plus spacing and type scale on `ctx`.
    pub fn apply(&self, ctx: &egui::Context) {
        ctx.set_visuals(self.visuals());
        ctx.all_styles_mut(|s| {
            s.spacing.item_spacing = egui::vec2(6.0, 5.0);
            s.spacing.button_padding = egui::vec2(9.0, 5.0);
            s.spacing.indent = 18.0;
            s.spacing.scroll = egui::style::ScrollStyle {
                floating: true,
                floating_width: 8.0,
                ..Default::default()
            };
            s.text_styles.insert(
                TextStyle::Heading,
                FontId::new(17.0, FontFamily::Proportional),
            );
            s.text_styles
                .insert(TextStyle::Body, FontId::new(13.5, FontFamily::Proportional));
            s.text_styles.insert(
                TextStyle::Button,
                FontId::new(13.0, FontFamily::Proportional),
            );
            s.text_styles.insert(
                TextStyle::Small,
                FontId::new(11.5, FontFamily::Proportional),
            );
            s.text_styles.insert(
                TextStyle::Monospace,
                FontId::new(13.0, FontFamily::Monospace),
            );
        });
    }

    /// Card container: elevated fill, 1px border, medium radius.
    pub fn card(&self) -> egui::Frame {
        egui::Frame::new()
            .fill(self.elevated)
            .stroke(Stroke::new(1.0, self.border))
            .corner_radius(CornerRadius::same(self.radius_md))
            .inner_margin(Margin::symmetric(10, 8))
    }

    /// Hairline frame for grouping rows (results header band, status strip).
    pub fn band(&self) -> egui::Frame {
        egui::Frame::new()
            .fill(self.panel)
            .stroke(Stroke::new(1.0, self.border))
            .corner_radius(CornerRadius::same(self.radius_sm))
            .inner_margin(Margin::symmetric(8, 4))
    }

    /// Small caps-ish section label.
    #[must_use]
    pub fn section_label(&self, text: &str) -> egui::RichText {
        egui::RichText::new(text)
            .small()
            .strong()
            .color(self.text_secondary)
    }

    /// Primary action button (Run, Connect, Commit): accent fill, white text.
    pub fn primary_button(&self, text: &str) -> egui::Button<'static> {
        egui::Button::new(
            egui::RichText::new(text.to_owned())
                .strong()
                .color(Color32::WHITE),
        )
        .fill(self.accent)
        .corner_radius(CornerRadius::same(self.radius_sm))
    }

    /// Danger action button (Rollback): danger fill, white text.
    pub fn danger_button(&self, text: &str) -> egui::Button<'static> {
        egui::Button::new(
            egui::RichText::new(text.to_owned())
                .strong()
                .color(Color32::WHITE),
        )
        .fill(self.danger)
        .corner_radius(CornerRadius::same(self.radius_sm))
    }
}
