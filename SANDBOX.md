# Sandbox host setup

Merry runs process actions inside [bubblewrap](https://github.com/containers/bubblewrap)
(`bwrap`). The TUI and `merry run` use two layers by default:

- an **outer CLI sandbox** that limits which host paths the session can see, and
- an **inner action sandbox**, started from inside the outer one, for each
  process action.

The inner layer therefore needs bubblewrap to run inside bubblewrap. Before the
outer sandbox starts, Merry probes exactly that. When the probe fails, startup
stops with a warning like this instead of starting a session whose every action
would fail:

```
warning: bubblewrap cannot start inside Merry's outer sandbox on this host, so every process action would fail; command permission approval cannot fix sandbox initialization.
  bwrap: No permissions to create new namespace, likely because the kernel does not allow non-privileged user namespaces. ...
  The bubblewrap AppArmor profile /etc/apparmor.d/bwrap-userns-restrict is installed. On Ubuntu 24.04 and newer it strips capabilities from nested bubblewrap even though the kernel allows unprivileged user namespaces.
  See SANDBOX.md (https://github.com/locez/merry/blob/main/SANDBOX.md) to allow nested bubblewrap, or rerun with --inner-sandbox to use a single action sandbox.
```

Merry never downgrades the sandbox mode on its own. Fix the host as described
below, or choose a weaker mode explicitly with `--inner-sandbox` (one action
sandbox, no outer layer) or `--no-sandbox` (no bubblewrap at all).

## Requirements

- `bwrap` on `PATH` (package `bubblewrap` on Debian, Ubuntu, Fedora, and Arch).
- Unprivileged user namespaces. Every mainstream distribution kernel allows
  them today; see [Kernel restrictions](#kernel-restrictions) for the sysctls
  that turn them off.
- Nested bubblewrap for the default outer+inner mode. Debian, Fedora, and Arch
  allow this out of the box. **Ubuntu 24.04 and newer block it by default**
  through AppArmor; see the next section.

## Check your host

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

## Ubuntu 24.04 and newer: the bubblewrap AppArmor profile

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

### Allow nested bubblewrap

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

### Alternatives

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

## Kernel restrictions

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
