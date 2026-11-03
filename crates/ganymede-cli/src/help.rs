//! The top-level `ganymede --help`: colours, commands in groups, examples,
//! and exit statuses. Each subcommand's own `--help` is clap's default
//! layout with the same colours.

use clap::builder::styling::{AnsiColor, Style, Styles};
use clap::{Command, CommandFactory};

/// Colours for every help page and error, as cargo uses.
pub const STYLES: Styles = Styles::styled()
    .header(AnsiColor::Green.on_default().bold())
    .usage(AnsiColor::Green.on_default().bold())
    .literal(AnsiColor::Cyan.on_default().bold())
    .placeholder(AnsiColor::Cyan.on_default())
    .error(AnsiColor::Red.on_default().bold())
    .valid(AnsiColor::Cyan.on_default().bold())
    .invalid(AnsiColor::Yellow.on_default().bold());

/// The commands under each heading of the top-level help, in order.
/// Every subcommand appears in exactly one group (a test checks).
pub const GROUPS: &[(&str, &[&str])] = &[
    (
        "Run code",
        &["run", "console", "execute", "kernel", "attach"],
    ),
    ("Notebooks", &["nb", "trust"]),
    ("Servers", &["serve", "lab", "ui", "mcp"]),
    ("Setup", &["kernelspec", "info", "completions"]),
];

/// Examples shown at the end of the top-level help: command, what it does.
const EXAMPLES: &[(&str, &str)] = &[
    (
        "ganymede run -k fake '1 + 1'",
        "run a cell on the built-in kernel",
    ),
    (
        "ganymede execute nb.ipynb --inplace",
        "run a notebook and save its outputs",
    ),
    ("ganymede lab", "serve JupyterLab here and open it"),
    ("ganymede kernelspec list", "list the kernels you can use"),
];

const EXIT_STATUSES: &[(&str, &str)] = &[
    ("0", "success"),
    (
        "1",
        "a cell failed, a kernel died or timed out, or a file or download failed",
    ),
    ("2", "usage error"),
    (
        "128+N",
        "stopped by signal N: 130 Ctrl-C, 143 SIGTERM, 129 SIGHUP",
    ),
];

fn styled(style: &Style, text: &str) -> String {
    format!("{}{text}{}", style.render(), style.render_reset())
}

/// Rows as `  NAME  TEXT`, names styled as literals and padded to `width`.
fn rows(out: &mut String, rows: impl IntoIterator<Item = (String, String)>, width: usize) {
    let literal = STYLES.get_literal();
    for (name, text) in rows {
        let pad = " ".repeat(width.saturating_sub(name.len()));
        out.push_str(&format!("  {}{pad}  {text}\n", styled(literal, &name)));
    }
}

/// The CLI with the grouped top-level help.
pub fn command() -> Command {
    let cmd = crate::Cli::command();
    let header = STYLES.get_header();
    let width = GROUPS
        .iter()
        .flat_map(|(_, names)| names.iter().map(|n| n.len()))
        .max()
        .unwrap_or(0);

    let mut groups = String::new();
    for (heading, names) in GROUPS {
        groups.push_str(&styled(header, &format!("{heading}:")));
        groups.push('\n');
        let about = |name: &str| {
            cmd.find_subcommand(name)
                .and_then(|sc| sc.get_about())
                .map(|a| a.to_string())
                .unwrap_or_default()
        };
        rows(
            &mut groups,
            names.iter().map(|n| (n.to_string(), about(n))),
            width,
        );
        groups.push('\n');
    }

    let mut after = String::new();
    after.push_str(&styled(header, "Examples:"));
    after.push('\n');
    let width = EXAMPLES.iter().map(|(c, _)| c.len()).max().unwrap_or(0);
    rows(
        &mut after,
        EXAMPLES.iter().map(|(c, d)| (c.to_string(), d.to_string())),
        width,
    );
    after.push('\n');
    after.push_str(&styled(header, "Exit status:"));
    after.push('\n');
    let width = EXIT_STATUSES
        .iter()
        .map(|(c, _)| c.len())
        .max()
        .unwrap_or(0);
    rows(
        &mut after,
        EXIT_STATUSES
            .iter()
            .map(|(c, d)| (c.to_string(), d.to_string())),
        width,
    );
    after.push_str(&format!(
        "\nThe kernel {} is built in, for trying things out.\n\
         Run {} for a command's flags and details.",
        styled(STYLES.get_literal(), "fake"),
        styled(STYLES.get_literal(), "ganymede help <COMMAND>"),
    ));

    let template = format!(
        "{{about-with-newline}}\n{{usage-heading}} {{usage}}\n\n{groups}{}\n{{options}}{{after-help}}",
        styled(header, "Options:"),
    );
    cmd.help_template(template).after_help(after)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_command_is_in_one_group() {
        let cmd = crate::Cli::command();
        let mut grouped: Vec<&str> = GROUPS.iter().flat_map(|(_, n)| n.iter().copied()).collect();
        let len = grouped.len();
        grouped.sort_unstable();
        grouped.dedup();
        assert_eq!(grouped.len(), len, "a command is in two groups");
        let mut all: Vec<&str> = cmd
            .get_subcommands()
            .filter(|sc| !sc.is_hide_set())
            .map(|sc| sc.get_name())
            .collect();
        all.sort_unstable();
        assert_eq!(grouped, all);
    }

    /// Each command's summary, as its parent's command list shows it,
    /// fits on one line next to the name.
    fn check_summaries(cmd: &Command, path: &str) {
        for sc in cmd.get_subcommands() {
            let path = format!("{path} {}", sc.get_name());
            let about = sc.get_about().map(|a| a.to_string()).unwrap_or_default();
            assert!(!about.is_empty(), "`{path}` has no summary");
            assert!(about.len() <= 66, "`{path}` summary too long: {about}");
            check_summaries(sc, &path);
        }
    }

    #[test]
    fn summaries_fit_one_line() {
        let cmd = crate::Cli::command();
        check_summaries(&cmd, "ganymede");
        for arg in cmd.get_arguments() {
            let help = arg.get_help().map(|a| a.to_string()).unwrap_or_default();
            assert!(help.len() <= 64, "--{}: {help}", arg.get_id());
        }
    }

    #[test]
    fn help_lists_every_command_and_example() {
        let help = command().render_help().to_string();
        for (_, names) in GROUPS {
            for n in *names {
                assert!(help.contains(&format!("  {n} ")), "{n} missing:\n{help}");
            }
        }
        assert!(help.contains("Examples:"), "{help}");
        assert!(help.contains("Exit status:"), "{help}");
        assert!(help.contains("--kernelspec-root"), "{help}");
    }
}
