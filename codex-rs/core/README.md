# codex-core

This crate implements the business logic for Codex. It is designed to be used by the various Codex UIs written in Rust.

## Wine-exec integration tests

On x86-64 Linux, run the shared suite against the Windows exec server with
`bazel test //codex-rs/core:core-all-wine-exec-test`.

Local execution targets the host OS, Docker targets Linux, and Wine exec targets
Windows. Choose the skip macro by what the test depends on:

- `skip_if_target_windows!`: Windows target behavior.
- `skip_if_host_windows!`: Windows host constraints.
- `skip_if_remote!`: Local-only test behavior.
- `skip_if_no_remote_env!`: Remote-only test behavior.
- `skip_if_wine_exec!`: Wine-specific runner debt.

## Dependencies

Note that `codex-core` makes some assumptions about certain helper utilities being available in the environment. Currently, this support matrix is:

### macOS

Expects `/usr/bin/sandbox-exec` to be present.

When using the workspace-write sandbox policy, the Seatbelt profile allows
writes under the configured writable roots while keeping `.git` (directory or
pointer file), the resolved `gitdir:` target, and `.codex` read-only.

Network access and filesystem read/write roots are controlled by
`SandboxPolicy`. Seatbelt consumes the resolved policy and enforces it.

Seatbelt also keeps the legacy default preferences read access
(`user-preference-read`) needed for cfprefs-backed macOS behavior.

### Linux

Expects the binary containing `codex-core` to run the equivalent of `codex sandbox` when `arg0` is `codex-linux-sandbox`. See the `codex-arg0` crate for details.

Legacy `SandboxPolicy` / `sandbox_mode` configs are still supported on Linux.
They can continue to use the legacy Landlock path when the split filesystem
policy is sandbox-equivalent to the legacy model after `cwd` resolution.
Split filesystem policies that need direct `FileSystemSandboxPolicy`
enforcement, such as read-only or denied carveouts under a broader writable
root, automatically route through bubblewrap. The legacy Landlock path is used
only when the split filesystem policy round-trips through the legacy
`SandboxPolicy` model without changing semantics. That includes overlapping
cases like `/repo = write`, `/repo/a = none`, `/repo/a/b = write`, where the
more specific writable child must reopen under a denied parent.

The Linux sandbox helper prefers the first `bwrap` found on `PATH` outside the
current working directory whenever it is available. If `bwrap` is present but
too old to support `--argv0`, the helper keeps using system bubblewrap and
switches to a no-`--argv0` compatibility path for the inner re-exec. If
`bwrap` is missing, it falls back to the bundled `codex-resources/bwrap`
binary shipped with Codex and Codex surfaces a startup warning through its
normal notification path instead of printing directly from the sandbox helper.
Codex also surfaces a startup warning when bubblewrap cannot create user
namespaces. WSL2 uses the normal Linux bubblewrap path. WSL1 is not supported
for bubblewrap sandboxing because it cannot create the required user
namespaces, so Codex rejects sandboxed shell commands that would enter the
bubblewrap path before invoking `bwrap`.

### Windows

Legacy `SandboxPolicy` / `sandbox_mode` configs are still supported on
Windows. Legacy `read-only` and `workspace-write` policies imply full
filesystem read access; exact readable roots are represented by split
filesystem policies instead.

The elevated Windows sandbox also supports:

- legacy `ReadOnly` and `WorkspaceWrite` behavior
- split filesystem policies that need exact readable roots, exact writable
  roots, or extra read-only carveouts under writable roots
- backend-managed system read roots required for basic execution, such as
  `C:\Windows`, `C:\Program Files`, `C:\Program Files (x86)`, and
  `C:\ProgramData`, when a split filesystem policy requests platform defaults

The unelevated restricted-token backend still supports the legacy full-read
Windows model for legacy `ReadOnly` and `WorkspaceWrite` behavior. It also
supports a narrow split-filesystem subset: full-read split policies whose
writable roots still match the legacy `WorkspaceWrite` root set, but add extra
read-only carveouts under those writable roots.

