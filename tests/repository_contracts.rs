use std::{
    collections::BTreeSet,
    fs,
    os::unix::fs::{PermissionsExt, symlink},
    path::PathBuf,
    process::Command,
};
use walkdir::WalkDir;

fn repository_root() -> PathBuf {
    let root = std::env::current_dir().expect("read the Cargo test working directory");
    assert!(
        root.join("Cargo.toml").is_file() && root.join(".agents").is_dir(),
        "repository contract tests must run from the crate root: {}",
        root.display()
    );
    root
}

/// A module's source: its file and, once it is split into child modules, every file in its folder.
fn module_source(root: &std::path::Path, module: &str) -> String {
    let mut text = fs::read_to_string(root.join(format!("{module}.rs"))).unwrap();
    let folder = root.join(module);
    if folder.is_dir() {
        let mut files: Vec<PathBuf> = fs::read_dir(&folder)
            .unwrap()
            .map(|entry| entry.unwrap().path())
            .filter(|path| path.extension().is_some_and(|extension| extension == "rs"))
            .collect();
        files.sort();
        for path in files {
            text.push('\n');
            text.push_str(&fs::read_to_string(path).unwrap());
        }
    }
    text
}

#[test]
fn local_markdown_links_resolve() {
    let root = repository_root();
    let mut failures = Vec::new();

    for entry in WalkDir::new(&root).into_iter().filter_entry(|entry| {
        !matches!(
            entry.file_name().to_str(),
            Some(".git" | "target" | ".archive" | "node_modules")
        )
    }) {
        let path = entry.unwrap().into_path();
        if path.extension().and_then(|value| value.to_str()) != Some("md") {
            continue;
        }
        let body = fs::read_to_string(&path).unwrap();
        let mut remainder = body.as_str();
        while let Some((_, after_open)) = remainder.split_once("](") {
            let Some((raw_target, after_close)) = after_open.split_once(')') else {
                failures.push(format!("{}: unclosed Markdown link", path.display()));
                break;
            };
            remainder = after_close;
            let target = raw_target
                .trim()
                .trim_matches(['<', '>'])
                .split_whitespace()
                .next()
                .unwrap_or("");
            if target.is_empty()
                || target.starts_with('#')
                || target.contains("://")
                || target.starts_with("mailto:")
            {
                continue;
            }
            let target = target.split(['#', '?']).next().unwrap_or("");
            let resolved = path.parent().unwrap().join(target);
            if !resolved.exists() {
                failures.push(format!("{} -> {target}", path.display()));
            }
        }
    }

    assert!(
        failures.is_empty(),
        "local Markdown links do not resolve:\n{}",
        failures.join("\n")
    );
}

#[test]
fn public_entrypoints_use_the_release_product_and_setup_skill() {
    let root = repository_root();
    let readme = fs::read_to_string(root.join("README.md")).unwrap();
    let html = fs::read_to_string(root.join("web/index.html")).unwrap();
    let javascript = fs::read_to_string(root.join("web/app.js")).unwrap();
    let operations =
        fs::read_to_string(root.join("frontend/src/components/OperationsView.svelte")).unwrap();
    let provenance =
        fs::read_to_string(root.join("frontend/src/components/ProvenanceExplorer.svelte")).unwrap();

    assert!(readme.starts_with("# EnrichedLab Agent Sandbox\n"));
    assert!(!readme.contains("# OpenShell Agent Sandbox"));
    assert!(readme.contains(".agents/skills/enrichedlab-agent-setup/SKILL.md"));
    assert!(html.contains("id=\"app\""));
    assert!(readme.contains("--max-segments 8"));
    assert!(operations.contains("Research studies"));
    assert!(operations.contains("Open provenance replay"));
    assert!(provenance.contains("Provenance replay"));
    assert!(provenance.contains("Provider-returned reasoning"));
    assert!(javascript.contains("Provenance replay"));
    for field in [
        "max_output_files",
        "max_output_bytes",
        "route_context_limit",
        "route_output_limit",
        "route_pool_cap",
    ] {
        assert!(
            operations.contains(field) || javascript.contains(field),
            "dashboard does not show {field}"
        );
    }
}

#[test]
fn automatic_state_runtime_contract_is_documented_and_fail_closed() {
    let root = repository_root();
    let app = module_source(&root, "src/app");
    let research = module_source(&root, "src/research");
    let state_doc = fs::read_to_string(root.join("docs/agent-state.md")).unwrap();
    let release_doc = fs::read_to_string(root.join("docs/release.md")).unwrap();

    assert!(app.contains("observations.peak_context_tokens"));
    assert!(app.contains("worker.route_output_limit"));
    assert!(app.contains("research::event_stream_complete"));
    assert!(!app.contains("OsString::from(\"--continue\")"));
    assert!(!app.contains("OsString::from(\"--session\")"));
    for marker in [
        "agent.segment.started",
        "agent.tool.observed",
        "workspace.snapshot.captured",
        "peak_context_tokens",
        "reserved controller research log destination",
    ] {
        assert!(
            research.contains(marker),
            "research runtime is missing {marker}"
        );
    }
    assert!(state_doc.contains("Automatic event parsing supports pinned Pi and OpenCode"));
    assert!(release_doc.contains("Missing terminal usage"));
}

