# Aggressive security level: protected files

`/security` → Aggressive runs every agent command in a sandbox that writes only
in the current folder. This page explains the file protections that people ask
about most, and their limits. `/security` shows the same rules before you
confirm the level, and the inspector shows what is in force. When the image
tool is refused a protected file, Corbanu explains it on screen, as for a
refused command, and lists it in the inspector (#1087).

## Credential files

Agent commands can't read `.env`, `.env.*` and `.envrc` files (any letter
case), also ones created after the session started, and no grant lifts this.
To allow it, switch to Permissive.

**Templates are exempt.** `.env.example`, `.env.sample` and `.env.template`
(any letter case) hold no credentials, and checkouts, installs and scaffolds
write them, so agent commands can read, create and change them on every
system. A name such as `.env.example.local` is still a credential file.
Because an agent can make a template, don't rename one into `.env` when
you fill in real values: copy it instead (`cp .env.example .env`). A
template an agent made can be a hard link to a file it reads, and on Linux
and Windows also a link; on macOS an agent command can't create one as a
symbolic link or pipe. A `.env` that is a link or has other names is refused,
as below.

A deny rule of your own for `**/.env.*` in a permission profile still covers
the templates. Only a profile that denies all three of `**/.env`,
`**/.env.*` and `**/.envrc` for a folder, as Aggressive does, leaves them out.

How each system protects files created later:

- **macOS:** agent commands can't read, create, link, rename to or overwrite a
  credential file anywhere. Folders with these names (a `.env` virtualenv) still
  work.
- **Linux:** each command hides the credential files that exist when it
  starts, and a command is stopped when one appears while it runs. Limits:
  - A process that is already waiting for the file can read it in the moment
    between the file appearing and the stop.
  - Only the current folder is covered.
  - Folders on network or FUSE filesystems refuse the command.
- **Windows** (the elevated sandbox; the non-admin sandbox refuses
  these profiles): each command is denied the credential files that exist
  when it starts, and a command is stopped when one appears while it runs
  (also a file saved over a denied one, which gets a new ACL). The same limits
  as on Linux apply: a process already waiting for the file can read it in the
  moment before the stop, and only the current folder is covered. A
  credential file that appears just as a command starts refuses that command;
  run it again. A current folder that is a drive root or on a network or WSL
  share, or holds more than a million files, refuses commands.

## A credential file that is a link

A `.env` (or other credential name) that is a link or a pipe, or on macOS
and Linux a file with other hard links, can't be protected: what you write
to it lands somewhere an agent command could read under another name.
Aggressive refuses to start while the current folder holds one, and on
Linux every command is refused while it exists. The message names the file
and where the link points.

To fix a link, replace it with a regular file (delete the link, then create
the file in its place), or move it out of the workspace. What was written to
it is in the file it pointed to: unless that is a credential file too, agent
commands may have read it, so delete that file too (moving it does not hide it)
and change any secret in it.

For a file with other hard links, delete the other names (`find` with
`-samefile` lists them) or replace the file with a regular copy, and change
any secret in it.

## Grant once

Under Aggressive an approval never lifts the write protections. "Grant once"
lets one command write the files that run code later, which Aggressive
otherwise keeps read-only, and only inside the current folder (the only place
Aggressive lets commands write):

- git hooks and config kept outside a `.git` folder: a hooks folder set with
  `core.hooksPath`, such as Husky's `.husky`, or a git folder with another
  name (`--separate-git-dir`, a `.bare` layout);
- when the current folder holds your home folder: your shell and git start-up
  files (such as `.zshrc`, `.profile`, `.gitconfig`, `.config/git` and
  `.local/bin`), login agents and services (`Library/LaunchAgents`,
  `.config/systemd`, `.config/autostart`) and `.claude` settings.

