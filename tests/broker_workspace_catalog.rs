use enrichedlab_agent_sandbox::broker::{
    model::{AdministrationView, GroupView, MeView, PrincipalView, ProfileView},
    workspace_catalog::WorkspaceCatalog,
};
use serde_json::{Value, json};
use uuid::Uuid;

fn identity() -> MeView {
    MeView {
        principal: PrincipalView {
            id: Uuid::from_u128(1),
            email: "researcher@example.invalid".into(),
            display_name: None,
        },
        groups: vec![GroupView {
            key: "lab-a".into(),
            name: "Lab A".into(),
            role: "researcher".into(),
            max_active_agents: 4,
            max_active_agents_per_principal: 2,
            max_queued_tasks: 10,
            retention_days: 30,
            profiles: vec!["reviewed-r".into()],
            lab_data: false,
        }],
        profiles: vec![ProfileView {
            key: "reviewed-r".into(),
            name: "Reviewed R method".into(),
            description: "A registered method".into(),
            tool_bundle: "r-reference".into(),
            max_run_seconds: 120,
            offered: true,
            in_runs: true,
        }],
        administration: AdministrationView {
            project_inspection: false,
        },
    }
}
fn config() -> Value {
    json!({"schema_version":1,"entries":[{
        "key":"reference-study","name":"Reference study","description":"Public reference data with reviewed R methods.",
        "group":"lab-a","principals":[Uuid::from_u128(1)],"allowed_profiles":["reviewed-r"],
        "workspace":{"schema_version":1,"profile":"reviewed-r","image_digest":format!("sha256:{}","a".repeat(64)),
            "source_digest":format!("sha256:{}","b".repeat(64)),"input_refs":["input://reference/study"],
            "tool_refs":["tool://reviewed-r/1"],"preparation_receipt":null}
    }]})
}
fn parse(value: &Value) -> anyhow::Result<WorkspaceCatalog> {
    WorkspaceCatalog::parse(&serde_json::to_vec(value).unwrap())
}

#[test]
fn choices_require_project_group_principal_and_all_method_permissions() {
    let catalog = parse(&config()).unwrap();
    let mut user = identity();
    assert_eq!(catalog.available("lab-a", &user).len(), 1);
    assert!(catalog.available("lab-b", &user).is_empty());
    user.principal.id = Uuid::from_u128(2);
    assert!(catalog.available("lab-a", &user).is_empty());
    user = identity();
    user.groups.clear();
    assert!(catalog.available("lab-a", &user).is_empty());
    user = identity();
    user.profiles.clear();
    assert!(catalog.available("lab-a", &user).is_empty());
}

#[test]
fn initial_question_binds_owned_references_without_claiming_prepared_data() {
    let catalog = parse(&config()).unwrap();
    let user = identity();
    let entry = catalog.available("lab-a", &user)[0];
    let state = entry.initial_state(
        "Compare the groups".into(),
        vec!["Keep experimental units separate".into()],
    );
    state.validate().unwrap();
    assert_eq!(state.workspace.input_refs, vec!["input://reference/study"]);
    assert!(state.workspace.preparation_receipt.is_none());
    assert!(state.plan.is_empty() && state.lessons.is_empty());
    let view = serde_json::to_value(entry.choice()).unwrap();
    assert!(view.get("principals").is_none() && view.get("workspace").is_none());
}

#[test]
fn catalog_rejects_invalid_pins_fabricated_preparation_and_ambiguous_keys() {
    let mut bad = config();
    bad["entries"][0]["workspace"]["image_digest"] = "latest".into();
    assert!(parse(&bad).is_err());
    let mut bad = config();
    bad["entries"][0]["workspace"]["preparation_receipt"] = "receipt://not-executed".into();
    assert!(parse(&bad).is_err());
    let mut bad = config();
    let duplicate = bad["entries"][0].clone();
    bad["entries"].as_array_mut().unwrap().push(duplicate);
    assert!(parse(&bad).is_err());
    let mut bad = config();
    bad["entries"][0]["workspace"]["input_refs"] = json!([]);
    assert!(parse(&bad).is_err());
}

#[test]
fn private_catalog_file_rejects_symlinks_and_shared_permissions() {
    use std::os::unix::fs::{PermissionsExt, symlink};
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("catalog.json");
    std::fs::write(&path, serde_json::to_vec(&config()).unwrap()).unwrap();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();
    assert!(WorkspaceCatalog::load(&path).is_ok());
    let link = root.path().join("link.json");
    symlink(&path, &link).unwrap();
    assert!(WorkspaceCatalog::load(&link).is_err());
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();
    assert!(WorkspaceCatalog::load(&path).is_err());
}

/// Three entries in one lab: two that share a workspace image, as FoodData Central and FNDDS do, and one that does not.
fn sets() -> Value {
    let entry = |key: &str,
                 image: &str,
                 source: &str,
                 inputs: Value,
                 tools: Value,
                 profiles: Value| {
        json!({"key":key,"name":format!("Set {key}"),"description":format!("What set {key} holds."),"group":"lab-a",
            "principals":null,"allowed_profiles":profiles,
            "workspace":{"schema_version":1,"profile":format!("profile-{key}"),"image_digest":format!("sha256:{}",image.repeat(64)),
                "source_digest":format!("sha256:{}",source.repeat(64)),"input_refs":inputs,"tool_refs":tools,
                "preparation_receipt":null}})
    };
    json!({"schema_version":1,"entries":[
        entry("foods", "a", "b", json!(["input://reference/foods"]), json!(["tool://sql/1"]), json!(["reviewed-r"])),
        entry("survey", "a", "c", json!(["input://reference/survey", "input://reference/foods"]),
            json!(["tool://sql/1", "tool://sql/2"]), json!(["reviewed-r", "reviewed-sql"])),
        entry("figures", "d", "e", json!(["input://reference/figures"]), json!(["tool://r/1"]), json!(["reviewed-r"])),
    ]})
}

