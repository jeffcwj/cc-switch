//! Copy providers from one app's list into another app's list.
//!
//! OpenCode's provider key is the row id and also the key DSH uses in
//! `providers.<key>`, so an OpenCode sync upserts by that key. Codex, Claude Code and
//! Claude Desktop have no per-provider key — their rows are matched by name instead.
//!
//! Rows are created through the same service calls the UI uses (`add` / `update`), so
//! a sync behaves exactly like adding the provider by hand: no schema changes, no extra
//! metadata keys.

use crate::app_config::AppType;
use crate::error::AppError;
use crate::provider::{Provider, ProviderMeta};
use crate::services::provider::ProviderService;
use crate::store::AppState;
use serde::Serialize;
use serde_json::{json, Map, Value};

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SyncSkip {
    pub id: String,
    pub name: String,
    pub reason: String,
}

#[derive(Debug, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SyncReport {
    /// Provider keys created in the target app.
    pub created: Vec<String>,
    /// Provider keys updated in place.
    pub updated: Vec<String>,
    /// Sources that could not be converted, with the reason shown to the user.
    pub skipped: Vec<SyncSkip>,
}

impl SyncReport {
    fn absorb(&mut self, other: SyncReport) {
        self.created.extend(other.created);
        self.updated.extend(other.updated);
        self.skipped.extend(other.skipped);
    }
}

/// Sync targets exposed by the OpenCode card and the page-level action.
pub fn target_app(raw: &str) -> Result<AppType, AppError> {
    match raw {
        "dsh" => Ok(AppType::Dsh),
        "codex" => Ok(AppType::Codex),
        "claude" => Ok(AppType::Claude),
        "claude-desktop" => Ok(AppType::ClaudeDesktop),
        other => Err(AppError::InvalidInput(format!(
            "unsupported sync target '{other}'"
        ))),
    }
}

/// Apps a card may copy itself into. Claude Code and Claude Desktop are the same gateway in
/// two shapes, so they can be copied into each other; OpenCode feeds all of them.
pub fn available_targets(source: AppType) -> &'static [AppType] {
    match source {
        AppType::OpenCode => &[
            AppType::Dsh,
            AppType::Codex,
            AppType::Claude,
            AppType::ClaudeDesktop,
        ],
        AppType::Claude => &[AppType::ClaudeDesktop],
        AppType::ClaudeDesktop => &[AppType::Claude],
        _ => &[],
    }
}

fn ensure_target(source: AppType, target: AppType) -> Result<(), AppError> {
    if available_targets(source).contains(&target) {
        return Ok(());
    }
    Err(AppError::InvalidInput(format!(
        "cannot copy {} providers into {}",
        source.as_str(),
        target.as_str()
    )))
}

/// Sync a single provider (by key) into `target`.
pub fn sync_one(
    state: &AppState,
    source_app: AppType,
    key: &str,
    target: AppType,
) -> Result<SyncReport, AppError> {
    ensure_target(source_app, target)?;
    let source = ProviderService::list(state, source_app)?
        .get(key)
        .cloned()
        .ok_or_else(|| {
            AppError::InvalidInput(format!(
                "{} provider '{key}' not found",
                source_app.as_str()
            ))
        })?;
    let mut report = SyncReport::default();
    sync_provider(state, source_app, &source, target, &mut report)?;
    Ok(report)
}

/// Sync every provider of `source_app` into `target`.
///
/// For OpenCode only rows present in its live config are copied: the panel also lists
/// providers that were never added there, and turning those into cards elsewhere would
/// surprise the user. Conversion failures are reported per provider instead of aborting the
/// run, so one Anthropic-only row cannot block the rest.
pub fn sync_all(
    state: &AppState,
    source_app: AppType,
    target: AppType,
) -> Result<SyncReport, AppError> {
    ensure_target(source_app, target)?;
    let sources: Vec<Provider> = match source_app {
        AppType::OpenCode => {
            let added = crate::opencode_config::get_providers()?;
            ProviderService::list(state, AppType::OpenCode)?
                .into_iter()
                .filter(|(key, _)| added.contains_key(key))
                .map(|(_, provider)| provider)
                .collect()
        }
        _ => ProviderService::list(state, source_app)?
            .into_values()
            // The official card carries no gateway `env`; there is nothing to copy.
            .filter(|provider| provider.settings_config.get("env").is_some())
            .collect(),
    };

    let mut report = SyncReport::default();
    for source in sources {
        if let Err(error) = sync_provider(state, source_app, &source, target, &mut report) {
            report.skipped.push(SyncSkip {
                id: source.id.clone(),
                name: source.name.clone(),
                reason: error.to_string(),
            });
        }
    }
    Ok(report)
}

