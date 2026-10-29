//! Allow and deny rules, services, requests and proposals.
use super::*;

pub(super) fn allow(config: &RuntimeConfig, output: OutputFormat, args: AllowArgs) -> Result<()> {
    let mut record = state::load(config, &args.name)?;
    let previous = record.clone();
    let host = policy::validate_public_host(&args.host)?;
    let rule_name = policy::host_rule_name(&host);
    if record.read_hosts.contains(&host) || record.write_hosts.contains(&host) {
        let _ = openshell::remove_rule(&config.openshell_bin, &record, &rule_name);
    }
    let endpoint = format!(
        "{host}:443:{}:rest:enforce",
        if args.write { "full" } else { "read-only" }
    );
    let binaries = [
        "/usr/bin/curl",
        "/usr/bin/wget",
        "/usr/bin/git",
        "/usr/bin/node",
        "/usr/bin/python3",
        OPENCODE_BIN,
        PI_BIN,
        "/sandbox/.local/bin/**",
    ];
    openshell::add_endpoint(
        &config.openshell_bin,
        &record,
        &endpoint,
        &binaries,
        Some(&rule_name),
    )?;
    record.read_hosts.retain(|candidate| candidate != &host);
    record.write_hosts.retain(|candidate| candidate != &host);
    if args.write {
        record.write_hosts.push(host.clone());
    } else {
        record.read_hosts.push(host.clone());
    }
    if let Err(error) = save_policy_source(config, &record) {
        let _ = openshell::remove_rule(&config.openshell_bin, &record, &rule_name);
        let _ = save_policy_source(config, &previous);
        return Err(error).context("policy source update failed; live rule was removed");
    }
    audit::append(
        config,
        "host-controller",
        "network.allow",
        &format!("sandbox/{}/network/{host}", record.name),
        "passed",
        json!({"write": args.write, "rule": rule_name}),
    )?;
    print_policy_action(output, "allow", &record, &host, args.write)
}

pub(super) fn deny(config: &RuntimeConfig, output: OutputFormat, args: DenyArgs) -> Result<()> {
    let mut record = state::load(config, &args.name)?;
    let previous = record.clone();
    let host = policy::validate_public_host(&args.host)?;
    let was_write = record.write_hosts.contains(&host);
    if !was_write && !record.read_hosts.contains(&host) {
        bail!(
            "host {host} is not managed by this controller for {}",
            record.name
        );
    }
    let rule_name = policy::host_rule_name(&host);
    openshell::remove_rule(&config.openshell_bin, &record, &rule_name)?;
    record.read_hosts.retain(|candidate| candidate != &host);
    record.write_hosts.retain(|candidate| candidate != &host);
    if let Err(error) = save_policy_source(config, &record) {
        let endpoint = format!(
            "{host}:443:{}:rest:enforce",
            if was_write { "full" } else { "read-only" }
        );
        let binaries = [
            "/usr/bin/curl",
            "/usr/bin/wget",
            "/usr/bin/git",
            "/usr/bin/node",
            "/usr/bin/python3",
            OPENCODE_BIN,
            PI_BIN,
            "/sandbox/.local/bin/**",
        ];
        let _ = openshell::add_endpoint(
            &config.openshell_bin,
            &previous,
            &endpoint,
            &binaries,
            Some(&rule_name),
        );
        let _ = save_policy_source(config, &previous);
        return Err(error).context("policy source update failed; live rule was restored");
    }
    audit::append(
        config,
        "host-controller",
        "network.deny",
        &format!("sandbox/{}/network/{host}", record.name),
        "passed",
        json!({"rule": rule_name}),
    )?;
    print_policy_action(output, "deny", &record, &host, false)
}

pub(super) fn save_policy_source(config: &RuntimeConfig, record: &SandboxRecord) -> Result<()> {
    policy::sync_active(config, record)
}

