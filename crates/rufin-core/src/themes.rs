use serde::{Deserialize, Serialize};
use std::{collections::BTreeMap, path::Path};

pub const CONTROLLER_COLORS: &[(&str, &str)] = &[
    ("--window-bg-color", "var(--window-bg-color)"),
    ("--window-fg-color", "var(--window-fg-color)"),
    ("--card-bg-color", "var(--card-bg-color)"),
    ("--accent-color", "var(--accent-color)"),
    ("--bg", "var(--view-bg-color)"),
    ("--sidebar", "var(--sidebar-bg-color)"),
    ("--surface", "var(--card-bg-color)"),
    ("--player", "var(--headerbar-bg-color)"),
    ("--text", "var(--view-fg-color)"),
    ("--accent", "var(--accent-color)"),
    ("--blue", "var(--accent-bg-color)"),
    // A favorite takes the accent, as on GTK.
    ("--favorite", "var(--accent-color)"),
    ("--accent-foreground", "var(--accent-fg-color)"),
    ("--popover", "var(--popover-bg-color)"),
    ("--right-sidebar", "var(--secondary-sidebar-bg-color)"),
    ("--line", "var(--sidebar-border-color)"),
    ("--border-color", "var(--border-color)"),
    ("--hover", "var(--shade-color)"),
    (
        "--muted",
        "color-mix(in srgb, var(--view-fg-color) 60%, transparent)",
    ),
];

#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum Mode {
    Light,
    Dark,
}

#[derive(Clone, Debug, Deserialize)]
pub struct Accent {
    pub name: String,
    pub color: String,
    pub foreground: String,
}

#[derive(Clone, Debug, Deserialize)]
pub struct Theme {
    #[serde(skip)]
    pub path: Option<std::path::PathBuf>,
    #[serde(skip)]
    pub id: String,
    pub name: String,
    pub mode: Mode,
    pub colors: BTreeMap<String, String>,
    #[serde(default)]
    pub accents: Vec<Accent>,
}

impl Theme {
    pub fn colors(&self, themes: &[Theme], accent: Option<&str>) -> BTreeMap<String, String> {
        let mut colors = themes[usize::from(self.mode == Mode::Dark)].colors.clone();
        colors.extend(self.colors.clone());
        if let Some(accent) = self.accent(accent) {
            colors.insert("accent-bg-color".into(), accent.color.clone());
            colors.insert("accent-fg-color".into(), accent.foreground.clone());
            colors.insert(
                "accent-color".into(),
                "oklab(from var(--accent-bg-color) var(--standalone-color-oklab))".into(),
            );
        }
        colors
    }

    pub fn accent(&self, name: Option<&str>) -> Option<&Accent> {
        self.accents
            .iter()
            .find(|accent| Some(accent.name.as_str()) == name)
            .or_else(|| self.accents.first())
    }
}

pub fn controller_colors(
    settings: &crate::settings::Settings,
    directory: &Path,
) -> BTreeMap<String, String> {
    let preference = &settings.appearance.theme_preference;
    let mut themes = builtins();
    if matches!(preference, crate::settings::ThemePreference::Named(id) if id.starts_with("custom:"))
    {
        let (custom, errors) = custom(directory);
        for error in errors {
            tracing::warn!(%error, "could not load theme");
        }
        themes.extend(custom);
    }
    let Some(theme) = preference.selected(&themes) else {
        return BTreeMap::new();
    };
    theme_colors(
        theme,
        &themes,
        settings.appearance.accent_preference,
        &settings.appearance.theme_accents,
    )
}

pub fn theme_colors(
    theme: &Theme,
    themes: &[Theme],
    accent: crate::settings::AccentPreference,
    accents: &BTreeMap<String, String>,
) -> BTreeMap<String, String> {
    let id = &theme.id;
    let mut colors: BTreeMap<String, String> = theme
        .colors(themes, accents.get(id).map(String::as_str))
        .into_iter()
        .map(|(key, value)| (format!("--{key}"), value))
        .collect();
    if theme.accents.is_empty()
        && let Some(color) = accent.color()
    {
        colors.insert("--accent-bg-color".into(), color.into());
        colors.insert("--accent-color".into(), color.into());
        colors.insert("--accent-fg-color".into(), "#ffffff".into());
    }
    colors
        .entry("--accent-bg-color".into())
        .or_insert_with(|| "#3584e4".into());
    colors
        .entry("--accent-fg-color".into())
        .or_insert_with(|| "#ffffff".into());
    colors.entry("--accent-color".into()).or_insert_with(|| {
        "oklab(from var(--accent-bg-color) var(--standalone-color-oklab))".into()
    });
    colors
        .entry("--error-color".into())
        .or_insert_with(|| "#e62d42".into());
    for &(key, value) in CONTROLLER_COLORS {
        if value != format!("var({key})") {
            colors.entry(key.into()).or_insert_with(|| value.into());
        }
    }
    colors.insert(
        "color-scheme".into(),
        match theme.mode {
            Mode::Light => "light",
            Mode::Dark => "dark",
        }
        .into(),
    );
    colors
}

