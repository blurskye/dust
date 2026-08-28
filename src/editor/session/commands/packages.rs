use super::*;

pub(super) fn register_commands(
    registry: &mut CommandRegistry<AppEffect>,
) -> Result<(), CommandError> {
    for (id, title, update) in [
        ("package.install", "Install verified package", false),
        ("package.update", "Update verified package", true),
    ] {
        register(
            registry,
            id,
            title,
            ContextRequirement::Application,
            ArgumentSchema::default()
                .field("package_manifest", ArgumentType::String, true)
                .field("plugin_manifest", ArgumentType::String, true)
                .field("component", ArgumentType::String, true),
            move |invocation| {
                Ok(AppEffect::PackageInstall {
                    package_manifest: PathBuf::from(string_arg(invocation, "package_manifest")?),
                    plugin_manifest: PathBuf::from(string_arg(invocation, "plugin_manifest")?),
                    component: PathBuf::from(string_arg(invocation, "component")?),
                    update,
                })
            },
        )?;
    }
    register(
        registry,
        "package.registry.search",
        "Search official package registry",
        ContextRequirement::Application,
        ArgumentSchema::default().field("query", ArgumentType::String, true),
        |invocation| {
            Ok(AppEffect::PackageRegistrySearch(
                string_arg(invocation, "query")?.into(),
            ))
        },
    )?;
    for (id, title, install) in [
        (
            "package.registry.show",
            "Show exact registry package",
            false,
        ),
        (
            "package.registry.install",
            "Install exact registry package",
            true,
        ),
    ] {
        register(
            registry,
            id,
            title,
            ContextRequirement::Application,
            ArgumentSchema::default()
                .field("publisher", ArgumentType::String, true)
                .field("name", ArgumentType::String, true)
                .field("version", ArgumentType::String, true),
            move |invocation| {
                let publisher = string_arg(invocation, "publisher")?.into();
                let name = string_arg(invocation, "name")?.into();
                let version = string_arg(invocation, "version")?.into();
                Ok(if install {
                    AppEffect::PackageRegistryInstall {
                        publisher,
                        name,
                        version,
                    }
                } else {
                    AppEffect::PackageRegistryShow {
                        publisher,
                        name,
                        version,
                    }
                })
            },
        )?;
    }
    register(
        registry,
        "package.list",
        "List installed packages",
        ContextRequirement::Application,
        ArgumentSchema::default(),
        |_| AppEffect::PackageList,
    )?;
    register(
        registry,
        "package.verify",
        "Verify package store",
        ContextRequirement::Application,
        ArgumentSchema::default(),
        |_| AppEffect::PackageVerify,
    )?;
    register(
        registry,
        "package.rollback",
        "Rollback package lock",
        ContextRequirement::Application,
        ArgumentSchema::default(),
        |_| AppEffect::PackageRollback,
    )?;
    register(
        registry,
        "package.remove",
        "Remove installed package",
        ContextRequirement::Application,
        ArgumentSchema::default().field("name", ArgumentType::String, true),
        |invocation| {
            Ok(AppEffect::PackageRemove(
                string_arg(invocation, "name")?.into(),
            ))
        },
    )?;
    register(
        registry,
        "package.gc",
        "Collect unreferenced package objects",
        ContextRequirement::Application,
        ArgumentSchema::default(),
        |_| AppEffect::PackageGarbageCollect,
    )?;
    register(
        registry,
        "plugin.disable",
        "Disable plugin",
        ContextRequirement::Application,
        ArgumentSchema::default().field("id", ArgumentType::String, true),
        |invocation| {
            Ok(AppEffect::PluginDisable(
                string_arg(invocation, "id")?.to_owned(),
            ))
        },
    )?;
    Ok(())
}
