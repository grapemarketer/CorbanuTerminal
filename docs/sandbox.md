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

Windows itself creates one such folder: `C:\ProgramData\Microsoft\DeviceSync` is
labelled Low and lets everyone write to it, so non-admin sandbox commands can
create, change and delete files there (#1770; measured on Windows Server
2022). We accept this: it is one Windows folder, and a normal user can't
change its permissions. Don't keep files there that you rely on. Default
sandbox commands can currently write there too, and create files in the
other folders that let everyone do so (such as the machine key folders under
`C:\ProgramData\Microsoft\Crypto`); a change awaiting security sign-off
(#1798) would stop that.

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
  other runs (#1560), and read the other's memory, such as secrets in its
  environment or what you type at its prompts (#1838). The default sandbox
  doesn't prevent borrowing yet either (#1560, #1648), so avoid running
  commands in several projects at once.
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
has ended); a warning follows the command too (or a later command, if it
left processes running). Check its
contents before you choose to load it. Such folders
deeper in your workspace are covered too. One that is new, or was replaced,
when the command ends is treated the same way. A read-only `.codex` or
`.agents` folder your permission profile names that doesn't exist yet (or
one on the way to a read-only folder inside it) is created by Corbanu
Terminal before the command starts, so that command doesn't count it as
planted; another command that was already running still does. Commands
can't change, move or delete the ones that existed when they started, or
the folders above them. A folder the sandbox can't list counts as planted,
and so does one holding a folder whose name isn't valid Unicode (when that is
the workspace itself, its top-level `.codex`/`.agents` count as planted until
that folder is renamed or removed). Folders inside
`.git` and the temporary folder (`%TEMP%`) aren't checked.

**A `.git` created while a command runs.** Git runs a `.git` folder's hooks
and config outside the sandbox whenever you run git there. So if the `.git`
at the top of a folder non-admin sandbox commands may write to appears, or
is replaced, while a command runs, Corbanu Terminal warns you right after
that command ends (for one that leaves processes running, after a later
command once they have ended), warns you again at startup and asks in the TUI whether you
have checked it. Since git runs anywhere, the startup warning and the TUI
prompt cover every folder commands may write to (`%TEMP%` included) and
anything below the working folder, not only the working folder and the
folders above it. Commands can still create a
`.git` (`git init` works). A `.git` that is a link (a junction or symbolic
link) is flagged after every command.

**Repositories deeper inside, and bare ones.** The same warnings cover the
repositories deeper in a folder non-admin commands may write to (a nested
`.git` folder, file or link, and a folder that is itself a bare
repository), and a writable folder that is itself a bare repository. One is
flagged when it is new or replaced, or when what git would run from it
changes: its `config`, `config.worktree`, `commondir` or `gitdir` file, its
`hooks`, rebase or sequencer steps, legacy `remotes`/`branches`
definitions, a link or hard link among those files, at the top of its git
folder or in its `logs`, or the same in its submodules (`modules`), its
worktrees and the folders a `gitdir:` line or `commondir` names inside the
writable folders. Files are compared
by content, so rewriting one with the same content isn't a change. A new
repository with exactly the shape `git init` or `git clone` leave (only
the config keys they write and no `ext::` URL, only `*.sample` hooks, no
links, no other repositories inside) isn't flagged, so cloning a project
to look at it doesn't ask; a later change to it is, and so is a repository
that replaced another. A repository reached through a folder link is
checked too: one that was there when the command started and was moved
elsewhere and linked back, one directly behind a link, and one deeper behind
a link into a folder the walk doesn't cover (`%TEMP%`). A folder too large
to look through (over 20,000 entries, more than 2,000 or six levels of
nested repositories, or a link into `%TEMP%` with more than 20,000 entries
behind it), or one holding an entry whose name isn't valid Unicode, counts
as changed after every command. The warning names what
changed. Nested repositories in the temporary folder (`%TEMP%`) aren't
checked, and a nested `.git` link is flagged after every command, as at the
top.

