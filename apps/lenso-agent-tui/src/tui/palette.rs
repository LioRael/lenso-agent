use ratatui::style::Color;

pub(super) struct Palette;

impl Palette {
    // GrokNight's neutral canvas and semantic accents, adapted to Lenso's
    // terminal surface. RGB-capable terminals get the reference hierarchy;
    // Ratatui/crossterm handles lower-color terminal fallback.
    pub(super) const BG_BASE: Color = Color::Rgb(20, 20, 20);
    pub(super) const ACCENT: Color = Color::Rgb(187, 154, 247);
    pub(super) const BORDER: Color = Color::Rgb(50, 50, 55);
    pub(super) const BORDER_ACTIVE: Color = Color::Rgb(80, 80, 88);
    pub(super) const SELECTION_BORDER: Color = Color::Rgb(60, 60, 65);
    pub(super) const HOVER_BORDER: Color = Color::Rgb(30, 30, 34);
    pub(super) const HOVER_SURFACE: Color = Color::Rgb(24, 24, 24);
    pub(super) const ERROR: Color = Color::Rgb(247, 118, 142);
    pub(super) const SUCCESS: Color = Color::Rgb(158, 206, 106);
    pub(super) const MUTED: Color = Color::Rgb(108, 108, 108);
    pub(super) const QUIET: Color = Color::Rgb(88, 88, 88);
    pub(super) const CODE: Color = Color::Rgb(58, 149, 171);
    pub(super) const COMMAND: Color = Color::Rgb(224, 175, 104);
    pub(super) const PATH: Color = Color::Rgb(255, 158, 100);
    pub(super) const HEADING_H1: Color = Color::Rgb(26, 188, 156);
    pub(super) const HEADING_H2: Color = Color::Rgb(122, 162, 247);
    pub(super) const HEADING_H3: Color = Color::Rgb(157, 124, 216);
    pub(super) const HEADING_H4: Color = Color::Rgb(120, 120, 120);
    pub(super) const HEADING_H5: Color = Color::Rgb(108, 108, 108);
    pub(super) const HEADING_H6: Color = Color::Rgb(90, 90, 90);
    pub(super) const LINK: Color = Color::Rgb(122, 166, 218);
    pub(super) const SURFACE: Color = Color::Rgb(28, 28, 28);
    pub(super) const VISUAL_SURFACE: Color = Color::Rgb(54, 54, 54);
    pub(super) const USER_SURFACE: Color = Color::Rgb(36, 36, 36);
    pub(super) const SURFACE_TEXT: Color = Color::Rgb(225, 225, 225);
    pub(super) const SECONDARY_TEXT: Color = Color::Rgb(200, 200, 200);
    pub(super) const USER_ACCENT: Color = Color::Rgb(200, 200, 200);
}