pub(super) fn service(
    config: &RuntimeConfig,
    output: OutputFormat,
    command: ServiceCommand,
) -> Result<()> {
    match command {
        ServiceCommand::Open(args) => print_value(
            output,
            &lease::open_host(
                config,
                &args.sandbox,
                args.target_port,
                args.local_port,
                args.ttl_minutes,
                &args.label,
            )?,
            "Host service lease created",
        ),
        ServiceCommand::Lan(args) => print_value(
            output,
            &lease::open_lan(
                config,
                &args.host_lease_id,
                args.bind,
                args.local_port,
                &args.sources,
                &args.clients,
                args.ttl_minutes,
            )?,
            "LAN mTLS lease created",
        ),
        ServiceCommand::Peer(args) => print_value(
            output,
            &lease::open_peer(
                config,
                &args.source,
                &args.target,
                args.target_port,
                args.source_port,
                args.ttl_minutes,
            )?,
            "Peer lease created",
        ),
        ServiceCommand::List => print_value(output, &lease::list(config)?, "Service leases"),
        ServiceCommand::Close { lease_id } => print_value(
            output,
            &lease::close(config, &lease_id, "closed from CLI")?,
            "Service lease closed",
        ),
    }
}

pub(super) fn request_command(
    config: &RuntimeConfig,
    output: OutputFormat,
    command: RequestCommand,
) -> Result<()> {
    match command {
        RequestCommand::List { sandbox } => print_value(
            output,
            &request::list_pending(config, sandbox.as_deref())?,
            "Pending requests",
        ),
        RequestCommand::Get {
            sandbox,
            request_id,
        } => print_value(
            output,
            &request::get(config, &sandbox, &request_id)?,
            "Request",
        ),
        RequestCommand::Approve {
            sandbox,
            request_id,
            reason,
            actor,
        } => print_value(
            output,
            &request::approve(config, &sandbox, &request_id, &actor, reason.as_deref())?,
            "Request approved",
        ),
        RequestCommand::Reject {
            sandbox,
            request_id,
            reason,
            actor,
        } => print_value(
            output,
            &request::reject(config, &sandbox, &request_id, &actor, &reason)?,
            "Request rejected",
        ),
        RequestCommand::ProcessAuto => print_value(
            output,
            &request::process_auto(config)?,
            "Auto-safe decisions",
        ),
        RequestCommand::Schema => print_value(output, &request::schema(), "Request schema"),
    }
}

pub(super) fn proposal(
    config: &RuntimeConfig,
    output: OutputFormat,
    command: ProposalCommand,
) -> Result<()> {
    match command {
        ProposalCommand::Pending { sandbox } => {
            let record = state::load(config, &sandbox)?;
            print_text(
                output,
                &openshell::proposal_get(&config.openshell_bin, &record, Some("pending"))?,
                "pending",
            )
        }
        ProposalCommand::History { sandbox } => {
            let record = state::load(config, &sandbox)?;
            print_text(
                output,
                &openshell::proposal_history(&config.openshell_bin, &record)?,
                "history",
            )
        }
        ProposalCommand::Approve { sandbox, chunk_id } => {
            decide_proposal(config, output, &sandbox, &chunk_id, true, None)
        }
        ProposalCommand::Reject {
            sandbox,
            chunk_id,
            reason,
        } => decide_proposal(config, output, &sandbox, &chunk_id, false, Some(&reason)),
    }
}

pub(super) fn decide_proposal(
    config: &RuntimeConfig,
    output: OutputFormat,
    sandbox: &str,
    chunk_id: &str,
    approve: bool,
    reason: Option<&str>,
) -> Result<()> {
    let record = state::load(config, sandbox)?;
    let result =
        openshell::proposal_decide(&config.openshell_bin, &record, chunk_id, approve, reason)?;
    if approve {
        policy::sync_active(config, &record)
            .context("proposal was approved live, but its active policy could not be persisted")?;
    }
    audit::append(
        config,
        "host-cli",
        if approve {
            "proposal.approve"
        } else {
            "proposal.reject"
        },
        &format!("sandbox/{sandbox}/proposal/{chunk_id}"),
        "passed",
        json!({"reason": reason}),
    )?;
    print_text(
        output,
        &result,
        if approve { "approved" } else { "rejected" },
    )
}
