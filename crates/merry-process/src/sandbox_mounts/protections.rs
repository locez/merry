//! Project admitted restrictions across aliases after dependency access converges.

use super::{
    SandboxMountError,
    inputs::MountInput,
    namespace::{BindingOrigin, Namespace},
    with_suffix,
};
use merry_runtime::PathAccess;
use std::collections::BTreeSet;

pub(super) fn apply(namespace: &mut Namespace) -> Result<(), SandboxMountError> {
    let mut projections = Vec::new();
    let mut destinations = namespace
        .bindings()
        .iter()
        .filter(|binding| binding.access == PathAccess::Deny)
        .map(|binding| binding.destination.clone())
        .collect::<BTreeSet<_>>();
    for alias in namespace
        .bindings()
        .iter()
        .filter(|binding| binding.exists && binding.access != PathAccess::Deny)
    {
        for restriction in namespace
            .bindings()
            .iter()
            .filter(|binding| binding.origin == BindingOrigin::Admitted)
        {
            if restriction.access != PathAccess::Deny && alias.origin != BindingOrigin::Dependency {
                continue;
            }
            let Ok(relative) = restriction.source.strip_prefix(&alias.source) else {
                continue;
            };
            if relative.as_os_str().is_empty() && restriction.access != PathAccess::Deny {
                continue;
            }
            if !alias.directory && !relative.as_os_str().is_empty() {
                continue;
            }
            let destination = with_suffix(alias.destination.clone(), relative);
            if restriction.access == PathAccess::Deny && !destinations.insert(destination.clone()) {
                continue;
            }
            let access = if restriction.access == PathAccess::Deny {
                PathAccess::Deny
            } else if alias.access == PathAccess::ReadOnly {
                PathAccess::ReadOnly
            } else {
                restriction.access
            };
            projections.push((
                restriction.source.clone(),
                destination,
                access,
                restriction.optional,
            ));
            if projections.len() >= 8192 {
                return Err(SandboxMountError::Limit {
                    kind: "alias protection count",
                    path: alias.destination.clone(),
                });
            }
        }
    }
    for (source, destination, access, optional) in projections {
        namespace.add_protection(MountInput::new(&source, &destination, access, optional)?)?;
    }
    Ok(())
}