Deletes in the unelevated backend (#158): its write-restricted token checks
the writable-root capability SIDs for write accesses, including `DELETE`, but
Windows does not treat `FILE_DELETE_CHILD` as a write. NTFS lets a caller delete
or move a file it has no `DELETE` right on when the parent directory grants
`FILE_DELETE_CHILD`, so on its own the token let a sandboxed command delete
most of the user profile, in both `ReadOnly` and `WorkspaceWrite` modes.
Commands therefore run at Low integrity: a Low-integrity process cannot delete,
move or write an object labelled Medium (the default for unlabelled objects).
Each writable root gets an inheritable Low no-write-up label while a session
uses it, so writes and deletes inside it keep working (option (b), security
sign-off by Travis on 2026-10-13):

- Every legacy command takes a lease on its writable roots. The first lease on
  a root records the root's own label and labels it Low; when the last lease
  ends (once the command and what it left running have exited), the recorded
  label is put back and propagates to the folder's children again. So once no
  session runs, a `ReadOnly` session, the PF-27-S08 credential broker or
  another Low-integrity program cannot delete in a workspace or `%TEMP%` an
  earlier `WorkspaceWrite` session used.
- The records are per Windows user, in
  `%LOCALAPPDATA%\CorbanuTerminalSandbox\integrity-labels` (labelled Medium,
  no write-up, no read-up and no execute-up, so no Low-integrity process can
  open anything there). Each live session holds a session file open; a
  session that was killed leaves a file that can be deleted, and every lease,
  the end of every lease and the start of the TUI and app server put back the
  labels only such sessions held. A writable root that holds the records
  protects them like `.git`.
- While a command runs, its roots are Low: other Low-integrity programs of the
  user can delete there (a `ReadOnly` session running at the same time and
  the credential broker included, so the broker's confinement holds for
  `%TEMP%` only while no legacy session uses it), and programs that are not
  write-restricted can write there.
- Labelling and putting the label back each walk the whole folder tree, once
  per command when no other session holds the root, which can be slow for
  large trees (or a large `%TEMP%`). Objects a command itself labels Low (with
  `icacls`, say) keep that label. The records are kept by path: a nested root
  that another session's command moves keeps its Low label at the new place,
  and a root swapped for a link between the host resolving it and labelling it
  gets the label at the link's target (as the root's write grant does). Roots
  shared with another Windows account's sessions are not tracked across
  accounts; such a root can stay Low for good, since the other account records
  the Low label as the original.
- Labelling needs permission to change the folder's owner settings
  (`WRITE_OWNER`) and a file system that holds integrity labels. A root
  without them is left as it is: commands can't write there, and the sandbox
  log says why. If propagation stops partway, files below the root may stay
  unwritable (or, when the label is put back, Low).
- Processes a command leaves running (a server, say) keep the lease until the
  last of them exits. When Core exits first (or exits while a session runs),
  the labels stay Low until the next TUI or app-server start, or the next
  legacy command, puts them back. If the records are damaged, legacy commands
  that write stop with an error that says how to recover.
- Protected paths inside writable roots (and other read-only carve-outs) get
  an explicit Medium label, once, so they don't inherit the Low label and
  other Low-integrity programs can't write them. If that label can't be set
  while the path is Low, the command does not start. Paths that are links, or
  that resolve outside the writable roots (a `.git` file's `gitdir:` target,
  for example), are left alone.
- Commands can no longer write outside writable roots to objects that grant
  Everyone or the logon SID write access, or open other programs' named pipes
  and other IPC endpoints for writing, unless those are labelled Low.
- The desktop commands start on is labelled Low: the per-command private
  desktop, or Core's own desktop when Core runs off the interactive window
  station with `sandbox_private_desktop = false`.

A legacy command's job doesn't allow breaking away (#733; not covered by that
sign-off): a child started with `CREATE_BREAKAWAY_FROM_JOB` fails with access
denied, so its descendants stay in its job, where cancelling ends them and the
lease waits for them. A program that asks to break away gets that error unless
it retries without the flag. The elevated backend and unsandboxed commands
still allow breaking away.

