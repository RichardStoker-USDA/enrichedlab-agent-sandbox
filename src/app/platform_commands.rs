//! The platform, catalog, audit and PKI commands.
use super::*;

pub(super) fn platform_command(
    config: &RuntimeConfig,
    output: OutputFormat,
    command: PlatformCommand,
) -> Result<()> {
    let settings = config.load_settings()?;
    match command {
        PlatformCommand::List => {
            let mut gateways = BTreeMap::new();
            for (name, profile) in &settings.gateway_profiles {
                let health = if profile.enabled {
                    match platform::observe_gateway(config, name, profile) {
                        Ok(value) => serde_json::to_value(value)?,
                        Err(error) => {
                            json!({"passed": false, "error": error.to_string()})
                        }
                    }
                } else {
                    json!({"passed": false, "disabled": true})
                };
                gateways.insert(name.clone(), json!({"profile": profile, "health": health}));
            }
            print_value(
                output,
                &json!({
                    "default_gateway_profile": settings.default_gateway_profile,
                    "gateways": gateways,
                    "runtime_proofs": platform::load_proofs(config)?,
                    "sync_reports": workspace_sync::load_latest_reports(config)?
                }),
                "Platform profiles",
            )
        }
        PlatformCommand::Inspect { profile: name } => {
            let profile = settings.gateway_profile(&name)?;
            let health = platform::observe_gateway(config, &name, profile)?;
            let sandboxes = state::list(config)?
                .into_iter()
                .filter(|record| record.runtime.gateway_profile == name)
                .collect::<Vec<_>>();
            print_value(
                output,
                &json!({"profile_name": name, "profile": profile, "health": health, "sandboxes": sandboxes}),
                "Platform profile",
            )
        }
        PlatformCommand::Refresh { sandbox } => {
            let records = match sandbox {
                Some(name) => vec![state::load(config, &name)?],
                None => state::list(config)?,
            };
            let mut proofs = BTreeMap::new();
            let mut failures = BTreeMap::new();
            for record in records {
                match platform::observe(config, &record) {
                    Ok(proof) => {
                        proofs.insert(record.name, proof);
                    }
                    Err(error) => {
                        failures.insert(record.name, error.to_string());
                    }
                }
            }
            print_value(
                output,
                &json!({"proofs": proofs, "failures": failures}),
                if failures.is_empty() {
                    "Runtime proofs refreshed"
                } else {
                    "Runtime proof refresh completed with failures"
                },
            )?;
            if !failures.is_empty() {
                bail!("one or more runtime proof refreshes failed");
            }
            Ok(())
        }
        PlatformCommand::Configure(args) => {
            if args.driver != platform::RuntimeAdapter::Kubernetes
                && (args.runtime_class.is_some()
                    || args.kubeconfig.is_some()
                    || args.context.is_some()
                    || args.kubectl_bin.is_some()
                    || args.enable_peer_policies
                    || args.peer_gateway_ip.is_some())
            {
                bail!("Kubernetes observer options require the Kubernetes driver");
            }
            let kubeconfig = if let Some(kubeconfig) = &args.kubeconfig {
                if !kubeconfig.is_absolute() {
                    bail!("kubeconfig must be an absolute path");
                }
                let canonical = fs::canonicalize(kubeconfig)
                    .with_context(|| format!("missing kubeconfig {}", kubeconfig.display()))?;
                if !canonical.is_absolute() || !canonical.is_file() {
                    bail!("kubeconfig must resolve to a regular file");
                }
                Some(canonical)
            } else {
                None
            };
            let kubectl_bin = if args.driver == platform::RuntimeAdapter::Kubernetes {
                let kubectl_bin = args
                    .kubectl_bin
                    .as_ref()
                    .context("Kubernetes profiles require --kubectl-bin with an absolute path")?;
                if !kubectl_bin.is_absolute() {
                    bail!("kubectl binary must be an absolute path");
                }
                let canonical = fs::canonicalize(kubectl_bin)
                    .with_context(|| format!("missing kubectl binary {}", kubectl_bin.display()))?;
                if !canonical.is_absolute() || !canonical.is_file() {
                    bail!("kubectl binary must resolve to a regular file");
                }
                Some(canonical.to_string_lossy().into_owned())
            } else {
                None
            };
            let in_use = state::list(config)?
                .into_iter()
                .filter(|record| record.runtime.gateway_profile == args.name)
                .map(|record| record.name)
                .collect::<Vec<_>>();
            let replaced = settings.gateway_profiles.contains_key(&args.name);
            if replaced {
                if !args.replace {
                    bail!(
                        "gateway profile {} exists; use --replace after checking its sandboxes",
                        args.name
                    );
                }
                if !in_use.is_empty() {
                    bail!(
                        "gateway profile {} is frozen by managed sandboxes: {}",
                        args.name,
                        in_use.join(", ")
                    );
                }
            }
            let workspace_transport = match args.driver {
                platform::RuntimeAdapter::Docker | platform::RuntimeAdapter::Podman => {
                    platform::WorkspaceTransport::Bind
                }
                platform::RuntimeAdapter::Kubernetes | platform::RuntimeAdapter::Vm => {
                    platform::WorkspaceTransport::Sync
                }
            };
            let observer = (args.driver == platform::RuntimeAdapter::Kubernetes).then(|| {
                platform::KubernetesObserver {
                    transport: platform::ObserverTransport::Local,
                    command: vec![
                        kubectl_bin
                            .clone()
                            .expect("Kubernetes observer binary was validated"),
                    ],
                    namespace: args.namespace.clone(),
                    kubeconfig: kubeconfig.clone(),
                    context: args.context.clone(),
                    ssh_bin: PathBuf::from("ssh"),
                    ssh_host: None,
                    manage_peer_network_policies: args.enable_peer_policies,
                    peer_gateway_ip: args.peer_gateway_ip,
                }
            });
            let profile = platform::GatewayProfile {
                enabled: true,
                required: args.required,
                openshell_gateway: args.gateway,
                driver: args.driver,
                workspace_transport,
                bind_mounts_enabled: workspace_transport == platform::WorkspaceTransport::Bind,
                sync_max_files: args.sync_max_files,
                sync_max_bytes: args.sync_max_bytes,
                kubernetes: platform::KubernetesProfile {
                    runtime_class_name: args.runtime_class,
                    node_selector: BTreeMap::new(),
                    tolerations: Vec::new(),
                    priority_class_name: None,
                },
                observer,
            };
            profile.validate(&args.name)?;
            let mut updated = settings;
            updated
                .gateway_profiles
                .insert(args.name.clone(), profile.clone());
            if args.set_default {
                updated.default_gateway_profile = args.name.clone();
            }
            config.save_settings(&updated)?;
            audit::append(
                config,
                "host-controller",
                "platform.configure",
                &format!("gateway-profile/{}", args.name),
                "passed",
                json!({
                    "driver": profile.driver,
                    "workspace_transport": profile.workspace_transport,
                    "required": profile.required,
                    "set_default": args.set_default,
                    "replaced": replaced
                }),
            )?;
            print_value(
                output,
                &json!({"profile_name": args.name, "profile": profile}),
                "Gateway profile configured",
            )
        }
        PlatformCommand::SetDefault { profile } => {
            settings.gateway_profile(&profile)?;
            let mut updated = settings;
            updated.default_gateway_profile = profile.clone();
            config.save_settings(&updated)?;
            audit::append(
                config,
                "host-controller",
                "platform.set-default",
                &format!("gateway-profile/{profile}"),
                "passed",
                json!({}),
            )?;
            print_value(
                output,
                &json!({"default_gateway_profile": profile}),
                "Default gateway profile changed",
            )
        }
        PlatformCommand::Remove { profile } => {
            if profile == settings.default_gateway_profile {
                bail!("select another default profile before removing {profile}");
            }
            if !settings.gateway_profiles.contains_key(&profile) {
                bail!("unknown gateway profile {profile}");
            }
            let in_use = state::list(config)?
                .into_iter()
                .filter(|record| record.runtime.gateway_profile == profile)
                .map(|record| record.name)
                .collect::<Vec<_>>();
            if !in_use.is_empty() {
                bail!(
                    "gateway profile {profile} is frozen by managed sandboxes: {}",
                    in_use.join(", ")
                );
            }
            let mut updated = settings;
            updated.gateway_profiles.remove(&profile);
            updated.validate()?;
            config.save_settings(&updated)?;
            audit::append(
                config,
                "host-controller",
                "platform.remove",
                &format!("gateway-profile/{profile}"),
                "passed",
                json!({}),
            )?;
            print_value(
                output,
                &json!({"removed": profile}),
                "Unused gateway profile removed",
            )
        }
    }
}

