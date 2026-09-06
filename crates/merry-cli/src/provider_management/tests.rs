use super::{ProviderDiscoveryDraft, ProviderDraft, ProviderManagementService};
use crate::config::{ManagedProviderKind, ProviderAlias, XdgPaths};
use merry_llm::ReasoningEffort;
use merry_llm::{
    ModelCatalog, ModelCatalogEntry, ModelCatalogError, ModelCatalogFuture, ModelCatalogProvider,
    ModelName,
};
use merry_provider_openai::OpenAiProtocol;
use std::sync::Arc;
use tokio_util::sync::CancellationToken;

#[test]
fn provider_draft_debug_redacts_credentials() {
    let draft = ProviderDraft::new(
        "OpenCode",
        ProviderAlias::new("opencode").expect("valid alias"),
        ManagedProviderKind::OpenAiCompatible,
        Some(OpenAiProtocol::ChatCompletions),
        "https://opencode.example.test/v1",
        "sk-super-secret",
        ModelName::new("deepseek-v4-pro").expect("valid model"),
    )
    .expect("valid draft");

    let debug = format!("{draft:?}");
    assert!(debug.contains("<redacted>"));
    assert!(!debug.contains("sk-super-secret"));
}

#[test]
fn provider_draft_creation_requires_credentials_but_updates_can_retain_them() {
    let alias = ProviderAlias::new("opencode").expect("valid alias");
    let model = ModelName::new("model-a").expect("valid model");
    for secret in ["", " ", " secret", "secret\n"] {
        assert!(
            ProviderDraft::new(
                "OpenCode",
                alias.clone(),
                ManagedProviderKind::OpenAiCompatible,
                Some(OpenAiProtocol::Responses),
                "https://provider.example.test/v1",
                secret,
                model.clone(),
            )
            .is_err()
        );
    }
    let update = ProviderDraft::for_update(
        "OpenCode",
        alias,
        ManagedProviderKind::OpenAiCompatible,
        Some(OpenAiProtocol::Responses),
        "https://provider.example.test/v1",
        None,
        model,
    )
    .expect("updates may retain an existing credential");
    assert!(format!("{update:?}").contains("<unchanged>"));
}

#[test]
fn provider_discovery_draft_requires_new_credentials_and_redacts_them() {
    let alias = ProviderAlias::new("opencode").expect("valid alias");
    let missing = ProviderDiscoveryDraft::new(
        alias.clone(),
        None,
        ManagedProviderKind::OpenAiCompatible,
        Some(OpenAiProtocol::ChatCompletions),
        "https://opencode.example.test/v1",
        None,
    )
    .expect_err("new providers need credentials for discovery");
    assert!(missing.to_string().contains("enter an API key"));

    let draft = ProviderDiscoveryDraft::new(
        alias,
        None,
        ManagedProviderKind::OpenAiCompatible,
        Some(OpenAiProtocol::ChatCompletions),
        "https://opencode.example.test/v1",
        Some("sk-super-secret"),
    )
    .expect("valid discovery draft");
    let debug = format!("{draft:?}");
    assert!(debug.contains("<redacted>"));
    assert!(!debug.contains("sk-super-secret"));
}

#[test]
fn provider_discovery_draft_allows_retained_credentials_when_editing() {
    let draft = ProviderDiscoveryDraft::new(
        ProviderAlias::new("opencode").expect("valid alias"),
        Some(ProviderAlias::new("opencode").expect("valid original alias")),
        ManagedProviderKind::OpenAiCompatible,
        Some(OpenAiProtocol::Responses),
        "https://opencode.example.test/v1",
        None,
    )
    .expect("editing may retain the stored credential");

    assert!(format!("{draft:?}").contains("<retained>"));
}

