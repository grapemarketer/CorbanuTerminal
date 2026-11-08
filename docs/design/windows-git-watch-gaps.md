# Windows non-admin sandbox: `.git` watch gaps beyond D332

Design note for #1775. No code changes. Written 2026-11-07, read from the
code on `main` (after #1760, #1762 and #1777); nothing here was measured on
Windows (the PC is off). "Non-admin" is the legacy, unelevated sandbox;
"elevated" is the default Windows sandbox. A "session" below is what the
code calls one: a single sandboxed command, from start until its last
process exits.

## Why `.git` matters

Git reads a repository's `config` and runs its `hooks` with your full
rights, outside any sandbox, whenever you (or your editor) run git there.
Many config keys run programs, for example `core.fsmonitor` (on
`git status`), `core.pager` (on `git log`), `core.editor`,
`sequence.editor`, `core.sshCommand`, `core.askPass`, `gpg.program`,
`diff.external`, `diff.*.textconv`, `merge.*.driver`, `credential.helper`,
`!` aliases, and filter drivers (with a matching `.gitattributes` line).
Others name more config or hooks: `include.path`, `includeIf.*.path`,
`core.hooksPath`. So a sandboxed command that can plant or edit a git
folder can get code run later, unsandboxed. Editors make this worse: VS
Code's git support, for example, runs git in the background and looks for
repositories in subfolders.

## What D332 does today (#1760)

- Each non-admin session records the `.git` at the top of every writable
  root, `%TEMP%` included (as D262 does), with its file identity. The
  deeper D283 walk for nested `.codex`/`.agents` skips roots that come only
  from `%TEMP%`/`%TMP%`.
- When the session ends (or a sweep finds it crashed), a top-level `.git`
  that is new or replaced is recorded as planted.
- The next time Corbanu Terminal starts, it warns when such a `.git` lies
  between the project root and the working folder (TUI, `exec` and
  app-server), and the TUI asks whether you have checked it ("Keep warning
  me" / "Confirm them"). Nothing is refused: `git init` still works.
- A `.git` that is a link (junction or symbolic link) has no identity, so
  it is recorded again after every command and any confirmation is
  cleared: you are warned and asked at every start, and confirming never
  sticks.
- An existing top-level `.git` is already read-only for sandboxed commands
  (inherited from upstream; for a link the protection lands on its target).
  #1777 (from #1758) also protects, from the first command, a `.git` that
  the elevated sandbox makes on the way to a read-only `.git\hooks`.
- Not covered: everything in the four gaps below. The elevated sandbox
  keeps no planted-folder records at all (as for D262).

## How others treat the same cases

| Case                                  | Upstream Codex (all OSes)                               | Corbanu on macOS/Linux                                                                                                                                                                              | Windows non-admin today |
| ------------------------------------- | ------------------------------------------------------- | --------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- | ----------------------- |
| Nested `.git` below a writable root   | Writable, not watched                                   | Writable at every level; the post-command `.git` check (D6, Moderate/Aggressive) only looks at the repository a folder is in, so a new nested `.git` is left to the command-text classifier         | Writable, not watched   |
| Bare repository at a writable root    | Writable, not watched (only `<root>/.git` is protected) | Same                                                                                                                                                                                                | Not watched             |
| `core.hooksPath` in a writable folder | Writable                                                | Read-only at Moderate and Aggressive (`protected_surface/git_paths.rs`); a person's approval of one command lifts it (Moderate, #540) and so does Aggressive's "grant once"; writable at Permissive | Writable                |
| `.git` that is a link (`repo` tool)   | Protection lands on the target                          | Same                                                                                                                                                                                                | Flagged at every start  |

The non-admin sandbox refuses profiles that deny reads (`docs/sandbox.md`),
and Moderate and Aggressive add read denials. So in practice it only runs
at Permissive, where no platform covers these cases. Every option below is
therefore stricter than any other platform at the same level.

## When and where a warning appears

**When.** Today's records are written when a command ends, but the warning
and the prompt appear only at the next start of Corbanu Terminal. An
editor's background git can run a planted `core.fsmonitor` within seconds,
long before that. So every "flag" option below detects after the fact and
may come too late for editors; only protection (or git's own settings, gap
2 C) closes that window. A short notice right after the command ends, as D6
does on macOS/Linux, would shrink it.

**Where.** The startup warning (`config/src/loader/mod.rs`) only shows
entries from the project root down to the working folder, and the TUI
prompt (`windows_sandbox_prompts.rs`) only entries in the working folder or
a folder above it. That fits `.codex`/`.agents`, which only load
from there, but not git: you may run git anywhere. Already today, a D332
record for a `%TEMP%` root or an extra writable folder is never shown when
you start in the project. Most new flags below (a nested `sub\.git`, a bare
repository in a scratch folder, a hooks folder such as `.husky\_`) would be
recorded but never shown unless this changes, for example to "every
writable root of the session, and anything below the working folder".

Both are new UI and need a decision (decision 6). Until then, the new
checks are only worth shipping together with it.

## Keeping prompts rare (applies to all four gaps)

A warning that comes back at every start teaches people to click "Confirm"
without looking, which also weakens the `.codex`/`.agents` prompts. Rules
for any new check:

1. **Compare the end of a command with its start.** A repository a command
   makes and deletes again (test suites do this) is never shown. Accepted
   limit: a command can also plant a repository, wait for an editor's
   background git to use it, and delete it; only protection or gap 2 C
   closes that.
2. **Compare what git would run, not timestamps.** File identity for the
   `.git` entry, plus a content hash of the small files git reads for
   programs (`config`, `config.worktree`, `commondir`, `hooks\*`, a `.git`
   file's `gitdir:` line), the same files in the folders a `gitdir:` line
   or `commondir` names when those lie in a writable root, and the other
   things D6 watches on macOS/Linux:
   rebase and sequencer steps (`exec` lines), links and hard links git
   writes through, and the legacy `remotes`/`branches` folders. Rewriting a
   file with the same content is not a change.
3. **Links by what they point to.** A link is recorded as its own identity,
   its target text and the target's identity, and flagged only if one of
   these changes. This needs measurement 1 below before it can be trusted.
4. **One entry per repository**, saying what changed ("new hook
   `post-checkout`", "config changed"), batched into the existing planted
   warning and prompt (one per start), listing at most 20 entries and
   "and N more".
5. **Remember the confirmation** with the state that was confirmed. The
   repository is asked about again only after it changes again, so a new
   repository costs one prompt, not one per start.
6. **"Plain repository" filter.** A new repository is recorded but not
   prompted only if its whole git folder has exactly the shape `git init`
   or `git clone` produce: a plain folder (not a link or `gitdir:` file),
   no `commondir`, no `config.worktree`, no links anywhere inside, no
   rebase or sequencer state, hooks that are only `*.sample` files, and a
   config holding only the keys those commands write (format version, file
   mode, bare, logallrefupdates, symlinks, ignorecase, `remote.*.url`,
   `remote.*.fetch`, `branch.*.remote` and `branch.*.merge`, with no
   `ext::` URL in any of them). Anything else, an include,
   or anything unreadable makes it not plain. Later changes to it are
   prompted as usual. This is the main lever against prompts for "clone
   this to look at it".
7. **Warn, never block**, as D332 does.

## Gap 1: nested `.git` below a writable root

**The attack.** A command runs `git init sub` (or writes `sub\.git\config`
by hand) with `core.fsmonitor = <program>`. The next `git status` in `sub`,
by you or your editor, runs it unsandboxed. Existing nested repositories
(vendored clones, sub-projects) are just as open: a command can add a hook
to `sub\.git\hooks` or edit its config. A submodule's real git folder
lives in its parent's `.git\modules`: read-only when the parent is the
protected top-level repository, writable when the parent is itself nested.
Either way the checkout's `.git` file is writable and can be pointed at a
planted git folder. Today nothing notices any of this, because only the
top-level `.git` is recorded.

**What to flag, and when.** When each command ends, using the walk D283
already does for nested `.codex`/`.agents`:

- a nested `.git` (folder, file or link) that is new or replaced;
- for nested repositories that existed at the start, a change to what rule
  2 lists;
- in an existing nested `.git`, the folders that hold other repositories
  (`modules\*`, `worktrees\*`), which the D283 walk doesn't enter today;
- a folder the walk can't list counts, as in D283.

Shown wherever decision 6 settles. Like D283, this does not cover roots
that come only from `%TEMP%`/`%TMP%`.

**Cost.** No new walk: D283's walk already lists every folder below the
top of each root (it doesn't follow links or descend into `.git` or the
folders it reports). Each repository found adds an identity open and a
few small reads (estimated well under a millisecond each; to be measured).
A `repo` client with thousands of projects is the worst case (see gap 4).

**Options.**

- **A. Flag new or replaced nested `.git` only.** Cheapest, but misses
  edits inside repositories that already existed.
- **B. A, plus watch existing nested repositories (rule 2).** Detects every
  case above at the next start, changes no permissions, and agents can
  still commit in nested repositories.
- **C. B, plus protect the config and hooks of existing nested
  repositories the way D283 protects nested `.codex`/`.agents`.** Prevents
  instead of detects, so it also covers editors that run git before the
  next start. But it costs permission changes per repository per command
  (D322 in the management handoff: three nested folders still add 0.6 s
  after #1688/#1707),
  and it breaks agent commands that write these files in nested
  repositories (`git config`, or the `pre-commit` tool's hook install).

**Recommendation: B**, with rules 3 and 6. Keep C for later, only if
measurements show it is cheap and the breakage is acceptable.

## Gap 2: a bare repository planted at a writable root

**The attack.** When git looks for a repository it checks, in each folder
from the working folder upwards, first `<folder>\.git` and then whether the
folder itself is a git folder: a valid `HEAD` file, plus `objects` and
`refs` folders either there or in the folder a `commondir` file names (git
then reads `config` and `hooks` from that folder too). So a command can
write `HEAD`, `objects\`, `refs\` and a `config` with
`core.pager = <program>` straight into a writable root that has no
`.git`, such as a scratch folder or an extra writable folder. The next
`git log` there, or in any subfolder without its own repository, runs the
program. The same trick works one level down even where the root has a
`.git`: a subfolder made into a bare repository is found before the root's
`.git`. A root that already is a bare repository when the command starts
is just as open: its `config` and `hooks` are writable. This is the
"embedded bare repository" attack that git's own `safe.bareRepository`
setting exists for; its default still allows it.

**What to flag, and when.** When a command ends: a writable root
(`%TEMP%` included, as D332) that has become a git folder, and, for a root
that already was one, a change to what rule 2 lists. Any `HEAD` or
`commondir` file at a root, or in a folder the D283 walk lists, is a
candidate, checked with git's full rule (including `commondir`), so a
`HEAD` here with `objects` and `refs` elsewhere is still caught. Like
D283, the nested part does not cover roots that come only from
`%TEMP%`/`%TMP%`.

**Cost.** Near zero: a couple of file checks per root, and the nested
check works on listings the walk already has, reading more only for a
candidate.

**Options.**

- **A. Flag a bare repository that appears at the top of a writable
  root.** Git uses it exactly where it would use `<root>\.git`.
- **B. A, plus nested bare repositories and roots that already were bare
  repositories, with the gap 1 rules.**
- **C. Docs: recommend setting `safe.bareRepository=explicit` in your
  global git config.** Git then refuses to use a bare repository it finds
  by searching, and stops with an error. Use git 2.45 or later: in 2.38
  to 2.44, `explicit` also refuses git run inside `.git`,
  `.git\worktrees\*` and `.git\modules\*`, which breaks some hooks and
  submodule tools, and versions before 2.38 ignore it. It only covers tools
  that run the git command line with that global config (not libgit2 or
  JGit based tools, nor a git with another home folder, such as WSL's). It
  breaks workflows that run git inside a bare repository without
  `--git-dir`, including some bare-repository-plus-worktrees setups.

**Recommendation: A now, C in `docs/sandbox.md` now, B together with gap 1's
B.** C is the only option that also covers editors before the next start.

## Gap 3: `core.hooksPath` pointing into a writable folder

**The attack.** Many projects set `core.hooksPath` to a folder in the work
tree: Husky (`.husky` in v5 to v8, `.husky\_` in v9, which also holds a
`.gitignore` so its contents never show in `git status`), or a team
`.githooks` folder. The repository's config is protected, but the folder it
names is ordinary, writable project content. A command writes a hook there,
and your next `git commit` runs it. Config values that name a file in the
work tree are the same case: `include.path` (a whole config file), or a
program key set to a script in the project.

**What to flag, and when.** At command start, read `core.hooksPath` and
the same-class keys from every config git would read for each watched
repository: its own `config` and `config.worktree`, the global configs
(`%USERPROFILE%\.gitconfig`, the XDG config), the system configs
(including Git for Windows' `%PROGRAMDATA%\Git\config`), and the
files their `include`/`includeIf` lines name (bounded depth). Nested
repositories' configs come from the gap 1 walk. If a value resolves inside
a writable root and outside a protected `.git`, record that folder's or
file's contents (identity and content hash). When the command ends, flag
new or changed files there. Husky v9's `_` files are only stubs: each runs
`.husky\<hook name>`, which is the hook code. So for Husky, also watch the
files in `.husky` named after git hooks: a new untracked
`.husky\post-checkout` runs on the next checkout without anyone looking at
it. Other scripts those hooks call are ordinary project files, like a
`Makefile`, and show in `git diff`; they are out of scope.

**Keeping prompts rare.** `npm install` reruns Husky, which rewrites
`.husky\_`, probably with the same content: rule 2 (content hash) keeps
that silent (measurement 4).

**Cost.** A few small config reads, one listing and hashing a few kilobytes
per command: a few milliseconds.

**Options.**

- **A. Watch and flag the hooks folder and the same-class targets.**
  Stricter only; nothing breaks.
- **B. Protect it, as macOS/Linux do at Moderate and Aggressive.** Breaks
  Husky's install inside the sandbox and goes beyond what any platform does
  at Permissive.
- **C. Docs only.** Explain the gap and suggest reviewing hooks folders.

**Recommendation: A.**

## Gap 4: Google `repo` tool layouts (prompt fatigue)

**The layouts.** In a `repo` client, each project's `.git` is a symbolic
link to `.repo\projects\<path>.git` (older clients: a `.git` folder whose
entries are links; with `--worktree`: a `.git` file). The client root has
`.repo` but no `.git`. On Windows `repo` needs symbolic links (Developer
Mode or admin rights), so this is uncommon; the survey in #1682 found no
`.git` link among 977 `.git` entries on three of our machines.

**The problem.** When a writable root is a project, its `.git` link has no
identity, so D332 records it again after every command: the warning and
prompt come back at every start and confirming never sticks. When the
writable root is the client root, the real git folders under
`.repo\projects` and `.repo\project-objects` are bare repositories inside
the writable root, and nothing protects or watches their config and hooks
today.

**What to flag, and when.** Use rule 3 for links: record the link's own
identity, its target text and the target folder's identity, and flag only
when one changes (deleting and remaking the link, or retargeting it in
place). For the client-root case, gap 1 B and gap 2 B cover `.repo`: the
projects' links and the bare git folders are recorded once and only their
changes are flagged.

**Cost.** Reading one link target and one extra identity per link. A large
client (hundreds to thousands of projects) multiplies gap 1's
per-repository cost; it should be measured, and anything the walk can't
read still counts as planted, as in D283.

**Options.**

- **A. Keep D332 as shipped.** Safe but noisy; fatigue spreads to every
  planted prompt.
- **B. Identify links by link plus target (rule 3), and stop flagging an
  unchanged top-level `.git` link.** One prompt per new client project,
  then quiet until something changes.
- **C. A per-folder "this is a `repo` client" setting that skips `.git`
  link checks.** Simple, but a blind spot the user must remember to scope.

**Recommendation: B**, once measurement 1 shows a sandboxed command can't
retarget a link without changing what rule 3 records.

## What fits D332, and what needs a new owner decision

D332 (accepted 11-07 17:45Z; management handoff, section 9) is "flag a
`.git` that appears or changes during a session", shipped for the
top-level `.git` as "new or replaced", with links always counting. Read
conservatively, only the first two rows fit it. Everything else is new
behaviour, and anything that removes a shipped warning or moves the
sandbox boundary also needs security sign-off.

| Item                                                                                                                                      | Stricter only?                                 | Status                                                                                                                                                                 |
| ----------------------------------------------------------------------------------------------------------------------------------------- | ---------------------------------------------- | ---------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| Gap 1 A: flag a nested `.git` folder or file that is new or replaced; nested links count every time, as top-level ones do                 | Yes                                            | Within D332 (the same rule at a new depth)                                                                                                                             |
| Gap 2 A: flag a bare repository appearing at the top of a writable root                                                                   | Yes                                            | Within D332's intent: it fills the very slot D332 watches (git uses it in place of `<root>\.git`), whereas a nested bare repository is both a new kind and a new place |
| Gap 2 C: recommend `safe.bareRepository=explicit` (git 2.45+) in the docs                                                                 | Docs only                                      | No decision needed                                                                                                                                                     |
| Rule 3 for nested links (decision 1)                                                                                                      | Yes for nested; same mechanism as gap 4 B      | Decision and security sign-off, after measurement 1                                                                                                                    |
| Gap 4 B: stop flagging an unchanged top-level `.git` link (decision 1)                                                                    | No (removes a shipped warning)                 | Decision and security sign-off                                                                                                                                         |
| Gap 1 B and rule 2: watch the contents of existing nested repositories and roots that already are bare repositories (decision 2)          | Yes                                            | Decision (wider than "appears or is replaced"; new records)                                                                                                            |
| Rule 6, "plain repository" filter (decision 3)                                                                                            | Yes for new coverage                           | Decision (chooses which new repositories go unasked)                                                                                                                   |
| Gap 2 B: nested bare repositories (decision 2)                                                                                            | Yes                                            | Decision (not a `.git`)                                                                                                                                                |
| Gap 3 A: watch `core.hooksPath` and same-class targets (decision 4)                                                                       | Yes                                            | Decision (ordinary project folders)                                                                                                                                    |
| Gap 1 C, gap 3 B: protect instead of flag (decision 5)                                                                                    | Moves the boundary; breaks some agent commands | Decision and security sign-off                                                                                                                                         |
| When and where warnings appear: a notice right after the command, and showing entries outside the working folder's ancestors (decision 6) | Yes                                            | Decision (new UI); the new checks depend on it                                                                                                                         |
| The same flags in the elevated sandbox (decision 7)                                                                                       | Yes                                            | Decision; it keeps no planted records at all today                                                                                                                     |

Order: gap 1 A and gap 2 A can be recorded now, but most of what they find
is only shown once decision 6 is taken. In a `repo` client root, gap 1 A
would also flag every project's link at every start until decision 1 is
taken. So the practical order is decision 6 first, then gap 1 A and gap 2 A
together with decision 1.

## To measure when the Windows PC is back

1. Whether a Low-integrity command can delete and remake, or retarget in
   place, a `.git` junction or symbolic link in a writable root, and
   whether every such change moves the link's identity or target text.
2. The owner of files a non-admin and an elevated command create, and
   whether git's `safe.directory` ownership check then refuses them (that
   would already cover much of gaps 1 and 2 in the elevated sandbox).
3. The added cost of gap 1 B and gap 2 B on this repository (38k files)
   and on a `repo` client with about 100 projects.
4. Whether `npm install` with Husky v9 inside the sandbox rewrites
   `.husky\_` with identical content (gap 3 silence).
5. Warnings per start in a `repo` client after `repo sync` in the sandbox,
   before and after gap 4 B.

## References

- #1775 (this note), #1682 and #1760 (D332), #1541 and #1642 (D283), #1506
  and #1550 (D262), D322 (#1699, #1688, #1707), #1758 and #1777, #540,
  #1766.
- Code: `codex-rs/windows-sandbox-rs/src/nested_metadata.rs` and
  `integrity_leases.rs`, `codex-rs/config/src/planted_project_metadata.rs`,
  `codex-rs/tui/src/chatwidget/windows_sandbox_prompts.rs`,
  `codex-rs/protocol/src/permissions.rs`
  (`default_read_only_subpaths_for_writable_root`),
  `codex-rs/core/src/security/git_postcheck.rs`,
  `codex-rs/core/src/security/protected_surface/git_paths.rs`.
- Google `repo` internal layout: `docs/internal-fs-layout.md` in the
  git-repo project.
