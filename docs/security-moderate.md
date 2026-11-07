# Moderate security level

`/security` → Moderate protects you from instructions hidden in files, tool
output and web or MCP content, and keeps secrets away from agent commands,
while normal work continues. It keeps your own approval, sandbox, network and
web search settings and adds the protections below. The review in
`/security` lists the same rows before you confirm, and the inspector (`i`)
shows what is in force: its recent denials include what Moderate's sandbox
refused (and a protected file the image tool was refused, which a warning
on screen also explains, as for a command, #1087), and a model request of
the session (or one of its agents) that the credential broker refused, for
any reason, shows the broker as degraded until a later request of the same
session goes through. A refused request outside any session (the model
list, the wallet) shows in every session's inspector.

Moderate needs the untrusted-content labels (the `source_envelopes` feature):
without them Corbanu refuses every model request under Moderate, so
`/security` doesn't save Moderate until the feature is on.

## What Moderate adds

- **Commands stay in the sandbox.** No agent command runs outside your
  sandbox, even when you approve a request to run outside it. "Retry without
  sandbox?" isn't offered. Commands you allowed with "don't ask again"
  (also under Permissive) skip the prompt but stay in the sandbox, with the
  protected-file rules below kept, also when they ask to run outside it.
  After untrusted content they don't count for protected actions.
  "Revoke all active authority" (`/security` → `g`) doesn't suspend them.
- **Protected files.** Agent commands can't read the credential files in your
  home folder (such as `.ssh`, `.aws`, `.gnupg`, `.kube`, `.docker`, `.netrc`,
  `.npmrc` and `~/Library/Keychains`) or Corbanu's sign-in, vault, config,
  history and state. They can't write files that run code later: your shell
  and git start-up files, git hooks and config, the project's `.codex` and
  `.agents`, and Corbanu's own files. Your fresh approval of one command
  lifts this for that command, except for Corbanu's runtime files, its
  security state and keys (such as `security_state.json` and
  `source-origin.key`) and what `secretless_agent_launch` keeps (such as
  `~/.aws/credentials` and `~/.netrc`; see below); a remembered
  or automatic approval never does. A project's `.env` files aren't covered;
  Aggressive covers them.
- **Untrusted content.** Tool, MCP, web, agent and memory output reaches the
  model labelled untrusted. After it, protected actions need your fresh
  approval: the vault, credentials (a project's `.env` files included),
  Corbanu's own files and security settings, disclosure, value transfer, code
  you haven't seen, MCP calls that change or send data, and tools Corbanu
  hasn't classified. Where approving can't unlock a file, the prompt names
  it and says what it is: under `secretless_agent_launch`, Corbanu's own
  files (such as its sign-in, vault, config and history), CLI tokens (`~/.aws/credentials`, `~/.netrc`,
  `~/.npmrc`, `~/.git-credentials`, `~/.config/gh/hosts.yml`,
  `~/.docker/config.json`, Claude's sign-in) and, on macOS,
  `~/Library/Keychains` stay closed to agent commands (see
  [Protected launch](config.md#protected-launch-secretless_agent_launch)).
  Cached, remembered or automatic approvals don't count, and with approvals
  off these actions are refused.
- **Model keys.** From the next start, provider API keys are held by the
  credential broker, as under Aggressive. `broker_model_auth = false` in your
  own config keeps it off.
- **Child agents.** Agents started in this Corbanu Terminal (`/spawn` and the
  agent tools) run with Moderate enforced. Claude panes are refused, because
  Claude Code runs outside the sandbox, unless `contained_external_agents` and
  `secretless_agent_launch` run them in it.
- **Nested agents.** `corbanu exec` and `review` started by an agent command
  run with Moderate enforced, also when pointed at another Corbanu home. With
  this Corbanu home, which they can't write inside the sandbox, they run in
  an empty Corbanu home of their own, without your config, sign-in or
  model keys: the agent passes the model and provider with `-m` and `-c`, and
  they reach the model only if agent commands have network access and a key.
  They run headless, nobody approves their commands, and a run without a
  sandbox is refused. Interactive sessions, `app-server`, `mcp-server` and
  credential commands (such as `corbanu vault`) an agent command starts are
  refused.

Not available yet: financial preview and approvals, isolated retrieval,
screened search, brokered browser login, quarantine, the untrusted-content
detectors and Agent Sweep.

## Command output in shell arithmetic

Shell arithmetic runs code hidden in a variable's value (`a[$(cmd)]`), so
after untrusted content a command line that puts command output or input
into arithmetic counts as code you haven't seen and asks first, for example
`v=$(cat notes); echo $((v))` (#937). Corbanu can't follow every way a value
reaches arithmetic, so on a line that holds command output (`$(...)`,
backquotes, `read`) it also asks when arithmetic reads anything not shown
to be a number on that line. These lines ask, although most are harmless:

- variables from the environment or positional parameters:
  `out=$(cmd); echo $((HOME_COUNT + $1))`
- `eval` or `alias` anywhere on the line: `out=$(cmd); eval "echo hi"`
- text operands of a numeric `printf`: `v=$(cmd); printf '%d' "$v"`
- `${!arr[@]}` loops and associative-array literal keys:
  `out=$(cmd); for k in "${!m[@]}"; do echo $k; done`
- a parenthesised group attached to a word with a name or command output in
  it, because zsh reads such a group as glob qualifiers, whose subscripts are
  arithmetic and whose `e` and `+` qualifiers run code (D57). Quoted text
  counts too, since a nested shell may run it:
  `w=$(cmd); print -l /*(.[w])`, `out=$(cmd); grep -E '(foo|bar)' f`,
  `out=$(cmd); git commit -m "fix(core): x"`,
  `out=$(cmd); python3 -c "print('hi')"`, and regular expressions such as
  `'([a-z]+)'`

Counters and plain uses still run without asking:
`out=$(cmd); for ((i=0; i<3; i++)); do echo $i; done`,
`out=$(cmd); n=$(date +%s); echo $((n + 1))`, `out=$(cmd); ls *(/)`,
`out=$(cmd); arr=(1 2); diff <(echo a) <(echo b)`. Splitting such a line
in two (the output on one, the arithmetic or pattern on the next) avoids
the prompt.

## Other code hidden in a command line

After untrusted content these also count as code you haven't seen and ask
first, with or without command output on the line:

- zsh glob qualifiers that run code: `e` and `oe` with code
  (`ls *(e:'rm x':)`) and `+name` (`ls *(+f)`) (#1028). Quoted text that zsh
  would read that way asks too, such as `git commit -m "fix(tests): x"`,
  except on the plain lines described next (#1172, D85). A line that runs as
  `-c` or `-lc` under a system `bash` or `dash` (`/bin/bash`,
  `/usr/bin/bash`, `/bin/dash`, `/usr/bin/dash`, or a `/bin/sh` or
  `/usr/bin/sh` that is one of them, as on macOS) doesn't ask for this when
  it holds only plain commands of these programs: `echo`, `cd`, `pwd`,
  `true`, `false`, `ls`, `cat`, `head`, `tail`, `wc`, `grep`, `mkdir`,
  `touch`, and `git status`, `diff`, `log`, `show`, `add`, `commit`,
  `restore`, `switch`, `checkout`, `branch`, `rm`, `mv` or `stash` (with no
  option before the subcommand, no `--output` and no unquoted `{`, `*`, `?`
  or `[`). Plain means literal words joined by `;`, `&&`, `||`, `|` or a
  newline: no `$(...)`, variables, redirections, groups, assignments, `if`
  or loops, comments, backslashes or control characters (and no non-ASCII
  character right before `|`, a brace, a bracket or `~`). A system `zsh`
  (`/bin/zsh`, `/usr/bin/zsh`, the macOS default) doesn't ask for this
  either when such a line also has no `cd`, no `''` and no unquoted
  parenthesis, glob character, brace, `~` or expansion in any word (#932,
  #1197). So `git commit -m "fix(tests): x"` runs without this question
  under `/bin/bash` and under `/bin/zsh`. It still asks unquoted
  (`git commit -m fix(tests)`), beside a variable or other expansion, under
  a shell named without one of those system paths (such as `zsh`, `bash` or
  Homebrew's `/opt/homebrew/bin/bash`), with any other program on it, when
  typed into a running shell, and for every command when the zsh-fork shell
  is on.
- bash's prompt expansion `${v@P}`, which runs any `$(...)` in the value, as
  `eval` would (#1012). Other ways a shell runs a variable's value
  (`PS4` with `set -x`, zsh's `${(e)v}` and `print -P`, `trap "$v"`) aren't
  covered yet (#1064).
- substitutions (`$(...)`, `<(...)`, `>(...)`) nested more than 32 deep
  (#1020).

## Choosing, restarting and going back

When you confirm Moderate, Core's rules (the sandbox, protected files and
untrusted content) apply at once in every session of this Corbanu Terminal,
and grants and "for session" approvals end. A command, file edit or MCP tool
call whose approval prompt was still open is decided again under the new
level, so you're asked again where it needs approval; an answer to the old
prompt isn't used, and a retry outside the sandbox that was being approved (or
was due) isn't run (#1001). A command or file edit that carried extra
permissions (ones it asked for, or grants made before the change) isn't
decided again: it doesn't run, and the agent can ask again (#1001, #1057).
Other Corbanu Terminals running on the same Corbanu home take them within
about a second and say so, as they do with the kill switch. The credential
broker starts at the next start (in each of them); press `r` to restart right
away. Your `config.toml` isn't changed. If you confirm before the window's own
session has started, that session starts under Core's Moderate rules too
(#1047). In a window whose sessions run on an app server outside Corbanu
Terminal (a remote server or the local daemon), Moderate reaches that server
only if it runs on this Corbanu home, as it reaches another Corbanu Terminal;
Corbanu Terminal starts against such a server only under Permissive, so the
level picker doesn't offer "restart now" there.

Choosing Permissive or Aggressive from Moderate shows what changes first.
Back to Permissive takes effect when you restart; until then Moderate stays
on. Aggressive applies Core's stricter rules at once when its preflight
passes (the `protected_mode_preflight` feature), also in the other Corbanu
Terminals running on the home (#919), and the rest at the restart.
The kill switch (`/security` → `g`) works under every level.

## Limits

- If your own settings turn the sandbox off (full access), commands that the
  protected-file rules don't cover run without one. Choose a sandbox in your
  settings to keep every command in it.
- With full access in your own settings, commands can also write the project's
  `.git`, except its hooks and config (with a sandbox setting `.git` stays
  read-only). A command could leave something there that your own git later
  follows outside the sandbox (#874): a link such as `.git/COMMIT_EDITMSG` to
  a file elsewhere (or to `.git/config`), which your next `git commit` writes;
  a hard link; rebase or cherry-pick steps whose `exec` lines, or a merge
  strategy that isn't git's own, run when you continue; a remote in
  `.git/remotes`; config or a hooks folder where there was none; or a replaced
  `.git`. On macOS and Linux, Corbanu looks at `.git` before a session's first
  such command is decided (and reports links, steps and hard links already
  there, which then hold that command too), after each one, before each later
  command and at the end of each turn, and warns you about anything new, with
  the command that removes it. It removes nothing itself. Below `objects`,
  `lfs` and `annex` it looks only at their own entries: git reads the files
  below and puts new ones in place, it doesn't write through them. Links
  inside a hooks folder are left alone (git runs hooks, it doesn't write
  through them). Until you remove what it found, and while it can't look at
  all of `.git` (a folder it can't read, or too many entries), approving a
  command in that project doesn't lift the protected-file rules (nor run it
  outside the sandbox, nor let it create user namespaces), in any session of
  this Corbanu Terminal, and a note names what it found. (A known tool's
  command approved after untrusted content lifts no rule anyway: its note
  mentions only user namespaces. An additional permission you approve to
  write to a protected file is refused, and its note says so.) If the
  repository's own git folder was unreadable at its first look, config files
  or hooks added before the first complete look may not be reported as new.
  Check that config and those hooks yourself before you run git there. Some
  setups of your own hold them too until you remove them: a layout made of
  links (`git new-workdir`, Android's `repo`), a `config.worktree` that
  `git sparse-checkout` adds, or a hooks folder a hook installer creates
  where there was none. On Linux the sandbox leaves empty
  files and folders in place while a command runs: a new config file or hooks
  folder that is empty is reported (in any session) once something is put in
  it (once this Corbanu Terminal remembers 1024 of them, a new one is
  reported at once instead, one time per session while it stays held, and
  again once filled), and an empty `.git` folder replaced by another empty one isn't reported
  (unless the first empty one was reported as a replacement, or, once more
  than 1024 different `.git` folders have been reported as replacements in
  this Corbanu Terminal, any).
  It doesn't watch in between: a process a command left running can change
  `.git` after the last look, even while an approved command runs (such a
  process doesn't stop an approval from lifting the rules), and what it
  changes after a turn ends is found at the next command, not before you run
  git yourself. It watches the repositories of the folders agent commands run
  in (a command's own working folder, `workdir`, too): not a repository a
  command reaches elsewhere (`git -C`, a `cd` in the command or in a shell it
  left running, a program the zsh-fork shell starts in another folder), not
  one a command creates and fills in one go (nothing was there to compare
  with). A `.git` that was missing at a look and is back as a different one is
  looked at in full: its config, hooks and remotes are reported as new, also
  after `git init` or a fresh clone there (not a worktree git adds again for
  the same repository), and approving a command in that project doesn't lift
  the rules until you remove them or restart Corbanu Terminal; the same one
  moved back (the same inode and, on Linux where the file system records it,
  the same creation time) is compared with the last look (looked at in full
  where it now names other git folders). On Linux a new `.git` file that gets
  a removed one's inode number with the same text counts as moved back where
  the file system doesn't record creation times, or where it was made within a
  few milliseconds of the old one (one tick of the clock that stamps them).
  Where a worktree's or submodule's `.git` is replaced by another kind, or
  pointed at other git folders, check its config and hooks yourself before you
  run git there. New config and hooks are checked only in a repository's own
  git folder, not in its worktrees' or submodules'. A session that works in
  more than 16 repositories forgets the oldest; its next look there reports
  only links, steps and hard links. This Corbanu Terminal also keeps that
  history for at most 64 sessions. When another session needs a look, it
  forgets the least recently used session's repository history; the next
  look in that session has the same first-look limit.
- Corbanu Terminal finds the `.git` files of a repository's linked worktrees
  through the folders under `.git/worktrees`, and keeps protecting those it
  found until it quits. A command that makes one of those folders unreadable
  hides that worktree's `.git` file from the next start, which then doesn't
  protect it. If a command changed permissions under `.git/worktrees`, check
  them before you restart.
- MCP servers, apps, hooks and `!` shell commands run outside the sandbox, as
  under every level.
- On Linux, where a command you approve can write a Corbanu home's folder
  (another Corbanu home such as `~/.codex`, or this one without
  `secretless_agent_launch`), it can create those of Corbanu's security files
  there that don't exist yet (such as `security_state.json` before a level is
  first confirmed); it can't read or change the ones that exist. On macOS it
  can't create them either.
- Nested agents are recognised on macOS and Linux only, and on Linux not when
  they run as root. Corbanu marks
  Moderate by denying agent commands a file in the Corbanu home's `tmp`
  folder, and lists the homes Moderate runs on in the `moderate-homes` folder
  under `~/Library/Application Support/Corbanu` (macOS) or
  `~/.local/state/corbanu` (Linux); under Moderate agent commands can't change
  that list. A launch pointed at another Corbanu home finds its origin through
  that list or your default home (`~/.corbanu`); if the list can't be written
  or your account can't be looked up, such a launch runs at that home's level,
  as before. With a Corbanu home other than the default, a nested launch is
  refused, as before, while `~/.corbanu` or `~/.pfterminal` also stores a
  security level and a vault. A program started outside the sandbox (MCP
  servers, hooks, `!` commands) isn't an agent command and isn't held to
  Moderate. A nested run's own Corbanu home is removed when the run ends, but
  stays in the temporary folder if the run stops on an error (#766).
- On Linux agent commands can't create user namespaces (#826): `unshare` and
  `clone` with a new user namespace fail with "Operation not permitted". In
  one of its own a command could open the files the sandbox hides and mount
  over paths, so a nested `corbanu exec` wouldn't be recognised. This covers
  every process in Corbanu's Linux sandbox and contained Claude panes
  (#891), but not commands that run without it (your settings turn the
  sandbox off, or an external sandbox) or a remote Linux executor used from
  macOS or Windows. What needs a user namespace fails under Moderate and
  Aggressive: a sandbox of its own (so the commands of a nested
  `corbanu exec` don't run; Corbanu says why), rootless containers (Podman,
  Buildah, rootless Docker), `bwrap`, Flatpak, Nix's build sandbox, and Chrome or
  Electron with their sandbox on (Puppeteer's default; pass `--no-sandbox`,
  as Playwright does). 32-bit programs stop too. In a contained Claude pane,
  Claude Code's own Bash sandbox (bubblewrap) can't start (bubblewrap
  reports "No permissions to create new namespace"); depending on its
  version and settings, Claude Code then fails the command or offers to run
  it outside its own sandbox, which, like every pane command, asks you
  first and still runs in Corbanu's sandbox. No approval lifts the rule
  inside a contained Claude pane. Elsewhere, choose Permissive to run them,
  or allow it for one command (next point).
- You can allow user namespaces for one command (D27, Linux, Moderate only).
  When the agent asks to run a command outside the sandbox before untrusted
  content enters the session (every command's output counts, so in practice
  this is the first command of a new session, before the agent has read
  anything from outside), the approval prompt says that approving it
  lifts Moderate's protected-file rules for that run and, with a warning,
  that it also lets that one run create user namespaces: inside one, the
  command can get past the file protections that remain for that run (it is
  root over its own files there and can mount over paths), so a Corbanu it
  starts may not run at Moderate, and more of the kernel is exposed to it.
  Approve only a command you trust. Only your answer to that prompt counts:
  not "for the session" for the next request, a saved "don't ask again"
  rule (the command then stays in the sandbox with the rules kept), a
  permission hook or the automatic reviewer. After untrusted content (D48,
  Linux, Moderate only) it is offered only for a command whose program is a
  known tool: `unshare`, `bwrap`, `podman`, `buildah`, `flatpak` or `nix`.
  The command must be one plain command (directly, or as the whole script
  of a system `bash`, `sh` or `zsh`), optionally after `env`, `sudo` or
  `nohup` (or, first in the script, `exec` or `command`) with simple
  options. Each program it runs (the shell, the wrappers, the tool) must
  come from a system folder (`/usr/bin`, `/bin`, `/usr/sbin`, `/sbin`,
  `/usr/local/bin`, `/run/current-system/sw/bin`,
  `/nix/var/nix/profiles/default/bin`) the agent can't write: named by that
  path, or by name and found there first through the command's `PATH`, with
  no relative folder or folder the agent can write searched before it (a
  remote run's `PATH` isn't checked, so there only the full system path
  counts). Anything else isn't offered and keeps the rule: other programs,
  more than one command (`;`, `&&`, `|`), variables, substitutions,
  redirections, `NAME=value` settings, a copy of the tool or of the shell
  elsewhere (`./podman`, a shell the agent names), other wrappers
  (`timeout`, `bash -c`), and aliases or functions the command defines
  (your own shell's functions and aliases still apply, and so does a
  `PATH` your shell's start-up files or saved snapshot set: Corbanu checks
  the `PATH` it gives the command). You are asked every
  time, by you only, with the same warning: not "for the session", a saved
  rule, a hook or the automatic reviewer, and also where the automatic
  reviewer would otherwise answer, so approving that request allows
  user namespaces for it (decline, and ask the agent to run it without
  leaving the sandbox, to run it without them). Approving lifts nothing
  else: the protected-file rules stay, unless the command is also a
  protected action, whose fresh approval (asked also under a saved rule)
  lifts them for that run as before. If more untrusted content arrives
  before it starts, it isn't allowed. A known tool can itself run any
  program inside the namespace (`unshare -r sh -c '…'`, `podman run`),
  so read the whole command. Nor is it given while Corbanu's check of the
  project's `.git` holds the protected-file rules (see above), even where
  the prompt warned of it. The allowance covers the approved process and
  everything it starts or is given while it runs, also after untrusted
  content arrives: an interactive program such as a shell keeps it for
  every command typed into it later. The run still stays in the sandbox
  (writes outside your writable folders and blocked network still fail).
  Aggressive, and a launch that enforces Aggressive, never allow it.
- A command that fails this way says why instead of a bare "Operation not
  permitted": its output (what the agent sees) and a warning in your
  transcript name the rule and the level and, under Moderate, how to allow
  it for one command (only for a known tool's command, or in a new session,
  since the failed command's output is untrusted content), and the
  `/security` inspector lists "creating a
  user namespace" under recent denials. Before you approve a command that runs
  `unshare`, `bwrap`, Podman, Buildah or Flatpak and stays in the sandbox,
  the approval prompt says it will fail. A contained Claude pane ends such a
  turn with a note naming the rule. The explanation only reads the
  command's output; it never changes what runs.
- A remote app server can't enforce a level chosen here; Corbanu refuses to
  start against one until you choose Permissive.

See [Aggressive security level: protected files](security-aggressive.md) for
the protected-file rules both levels share.

Product requirement: [Product surface](corbanu-product-spec.md#product-surface) —
"Add strong protection without making normal agent work painful."
