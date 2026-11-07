## Sandbox & approvals

For information about Codex sandboxing and approvals, see [this documentation](https://developers.openai.com/codex/security).

## Windows non-admin sandbox

On Windows, Corbanu Terminal can sandbox commands in two ways. The **default
sandbox** needs a one-time Administrator setup. The **non-admin sandbox**
(also called the unelevated or legacy sandbox) needs no setup, but it protects
less.

**What the non-admin sandbox stops.** Sandboxed commands run at Low
integrity. In both Read Only and workspace-write modes they can't create,
change, delete or move files and folders outside the folders they may write
to, except in places marked low-integrity: `AppData\LocalLow`, anything you
or a program labelled Low, and folders another non-admin sandbox session is
using at that moment.

**What it does not stop.**

- Reading your files (see "Files a profile protects from reading" below).
- Network access by programs that ignore proxy settings: the network block
  only sets proxy variables and offline settings for common tools.
- A few ways of starting programs that leave the command's job, so cancelling
  doesn't end them (see #1507 in `codex-rs/core/README.md`; measured on a
  server without a desktop session, not yet on a desktop).
- Two commands running at the same time in different projects (or with
  different extra folders they may write to) aren't kept apart: one can
  borrow the other's access and write in that project's folders while the
  other runs (#1560). The default sandbox doesn't prevent this yet either
  (#1560, #1648), so avoid running commands in several projects at once.
  In the default sandbox, a command that races another command's start-up
  can also run code with the sandbox account's full access, outside its own
  restrictions (#1721).
- Drives without Windows permissions, such as FAT or exFAT USB drives: neither
  sandbox can protect files there.

The default sandbox runs commands as a separate account with a firewall-based
network block, and protects more.

**When you will see a warning.** If your sandboxed commands run in the
non-admin sandbox, Corbanu Terminal tells you once per session:

- in the TUI, as a warning at startup and after you choose the non-admin
  sandbox during setup; the setup screens and `/permissions` also describe the
  risk;
- in `corbanu exec`, as a `warning:` line on stderr;
- in `corbanu sandbox`, as a `warning:` line on stderr before the command runs;
- in app-server clients, as a configuration warning.

The warning never blocks a command.

**Project folders created while a command runs.** If the `.codex` or
`.agents` folder at the top of your workspace (or of another folder commands
may write to) doesn't exist when a non-admin sandbox command starts and
exists when it ends, a command may have planted it. Corbanu Terminal then
doesn't load its config, hooks or skills, warns you at startup, and asks in
the TUI whether to load it (once the command, and anything it left running,
has ended). Check its contents before you choose to load it. Such folders
deeper in your workspace are covered too. One that is new, or was replaced,
when the command ends is treated the same way. A read-only `.codex` or
`.agents` folder your permission profile names that doesn't exist yet (or
one on the way to a read-only folder inside it) is created by Corbanu
Terminal before the command starts, so that command doesn't count it as
planted; another command that was already running still does. Commands
can't change, move or delete the ones that existed when they started, or
the folders above them. A folder the sandbox can't list counts as planted. Folders inside
`.git` and the temporary folder (`%TEMP%`) aren't checked.

**Read-only paths that don't exist yet.** If your permission profile marks a
path inside a writable folder read-only and that path doesn't exist when a
sandboxed command starts, Corbanu Terminal creates it as an empty folder and
protects it, so the command can't create it and fill it. Both Windows
sandboxes do this. It has two side effects:

- A path meant to be a file, such as `.env`, becomes an empty folder wherever
  it's missing, and tools that expect a file there may fail to read it. To
  keep a file there, create the file before you start a session. If the
  folder has already been made, delete it while no session is running, then
  create the file.
- A read-only path inside `.git`, such as `.git\hooks`, in a folder that
  isn't a git repository yet creates a protected `.git` there that holds only
  an empty `hooks` folder. Git doesn't treat that as a repository, so git
  commands in that folder report "not a git repository" or use a repository
  in a folder above it. Run `git init` there yourself, or remove the entry
  and then delete that `.git` folder while no session is running.

**Files a profile protects from reading.** The non-admin sandbox can't stop a
command from reading a file. So if your permission profile denies reading some
files (`deny` entries in your config, or the protected paths a security level
adds), Corbanu Terminal does not run the agent's commands in the non-admin
sandbox. The command fails with a message that it was not run because this
Windows sandbox mode can't block reading the protected files. The same applies
to a profile that limits which folders commands may read. Switch to the
default sandbox, as below, to run it. Profiles without such entries are not
affected.

**How to switch to the default sandbox.**

- In the TUI, run `/setup-default-sandbox` and approve the Administrator prompt.
  Corbanu Terminal saves the choice for later sessions.
- Or set the mode in `config.toml` in your Corbanu home folder:

  ```toml
  [windows]
  sandbox = "elevated"
  ```

  You can also pass `-c windows.sandbox=elevated` for a single run. If setup
  has not run yet, the first sandboxed command asks for Administrator approval
  once.

**Several sessions at once.** The default sandbox's read protections are
shared by every Corbanu Terminal session on the computer, whatever its Corbanu
home folder. When you remove a `deny` entry, its protection stays while any
other session that may rely on it is still running, and goes with the first
command you run after they have all ended. To track this, each session that
protects files registers itself in `C:\ProgramData\CorbanuTerminalSandbox`
while it runs. The sandbox setup creates that folder, owned by Administrators,
so updating to this version asks for Administrator approval once more. Until
the setup has run, and while that folder is missing or owned by anyone else,
no protection is removed; if that happens after the setup has run, sandboxed
commands with the secretless launch on are refused until you run the setup
again (it repairs the folder). Sessions started by an
older version are not registered, so their protections aren't kept for them.

**Over SSH.** The default sandbox also works when you start Corbanu Terminal
in a Windows OpenSSH session (or from a service). There, Corbanu Terminal
gives the command runner's logon (the SSH session's own, not the sandbox
accounts every Windows user's sandbox shares) the least access it needs in the
session's window station and desktop (no hooks, windows or clipboard). Once
the runner has started its command, that access shrinks to what the command
and its children need to start: they can't create desktops there, or open
your session's desktop unless `windows.sandbox_private_desktop = false`. It
goes when the runner exits, even if Corbanu Terminal has exited by then, so a
background process left running after its command ends can't start new
programs that need a window station. (If Corbanu Terminal is ended in the
moment between starting a runner and handing its access to the small helper
process that removes it, or that helper can't start and Corbanu Terminal
exits before the runner, that access stays until the SSH session ends.) The
non-admin sandbox with `windows.sandbox_private_desktop = false` works over
SSH too. Earlier builds couldn't run any command there: each one failed after
15 seconds with `windows sandbox: timed out after 15000ms connecting runner
pipe-in`, and the System event log showed `codex-command-runner ...
(0xc0000142)`. If you see that with an older build, update, or start Corbanu
Terminal from the console or Remote Desktop session instead.

Your organization's managed requirements may allow only the non-admin sandbox.
In that case the warning still appears so you know the limits.

This page documents the shipped capability "existing general sandboxing" from
the [product specification](corbanu-product-spec.md) heading "Shipping MVP —
LIVE": "The shipping MVP already has a wallet, vault, scoped signing, approvals,
and general workspace sandboxing."
