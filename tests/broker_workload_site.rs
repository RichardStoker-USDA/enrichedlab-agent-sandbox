use enrichedlab_agent_sandbox::broker::workload_site::{WorkloadSiteConfig, safe_error_code};

#[test]
fn site_config_cannot_replace_identity_or_accept_credential_values() {
    let mut site = WorkloadSiteConfig::default();
    for key in [
        "sandbox.enrichedlab.org/run-id",
        "app.kubernetes.io/name",
        "invalid key",
    ] {
        site.labels.clear();
        site.labels.insert(key.into(), "value".into());
        assert!(site.validate().is_err());
    }
    site.labels.clear();
    site.image_pull_secrets = vec!["https://not-a-secret-reference".into()];
    assert!(site.validate().is_err());
    site.image_pull_secrets = vec!["pull".into(), "pull".into()];
    assert!(site.validate().is_err());
}

#[test]
fn only_typed_failure_codes_leave_the_custodian() {
    let error = anyhow::anyhow!("publication_journal_unavailable")
        .context("transport returned a private URL and credentials");
    assert_eq!(safe_error_code(&error), "publication_journal_unavailable");
    for text in [
        "postgresql://private.invalid",
        "input_failed: private response",
        "arbitrary_payload",
    ] {
        assert_eq!(
            safe_error_code(&anyhow::anyhow!(text)),
            "custody_operation_failed_recovery_required"
        );
    }
}