**Hooks folders and files a git config names.** Many projects point
`core.hooksPath` at a folder in the project (Husky's `.husky` or
`.husky\_`, a team `.githooks`). The repository's config may be protected,
but that folder is ordinary project content, so a command could add a hook
there for your next `git commit`. The non-admin sandbox reads the configs
git would read for each repository it watches (the top-level `.git`
included, plus the global and system git configs in their usual places and
the files their `include` lines name; a repository the writable folder is
inside of isn't read) and records, by content, each such hooks
folder, each included config file, and each file a program setting names
(such as `core.fsmonitor`, a credential helper or an alias set to a script
in the project: a first word that looks like a path, and every later word),
when it lies in a folder commands may write to. A path a config or a
`.git` file names on a network share or a device (`\\server\share`,
`\\?\`, `CON` ...) is never opened (a link in the project is still
followed; a device name written with a superscript digit, such as `COM¹`,
isn't recognized as one). For the same reason, in a writable folder that is
itself on a network share or written with a `\\?\` prefix, the config of
a repository whose git folder lies there isn't read and paths relative to
that folder aren't recorded, and no warning says so; only a path a config
names by drive letter, or through a link, is. A global or system
git config that lies in such a folder is recorded too. A new, changed or
removed file there is flagged like a changed repository, and so is one
that appears where a config points (a new repository's `.githooks`, say).
For Husky, the files named after git hooks in the folder above `_` count,
and so does the `_` folder below any hooks folder. Rewriting a file with
the same content (as `npm install` does when it reruns Husky) isn't a
change. A hooks folder with more than 1,024 entries, or configs too large to
read (over 1 MB in one file, 100,000 entries, 4,096 files, more than
eight `core.worktree` values or 4,096 named paths), count as changed after
every command. Other scripts those hooks call are ordinary project files
and aren't checked. That includes `node_modules\.bin`: Husky v9 puts it
first on `PATH` for its hooks, so a hook that runs `npx …` runs whatever is
there. Husky's own startup files (`init.sh` in its config folder, v8's
`~/.huskyrc`) aren't checked either, and nor are program settings outside
the list the sandbox knows.

**The default (elevated) Windows sandbox** keeps the same records and
shows the same warnings (D425, #1846), with three differences. They are
written when the command itself ends: Corbanu Terminal can't see the
processes a command leaves running there, so what those change later isn't
flagged. Nested `.codex`/`.agents` folders that already exist get no extra
protection: a command can change what is inside them unflagged; only moving
or replacing one is flagged. And the records themselves (in
`%LOCALAPPDATA%\CorbanuTerminalSandbox`) aren't protected from a command
whose writable folder contains them, such as your whole user folder.
Its commands run as their own Windows account, so what they create is owned
by that account, and git refuses such a repository ("dubious ownership")
unless a `safe.directory` setting trusts it. Corbanu Terminal flags these
anyway: many git setups trust every folder (`safe.directory = *`), and a
change inside one of your own repositories (a hook, `core.hooksPath`) is
never refused.

**Bare repositories a command plants.** Git also uses a folder that holds
`HEAD`, `objects` and `refs` as a repository (a bare repository), and reads
its config and hooks. A sandboxed command can make one in any folder it may
write to, and git run there or in a folder below it later runs whatever
that config names. Both Windows sandboxes check for these (see above);
macOS and Linux don't (#1775, #1846).
We recommend making git refuse a bare repository it finds by searching:

```shell
git config --global safe.bareRepository explicit
```

Use git 2.45 or later: 2.38 to 2.44 also refuse git run inside `.git`,
which breaks some hooks and submodule tools, and versions before 2.38 ignore
the setting. It covers the git command line
with your global config, not tools built on libgit2 or JGit, nor a git with
another home folder (such as WSL's). Where you do work in a bare repository,
name it with `--git-dir` or `GIT_DIR`.

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
  isn't a git repository yet creates a `.git` there that holds only an empty
  `hooks` folder. When that `.git` is at the top of a folder commands may
  write to (other than `%TEMP%`), both sandboxes protect the whole of it at
  once, starting with the command that made it (#1758); a deeper one, such
  as `sub\.git` for `sub\.git\hooks`, has only `hooks` protected. Git
  doesn't treat such a `.git` as a repository, so git commands in that
  folder report "not a git repository" or use a repository in a folder
  above it. Run `git init` there yourself, or remove the entry and then
  delete that `.git` folder while no session is running.

**A writable folder inside a read-only one.** Neither Windows sandbox can
enforce a profile that makes a folder writable inside a read-only one
(the protected `.git`, `.codex` and `.agents` folders count as read-only), so
both refuse it, and the message names both folders. Folders are compared as
spelled and by their real path, so a junction, folder symbolic link or
`subst` drive doesn't hide this (#1801). Two cases behave differently:

- The same folder marked read-only under one spelling and writable under
  another (for example by its real path and through a junction) isn't
  refused. In the non-admin sandbox it is read-only (#1780).
- A protected `.git`, `.codex` or `.agents` folder that is itself a link to a
  folder holding another writable folder of the profile is refused, because
  that writable folder then lies inside a protected one. Replace the link
  with a folder, or drop that writable folder from the profile. If this
  refuses a setup you rely on, please report it.

**Files a profile protects from reading.** The non-admin sandbox can't stop a
command from reading a file. So if your permission profile denies reading some
files (`deny` entries in your config, or the protected paths a security level
adds), Corbanu Terminal does not run the agent's commands in the non-admin
sandbox. The command fails with a message that it was not run because this
Windows sandbox mode can't block reading the protected files. The same applies
to a profile that limits which folders commands may read. Switch to the
default sandbox, as below, to run it. Profiles without such entries are not
affected.

**Known limitations: tools that use named pipes.** (#1797) A command in the
non-admin sandbox can't open a named pipe it created for writing when the pipe
has Windows' default permissions. By default Windows lets only the pipe's
owner, SYSTEM and Administrators write to a pipe, and the non-admin sandbox's
write restriction doesn't count the command as the owner. So opening such a
pipe for writing, as most programs do to connect to it, fails with "access
denied" (Windows error 5). These fail there:

- Node.js starting a child process with piped input or output (`execSync`,
  `spawnSync`): `EPERM`. A child started with `stdio: 'inherit'` or
  `'ignore'` works.
- Git Bash (`bash.exe`) crashes on start with
  `couldn't create signal pipe, Win32 error 5`.
- A local `git clone` (from a folder on your computer) fails with the same
  crash, from Git's own `sh.exe`.
- Python `multiprocessing` (for example `multiprocessing.Pool`):
  `PermissionError: [WinError 5] Access is denied`. `subprocess` with pipes
  works.
- Other programs that create and then use their own named pipe, such as .NET's
  `NamedPipeClientStream` in PowerShell (`Access to the path is denied.`).

The default sandbox runs all of these. If you need them, switch to the default
sandbox as below. This was measured on Windows Server 2022, in both an
administrator and a normal session.

**Default sandbox: commands can read other projects' running commands.**
(#1803) A default sandbox command can read the memory of another default
sandbox command running at the same time in a different project, such as
secrets in its environment and what it prints. For a command run in a
terminal, it can also read that command's console host, which holds what you
type at its prompts and what it prints. It can't change them or start
programs through them. A change awaiting security sign-off (#1800), which
starts each command on a logon of its own, stops the memory read; a
follow-up built on it, with a test that checks it, will stop the console
read. Until then, avoid running commands in several projects at once.

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