fn sync_provider(
    state: &AppState,
    source_app: AppType,
    source: &Provider,
    target: AppType,
    report: &mut SyncReport,
) -> Result<(), AppError> {
    let mut one = SyncReport::default();
    let result = match target {
        AppType::Dsh => sync_dsh(state, source_app, source, &mut one),
        AppType::Codex => sync_codex(state, source_app, source, &mut one),
        AppType::ClaudeDesktop => sync_desktop(state, source_app, source, &mut one),
        AppType::Claude => sync_claude(state, source_app, source, &mut one),
        _ => Err(AppError::InvalidInput(format!(
            "app '{}' is not a sync target",
            target.as_str()
        ))),
    };
    report.absorb(one);
    result
}

/// The parts of an OpenCode row the DSH / Codex row needs.
struct SourceView {
    key: String,
    name: String,
    npm: String,
    base_url: String,
    api_key: String,
    models: Vec<(String, Value)>,
}

impl SourceView {
    fn of(source: &Provider) -> Result<Self, AppError> {
        let config = &source.settings_config;
        let npm = config
            .get("npm")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .trim()
            .to_string();
        let options = config.get("options");
        let base_url = options
            .and_then(|value| value.get("baseURL"))
            .and_then(Value::as_str)
            .unwrap_or_default()
            .trim()
            .trim_end_matches('/')
            .to_string();
        let api_key = options
            .and_then(|value| value.get("apiKey"))
            .and_then(Value::as_str)
            .unwrap_or_default()
            .trim()
            .to_string();
        if base_url.is_empty() {
            return Err(AppError::InvalidInput(format!(
                "OpenCode provider '{}' has no baseURL",
                source.id
            )));
        }
        if api_key.is_empty() {
            return Err(AppError::InvalidInput(format!(
                "OpenCode provider '{}' has no apiKey",
                source.id
            )));
        }
        let models = config
            .get("models")
            .and_then(Value::as_object)
            .map(|models| {
                models
                    .iter()
                    .map(|(id, model)| (id.clone(), model.clone()))
                    .collect()
            })
            .unwrap_or_default();
        // 卡片名是用户在 CC Switch 里看到、也可能改过的名字，优先用它；只有卡片名为空时才退回
        // OpenCode 配置里的 `name`，否则同一家的多个变体在 Codex / DSH 列表里会重名。
        let name = Some(source.name.trim())
            .filter(|name| !name.is_empty())
            .map(str::to_string)
            .or_else(|| {
                config
                    .get("name")
                    .and_then(Value::as_str)
                    .map(str::trim)
                    .filter(|name| !name.is_empty())
                    .map(str::to_string)
            })
            .unwrap_or_else(|| source.id.clone());
        Ok(Self {
            key: source.id.clone(),
            name,
            npm,
            base_url,
            api_key,
            models,
        })
    }

    /// The gateway/env shape Claude Code and Claude Desktop rows share.
    fn from_env(source: &Provider, models: Vec<(String, Value)>) -> Result<Self, AppError> {
        let base_url = env_text(source, "ANTHROPIC_BASE_URL")
            .map(|url| url.trim_end_matches('/').to_string())
            .ok_or_else(|| {
                AppError::InvalidInput(format!(
                    "provider '{}' has no ANTHROPIC_BASE_URL",
                    source.name
                ))
            })?;
        let api_key = env_text(source, "ANTHROPIC_AUTH_TOKEN")
            .or_else(|| env_text(source, "ANTHROPIC_API_KEY"))
            .ok_or_else(|| {
                AppError::InvalidInput(format!(
                    "provider '{}' has no ANTHROPIC_AUTH_TOKEN",
                    source.name
                ))
            })?;
        let format = source
            .meta
            .as_ref()
            .and_then(|meta| meta.api_format.as_deref());
        let name = source.name.trim();
        Ok(Self {
            key: source.id.clone(),
            name: if name.is_empty() {
                source.id.clone()
            } else {
                name.to_string()
            },
            npm: npm_for_format(format).to_string(),
            base_url,
            api_key,
            models,
        })
    }
}

/// Read a provider row of any supported source app into the shape the converters below use.
fn source_view(app: AppType, source: &Provider) -> Result<SourceView, AppError> {
    match app {
        AppType::OpenCode => SourceView::of(source),
        AppType::Claude => SourceView::from_env(
            source,
            claude_env_models(source)
                .into_iter()
                .map(|id| (id, Value::Null))
                .collect(),
        ),
        AppType::ClaudeDesktop => SourceView::from_env(
            source,
            desktop_route_models(source)
                .into_iter()
                .map(|id| (id, Value::Null))
                .collect(),
        ),
        other => Err(AppError::InvalidInput(format!(
            "app '{}' is not a sync source",
            other.as_str()
        ))),
    }
}

fn env_text(source: &Provider, key: &str) -> Option<String> {
    source
        .settings_config
        .pointer("/env")
        .and_then(|env| env.get(key))
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
}

/// The SDK family a row speaks, expressed the way OpenCode's `npm` field spells it. Claude
/// Code and Claude Desktop rows carry no `npm`, only the format the gateway expects.
fn npm_for_format(format: Option<&str>) -> &'static str {
    match format {
        Some("openai_chat") => "@ai-sdk/openai-compatible",
        Some("openai_responses") => "@ai-sdk/openai",
        _ => "@ai-sdk/anthropic",
    }
}

