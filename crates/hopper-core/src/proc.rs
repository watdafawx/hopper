//! Spawning console tools without a console window flashing up.
//!
//! Hopper is a GUI program, so every child that is a console program (reg,
//! cmd, powershell, git, java -version…) gets its own window unless told
//! not to. Route those spawns through `hidden`.

use std::process::Command;

/// `CREATE_NO_WINDOW`: the child runs without a console window.
pub const CREATE_NO_WINDOW: u32 = 0x0800_0000;

/// Makes `cmd` start without a console window (no-op off Windows).
pub fn hidden(cmd: &mut Command) -> &mut Command {
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        cmd.creation_flags(CREATE_NO_WINDOW);
    }
    cmd
}

/// `Command::new(program)` that starts without a console window.
pub fn command(program: impl AsRef<std::ffi::OsStr>) -> Command {
    let mut cmd = Command::new(program);
    hidden(&mut cmd);
    cmd
}
