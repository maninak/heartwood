use std::ffi::OsStr;
use std::{env, fmt};

use inquire::ui::{ErrorMessageRenderConfig, StyleSheet, Styled};
use inquire::InquireError;
use inquire::{ui::Color, ui::RenderConfig, Confirm, CustomType, Password};
use once_cell::sync::Lazy;
use zeroize::Zeroizing;

use crate::command;
use crate::format;
use crate::{style, Paint};

// TODO: Try not to export this.
pub use inquire::Select;

pub const ERROR_PREFIX: Paint<&str> = Paint::red("✗");
pub const ERROR_HINT_PREFIX: Paint<&str> = Paint::yellow("✗");
pub const WARNING_PREFIX: Paint<&str> = Paint::yellow("!");
pub const TAB: &str = "    ";

/// Passphrase input.
pub type Passphrase = Zeroizing<String>;

/// Render configuration.
pub static CONFIG: Lazy<RenderConfig> = Lazy::new(|| RenderConfig {
    prompt: StyleSheet::new().with_fg(Color::LightCyan),
    prompt_prefix: Styled::new("?").with_fg(Color::LightBlue),
    answered_prompt_prefix: Styled::new("✓").with_fg(Color::LightGreen),
    answer: StyleSheet::new(),
    highlighted_option_prefix: Styled::new("*").with_fg(Color::LightYellow),
    help_message: StyleSheet::new().with_fg(Color::DarkGrey),
    error_message: ErrorMessageRenderConfig::default_colored()
        .with_prefix(Styled::new("✗").with_fg(Color::LightRed)),
    ..RenderConfig::default_colored()
});

#[macro_export]
macro_rules! info {
    ($($arg:tt)*) => ({
        println!("{}", format_args!($($arg)*));
    })
}

#[macro_export]
macro_rules! success {
    ($($arg:tt)*) => ({
        $crate::io::success_args(format_args!($($arg)*));
    })
}

#[macro_export]
macro_rules! tip {
    ($($arg:tt)*) => ({
        $crate::io::tip_args(format_args!($($arg)*));
    })
}

pub use info;
pub use success;
pub use tip;

pub fn success_args(args: fmt::Arguments) {
    println!("{} {args}", Paint::green("✓"));
}

pub fn tip_args(args: fmt::Arguments) {
    println!("👉 {}", style(format!("{args}")).italic());
}

pub fn columns() -> Option<usize> {
    termion::terminal_size().map(|(cols, _)| cols as usize).ok()
}

pub fn headline(headline: impl fmt::Display) {
    println!();
    println!("{}", style(headline).bold());
    println!();
}

pub fn header(header: &str) {
    println!();
    println!("{}", style(format::yellow(header)).bold().underline());
    println!();
}

pub fn blob(text: impl fmt::Display) {
    println!("{}", style(text.to_string().trim()).dim());
}

pub fn blank() {
    println!()
}

pub fn print(msg: impl fmt::Display) {
    println!("{msg}");
}

pub fn prefixed(prefix: &str, text: &str) -> String {
    text.split('\n')
        .map(|line| format!("{prefix}{line}\n"))
        .collect()
}

pub fn help(name: &str, version: &str, description: &str, usage: &str) {
    println!("rad-{name} {version}\n{description}\n{usage}");
}

pub fn usage(name: &str, usage: &str) {
    println!(
        "{} {}\n{}",
        ERROR_PREFIX,
        Paint::red(format!("Error: rad-{name}: invalid usage")),
        Paint::red(prefixed(TAB, usage)).dim()
    );
}

pub fn println(prefix: impl fmt::Display, msg: impl fmt::Display) {
    println!("{prefix} {msg}");
}

pub fn indented(msg: impl fmt::Display) {
    println!("{TAB}{msg}");
}

pub fn subcommand(msg: impl fmt::Display) {
    println!("{} {}", style("$").dim(), style(msg).dim());
}

pub fn warning(warning: &str) {
    println!(
        "{} {} {warning}",
        WARNING_PREFIX,
        Paint::yellow("Warning:").bold(),
    );
}

pub fn error(error: impl fmt::Display) {
    println!("{ERROR_PREFIX} {error}");
}

pub fn ask<D: fmt::Display>(prompt: D, default: bool) -> bool {
    let prompt = prompt.to_string();

    Confirm::new(&prompt)
        .with_default(default)
        .with_render_config(*CONFIG)
        .prompt()
        .unwrap_or_default()
}

pub fn confirm<D: fmt::Display>(prompt: D) -> bool {
    ask(prompt, true)
}

pub fn abort<D: fmt::Display>(prompt: D) -> bool {
    ask(prompt, false)
}

pub fn input<S, E>(message: &str, default: Option<S>) -> anyhow::Result<S>
where
    S: fmt::Display + std::str::FromStr<Err = E> + Clone,
    E: fmt::Debug + fmt::Display,
{
    let input = CustomType::<S>::new(message).with_render_config(*CONFIG);
    let value = match default {
        Some(default) => input.with_default(default).prompt()?,
        None => input.prompt()?,
    };
    Ok(value)
}

pub fn passphrase<K: AsRef<OsStr>>(var: K) -> Result<Passphrase, anyhow::Error> {
    if let Ok(p) = env::var(var) {
        Ok(Passphrase::from(p))
    } else {
        Ok(Passphrase::from(
            Password::new("Passphrase:")
                .with_render_config(*CONFIG)
                .with_display_mode(inquire::PasswordDisplayMode::Masked)
                .without_confirmation()
                .prompt()?,
        ))
    }
}

pub fn passphrase_confirm<K: AsRef<OsStr>>(
    prompt: &str,
    var: K,
) -> Result<Passphrase, anyhow::Error> {
    if let Ok(p) = env::var(var) {
        Ok(Passphrase::from(p))
    } else {
        Ok(Passphrase::from(
            Password::new(prompt)
                .with_render_config(*CONFIG)
                .with_display_mode(inquire::PasswordDisplayMode::Masked)
                .with_custom_confirmation_message("Repeat passphrase:")
                .with_custom_confirmation_error_message("The passphrases don't match.")
                .with_help_message("This passphrase protects your radicle identity")
                .prompt()?,
        ))
    }
}

pub fn passphrase_stdin() -> Result<Passphrase, anyhow::Error> {
    let mut input = String::new();
    std::io::stdin().read_line(&mut input)?;

    Ok(Passphrase::from(input.trim_end().to_owned()))
}

pub fn select<'a, T>(
    prompt: &str,
    options: &'a [T],
    active: &'a T,
) -> Result<Option<&'a T>, InquireError>
where
    T: fmt::Display + Eq + PartialEq,
{
    let active = options.iter().position(|o| o == active);
    let selection =
        Select::new(prompt, options.iter().collect::<Vec<_>>()).with_render_config(*CONFIG);

    if let Some(active) = active {
        selection.with_starting_cursor(active).prompt_skippable()
    } else {
        selection.prompt_skippable()
    }
}

pub fn markdown(content: &str) {
    if !content.is_empty() && command::bat(["-p", "-l", "md"], content).is_err() {
        blob(content);
    }
}