/// The theme color a preview shape takes.
#[derive(Clone, Copy, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum PreviewColor {
    Window,
    Sidebar,
    View,
    Card,
    Text,
    Accent,
    AccentText,
}

impl PreviewColor {
    /// The theme's own name for the color; the two accent colors come from the chosen accent.
    pub const fn name(self) -> &'static str {
        match self {
            Self::Window => "window-bg-color",
            Self::Sidebar => "sidebar-bg-color",
            Self::View => "view-bg-color",
            Self::Card => "card-bg-color",
            Self::Text => "view-fg-color",
            Self::Accent => "accent-bg-color",
            Self::AccentText => "accent-fg-color",
        }
    }
}

/// One shape of a theme tile's picture: a rounded box, or the named icon in that box.
#[derive(Clone, Copy, Serialize)]
pub struct PreviewShape {
    pub x: f32,
    pub y: f32,
    pub width: f32,
    pub height: f32,
    pub radius: f32,
    pub color: PreviewColor,
    pub alpha: f32,
    pub icon: Option<&'static str>,
}

/// A theme tile's picture, back to front: the top-left of the Home page at about half size,
/// in pixels of a 96 pixel high tile. Shapes run past the tile's right and bottom edges and
/// are clipped there, so a wider tile shows more of the page.
pub fn preview() -> Vec<PreviewShape> {
    use crate::settings::sidebar::SidebarRouteItem as Item;
    use PreviewColor::*;
    const EDGE: f32 = 2000.0;
    let rect = |x, y, width, height, radius, color, alpha| PreviewShape {
        x,
        y,
        width,
        height,
        radius,
        color,
        alpha,
        icon: None,
    };
    let icon = |name, x, y, size, color, alpha| PreviewShape {
        x,
        y,
        width: size,
        height: size,
        radius: 0.0,
        color,
        alpha,
        icon: Some(name),
    };
    let mut shapes = vec![
        rect(0.0, 0.0, EDGE, EDGE, 0.0, Window, 1.0),
        icon("rufin-sidebar-show-symbolic", 8.0, 5.5, 8.0, Text, 0.85),
        icon("rufin-go-previous-symbolic", 23.0, 5.5, 8.0, Text, 0.85),
        icon("rufin-go-next-symbolic", 35.0, 5.5, 8.0, Text, 0.4),
        rect(100.0, 3.5, EDGE, 12.0, 6.0, Text, 0.09),
        icon("rufin-search-symbolic", 105.0, 6.0, 7.0, Text, 0.6),
        rect(4.0, 19.0, 56.0, EDGE, 6.0, Sidebar, 1.0),
        rect(7.0, 22.0, 50.0, 13.0, 4.0, Text, 0.11),
    ];
    let rows = [
        (Item::Home, 18.0),
        (Item::Favorites, 27.0),
        (Item::Albums, 22.0),
        (Item::Tracks, 19.0),
        (Item::Artists, 21.0),
        (Item::Folders, 24.0),
    ];
    for (row, (item, width)) in rows.into_iter().enumerate() {
        let y = row as f32 * 15.0;
        let (icon_alpha, text_alpha) = if row == 0 { (1.0, 0.95) } else { (0.75, 0.6) };
        let name = crate::route::sidebar_route_descriptor(item).icon_name;
        shapes.push(icon(name, 11.0, 24.5 + y, 8.0, Text, icon_alpha));
        shapes.push(rect(24.0, 27.0 + y, width, 3.0, 1.5, Text, text_alpha));
    }
    shapes.extend([
        rect(64.0, 19.0, EDGE, EDGE, 6.0, View, 1.0),
        rect(68.0, 23.0, EDGE, 30.0, 5.0, Accent, 0.22),
        rect(74.0, 30.0, 31.0, 31.0, 3.5, View, 1.0),
        rect(74.0, 30.0, 31.0, 31.0, 3.5, Text, 0.2),
        rect(111.0, 32.0, 46.0, 4.5, 2.25, Text, 0.95),
        rect(111.0, 41.0, 30.0, 3.0, 1.5, Text, 0.55),
        rect(111.0, 58.0, 33.0, 13.5, 6.75, Accent, 1.0),
        icon(
            "rufin-media-playback-start-symbolic",
            116.0,
            61.5,
            6.5,
            AccentText,
            1.0,
        ),
        rect(125.5, 63.2, 13.0, 3.0, 1.5, AccentText, 1.0),
        rect(148.0, 58.0, 30.0, 13.5, 6.75, Text, 0.11),
        rect(68.0, 79.0, 26.0, 4.5, 2.25, Text, 0.95),
        rect(68.0, 89.0, EDGE, 20.0, 5.0, Card, 1.0),
    ]);
    shapes
}