/// Claude Code's model aliases, in the order DSH/Codex should list them.
fn claude_env_models(source: &Provider) -> Vec<String> {
    const KEYS: [&str; 4] = [
        "ANTHROPIC_MODEL",
        "ANTHROPIC_DEFAULT_HAIKU_MODEL",
        "ANTHROPIC_DEFAULT_SONNET_MODEL",
        "ANTHROPIC_DEFAULT_OPUS_MODEL",
    ];
    let mut seen: Vec<String> = Vec::new();
    for key in KEYS {
        if let Some(model) = env_text(source, key) {
            if !seen.contains(&model) {
                seen.push(model);
            }
        }
    }
    seen
}

/// Claude Desktop's route table flattened into model ids, keeping the role order.
fn desktop_route_models(source: &Provider) -> Vec<String> {
    let Some(routes) = source
        .meta
        .as_ref()
        .map(|meta| &meta.claude_desktop_model_routes)
    else {
        return Vec::new();
    };
    let mut seen: Vec<String> = Vec::new();
    for route in crate::claude_desktop_config::DEFAULT_PROXY_ROUTES {
        if let Some(mapped) = routes.get(route.route_id) {
            let model = mapped.model.trim();
            if !model.is_empty() && !seen.iter().any(|item| item == model) {
                seen.push(model.to_string());
            }
        }
    }
    for mapped in routes.values() {
        let model = mapped.model.trim();
        if !model.is_empty() && !seen.iter().any(|item| item == model) {
            seen.push(model.to_string());
        }
    }
    seen
}

/// DSH's `api` values, which mirror the SDK family the provider speaks.
fn dsh_api(npm: &str) -> Option<&'static str> {
    match npm {
        "@ai-sdk/openai" => Some("openai-responses"),
        "@ai-sdk/openai-compatible" => Some("openai-completions"),
        "@ai-sdk/anthropic" => Some("anthropic-messages"),
        _ => None,
    }
}

/// Codex only speaks Responses or Chat Completions; Anthropic needs the local router.
fn codex_wire_api(npm: &str) -> Option<&'static str> {
    match npm {
        "@ai-sdk/openai" => Some("responses"),
        "@ai-sdk/openai-compatible" => Some("chat"),
        _ => None,
    }
}

/// DSH's `input` accepts only `text` and `image` (`MODALITIES` in dsh-llm-pi-ai); anything
/// else makes the whole profile fail to load, so unsupported modalities are dropped rather
/// than copied.
fn dsh_input_modalities(model: &Value) -> Option<Vec<Value>> {
    let input = model.pointer("/modalities/input")?.as_array()?;
    let kept: Vec<Value> = input
        .iter()
        .filter(|item| matches!(item.as_str(), Some("text") | Some("image")))
        .cloned()
        .collect();
    (!kept.is_empty()).then_some(kept)
}

fn dsh_provider(view: &SourceView) -> Result<Provider, AppError> {
    let api = dsh_api(&view.npm).ok_or_else(|| {
        AppError::InvalidInput(format!(
            "DSH cannot load npm package '{}' of '{}'",
            view.npm, view.name
        ))
    })?;

    let models: Map<String, Value> = view
        .models
        .iter()
        .map(|(id, model)| {
            let mut entry = Map::new();
            let display = model
                .get("name")
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|name| !name.is_empty())
                .unwrap_or(id)
                .to_string();
            entry.insert("name".into(), json!(display));
            // Limits mean the same thing in both schemas; carry them over so DSH can size the
            // context. Modalities do not: OpenCode also models pdf/video input, while DSH only
            // accepts `text` and `image` (`MODALITIES` in dsh-llm-pi-ai) and rejects the whole
            // config on anything else.
            if let Some(limit) = model.get("limit") {
                entry.insert("limit".into(), limit.clone());
            }
            if let Some(input) = dsh_input_modalities(model) {
                entry.insert("modalities".into(), json!({ "input": input }));
            }
            // OpenCode models describe their thinking levels as `variants`; DSH calls the
            // same thing `reasoningEfforts` (a name → effort map, or `false` for none).
            let efforts = model
                .get("variants")
                .and_then(Value::as_object)
                .map(|variants| {
                    variants
                        .iter()
                        .filter_map(|(name, variant)| {
                            let effort = variant
                                .get("reasoningEffort")
                                .and_then(Value::as_str)
                                .or_else(|| variant.as_str())?;
                            Some((name.clone(), json!(effort)))
                        })
                        .collect::<Map<String, Value>>()
                })
                .unwrap_or_default();
            match (model.get("reasoning").and_then(Value::as_bool), efforts) {
                // The model is not a reasoning model at all.
                (Some(false), _) => {
                    entry.insert("reasoningEfforts".into(), Value::Bool(false));
                }
                (_, efforts) if !efforts.is_empty() => {
                    let mut ordered = Map::new();
                    // "off" first: it means "send no effort at all".
                    if !efforts.contains_key("off") {
                        ordered.insert("off".into(), Value::Null);
                    }
                    ordered.extend(efforts);
                    entry.insert("reasoningEfforts".into(), Value::Object(ordered));
                }
                // Reasoning model without declared variants: let DSH fill in its defaults.
                (Some(true), _) => {
                    entry.insert("reasoning".into(), Value::Bool(true));
                }
                _ => {}
            }
            (id.clone(), Value::Object(entry))
        })
        .collect();

    let mut provider = Provider::with_id(
        view.key.clone(),
        view.name.clone(),
        json!({
            "name": view.name,
            "api": api,
            "options": { "baseURL": view.base_url, "apiKey": view.api_key },
            "models": Value::Object(models),
        }),
        Some(view.base_url.clone()),
    );
    provider.meta = Some(ProviderMeta {
        endpoint_auto_select: Some(true),
        ..Default::default()
    });
    Ok(provider)
}

