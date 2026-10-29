//! The agent-state commands.
use super::*;

pub(super) fn agent_state_command(
    config: &RuntimeConfig,
    output: OutputFormat,
    command: AgentStateCommand,
) -> Result<()> {
    match command {
        AgentStateCommand::Init(args) => {
            if args.profile == "scientific-worker" {
                if args.domain != agent_state::StateDomain::Scientific {
                    bail!("the scientific-worker profile requires --domain scientific");
                }
                let objective = args
                    .objective
                    .as_deref()
                    .context("the scientific-worker profile requires --objective")?;
                let collections = agent_state::seed_scientific_worker(
                    config,
                    &args.run,
                    &args.subject,
                    objective,
                    args.team.as_deref(),
                )?;
                print_value(
                    output,
                    &json!({
                        "run": agent_state::load_run(config, &args.run)?,
                        "collections": collections
                    }),
                    "Agent state initialized",
                )
            } else {
                if args.objective.is_some() {
                    bail!("--objective is accepted only by the scientific-worker profile");
                }
                print_value(
                    output,
                    &agent_state::init_run(
                        config,
                        &args.run,
                        args.domain,
                        &args.profile,
                        &args.subject,
                        args.team.as_deref(),
                    )?,
                    "Agent state initialized",
                )
            }
        }
        AgentStateCommand::List => print_value(
            output,
            &agent_state::list_summaries(config)?,
            "Agent state runs",
        ),
        AgentStateCommand::View { run, subject } => print_value(
            output,
            &agent_state::generate_view(config, &run, &subject)?,
            "Prompt-facing state view",
        ),
        AgentStateCommand::Seed(args) => {
            let access = args
                .audiences
                .iter()
                .map(|audience| agent_state::CollectionAccess {
                    audience: *audience,
                    principals: args.principals.clone(),
                })
                .collect::<Vec<_>>();
            print_value(
                output,
                &agent_state::seed_collection_file(
                    config,
                    &args.run,
                    &args.subject,
                    &args.collection,
                    &args.schema,
                    &args.value_file,
                    &args.authorities,
                    &access,
                    args.prompt_eligible,
                )?,
                "State collection seeded",
            )
        }
        AgentStateCommand::Patch(args) => print_value(
            output,
            &agent_state::apply_mutation_file(config, &args.file, &args.actor, args.authority)?,
            "State mutation applied",
        ),
        AgentStateCommand::Event(args) => print_value(
            output,
            &agent_state::append_event_file(
                config,
                &args.run,
                &args.subject,
                &args.kind,
                &args.actor,
                args.authority,
                &args.payload_file,
            )?,
            "State event appended",
        ),
        AgentStateCommand::Artifact(args) => print_value(
            output,
            &agent_state::add_artifact(
                config,
                &args.run,
                &args.subject,
                &args.file,
                &args.media_type,
                &args.role,
                &args.label,
                args.sensitivity,
                &args.actor,
            )?,
            "Artifact recorded",
        ),
        AgentStateCommand::Context(args) => print_value(
            output,
            &agent_state::context_status(
                config,
                &args.profile,
                args.window_tokens,
                args.used_tokens,
                args.output_reserve_tokens,
            )?,
            "Context pressure",
        ),
        AgentStateCommand::Checkpoint(args) => print_value(
            output,
            &agent_state::checkpoint(config, &args.run, &args.subject, &args.reason, &args.actor)?,
            "State checkpoint written",
        ),
        AgentStateCommand::Seal(args) => print_value(
            output,
            &agent_state::seal_run(config, &args.run, &args.subject, &args.reason, &args.actor)?,
            "State run sealed",
        ),
        AgentStateCommand::Reconcile(args) => print_value(
            output,
            &agent_state::reconcile_outbox(config, &args.run, &args.subject, &args.workspace)?,
            "State outbox reconciled",
        ),
        AgentStateCommand::Trace { run } => print_value(
            output,
            &agent_state::trace(config, &run)?,
            "State event trace",
        ),
        AgentStateCommand::Export(args) => print_value(
            output,
            &agent_state::export_run(
                config,
                &args.run,
                &args.output_dir,
                args.include_restricted,
                args.include_event_payloads,
            )?,
            "Agent state run exported",
        ),
        AgentStateCommand::Verify => print_value(
            output,
            &agent_state::verify(config)?,
            "Agent state verified",
        ),
        AgentStateCommand::Profile { command } => match command {
            StateProfileCommand::List => print_value(
                output,
                &agent_state::list_profiles(config)?,
                "State profiles",
            ),
            StateProfileCommand::Install { file, replace } => print_value(
                output,
                &agent_state::install_profile(config, &file, replace)?,
                "State profile installed",
            ),
        },
        AgentStateCommand::Schema { command } => match command {
            StateSchemaCommand::List => print_value(
                output,
                &agent_state::list_schema_names(config)?,
                "State schemas",
            ),
            StateSchemaCommand::Install {
                name,
                file,
                replace,
            } => print_value(
                output,
                &json!({"name": agent_state::install_schema(config, &name, &file, replace)?}),
                "State schema installed",
            ),
        },
    }
}
