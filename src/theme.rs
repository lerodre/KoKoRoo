use eframe::egui::Color32;
use serde::{Deserialize, Serialize};

#[derive(Serialize, Deserialize, Clone)]
pub struct Theme {
    #[serde(default = "default_sidebar_bg")]
    pub sidebar_bg: [u8; 3],
    #[serde(default = "default_btn_positive")]
    pub btn_positive: [u8; 3],
    #[serde(default = "default_btn_negative")]
    pub btn_negative: [u8; 3],
    #[serde(default = "default_btn_primary")]
    pub btn_primary: [u8; 3],
    #[serde(default = "default_btn_neutral")]
    pub btn_neutral: [u8; 3],
    #[serde(default = "default_accent")]
    pub accent: [u8; 3],
    #[serde(default = "default_warning")]
    pub warning: [u8; 3],
    #[serde(default = "default_error")]
    pub error: [u8; 3],
    #[serde(default = "default_chat_self")]
    pub chat_self: [u8; 3],
    #[serde(default = "default_chat_peer")]
    pub chat_peer: [u8; 3],
    #[serde(default = "default_text_muted")]
    pub text_muted: [u8; 3],
    #[serde(default = "default_text_dim")]
    pub text_dim: [u8; 3],
    #[serde(default = "default_panel_bg")]
    pub panel_bg: [u8; 3],
    #[serde(default = "default_text_primary")]
    pub text_primary: [u8; 3],
    #[serde(default = "default_widget_bg")]
    pub widget_bg: [u8; 3],
    #[serde(default = "default_widget_hovered")]
    pub widget_hovered: [u8; 3],
    #[serde(default = "default_widget_active")]
    pub widget_active: [u8; 3],
    #[serde(default = "default_separator")]
    pub separator: [u8; 3],
}

fn default_sidebar_bg() -> [u8; 3] { [25, 25, 30] }
fn default_btn_positive() -> [u8; 3] { [40, 140, 60] }
fn default_btn_negative() -> [u8; 3] { [180, 40, 40] }
fn default_btn_primary() -> [u8; 3] { [40, 100, 180] }
fn default_btn_neutral() -> [u8; 3] { [100, 100, 100] }
fn default_accent() -> [u8; 3] { [80, 200, 80] }
fn default_warning() -> [u8; 3] { [255, 200, 50] }
fn default_error() -> [u8; 3] { [255, 60, 60] }
fn default_chat_self() -> [u8; 3] { [100, 180, 255] }
fn default_chat_peer() -> [u8; 3] { [180, 255, 100] }
fn default_text_muted() -> [u8; 3] { [128, 128, 128] }
fn default_text_dim() -> [u8; 3] { [140, 140, 140] }
fn default_panel_bg() -> [u8; 3] { [27, 27, 27] }
fn default_text_primary() -> [u8; 3] { [210, 210, 210] }
fn default_widget_bg() -> [u8; 3] { [60, 60, 60] }
fn default_widget_hovered() -> [u8; 3] { [70, 70, 70] }
fn default_widget_active() -> [u8; 3] { [0, 92, 128] }
fn default_separator() -> [u8; 3] { [60, 60, 60] }

fn hsl_to_rgb(h: f32, s: f32, l: f32) -> [u8; 3] {
    let h = ((h % 360.0) + 360.0) % 360.0;
    let c = (1.0 - (2.0 * l - 1.0).abs()) * s;
    let x = c * (1.0 - ((h / 60.0) % 2.0 - 1.0).abs());
    let m = l - c / 2.0;
    let (r1, g1, b1) = if h < 60.0 {
        (c, x, 0.0)
    } else if h < 120.0 {
        (x, c, 0.0)
    } else if h < 180.0 {
        (0.0, c, x)
    } else if h < 240.0 {
        (0.0, x, c)
    } else if h < 300.0 {
        (x, 0.0, c)
    } else {
        (c, 0.0, x)
    };
    [
        ((r1 + m) * 255.0).round().clamp(0.0, 255.0) as u8,
        ((g1 + m) * 255.0).round().clamp(0.0, 255.0) as u8,
        ((b1 + m) * 255.0).round().clamp(0.0, 255.0) as u8,
    ]
}

