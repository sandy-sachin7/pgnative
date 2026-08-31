//! Design tokens + light/dark themes — per AGENTS §31.
use egui::{Color32, Visuals};

#[derive(Debug, Clone)]
pub struct Theme {
    pub bg: Color32,
    pub fg: Color32,
    pub accent: Color32,
    pub is_dark: bool,
}

impl Theme {
    #[must_use]
    pub fn dark() -> Self {
        Self {
            bg: Color32::from_rgb(18, 18, 22),
            fg: Color32::from_rgb(220, 220, 225),
            accent: Color32::from_rgb(90, 140, 255),
            is_dark: true,
        }
    }
    #[must_use]
    pub fn light() -> Self {
        Self {
            bg: Color32::from_rgb(248, 248, 250),
            fg: Color32::from_rgb(30, 30, 35),
            accent: Color32::from_rgb(40, 100, 220),
            is_dark: false,
        }
    }
    #[must_use]
    pub fn visuals(&self) -> Visuals {
        if self.is_dark {
            Visuals::dark()
        } else {
            Visuals::light()
        }
    }
}