fn codex_provider(view: &SourceView, id: String) -> Result<Provider, AppError> {
    let wire_api = codex_wire_api(&view.npm).ok_or_else(|| {
        AppError::InvalidInput(format!(
            "Codex cannot talk to npm package '{}' of '{}'; route it through the local proxy",
            view.npm, view.name
        ))
    })?;

    let model = view.models.first().map(|(id, _)| id.as_str()).unwrap_or("");
    let mut config = String::from("model_provider = \"custom\"\n");
    if !model.is_empty() {
        config.push_str(&format!("model = {}\n", toml_string(model)));
    }
    config.push_str("\n[model_providers]\n[model_providers.custom]\n");
    config.push_str("name = \"custom\"\n");
    config.push_str(&format!("wire_api = {}\n", toml_string(wire_api)));
    config.push_str("requires_openai_auth = true\n");
    config.push_str(&format!("base_url = {}\n", toml_string(&view.base_url)));

    let mut provider = Provider::with_id(
        id,
        view.name.clone(),
        json!({
            "auth": { "OPENAI_API_KEY": view.api_key },
            "config": config,
        }),
        Some(view.base_url.clone()),
    );
    provider.meta = Some(ProviderMeta {
        endpoint_auto_select: Some(true),
        api_format: Some(
            if wire_api == "responses" {
                "openai_responses"
            } else {
                "openai_chat"
            }
            .to_string(),
        ),
        ..Default::default()
    });
    Ok(provider)
}

/// Minimal TOML basic-string escaping for the values we emit (keys, model ids, URLs).
fn toml_string(value: &str) -> String {
    let mut escaped = String::with_capacity(value.len() + 2);
    escaped.push('"');
    for character in value.chars() {
        match character {
            '\\' => escaped.push_str("\\\\"),
            '"' => escaped.push_str("\\\""),
            '\n' => escaped.push_str("\\n"),
            '\r' => escaped.push_str("\\r"),
            '\t' => escaped.push_str("\\t"),
            _ => escaped.push(character),
        }
    }
    escaped.push('"');
    escaped
}

fn sync_dsh(
    state: &AppState,
    source_app: AppType,
    source: &Provider,
    report: &mut SyncReport,
) -> Result<(), AppError> {
    let view = source_view(source_app, source)?;
    let provider = dsh_provider(&view)?;
    let key = view.key.clone();
    let settings_config = provider.settings_config.clone();

    let exists = state.db.get_provider_by_id(&key, &AppType::Dsh)?.is_some();
    if exists {
        ProviderService::update(state, AppType::Dsh, Some(&key), provider)?;
        report.updated.push(key.clone());
    } else {
        match ProviderService::add(state, AppType::Dsh, provider, true) {
            Ok(_) => report.created.push(key.clone()),
            // Live 里已经有这一家、只是数据库没这行时，`add` 会拒绝建行。此时把配置推进
            // live 就够了：DSH 面板列的是 live，用户看到的仍然是同一家。
            Err(_) if crate::dsh_config::provider_key_exists(&key).unwrap_or(false) => {
                crate::dsh_config::set_provider(&key, settings_config.clone())?;
                report.updated.push(key.clone());
            }
            Err(error) => return Err(error),
        }
    }

    // Adding writes the profile(s); updating only rewrites live when the key is already
    // there, so a row created outside CC Switch still gets its profile entry.
    if !crate::dsh_config::provider_key_exists(&key)? {
        crate::dsh_config::set_provider(&key, settings_config)?;
    }
    Ok(())
}

fn sync_codex(
    state: &AppState,
    source_app: AppType,
    source: &Provider,
    report: &mut SyncReport,
) -> Result<(), AppError> {
    let view = source_view(source_app, source)?;
    let existing = ProviderService::list(state, AppType::Codex)?
        .into_values()
        .find(|provider| provider.name.eq_ignore_ascii_case(&view.name));

    match existing {
        Some(current) => {
            let id = current.id.clone();
            let provider = codex_provider(&view, id.clone())?;
            ProviderService::update(state, AppType::Codex, Some(&id), provider)?;
            report.updated.push(view.name.clone());
        }
        None => {
            let provider = codex_provider(&view, uuid::Uuid::new_v4().to_string())?;
            ProviderService::add(state, AppType::Codex, provider, false)?;
            report.created.push(view.name.clone());
        }
    }
    Ok(())
}

