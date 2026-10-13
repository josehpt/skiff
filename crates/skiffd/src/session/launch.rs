use portable_pty::CommandBuilder;
use skiff_core::session::SessionSpec;
use std::path::Path;
use super::{default_shell, SHELL_HANDOFF_TITLE};

/// The command for the PTY, and whether it is the shell itself.
pub(super) fn launch_command(spec: &SessionSpec, command: &str, cwd: &Path, id: &str) -> (CommandBuilder, bool) {
    // A peek tool runs as given and ends with its program: the peek closes then.
    if spec.peek {
        let mut cmd = CommandBuilder::new(command);
        cmd.args(&spec.args);
        cmd.cwd(cwd);
        cmd.env("TERM", "xterm-256color");
        cmd.env("COLORTERM", "truecolor");
        // `sh -c` sets no size, and tools such as delta read it to lay out.
        cmd.env("COLUMNS", spec.cols.to_string());
        cmd.env("LINES", spec.rows.to_string());
        return (cmd, false);
    }
    // An alias from the user's rc files stands for a command line.
    let (program, args) = match skiff_core::alias::expand(command) {
        Some(w) => (w[0].clone(), [&w[1..], &spec.args[..]].concat()),
        None => (command.to_string(), spec.args.clone()),
    };
    // A program that is not the shell hands the terminal to a shell when it
    // exits, so Ctrl+C in an agent leaves a prompt instead of a dead pane.
    // Ctrl+C signals the whole foreground group, the wrapper included; the
    // `trap :` keeps the wrapper alive, and unlike `trap ''` the child does
    // not inherit it, so the agent still gets SIGINT as normal. The title
    // carries the program's exit code, which the hand-off would lose.
    // Keep the daemon's prepared environment. A login shell resets PATH;
    // another Ctrl+C during profile loading can leave agent commands missing.
    let shell = default_shell();
    let is_shell = program == shell
        || std::path::Path::new(&program).file_name() == std::path::Path::new(&shell).file_name();
    let (program, args) = if is_shell {
        (program, args)
    } else {
        let mut wrapped = vec![
            "-c".to_string(),
            format!(
                "trap : INT; \"$@\"; printf '\\033]0;{SHELL_HANDOFF_TITLE}:%s\\007' \"$?\"; exec \"$SKIFF_SHELL\" -i"
            ),
            "skiff".to_string(),
            program,
        ];
        wrapped.extend(args);
        ("/bin/sh".to_string(), wrapped)
    };
    let mut cmd = CommandBuilder::new(&program);
    cmd.args(&args);
    cmd.env("SKIFF_SHELL", &shell);
    cmd.cwd(cwd);
    cmd.env("TERM", "xterm-256color");
    cmd.env("COLORTERM", "truecolor");
    // So the `skiff` CLI in a pane knows its own session and reaches this daemon.
    cmd.env("SKIFF_SESSION", id);
    cmd.env("SKIFF_SOCKET", crate::paths::socket());
    (cmd, is_shell)
}
