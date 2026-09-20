# Sandbox setup and permissions

Merry runs every process action inside bubblewrap. [Setup](#setup) prepares the
host, [Configure](#configure) sets the boundary for a session, and
[How it works](#how-it-works) explains the model behind those settings.

## Setup

Merry runs process actions inside [bubblewrap](https://github.com/containers/bubblewrap)
(`bwrap`). The TUI and `merry run` use two layers by default:

- an **outer CLI sandbox** that limits which host paths the session can see, and
- an **inner action sandbox**, started from inside the outer one, for each
  process action.

The inner layer therefore needs bubblewrap to run inside bubblewrap.

### Requirements

- `bwrap` on `PATH` (package `bubblewrap` on Debian, Ubuntu, Fedora, and Arch).
- Unprivileged user namespaces. Every mainstream distribution kernel allows
  them today; see [Kernel restrictions](#kernel-restrictions) for the sysctls
  that turn them off.
- Nested bubblewrap for the default outer+inner mode. Debian, Fedora, and Arch
  allow this out of the box. **Ubuntu 24.04 and newer block it by default**
  through AppArmor; see the next section.

### Check your host

Run the same probes Merry uses. The first checks a single sandbox, the second
checks bubblewrap inside bubblewrap:

```sh
bwrap --unshare-user --ro-bind / / --proc /proc --dev /dev -- /usr/bin/true && echo single OK

bwrap --unshare-user --ro-bind / / --proc /proc --dev /dev -- \
  bwrap --unshare-user --ro-bind / / --proc /proc --dev /dev -- /usr/bin/true && echo nested OK
```

If the second command prints `bwrap: No permissions to create new namespace`
while the first succeeds, the kernel is not the problem. Look at the kernel
audit log while it fails:

```sh
sudo journalctl -k --since '-1min' | grep apparmor
```

A line such as the following confirms the AppArmor case:

```
apparmor="DENIED" operation="capable" class="cap" profile="unpriv_bwrap" comm="bwrap" capname="sys_admin"
```

### Ubuntu 24.04 and newer: the bubblewrap AppArmor profile

Before the outer sandbox starts, Merry probes whether bubblewrap can run inside
bubblewrap. When the probe fails, startup stops with a warning like this instead
of starting a session whose every action would fail:

```
warning: bubblewrap cannot start inside Merry's outer sandbox on this host, so every process action would fail; command permission approval cannot fix sandbox initialization.
  bwrap: No permissions to create new namespace, likely because the kernel does not allow non-privileged user namespaces. ...
  The bubblewrap AppArmor profile /etc/apparmor.d/bwrap-userns-restrict is installed. On Ubuntu 24.04 and newer it strips capabilities from nested bubblewrap even though the kernel allows unprivileged user namespaces.
  See SANDBOX.md (https://github.com/locez/merry/blob/main/SANDBOX.md) to allow nested bubblewrap, or rerun with --inner-sandbox to use a single action sandbox.
```

Merry never downgrades the sandbox mode on its own: fix the host as described
below, or choose a weaker mode explicitly with `--inner-sandbox` (one action
sandbox, no outer layer) or `--no-sandbox` (no bubblewrap at all).

Ubuntu restricts unprivileged user namespaces with AppArmor
(`kernel.apparmor_restrict_unprivileged_userns=1`) and ships a profile for
bubblewrap in `/etc/apparmor.d/bwrap-userns-restrict`. That profile lets
`bwrap` itself create namespaces and mount, but everything `bwrap` runs is
stacked into a child profile named `unpriv_bwrap` that denies all capabilities.
A second `bwrap` started inside the sandbox can create its user namespace but
is refused `CAP_SYS_ADMIN` for its mounts, which is the failure above.

Note that the hint in bubblewrap's own error message about
`kernel.unprivileged_userns_clone` does not apply here. Turning off
`kernel.apparmor_restrict_unprivileged_userns` alone does not help either,
because the profile still attaches to `/usr/bin/bwrap` by path.

#### Allow nested bubblewrap

The packaged profile includes two site-local override files. Create both with
the same single rule:

```sh
sudo tee /etc/apparmor.d/local/bwrap-userns-restrict /etc/apparmor.d/local/unpriv_bwrap >/dev/null <<'RULE'
# Allow bubblewrap inside bubblewrap: when a sandboxed process executes bwrap,
# attach the full bwrap profile again instead of the capability-stripped stack.
priority=1 allow px /usr/bin/bwrap -> bwrap,
RULE
sudo apparmor_parser -r /etc/apparmor.d/bwrap-userns-restrict
```

Then rerun the nested probe from [Check your host](#check-your-host); it should
print `nested OK`, and Merry starts normally.

What this changes: only the exec of `/usr/bin/bwrap` from inside a sandbox is
treated differently. The nested `bwrap` gets the same profile as the first one,
and the commands it runs land in `bwrap//&unpriv_bwrap` again, so they stay
capability-stripped exactly like before. Everything else Ubuntu's restriction
covers is unchanged, and `kernel.apparmor_restrict_unprivileged_userns` stays
at `1`. Because the rule lives in the `local/` override files, package upgrades
do not remove it.

This procedure was verified on Ubuntu 25.04 (AppArmor 4.1, bubblewrap 0.11).
The `priority=` keyword needs AppArmor 4.1 or newer; check with
`apparmor_parser --version`. Ubuntu 24.04 ships AppArmor 4.0, where the rule
has not been tested. If `apparmor_parser -r` rejects it there, use one of the
alternatives below.

#### Alternatives

- `merry --inner-sandbox`: one action sandbox without the outer layer. The
  action policy (workspace writes, network, path review) still applies, but the
  outer path ceiling does not. This works on a stock Ubuntu host because it
  needs only a single bubblewrap layer.
- `merry --no-sandbox`: no bubblewrap at all. Process actions inherit the host
  filesystem, environment, and permissions.
- Relaxing the whole system is **not recommended**: setting
  `kernel.apparmor_restrict_unprivileged_userns=0` **and** disabling the bwrap
  profile (`sudo apparmor_parser -R /etc/apparmor.d/bwrap-userns-restrict`)
  makes nesting work but removes Ubuntu's user-namespace mitigation for every
  program. Disabling the profile without also changing the sysctl breaks even a
  single bubblewrap layer (`bwrap: setting up uid map: Permission denied`).

### Kernel restrictions

These block even a single sandbox. Merry reports them as a missing inner
sandbox after the first probe.

- `kernel.unprivileged_userns_clone` (Debian 10 and older, some hardened
  kernels): set it to `1`, for example with a file in `/etc/sysctl.d/`.
- `user.max_user_namespaces`: must be greater than `0`.
- Custom kernels need `CONFIG_USER_NS=y`; the other namespace options
  (`CONFIG_PID_NS`, `CONFIG_IPC_NS`, `CONFIG_UTS_NS`, `CONFIG_NET_NS`) are
  needed for the corresponding isolation features.
- Containers (Docker, LXC, CI runners) often forbid nested user namespaces by
  seccomp or by running without `CAP_SYS_ADMIN`; use `--inner-sandbox` or
  `--no-sandbox` there, or run the container with the required permissions.

## Configure

Merry reads these settings from `config.toml` and from the matching command-line
flags. Sandbox mode and approval policy are independent: the sandbox sets the
execution boundary, and the approval policy sets who reviews permission requests
inside it, so any policy combines with any sandbox mode. The
[README](README.md#sandbox-and-permissions) keeps the short version.

### Sandbox mode

| Flag | `[cli] sandbox` | Execution boundary |
|---|---|---|
| `--with-sandbox` | `"with-sandbox"` | outer+inner bubblewrap, the built-in default for the TUI and `merry run` |
| `--inner-sandbox` | `"inner-sandbox"` | the Codex-compatible single inner action sandbox, without the outer layer |
| `--no-sandbox` | `"no-sandbox"` | unrestricted host execution: actions inherit the host filesystem, environment, and permissions without any bubblewrap namespace |

```toml
[cli]
sandbox = "with-sandbox"
```

A sandbox mode given on the command line replaces the configured one. Merry does
not downgrade on its own, so a host that cannot nest bubblewrap needs the host
fix in
[Ubuntu 24.04 and newer](#ubuntu-2404-and-newer-the-bubblewrap-apparmor-profile)
or [Kernel restrictions](#kernel-restrictions), or one of the opt-outs above.

### Approval policy

`--approval-policy` names who reviews permission requests before a command runs,
and `[cli] approval_policy` sets the default for every TUI and `merry run`
session:

```toml
[cli]
approval_policy = "model_then_human"
```

| Value | Reviewer |
|---|---|
| `none` | nobody: configured actions run without an approval round |
| `deny` | nobody: every permission request is rejected without asking |
| `model_only` | the `[models.approval_review]` model decides; no human fallback |
| `model_then_human` (default) | the model decides first; when it denies or cannot decide, you are asked in the TUI dialog or on the `merry run` prompt |
| `human_only` | you decide, in the TUI dialog or on the `merry run` prompt |

A flag for one dimension replaces only that dimension's configured default:
`--with-sandbox` keeps a configured `approval_policy`, and `--approval-policy`
keeps a configured `sandbox`. `--approval-policy none` skips model and host
permission review for configured actions; `--approval-policy deny` is the
opposite end, where actions that need no approval still run but every permission
request and high-risk action review is denied without consulting a reviewer.
Neither policy changes any access ceiling: `deny_paths`, `review_paths` masking,
and `network = false` still apply.

### Permissions

`[permissions]` declares the host capabilities that actions may use:

```toml
[permissions]
# Inner action network capability ceiling; defaults to true.
network = true

# Preauthorized in the inner sandbox at the declared access level, and the
# outer access ceiling for the session.
readonly_paths = ["/etc", "/var/log", "company-readonly"]
readwrite_paths = ["company-work"]

# Existing subtrees within those grants that require per-action review.
review_paths = []
deny_paths = ["~/.ssh"]

# Host integrations: forwarded and preauthorized when the endpoint exists.
ssh_agent = true
gpg_agent = true
dbus = true

# Environment assignments for Merry-managed action processes only.
# environment = [{ name = "RUSTUP_TOOLCHAIN", value = "stable" }]
```

- `network = true` allows network requests, not network access: ordinary actions
  stay network-isolated and each one still has to request and obtain its own
  approval. `network = false` rejects network requests before review or
  execution, including under the `none` approval policy, and approval cannot
  override the ceiling. The setting is inherited by new process sessions, does
  not restrict model-provider or configured MCP connections, and does not add
  network isolation to explicit `--no-sandbox` host execution.
- `readonly_paths` and `readwrite_paths` are preauthorized in the inner sandbox
  at their declared access level. `review_paths` marks existing subtrees within
  these grants for **per-action** review, without adding a new grant or raising
  the access ceiling. With `readonly_paths = ["/abc"]` and
  `review_paths = ["/abc/d"]`, `/abc/e` remains readable; `/abc/d` is masked
  until the action explicitly requests that path or a specific descendant
  through `run_process.permissions` or `request_permissions`. Approval stays
  read-only and is not reused by later actions. A broader parent grant does not
  unlock a separately reviewed child; nested review markers and `deny_paths`
  still apply. Denied paths cannot be approved. Merry's configuration, state
  directories, and configured provider credential files are product-private and
  are not exposed to task processes merely because Merry itself needs them.
- `ssh_agent`, `gpg_agent`, and `dbus` each enable outer-sandbox forwarding and
  preauthorize the matching inner capability: when the validated endpoint
  exists, ordinary actions use the forwarded socket and automatically imported
  client files without a separate request. `review_paths` still mask a
  configured endpoint until its exact path is approved for that action, and
  `deny_paths` always mask it. An integration that is not enabled here can still
  be requested for one permissioned action when its endpoint is visible, and a
  missing agent does not prevent ordinary actions from starting.
- `environment` assignments are injected into Merry-managed action processes
  after the inner sandbox defaults, so they may intentionally override `PATH`,
  `HOME`, `TMPDIR`, or `PWD`. They do not change the outer bootstrap or provider
  environment, and filesystem access is still governed by the path rules above.

## How it works

### Sandbox layers

Both inner modes start from a read-only view of their parent filesystem, so
ordinary commands can see host configuration and toolchains. The inner action
policy controls workspace writes, network access, path review, and modeled host
integrations (SSH agent, native GPG agent, and D-Bus session bus). It masks known
unapproved socket endpoints, including endpoints under `/tmp`, rather than
hiding their entire shared parent directory. It is not a general IPC filter:
unmodeled Unix sockets visible through the inherited filesystem may remain
reachable even with read-only mounts and network isolation. In the default
outer+inner mode, the outer sandbox limits which host paths are visible before
the inner action starts.

In the normal mode, the outer `/tmp` is a session-scoped in-memory tmpfs reused
by action sandboxes. With `--no-sandbox`, action `/tmp` maps to the current
process's validated `TMPDIR` directly. Debug commands remain unsandboxed unless
`--with-sandbox` is supplied.

### Mount plan and scanning

Outer filesystem mounts are applied parent-first after resolving access-rule
precedence. Explicit file and directory imports preserve symbolic links instead
of replacing them with their target's contents. Trusted and integration directory
imports inspect only their direct children for link dependencies. Ordinary child
directories remain covered by the whole-directory bind and are not traversed.
Already-visible targets reuse their effective mount without further scanning;
missing targets are added only from an already-admitted
host source, without importing a file's whole parent directory. Directory aliases
retain descendant denials and read-only restrictions. External targets outside
the admitted source scopes still require an explicit path grant.

Newly added directory targets receive the same one-level inspection. Deeper links
inside ordinary subdirectories are not proactively discovered; explicitly imported
subdirectories can be inspected independently. SSH Include discovery still follows
referenced configuration paths within the admitted view.

Directory scanning is bounded (250,000 entries, 8,192 mounts or aliases, depth
128, and 10 seconds). Exceeding a bound fails preparation; dangling links, cycles
among directory entries, and inaccessible entries retain their unavailable
behavior. Explicit cyclic mount destinations fail preparation. Runtime-provided
`/proc` and `/dev` are not recursively scanned or resolved through host process
IDs. `/usr` remains one read-only directory import, not a per-file mount tree;
base system imports and workspace/development trees are not automatic scan roots.
Inner action sandboxes inherit these system mounts
instead of repeating them; workspace overlays, read-only `.git` metadata,
reviewed grants, network isolation, and host-integration controls still apply.
Optional development paths such as `.rustup/toolchains` remain built in, but
missing sources are skipped without creating directories beneath a read-only
HOME. This applies to both inner-only and outer+inner execution.

### Enforcement

Enforcement uses mount namespaces, not shell-path parsing: scripts receive the
same restricted view. Directories are replaced with empty read-only mounts and
individual files with empty read-only placeholders; an empty result or missing
file inside the sandbox does not prove that the host path is absent. There is no
transparent retry or automatic elevation. Known symlink and inherited bind-mount
aliases receive the same restrictions. This is pathname isolation, not content
tracking: separately copied data and arbitrary hard-link aliases are not covered.
Missing or unprotectable review and explicit deny targets fail closed during
preflight, before any action starts. Create the configured target first or protect
an existing ancestor. Merry never creates host paths to install these masks;
optional, absent development mounts remain skippable.

### Host integration reachability

Path policy decides whether an enabled endpoint is reachable, not whether it is
announced. `SSH_AUTH_SOCK`, `DBUS_SESSION_BUS_ADDRESS`, and `GNUPGHOME` name the
configured endpoints for every action, while a `deny_paths` or `review_paths`
entry covering the socket, the keyring, or one of their parent directories masks
the mount itself. A broad deny therefore also hides the agent even though
`ssh_agent`/`gpg_agent`/`dbus` is enabled: the client sees the endpoint and fails
when it connects. Keep those denies narrow (or outside the endpoint tree) when the
integration should stay usable, and remember that `deny_paths` is never reopened
by an approval.

Native GPG socket discovery uses `gpgconf --list-dirs` and honors `GNUPGHOME`; it
does not assume sockets live in `.gnupg` or `/run`. Outer scaffolding preserves
private socket-directory permissions without importing their other contents.
Explicitly requesting an unavailable endpoint reports an error.

### SSH agent and configuration snapshots

Once the SSH integration is available, `~/.ssh/known_hosts` and
`known_hosts2` are exposed read-only to inner actions,
without granting access to private keys, `~/.ssh/config`, or the network. These
files still obey `deny_paths` and `review_paths`; an explicit deny of the entire
`.ssh` directory blocks them as well. Existing host identities can be checked,
but new or changed host keys are not automatically accepted and host trust files
are not made writable. No `StrictHostKeyChecking` or SSH configuration override
is injected.

Enabling `ssh_agent` imports `/etc/passwd` and `/etc/group` for client account
lookup, plus `/etc/ssh/ssh_config` and `/etc/ssh/ssh_config.d`, read-only. It does
not require granting the whole `/etc` directory or import SSH server
configuration, host private keys, or shadow password databases. These imports
obey explicit path denials; Include targets outside the existing sandbox view
still need an explicit `readonly_paths` grant. System SSH configuration already
exposed by filesystem rules remains available when `ssh_agent` is disabled.

Before entering a user namespace, Merry
validates regular configuration files against OpenSSH's owner/write-mode rules.
Safe root-owned files that would otherwise become UID 65534 are supplied as
unchanged, read-only private snapshots at their resolved sandbox destinations;
original symlinks remain in place and multiple aliases share one snapshot.
Ordinary mount coverage does not suppress these deliberate replacements. Snapshots travel
through sealed anonymous-memory FDs and are consumed by bubblewrap; the host
files are never modified. Snapshot mounts replace the corresponding original
file binds instead of stacking a second mount that inner actions cannot rebind.
Both the outer and inner mount plans preserve path denials and per-action review.
This does not import the whole `/etc/ssh` directory or make unsafe/unknown
ownership acceptable.

Snapshot discovery starts at `/etc/ssh/ssh_config` and follows static recursive
`Include` paths, quoted filenames, globs, and symlinks within the allowed view.
Connection-dependent percent tokens, environment/tilde expansion, and unsupported
Include patterns are left to OpenSSH and are not automatically snapshotted. This
is scoped SSH compatibility, not a global UID remapping for other programs.
Configuration copies last for the corresponding sandbox lifetime; restart the
outer sandbox to pick up host changes.

Bounded SSH discovery or read failures disable only this compatibility adaptation:
Merry reports a warning, preserves the original mounts, and lets unrelated actions
start. OpenSSH may still reject incompatible ownership; its security checks are
never disabled. Errors resolving the admitted path policy remain fatal.

### GPG agent and public-key stores

`gpg_agent = true` makes the conventional public-key stores available to inner
actions. When the integration is available, inner actions import `pubring.kbx` and
legacy `pubring.gpg` read-only, without additional `readonly_paths`. Each such
action gets a private, temporary `GNUPGHOME` view
at the original path for locks and a fresh trust database. These client writes
never modify the host keyring, even when the host directory is declared read-only.
Host private-key files, configuration, and trust state are not automatically
imported. Listing public keys and verifying signatures do not require a running
agent; a valid signature does not imply host ownertrust was imported. This
file-based integration does not yet support keyboxd databases: their presence
is reported explicitly rather than forwarding a writable host keyboxd interface
or presenting a stale file-based keyring.

Public-key sources still obey `deny_paths` and `review_paths`. A path grant does
not authorize an otherwise masked agent socket. If a socket is also under
`review_paths`, both the integration and the exact path approval are required.
GPG's extra/browser and keyboxd endpoints are not implicitly exposed. No host
keyring is made writable as a workaround.
Direct `merry-process` consumers supply discovered `GpgAgentSockets` explicitly;
the CLI performs discovery when preparing sandboxed process backends.

### Capability retention

Runtime owns the session capability store and retention decisions. Process adapters
consume read-only snapshots and report normalized path constraints; they do not
record grants. Each preparation captures a fresh mount-alias view, shared by path
review, masking, and client-resource discovery, rather than caching the filesystem
for an entire session. Trusted configuration flags are the preauthorized baseline
for paths and host integrations; capabilities approved through a request may be
retained within that session. Separately reviewed paths still require per-action
approval.

### Permission review without a terminal

Reading the task from stdin consumes that stream, so `merry run -` answers
permission review on the controlling terminal rather than on stdin. Piping
approval answers alongside the task does not work: they would be read as part
of the task. Under the default outer sandbox, whose `--new-session` leaves the
sandboxed process unable to open `/dev/tty`, a `run -` with a `model_then_human`
or `human_only` policy binds only the parent's terminal device into the sandbox
at `/dev/merry-review-tty` and reads answers there; tool processes never see
it. When the process has no controlling terminal at all, review has no way to
ask and each request is denied with that reason on stderr, so grant the
capabilities up front, pass the task on argv, or pick `model_only` or `deny`
when a piped run needs approvals without a terminal.