#[test]
fn scientific_worker_policy_can_bootstrap_the_pinned_runner() {
    let root = repository_root();
    let policy = fs::read_to_string(root.join("policies/data-science.yaml")).unwrap();
    let app = module_source(&root, "src/app");

    assert!(policy.contains("host: registry.npmjs.org, port: 443, protocol: rest, enforcement: enforce, access: read-only"));
    assert!(policy.contains("allow_encoded_slash: true"));
    assert!(policy.contains("path: /usr/bin/npm"));
    assert!(policy.contains("path: /usr/bin/node"));
    assert!(policy.contains("path: /sandbox/.local/pi/node_modules/.bin/pi"));
    assert!(app.contains("openshell::exec_capture("));
    assert!(app.contains("controller-bootstrap"));
}

#[test]
fn postgres_provenance_keeps_migration_and_runtime_authority_separate() {
    let root = repository_root();
    let migration =
        fs::read_to_string(root.join("migrations/postgres/001_provenance.sql")).unwrap();
    let grants = fs::read_to_string(root.join("migrations/postgres/grant_runtime.psql")).unwrap();
    let source = fs::read_to_string(root.join("src/provenance.rs")).unwrap();

    for marker in [
        "ENABLE ROW LEVEL SECURITY",
        "FORCE ROW LEVEL SECURITY",
        "current_setting('enrichedlab.tenant_id', true)",
        "provenance_events_no_update",
        "provenance_events_no_delete",
    ] {
        assert!(migration.contains(marker), "migration is missing {marker}");
    }
    for marker in [
        "NOSUPERUSER",
        "NOBYPASSRLS",
        "REVOKE CREATE ON SCHEMA enrichedlab_agent",
        "GRANT SELECT, INSERT",
    ] {
        assert!(
            grants.contains(marker),
            "runtime grant file is missing {marker}"
        );
    }
    assert!(!grants.contains("GRANT ALL"));
    assert!(source.contains("never falls back to the runtime identity"));
}

#[test]
fn remote_broker_keeps_the_cluster_boundary_fail_closed() {
    let root = repository_root();
    let api = fs::read_to_string(root.join("deploy/kubernetes/broker/workloads/api.yaml")).unwrap();
    let worker =
        fs::read_to_string(root.join("deploy/kubernetes/broker/workloads/worker.yaml")).unwrap();
    let service =
        fs::read_to_string(root.join("deploy/kubernetes/broker/workloads/service.yaml")).unwrap();
    let network =
        fs::read_to_string(root.join("deploy/kubernetes/broker/foundation/network-policy.yaml"))
            .unwrap();
    let example =
        fs::read_to_string(root.join("deploy/kubernetes/broker/example/kustomization.yaml"))
            .unwrap();
    let auth = fs::read_to_string(root.join("src/broker/auth.rs")).unwrap();
    let client = fs::read_to_string(root.join("src/broker/client.rs")).unwrap();
    let boundary = fs::read_to_string(root.join("scripts/scan_public_boundary.sh")).unwrap();

    for workload in [&api, &worker] {
        assert!(workload.contains("automountServiceAccountToken: false"));
        assert!(workload.contains("allowPrivilegeEscalation: false"));
        assert!(workload.contains("readOnlyRootFilesystem: true"));
        assert!(workload.contains("enrichedlab.ai/agent-control-node: selected"));
    }
    assert!(api.contains("name: stage-jwks-ca"));
    assert!(api.contains("install -m 0400 /staging/ca.pem /run/trust/jwks/ca.pem"));
    assert!(worker.contains("AGENT_SANDBOX_DOCKER_BIN"));
    assert!(worker.contains("value: /bin/false"));
    assert!(worker.contains("secretName: agent-broker-openshell-client"));
    assert!(service.contains("type: ClusterIP"));
    assert!(!service.contains("NodePort"));
    assert!(!service.contains("LoadBalancer"));
    assert!(network.contains("name: agent-broker-default-deny"));
    assert!(network.contains("name: agent-broker-api-ingress"));
    assert!(
        example.contains("sha256:0000000000000000000000000000000000000000000000000000000000000000")
    );
    assert!(example.contains("namespace: enrichedlab-agent-system"));
    assert!(example.contains("  - ../workloads"));
    assert!(!example.contains("  - ../base"));
    assert!(auth.contains("cf-access-jwt-assertion"));
    assert!(client.contains("const ACCESS_EDGE_TOKEN: &str = \"cf-access-token\""));
    assert!(client.contains("const MUTATION_INTENT: &str = \"x-agent-sandbox-intent\""));
    assert!(client.contains(".header(ACCESS_EDGE_TOKEN, token)"));
    assert!(boundary.contains("git ls-files --cached --others --exclude-standard"));
    assert!(boundary.contains("gitleaks stdin --redact --no-banner"));
}

