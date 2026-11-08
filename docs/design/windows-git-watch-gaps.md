# Windows non-admin sandbox: `.git` watch gaps beyond D332

Design note for #1775. No code changes. Written 2026-11-07, read from the
code on `main` (after #1760, #1762 and #1777); nothing here was measured on
Windows (the PC is off). "Non-admin" is the legacy, unelevated sandbox;
"elevated" is the default Windows sandbox.

## Why `.git` matters

Git reads a repository's `config` and runs its `hooks` with your full
rights, outside any sandbox, whenever you (or your editor) run git there.
Several config keys run programs: `core.fsmonitor` (on `git status`),
`core.pager` (on `git log` or `git diff`), `core.editor`, `core.sshCommand`,
`diff.external`, `alias.*`, filter drivers and `credential.helper`. So a
sandboxed command that can plant or edit a git folder can get code run
later, unsandboxed. Editors make this worse: VS Code's git support, for
example, runs git in the background and looks for repositories in
subfolders.

## What D332 does today (#1760)

- In the non-admin sandbox, each session records the `.git` at the top of
  each watched writable root (D283's roots, so not `%TEMP%`/`%TMP%`), with
  its file identity.
- When the session ends (or a sweep finds it crashed), a top-level `.git`
  that is new or replaced is recorded as planted. Corbanu Terminal warns at
  startup when it lies between the project root and the working folder, and
  the TUI asks whether you have checked it ("Keep warning me" / "Confirm
  them"). Nothing is refused: `git init` still works.
- A `.git` that is a link (junction or symbolic link) has no identity, so it
  is flagged after every session.
- An existing top-level `.git` is already read-only for sandboxed commands
  (inherited from upstream; for a link the protection lands on its target).
  #1777 (from #1758) does the same in the elevated sandbox for a `.git` made
  on the way to a read-only `.git\hooks`.
- Not covered: everything in the four gaps below. The elevated sandbox keeps
  no planted-folder records at all (as for D262).

## How others treat the same cases

| Case                                  | Upstream Codex (all OSes)                               | Corbanu on macOS/Linux                                                                                                                                                                      | Windows non-admin today     |
| ------------------------------------- | ------------------------------------------------------- | ------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- | --------------------------- |
| Nested `.git` below a writable root   | Writable, not watched                                   | Writable at every level; the post-command `.git` check (D6, Moderate/Aggressive) only looks at the repository a folder is in, so a new nested `.git` is left to the command-text classifier | Writable, not watched       |
| Bare repository at a writable root    | Writable, not watched (only `<root>/.git` is protected) | Same                                                                                                                                                                                        | Not watched                 |
| `core.hooksPath` in a writable folder | Writable                                                | Read-only at Moderate and Aggressive (`protected_surface/git_paths.rs`; Aggressive's "grant once" can lift it); writable at Permissive                                                      | Writable                    |
| `.git` that is a link (`repo` tool)   | Protection lands on the target                          | Same                                                                                                                                                                                        | Flagged after every session |

The non-admin sandbox refuses profiles that deny reads (`docs/sandbox.md`),
and Moderate and Aggressive add read denials. So in practice it only runs
at Permissive, where no platform covers these cases. Every option below is
therefore stricter than any other platform at the same level.

## Keeping prompts rare (applies to all four gaps)

A prompt that shows up after every command teaches people to click
"Confirm" without looking, which also weakens the `.codex`/`.agents`
prompts. Rules for any new check:

1. **Compare the end of a session with its start.** A repository a command
   makes and deletes again (test suites do this) is never shown.
2. **Compare what git would run, not timestamps.** File identity for the
   `.git` entry; identity plus a content hash for the small files git runs
   (`config`, `config.worktree`, `commondir`, `hooks\*`). Rewriting a file
   with the same content is not a change.
3. **Links by what they point to.** A link is recorded as its own identity,
   its target text and the target's identity, and flagged only if one of
   these changes. Never "every link counts every time" for new coverage.
4. **One entry per repository**, saying what changed ("new hook
   `post-checkout`", "config changed"), batched into the existing planted
   prompt, at most one prompt per session, listing at most 20 entries and
   "and N more".
5. **Remember the confirmation** with the state that was confirmed. The
   repository is asked about again only after it changes again, so a new
   repository costs one prompt, not one per command.
6. **Optional "plain repository" filter** (decision 1 below). A new
   repository whose config holds only the keys `git init`/`git clone` write
   (format version, file mode, bare, logallrefupdates, symlinks, ignorecase,
   `remote.*.url`/`fetch` without an `ext::` URL, `branch.*`), whose hooks
   are only `*.sample` files, and whose `.git` is a plain folder is recorded
   but not prompted. Any other key, an `include`, or an unreadable file
   makes it not plain. Later changes to it are prompted as usual. This is
   the main lever against prompts for "clone this to look at it".
7. **Warn, never block**, as D332 does.

## Gap 1: nested `.git` below a writable root

**The attack.** A command runs `git init sub` (or writes `sub\.git\config`
by hand) with `core.fsmonitor = <program>`. The next `git status` in `sub`,
by you or your editor, runs it unsandboxed. Existing nested repositories
(vendored clones, sub-projects, and the `.git` file of each submodule
checkout) are just as open: a command can add a hook to `sub\.git\hooks`,
edit its config, or point a submodule's `.git` file at a planted git
folder. Today nothing notices any of this, because only the top-level
`.git` is recorded.

**What to flag, and when.** At the end of each session, using the walk
D283 already does for nested `.codex`/`.agents`:

- a nested `.git` (folder, file or link) that is new or replaced;
- for nested repositories that existed at the start, a change to `config`,
  `config.worktree`, `commondir`, anything under `hooks`, or a `.git` file's
  `gitdir:` line;
- a folder the walk can't list counts, as in D283.

Shown in the same TUI prompt and, when the working folder is below it, in
the startup warning.

**Cost.** No new walk: D283's walk already lists every folder and only
skips descending into `.git`. Each repository found adds an identity open
and reading a few small files (estimated well under a millisecond each;
to be measured). A `repo` client with thousands of projects is the worst
case (see gap 4).

**Options.**

- **A. Flag new or replaced nested `.git` only.** Cheapest, but misses
  edits inside repositories that already existed.
- **B. A, plus watch the config and hooks of existing nested repositories
  (rules 2 and 3).** Detects every case above after the fact, changes no
  permissions, and agents can still commit in nested repositories.
- **C. B, plus protect the config and hooks of existing nested repositories
  the way D283 protects nested `.codex`/`.agents`.** Prevents instead of
  detects, also before your editor's background git sees it. But it costs
  permission changes per repository each session (D322: about 0.2 s per
  protected nested folder even after #1688/#1707), and it breaks agent commands
  that write these files in nested repositories (`git config`, hook
  installers such as `pre-commit install`).

**Recommendation: B**, with the "plain repository" filter. Keep C for later,
only if measurements show it is cheap and the breakage is acceptable.

## Gap 2: a bare repository planted at a writable root

**The attack.** When git looks for a repository it checks, in each folder
from the working folder upwards, first `<folder>\.git` and then whether the
folder itself is a git folder (a `HEAD` file plus `objects` and `refs`
folders). So a command can write `HEAD`, `objects\`, `refs\` and a `config`
with `core.pager = <program>` straight into a writable root that has no
`.git`, such as a scratch folder or an extra writable folder. The next
`git log` there, or in any subfolder without its own repository, runs the
program. The same trick works one level down even where the root has a
`.git`: a subfolder made into a bare repository is found before the root's
`.git`. This is the "embedded bare repository" attack that git's own
`safe.bareRepository` setting exists for; its default still allows it.

**What to flag, and when.** At the end of a session: a watched root that
has become a git folder (three file checks per root). For nested bare
repositories, the D283 walk can spot the `HEAD`/`objects`/`refs` pattern in
listings it already has, with no extra reads unless it matches.

**Cost.** Near zero for the root check; the nested check is in-memory on
the existing walk.

**Options.**

- **A. Flag a bare repository that appears at the top of a writable root.**
  Git uses it exactly where it would use `<root>\.git`.
- **B. A, plus nested bare repositories, with the gap 1 rules** (plain
  filter, content compare for existing ones).
- **C. Docs only: recommend setting `safe.bareRepository=explicit` in
  your global git config** (git 2.38 or later). Git then ignores bare repositories it
  finds by searching, for every program on the machine, including editors.
  It breaks only workflows that `cd` into a bare repository and run git
  there without `--git-dir`.

**Recommendation: A now, C in `docs/sandbox.md` now, B together with gap 1's
B.** C is the only option that also protects the time before the prompt is
answered.

## Gap 3: `core.hooksPath` pointing into a writable folder

**The attack.** Many projects set `core.hooksPath` to a folder in the work
tree: Husky (`.husky` in older versions, `.husky\_` in v9, which also holds
a `.gitignore` so its contents never show in `git status`), or a team
`.githooks` folder. The repository's config is protected, but the folder it
names is ordinary, writable project content. A command writes a hook there,
and your next `git commit` runs it. A `core.hooksPath` in your global config
that happens to point into a writable folder is the same, rarer, case. So
are config values that name a file inside the work tree: `include.path`
(a whole config file), or `core.fsmonitor`/`core.sshCommand` set to a
script in the project.

**What to flag, and when.** At session start, read `core.hooksPath` (and the
same-class keys) from the repository config of each watched root and from
your global configs (`%USERPROFILE%\.gitconfig`, the XDG config). If it
resolves inside a writable root and outside a protected `.git`, record that
folder's files with identity and content hash. At the end, flag new or
changed files there. Scripts that those hooks call elsewhere in the project
(Husky v9's `_` hooks run the tracked `.husky\pre-commit`) are ordinary
project files, like a `Makefile`, and show in `git diff`; they are out of
scope.

**Keeping prompts rare.** `npm install` reruns Husky, which rewrites
`.husky\_` with the same content: rule 2 (content hash) keeps that silent.

**Cost.** A few small config reads, one listing and hashing a few kilobytes
per session: a few milliseconds.

**Options.**

- **A. Watch and flag the hooks folder (and the same-class targets).**
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
Mode or admin rights), so this is uncommon; the D332 survey found no `.git`
link among 977 `.git` entries on the Mac, glitch and the desktop.

**The problem.** When a writable root is a project, its `.git` link has no
identity, so D332 flags it after every session: a prompt after every
command. When the writable root is the client root, the real git folders
under `.repo\projects` and `.repo\project-objects` are bare repositories
inside the writable root, and nothing protects or watches their config and
hooks today.

**What to flag, and when.** Use rule 3 for links: record the link's own
identity, its target text and the target folder's identity, and flag only
when one changes (deleting and remaking the link, or retargeting it in
place). For the client-root case, gap 1 B and gap 2 B cover `.repo`: the
projects' links and the bare git folders are recorded once and only their
changes are flagged.

**Cost.** Reading one link target and one extra identity per link. A large
client (hundreds to thousands of projects) multiplies gap 1's per-repository
cost; it should be measured, and anything the walk can't read still counts
as planted, as in D283.

**Options.**

- **A. Keep D332 as shipped.** Safe but noisy; fatigue spreads to every
  planted prompt.
- **B. Identify links by link plus target (rule 3), and stop flagging an
  unchanged top-level `.git` link.** One prompt per new client project, then
  quiet until something changes.
- **C. A per-folder "this is a `repo` client" setting that skips `.git` link
  checks.** Simple, but a blind spot the user must remember to scope.

**Recommendation: B.**

## What fits D332, and what needs a new owner decision

D332 (accepted 11-07 17:45Z) is "flag a `.git` that appears or changes
during a session", shipped for the top-level `.git`. Every item below is
warnings only, unless marked.

| Item                                                                     | Stricter only?                                 | Status                                                                                                                                                           |
| ------------------------------------------------------------------------ | ---------------------------------------------- | ---------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| Gap 1 A/B: flag new, replaced or changed nested `.git` (links by rule 3) | Yes                                            | Within D332: a nested `.git` that appears or changes is the decided case, at a new depth. Applying rule 3 to nested links narrows nothing that is watched today. |
| Gap 2 A: flag a bare repository appearing at the top of a writable root  | Yes                                            | Within D332's intent: it is the folder git uses in place of `<root>\.git`.                                                                                       |
| Gap 2 C: recommend `safe.bareRepository=explicit` in the docs            | Docs only                                      | No decision needed.                                                                                                                                              |
| Rules 1, 2, 4, 5, 7 for the new checks                                   | Yes                                            | Within D332 (they shape new warnings only).                                                                                                                      |
| Rule 6 ("plain repository" filter)                                       | Yes for new coverage                           | Needs a decision: it chooses which new repositories are trusted unasked.                                                                                         |
| Gap 2 B: nested bare repositories                                        | Yes                                            | Needs a decision: not a `.git`.                                                                                                                                  |
| Gap 3 A: watch `core.hooksPath` and same-class targets                   | Yes                                            | Needs a decision: ordinary project folders, not `.git`.                                                                                                          |
| Gap 4 B: stop flagging an unchanged top-level `.git` link                | No (removes a shipped warning)                 | Needs a decision and security sign-off.                                                                                                                          |
| Gap 1 C, gap 3 B: protect instead of flag                                | Moves the boundary; breaks some agent commands | Needs a decision and security sign-off.                                                                                                                          |
| The same flags in the elevated sandbox                                   | Yes                                            | Needs a decision; it keeps no planted records at all today.                                                                                                      |

Order: ship gap 1 B together with rule 3 for nested links, or a `repo`
client root would be flagged after every session.

## To measure when the Windows PC is back

1. Whether a Low-integrity command can delete and remake, or retarget in
   place, a `.git` junction or symbolic link in a writable root.
2. The owner of files a non-admin and an elevated command create, and
   whether git's `safe.directory` ownership check then refuses them (that
   would already cover much of gaps 1 and 2 in the elevated sandbox).
3. The added cost of gap 1 B and gap 2 B on this repository (38k files)
   and on a `repo` client with about 100 projects.
4. Whether `npm install` with Husky v9 inside the sandbox rewrites
   `.husky\_` with identical content (gap 3 silence).
5. Prompts per session in a `repo` client running `repo sync` in the
   sandbox, before and after gap 4 B.

## References

- #1775 (this note), #1682 and #1760 (D332), #1541 and #1642 (D283), #1506
  and #1550 (D262), D322 (#1699, #1688, #1707), #1758 and #1777, #1766.
- Code: `codex-rs/windows-sandbox-rs/src/nested_metadata.rs`,
  `codex-rs/config/src/planted_project_metadata.rs`,
  `codex-rs/protocol/src/permissions.rs`
  (`default_read_only_subpaths_for_writable_root`),
  `codex-rs/core/src/security/git_postcheck.rs`,
  `codex-rs/core/src/security/protected_surface/git_paths.rs`.
- Google `repo` internal layout: `docs/internal-fs-layout.md` in the
  git-repo project.