pub(super) fn catalog_command(
    config: &RuntimeConfig,
    output: OutputFormat,
    command: CatalogCommand,
) -> Result<()> {
    match command {
        CatalogCommand::Add(args) => print_value(
            output,
            &catalog::add(
                config,
                &args.name,
                &args.kind,
                &args.path,
                args.entrypoint.as_deref(),
                args.source_sandbox.as_deref(),
                args.team.as_deref(),
            )?,
            "Catalog entry added",
        ),
        CatalogCommand::List => print_value(output, &catalog::list(config)?, "Tool catalog"),
        CatalogCommand::Verify { name } => print_value(
            output,
            &catalog::verify(config, name.as_deref())?,
            "Catalog verification passed",
        ),
        CatalogCommand::Remove { name } => print_value(
            output,
            &catalog::remove(config, &name)?,
            "Catalog entry removed; artifact kept",
        ),
    }
}

pub(super) fn audit_command(
    config: &RuntimeConfig,
    output: OutputFormat,
    command: AuditCommand,
) -> Result<()> {
    match command {
        AuditCommand::Verify => {
            let records = audit::verify(config)?;
            print_value(
                output,
                &json!({"valid": true, "records": records.len(), "path": audit::log_path(config)}),
                "Audit chain valid",
            )
        }
        AuditCommand::Tail { count } => {
            print_value(output, &audit::tail(config, count)?, "Audit events")
        }
        AuditCommand::Snapshot { sandbox } => print_value(
            output,
            &evidence::snapshot(config, &sandbox)?,
            "Evidence snapshot captured",
        ),
        AuditCommand::Isolation { sandbox } => print_value(
            output,
            &evidence::isolation(config, &sandbox)?,
            "Isolation probe passed",
        ),
    }
}