pub fn builtins() -> Vec<Theme> {
    macro_rules! themes {
        ($($id:literal),+ $(,)?) => {
            vec![$({
                let mut theme: Theme = serde_json::from_str(include_str!(concat!("../../../resources/themes/", $id, ".json")))
                    .expect("bundled theme must be valid");
                theme.id = concat!("builtin:", $id).into();
                theme
            }),+]
        };
    }
    themes!(
        "light",
        "dark",
        "catppuccin-latte",
        "catppuccin-frappe",
        "catppuccin-macchiato",
        "catppuccin-mocha",
        "dracula",
        "gruvbox",
        "nord",
        "one-dark",
        "tokyo-night",
        "rose-pine",
        "solarized",
        "everforest"
    )
}

pub fn custom(directory: &Path) -> (Vec<Theme>, Vec<String>) {
    let entries = match std::fs::read_dir(directory) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Default::default(),
        Err(error) => {
            return (
                Vec::new(),
                vec![format!("{}: {error}", directory.display())],
            );
        }
    };
    let mut themes = Vec::new();
    let mut errors = Vec::new();
    for entry in entries {
        let result = entry.map_err(|error| error.to_string()).and_then(|entry| {
            let path = entry.path();
            if path
                .extension()
                .is_none_or(|extension| !extension.eq_ignore_ascii_case("json"))
            {
                return Ok(None);
            }
            let read = || -> Result<Theme, Box<dyn std::error::Error>> {
                let mut theme: Theme = serde_json::from_slice(&std::fs::read(&path)?)?;
                theme.id = format!(
                    "custom:{}",
                    path.file_stem().unwrap_or_default().to_string_lossy()
                );
                theme.path = Some(path.clone());
                Ok(theme)
            };
            read()
                .map(Some)
                .map_err(|error| format!("{}: {error}", path.display()))
        });
        match result {
            Ok(Some(theme)) => themes.push(theme),
            Ok(None) => (),
            Err(error) => errors.push(error),
        }
    }
    themes.sort_by(|left, right| left.name.cmp(&right.name).then(left.id.cmp(&right.id)));
    (themes, errors)
}

pub fn create_folder(directory: &Path) -> std::io::Result<bool> {
    std::fs::create_dir_all(directory)?;
    if std::fs::read_dir(directory)?.next().is_none() {
        use std::io::Write;
        std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(directory.join("custom.json"))?
            .write_all(include_bytes!("../../../resources/themes/custom.json"))?;
        return Ok(true);
    }
    Ok(false)
}

pub fn import(directory: &Path, name: &str, bytes: &[u8]) -> Result<String, String> {
    serde_json::from_slice::<Theme>(bytes).map_err(|error| error.to_string())?;
    let stem = Path::new(name)
        .file_stem()
        .and_then(|stem| stem.to_str())
        .filter(|stem| !stem.is_empty())
        .ok_or_else(|| "Theme file needs a name".to_string())?;
    std::fs::create_dir_all(directory).map_err(|error| error.to_string())?;
    std::fs::write(directory.join(format!("{stem}.json")), bytes)
        .map_err(|error| error.to_string())?;
    Ok(format!("custom:{stem}"))
}
