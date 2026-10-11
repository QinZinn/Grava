//! The generated documentation: shell completions and the manual page. Both
//! come from the clap definitions, so they cannot drift from `--help`.

use std::io::{self, Write};

use clap::CommandFactory;
use clap_complete::{Shell, generate};

use crate::{
    cli::{Cli, CompletionShell},
    render::hud::help_lines,
};

impl From<CompletionShell> for Shell {
    fn from(shell: CompletionShell) -> Self {
        match shell {
            CompletionShell::Bash => Self::Bash,
            CompletionShell::Zsh => Self::Zsh,
            CompletionShell::Fish => Self::Fish,
        }
    }
}

/// Write the completion script for `shell`.
pub fn write_completions(shell: CompletionShell, out: &mut dyn Write) {
    let mut command = Cli::command();
    generate(Shell::from(shell), &mut command, "grava", out);
}

/// Write the manual page, roff source for `man(1)`.
pub fn write_man_page(out: &mut dyn Write) -> io::Result<()> {
    let command = Cli::command();
    // The version text of `--version` has several lines; the title takes one.
    let man = clap_mangen::Man::new(command)
        .source(format!("grava {}", env!("CARGO_PKG_VERSION")))
        .manual("User Commands");
    man.render_title(out)?;
    man.render_name_section(out)?;
    man.render_synopsis_section(out)?;
    man.render_description_section(out)?;
    man.render_options_section(out)?;
    out.write_all(extra_sections().as_bytes())
        .and_then(|()| out.write_all(b".SH VERSION\n"))
        .and_then(|()| out.write_all(escape(env!("CARGO_PKG_VERSION")).as_bytes()))
        .and_then(|()| out.write_all(b"\n"))
}

/// A name or a command, in roff's bold face.
fn bold(text: &str) -> String {
    format!("\\fB{}\\fR", escape(text))
}

/// Text for roff: backslashes and hyphens are special, and a line must not
/// start with a control character.
fn escape(text: &str) -> String {
    let mut escaped = String::with_capacity(text.len());
    for character in text.chars() {
        match character {
            '\\' => escaped.push_str("\\e"),
            '-' => escaped.push_str("\\-"),
            _ => escaped.push(character),
        }
    }
    escaped
}

/// Free text: a line that starts with `.` or `'` would be a request.
fn paragraph(text: &str) -> String {
    let mut lines = Vec::new();
    for line in text.lines() {
        let line = escape(line);
        if line.starts_with(['.', '\'']) {
            lines.push(format!("\\&{line}"));
        } else {
            lines.push(line);
        }
    }
    lines.join("\n")
}

fn section(title: &str, body: &str, out: &mut String) {
    out.push_str(&format!(".SH {title}\n{body}\n"));
}

/// A list of `(term, description)` pairs as tagged paragraphs.
fn definitions(items: &[(String, &str)]) -> String {
    let mut text = String::new();
    for (term, description) in items {
        text.push_str(&format!(".TP\n{term}\n{}\n", paragraph(description)));
    }
    text
}