#[test]
fn several_sets_combine_into_one_workspace_that_names_each() {
    use enrichedlab_agent_sandbox::broker::workspace_catalog::{
        CombineRefusal, combine, initial_state,
    };
    let catalog = parse(&sets()).unwrap();
    let mut user = identity();
    user.profiles.push(ProfileView {
        key: "reviewed-sql".into(),
        name: "Reviewed SQL".into(),
        description: "A registered query method".into(),
        tool_bundle: "sql-reference".into(),
        max_run_seconds: 120,
        offered: true,
        in_runs: true,
    });
    let entries = catalog.available("lab-a", &user);
    assert_eq!(entries.len(), 3);
    let pick = |keys: &[&str]| -> Vec<_> {
        keys.iter()
            .map(|k| *entries.iter().find(|e| e.key == *k).unwrap())
            .collect()
    };
    // The union of data, tools and methods, in the order chosen, with the first set's profile and digests.
    let (workspace, profiles) = combine(&pick(&["survey", "foods"])).unwrap();
    assert_eq!(
        workspace.input_refs,
        vec!["input://reference/survey", "input://reference/foods"]
    );
    assert_eq!(workspace.tool_refs, vec!["tool://sql/1", "tool://sql/2"]);
    assert_eq!(profiles, vec!["reviewed-r", "reviewed-sql"]);
    assert_eq!(workspace.profile, "profile-survey");
    assert_eq!(
        workspace.source_digest,
        format!("sha256:{}", "c".repeat(64))
    );
    let points: Vec<(&str, &str)> = workspace
        .starting_points
        .iter()
        .map(|p| (p.key.as_str(), p.name.as_str()))
        .collect();
    assert_eq!(
        points,
        vec![("survey", "Set survey"), ("foods", "Set foods")]
    );
    assert_eq!(
        workspace.starting_points[1].input_refs,
        vec!["input://reference/foods"]
    );
    let state = initial_state(
        workspace.clone(),
        "Compare what the foods hold".into(),
        vec![],
    );
    state.validate().unwrap();
    // A saved version names the sets; one set alone names none, as before.
    let saved = serde_json::to_value(&state).unwrap();
    assert_eq!(saved["workspace"]["starting_points"][0]["key"], "survey");
    let single = serde_json::to_value(entries[0].initial_state("One set".into(), vec![])).unwrap();
    assert!(single["workspace"].get("starting_points").is_none());
    // Sets that need different workspace images are refused, naming every set chosen; so are repeats and one set.
    assert_eq!(
        combine(&pick(&["foods", "figures"])).unwrap_err(),
        CombineRefusal::Unsupported(vec!["foods".into(), "figures".into()])
    );
    assert_eq!(
        combine(&pick(&["foods", "foods"])).unwrap_err(),
        CombineRefusal::Invalid
    );
    assert_eq!(
        combine(&pick(&["foods"])).unwrap_err(),
        CombineRefusal::Invalid
    );
}

#[test]
fn a_saved_version_with_sets_must_name_its_own_data() {
    use enrichedlab_agent_sandbox::broker::workspace_catalog::initial_state;
    let catalog = parse(&sets()).unwrap();
    let entries = catalog.available("lab-a", &identity());
    let valid = || {
        let mut state = initial_state(entries[0].workspace.clone(), "Question".into(), vec![]);
        let mut second = entries[0].workspace.clone();
        second.profile = "profile-other".into();
        state.workspace.starting_points = vec![
            enrichedlab_agent_sandbox::broker::project::StartingPoint {
                key: "foods".into(),
                name: "Foods".into(),
                description: "What foods hold.".into(),
                profile: state.workspace.profile.clone(),
                source_digest: state.workspace.source_digest.clone(),
                input_refs: state.workspace.input_refs.clone(),
                tool_refs: state.workspace.tool_refs.clone(),
            },
            enrichedlab_agent_sandbox::broker::project::StartingPoint {
                key: "other".into(),
                name: "Other".into(),
                description: "Another set.".into(),
                profile: second.profile,
                source_digest: second.source_digest,
                input_refs: state.workspace.input_refs.clone(),
                tool_refs: state.workspace.tool_refs.clone(),
            },
        ];
        state
    };
    valid().validate().unwrap();
    // One set alone is not a list of sets.
    let mut one = valid();
    one.workspace.starting_points.truncate(1);
    assert!(one.validate().is_err());
    // A set's data must be among the workspace's.
    let mut stray = valid();
    stray.workspace.starting_points[1].input_refs = vec!["input://reference/elsewhere".into()];
    assert!(stray.validate().is_err());
    // The first set is the one the workspace's profile names.
    let mut swapped = valid();
    swapped.workspace.starting_points.swap(0, 1);
    assert!(swapped.validate().is_err());
    // Keys are distinct.
    let mut twice = valid();
    twice.workspace.starting_points[1].key = "foods".into();
    assert!(twice.validate().is_err());
    // A catalog entry never declares sets of its own.
    let mut bad = sets();
    bad["entries"][0]["workspace"]["starting_points"] = json!([]);
    assert!(parse(&bad).is_ok(), "an empty list is the same as none");
    bad["entries"][0]["workspace"]["starting_points"] =
        serde_json::to_value(&valid().workspace.starting_points).unwrap();
    assert!(parse(&bad).is_err());
}