#[tokio::test]
async fn provider_discovery_resolves_the_managed_credential_for_edits() {
    let (_temp, paths) = test_paths();
    let mut service = ProviderManagementService::new(paths).expect("service");
    let alias = ProviderAlias::new("opencode").expect("valid alias");
    service
        .save_provider(
            ProviderDraft::new(
                "OpenCode",
                alias.clone(),
                ManagedProviderKind::OpenAiCompatible,
                Some(OpenAiProtocol::ChatCompletions),
                "https://opencode.example.test/v1",
                "sk-retained-secret",
                ModelName::new("model-a").expect("valid model"),
            )
            .expect("valid provider"),
        )
        .await
        .expect("save provider");

    let credential = service
        .resolve_retained_api_key(&alias)
        .expect("retained credential");

    assert_eq!(credential, "sk-retained-secret");
}

#[tokio::test]
async fn lists_user_and_managed_profiles_without_deduplicating_endpoints() {
    let (temp, paths) = test_paths();
    tokio::fs::create_dir_all(paths.config_dir())
        .await
        .expect("config dir");
    tokio::fs::write(
        paths.config_file(),
        r#"
[providers.user-gateway]
display_name = "User Gateway"
default_model = "model-user"
type = "openai-compatible"
base_url = "https://gateway.example.test/v1"
api_key = "sk-user"
"#,
    )
    .await
    .expect("user config");
    let mut service = ProviderManagementService::new(paths.clone()).expect("service");
    service
        .save_provider(
            ProviderDraft::new(
                "Managed Gateway",
                ProviderAlias::new("managed-gateway").expect("alias"),
                ManagedProviderKind::OpenAiCompatible,
                Some(OpenAiProtocol::ChatCompletions),
                "https://gateway.example.test/v1",
                "sk-managed",
                ModelName::new("model-managed").expect("model"),
            )
            .expect("draft"),
        )
        .await
        .expect("managed provider save");

    let profiles = service.profiles().expect("profiles");

    assert_eq!(profiles.len(), 2);
    assert_eq!(profiles[0].alias().as_str(), "managed-gateway");
    assert_eq!(profiles[1].alias().as_str(), "user-gateway");
    let error = service
        .editable_provider(&ProviderAlias::new("user-gateway").expect("alias"))
        .expect_err("config.toml provider should be read-only");
    assert!(error.to_string().contains("read-only"));
    assert!(
        !error
            .to_string()
            .contains("invalid provider management request")
    );
    drop(temp);
}

#[tokio::test]
async fn user_alias_collision_fails_before_managed_registry_mutation() {
    let (_temp, paths) = test_paths();
    tokio::fs::create_dir_all(paths.config_dir())
        .await
        .expect("config dir");
    tokio::fs::write(
        paths.config_file(),
        r#"
[providers.opencode]
type = "openai-compatible"
api_key = "sk-user"
"#,
    )
    .await
    .expect("user config");
    let mut service = ProviderManagementService::new(paths.clone()).expect("service");
    let draft = ProviderDraft::new(
        "OpenCode",
        ProviderAlias::new("opencode").expect("alias"),
        ManagedProviderKind::OpenAiCompatible,
        Some(OpenAiProtocol::ChatCompletions),
        "https://opencode.example.test/v1",
        "sk-managed",
        ModelName::new("model-a").expect("model"),
    )
    .expect("draft");

    let error = service
        .save_provider(draft)
        .await
        .expect_err("user alias collision should fail");

    assert!(error.to_string().contains("opencode"));
    assert!(!paths.managed_providers_file().exists());
    assert!(!paths.managed_secrets_dir().exists());
}

#[tokio::test]
async fn add_provider_does_not_overwrite_existing_managed_alias() {
    let (_temp, paths) = test_paths();
    let mut service = ProviderManagementService::new(paths.clone()).expect("service");
    service
        .save_provider(
            ProviderDraft::new(
                "OpenCode Original",
                ProviderAlias::new("opencode").expect("alias"),
                ManagedProviderKind::OpenAiCompatible,
                Some(OpenAiProtocol::ChatCompletions),
                "https://first.example.test/v1",
                "sk-first",
                ModelName::new("model-a").expect("model"),
            )
            .expect("draft"),
        )
        .await
        .expect("first provider save");

    let error = service
        .save_provider(
            ProviderDraft::new(
                "OpenCode Replacement",
                ProviderAlias::new("opencode").expect("alias"),
                ManagedProviderKind::OpenAiCompatible,
                Some(OpenAiProtocol::Responses),
                "https://second.example.test/v1",
                "sk-second",
                ModelName::new("model-b").expect("model"),
            )
            .expect("draft"),
        )
        .await
        .expect_err("add must not overwrite an existing managed alias");

    assert!(error.to_string().contains("already exists"));
    assert_eq!(secret_file_count(&paths).await, 1);
    assert_eq!(
        service
            .profiles()
            .expect("profiles")
            .into_iter()
            .next()
            .expect("profile")
            .display_name(),
        "OpenCode Original"
    );
}

