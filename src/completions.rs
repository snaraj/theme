//! Offline shell definitions. Completion never invokes theme or a network helper.

const COMMANDS: &str = "set random unsplash get list ls search browse surf index preview status update upgrade version rename rm remove completions help";
const SHELLS: &str = "bash zsh fish nushell";
const OPTIONS: &[(&str, &str)] = &[
    (
        "set|random|unsplash",
        "--rotate --extend --desktop-only --help",
    ),
    ("get", "--rotate --extend --mkdir --help"),
    ("list|ls|search", "--verbose -n --all --help"),
    ("preview", "--verbose --wallpaper --help"),
    (
        "browse|surf",
        "--all --favorites --page-size --color --min-contrast --coverage --min-width --min-height --aspect --help",
    ),
    ("update|upgrade", "--version --binary --help"),
];

pub fn run(args: &[String]) {
    if args.is_empty() || args == ["--help"] || args == ["-h"] {
        print!(
            "theme completions <bash|zsh|fish|nushell>\n\nPrint shell completion definitions to stdout; save and source them in your shell.\n"
        );
        return;
    }
    if args.len() != 1 {
        crate::ui::die("usage: theme completions <bash|zsh|fish|nushell>");
    }
    match args[0].as_str() {
        "bash" | "zsh" => bourne(&args[0]),
        "fish" => fish(),
        "nushell" | "nu" => nushell(),
        _ => crate::ui::die("completion shell must be bash, zsh, fish, or nushell"),
    }
}

#[allow(clippy::print_literal)] // Keep shell syntax literal rather than escaping braces twice.
fn bourne(shell: &str) {
    let bash = shell == "bash";
    if bash {
        print!(
            "_theme() {{\n  local cur=${{COMP_WORDS[COMP_CWORD]}} prev=${{COMP_WORDS[COMP_CWORD-1]}} cmd=${{COMP_WORDS[1]}} item\n  local words='{COMMANDS}'\n  if (( COMP_CWORD > 1 )); then\n"
        );
    } else {
        print!(
            "#compdef theme\n_theme() {{\n  local cur=$words[CURRENT] prev=$words[CURRENT-1] cmd=$words[2]\n  local candidates='{COMMANDS}'\n  if (( CURRENT > 2 )); then\n"
        );
    }
    let variable = if bash { "words" } else { "candidates" };
    println!("    case $cmd in");
    for (commands, flags) in OPTIONS {
        println!("      {commands}) {variable}='{flags}' ;;");
    }
    println!(
        "      completions) {variable}='{SHELLS}' ;;\n      help) {variable}='{COMMANDS}' ;;\n      *) {variable}='--help' ;;\n    esac\n    case $prev in\n      --rotate) {variable}='left right' ;;\n    esac\n  fi"
    );
    if bash {
        print!(
            "{}",
            r#"  COMPREPLY=()
  if (( COMP_CWORD > 1 )) && [[ $cur != -* && $cmd != completions && $cmd != help && $prev != --rotate ]]; then
    while IFS= read -r item; do COMPREPLY+=("$item"); done < <(compgen -f -- "$cur")
    compopt -o filenames 2>/dev/null || true
  else
    while IFS= read -r item; do COMPREPLY+=("$item"); done < <(compgen -W "$words" -- "$cur")
  fi
}
complete -F _theme theme
"#
        );
    } else {
        print!(
            "{}",
            r#"  if (( CURRENT > 2 )) && [[ $cur != -* && $cmd != completions && $cmd != help && $prev != --rotate ]]; then
    _files
  else
    compadd -- ${(z)candidates}
  fi
}
compdef _theme theme
"#
        );
    }
}

fn fish() {
    println!("complete -c theme -n '__fish_use_subcommand' -f -a '{COMMANDS}'");
    for (commands, flags) in OPTIONS {
        let commands = commands.replace('|', " ");
        for flag in flags.split_whitespace() {
            let option = if let Some(long) = flag.strip_prefix("--") {
                format!("-l {long}")
            } else {
                format!("-s {}", &flag[1..])
            };
            let takes_value = matches!(
                flag,
                "--rotate"
                    | "--mkdir"
                    | "--wallpaper"
                    | "-n"
                    | "--version"
                    | "--binary"
                    | "--page-size"
                    | "--color"
                    | "--min-contrast"
                    | "--coverage"
                    | "--min-width"
                    | "--min-height"
                    | "--aspect"
            );
            println!(
                "complete -c theme -n '__fish_seen_subcommand_from {commands}' {option}{}",
                if takes_value { " -r" } else { "" }
            );
        }
    }
    println!("complete -c theme -n '__fish_seen_subcommand_from completions' -f -a '{SHELLS}'");
    println!("complete -c theme -n '__fish_seen_subcommand_from help' -f -a '{COMMANDS}'");
    println!(
        "complete -c theme -n '__fish_seen_subcommand_from set random unsplash get' -l rotate -r -f -a 'left right'"
    );
}

fn nushell() {
    println!(
        "def 'theme commands' [] {{ [{}] }}",
        COMMANDS
            .split_whitespace()
            .map(|s| format!("'{s}'"))
            .collect::<Vec<_>>()
            .join(" ")
    );
    println!("def 'theme shells' [] {{ [bash zsh fish nushell] }}");
    println!(
        "export extern theme [command?: string@'theme commands', ...args: string, --help(-h), --version(-V)]"
    );
    println!("export extern 'theme completions' [shell: string@'theme shells', --help(-h)]");
    for (commands, flags) in OPTIONS {
        for command in commands.split('|') {
            println!("export extern 'theme {command}' [\n  ...args: string");
            for flag in flags.split_whitespace() {
                // Nu cannot type an optional flag value: bool rejects
                // --extend=112233, string rejects bare --extend. Leave both
                // forms to the external command's argument parser.
                if flag == "--extend" {
                    continue;
                }
                let kind = match flag {
                    "--rotate" | "--mkdir" | "--wallpaper" | "--version" | "--binary"
                    | "--color" | "--aspect" => ": string",
                    "-n" | "--page-size" | "--min-width" | "--min-height" => ": int",
                    "--min-contrast" | "--coverage" => ": float",
                    _ => "",
                };
                println!("  {flag}{kind}");
            }
            println!("]");
        }
    }
}
