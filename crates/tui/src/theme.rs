//! Built-in and user-defined color themes for the TUI.

use std::collections::BTreeMap;
use std::path::PathBuf;

use ratatui::style::{Color, Modifier, Style};
use serde::{Deserialize, Serialize};

/// A named theme, including the original dark/light presets and additional palettes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Theme {
    Dark,
    Light,
    FrostDark,
    NordicDeep,
    CyberAutumn,
    GruvboxSepia,
    Synthwave84,
    DraculaVivid,
    GhostInTheShell,
    VaporwaveSunset,
    TokyoNight,
    SolarizedDesert,
    DeepSpaceVoyager,
    BioLuminescentDeep,
    CrimsonAlchemist,
    RadioactiveFallout,
    FrozenTundra,
    MonochromaticNoir,
    CoffeeShopCoder,
    DigitalForest,
    ElectricCandy,
    MidnightHacker,
    AncientPapyrus,
    VolcanoMagma,
    AuroraBorealis,
    OceanTrench,
    Cyberpunk2077,
    RetroArcade,
    Custom { name: String, styles: ThemeStyles },
}

impl Default for Theme {
    fn default() -> Self {
        Self::Dark
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct ThemeFile {
    #[serde(default = "default_theme_name")]
    active: String,
    #[serde(default)]
    custom: BTreeMap<String, ThemePalette>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct ThemePalette {
    background: String,
    primary: String,
    accent: String,
    secondary: String,
    success: String,
    warning: String,
    error: String,
    metadata: String,
}

fn default_theme_name() -> String {
    "dark".to_string()
}

impl Theme {
    pub fn dark() -> Self {
        Self::Dark
    }

    pub fn light() -> Self {
        Self::Light
    }

    /// All available built-in themes.
    pub fn builtins() -> Vec<Self> {
        vec![
            Self::Dark,
            Self::Light,
            Self::FrostDark,
            Self::NordicDeep,
            Self::CyberAutumn,
            Self::GruvboxSepia,
            Self::Synthwave84,
            Self::DraculaVivid,
            Self::GhostInTheShell,
            Self::VaporwaveSunset,
            Self::TokyoNight,
            Self::SolarizedDesert,
            Self::DeepSpaceVoyager,
            Self::BioLuminescentDeep,
            Self::CrimsonAlchemist,
            Self::RadioactiveFallout,
            Self::FrozenTundra,
            Self::MonochromaticNoir,
            Self::CoffeeShopCoder,
            Self::DigitalForest,
            Self::ElectricCandy,
            Self::MidnightHacker,
            Self::AncientPapyrus,
            Self::VolcanoMagma,
            Self::AuroraBorealis,
            Self::OceanTrench,
            Self::Cyberpunk2077,
            Self::RetroArcade,
        ]
    }

    /// Cycle to the next built-in palette.
    pub fn other(&self) -> Self {
        let themes = Self::builtins();
        let current = themes
            .iter()
            .position(|theme| theme.name() == self.name())
            .unwrap_or(0);
        themes[(current + 1) % themes.len()].clone()
    }

    pub fn name(&self) -> &str {
        match self {
            Self::Dark => "dark",
            Self::Light => "light",
            Self::FrostDark => "frost-dark",
            Self::NordicDeep => "nordic-deep",
            Self::CyberAutumn => "cyber-autumn",
            Self::GruvboxSepia => "gruvbox-sepia",
            Self::Synthwave84 => "synthwave-84",
            Self::DraculaVivid => "dracula-vivid",
            Self::GhostInTheShell => "ghost-in-the-shell",
            Self::VaporwaveSunset => "vaporwave-sunset",
            Self::TokyoNight => "tokyo-night",
            Self::SolarizedDesert => "solarized-desert",
            Self::DeepSpaceVoyager => "deep-space-voyager",
            Self::BioLuminescentDeep => "bio-luminescent-deep",
            Self::CrimsonAlchemist => "crimson-alchemist",
            Self::RadioactiveFallout => "radioactive-fallout",
            Self::FrozenTundra => "frozen-tundra",
            Self::MonochromaticNoir => "monochromatic-noir",
            Self::CoffeeShopCoder => "coffee-shop-coder",
            Self::DigitalForest => "digital-forest",
            Self::ElectricCandy => "electric-candy",
            Self::MidnightHacker => "midnight-hacker",
            Self::AncientPapyrus => "ancient-papyrus",
            Self::VolcanoMagma => "volcano-magma",
            Self::AuroraBorealis => "aurora-borealis",
            Self::OceanTrench => "ocean-trench",
            Self::Cyberpunk2077 => "cyberpunk-2077",
            Self::RetroArcade => "retro-arcade",
            Self::Custom { name, .. } => name,
        }
    }

    /// User-friendly display title for the theme.
    pub fn display_name(&self) -> &str {
        match self {
            Self::Dark => "Dark",
            Self::Light => "Light",
            Self::FrostDark => "Frost Dark",
            Self::NordicDeep => "Nordic Deep",
            Self::CyberAutumn => "Cyber Autumn",
            Self::GruvboxSepia => "Gruvbox Sepia",
            Self::Synthwave84 => "Synthwave '84",
            Self::DraculaVivid => "Dracula Vivid",
            Self::GhostInTheShell => "Ghost in the Shell",
            Self::VaporwaveSunset => "Vaporwave Sunset",
            Self::TokyoNight => "Tokyo Night",
            Self::SolarizedDesert => "Solarized Desert",
            Self::DeepSpaceVoyager => "Deep Space Voyager",
            Self::BioLuminescentDeep => "Bio-Luminescent Deep",
            Self::CrimsonAlchemist => "Crimson Alchemist",
            Self::RadioactiveFallout => "Radioactive Fallout",
            Self::FrozenTundra => "Frozen Tundra",
            Self::MonochromaticNoir => "Monochromatic Noir",
            Self::CoffeeShopCoder => "Coffee Shop Coder",
            Self::DigitalForest => "Digital Forest",
            Self::ElectricCandy => "Electric Candy",
            Self::MidnightHacker => "Midnight Hacker",
            Self::AncientPapyrus => "Ancient Papyrus",
            Self::VolcanoMagma => "Volcano Magma",
            Self::AuroraBorealis => "Aurora Borealis",
            Self::OceanTrench => "Ocean Trench",
            Self::Cyberpunk2077 => "Cyberpunk 2077",
            Self::RetroArcade => "Retro Arcade",
            Self::Custom { name, .. } => name.as_str(),
        }
    }

    /// One-line description of the aesthetic/vibe.
    pub fn vibe(&self) -> &str {
        match self {
            Self::Dark => "Default sleek dark palette with cyan and amber accents.",
            Self::Light => "Clean light theme with sharp contrast and blue accents.",
            Self::FrostDark => "Crisp arctic ice tones over deep navy darkness.",
            Self::NordicDeep => "Muted arctic elegance inspired by the Nord palette.",
            Self::CyberAutumn => "Warm golden embers and glowing pumpkin accents.",
            Self::GruvboxSepia => "Retro earthy warmth and high-contrast sepia tones.",
            Self::Synthwave84 => "Outrun neon magenta, electric cyan, and purple haze.",
            Self::DraculaVivid => "Vivid gothic purple with vampire neon green accents.",
            Self::GhostInTheShell => "Cyberpunk espionage, glowing tech on deep dark green.",
            Self::VaporwaveSunset => "1980s neon aesthetic, bright pinks and deep purples.",
            Self::TokyoNight => {
                "Clean, modern developer setup with deep navy and neon blue accents."
            }
            Self::SolarizedDesert => {
                "Earthy, premium low-contrast theme designed for reduced eye strain."
            }
            Self::DeepSpaceVoyager => "High contrast galactic void with cosmic starlight accents.",
            Self::BioLuminescentDeep => {
                "Abyssal underwater bioluminescence featuring soft neon cyan."
            }
            Self::CrimsonAlchemist => "Industrial steampunk, dark iron with liquid copper.",
            Self::RadioactiveFallout => "Classic terminal phosphor green with extreme contrast.",
            Self::FrozenTundra => {
                "Glacial white out, stark blue shadows, and piercing ice accents."
            }
            Self::MonochromaticNoir => "Sleek grayscale minimalism with pure white accents.",
            Self::CoffeeShopCoder => "Warm, roasted brown palettes with milky cream highlights.",
            Self::DigitalForest => "Deep woods camouflage mixed with matrix green accents.",
            Self::ElectricCandy => "Playful, hyper-saturated bubblegum pop palette.",
            Self::MidnightHacker => {
                "The classic Hollywood dark-web vibe; lime green over charcoal."
            }
            Self::AncientPapyrus => "Light mode theme evoking a weathered historical manuscript.",
            Self::VolcanoMagma => "Intensely hot volcanic theme with burning orange and deep ash.",
            Self::AuroraBorealis => {
                "Cosmic northern lights theme featuring shifts between green and purple."
            }
            Self::OceanTrench => "Dark navy gradients and neon marine biology colors.",
            Self::Cyberpunk2077 => "Glaring retro neon yellow mixed with toxic cyan.",
            Self::RetroArcade => "Classic 80s arcade machine; dark cabinet with pixelated colors.",
            Self::Custom { .. } => "Custom user-defined theme palette.",
        }
    }

    /// Interactive progress phrases associated with this theme.
    pub fn phrases(&self) -> &[&'static str] {
        match self {
            Self::GhostInTheShell => &[
                "Breaching firewalls",
                "Decrypting payloads",
                "Spoofing mainframes",
                "Hijacking uplinks",
                "Intercepting packets",
            ],
            Self::VaporwaveSunset => &[
                "Chilling out",
                "Processing nostalgia",
                "Surfing grid waves",
                "Synthesizing basslines",
                "Buffering retro",
            ],
            Self::TokyoNight => &[
                "Walking neon streets",
                "Querying databases",
                "Compiling binaries",
                "Fetching assets",
                "Initializing nodes",
            ],
            Self::SolarizedDesert => &[
                "Basking in sun",
                "Sifting through sand",
                "Aligning mirrors",
                "Absorbing heat",
                "Exhuming artifacts",
            ],
            Self::DeepSpaceVoyager => &[
                "Charting star clusters",
                "Escaping event horizons",
                "Plotting warp jumps",
                "Calibrating thrusters",
                "Transmitting telemetry",
            ],
            Self::BioLuminescentDeep => &[
                "Drifting downward",
                "Glowing silently",
                "Tracking sonar pings",
                "Exhaling bubbles",
                "Anchoring logic",
            ],
            Self::CrimsonAlchemist => &[
                "Transmuting elements",
                "Forging metal",
                "Heating crucibles",
                "Pumping bellows",
                "Cranking gears",
            ],
            Self::RadioactiveFallout => &[
                "Scrubbing radiation",
                "Booting terminals",
                "Venting reactors",
                "Checking Geiger counters",
                "Enriching fuel rods",
            ],
            Self::FrozenTundra => &[
                "Freezing routines",
                "Shoveling data snow",
                "Condensing mist",
                "Breaking glaciers",
                "Thawing pipelines",
            ],
            Self::MonochromaticNoir => &[
                "Developing negatives",
                "Smoking cigarettes",
                "Investigating files",
                "Waiting in shadows",
                "Rewinding tapes",
            ],
            Self::CoffeeShopCoder => &[
                "Grinding beans",
                "Frothing milk",
                "Roasting batches",
                "Steeping grinds",
                "Pouring shots",
            ],
            Self::DigitalForest => &[
                "Planting seeds",
                "Growing trees",
                "Intersecting roots",
                "Photosynthesizing text",
                "Rustling leaves",
            ],
            Self::ElectricCandy => &[
                "Chewing gum",
                "Popping bubbles",
                "unwrapping sweets",
                "Dissolving sugar",
                "Mixing flavors",
            ],
            Self::MidnightHacker => &[
                "Injecting scripts",
                "Cracking hashes",
                "Bypassing tokens",
                "Port scanning",
                "Elevating privileges",
            ],
            Self::AncientPapyrus => &[
                "Translating runes",
                "Unrolling scrolls",
                "Dipping quills",
                "Deciphering text",
                "Carving clay",
            ],
            Self::VolcanoMagma => &[
                "Melting rock",
                "Erupting cores",
                "Hardening basalt",
                "Flowing magma",
                "Venting pressure",
            ],
            Self::AuroraBorealis => &[
                "Shifting curtains",
                "Ionizing gas",
                "Magnetizing poles",
                "Dancing silently",
                "Particle scattering",
            ],
            Self::OceanTrench => &[
                "Submerging modules",
                "Adjusting ballasts",
                "Diving deep",
                "Mapping typography",
                "Navigating reefs",
            ],
            Self::Cyberpunk2077 => &[
                "Installing implants",
                "Overclocking optics",
                "Jacking into net",
                "Rewiring synapses",
                "Overheating ice",
            ],
            Self::RetroArcade => &[
                "Inserting coins",
                "Spawning enemies",
                "Tallying highscores",
                "Rendering sprites",
                "Leveling up",
            ],
            _ => &[
                "Thinking through the edges",
                "Reading the room (and the code)",
                "Checking a few more corners",
                "The tiny gears are turning",
                "Still on it — good work takes a moment",
            ],
        }
    }

    /// Parse a built-in theme name (case-insensitive, spaces or underscores accepted).
    pub fn parse(value: &str) -> Option<Self> {
        let normalized = normalize_name(value);
        Self::builtins()
            .into_iter()
            .find(|theme| theme.name() == normalized)
    }

    /// Construct the colors for a custom theme.
    pub fn palette(
        background: &str,
        primary: &str,
        accent: &str,
        secondary: &str,
        success: &str,
        warning: &str,
        error: &str,
        metadata: &str,
    ) -> Result<ThemeStyles, String> {
        Ok(ThemeStyles {
            bg: parse_hex_color(background)?,
            fg: parse_hex_color(primary)?,
            user_color: parse_hex_color(accent)?,
            assistant_color: parse_hex_color(accent)?,
            tool_color: parse_hex_color(warning)?,
            error_color: parse_hex_color(error)?,
            border_color: parse_hex_color(secondary)?,
            success_color: parse_hex_color(success)?,
            warning_color: parse_hex_color(warning)?,
            metadata_color: parse_hex_color(metadata)?,
        })
    }

    /// Create a custom theme after validating its name and colors.
    pub fn custom(name: &str, palette: Result<ThemeStyles, String>) -> Result<Self, String> {
        let styles = palette?;
        let name = normalize_name(name);
        if name.is_empty()
            || name.len() > 32
            || !name
                .bytes()
                .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-')
        {
            return Err("theme name must use 1-32 lowercase letters, digits, or hyphens".into());
        }
        if Self::parse(&name).is_some() {
            return Err(format!("`{name}` is a built-in theme name"));
        }
        Ok(Self::Custom { name, styles })
    }

    /// Import a custom theme from a JSON payload.
    ///
    /// Accepts a JSON string with `name` and the 8 color roles:
    /// `background`, `primary`, `accent`, `secondary`, `success`, `warning`, `error`, `metadata`.
    /// Colors may be provided at the top level or inside a `palette` object.
    pub fn from_json(json_str: &str) -> Result<Self, String> {
        #[derive(Deserialize)]
        struct ThemeJsonInput {
            name: Option<String>,
            #[serde(default)]
            palette: Option<ThemePaletteJson>,
            #[serde(default)]
            background: Option<String>,
            #[serde(default)]
            primary: Option<String>,
            #[serde(default)]
            accent: Option<String>,
            #[serde(default)]
            secondary: Option<String>,
            #[serde(default)]
            success: Option<String>,
            #[serde(default)]
            warning: Option<String>,
            #[serde(default)]
            error: Option<String>,
            #[serde(default)]
            metadata: Option<String>,
        }

        #[derive(Deserialize)]
        struct ThemePaletteJson {
            background: Option<String>,
            primary: Option<String>,
            accent: Option<String>,
            secondary: Option<String>,
            success: Option<String>,
            warning: Option<String>,
            error: Option<String>,
            metadata: Option<String>,
        }

        let input: ThemeJsonInput =
            serde_json::from_str(json_str).map_err(|e| format!("Invalid JSON: {e}"))?;
        let name = input.name.unwrap_or_else(|| "custom-theme".to_string());
        let pal = input.palette.unwrap_or(ThemePaletteJson {
            background: None,
            primary: None,
            accent: None,
            secondary: None,
            success: None,
            warning: None,
            error: None,
            metadata: None,
        });

        let bg = input
            .background
            .or(pal.background)
            .ok_or_else(|| "missing field `background`".to_string())?;
        let fg = input
            .primary
            .or(pal.primary)
            .ok_or_else(|| "missing field `primary`".to_string())?;
        let accent = input
            .accent
            .or(pal.accent)
            .ok_or_else(|| "missing field `accent`".to_string())?;
        let secondary = input
            .secondary
            .or(pal.secondary)
            .ok_or_else(|| "missing field `secondary`".to_string())?;
        let success = input
            .success
            .or(pal.success)
            .ok_or_else(|| "missing field `success`".to_string())?;
        let warning = input
            .warning
            .or(pal.warning)
            .ok_or_else(|| "missing field `warning`".to_string())?;
        let error = input
            .error
            .or(pal.error)
            .ok_or_else(|| "missing field `error`".to_string())?;
        let metadata = input
            .metadata
            .or(pal.metadata)
            .ok_or_else(|| "missing field `metadata`".to_string())?;

        let styles = Self::palette(
            &bg, &fg, &accent, &secondary, &success, &warning, &error, &metadata,
        )?;
        Self::custom(&name, Ok(styles))
    }

    /// Convert Hue, Saturation, Lightness to RGB (0-255).
    pub fn hsl_to_rgb(h: f32, s: f32, l: f32) -> (u8, u8, u8) {
        let h = (h % 360.0 + 360.0) % 360.0;
        let s = s.clamp(0.0, 1.0);
        let l = l.clamp(0.0, 1.0);
        let c = (1.0 - (2.0 * l - 1.0).abs()) * s;
        let h_prime = h / 60.0;
        let x = c * (1.0 - ((h_prime % 2.0) - 1.0).abs());
        let (r1, g1, b1) = if h_prime < 1.0 {
            (c, x, 0.0)
        } else if h_prime < 2.0 {
            (x, c, 0.0)
        } else if h_prime < 3.0 {
            (0.0, c, x)
        } else if h_prime < 4.0 {
            (0.0, x, c)
        } else if h_prime < 5.0 {
            (x, 0.0, c)
        } else {
            (c, 0.0, x)
        };
        let m = l - c / 2.0;
        (
            ((r1 + m) * 255.0).round().clamp(0.0, 255.0) as u8,
            ((g1 + m) * 255.0).round().clamp(0.0, 255.0) as u8,
            ((b1 + m) * 255.0).round().clamp(0.0, 255.0) as u8,
        )
    }

    /// Convert RGB (0-255) to Hue (0-360), Saturation (0-1), Lightness (0-1).
    pub fn rgb_to_hsl(r: u8, g: u8, b: u8) -> (f32, f32, f32) {
        let r = r as f32 / 255.0;
        let g = g as f32 / 255.0;
        let b = b as f32 / 255.0;
        let max = r.max(g).max(b);
        let min = r.min(g).min(b);
        let delta = max - min;
        let l = (max + min) / 2.0;
        if delta == 0.0 {
            return (0.0, 0.0, l);
        }
        let s = if l < 0.5 {
            delta / (max + min)
        } else {
            delta / (2.0 - max - min)
        };
        let mut h = if max == r {
            (g - b) / delta + (if g < b { 6.0 } else { 0.0 })
        } else if max == g {
            (b - r) / delta + 2.0
        } else {
            (r - g) / delta + 4.0
        };
        h *= 60.0;
        (h.clamp(0.0, 360.0), s.clamp(0.0, 1.0), l.clamp(0.0, 1.0))
    }

    /// Convert a hex color string `#RRGGBB` to (H, S, L).
    pub fn hex_to_hsl(hex: &str) -> (f32, f32, f32) {
        if let Ok(Color::Rgb(r, g, b)) = parse_hex_color(hex) {
            Self::rgb_to_hsl(r, g, b)
        } else {
            (180.0, 0.8, 0.5)
        }
    }

    /// Convert HSL to a `#RRGGBB` hex string.
    pub fn hsl_to_hex(h: f32, s: f32, l: f32) -> String {
        let (r, g, b) = Self::hsl_to_rgb(h, s, l);
        format!("#{r:02X}{g:02X}{b:02X}")
    }

    /// Parse a `#RRGGBB` hex color.
    pub fn parse_hex_color(hex: &str) -> Result<Color, String> {
        parse_hex_color(hex)
    }

    /// Convert a Color::Rgb into a `#RRGGBB` hex string.
    pub fn color_hex(color: Color) -> Result<String, String> {
        color_hex(color)
    }

    /// Load saved custom themes and the active selection.
    pub fn load_saved() -> Result<(Self, Vec<Self>), String> {
        Self::load_from(&theme_file_path())
    }

    fn load_from(path: &std::path::Path) -> Result<(Self, Vec<Self>), String> {
        let raw = match std::fs::read_to_string(&path) {
            Ok(raw) => raw,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                return Ok((Self::Dark, Vec::new()));
            }
            Err(error) => return Err(format!("could not read {}: {error}", path.display())),
        };
        let file: ThemeFile = toml::from_str(&raw)
            .map_err(|error| format!("could not parse {}: {error}", path.display()))?;
        let mut custom = Vec::with_capacity(file.custom.len());
        for (name, palette) in file.custom {
            let theme = Self::custom(
                &name,
                Self::palette(
                    &palette.background,
                    &palette.primary,
                    &palette.accent,
                    &palette.secondary,
                    &palette.success,
                    &palette.warning,
                    &palette.error,
                    &palette.metadata,
                ),
            )?;
            custom.push(theme);
        }
        let active_name = normalize_name(&file.active);
        let active = Self::parse(&active_name)
            .or_else(|| {
                custom
                    .iter()
                    .find(|theme| theme.name() == active_name)
                    .cloned()
            })
            .ok_or_else(|| format!("unknown active theme `{}`", file.active))?;
        Ok((active, custom))
    }

    /// Persist custom themes and selection to `<SICK_HOME>/themes.toml`.
    pub fn save(custom: &[Self], active: &Self) -> Result<(), String> {
        Self::save_to(custom, active, &theme_file_path())
    }

    fn save_to(custom: &[Self], active: &Self, path: &std::path::Path) -> Result<(), String> {
        let mut definitions = BTreeMap::new();
        for theme in custom {
            let Self::Custom { name, styles } = theme else {
                continue;
            };
            definitions.insert(name.clone(), ThemePalette::try_from(*styles)?);
        }
        let raw = toml::to_string_pretty(&ThemeFile {
            active: active.name().to_string(),
            custom: definitions,
        })
        .map_err(|error| format!("could not serialize theme preferences: {error}"))?;
        let parent = path
            .parent()
            .ok_or_else(|| "theme preferences path has no parent directory".to_string())?;
        std::fs::create_dir_all(parent)
            .map_err(|error| format!("could not create {}: {error}", parent.display()))?;
        std::fs::write(&path, raw)
            .map_err(|error| format!("could not save {}: {error}", path.display()))?;
        Ok(())
    }

    pub fn styles(&self) -> ThemeStyles {
        match self {
            Self::Dark => palette(
                "#111827", "#E5E7EB", "#38BDF8", "#64748B", "#34D399", "#FBBF24", "#F87171",
                "#94A3B8",
            ),
            Self::Light => palette(
                "#F8FAFC", "#0F172A", "#2563EB", "#64748B", "#059669", "#B45309", "#DC2626",
                "#475569",
            ),
            Self::FrostDark => palette(
                "#0B1220", "#E0F2FE", "#38BDF8", "#64748B", "#34D399", "#FBBF24", "#F87171",
                "#7DD3FC",
            ),
            Self::NordicDeep => palette(
                "#2E3440", "#D8DEE9", "#88C0D0", "#4C566A", "#A3BE8C", "#EBCB8B", "#BF616A",
                "#81A1C1",
            ),
            Self::CyberAutumn => palette(
                "#1C1917", "#FDE047", "#FB923C", "#78716C", "#A3E635", "#FACC15", "#F87171",
                "#D6D3D1",
            ),
            Self::GruvboxSepia => palette(
                "#282828", "#FBF1C7", "#B8BB26", "#928374", "#B8BB26", "#FABD2F", "#FB4934",
                "#83A598",
            ),
            Self::Synthwave84 => palette(
                "#190F2E", "#FFFFFF", "#F43F5E", "#A855F7", "#4ADE80", "#FACC15", "#FB7185",
                "#06B6D4",
            ),
            Self::DraculaVivid => palette(
                "#282A36", "#F8F8F2", "#50FA7B", "#6272A4", "#50FA7B", "#F1FA8C", "#FF5555",
                "#8BE9FD",
            ),
            Self::GhostInTheShell => palette(
                "#0B130E", "#E6F4EA", "#00FF66", "#4A6B53", "#10B981", "#EAB308", "#F43F5E",
                "#00E5FF",
            ),
            Self::VaporwaveSunset => palette(
                "#1A0B2E", "#FFEBF7", "#FF007F", "#6B4E8B", "#00F5D4", "#FFB703", "#FF4D6D",
                "#7B2CBF",
            ),
            Self::TokyoNight => palette(
                "#1A1B26", "#C0CAF5", "#7AA2F7", "#565F89", "#9ECE6A", "#E0AF68", "#F7768E",
                "#BB9AF7",
            ),
            Self::SolarizedDesert => palette(
                "#2A2421", "#E6D5C3", "#D97706", "#786B63", "#84CC16", "#F59E0B", "#EF4444",
                "#06B6D4",
            ),
            Self::DeepSpaceVoyager => palette(
                "#030712", "#F9FAFB", "#6366F1", "#4B5563", "#34D399", "#FB923C", "#DC2626",
                "#A5B4FC",
            ),
            Self::BioLuminescentDeep => palette(
                "#071724", "#E0F2FE", "#22D3EE", "#475569", "#10B981", "#FBBF24", "#EF4444",
                "#0284C7",
            ),
            Self::CrimsonAlchemist => palette(
                "#1C1917", "#FAFAF9", "#EA580C", "#78716C", "#16A34A", "#D97706", "#DC2626",
                "#CA8A04",
            ),
            Self::RadioactiveFallout => palette(
                "#050B05", "#39FF14", "#ADFF2F", "#1B4D1B", "#00FF00", "#FFFF00", "#FF0000",
                "#00FFCC",
            ),
            Self::FrozenTundra => palette(
                "#0F172A", "#F8FAFC", "#38BDF8", "#64748B", "#22C55E", "#EAB308", "#EF4444",
                "#93C5FD",
            ),
            Self::MonochromaticNoir => palette(
                "#121212", "#E5E5E5", "#FFFFFF", "#525252", "#A3A3A3", "#737373", "#262626",
                "#D4D4D4",
            ),
            Self::CoffeeShopCoder => palette(
                "#1C1412", "#FDF8F5", "#D4A373", "#7A6862", "#8F9779", "#DDB892", "#A63A50",
                "#E6CCB2",
            ),
            Self::DigitalForest => palette(
                "#0F1411", "#F2F7F4", "#4ADE80", "#556B5D", "#22C55E", "#F59E0B", "#EF4444",
                "#2E7D32",
            ),
            Self::ElectricCandy => palette(
                "#20081C", "#FFF0FA", "#F43F5E", "#9D4EDD", "#06B6D4", "#FF9E00", "#E63946",
                "#FF70A6",
            ),
            Self::MidnightHacker => palette(
                "#0A0E14", "#B3F36F", "#E6B450", "#4D5A6E", "#7FD962", "#F29718", "#F26D78",
                "#39BAE6",
            ),
            Self::AncientPapyrus => palette(
                "#F4ECD8", "#2B261D", "#8C6239", "#706553", "#3E6B5C", "#B38F4D", "#9E3A3A",
                "#5C7080",
            ),
            Self::VolcanoMagma => palette(
                "#170F0D", "#FFF3F0", "#FF4500", "#6B5752", "#E07A5F", "#F4A261", "#D62828",
                "#F77F00",
            ),
            Self::AuroraBorealis => palette(
                "#0D1117", "#ECFDF5", "#10B981", "#4B5563", "#34D399", "#A855F7", "#F43F5E",
                "#818CF8",
            ),
            Self::OceanTrench => palette(
                "#030A16", "#E2EAFC", "#00B4D8", "#3F51B5", "#4CAF50", "#FFC107", "#FF5722",
                "#0077B6",
            ),
            Self::Cyberpunk2077 => palette(
                "#030303", "#FCEE09", "#00F0FF", "#474747", "#39FF14", "#FF5400", "#FF0055",
                "#9D4EDD",
            ),
            Self::RetroArcade => palette(
                "#11001C", "#FFD000", "#3A86FF", "#6E1425", "#70E000", "#FF6000", "#D00000",
                "#8338EC",
            ),
            Self::Custom { styles, .. } => *styles,
        }
    }
}

impl TryFrom<ThemeStyles> for ThemePalette {
    type Error = String;

    fn try_from(styles: ThemeStyles) -> Result<Self, Self::Error> {
        Ok(Self {
            background: color_hex(styles.bg)?,
            primary: color_hex(styles.fg)?,
            accent: color_hex(styles.assistant_color)?,
            secondary: color_hex(styles.border_color)?,
            success: color_hex(styles.success_color)?,
            warning: color_hex(styles.warning_color)?,
            error: color_hex(styles.error_color)?,
            metadata: color_hex(styles.metadata_color)?,
        })
    }
}

fn normalize_name(name: &str) -> String {
    name.trim().to_ascii_lowercase().replace([' ', '_'], "-")
}

pub fn parse_hex_color(hex: &str) -> Result<Color, String> {
    let digits = hex.strip_prefix('#').unwrap_or(hex);
    if digits.len() != 6 || !digits.is_ascii() {
        return Err(format!("invalid hex color `{hex}` (expected #RRGGBB)"));
    }
    let red = u8::from_str_radix(&digits[0..2], 16)
        .map_err(|_| format!("invalid hex color `{hex}` (expected #RRGGBB)"))?;
    let green = u8::from_str_radix(&digits[2..4], 16)
        .map_err(|_| format!("invalid hex color `{hex}` (expected #RRGGBB)"))?;
    let blue = u8::from_str_radix(&digits[4..6], 16)
        .map_err(|_| format!("invalid hex color `{hex}` (expected #RRGGBB)"))?;
    Ok(Color::Rgb(red, green, blue))
}

pub fn color_hex(color: Color) -> Result<String, String> {
    match color {
        Color::Rgb(red, green, blue) => Ok(format!("#{red:02X}{green:02X}{blue:02X}")),
        _ => Err("custom theme colors must use RGB values".to_string()),
    }
}

fn palette(
    background: &str,
    primary: &str,
    accent: &str,
    secondary: &str,
    success: &str,
    warning: &str,
    error: &str,
    metadata: &str,
) -> ThemeStyles {
    Theme::palette(
        background, primary, accent, secondary, success, warning, error, metadata,
    )
    .expect("built-in theme colors are valid")
}

fn theme_file_path() -> PathBuf {
    shell_common::sick_home().join("themes.toml")
}

/// Resolved colors for the active theme.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ThemeStyles {
    pub bg: Color,
    pub fg: Color,
    pub user_color: Color,
    pub assistant_color: Color,
    pub tool_color: Color,
    pub error_color: Color,
    pub border_color: Color,
    pub success_color: Color,
    pub warning_color: Color,
    pub metadata_color: Color,
}

impl ThemeStyles {
    pub fn base(&self) -> Style {
        Style::default().bg(self.bg).fg(self.fg)
    }