#[tokio::test]
async fn deletes_only_managed_provider_and_removes_its_secret() {
    let (_temp, paths) = test_paths();
    let mut service = ProviderManagementService::new(paths.clone()).expect("service");
    service
        .save_provider(
            ProviderDraft::new(
                "OpenCode",
                ProviderAlias::new("opencode").expect("alias"),
                ManagedProviderKind::OpenAiCompatible,
                Some(OpenAiProtocol::ChatCompletions),
                "https://opencode.example.test/v1",
                "sk-managed",
                ModelName::new("model-a").expect("model"),
            )
            .expect("draft"),
        )
        .await
        .expect("provider save");
    assert_eq!(secret_file_count(&paths).await, 1);

    service
        .delete_provider(&ProviderAlias::new("opencode").expect("alias"))
        .await
        .expect("managed provider delete");

    assert!(service.profiles().expect("profiles").is_empty());
    assert_eq!(secret_file_count(&paths).await, 0);
}

#[tokio::test]
async fn edits_managed_provider_and_retains_api_key_when_blank() {
    let (_temp, paths) = test_paths();
    let mut service = ProviderManagementService::new(paths.clone()).expect("service");
    let alias = ProviderAlias::new("opencode").expect("alias");
    service
        .save_provider(
            ProviderDraft::new(
                "OpenCode",
                alias.clone(),
                ManagedProviderKind::OpenAiCompatible,
                Some(OpenAiProtocol::ChatCompletions),
                "https://old.example.test/v1",
                "sk-retained",
                ModelName::new("model-a").expect("model"),
            )
            .expect("draft")
            .with_reasoning_effort(Some(ReasoningEffort::new("medium").expect("valid effort"))),
        )
        .await
        .expect("provider save");

    let editable = service.editable_provider(&alias).expect("editable profile");
    assert_eq!(editable.display_name, "OpenCode");
    assert_eq!(editable.protocol, Some(OpenAiProtocol::ChatCompletions));
    assert_eq!(
        editable
            .reasoning_effort
            .as_ref()
            .map(ReasoningEffort::as_str),
        Some("medium")
    );
    service
        .update_provider(
            &alias,
            ProviderDraft::for_update(
                "OpenCode Edited",
                alias.clone(),
                ManagedProviderKind::OpenAiCompatible,
                Some(OpenAiProtocol::Responses),
                "https://new.example.test/v1",
                None,
                ModelName::new("model-b").expect("model"),
            )
            .expect("update draft")
            .with_reasoning_effort_text("max ultra")
            .expect("valid custom effort"),
        )
        .await
        .expect("provider update");

    let updated = service.editable_provider(&alias).expect("updated profile");
    assert_eq!(updated.display_name, "OpenCode Edited");
    assert_eq!(updated.protocol, Some(OpenAiProtocol::Responses));
    assert_eq!(updated.base_url, "https://new.example.test/v1");
    assert_eq!(updated.default_model.as_str(), "model-b");
    assert_eq!(
        updated
            .reasoning_effort
            .as_ref()
            .map(ReasoningEffort::as_str),
        Some("max ultra")
    );
    assert_eq!(secret_file_count(&paths).await, 1);
    let mut secrets = tokio::fs::read_dir(paths.managed_secrets_dir())
        .await
        .expect("secret directory");
    let secret_path = secrets
        .next_entry()
        .await
        .expect("secret entry")
        .expect("secret file")
        .path();
    assert_eq!(
        tokio::fs::read_to_string(secret_path)
            .await
            .expect("secret contents")
            .trim(),
        "sk-retained"
    );
}