fn extra_sections() -> String {
    let mut out = String::new();

    section(
        "FILES",
        &definitions(&[
            (
                bold("$XDG_CONFIG_HOME/grava/config.toml"),
                "The configuration, or ~/.config/grava/config.toml when XDG_CONFIG_HOME is not set. \
                 Every key is optional. grava never creates it by itself: \
                 `grava --init` writes a fully commented copy, and `grava --print-default-config` prints one.",
            ),
            (
                bold("$XDG_CONFIG_HOME/grava/themes/NAME.toml"),
                "User themes. A file with the name of a built-in theme replaces it. \
                 `grava --list-themes` lists everything that can be chosen.",
            ),
            (
                bold("$XDG_CACHE_HOME/wal/colors.json"),
                "The colors read by the pywal theme (~/.cache/wal/colors.json by default).",
            ),
        ]),
        &mut out,
    );

    let mut keys = Vec::new();
    for (keys_text, what) in help_lines(false) {
        if keys_text != "any key" {
            keys.push((bold(keys_text), what));
        }
    }
    section("KEYS", &definitions(&keys), &mut out);

    section(
        "SIGNALS",
        &definitions(&[
            (
                bold("SIGUSR1"),
                "Reload the whole configuration, like the r key.",
            ),
            (bold("SIGUSR2"), "Reload the colors only, like the c key."),
            (
                format!(
                    "{}, {}, {}",
                    bold("SIGINT"),
                    bold("SIGTERM"),
                    bold("SIGHUP")
                ),
                "Quit and restore the terminal.",
            ),
        ]),
        &mut out,
    );

    section(
        "ENVIRONMENT",
        &definitions(&[
            (
                format!("{}, {}", bold("XDG_CONFIG_HOME"), bold("HOME")),
                "Where the configuration and the themes are looked for.",
            ),
            (
                bold("NO_COLOR"),
                "When set and not empty, colors are off unless render.color_mode names one.",
            ),
            (
                format!("{}, {}", bold("COLORTERM"), bold("TERM")),
                "Choose the color depth. COLORTERM=truecolor gives 24-bit colors, \
                 TERM=linux (the Linux console) 16 colors with ASCII glyphs and no alternate screen.",
            ),
            (
                format!("{}, {}, {}", bold("LC_ALL"), bold("LC_CTYPE"), bold("LANG")),
                "A UTF-8 locale lets render.glyphs = auto use Braille; anything else falls back to ASCII.",
            ),
            (
                format!("{}, {}", bold("PULSE_SERVER"), bold("XDG_RUNTIME_DIR")),
                "Where the PulseAudio or PipeWire socket is found.",
            ),
        ]),
        &mut out,
    );

    section(
        "EXAMPLES",
        &format!(
            ".TP\n{}\nWatch the built\\-in demo without any audio system.\n\
             .TP\n{}\nDraw the monitor of the default output on the circular plate.\n\
             .TP\n{}\nDraw what parec records from the default monitor.\n\
             .TP\n{}\nPrint a single deterministic frame, for tests and screenshots.\n",
            bold("grava -i demo"),
            bold("grava --shape circle --theme nord"),
            bold(
                "parec -d @DEFAULT_MONITOR@ --format=s16le --rate=48000 --channels=2 | grava -i stdin"
            ),
            bold("grava --headless -i demo --frames 300 --size 80x40 --seed 42 --no-color"),
        ),
        &mut out,
    );

    section(
        "EXIT STATUS",
        "0 after a normal exit, 1 when the configuration is invalid, no audio input can be opened, \
         or the terminal cannot be used.",
        &mut out,
    );
    out
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use super::*;

    fn completions(shell: CompletionShell) -> String {
        let mut bytes = Vec::new();
        write_completions(shell, &mut bytes);
        String::from_utf8(bytes).unwrap()
    }

    fn man_page() -> String {
        let mut bytes = Vec::new();
        write_man_page(&mut bytes).unwrap();
        String::from_utf8(bytes).unwrap()
    }

    #[test]
    fn every_shell_completes_every_option() {
        let command = Cli::command();
        let longs: Vec<_> = command
            .get_arguments()
            .filter_map(|argument| argument.get_long())
            .filter(|name| !matches!(*name, "help" | "version"))
            .collect();
        assert!(longs.len() > 15);
        for shell in [
            CompletionShell::Bash,
            CompletionShell::Zsh,
            CompletionShell::Fish,
        ] {
            let script = completions(shell);
            assert!(script.contains("grava"), "{shell:?}");
            for long in &longs {
                assert!(script.contains(long), "{shell:?} lacks --{long}");
            }
        }
        // Values are completed too.
        assert!(completions(CompletionShell::Bash).contains("braille"));
        assert!(completions(CompletionShell::Fish).contains("quadrant"));
    }

    #[test]
    fn the_man_page_documents_the_options_the_keys_and_the_environment() {
        let page = man_page();
        assert!(page.starts_with(".ie \\n(.g .ds Aq"), "{}", &page[..40]);
        assert!(page.contains(".TH grava 1"));
        for heading in [
            "NAME",
            "SYNOPSIS",
            "DESCRIPTION",
            "OPTIONS",
            "FILES",
            "KEYS",
            "SIGNALS",
            "ENVIRONMENT",
            "EXAMPLES",
            "EXIT STATUS",
        ] {
            assert!(page.contains(&format!(".SH {heading}")), "{heading}");
        }
        let command = Cli::command();
        for argument in command.get_arguments() {
            if let Some(long) = argument.get_long() {
                assert!(
                    page.contains(&escape(&format!("--{long}"))),
                    "--{long} missing"
                );
            }
        }
        for (keys, what) in help_lines(false) {
            if keys != "any key" {
                assert!(page.contains(&escape(keys)), "key {keys}");
                assert!(page.contains(&escape(what)), "{what}");
            }
        }
        for word in [
            "SIGUSR1",
            "SIGUSR2",
            "NO_COLOR",
            "XDG_CONFIG_HOME",
            "themes/NAME.toml",
        ] {
            assert!(
                page.contains(&escape(word)) || page.contains(word),
                "{word}"
            );
        }
        // Text pasted into roff cannot start a request by accident.
        for line in page.lines() {
            assert!(
                !line.starts_with(".-") && !line.contains("\\fB\\fR"),
                "{line}"
            );
        }
    }

    #[test]
    fn roff_escaping_protects_hyphens_backslashes_and_leading_dots() {
        assert_eq!(escape("a-b\\c"), "a\\-b\\ec");
        assert_eq!(
            paragraph(".TH evil\nnormal\n'quote"),
            "\\&.TH evil\nnormal\n\\&'quote"
        );
        assert_eq!(bold("--x"), "\\fB\\-\\-x\\fR");
    }
}
