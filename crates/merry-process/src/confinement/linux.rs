use super::SandboxLaunch;
use landlock::{
    ABI, Access, AccessFs, CompatLevel, Compatible, Ruleset, RulesetAttr, RulesetCreatedAttr,
    RulesetStatus,
};
use seccompiler::{
    SeccompAction, SeccompCmpArgLen, SeccompCmpOp, SeccompCondition, SeccompFilter, SeccompRule,
};
use std::collections::BTreeMap;

pub(super) fn probe() -> Result<(), String> {
    let ruleset = Ruleset::default()
        .handle_access(AccessFs::from_all(ABI::V1))
        .map_err(|error| error.to_string())?
        .create()
        .map_err(|error| error.to_string())?;
    drop(ruleset);
    Ok(())
}

pub(super) fn apply(launch: &SandboxLaunch) -> Result<(), String> {
    apply_landlock(launch)?;
    if !launch.network() {
        apply_network_seccomp()?;
    }
    Ok(())
}

fn apply_landlock(launch: &SandboxLaunch) -> Result<(), String> {
    let abi = ABI::V5;
    let ruleset = Ruleset::default()
        .handle_access(AccessFs::from_all(abi))
        .map_err(|error| error.to_string())?
        .create()
        .map_err(|error| error.to_string())?
        .set_compatibility(CompatLevel::BestEffort);
    let ruleset = add_paths(ruleset, &launch.read_only, AccessFs::from_read(abi))?;
    let ruleset = add_paths(ruleset, &launch.read_write, AccessFs::from_all(abi))?;
    let ruleset = add_paths(
        ruleset,
        &launch.traverse,
        landlock::BitFlags::from(AccessFs::Execute),
    )?;
    let status = ruleset
        .restrict_self()
        .map_err(|error| format!("Landlock restriction failed: {error}"))?;
    if status.ruleset == RulesetStatus::NotEnforced {
        return Err(
            "Landlock could not enforce the workspace sandbox. Pass --sandbox off to run without it."
                .to_owned(),
        );
    }
    Ok(())
}

fn add_paths(
    ruleset: landlock::RulesetCreated,
    paths: &[String],
    access: landlock::BitFlags<AccessFs>,
) -> Result<landlock::RulesetCreated, String> {
    ruleset
        .add_rules(landlock::path_beneath_rules(paths, access))
        .map_err(|error| format!("failed to add Landlock rules: {error}"))
}

fn apply_network_seccomp() -> Result<(), String> {
    let deny = |domain| -> Result<SeccompRule, String> {
        SeccompRule::new(vec![
            SeccompCondition::new(0, SeccompCmpArgLen::Dword, SeccompCmpOp::Eq, domain)
                .map_err(|error| error.to_string())?,
        ])
        .map_err(|error| error.to_string())
    };
    let mut rules = BTreeMap::new();
    rules.insert(
        libc::SYS_socket,
        vec![
            deny(libc::AF_INET as u64)?,
            deny(libc::AF_INET6 as u64)?,
            deny(libc::AF_PACKET as u64)?,
        ],
    );
    let arch = seccompiler::TargetArch::try_from(std::env::consts::ARCH)
        .map_err(|error| error.to_string())?;
    let filter = SeccompFilter::new(
        rules,
        SeccompAction::Allow,
        SeccompAction::Errno(libc::EPERM as u32),
        arch,
    )
    .map_err(|error| error.to_string())?;
    let program = seccompiler::BpfProgram::try_from(filter)
        .map_err(|error: seccompiler::BackendError| error.to_string())?;
    seccompiler::apply_filter(&program).map_err(|error| error.to_string())
}