#[test]
fn research_skill_matches_the_scientific_method_schema() {
    let root = repository_root();
    let protocol = fs::read_to_string(
        root.join(".agents/skills/enrichedlab-research-team/references/research_protocol.md"),
    )
    .unwrap();

    for marker in [
        "\"schema_version\": 2",
        "\"method\"",
        "\"quality_control\"",
        "\"llm_is_scientific_instrument\"",
        "llm-method",
    ] {
        assert!(
            protocol.contains(marker),
            "research protocol is missing {marker}"
        );
    }
}

#[test]
fn agent_skills_are_well_formed_and_focused() {
    let root = repository_root();
    let skills_root = root.join(".agents/skills");
    let expected = [
        ("enrichedlab-agent-setup", "references/setup_questions.md"),
        (
            "enrichedlab-agent-install",
            "references/platform_install.md",
        ),
        (
            "enrichedlab-agent-qualify",
            "references/qualification_matrix.md",
        ),
        (
            "enrichedlab-agent-operations",
            "references/service_and_approval.md",
        ),
        (
            "enrichedlab-agent-continuity",
            "references/logical_agent_lifecycle.md",
        ),
        (
            "enrichedlab-research-team",
            "references/research_protocol.md",
        ),
        ("enrichedlab-agent-state", "references/state_contract.md"),
        ("enrichedlab-agent-remote", "references/client_setup.md"),
        ("enrichedlab-agent-broker", "references/site_deployment.md"),
    ];
    let expected_names = expected
        .iter()
        .map(|(name, _)| name.to_string())
        .collect::<BTreeSet<_>>();
    let observed_names = fs::read_dir(&skills_root)
        .unwrap()
        .map(|entry| entry.unwrap().file_name().to_string_lossy().to_string())
        .collect::<BTreeSet<_>>();
    assert_eq!(observed_names, expected_names, "focused skill set drifted");

    for (name, reference) in expected {
        let directory = skills_root.join(name);
        let skill_path = directory.join("SKILL.md");
        assert!(skill_path.is_file(), "{name} has no uppercase SKILL.md");
        let file_names = fs::read_dir(&directory)
            .unwrap()
            .map(|entry| entry.unwrap().file_name().to_string_lossy().to_string())
            .collect::<BTreeSet<_>>();
        assert!(
            file_names.contains("SKILL.md") && !file_names.contains("skill.md"),
            "{name} has a lowercase skill filename"
        );
        let body = fs::read_to_string(&skill_path).unwrap();
        let frontmatter = body
            .strip_prefix("---\n")
            .and_then(|value| value.split_once("\n---\n"))
            .map(|(value, _)| value)
            .expect("skill frontmatter is malformed");
        assert!(
            frontmatter
                .lines()
                .any(|line| line == format!("name: {name}"))
        );
        assert!(
            frontmatter
                .lines()
                .any(|line| line.starts_with("description: ")),
            "{name} has no description"
        );
        for line in frontmatter.lines() {
            let key = line.split_once(':').map(|(key, _)| key).unwrap_or("");
            assert!(
                matches!(key, "name" | "description" | "license" | "metadata"),
                "{name} has unsupported frontmatter key {key}"
            );
        }
        assert!(
            directory.join(reference).is_file(),
            "{name} is missing {reference}"
        );
        assert!(
            body.contains(reference),
            "{name} does not route to {reference}"
        );
        let interface = fs::read_to_string(directory.join("agents/openai.yaml")).unwrap();
        assert!(interface.contains("display_name:"));
        assert!(interface.contains("short_description:"));
    }

    let setup = fs::read_to_string(skills_root.join("enrichedlab-agent-setup/SKILL.md")).unwrap();
    for routed in [
        "enrichedlab-agent-install",
        "enrichedlab-agent-qualify",
        "enrichedlab-agent-remote",
        "enrichedlab-agent-broker",
    ] {
        assert!(
            setup.contains(routed),
            "setup skill does not route to {routed}"
        );
    }
    assert!(
        !root
            .join("skills/openshell-agent-sandbox/SKILL.md")
            .exists(),
        "the prior monolithic skill still exists"
    );
}