impl Default for Theme {
    fn default() -> Self {
        Self {
            sidebar_bg: default_sidebar_bg(),
            btn_positive: default_btn_positive(),
            btn_negative: default_btn_negative(),
            btn_primary: default_btn_primary(),
            btn_neutral: default_btn_neutral(),
            accent: default_accent(),
            warning: default_warning(),
            error: default_error(),
            chat_self: default_chat_self(),
            chat_peer: default_chat_peer(),
            text_muted: default_text_muted(),
            text_dim: default_text_dim(),
            panel_bg: default_panel_bg(),
            text_primary: default_text_primary(),
            widget_bg: default_widget_bg(),
            widget_hovered: default_widget_hovered(),
            widget_active: default_widget_active(),
            separator: default_separator(),
        }
    }
}

impl Theme {
    pub fn sidebar_bg(&self) -> Color32 { Color32::from_rgb(self.sidebar_bg[0], self.sidebar_bg[1], self.sidebar_bg[2]) }
    pub fn btn_positive(&self) -> Color32 { Color32::from_rgb(self.btn_positive[0], self.btn_positive[1], self.btn_positive[2]) }
    pub fn btn_negative(&self) -> Color32 { Color32::from_rgb(self.btn_negative[0], self.btn_negative[1], self.btn_negative[2]) }
    pub fn btn_primary(&self) -> Color32 { Color32::from_rgb(self.btn_primary[0], self.btn_primary[1], self.btn_primary[2]) }
    pub fn btn_neutral(&self) -> Color32 { Color32::from_rgb(self.btn_neutral[0], self.btn_neutral[1], self.btn_neutral[2]) }
    pub fn accent(&self) -> Color32 { Color32::from_rgb(self.accent[0], self.accent[1], self.accent[2]) }
    pub fn warning(&self) -> Color32 { Color32::from_rgb(self.warning[0], self.warning[1], self.warning[2]) }
    pub fn error(&self) -> Color32 { Color32::from_rgb(self.error[0], self.error[1], self.error[2]) }
    pub fn chat_self(&self) -> Color32 { Color32::from_rgb(self.chat_self[0], self.chat_self[1], self.chat_self[2]) }
    pub fn chat_peer(&self) -> Color32 { Color32::from_rgb(self.chat_peer[0], self.chat_peer[1], self.chat_peer[2]) }
    pub fn text_muted(&self) -> Color32 { Color32::from_rgb(self.text_muted[0], self.text_muted[1], self.text_muted[2]) }
    pub fn text_dim(&self) -> Color32 { Color32::from_rgb(self.text_dim[0], self.text_dim[1], self.text_dim[2]) }
    pub fn panel_bg(&self) -> Color32 { Color32::from_rgb(self.panel_bg[0], self.panel_bg[1], self.panel_bg[2]) }
    pub fn text_primary(&self) -> Color32 { Color32::from_rgb(self.text_primary[0], self.text_primary[1], self.text_primary[2]) }
    pub fn widget_bg(&self) -> Color32 { Color32::from_rgb(self.widget_bg[0], self.widget_bg[1], self.widget_bg[2]) }
    pub fn widget_hovered(&self) -> Color32 { Color32::from_rgb(self.widget_hovered[0], self.widget_hovered[1], self.widget_hovered[2]) }
    pub fn widget_active(&self) -> Color32 { Color32::from_rgb(self.widget_active[0], self.widget_active[1], self.widget_active[2]) }
    pub fn separator(&self) -> Color32 { Color32::from_rgb(self.separator[0], self.separator[1], self.separator[2]) }

    pub fn to_hex(rgb: [u8; 3]) -> String {
        format!("#{:02X}{:02X}{:02X}", rgb[0], rgb[1], rgb[2])
    }

    pub fn from_hex(hex: &str) -> Option<[u8; 3]> {
        let hex = hex.trim_start_matches('#');
        if hex.len() != 6 { return None; }
        let r = u8::from_str_radix(&hex[0..2], 16).ok()?;
        let g = u8::from_str_radix(&hex[2..4], 16).ok()?;
        let b = u8::from_str_radix(&hex[4..6], 16).ok()?;
        Some([r, g, b])
    }

