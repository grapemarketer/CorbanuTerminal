// D324 (#1721): no console, so Windows starts no console host for the
// runner. That host would start with the logon's default DACL, open to every
// elevated command until it exits. The runner prints nothing; its commands
// get their own console or a ConPTY.
#![cfg_attr(target_os = "windows", windows_subsystem = "windows")]

#[cfg(target_os = "windows")]
mod win;

#[cfg(target_os = "windows")]
fn main() -> anyhow::Result<()> {
    win::main()
}

#[cfg(not(target_os = "windows"))]
fn main() {
    panic!("codex-command-runner is Windows-only");
}