#[test]
fn skill_installer_links_the_full_bundle_for_omp() {
    let root = repository_root();
    let temp = tempfile::tempdir().unwrap();
    let home = temp.path().join("home");
    let config = temp.path().join("config");
    let state = temp.path().join("state");
    fs::create_dir(&home).unwrap();

    let output = Command::new("sh")
        .arg(root.join("scripts/install_agent_skills.sh"))
        .arg("--all")
        .env("HOME", &home)
        .env("XDG_CONFIG_HOME", &config)
        .env("XDG_STATE_HOME", &state)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "skill installer failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    let omp_skills = home.join(".omp/agent/skills");
    for name in [
        "enrichedlab-agent-setup",
        "enrichedlab-agent-install",
        "enrichedlab-agent-qualify",
        "enrichedlab-agent-operations",
        "enrichedlab-agent-continuity",
        "enrichedlab-research-team",
        "enrichedlab-agent-state",
        "enrichedlab-agent-remote",
        "enrichedlab-agent-broker",
    ] {
        let installed = omp_skills.join(name);
        assert!(installed.is_symlink(), "OMP skill is not linked: {name}");
        assert_eq!(
            fs::canonicalize(installed).unwrap(),
            fs::canonicalize(root.join(".agents/skills").join(name)).unwrap(),
            "OMP skill target drifted: {name}"
        );
    }
}

#[test]
fn sandbox_image_does_not_claim_the_workspace_mount() {
    let root = repository_root();
    let containerfile = fs::read_to_string(root.join("images/Containerfile")).unwrap();

    assert!(
        !containerfile
            .lines()
            .any(|line| { line.trim().eq_ignore_ascii_case("WORKDIR /sandbox/work") }),
        "/sandbox/work is reserved for the OpenShell workspace mount"
    );
}

#[test]
fn active_ci_matches_the_reviewed_copy() {
    let root = repository_root();
    let active = fs::read(root.join(".github/workflows/ci.yml")).unwrap();
    let reviewed = fs::read(root.join("examples/github-actions/ci.yml")).unwrap();
    assert_eq!(active, reviewed, "active CI drifted from its reviewed copy");
}

#[test]
fn dashboard_mobile_contract_bounds_navigation_and_content() {
    let root = repository_root();
    let css = fs::read_to_string(root.join("frontend/src/styles.css")).unwrap();

    assert!(
        css.contains("min-width: 0;"),
        "dashboard content needs shrinkable grid items"
    );
    assert!(
        css.contains(".table-wrap { max-width: 100%; overflow: auto; }"),
        "wide tables need bounded scrolling"
    );
    assert!(
        css.contains(".sidebar nav { grid-template-columns: repeat(2, 1fr); }"),
        "narrow navigation must expose every section without horizontal scrolling"
    );
    assert!(
        css.contains(
            ".page-heading, .provenance-head { align-items: flex-start; flex-direction: column; }"
        ),
        "narrow headings must wrap inside the viewport"
    );
}

#[test]
fn dashboard_color_contract_uses_published_palette_and_accessible_pairs() {
    fn channel(value: &str) -> f64 {
        let value = u8::from_str_radix(value, 16).unwrap() as f64 / 255.0;
        if value <= 0.04045 {
            value / 12.92
        } else {
            ((value + 0.055) / 1.055).powf(2.4)
        }
    }

    fn luminance(value: &str) -> f64 {
        assert_eq!(value.len(), 7, "expected a six-digit hex color");
        0.2126 * channel(&value[1..3])
            + 0.7152 * channel(&value[3..5])
            + 0.0722 * channel(&value[5..7])
    }

    fn contrast(first: &str, second: &str) -> f64 {
        let first = luminance(first);
        let second = luminance(second);
        let (lighter, darker) = if first > second {
            (first, second)
        } else {
            (second, first)
        };
        (lighter + 0.05) / (darker + 0.05)
    }

    let root = repository_root();
    let css = fs::read_to_string(root.join("web/app.css")).unwrap();
    let architecture = fs::read_to_string(root.join("docs/architecture.md")).unwrap();

    for color in ["#022851", "#ffbf00", "#6fcfeb", "#c10230"] {
        assert!(css.contains(color), "dashboard palette is missing {color}");
    }
    assert!(
        architecture.contains("communicationsguide.ucdavis.edu/brand-guide/colors"),
        "the public color reference must stay attributable"
    );
    assert!(
        architecture.contains("not affiliated with or endorsed by UC Davis"),
        "the color reference must not imply affiliation"
    );

    let text_pairs = [
        ("#022851", "#f5f7fa", "light body"),
        ("#4f6073", "#ffffff", "light secondary"),
        ("#ffffff", "#022851", "light primary and dark body"),
        ("#cdd6e0", "#022851", "dark secondary"),
        ("#022851", "#ffbf00", "dark primary action"),
        ("#cdd6e0", "#011b37", "navigation secondary"),
        ("#ffffff", "#c10230", "destructive action"),
    ];
    for (foreground, background, role) in text_pairs {
        let ratio = contrast(foreground, background);
        assert!(ratio >= 4.5, "{role} contrast is only {ratio:.2}:1");
    }
}