    pub fn all_entries(&self) -> Vec<(&'static str, &'static str, [u8; 3])> {
        vec![
            ("sidebar_bg", "Sidebar Background", self.sidebar_bg),
            ("btn_positive", "Positive Buttons", self.btn_positive),
            ("btn_negative", "Negative Buttons", self.btn_negative),
            ("btn_primary", "Primary Buttons", self.btn_primary),
            ("btn_neutral", "Neutral Buttons", self.btn_neutral),
            ("accent", "Accent / Success", self.accent),
            ("warning", "Warning", self.warning),
            ("error", "Error", self.error),
            ("chat_self", "Chat (You)", self.chat_self),
            ("chat_peer", "Chat (Peer)", self.chat_peer),
            ("text_muted", "Muted Text", self.text_muted),
            ("text_dim", "Dim Text", self.text_dim),
            ("panel_bg", "Panel Background", self.panel_bg),
            ("text_primary", "Primary Text", self.text_primary),
            ("widget_bg", "Widget Background", self.widget_bg),
            ("widget_hovered", "Widget Hovered", self.widget_hovered),
            ("widget_active", "Widget Active", self.widget_active),
            ("separator", "Separators", self.separator),
        ]
    }

    pub fn smart_randomize(&mut self, locks: &std::collections::HashSet<String>) {
        use rand::Rng;
        let mut rng = rand::thread_rng();

        let base_hue: f32 = rng.gen_range(0.0..360.0);

        // ── Pick a color harmony (5 types for variety) ──
        let offsets: [f32; 4] = match rng.gen_range(0u8..5) {
            0 => [137.508, 275.016, 412.524, 550.032], // Golden ratio
            1 => [180.0, 210.0, 30.0, 150.0],          // Complementary
            2 => [120.0, 240.0, 60.0, 300.0],          // Triadic
            3 => [30.0, 60.0, -30.0, -60.0],           // Analogous
            _ => [150.0, 210.0, 330.0, 60.0],          // Split-complementary
        };

        // ── Dark vs light theme ──
        let is_light = rng.gen_bool(0.4);

        macro_rules! rf {
            ($lo:expr, $hi:expr) => { rng.gen_range($lo..=$hi) }
        }
        macro_rules! set {
            ($name:expr, $rgb:expr) => {
                if !locks.contains($name) { self.set_by_name($name, $rgb); }
            }
        }

        let bg_hue = base_hue + rf!(-25.0f32, 25.0);

        if is_light {
            // ── Light theme: pasteles saturados (ej: rosa pastel, lila, mint) ──
            set!("panel_bg",       hsl_to_rgb(bg_hue, rf!(0.10f32, 0.55), rf!(0.88f32, 0.96)));
            set!("sidebar_bg",     hsl_to_rgb(bg_hue, rf!(0.12f32, 0.60), rf!(0.82f32, 0.91)));
            set!("widget_bg",      hsl_to_rgb(bg_hue, rf!(0.08f32, 0.45), rf!(0.80f32, 0.89)));
            set!("widget_hovered", hsl_to_rgb(bg_hue, rf!(0.10f32, 0.50), rf!(0.73f32, 0.83)));
            set!("separator",      hsl_to_rgb(bg_hue, rf!(0.06f32, 0.35), rf!(0.75f32, 0.85)));
            set!("text_primary",   hsl_to_rgb(bg_hue, rf!(0.0f32, 0.20), rf!(0.08f32, 0.22)));
            set!("text_muted",     hsl_to_rgb(bg_hue, rf!(0.0f32, 0.15), rf!(0.35f32, 0.50)));
            set!("text_dim",       hsl_to_rgb(bg_hue, rf!(0.0f32, 0.15), rf!(0.30f32, 0.45)));
        } else {
            // ── Dark theme: fondos ricos y profundos (ej: navy, burgundy, teal, púrpura) ──
            set!("panel_bg",       hsl_to_rgb(bg_hue, rf!(0.08f32, 0.65), rf!(0.04f32, 0.18)));
            set!("sidebar_bg",     hsl_to_rgb(bg_hue, rf!(0.10f32, 0.70), rf!(0.03f32, 0.15)));
            set!("widget_bg",      hsl_to_rgb(bg_hue, rf!(0.08f32, 0.50), rf!(0.14f32, 0.30)));
            set!("widget_hovered", hsl_to_rgb(bg_hue, rf!(0.10f32, 0.55), rf!(0.20f32, 0.38)));
            set!("separator",      hsl_to_rgb(bg_hue, rf!(0.06f32, 0.40), rf!(0.14f32, 0.28)));
            set!("text_primary",   hsl_to_rgb(bg_hue, rf!(0.0f32, 0.15), rf!(0.78f32, 0.93)));
            set!("text_muted",     hsl_to_rgb(bg_hue, rf!(0.0f32, 0.12), rf!(0.40f32, 0.55)));
            set!("text_dim",       hsl_to_rgb(bg_hue, rf!(0.0f32, 0.12), rf!(0.46f32, 0.60)));
        }

        // ── Widget active: first creative hue, saturated ──
        set!("widget_active", hsl_to_rgb(base_hue + offsets[0], rf!(0.45f32, 0.82), rf!(0.26f32, 0.48)));

        // ── UX-critical: fixed hue ranges (work in both light & dark) ──
        let btn_l = if is_light { (0.35f32, 0.52f32) } else { (0.30f32, 0.48f32) };

        // Green = positive/accept
        set!("btn_positive", hsl_to_rgb(rf!(100.0f32, 150.0), rf!(0.48f32, 0.85), rf!(btn_l.0, btn_l.1)));
        set!("accent",       hsl_to_rgb(rf!(95.0f32, 155.0),  rf!(0.48f32, 0.85), rf!(0.40f32, 0.65)));

        // Red = negative/error
        let red_hue: f32 = if rng.gen_bool(0.5) { rf!(345.0f32, 360.0) } else { rf!(0.0f32, 15.0) };
        set!("btn_negative", hsl_to_rgb(red_hue, rf!(0.55f32, 0.85), rf!(btn_l.0, btn_l.1)));
        let err_hue: f32 = if rng.gen_bool(0.5) { rf!(348.0f32, 360.0) } else { rf!(0.0f32, 12.0) };
        set!("error", hsl_to_rgb(err_hue, rf!(0.65f32, 0.92), rf!(0.45f32, 0.65)));

        // Yellow = warning
        set!("warning", hsl_to_rgb(rf!(35.0f32, 55.0), rf!(0.72f32, 1.0), rf!(0.50f32, 0.68)));

        // ── Creative: distributed by chosen harmony ──
        set!("btn_primary", hsl_to_rgb(base_hue + offsets[1], rf!(0.45f32, 0.82), rf!(0.32f32, 0.52)));
        set!("btn_neutral", hsl_to_rgb(bg_hue, rf!(0.0f32, 0.14), rf!(0.28f32, 0.48)));
        set!("chat_self",   hsl_to_rgb(base_hue + offsets[2] + rf!(-18.0f32, 18.0), rf!(0.48f32, 0.85), rf!(0.45f32, 0.72)));
        set!("chat_peer",   hsl_to_rgb(base_hue + offsets[3] + rf!(-18.0f32, 18.0), rf!(0.48f32, 0.85), rf!(0.45f32, 0.72)));
    }

    pub fn set_by_name(&mut self, name: &str, value: [u8; 3]) {
        match name {
            "sidebar_bg" => self.sidebar_bg = value,
            "btn_positive" => self.btn_positive = value,
            "btn_negative" => self.btn_negative = value,
            "btn_primary" => self.btn_primary = value,
            "btn_neutral" => self.btn_neutral = value,
            "accent" => self.accent = value,
            "warning" => self.warning = value,
            "error" => self.error = value,
            "chat_self" => self.chat_self = value,
            "chat_peer" => self.chat_peer = value,
            "text_muted" => self.text_muted = value,
            "text_dim" => self.text_dim = value,
            "panel_bg" => self.panel_bg = value,
            "text_primary" => self.text_primary = value,
            "widget_bg" => self.widget_bg = value,
            "widget_hovered" => self.widget_hovered = value,
            "widget_active" => self.widget_active = value,
            "separator" => self.separator = value,
            _ => {}
        }
    }
}