/// Claude Desktop 3P only speaks Anthropic Messages; every other SDK family reaches it through
/// the local router, which converts the request on the way out.
fn desktop_api_format(npm: &str) -> Option<&'static str> {
    match npm {
        "@ai-sdk/anthropic" => Some("anthropic"),
        "@ai-sdk/openai" => Some("openai_responses"),
        "@ai-sdk/openai-compatible" => Some("openai_chat"),
        _ => None,
    }
}

/// A Claude Desktop card in local-route mode: Desktop keeps its four role slots, so the source's
/// models are spread across them in order and the first one repeats when there are fewer models
/// than slots.
fn desktop_provider(view: &SourceView) -> Result<Provider, AppError> {
    let api_format = desktop_api_format(&view.npm).ok_or_else(|| {
        AppError::InvalidInput(format!(
            "Claude Desktop cannot route npm package '{}' of '{}'",
            view.npm, view.name
        ))
    })?;
    let models: Vec<String> = view.models.iter().map(|(id, _)| id.clone()).collect();
    if models.is_empty() {
        return Err(AppError::InvalidInput(format!(
            "OpenCode provider '{}' has no models to show in Claude Desktop",
            view.name
        )));
    }

    let mut routes = std::collections::HashMap::new();
    for (index, route) in crate::claude_desktop_config::DEFAULT_PROXY_ROUTES
        .iter()
        .enumerate()
    {
        let model = models[index % models.len()].clone();
        routes.insert(
            route.route_id.to_string(),
            crate::provider::ClaudeDesktopModelRoute {
                label_override: Some(model.clone()),
                model,
                supports_1m: Some(route.supports_1m),
            },
        );
    }

    let mut provider = Provider::with_id(
        uuid::Uuid::new_v4().to_string(),
        view.name.clone(),
        json!({
            "env": {
                "ANTHROPIC_BASE_URL": view.base_url,
                "ANTHROPIC_AUTH_TOKEN": view.api_key,
            },
        }),
        Some(view.base_url.clone()),
    );
    provider.meta = Some(ProviderMeta {
        endpoint_auto_select: Some(true),
        api_format: Some(api_format.to_string()),
        claude_desktop_mode: Some(crate::provider::ClaudeDesktopMode::Proxy),
        claude_desktop_model_routes: routes,
        ..Default::default()
    });
    Ok(provider)
}

fn sync_desktop(
    state: &AppState,
    source_app: AppType,
    source: &Provider,
    report: &mut SyncReport,
) -> Result<(), AppError> {
    let view = source_view(source_app, source)?;
    let existing = ProviderService::list(state, AppType::ClaudeDesktop)?
        .into_values()
        .find(|provider| provider.name.eq_ignore_ascii_case(&view.name));

    match existing {
        Some(current) => {
            let id = current.id.clone();
            let mut provider = desktop_provider(&view)?;
            provider.id = id.clone();
            ProviderService::update(state, AppType::ClaudeDesktop, Some(&id), provider)?;
            report.updated.push(view.name.clone());
        }
        None => {
            let provider = desktop_provider(&view)?;
            ProviderService::add(state, AppType::ClaudeDesktop, provider, false)?;
            report.created.push(view.name.clone());
        }
    }
    Ok(())
}

/// Claude Code reads its gateway from the `env` block of `~/.claude/settings.json`. Its model
/// aliases are role slots like Desktop's, so the source's models fill the default plus the
/// haiku / sonnet / opus defaults in order, repeating when the source has fewer models.
fn claude_provider(view: &SourceView) -> Result<Provider, AppError> {
    let api_format = desktop_api_format(&view.npm).ok_or_else(|| {
        AppError::InvalidInput(format!(
            "Claude Code cannot route npm package '{}' of '{}'",
            view.npm, view.name
        ))
    })?;
    let models: Vec<String> = view.models.iter().map(|(id, _)| id.clone()).collect();

    let mut env = Map::new();
    env.insert("ANTHROPIC_BASE_URL".to_string(), json!(view.base_url));
    env.insert("ANTHROPIC_AUTH_TOKEN".to_string(), json!(view.api_key));
    if let Some(first) = models.first() {
        env.insert("ANTHROPIC_MODEL".to_string(), json!(first));
        if models.len() > 1 {
            let alias = |offset: usize| models[offset % models.len()].clone();
            env.insert("ANTHROPIC_DEFAULT_HAIKU_MODEL".to_string(), json!(alias(1)));
            env.insert("ANTHROPIC_DEFAULT_SONNET_MODEL".to_string(), json!(alias(2)));
            env.insert("ANTHROPIC_DEFAULT_OPUS_MODEL".to_string(), json!(alias(3)));
        }
    }

    let mut provider = Provider::with_id(
        uuid::Uuid::new_v4().to_string(),
        view.name.clone(),
        json!({ "env": Value::Object(env) }),
        Some(view.base_url.clone()),
    );
    provider.meta = Some(ProviderMeta {
        endpoint_auto_select: Some(true),
        api_format: Some(api_format.to_string()),
        ..Default::default()
    });
    Ok(provider)
}