Other ways to start a process (#1507), measured on the hosted Windows runner
(no Explorer running; not yet measured on a desktop): WMI's
`Win32_Process.Create` refuses a legacy command (access denied), as do
activating Explorer's `ShellWindows` (E_ACCESSDENIED) and the Task
Scheduler (opening its task folder fails with 0x80070003). Naming another
legacy command's process as the parent (`PROC_THREAD_ATTRIBUTE_PARENT_PROCESS`)
is refused too since D259 (but see #1560 below): each legacy command's token
carries its own identity SID, and its default DACL (what its processes, threads, token and
the pipes and other objects it creates without explicit security get)
grants full access only to the user, SYSTEM and that SID, and read and
execute to Everyone, the logon SID and the capability SIDs. A legacy command
writes only through its restricting SIDs, so it keeps full access to its own
processes but can't open another legacy command's with any write right
(start a child under it, change its DACL, duplicate its handles, write its
memory) or adjust its token. Core and other unsandboxed programs keep full
access through the user SID. Cost: a legacy command can't write to an object
that another legacy command's process (a concurrent one, or one left running
by an earlier command) created without explicit security, such as a named
pipe, event or section a shared daemon serves; nor to a file it created in a
folder without inheritable entries. Not closed: other legacy commands can
still end its processes (`PROCESS_TERMINATE` comes with execute access), and
can still duplicate and impersonate its token (`TOKEN_DUPLICATE` isn't a
write right). The borrowed token carries the command's identity, so with it
they can write to its processes again, including starting a child in its job
through the parent-process route, and write in its writable roots while it
runs (#1560). The boundary D259 draws between legacy commands holds only once
#1560 is closed. Two routes still start a child
outside the command's job: the COM servers `ShellBrowserWindow` (the child
runs at Low integrity but with an unrestricted token) and
`MMC20.Application` (Low, restricted). Closing them waits on a measurement
on a real desktop (D260); their tests are ignored until then.

Protected paths that already exist inside writable roots when a session
starts, such as `.git`, `.codex`, and `.agents`, also get an Everyone deny of
`FILE_DELETE_CHILD` on the path, its subdirectories, and its parent directory.
That keeps the sandbox from deleting, moving, or replacing them through the
parent, which would otherwise let it plant Git hooks or config that later run
unsandboxed. For an explicitly configured read-only path nested deeper in a
writable root (`w\a\b`, say), each folder between the root and it (`w\a`)
also denies the sandbox `DELETE`, and that folder's parent denies Everyone
`FILE_DELETE_CHILD`, so a command can't move or rename the folder away and
recreate the read-only path (#1506 item 3, D263). Commands can still write
inside those folders but can't delete or rename the folders themselves; the
default `.git`, `.codex` and `.agents` protections sit directly in the root
and don't add these. A `.codex` or `.agents` folder at the top of a writable
root that doesn't exist when a session starts isn't protected, so a command
can create it (#1506 item 2). Each session records which of them were absent;
one that exists when the session ends (or, for a session still running, as
soon as it appears; for one that crashed, at the next sweep) is recorded as
possibly planted in `planted-metadata.json` next to the integrity-label
records (D262). Until the user confirms it, Core loads neither its config
and hooks (the project layer is disabled) nor its skills, and says so at
startup; the TUI asks whether to load it, and confirming is refused while a
session that saw it appear still runs. A planted link counts as the folder
itself (it isn't followed), and a linked worktree's hooks from a planted root
checkout `.codex` are ignored too. If the records can't be read, no project
`.codex`/`.agents` content is loaded at all. Anything that creates such a
folder while a session runs is reported, including the user. Not covered:
`.codex`/`.agents` folders below the top of a writable root (between the
project root and the working directory), which commands can create or change
whether or not they exist. The
Everyone deny entries are visible in the folder's security settings while a
session needs them. Each one a session adds is recorded first, in
`delete-child-denies.json` next to the integrity-label records (#1506 item 1,
D261), and removed by the sweep that puts labels back once no live session's
writable root holds its folder and the folder isn't labelled Low (a Low
label means a sandbox, perhaps another Windows account's, whose records
these aren't, may be using it); a session that crashed has its entries
removed by the next sweep (a later legacy command, or the next TUI or
app-server start). A deny that D263 needs above an explicit nested read-only
path must be in place, or the command doesn't start. An entry that was already there and isn't recorded (one
the user made, or one left by a version before this change) is never
removed. Without usable records the entries are added untracked and stay, as
before. Unsandboxed deletes normally keep working because they use each
object's own `DELETE` right; only a delete that relies on the parent's
`FILE_DELETE_CHILD` is affected. To remove leftover entries, run
`icacls <dir> /remove:d *S-1-1-0` on the affected directories. That command
also removes any other Everyone deny entries on them. The sandbox `DELETE`
denies on folders above nested read-only paths (D263) name the sandbox's own
capability SIDs, like its other deny entries, and stay; if one can't be added,
the command doesn't start. To remove them, first drop the nested read-only
path from your configuration (otherwise the next legacy command adds them
back), then run `icacls <dir> /remove:d *<sid>` on each folder between the
writable root and the read-only path, for each SID listed in `cap_sid` under
`CODEX_HOME` (a JSON file). That also removes the sandbox's other deny
entries for that SID on the folder, which the next legacy command that needs
them adds back.

New `[permissions]` / split filesystem policies remain supported on Windows
only when they can be enforced directly by the selected Windows backend or
round-trip through the legacy `SandboxPolicy` model without changing semantics.
Policies that would require direct explicit unreadable carveouts (`none`) or
reopened writable descendants under read-only carveouts still fail closed
instead of running with weaker enforcement.

### All Platforms

Expects the binary containing `codex-core` to simulate the virtual
`apply_patch` CLI when `arg1` is `--codex-run-as-apply-patch`. See the
`codex-arg0` crate for details.