A file written this way runs code later, outside the sandbox. Nothing else
changes: credential files and Corbanu's sign-in, vault, config and history stay
unreadable; Corbanu's own files and the project's `.git` folder (its hooks and config
included), `.codex` and `.agents` stay read-only; other folders and the
network stay closed (with a managed network proxy, see
[its limit](#a-managed-network-proxy)).

**When it is not offered.** The approval prompt leaves Grant once out, and
says why, when it could not help: nothing the command writes is something
Grant once lifts, and the command:

- writes outside the current folder, including your own config files
  (`~/.gitconfig` from `git config --global`, `~/.npmrc` from `npm config set`,
  your pip config from `pip config set`, `gh config set`);
- writes the project's `.git`, `.codex` or `.agents`, or Corbanu's files;
- reads a protected file, or uses the macOS keychain (below);
- or shows every file it writes in its text (a plain read, a wait such as
  `sleep`, a `sed` that only prints lines, a new link in the current folder).
  Child agents' prompts follow the same rules.

Corbanu decides this from the command's text. When it can't tell what a
program writes (a script, `npm install`), Grant once is still offered, unless
the command also writes a start-up file outside the current folder, the
project's `.codex` or `.agents`, or Corbanu's files. The same reasons are
given where such a command fails.

## The macOS keychain

On macOS the sandbox doesn't let agent commands reach the keychain services.
If you approve `security find-generic-password` or another keychain command,
it runs but can't read, add or change any keychain item: it reports the item
as not found, or an error, and Corbanu says that Aggressive caused it. No
grant lifts this, and `~/Library/Keychains` is unreadable too. To allow it,
switch to Permissive.

This holds in every case, also when your organization's requirements route
agent commands through a network proxy or allow a Unix socket: Aggressive's
sandbox denies the keychain services (`com.apple.SecurityServer`,
`com.apple.securityd.xpc` and `com.apple.securityd.systemkeychain`) after
all its other rules (#758). Contained Claude panes get the same rule under
Aggressive. Certificate checks for secure connections use a different
service (`trustd`) and keep working.

## User namespaces on Linux

On Linux agent commands can't create user namespaces under Aggressive and
Moderate (#826): `unshare` and `clone` with a new user namespace fail with
"Operation not permitted", and `clone3` reports itself as unavailable, so
programs fall back to `clone` and keep working. In a user namespace of its
own a command is root over its own files: it could open the placeholders the
sandbox hides the vault store and other denied files with, and mount over
paths, so a nested `corbanu exec` wouldn't be recognised as nested. The rule
covers every process in Corbanu's Linux sandbox, bubblewrap's own included.
No grant or approval lifts it under Aggressive, or where the launch enforces
Aggressive; under Moderate a person can allow it for one command whose
request to run outside the sandbox they approve, with a warning (D27, see
[Moderate](security-moderate.md#limits)). Permissive is unchanged. Contained Claude
panes get the rule too (#891). Commands that run without Corbanu's sandbox
(an external sandbox) and a remote Linux executor used from macOS or Windows
aren't covered.

What needs a user namespace fails: a sandbox of its own (the commands of a
nested `corbanu exec`, which Corbanu explains), rootless containers (Podman,
Buildah, rootless Docker), `bwrap`, Flatpak, Nix's build sandbox, `unshare`,
and Chrome or Electron with their sandbox on (Puppeteer's default; pass
`--no-sandbox`, as Playwright does). 32-bit programs stop too. In a
contained Claude pane, Claude Code's own Bash sandbox (bubblewrap) can't
start (bubblewrap reports "No permissions to create new namespace");
depending on its version and settings, Claude Code then fails the command
or offers to run it outside its own sandbox, which, like every pane
command, asks you first and still runs in Corbanu's sandbox. Choose
Permissive to run them.

Such a failure is named, not left as a bare "Operation not permitted": the
command's output and a warning in your transcript name the rule and the
level, the `/security` inspector lists "creating a user namespace" under
recent denials, and the approval prompt for a command that runs `unshare`,
`bwrap`, Podman, Buildah or Flatpak says it will fail. A contained Claude
pane ends such a turn with a note naming the rule. The explanation only
reads the command's output; it never changes what runs.

## A managed network proxy

Aggressive turns the network off for agent commands, with one known limit:
when your organization's requirements enforce a managed network proxy
(`experimental_network`), agent commands can still connect to that proxy.
That is how network access works in that setup, so what they reach through
it is decided by your organization's proxy rules, not by Aggressive.

Hosts you allowed for good ("always allow") while a less strict level was
on aren't added to the proxy's allowlist under Aggressive, from the next
start (like the rest of the network rules); they apply again at a less
strict level. A host you allow for good while Aggressive is on applies under
it too (#1001, like remembered command approvals, #626).

- **macOS:** Unix sockets those requirements allow stay open. When they
  also allow local binding, agent commands can also accept and make local
  connections (any local port), and connect to port 53 (DNS) on any server,
  outside the proxy.
- **Linux:** agent commands run in their own network, which reaches only
  the proxy; local binding gives them no access to the computer's own ports.
- **Windows:** when those requirements also allow local binding, agent
  commands can also connect to any local port, outside the proxy.

The macOS keychain stays closed either way (above, #758).

## A separate database folder

Under Moderate and Aggressive, agent commands can't use Corbanu's database folder
when you move it out of the Corbanu home (`sqlite_home` or `CODEX_SQLITE_HOME`).
The whole folder is hidden from them, and so is `<folder>.db-backups` beside it.
If the folder holds the current folder or the Corbanu home, only Corbanu's own
entries in it are hidden.

**On macOS, shared folders are handled the same way.** These folders are shared by
many programs:

- `/tmp` and `/var/tmp`, also as `/private/tmp` and `/private/var/tmp`;
- the temporary folder (`TMPDIR`);
- your home folder itself.

If the database folder is one of these, only Corbanu's own entries are hidden, so
other commands can still use the folder:

- the databases, and any `*.sqlite*` file created later there (so other programs'
  databases at the top of that folder are unreadable to commands too);
- Corbanu's own database names with their journal and `-wal`/`-shm` files, which
  commands can't create either, in any letter case;
- the database init lock (`pfterminal_db_init.lock`) and the GPU lock
  (`gpu-controller.lock`);
- the `accounting-pending` and `db-backups` folders.

These stay protected even before they exist. A folder inside a shared one, such as
`/tmp/corbanu-db`, is still hidden whole.

**On Linux and Windows, a shared folder is still hidden whole.** Their sandboxes
can't stop a command from creating a file under a database's name, so hiding entries
one by one would be weaker there. Commands that need the folder fail under Moderate and
Aggressive.

Don't keep the databases in `/tmp` or `/var/tmp`: other people on the machine can
create files there too.

## Commands run outside the sandbox

Under Moderate no agent command runs outside the sandbox, even when you
approve it; Aggressive keeps every command in its sandbox too, once it applies
after a restart. Before the restart, from when you confirm Aggressive in
`/security`, the same holds when the untrusted-content labels are on and the
session uses a sandbox Corbanu applies (not an external one): Core's
protected-path rules then deny the vault store and sign-in file to every
command and keep each run in its sandbox. The inspector's "This session" rows
say which applies. The Aggressive review, the saved screen and the level list
say the same (#816): what holds from confirmation (approved commands kept in
the session's sandbox, network off when its sandbox has no network and its
permission-request tools are off, and any of Aggressive's environment, web
search and model-key settings your own config already has), and that the rest
starts at the restart. That includes the project's credential files (`.env`,
`.env.*` and `.envrc`): Aggressive's sandbox denies them, so before the
restart agent commands can still read them (#846); Core's rules from
confirmation cover the credential files in your home folder.

Under Moderate a command can still ask to run outside it
(an escalation request): the prompt says it stays in the sandbox, and
approving it only lifts Moderate's protected-file rules (credential and
Corbanu files, files that run code later) for that one command. A write
outside your writable folders, or network your sandbox blocks, still fails,
and "retry without sandbox?" is not offered. Commands you allowed with "don't
ask again" skip the prompt but stay in the sandbox too, with those rules
kept.

The Corbanu home's runtime files (the sandbox launchers in `tmp`, shell
snapshots, skills, plugins, packages and `AGENTS.md`) stay read-only to agent
commands, even after a person approves a command. If your own settings turn
the sandbox off (full access, or an external sandbox), commands that
Moderate's rules don't cover run without one; choose a sandbox in your
settings to keep every command in it. Where the sandbox can't start (for
example Linux without user namespaces), commands fail under Moderate instead
of running outside it.

## Agents after untrusted content

Aggressive checks the agent tools the same way as Moderate (D320, D335; see
[Moderate](security-moderate.md)): after untrusted content, starting an agent
or restarting a finished one asks for your approval (refused with approvals
off), and waiting on one, closing it or sending it a message (`send_input`,
`send_message`) goes ahead.

## Remembered approvals

Approvals you saved for good under a less strict level don't apply under
Aggressive: you're asked instead, and the prompt says why. They're kept and
apply again at a less strict level. This covers "don't ask again" command
rules (#626) and MCP tools set to always be allowed, by "Allow and don't ask
me again" or in a config file, an administrator's included (#1001): such a
tool is decided as if it had no "always allow" setting (by you, or by your
approvals reviewer or hooks, as usual). One you choose while Aggressive is on applies
under it too; Corbanu records MCP ones in `mcp-approvals-aggressive.json` in
its home, for connectors and for servers defined only in your own config.
For any other server (one a project or plugin defines or changes), "always
allow" chosen under Aggressive lasts only for that session while Aggressive
is on, and the prompt says so (#1057).

## Other Corbanu Terminals on the same home

When you confirm Aggressive, other Corbanu Terminals running on the same
Corbanu home move up to it within about a second (#919), as they do with
the kill switch and Moderate. Each says so in its history. Other Corbanu
processes on the home, such as a running `corbanu exec`, move up too. From
then on, these apply there:

- Core's protections;
- approval for every command, from each pane's next turn;
- the end of its grants and "for session" approvals;
- for a command or file edit whose approval prompt is still open, or that is
  still being prepared: the answer isn't used and nothing runs; the agent can
  ask again under Aggressive (#1001, #1057). An MCP tool call whose prompt is
  still open is decided again under Aggressive instead (#1001).

A running `corbanu exec` can't ask you for approval, so from then on it
refuses every command that isn't known read-only.

The rest of Aggressive starts in each of them when it restarts: its
sandbox, `.env` protection, network off and the model-key rules. Until then
it is shown at its own level. This only ever raises a level: a lower level
saved in another window applies only after a restart.

The other sessions of the window you confirm in take Core's protections and
approval for every command at once, and so do sessions you start there
before you restart, also when you confirm before the window's own session
has started (#978); that session then starts with them too (#1015). A window
on a remote app server is not covered: its sessions run in the server's
process, which, if it runs on this Corbanu home, takes the saved level as
another Corbanu Terminal would. The level picker in `/security` says so in
such a window and doesn't offer "restart now": Corbanu Terminal starts against
such a server only under Permissive, so start it locally to use the level
(#1047).