    pub fn body(&self) -> Style {
        Style::default().fg(self.fg)
    }

    pub fn header(&self) -> Style {
        Style::default()
            .bg(self.assistant_color)
            .fg(self.bg)
            .add_modifier(Modifier::BOLD)
    }

    pub fn dim(&self) -> Style {
        Style::default().fg(self.metadata_color)
    }

    pub fn border(&self) -> Style {
        Style::default().fg(self.border_color)
    }

    pub fn selected(&self) -> Style {
        Style::default()
            .bg(self.assistant_color)
            .fg(self.bg)
            .add_modifier(Modifier::BOLD)
    }

    pub fn role_style(&self, role: &str) -> Style {
        match role {
            "user" => Style::default()
                .fg(self.user_color)
                .add_modifier(Modifier::BOLD),
            "assistant" => Style::default().fg(self.assistant_color),
            "tool_call" | "tool_result" | "tool" => Style::default().fg(self.tool_color),
            "subagent" => Style::default()
                .fg(self.assistant_color)
                .add_modifier(Modifier::DIM),
            "shell" => Style::default().fg(self.success_color),
            "error" => Style::default().fg(self.error_color),
            _ => Style::default().fg(self.metadata_color),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builtins_include_the_requested_palette_names() {
        let themes = Theme::builtins();
        for name in [
            "frost-dark",
            "nordic-deep",
            "cyber-autumn",
            "gruvbox-sepia",
            "synthwave-84",
            "dracula-vivid",
            "ghost-in-the-shell",
            "vaporwave-sunset",
            "tokyo-night",
            "solarized-desert",
            "deep-space-voyager",
            "bio-luminescent-deep",
            "crimson-alchemist",
            "radioactive-fallout",
            "frozen-tundra",
            "monochromatic-noir",
            "coffee-shop-coder",
            "digital-forest",
            "electric-candy",
            "midnight-hacker",
            "ancient-papyrus",
            "volcano-magma",
            "aurora-borealis",
            "ocean-trench",
            "cyberpunk-2077",
            "retro-arcade",
        ] {
            assert!(Theme::parse(name).is_some(), "missing {name}");
        }
        assert_eq!(themes.len(), 28);
    }

    #[test]
    fn custom_theme_requires_valid_name_and_colors() {
        let palette = Theme::palette(
            "#101820", "#E0F2FE", "#38BDF8", "#64748B", "#34D399", "#FBBF24", "#F87171", "#94A3B8",
        );
        assert!(Theme::custom("my-theme", palette).is_ok());
        assert!(
            Theme::custom(
                "../outside",
                Theme::palette(
                    "#101820", "#E0F2FE", "#38BDF8", "#64748B", "#34D399", "#FBBF24", "#F87171",
                    "#94A3B8",
                ),
            )
            .is_err()
        );
        assert!(
            Theme::palette(
                "not-a-color",
                "#E0F2FE",
                "#38BDF8",
                "#64748B",
                "#34D399",
                "#FBBF24",
                "#F87171",
                "#94A3B8"
            )
            .is_err()
        );
    }

    #[test]
    fn custom_theme_preferences_round_trip() {
        let dir = tempfile::tempdir().unwrap();
        let theme = Theme::custom(
            "ocean",
            Theme::palette(
                "#0B1220", "#E0F2FE", "#38BDF8", "#64748B", "#34D399", "#FBBF24", "#F87171",
                "#7DD3FC",
            ),
        )
        .unwrap();
        let path = dir.path().join("themes.toml");
        Theme::save_to(std::slice::from_ref(&theme), &theme, &path).unwrap();

        let (active, custom) = Theme::load_from(&path).unwrap();
        assert_eq!(active, theme);
        assert_eq!(custom, vec![theme]);
    }

    #[test]
    fn names_are_case_insensitive_and_normalized() {
        assert_eq!(Theme::parse("Frost Dark").unwrap().name(), "frost-dark");
        assert_eq!(Theme::default().other(), Theme::Light);
    }

    #[test]
    fn builtin_theme_persists_without_custom_themes() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("themes.toml");
        let active = Theme::TokyoNight;
        Theme::save_to(&[], &active, &path).unwrap();

        let (loaded_active, loaded_custom) = Theme::load_from(&path).unwrap();
        assert_eq!(loaded_active, Theme::TokyoNight);
        assert!(loaded_custom.is_empty());
    }

    #[test]
    fn json_import_valid_and_invalid() {
        let valid_json = r##"{
            "name": "neon-glow",
            "background": "#0B130E",
            "primary": "#E6F4EA",
            "accent": "#00FF66",
            "secondary": "#4A6B53",
            "success": "#10B981",
            "warning": "#EAB308",
            "error": "#F43F5E",
            "metadata": "#00E5FF"
        }"##;
        let theme = Theme::from_json(valid_json).expect("valid theme json should parse");
        assert_eq!(theme.name(), "neon-glow");

        let missing_color = r##"{
            "name": "incomplete",
            "background": "#0B130E"
        }"##;
        assert!(Theme::from_json(missing_color).is_err());

        let invalid_hex = r##"{
            "name": "bad-hex",
            "background": "not-hex",
            "primary": "#E6F4EA",
            "accent": "#00FF66",
            "secondary": "#4A6B53",
            "success": "#10B981",
            "warning": "#EAB308",
            "error": "#F43F5E",
            "metadata": "#00E5FF"
        }"##;
        assert!(Theme::from_json(invalid_hex).is_err());
    }

    #[test]
    fn hsl_and_rgb_conversions_round_trip() {
        // Red
        let (r, g, b) = Theme::hsl_to_rgb(0.0, 1.0, 0.5);
        assert_eq!((r, g, b), (255, 0, 0));
        let (h, s, l) = Theme::rgb_to_hsl(255, 0, 0);
        assert_eq!(h.round() as u32, 0);
        assert!((s - 1.0).abs() < 0.01);
        assert!((l - 0.5).abs() < 0.01);

        // Hex conversion
        let hex = Theme::hsl_to_hex(120.0, 1.0, 0.5);
        assert_eq!(hex, "#00FF00");
    }
}
