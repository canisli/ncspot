#![allow(clippy::enum_glob_use)]

use cursive::theme::BaseColor::*;
use cursive::theme::Color::*;
use cursive::theme::PaletteColor::*;
use cursive::theme::*;
use log::warn;

use crate::config::{ConfigTheme, ConfigThemeConfig};

#[derive(Debug, Copy, Clone)]
enum Appearance {
    Light,
    Dark,
}

#[cfg(target_os = "macos")]
fn detect_appearance() -> Appearance {
    use std::process::Command;

    // `defaults read -g AppleInterfaceStyle` exits with 0 when Dark Mode is set.
    match Command::new("defaults")
        .args(["read", "-g", "AppleInterfaceStyle"])
        .output()
    {
        Ok(output) if output.status.success() => Appearance::Dark,
        _ => Appearance::Light,
    }
}

#[cfg(not(target_os = "macos"))]
fn detect_appearance() -> Appearance {
    Appearance::Light
}

fn select_theme(theme_cfg: &ConfigThemeConfig) -> Option<ConfigTheme> {
    let appearance = detect_appearance();

    match appearance {
        Appearance::Dark => theme_cfg
            .dark
            .clone()
            .or_else(|| theme_cfg.light.clone())
            .or_else(|| Some(theme_cfg.base.clone())),
        Appearance::Light => theme_cfg
            .light
            .clone()
            .or_else(|| theme_cfg.dark.clone())
            .or_else(|| Some(theme_cfg.base.clone())),
    }
}

/// Get the given color from the given [ConfigTheme]. The first argument is the [ConfigTheme] to get
/// the color out of. The second argument is the name of the color to get and is an identifier. The
/// third argument is a [Color] that is used as the default when no color can be parsed from the
/// provided [ConfigTheme].
///
/// # Examples
///
/// ```rust
/// load_color!(config_theme, background, TerminalDefault)
/// load_color!(config_theme, primary, TerminalDefault)
/// ```
macro_rules! load_color {
    ( $theme: expr_2021, $member: ident, $default: expr_2021 ) => {
        $theme
            .as_ref()
            .and_then(|t| t.$member.clone())
            .and_then(|c| Color::parse(c.as_ref()))
            .unwrap_or_else(|| {
                warn!(
                    "Failed to parse color in \"{}\", falling back to default",
                    stringify!($member)
                );
                $default
            })
    };
}

/// Create a [cursive::theme::Theme] from `theme_cfg`.
pub fn load(theme_cfg: &Option<ConfigThemeConfig>) -> Theme {
    let mut palette = Palette::default();
    let borders = BorderStyle::Simple;

    let selected_theme: Option<ConfigTheme> = theme_cfg.as_ref().and_then(select_theme);

    palette[Background] = load_color!(&selected_theme, background, TerminalDefault);
    palette[View] = load_color!(&selected_theme, background, TerminalDefault);
    palette[Primary] = load_color!(&selected_theme, primary, TerminalDefault);
    palette[Secondary] = load_color!(&selected_theme, secondary, Dark(Blue));
    palette[TitlePrimary] = load_color!(&selected_theme, title, Dark(Red));
    palette[HighlightText] = load_color!(&selected_theme, highlight, Dark(White));
    palette[Highlight] = load_color!(&selected_theme, highlight_bg, Dark(Red));
    palette[HighlightInactive] =
        load_color!(&selected_theme, highlight_inactive_bg, Dark(Blue));
    palette.set_color("playing", load_color!(&selected_theme, playing, Dark(Blue)));
    palette.set_color(
        "playing_selected",
        load_color!(&selected_theme, playing_selected, Light(Blue)),
    );
    palette.set_color(
        "playing_bg",
        load_color!(&selected_theme, playing_bg, TerminalDefault),
    );
    palette.set_color("error", load_color!(&selected_theme, error, TerminalDefault));
    palette.set_color("error_bg", load_color!(&selected_theme, error_bg, Dark(Red)));
    palette.set_color(
        "statusbar_progress",
        load_color!(&selected_theme, statusbar_progress, Dark(Blue)),
    );
    palette.set_color(
        "statusbar_progress_bg",
        load_color!(&selected_theme, statusbar_progress_bg, Light(Black)),
    );
    palette.set_color(
        "statusbar",
        load_color!(&selected_theme, statusbar, Dark(Yellow)),
    );
    palette.set_color(
        "statusbar_bg",
        load_color!(&selected_theme, statusbar_bg, TerminalDefault),
    );
    palette.set_color(
        "cmdline",
        load_color!(&selected_theme, cmdline, TerminalDefault),
    );
    palette.set_color(
        "cmdline_bg",
        load_color!(&selected_theme, cmdline_bg, TerminalDefault),
    );
    palette.set_color(
        "search_match",
        load_color!(&selected_theme, search_match, Light(Red)),
    );

    Theme {
        shadow: false,
        palette,
        borders,
    }
}