#[test]
fn peer_relay_scripts_compile_with_python() {
    let root = repository_root();
    let cache = tempfile::tempdir().unwrap();
    let output = Command::new("python3")
        .args(["-m", "py_compile", "runtime/peer_proxy.py"])
        .current_dir(&root)
        .env("PYTHONPYCACHEPREFIX", cache.path())
        .output()
        .expect("python3 is required on supported controller hosts");
    assert!(
        output.status.success(),
        "peer relay scripts did not compile: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn sandbox_teamlead_extension_contract_passes() {
    let root = repository_root();
    let output = Command::new("node")
        .args(["--test", "tests/sandbox_teamlead_contract.test.mjs"])
        .current_dir(&root)
        .output()
        .expect("Node.js is required for the harness adapter contract");
    assert!(
        output.status.success(),
        "sandbox Team Lead adapter tests failed:\n{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn shell_entrypoints_are_executable_and_parse() {
    let root = repository_root();
    let scripts = fs::read_dir(root.join("scripts")).unwrap();
    let mut checked = 0;

    for entry in scripts {
        let path = entry.unwrap().path();
        if path.extension().and_then(|value| value.to_str()) != Some("sh") {
            continue;
        }
        checked += 1;
        let mode = fs::metadata(&path).unwrap().permissions().mode();
        assert_ne!(mode & 0o111, 0, "{} is not executable", path.display());
        let output = Command::new("sh").arg("-n").arg(&path).output().unwrap();
        assert!(
            output.status.success(),
            "{} did not parse: {}",
            path.display(),
            String::from_utf8_lossy(&output.stderr)
        );
        let body = fs::read_to_string(&path).unwrap();
        assert!(
            !body.contains("openshell forward start"),
            "{} uses the ambiguous OpenShell forwarding command",
            path.display()
        );
    }

    assert!(checked >= 4, "expected the supported shell entrypoints");
}

/// A key committed and then deleted stays reachable, so the history scan must find it, including through a pattern
/// that starts with hyphens; a blob listed in the reviewed baseline is the only exception.
#[test]
fn public_boundary_scan_finds_a_deleted_key_in_history() {
    let root = repository_root();
    let work = tempfile::tempdir().unwrap();
    let repo = work.path();
    fs::create_dir(repo.join("scripts")).unwrap();
    fs::copy(
        root.join("scripts/scan_public_boundary.sh"),
        repo.join("scripts/scan_public_boundary.sh"),
    )
    .unwrap();
    // Built at run time so this source holds no key header; gitleaks stays off the PATH.
    let fence = "-".repeat(5);
    let key = format!("{fence}BEGIN PRIVATE KEY{fence}\nAAAA\n{fence}END PRIVATE KEY{fence}\n");
    let git = |args: &[&str]| {
        let output = Command::new("git")
            .args([
                "-c",
                "user.name=Test",
                "-c",
                "user.email=test@example.invalid",
            ])
            .args(args)
            .current_dir(repo)
            .env("PATH", "/usr/bin:/bin")
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "git {args:?}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        String::from_utf8(output.stdout).unwrap()
    };
    let scan = || {
        Command::new("sh")
            .arg("scripts/scan_public_boundary.sh")
            .current_dir(repo)
            .env("PATH", "/usr/bin:/bin")
            .output()
            .unwrap()
    };
    git(&["init", "-q"]);
    fs::write(repo.join("removed_notes.txt"), &key).unwrap();
    // -f: a global excludes file must not decide what this test commits.
    git(&["add", "-f", "removed_notes.txt", "scripts"]);
    git(&["commit", "-q", "-m", "add"]);
    let blob = git(&["rev-parse", "HEAD:removed_notes.txt"]);
    git(&["rm", "-q", "removed_notes.txt"]);
    git(&["commit", "-q", "-m", "remove"]);

    let found = scan();
    let report = String::from_utf8_lossy(&found.stderr);
    assert!(
        !found.status.success(),
        "the scan passed with a key in history"
    );
    assert!(
        report.contains("Reachable history contains a private-boundary marker:\nremoved_notes.txt"),
        "{report}"
    );

    fs::write(
        repo.join("scripts/public_boundary_baseline"),
        format!("{} removed_notes.txt\n", blob.trim()),
    )
    .unwrap();
    git(&["add", "."]);
    git(&["commit", "-q", "-m", "baseline"]);
    let accepted = scan();
    assert!(
        accepted.status.success(),
        "{}",
        String::from_utf8_lossy(&accepted.stderr)
    );
}

/// Site markers come only from the private pattern file, ignore case and spare the allowed public text; private process
/// references (decision numbers, dated owner citations, review, release and run ids) fail the scan in the tree and in
/// history.
#[test]
fn public_boundary_scan_refuses_site_names_in_any_case_and_process_references() {
    let root = repository_root();
    let work = tempfile::tempdir().unwrap();
    let repo = &work.path().join("repo");
    fs::create_dir(repo).unwrap();
    // The private pattern file lives beside the repository, never in it.
    let markers = work.path().join("markers.txt");
    fs::create_dir(repo.join("scripts")).unwrap();
    fs::copy(
        root.join("scripts/scan_public_boundary.sh"),
        repo.join("scripts/scan_public_boundary.sh"),
    )
    .unwrap();
    let git = |args: &[&str]| {
        let output = Command::new("git")
            .args([
                "-c",
                "user.name=Test",
                "-c",
                "user.email=test@example.invalid",
            ])
            .args(args)
            .current_dir(repo)
            .env("PATH", "/usr/bin:/bin")
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "git {args:?}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    };
    let scan = |file: &str, text: String| {
        fs::write(repo.join(file), text).unwrap();
        git(&["add", "-f", "."]);
        git(&["commit", "-q", "-m", file]);
        let output = Command::new("sh")
            .arg("scripts/scan_public_boundary.sh")
            .current_dir(repo)
            .env("PATH", "/usr/bin:/bin")
            .env("PUBLIC_BOUNDARY_EXTRA_PATTERNS_FILE", &markers)
            .output()
            .unwrap();
        (
            output.status.success(),
            String::from_utf8_lossy(&output.stderr).into_owned(),
        )
    };
    git(&["init", "-q"]);
    // A synthetic site marker and one allowed public use of it.
    fs::write(
        &markers,
        "# site markers\n\nexample[- ]?site[- ]?7\nallow:example site 7 public data\n",
    )
    .unwrap();
    let (passed, report) = scan(
        "notes.txt",
        "A plain note. R4.6.1 draws it. Verify checksums before S3 upload. It converges in generation 100.\n".into(),
    );
    assert!(passed, "{report}");
    let (passed, report) = scan(
        "public.txt",
        "Read Example Site 7 Public Data for this.\n".into(),
    );
    assert!(passed, "the allowed public text failed: {report}");
    let (passed, report) = scan("site.txt", "The EXAMPLE-SITE-7 cluster.\n".into());
    assert!(
        !passed
            && report.contains(
                "Current tracked or untracked files contain a private site marker:\nsite.txt"
            ),
        "{report}"
    );
    fs::remove_file(repo.join("site.txt")).unwrap();
    git(&["add", "-A"]);
    git(&["commit", "-q", "-m", "remove site"]);
    let (passed, report) = scan("notes.txt", "Another plain note.\n".into());
    assert!(
        !passed && report.contains("Reachable history contains a private site marker:\nsite.txt"),
        "{report}"
    );
    // A path with colons is reported whole, in the tree and in history.
    fs::create_dir(repo.join("a:b")).unwrap();
    let (passed, report) = scan("a:b/site.txt", "example site 7 again\n".into());
    assert!(
        !passed && report.contains("private site marker:\na:b/site.txt"),
        "{report}"
    );
    fs::remove_dir_all(repo.join("a:b")).unwrap();
    git(&["add", "-A"]);
    git(&["commit", "-q", "-m", "remove colon path"]);
    let output = Command::new("sh")
        .arg("scripts/scan_public_boundary.sh")
        .current_dir(repo)
        .env("PATH", "/usr/bin:/bin")
        .env("PUBLIC_BOUNDARY_EXTRA_PATTERNS_FILE", &markers)
        .output()
        .unwrap();
    let report = String::from_utf8_lossy(&output.stderr);
    let history = report
        .split("Reachable history contains a private site marker:\n")
        .nth(1)
        .unwrap_or_default();
    assert!(
        history.lines().any(|line| line == "a:b/site.txt"),
        "{report}"
    );
    // A pattern that does not compile, or one with capitals, stops the scan instead of matching nothing.
    for (bad, said) in [
        ("example-site-7\nallow:[\n", "does not compile"),
        ("example-[[:upper:]]{4}-7\n", "must be lowercase"),
        ("Example-Site-7\n", "must be lowercase"),
    ] {
        fs::write(&markers, bad).unwrap();
        let (passed, report) = scan("notes.txt", format!("A note for {bad:?}.\n"));
        assert!(!passed && report.contains(said), "{bad:?}: {report}");
    }
    fs::write(
        &markers,
        "example[- ]?site[- ]?7\nallow:example site 7 public data\n",
    )
    .unwrap();
    // The repository's own scan holds no site or owner marker of its own.
    let script = fs::read_to_string(root.join("scripts/scan_public_boundary.sh")).unwrap();
    assert!(!script.contains("private_context_pattern") && !script.contains("Ri''ch"));
    let reviewers = ["Sol", "Day\u{62}reak", "Co\u{64}ex"];
    let texts = [
        format!("Kept as {} {} says.", "decision", 123),
        format!("{} re-review N1 found it.", reviewers[2]),
        format!("{} containment review accepts it.", reviewers[2]),
        format!(
            "{} round 2 ({}, {}): a caret.",
            "Review", reviewers[0], reviewers[1]
        ),
        format!(
            "// {} review {}22 round 2: the bounds count results.",
            reviewers[0], "r"
        ),
        format!(
            "checked as a run takes it ({}, r33 {} round 2).",
            reviewers[0], "review"
        ),
        format!("// {} R12 DB-06: a refused volume.", "Review"),
        format!("// {} {}21 round 1: a bound.", "review", "r"),
        format!("// Rule (live {}, October 11).", "review"),
        format!("// {} r8 added bytes.", "Broker"),
        format!("// {} c26 lost a turn.", "Canary"),
        format!("// {} 4.3: the hand-off.", "VISION"),
        format!("// A table ({} G, migration 064).", "harness"),
        format!("-- Claimed fairly ({}_WORK 15).", "OPEN"),
        format!("// A long run (the owner, {} 12: walk away).", "October"),
        format!("// {} 5 review: the store records custody.", "Round"),
        format!("// {} finding F-07: runs are paged.", "Review"),
        format!("-- Figure previews ({} S7).", "workspace"),
        format!("// The live log (broker {} v4 item 9).", "shapes"),
        format!("(the autonomy suite's {} s7 proposed a query)", "question"),
        format!("(in the {} suite both refusals)", "B4a"),
        format!("//! Step checks ({} B3).", "program"),
        format!("/// An error (review {}, F3).", "C"),
        format!("// {} S7: previews are drawn.", "Workspace"),
        format!("// A table, as {}10's preparation writes it.", "S"),
        format!(
            "// The broker waits for it (broker shapes, {}7 previews).",
            "S"
        ),
        format!(
            "in suite runs on {} 44 and 47 the specialist",
            "generations"
        ),
        format!("/// The entity lists required by {}6.", "D"),
        format!("-- Upgrade databases that ran the one-row {}0b draft.", "D"),
        format!("// the {}1.1 lesson test", "R"),
        format!("the broker's {} v10 names it", "shape"),
        format!("for the {}-70 prompt", "generation"),
        format!("/// How the third call is narrowed ({} H4).", "049"),
        format!("-- What each task read ({}-027).", "ASSURE"),
        format!("/// from a service that predates {}4.", "L"),
        format!(
            "Every review ended the run (before {}5a, for a person).",
            "B"
        ),
        format!("// kept until {}4;", "L"),
        format!("It ran before {}5a.", "B"),
        format!("[as of after {}2]", "K"),
        format!("//! The upload guard ({}1): checks", "G"),
    ];
    for (n, text) in texts.iter().enumerate() {
        let file = format!("process_{n}.txt");
        let (passed, report) = scan(&file, format!("{text}\n"));
        let current = format!(
            "Current tracked or untracked files contain a private process reference:\n{file}"
        );
        assert!(!passed && report.contains(&current), "{text}: {report}");
        fs::remove_file(repo.join(&file)).unwrap();
        git(&["add", "-A"]);
        git(&["commit", "-q", "-m", "remove"]);
        let output = Command::new("sh")
            .arg("scripts/scan_public_boundary.sh")
            .current_dir(repo)
            .env("PATH", "/usr/bin:/bin")
            .output()
            .unwrap();
        let report = String::from_utf8_lossy(&output.stderr);
        // History keeps every earlier file too, so look for this one among the listed paths.
        let history = report
            .split("Reachable history contains a private process reference:\n")
            .nth(1)
            .unwrap_or_default();
        assert!(
            !output.status.success() && history.lines().any(|line| line == file),
            "{text}: {report}"
        );
    }
}

#[test]
fn qualification_script_generates_a_valid_default_name() {
    let root = repository_root();
    let script = fs::read_to_string(root.join("scripts/qualify_sandbox.sh")).unwrap();
    assert_eq!(
        script.matches("--trash-workspace").count(),
        2,
        "disposable qualification must trash its workspace on success and failure"
    );
    let temp = tempfile::tempdir().unwrap();
    let fake_bin = temp.path().join("bin");
    let capture = temp.path().join("sandbox-name");
    fs::create_dir(&fake_bin).unwrap();

    let agent_sandbox = fake_bin.join("agent-sandbox");
    fs::write(
        &agent_sandbox,
        r#"#!/bin/sh
set -eu
if [ "${1:-}" = create ]; then
    shift
    while [ "$#" -gt 0 ]; do
        if [ "$1" = --name ]; then
            printf '%s\n' "$2" > "$QUALIFICATION_CAPTURE"
            exit 0
        fi
        shift
    done
    exit 1
fi
exit 0
"#,
    )
    .unwrap();
    let mut permissions = fs::metadata(&agent_sandbox).unwrap().permissions();
    permissions.set_mode(0o700);
    fs::set_permissions(&agent_sandbox, permissions).unwrap();

    let system_path = std::env::var("PATH").unwrap_or_else(|_| "/usr/bin:/bin".into());
    let output = Command::new("sh")
        .args([
            root.join("scripts/qualify_sandbox.sh").to_str().unwrap(),
            "--profile",
            "docker-local",
        ])
        .env("PATH", format!("{}:{system_path}", fake_bin.display()))
        .env("QUALIFICATION_CAPTURE", &capture)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "qualification script failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    let name = fs::read_to_string(capture).unwrap();
    let name = name.trim();
    assert!(name.len() <= 19, "default name is too long: {name}");
    assert!(
        name.chars().all(|character| character.is_ascii_lowercase()
            || character.is_ascii_digit()
            || character == '-'),
        "default name is not a lowercase DNS label: {name}"
    );
}

#[test]
fn qualification_retries_one_transient_snapshot_failure() {
    let root = repository_root();
    let temp = tempfile::tempdir().unwrap();
    let fake_bin = temp.path().join("bin");
    let attempts = temp.path().join("snapshot-attempts");
    fs::create_dir(&fake_bin).unwrap();

    let agent_sandbox = fake_bin.join("agent-sandbox");
    fs::write(
        &agent_sandbox,
        r#"#!/bin/sh
set -eu
if [ "${1:-}" = audit ] && [ "${2:-}" = snapshot ]; then
    count=0
    if [ -f "$QUALIFICATION_ATTEMPTS" ]; then
        read -r count < "$QUALIFICATION_ATTEMPTS"
    fi
    count=$((count + 1))
    printf '%s\n' "$count" > "$QUALIFICATION_ATTEMPTS"
    [ "$count" -gt 1 ]
    exit
fi
exit 0
"#,
    )
    .unwrap();
    let mut permissions = fs::metadata(&agent_sandbox).unwrap().permissions();
    permissions.set_mode(0o700);
    fs::set_permissions(&agent_sandbox, permissions).unwrap();

    let system_path = std::env::var("PATH").unwrap_or_else(|_| "/usr/bin:/bin".into());
    let output = Command::new("sh")
        .args([
            root.join("scripts/qualify_sandbox.sh").to_str().unwrap(),
            "--profile",
            "docker-local",
            "--name",
            "retry-260830",
        ])
        .env("PATH", format!("{}:{system_path}", fake_bin.display()))
        .env("QUALIFICATION_ATTEMPTS", &attempts)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "qualification did not recover from one read-only snapshot failure: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(fs::read_to_string(attempts).unwrap().trim(), "2");
}

#[test]
fn kubernetes_profile_can_be_configured_without_live_cluster_access() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("workspaces");
    let state = temp.path().join("state");
    let config = temp.path().join("config");
    let kubectl = temp.path().join("kubectl");
    let kubectl_link = temp.path().join("kubectl-current");
    let kubeconfig = temp.path().join("cluster-config");
    let kubeconfig_link = temp.path().join("cluster-config-current");
    fs::write(&kubectl, "#!/bin/sh\nexit 0\n").unwrap();
    let mut permissions = fs::metadata(&kubectl).unwrap().permissions();
    permissions.set_mode(0o700);
    fs::set_permissions(&kubectl, permissions).unwrap();
    symlink(&kubectl, &kubectl_link).unwrap();
    fs::write(&kubeconfig, "apiVersion: v1\nkind: Config\n").unwrap();
    symlink(&kubeconfig, &kubeconfig_link).unwrap();

    let output = Command::new(env!("CARGO_BIN_EXE_agent-sandbox"))
        .args([
            "--root",
            root.to_str().unwrap(),
            "--state-dir",
            state.to_str().unwrap(),
            "--config-dir",
            config.to_str().unwrap(),
            "platform",
            "configure",
            "--name",
            "kubernetes-ci",
            "--gateway",
            "ci",
            "--driver",
            "kubernetes",
            "--required",
            "--set-default",
            "--namespace",
            "agent-sandboxes",
            "--runtime-class",
            "kata-qemu",
            "--kubectl-bin",
            kubectl_link.to_str().unwrap(),
            "--kubeconfig",
            kubeconfig_link.to_str().unwrap(),
        ])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "profile configuration failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    let settings = fs::read_to_string(config.join("controller.toml")).unwrap();
    assert!(settings.contains("default_gateway_profile = \"kubernetes-ci\""));
    assert!(settings.contains("workspace_transport = \"sync\""));
    assert!(settings.contains("runtime_class_name = \"kata-qemu\""));
    assert!(settings.contains(kubectl.to_str().unwrap()));
    assert!(settings.contains(kubeconfig.to_str().unwrap()));
    assert!(!settings.contains(kubectl_link.to_str().unwrap()));
    assert!(!settings.contains(kubeconfig_link.to_str().unwrap()));
}
