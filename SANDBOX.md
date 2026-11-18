# Workspace sandbox

Merry confines each process action. The Merry process itself stays on the host,
so its model provider and configured MCP connections are not covered by the
action network switch.

## Modes

| Flag | Config | Behavior |
| --- | --- | --- |
| `--sandbox workspace` | `sandbox = "workspace"` | Default. Each action is confined. |
| `--sandbox off` | `sandbox = "off"` | The action runs on the host. Path and network limits in `[permissions]` are not applied. |

On Linux, workspace mode uses [Landlock](https://docs.kernel.org/userspace-api/landlock.html)
for the filesystem and a seccomp filter that rejects `AF_INET`, `AF_INET6`, and
`AF_PACKET` sockets when that action was not granted network. The kernel must
be able to enforce Landlock. If it cannot, Merry stops and asks you to pass
`--sandbox off`. It does not silently drop the sandbox.

On macOS, workspace mode uses Seatbelt through `/usr/bin/sandbox-exec -p`.
The profile allows the same paths. Linux tests check that profile text.
Seatbelt itself is not executed on Linux.

## What an action can see

Readable: the workspace, `/usr`, `/bin`, `/sbin`, `/lib`, `/lib64`, `/opt`,
`/proc`, `/sys`, and a small set of `/etc` files (`ld.so`, `resolv.conf`,
`hosts`, `nsswitch.conf`, `passwd`, `group`, `localtime`, `gitconfig`, plus the
SSL, PKI, and alternatives directories). Existing `~/.gitconfig`,
`~/.config/git/config`, `~/.config/git/ignore`, and `~/.config/git/attributes`
are readable, and so are the same three files under `$XDG_CONFIG_HOME/git`
when that directory is absolute and different. A `deny_paths` or `review_paths`
entry still keeps a listed file out. `~/.git-credentials` is not granted.

Writable: the workspace, `/tmp`, `/var/tmp`, `/dev`, `/dev/shm`, `/dev/pts`,
`TMPDIR`, the Cargo registry and git database, and the XDG cache. Cargo `bin`
and rustup toolchains are read-only.

The rest of the home directory can be traversed so those granted paths can be
opened. It is not listed or read. Private keys, `~/.aws`, `~/.config/gh`, and
Merry's own config and state stay out.

Network is off for an ordinary action. After permission review approves
`network: true` for that action, the seccomp filter is not installed. The next
action starts closed again. `[permissions] network` defaults to on, which only
means a request is allowed to ask. `network = false` rejects the request before
review, and approval cannot override it.

## What Landlock cannot do

Landlock is an allow list. It cannot block a path inside a directory it
already allows. If `deny_paths` or `review_paths` names something inside the
workspace, `/tmp`, or another granted directory, that action does not start.
The error names both entries, for example `review_paths` inside the writable
workspace, and says which one to remove.

A read-only rule inside a writable parent is not enforced by the kernel. The
permission layer still records the ceiling, but a command that already has the
parent writable can write the child. In-workspace `.git` is intentionally
writable with the workspace.

Landlock does not hide directory entries, and it does not cover every metadata
operation. Names outside the allow list can still be stated even when opening
them fails.

## Permission mode

`--permission-mode auto` is the default. The approval-review model decides
first. `--auto-fallback ask` (the default) asks you when that model denies or
cannot decide. `--auto-fallback deny` returns the denial to the working model.
`--permission-mode ask`, `always-approve`, and `deny` skip that split.
`--always-approve` conflicts with `--permission-mode`.

A denial is a failed tool result. The turn continues until the model stops.

`--sandbox off` together with `--always-approve` runs without the sandbox and
without a review round. That combination is available and is not the default.

## First open

The first time workspace mode opens a directory, Merry shows a centered
dialog. The notice asks you to trust the directory. Read-write loads
`AGENTS.md`. Read-only does not load it and leaves the directory read-only.
"Don't ask again" saves that choice; leaving it at no asks again next time. Exit
closes Merry without opening the directory. `/tmp`, toolchains, the Cargo
registry, and the cache stay available either way. Network is not part of
this question.

A saved choice lives in `$XDG_STATE_HOME/merry/workspace-trust.json`, which
defaults to `~/.local/state/merry/workspace-trust.json`. Opening the same
directory again reuses it. Inside a Git checkout the saved directory
is the repository root, so a later launch from a subdirectory does not ask
again and commands run with that root as the workspace. `--trust-workspace yes`
or `no` sets the choice without a prompt. With no saved choice and no terminal,
Merry stops and tells you to pass that flag. `--sandbox off` does not ask,
because the process can already write anywhere, and it still loads `AGENTS.md`.

## Host integrations

`ssh_agent`, `gpg_agent`, and `dbus` grant the validated socket for ordinary
actions. SSH also grants existing `~/.ssh/known_hosts` and `known_hosts2`
read-only. GnuPG also grants existing `pubring.kbx` and `pubring.gpg`
read-only and sets `GNUPGHOME`. Private keys are not granted. A keyboxd
database is reported instead of being opened.

`deny_paths` and `review_paths` that cover an endpoint keep that endpoint out
of the action. A deny that sits inside a broader grant fails closed, as above.