fn sync_claude(
    state: &AppState,
    source_app: AppType,
    source: &Provider,
    report: &mut SyncReport,
) -> Result<(), AppError> {
    let view = source_view(source_app, source)?;
    let existing = ProviderService::list(state, AppType::Claude)?
        .into_values()
        .find(|provider| provider.name.eq_ignore_ascii_case(&view.name));

    match existing {
        Some(current) => {
            let id = current.id.clone();
            let mut provider = claude_provider(&view)?;
            provider.id = id.clone();
            ProviderService::update(state, AppType::Claude, Some(&id), provider)?;
            report.updated.push(view.name.clone());
        }
        None => {
            let provider = claude_provider(&view)?;
            ProviderService::add(state, AppType::Claude, provider, false)?;
            report.created.push(view.name.clone());
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn opencode_provider(npm: &str) -> Provider {
        Provider::with_id(
            "sample".to_string(),
            "Sample".to_string(),
            json!({
                "npm": npm,
                "name": "Sample",
                "options": { "baseURL": "https://api.example.com/v1/", "apiKey": "sk-test" },
                "models": {
                    "model-a": {
                        "name": "Model A",
                        "limit": { "context": 200000, "output": 64000 },
                        "modalities": { "input": ["text", "image"] }
                    }
                }
            }),
            Some("https://api.example.com/v1/".to_string()),
        )
    }

    #[test]
    fn maps_sdk_families_to_target_apis() {
        assert_eq!(dsh_api("@ai-sdk/openai"), Some("openai-responses"));
        assert_eq!(
            dsh_api("@ai-sdk/openai-compatible"),
            Some("openai-completions")
        );
        assert_eq!(dsh_api("@ai-sdk/anthropic"), Some("anthropic-messages"));
        assert_eq!(dsh_api("@ai-sdk/google"), None);
        assert_eq!(codex_wire_api("@ai-sdk/openai"), Some("responses"));
        assert_eq!(codex_wire_api("@ai-sdk/openai-compatible"), Some("chat"));
        assert_eq!(codex_wire_api("@ai-sdk/anthropic"), None);
    }

    #[test]
    fn dsh_row_keeps_key_url_and_model_labels() {
        let view = SourceView::of(&opencode_provider("@ai-sdk/openai-compatible")).unwrap();
        assert_eq!(view.base_url, "https://api.example.com/v1");
        let provider = dsh_provider(&view).unwrap();
        assert_eq!(provider.id, "sample");
        assert_eq!(provider.settings_config["api"], "openai-completions");
        assert_eq!(
            provider.settings_config["options"]["apiKey"],
            Value::String("sk-test".into())
        );
        assert_eq!(
            provider.settings_config["models"]["model-a"]["name"],
            "Model A"
        );
        assert_eq!(
            provider.settings_config["models"]["model-a"]["limit"]["context"],
            200000
        );
        assert_eq!(
            provider
                .meta
                .as_ref()
                .and_then(|meta| meta.endpoint_auto_select),
            Some(true)
        );
    }

    #[test]
    fn dsh_row_drops_modalities_dsh_would_reject() {
        let mut provider = opencode_provider("@ai-sdk/openai-compatible");
        provider.settings_config["models"]["model-a"]["modalities"]["input"] =
            json!(["text", "image", "pdf"]);
        provider.settings_config["models"]["model-b"] = json!({
            "name": "Model B",
            "modalities": { "input": ["pdf"] }
        });
        let view = SourceView::of(&provider).unwrap();
        let row = dsh_provider(&view).unwrap();
        assert_eq!(
            row.settings_config["models"]["model-a"]["modalities"]["input"],
            json!(["text", "image"])
        );
        // Nothing DSH accepts is left, so the field is omitted instead of written empty.
        assert!(row.settings_config["models"]["model-b"]
            .get("modalities")
            .is_none());
    }

    #[test]
    fn codex_row_emits_a_loadable_route() {
        let view = SourceView::of(&opencode_provider("@ai-sdk/openai")).unwrap();
        let provider = codex_provider(&view, "row-id".to_string()).unwrap();
        let config = provider.settings_config["config"].as_str().unwrap();
        assert!(config.contains("model_provider = \"custom\""));
        assert!(config.contains("model = \"model-a\""));
        assert!(config.contains("[model_providers.custom]"));
        assert!(config.contains("wire_api = \"responses\""));
        assert!(config.contains("base_url = \"https://api.example.com/v1\""));
        assert_eq!(
            provider.settings_config["auth"]["OPENAI_API_KEY"],
            Value::String("sk-test".into())
        );
        assert_eq!(
            provider
                .meta
                .as_ref()
                .and_then(|meta| meta.api_format.as_deref()),
            Some("openai_responses")
        );
        // Must parse as TOML, otherwise Codex refuses the whole file.
        let parsed: toml::Value = toml::from_str(config).expect("valid toml");
        assert_eq!(
            parsed["model_providers"]["custom"]["base_url"].as_str(),
            Some("https://api.example.com/v1")
        );
    }

    #[test]
    fn anthropic_families_are_skipped_for_codex() {
        let view = SourceView::of(&opencode_provider("@ai-sdk/anthropic")).unwrap();
        assert!(codex_provider(&view, "row-id".to_string()).is_err());
        // DSH can still take it.
        assert!(dsh_provider(&view).is_ok());
    }

    fn reasoning_model(reasoning: Value, variants: Value) -> Provider {
        Provider::with_id(
            "sample".to_string(),
            "Sample".to_string(),
            json!({
                "npm": "@ai-sdk/openai",
                "name": "Sample",
                "options": { "baseURL": "https://api.example.com/v1", "apiKey": "sk-test" },
                "models": {
                    "model-a": {
                        "name": "Model A",
                        "reasoning": reasoning,
                        "variants": variants
                    }
                }
            }),
            Some("https://api.example.com/v1".to_string()),
        )
    }

    #[test]
    fn dsh_row_keeps_opencode_thinking_variants_as_efforts() {
        let source = reasoning_model(
            json!(true),
            json!({
                "low": { "reasoningEffort": "low" },
                "max": { "reasoningEffort": "max" }
            }),
        );
        let view = SourceView::of(&source).unwrap();
        let provider = dsh_provider(&view).unwrap();
        let efforts = &provider.settings_config["models"]["model-a"]["reasoningEfforts"];
        assert_eq!(efforts["low"], "low");
        assert_eq!(efforts["max"], "max");
        // DSH exposes an explicit "off" level; it must be first-class, not missing.
        assert!(efforts.get("off").is_some());
    }

    #[test]
    fn dsh_row_marks_non_reasoning_models() {
        let source = reasoning_model(json!(false), Value::Null);
        let view = SourceView::of(&source).unwrap();
        let provider = dsh_provider(&view).unwrap();
        assert_eq!(
            provider.settings_config["models"]["model-a"]["reasoningEfforts"],
            Value::Bool(false)
        );
    }

    #[test]
    fn dsh_row_keeps_reasoning_flag_when_variants_are_missing() {
        let source = reasoning_model(json!(true), Value::Null);
        let view = SourceView::of(&source).unwrap();
        let provider = dsh_provider(&view).unwrap();
        // Let dsh_config fill in its standard effort set.
        assert_eq!(
            provider.settings_config["models"]["model-a"]["reasoning"],
            Value::Bool(true)
        );
    }

    #[test]
    fn desktop_row_hands_the_relay_to_the_local_proxy() {
        let view = SourceView::of(&opencode_provider("@ai-sdk/anthropic")).unwrap();
        let provider = desktop_provider(&view).unwrap();

        assert_eq!(
            provider.settings_config["env"]["ANTHROPIC_BASE_URL"],
            Value::String("https://api.example.com/v1".into())
        );
        assert_eq!(
            provider.settings_config["env"]["ANTHROPIC_AUTH_TOKEN"],
            Value::String("sk-test".into())
        );

        let meta = provider.meta.expect("meta");
        assert_eq!(
            meta.claude_desktop_mode,
            Some(crate::provider::ClaudeDesktopMode::Proxy)
        );
        assert_eq!(meta.api_format.as_deref(), Some("anthropic"));
        // Desktop keeps its four role slots; every one of them has to point somewhere.
        assert_eq!(
            meta.claude_desktop_model_routes.len(),
            crate::claude_desktop_config::DEFAULT_PROXY_ROUTES.len()
        );
        for route in crate::claude_desktop_config::DEFAULT_PROXY_ROUTES {
            let mapped = meta
                .claude_desktop_model_routes
                .get(route.route_id)
                .expect("route is mapped");
            assert_eq!(mapped.model, "model-a");
        }
    }

    #[test]
    fn desktop_row_rejects_unknown_sdk_families() {
        let view = SourceView::of(&opencode_provider("@ai-sdk/google")).unwrap();
        assert!(desktop_provider(&view).is_err());
    }

    #[test]
    fn claude_row_writes_the_gateway_env() {
        let view = SourceView::of(&opencode_provider("@ai-sdk/anthropic")).unwrap();
        let provider = claude_provider(&view).unwrap();
        let env = &provider.settings_config["env"];

        assert_eq!(env["ANTHROPIC_BASE_URL"], "https://api.example.com/v1");
        assert_eq!(env["ANTHROPIC_AUTH_TOKEN"], "sk-test");
        assert_eq!(env["ANTHROPIC_MODEL"], "model-a");
        // One model cannot fill the role aliases; leaving them out keeps Claude Code's own
        // defaults instead of inventing entries.
        assert!(env.get("ANTHROPIC_DEFAULT_OPUS_MODEL").is_none());
        assert_eq!(
            provider.meta.expect("meta").api_format.as_deref(),
            Some("anthropic")
        );
    }

    #[test]
    fn claude_row_spreads_models_across_the_role_aliases() {
        let mut source = opencode_provider("@ai-sdk/openai");
        source.settings_config["models"] = json!({ "a": {}, "b": {}, "c": {}, "d": {} });
        let view = SourceView::of(&source).unwrap();
        let provider = claude_provider(&view).unwrap();
        let env = &provider.settings_config["env"];

        assert_eq!(env["ANTHROPIC_MODEL"], "a");
        let mut aliases: Vec<&str> = [
            "ANTHROPIC_DEFAULT_HAIKU_MODEL",
            "ANTHROPIC_DEFAULT_SONNET_MODEL",
            "ANTHROPIC_DEFAULT_OPUS_MODEL",
        ]
        .iter()
        .filter_map(|key| env.get(*key).and_then(Value::as_str))
        .collect();
        aliases.sort_unstable();
        assert_eq!(aliases, vec!["b", "c", "d"]);
    }

    #[test]
    fn claude_row_rejects_unknown_sdk_families() {
        let view = SourceView::of(&opencode_provider("@ai-sdk/google")).unwrap();
        assert!(claude_provider(&view).is_err());
    }

    fn claude_row() -> Provider {
        Provider::with_id(
            "claude-relay".to_string(),
            "Claude Relay".to_string(),
            json!({
                "env": {
                    "ANTHROPIC_BASE_URL": "https://claude.example.com/",
                    "ANTHROPIC_AUTH_TOKEN": "sk-claude",
                    "ANTHROPIC_MODEL": "claude-sonnet-5",
                    "ANTHROPIC_DEFAULT_OPUS_MODEL": "claude-opus-5"
                }
            }),
            None,
        )
    }

    fn desktop_row() -> Provider {
        let mut provider = Provider::with_id(
            "desktop-relay".to_string(),
            "Desktop Relay".to_string(),
            json!({
                "env": {
                    "ANTHROPIC_BASE_URL": "https://desktop.example.com",
                    "ANTHROPIC_AUTH_TOKEN": "sk-desktop"
                }
            }),
            None,
        );
        provider.meta = Some(ProviderMeta {
            claude_desktop_mode: Some(crate::provider::ClaudeDesktopMode::Proxy),
            claude_desktop_model_routes: [(
                "claude-sonnet-5".to_string(),
                crate::provider::ClaudeDesktopModelRoute {
                    model: "gpt-5.6-sol".to_string(),
                    label_override: Some("Sol".to_string()),
                    supports_1m: Some(true),
                },
            )]
            .into_iter()
            .collect(),
            ..Default::default()
        });
        provider
    }

    #[test]
    fn claude_row_feeds_the_desktop_route_table() {
        let view = source_view(AppType::Claude, &claude_row()).unwrap();
        assert_eq!(view.npm, "@ai-sdk/anthropic");
        assert_eq!(view.base_url, "https://claude.example.com");
        assert_eq!(view.api_key, "sk-claude");
        assert_eq!(view.models.len(), 2);

        let provider = desktop_provider(&view).unwrap();
        assert_eq!(
            provider.settings_config["env"]["ANTHROPIC_BASE_URL"],
            json!("https://claude.example.com")
        );
        let routes = &provider
            .meta
            .as_ref()
            .expect("desktop rows always carry meta")
            .claude_desktop_model_routes;
        assert_eq!(routes["claude-sonnet-5"].model, "claude-sonnet-5");
        assert_eq!(routes["claude-opus-5"].model, "claude-opus-5");
    }

    #[test]
    fn desktop_row_feeds_the_claude_aliases() {
        let view = source_view(AppType::ClaudeDesktop, &desktop_row()).unwrap();
        assert_eq!(view.npm, "@ai-sdk/anthropic");
        assert_eq!(view.base_url, "https://desktop.example.com");
        assert_eq!(view.models.len(), 1);
        assert_eq!(view.models[0].0, "gpt-5.6-sol");

        let provider = claude_provider(&view).unwrap();
        assert_eq!(
            provider.settings_config["env"]["ANTHROPIC_MODEL"],
            json!("gpt-5.6-sol")
        );
        assert_eq!(
            provider.settings_config["env"]["ANTHROPIC_AUTH_TOKEN"],
            json!("sk-desktop")
        );
    }

    #[test]
    fn sync_pairs_stay_inside_the_shipped_matrix() {
        assert!(ensure_target(AppType::OpenCode, AppType::Claude).is_ok());
        assert!(ensure_target(AppType::Claude, AppType::ClaudeDesktop).is_ok());
        assert!(ensure_target(AppType::ClaudeDesktop, AppType::Claude).is_ok());
        assert!(ensure_target(AppType::Claude, AppType::Dsh).is_err());
        assert!(ensure_target(AppType::Codex, AppType::Claude).is_err());
    }
}