#[tokio::test]
async fn model_cache_is_available_before_refresh_and_survives_failure() {
    let (_temp, paths) = test_paths();
    let service = ProviderManagementService::new(paths).expect("service");
    let alias = ProviderAlias::new("opencode").expect("alias");
    let cached = catalog(&["cached-model"]);
    service
        .save_model_cache(&alias, &cached)
        .await
        .expect("cache save");

    assert_eq!(
        service
            .load_model_cache(&alias)
            .await
            .expect("cache load")
            .expect("cache exists")
            .models()[0]
            .id()
            .as_str(),
        "cached-model"
    );
    let failing: Arc<dyn ModelCatalogProvider> = Arc::new(ScriptedCatalog::Failure);
    service
        .discover_and_cache_with(&alias, failing, CancellationToken::new())
        .await
        .expect_err("refresh should fail");
    assert_eq!(
        service
            .load_model_cache(&alias)
            .await
            .expect("cache load")
            .expect("old cache remains")
            .models()[0]
            .id()
            .as_str(),
        "cached-model"
    );
}

#[tokio::test]
async fn successful_refresh_replaces_cache_and_cancelled_refresh_does_not() {
    let (_temp, paths) = test_paths();
    let service = ProviderManagementService::new(paths).expect("service");
    let alias = ProviderAlias::new("opencode").expect("alias");
    service
        .save_model_cache(&alias, &catalog(&["old-model"]))
        .await
        .expect("old cache");
    let success: Arc<dyn ModelCatalogProvider> =
        Arc::new(ScriptedCatalog::Success(catalog(&["new-model"])));
    service
        .discover_and_cache_with(&alias, success, CancellationToken::new())
        .await
        .expect("refresh succeeds");
    assert_eq!(
        service
            .load_model_cache(&alias)
            .await
            .expect("cache load")
            .expect("cache exists")
            .models()[0]
            .id()
            .as_str(),
        "new-model"
    );

    let token = CancellationToken::new();
    token.cancel();
    let cancelled: Arc<dyn ModelCatalogProvider> = Arc::new(ScriptedCatalog::Cancelled);
    service
        .discover_and_cache_with(&alias, cancelled, token)
        .await
        .expect_err("cancelled refresh");
    assert_eq!(
        service
            .load_model_cache(&alias)
            .await
            .expect("cache load")
            .expect("cache remains")
            .models()[0]
            .id()
            .as_str(),
        "new-model"
    );
}

enum ScriptedCatalog {
    Success(ModelCatalog),
    Failure,
    Cancelled,
}

impl ModelCatalogProvider for ScriptedCatalog {
    fn list_models<'a>(&'a self, token: CancellationToken) -> ModelCatalogFuture<'a> {
        Box::pin(async move {
            if token.is_cancelled() || matches!(self, Self::Cancelled) {
                return Err(ModelCatalogError::cancelled());
            }
            match self {
                Self::Success(catalog) => Ok(catalog.clone()),
                Self::Failure => Err(ModelCatalogError::new(
                    merry_llm::ModelCatalogErrorKind::Transport,
                    "fixture transport failure",
                )),
                Self::Cancelled => unreachable!(),
            }
        })
    }
}

fn catalog(models: &[&str]) -> ModelCatalog {
    ModelCatalog::new(
        models
            .iter()
            .map(|model| {
                ModelCatalogEntry::new(ModelName::new(model).expect("valid model"), Some("fixture"))
                    .expect("catalog entry")
            })
            .collect(),
    )
}

fn test_paths() -> (tempfile::TempDir, XdgPaths) {
    let temp = tempfile::tempdir().expect("tempdir");
    let paths = XdgPaths::from_parts(
        temp.path().join("home"),
        Some(temp.path().join("config")),
        Some(temp.path().join("state")),
    );
    (temp, paths)
}

async fn secret_file_count(paths: &XdgPaths) -> usize {
    let mut entries = tokio::fs::read_dir(paths.managed_secrets_dir())
        .await
        .expect("secrets directory");
    let mut count = 0;
    while entries.next_entry().await.expect("secret entry").is_some() {
        count += 1;
    }
    count
}