pub(super) fn pki_command(
    config: &RuntimeConfig,
    output: OutputFormat,
    command: PkiCommand,
) -> Result<()> {
    match command {
        PkiCommand::Init => print_value(output, &pki::init(config)?, "Local CA initialized"),
        PkiCommand::Issue { name } => print_value(
            output,
            &pki::issue_client(config, &name)?,
            "Client profile issued; credential values remain in the listed owner-only files",
        ),
        PkiCommand::List => print_value(output, &pki::load_index(config)?, "Client profiles"),
        PkiCommand::Revoke { name } => {
            let profile = pki::revoke_client(config, &name)?;
            let active: Vec<String> = lease::list(config)?
                .into_iter()
                .filter(|item| {
                    item.client_profiles.contains(&name)
                        && matches!(
                            item.state,
                            lease::LeaseState::Active | lease::LeaseState::Starting
                        )
                })
                .map(|item| item.id)
                .collect();
            for id in active {
                lease::close(config, &id, "client certificate revoked")?;
            }
            audit::append(
                config,
                "host-controller",
                "pki.revoke",
                &format!("pki/client/{name}"),
                "passed",
                json!({}),
            )?;
            print_value(
                output,
                &profile,
                "Client profile revoked and dependent leases closed",
            )
        }
    }
}
