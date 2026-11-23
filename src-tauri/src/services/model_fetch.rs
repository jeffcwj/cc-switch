//! 模型列表获取服务
//!
//! 通过 OpenAI 兼容的 GET /v1/models 端点获取供应商可用模型列表。
//! 主要面向第三方聚合站（硅基流动、OpenRouter 等），以及把 Anthropic
//! 协议挂在兼容子路径上的官方供应商（DeepSeek、Kimi、智谱 GLM 等）。

use reqwest::header::{HeaderMap, HeaderName, HeaderValue, AUTHORIZATION, USER_AGENT};
use reqwest::StatusCode;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::time::Duration;

/// 获取到的模型信息
///
/// 只有 `id` 是必然存在的。`/models` 端点愿意多说的能力字段（上下文窗口、
/// 输出上限、输入模态、是否支持推理）一律可选：自建中转通常一个字都不说，
/// OpenRouter 这类会把它们全带上。调用方拿到 `None` 就当端点没声明。
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FetchedModel {
    pub id: String,
    pub owned_by: Option<String>,
    /// 端点自报的上下文窗口（`context_length` / `limit.context` 等别名）。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub context_window: Option<u64>,
    /// 端点自报的单次输出上限。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_output_tokens: Option<u64>,
    /// 端点自报的输入模态，只保留已知的几种。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub input_modalities: Option<Vec<String>>,
    /// 端点自报的推理能力；`None` 表示端点没提。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reasoning: Option<bool>,
}

/// 模型列表响应的兼容格式。
///
/// OpenAI 兼容接口和 Anthropic 接口使用 `data` 字段；Codex 远端模型目录
/// （`{base}/models`，智谱 /api/v1 即此格式）使用 `models[].slug`。
///
/// `models` 收成 `Value` 而非强类型：部分供应商会在 `data` 旁附带任意形状的
/// `models`，而 serde 反序列化的是整个结构体，强类型字段一旦对不上就会连带
/// `data` 一起解析失败（#7593）。
#[derive(Debug, Deserialize)]
struct ModelsResponse {
    data: Option<Vec<ModelEntry>>,
    models: Option<serde_json::Value>,
}

#[derive(Debug, Deserialize)]
struct ModelEntry {
    id: String,
    owned_by: Option<String>,
    /// 供应商在 `/models` 里附带的能力字段形状各异，原样收下再归一化。
    #[serde(flatten)]
    rest: serde_json::Map<String, serde_json::Value>,
}

/// 端点在 `/models` 里报上下文窗口时用过的字段名。
///
/// 别名表照抄 DSH 的 `readListing`（`dsh-llm-pi-ai` 与它读同一批端点），
/// 再加 `limit.input` 这种把输入窗口单列的写法。
const CONTEXT_WINDOW_PATHS: &[&str] = &[
    "contextWindow",
    "context_window",
    "context_length",
    "max_input_tokens",
    "limit.context",
    "limit.input",
];

/// 端点报单次输出上限时用过的字段名（含 OpenRouter 的
/// `top_provider.max_completion_tokens`）。
const MAX_OUTPUT_PATHS: &[&str] = &[
    "maxOutputTokens",
    "max_output_tokens",
    "maxTokens",
    "max_tokens",
    "limit.output",
    "top_provider.max_completion_tokens",
];

/// 端点报输入模态时用过的字段名。
const INPUT_MODALITY_PATHS: &[&str] = &[
    "architecture.input_modalities",
    "input_modalities",
    "modalities.input",
    "input",
];

/// 端点报推理能力时用过的字段名。
const REASONING_PATHS: &[&str] = &["capabilities.reasoning", "reasoning"];

/// 已知的输入模态；其余取值（例如某些网关自造的 `file`）一律丢弃。
const KNOWN_MODALITIES: &[&str] = &["text", "image", "audio", "video", "pdf"];

/// 按 `a.b.c` 路径取一个字段，取不到就是 `None`。
fn lookup<'a>(
    map: &'a serde_json::Map<String, serde_json::Value>,
    path: &str,
) -> Option<&'a serde_json::Value> {
    let mut segments = path.split('.');
    let mut current = map.get(segments.next()?)?;
    for segment in segments {
        current = current.get(segment)?;
    }
    Some(current)
}

/// 正整数才认：`"64000"` 这种字符串也收，`0`、负数、小数、对象一律当没写。
fn positive_integer(value: Option<&serde_json::Value>) -> Option<u64> {
    let value = value?;
    if let Some(number) = value.as_u64() {
        return (number > 0).then_some(number);
    }
    value
        .as_str()?
        .trim()
        .parse::<u64>()
        .ok()
        .filter(|number| *number > 0)
}

fn advertised_capacity(
    entry: &serde_json::Map<String, serde_json::Value>,
    paths: &[&str],
) -> Option<u64> {
    paths
        .iter()
        .find_map(|path| positive_integer(lookup(entry, path)))
}

fn advertised_modalities(
    entry: &serde_json::Map<String, serde_json::Value>,
) -> Option<Vec<String>> {
    let declared = INPUT_MODALITY_PATHS
        .iter()
        .find_map(|path| lookup(entry, path)?.as_array())?;
    let mut modalities: Vec<String> = declared
        .iter()
        .filter_map(|modality| modality.as_str())
        .map(|modality| modality.trim().to_ascii_lowercase())
        .filter(|modality| KNOWN_MODALITIES.contains(&modality.as_str()))
        .collect();
    modalities.sort();
    modalities.dedup();
    (!modalities.is_empty()).then_some(modalities)
}

/// `reasoning: true`、`capabilities.reasoning: true`，或 `supported_parameters`
/// 里列了 `reasoning`，都算端点说「这个模型能推理」。
fn advertised_reasoning(entry: &serde_json::Map<String, serde_json::Value>) -> Option<bool> {
    if let Some(declared) = REASONING_PATHS
        .iter()
        .find_map(|path| lookup(entry, path)?.as_bool())
    {
        return Some(declared);
    }
    let parameters = lookup(entry, "supported_parameters")?.as_array()?;
    Some(
        parameters
            .iter()
            .filter_map(|parameter| parameter.as_str())
            .any(|parameter| parameter.eq_ignore_ascii_case("reasoning")),
    )
}

/// 把 `data[]` 的一条记录转成对外结构：能读到的能力字段都带上，读不到就留空。
fn fetched_from_entry(entry: ModelEntry) -> FetchedModel {
    let ModelEntry { id, owned_by, rest } = entry;
    FetchedModel {
        id,
        owned_by,
        context_window: advertised_capacity(&rest, CONTEXT_WINDOW_PATHS),
        max_output_tokens: advertised_capacity(&rest, MAX_OUTPUT_PATHS),
        input_modalities: advertised_modalities(&rest),
        reasoning: advertised_reasoning(&rest),
    }
}

/// `models[]` 条目对应的模型 id：`slug` 优先，回退 OpenAI 风格的 `id`；
/// 非数组、非对象、非字符串或空串一律跳过，绝不报错。
fn catalog_model_ids(models: Option<serde_json::Value>) -> Vec<String> {
    let Some(serde_json::Value::Array(entries)) = models else {
        return Vec::new();
    };
    entries
        .iter()
        .filter_map(|m| {
            ["slug", "id"]
                .iter()
                .find_map(|k| m.get(k)?.as_str().filter(|s| !s.is_empty()))
                .or_else(|| {
                    m.get("name")?
                        .as_str()?
                        .strip_prefix("models/")
                        .filter(|s| !s.is_empty())
                })
        })
        .map(str::to_string)
        .collect()
}

const FETCH_TIMEOUT_SECS: u64 = 15;
const MAX_REQUEST_HEADERS: usize = 64;
const MAX_HEADER_NAME_BYTES: usize = 256;
const MAX_HEADER_VALUE_BYTES: usize = 16 * 1024;

/// 404/405 响应体截断长度：避免把几十 KB HTML 404 页整页保留到错误串里。
const ERROR_BODY_MAX_CHARS: usize = 512;

/// 已知的「Anthropic 协议兼容子路径」后缀；按长度降序，最长前缀优先匹配。
/// baseURL 命中这些后缀时，候选列表会追加「剥离后缀再拼 /v1/models / /models」的版本。
const KNOWN_COMPAT_SUFFIXES: &[&str] = &[
    "/api/claudecode",
    "/api/anthropic",
    "/apps/anthropic",
    "/api/coding",
    "/claudecode",
    "/anthropic",
    "/step_plan",
    "/coding",
    "/claude",
];

/// 获取供应商的可用模型列表
///
/// 使用 OpenAI 兼容的 GET /v1/models 端点，按候选列表顺序尝试。
pub async fn fetch_models(
    base_url: &str,
    api_key: &str,
    is_full_url: bool,
    models_url_override: Option<&str>,
    user_agent: Option<HeaderValue>,
    api_format: Option<&str>,
    request_headers: Option<&BTreeMap<String, String>>,
) -> Result<Vec<FetchedModel>, String> {
    let candidates = if api_format == Some("google-generative-ai")
        && models_url_override.is_none()
        && !is_full_url
    {
        let base = base_url.trim_end_matches('/');
        let base = base
            .strip_suffix("/v1beta")
            .or_else(|| base.strip_suffix("/v1"))
            .unwrap_or(base);
        // 默认每页只回 50 个（Gemini / Gemma / Imagen / Veo 加起来早已超出），
        // 这里不翻页，直接要最大页。
        vec![format!("{base}/v1beta/models?pageSize=1000")]
    } else {
        build_models_url_candidates(base_url, is_full_url, models_url_override)?
    };
    let headers =
        build_model_fetch_headers(api_key, api_format, user_agent.as_ref(), request_headers)?;
    let mut last_err: Option<String> = None;
    let mut known_secrets = vec![api_key.to_string()];
    if let Some(request_headers) = request_headers {
        known_secrets.extend(request_headers.values().cloned());
    }

    for url in &candidates {
        log::debug!(
            "[ModelFetch] Trying endpoint: {}",
            crate::url_for_log_with_secrets(url, &known_secrets)
        );
        let request = crate::proxy::http_client::get_for_url(url)
            .get(url)
            .headers(headers.clone())
            .timeout(Duration::from_secs(FETCH_TIMEOUT_SECS));
        let response = match request.send().await {
            Ok(r) => r,
            Err(e) => {
                return Err(format!("Request failed: {e}"));
            }
        };

        let status = response.status();

        if status.is_success() {
            let resp: ModelsResponse = response
                .json()
                .await
                .map_err(|e| format!("Failed to parse response: {e}"))?;

            let mut models: Vec<FetchedModel> = if let Some(data) = resp.data {
                data.into_iter().map(fetched_from_entry).collect()
            } else {
                catalog_model_ids(resp.models)
                    .into_iter()
                    .map(|id| FetchedModel {
                        id,
                        ..FetchedModel::default()
                    })
                    .collect()
            };

            models.sort_by(|a, b| a.id.cmp(&b.id));
            return Ok(models);
        }

        if status == StatusCode::NOT_FOUND || status == StatusCode::METHOD_NOT_ALLOWED {
            let body = redact_model_fetch_error_body(
                response.text().await.unwrap_or_default(),
                &known_secrets,
            );
            last_err = Some(format!("HTTP {status}: {body}"));
            continue;
        }

        let body = redact_model_fetch_error_body(
            response.text().await.unwrap_or_default(),
            &known_secrets,
        );
        return Err(format!("HTTP {status}: {body}"));
    }

    Err(format!(
        "All candidates failed: {}",
        last_err.unwrap_or_else(|| "no candidates".to_string())
    ))
}

fn redact_model_fetch_error_body(body: String, known_secrets: &[String]) -> String {
    truncate_body(crate::redact_known_secrets_strict(&body, known_secrets))
}

fn build_model_fetch_headers(
    api_key: &str,
    api_format: Option<&str>,
    user_agent: Option<&HeaderValue>,
    request_headers: Option<&BTreeMap<String, String>>,
) -> Result<HeaderMap, String> {
    let custom_count = request_headers.map_or(0, BTreeMap::len);
    if api_key.is_empty() && custom_count == 0 {
        return Err("API Key or request headers are required to fetch models".to_string());
    }
    if custom_count > MAX_REQUEST_HEADERS {
        return Err(format!(
            "Too many model-fetch request headers (maximum {MAX_REQUEST_HEADERS})"
        ));
    }

    let mut headers = HeaderMap::new();
    if !api_key.is_empty() {
        let (name, value) = match api_format {
            Some("anthropic-messages") => (
                HeaderName::from_static("x-api-key"),
                HeaderValue::from_str(api_key)
                    .map_err(|error| format!("Invalid API Key header value: {error}"))?,
            ),
            Some("google-generative-ai") => (
                HeaderName::from_static("x-goog-api-key"),
                HeaderValue::from_str(api_key)
                    .map_err(|error| format!("Invalid API Key header value: {error}"))?,
            ),
            _ => (
                AUTHORIZATION,
                HeaderValue::from_str(&format!("Bearer {api_key}"))
                    .map_err(|error| format!("Invalid API Key header value: {error}"))?,
            ),
        };
        headers.insert(name, value);
    }

    if let Some(user_agent) = user_agent {
        headers.insert(USER_AGENT, user_agent.clone());
    }

    if let Some(request_headers) = request_headers {
        for (raw_name, raw_value) in request_headers {
            let name = raw_name.trim();
            if name.is_empty() || name.len() > MAX_HEADER_NAME_BYTES {
                return Err(format!("Invalid model-fetch header name: {raw_name}"));
            }
            if raw_value.len() > MAX_HEADER_VALUE_BYTES {
                return Err(format!("Model-fetch header value is too large: {name}"));
            }
            let name = HeaderName::from_bytes(name.as_bytes())
                .map_err(|error| format!("Invalid model-fetch header name {name}: {error}"))?;
            let value = HeaderValue::from_str(raw_value)
                .map_err(|error| format!("Invalid model-fetch header value for {name}: {error}"))?;
            headers.insert(name, value);
        }
    }

    // 兜底 User-Agent：reqwest 默认不发 UA，而前置 Cloudflare/WAF 的端点对
    // 无 UA 请求一律 403，且会被前端误读成「API Key 无效」（#7427）。
    // 供应商自定义 UA 与请求头里的 UA 都在上面 insert 过，优先级高于兜底。
    headers
        .entry(USER_AGENT)
        .or_insert_with(|| HeaderValue::from_static("cc-switch"));

    Ok(headers)
}

/// 构造「模型列表端点」的候选 URL 列表
///
/// 候选顺序：
/// 1. `models_url_override` 非空 → 只返回它
/// 2. baseURL 拼 `/v1/models`；若已以版本段 `/v{N}` 结尾（`/v1`、智谱
///    `/api/coding/paas/v4` 等），版本号已在路径里，改拼 `/models`
/// 3. 版本段非 `/v1`（如 `/v4`）时再追加 `/v1/models` 作为兜底次候选
/// 4. 若 baseURL 命中 [`KNOWN_COMPAT_SUFFIXES`]，剥离后缀再拼 `/v1/models`、`/models`
///
/// 结果已去重且保持首次出现顺序。
pub fn build_models_url_candidates(
    base_url: &str,
    is_full_url: bool,
    models_url_override: Option<&str>,
) -> Result<Vec<String>, String> {
    if let Some(raw) = models_url_override {
        let trimmed = raw.trim();
        if !trimmed.is_empty() {
            return Ok(vec![trimmed.to_string()]);
        }
    }

    let trimmed = base_url.trim().trim_end_matches('/');
    if trimmed.is_empty() {
        return Err("Base URL is empty".to_string());
    }

    let mut candidates: Vec<String> = Vec::new();

    if is_full_url {
        if let Some(idx) = trimmed.find("/v1/") {
            candidates.push(format!("{}/v1/models", &trimmed[..idx]));
        } else if let Some(idx) = trimmed.rfind('/') {
            let root = &trimmed[..idx];
            if root.contains("://") && root.len() > root.find("://").unwrap() + 3 {
                candidates.push(format!("{root}/v1/models"));
            }
        }
        if candidates.is_empty() {
            return Err("Cannot derive models endpoint from full URL".to_string());
        }
        return Ok(candidates);
    }

    // baseURL 已以版本段 /v{N} 结尾时（如 `/v1`、智谱 `/api/coding/paas/v4`），
    // OpenAI 惯例的模型端点是 `{base}/models`，不能再补 `/v1`
    // （否则 .../coding/paas/v4/v1/models → 404）。
    if ends_with_version_segment(trimmed) {
        candidates.push(format!("{trimmed}/models"));
        // 版本段非 /v1 时，保留旧的 /v1/models 作为兜底次候选（正确路径已在前）。
        if !trimmed.ends_with("/v1") {
            candidates.push(format!("{trimmed}/v1/models"));
        }
    } else {
        candidates.push(format!("{trimmed}/v1/models"));
    }

    if let Some(stripped) = strip_compat_suffix(trimmed) {
        let root = stripped.trim_end_matches('/');
        if !root.is_empty() && root.contains("://") {
            candidates.push(format!("{root}/v1/models"));
            candidates.push(format!("{root}/models"));
            // 阿里云百炼 / 千问 Token Plan：`/apps/anthropic` 入口不提供 /v1/models，
            // 模型列表只在 OpenAI 兼容路径 `/compatible-mode/v1/models` 下。
            if trimmed.ends_with("/apps/anthropic") {
                candidates.push(format!("{root}/compatible-mode/v1/models"));
            }
        }
    }

    // 候选最多 4 条，线性去重即可，不值得上 HashSet。
    let mut unique: Vec<String> = Vec::with_capacity(candidates.len());
    for url in candidates {
        if !unique.iter().any(|u| u == &url) {
            unique.push(url);
        }
    }

    Ok(unique)
}

/// 截断响应体到 [`ERROR_BODY_MAX_CHARS`] 字符，避免 HTML 404 页占用错误串。
fn truncate_body(body: String) -> String {
    if body.chars().count() <= ERROR_BODY_MAX_CHARS {
        body
    } else {
        let mut s: String = body.chars().take(ERROR_BODY_MAX_CHARS).collect();
        s.push('…');
        s
    }
}

/// 若 baseURL 以任一已知兼容子路径结尾，返回剥离后的剩余部分；否则 `None`。
///
/// 依赖 [`KNOWN_COMPAT_SUFFIXES`] 按长度降序排列，确保最长前缀优先命中
/// （否则 `/anthropic` 会提前匹配掉 `/api/anthropic` 的场景）。
fn strip_compat_suffix(base_url: &str) -> Option<&str> {
    for suffix in KNOWN_COMPAT_SUFFIXES {
        if base_url.ends_with(*suffix) {
            return Some(&base_url[..base_url.len() - suffix.len()]);
        }
    }
    None
}

/// 判断 baseURL 是否以 OpenAI 风格的版本段 `/v{N}` 结尾（`N` 为一个或多个数字），
/// 例如 `/v1`、`.../paas/v4`。这类 URL 版本号已在路径中，模型端点应为
/// `{base}/models`，不能再补 `/v1`（智谱 Coding Plan 即 `.../coding/paas/v4`）。
fn ends_with_version_segment(url: &str) -> bool {
    let last = url.rsplit('/').next().unwrap_or("");
    last.strip_prefix('v')
        .is_some_and(|digits| !digits.is_empty() && digits.bytes().all(|b| b.is_ascii_digit()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn model_fetch_headers_follow_pi_api_format() {
        let anthropic =
            build_model_fetch_headers("anthropic-key", Some("anthropic-messages"), None, None)
                .unwrap();
        assert_eq!(anthropic["x-api-key"], "anthropic-key");
        assert!(!anthropic.contains_key(AUTHORIZATION));

        let google =
            build_model_fetch_headers("google-key", Some("google-generative-ai"), None, None)
                .unwrap();
        assert_eq!(google["x-goog-api-key"], "google-key");
        assert!(!google.contains_key(AUTHORIZATION));

        let openai =
            build_model_fetch_headers("openai-key", Some("openai-responses"), None, None).unwrap();
        assert_eq!(openai[AUTHORIZATION], "Bearer openai-key");
    }

    #[test]
    fn model_fetch_headers_default_user_agent_when_unset() {
        // #7427：无 User-Agent 的请求会被前置 Cloudflare/WAF 的端点一律 403，
        // 且被前端误读成「API Key 无效」。未配置任何 UA 时补默认值。
        let headers = build_model_fetch_headers("key", None, None, None).unwrap();
        assert_eq!(headers[USER_AGENT], "cc-switch");

        // 纯请求头鉴权（无 api_key）同样要带默认 UA。
        let header_only = BTreeMap::from([("Authorization".to_string(), "Token x".to_string())]);
        let headers = build_model_fetch_headers("", None, None, Some(&header_only)).unwrap();
        assert_eq!(headers[USER_AGENT], "cc-switch");
    }

    #[test]
    fn model_fetch_headers_keep_explicit_user_agent_priority() {
        // 供应商自定义 UA 优先于默认值；请求头里的 UA 优先于两者。
        let custom = HeaderValue::from_str("my-agent/1.0").unwrap();
        let headers = build_model_fetch_headers("key", None, Some(&custom), None).unwrap();
        assert_eq!(headers[USER_AGENT], "my-agent/1.0");

        let request_headers =
            BTreeMap::from([("User-Agent".to_string(), "header-agent/2.0".to_string())]);
        let headers =
            build_model_fetch_headers("key", None, Some(&custom), Some(&request_headers)).unwrap();
        assert_eq!(headers[USER_AGENT], "header-agent/2.0");
    }

    #[test]
    fn model_fetch_headers_allow_validated_header_only_auth_and_overrides() {
        let custom = BTreeMap::from([
            ("Authorization".to_string(), "Token literal".to_string()),
            ("X-Tenant".to_string(), "tenant-a".to_string()),
        ]);
        let headers =
            build_model_fetch_headers("", Some("openai-completions"), None, Some(&custom)).unwrap();
        assert_eq!(headers[AUTHORIZATION], "Token literal");
        assert_eq!(headers["x-tenant"], "tenant-a");

        let override_default =
            BTreeMap::from([("x-api-key".to_string(), "header-managed-key".to_string())]);
        let headers = build_model_fetch_headers(
            "provider-key",
            Some("anthropic-messages"),
            None,
            Some(&override_default),
        )
        .unwrap();
        assert_eq!(headers["x-api-key"], "header-managed-key");
    }

    #[test]
    fn model_fetch_headers_reject_invalid_or_missing_credentials() {
        assert!(build_model_fetch_headers("", None, None, None).is_err());
        let invalid = BTreeMap::from([("bad header".to_string(), "literal-value".to_string())]);
        assert!(build_model_fetch_headers("", None, None, Some(&invalid)).is_err());
    }

    #[test]
    fn model_fetch_error_body_redacts_known_header_credentials() {
        let secrets = vec![
            "short".to_string(),
            "Bearer literal-header-secret".to_string(),
        ];
        let body = redact_model_fetch_error_body(
            "invalid short / Bearer literal-header-secret".to_string(),
            &secrets,
        );
        assert_eq!(body, "invalid [REDACTED] / [REDACTED]");
    }

    #[test]
    fn test_candidates_plain_root() {
        let c = build_models_url_candidates("https://api.siliconflow.cn", false, None).unwrap();
        assert_eq!(c, vec!["https://api.siliconflow.cn/v1/models"]);
    }

    #[test]
    fn test_candidates_trailing_slash() {
        let c = build_models_url_candidates("https://api.example.com/", false, None).unwrap();
        assert_eq!(c, vec!["https://api.example.com/v1/models"]);
    }

    #[test]
    fn test_candidates_with_v1() {
        let c = build_models_url_candidates("https://api.example.com/v1", false, None).unwrap();
        assert_eq!(c, vec!["https://api.example.com/v1/models"]);
    }

    #[test]
    fn test_candidates_zhipu_coding_paas_v4() {
        // 智谱 Coding Plan 端点以 /v4 版本段结尾：模型端点是 {base}/models，
        // 正确路径必须排在 .../v4/v1/models（404）之前。
        let c =
            build_models_url_candidates("https://open.bigmodel.cn/api/coding/paas/v4", false, None)
                .unwrap();
        assert_eq!(
            c,
            vec![
                "https://open.bigmodel.cn/api/coding/paas/v4/models",
                "https://open.bigmodel.cn/api/coding/paas/v4/v1/models",
            ]
        );
    }

    #[test]
    fn test_candidates_zai_coding_paas_v4() {
        let c = build_models_url_candidates("https://api.z.ai/api/coding/paas/v4", false, None)
            .unwrap();
        assert_eq!(
            c,
            vec![
                "https://api.z.ai/api/coding/paas/v4/models",
                "https://api.z.ai/api/coding/paas/v4/v1/models",
            ]
        );
    }

    #[test]
    fn test_ends_with_version_segment() {
        assert!(ends_with_version_segment("https://x.com/v1"));
        assert!(ends_with_version_segment(
            "https://open.bigmodel.cn/api/coding/paas/v4"
        ));
        assert!(ends_with_version_segment("https://x.com/v10"));
        assert!(!ends_with_version_segment("https://x.com/api"));
        assert!(!ends_with_version_segment("https://x.com/vX"));
        assert!(!ends_with_version_segment("https://x.com/models"));
        assert!(!ends_with_version_segment("https://api.siliconflow.cn"));
    }

    #[test]
    fn test_candidates_full_url() {
        let c = build_models_url_candidates(
            "https://proxy.example.com/v1/chat/completions",
            true,
            None,
        )
        .unwrap();
        assert_eq!(c, vec!["https://proxy.example.com/v1/models"]);
    }

    #[test]
    fn test_candidates_empty() {
        assert!(build_models_url_candidates("", false, None).is_err());
    }

    #[test]
    fn test_candidates_override_returns_single() {
        let c = build_models_url_candidates(
            "https://api.deepseek.com/anthropic",
            false,
            Some("https://api.deepseek.com/models"),
        )
        .unwrap();
        assert_eq!(c, vec!["https://api.deepseek.com/models"]);
    }

    #[test]
    fn test_candidates_override_empty_falls_through() {
        let c =
            build_models_url_candidates("https://api.siliconflow.cn", false, Some("   ")).unwrap();
        assert_eq!(c, vec!["https://api.siliconflow.cn/v1/models"]);
    }

    #[test]
    fn test_candidates_deepseek_strip_anthropic() {
        let c =
            build_models_url_candidates("https://api.deepseek.com/anthropic", false, None).unwrap();
        assert_eq!(
            c,
            vec![
                "https://api.deepseek.com/anthropic/v1/models",
                "https://api.deepseek.com/v1/models",
                "https://api.deepseek.com/models",
            ]
        );
    }

    #[test]
    fn test_candidates_zhipu_strip_api_anthropic() {
        let c = build_models_url_candidates("https://open.bigmodel.cn/api/anthropic", false, None)
            .unwrap();
        assert_eq!(
            c,
            vec![
                "https://open.bigmodel.cn/api/anthropic/v1/models",
                "https://open.bigmodel.cn/v1/models",
                "https://open.bigmodel.cn/models",
            ]
        );
    }

    #[test]
    fn test_candidates_bailian_strip_apps_anthropic() {
        let c = build_models_url_candidates(
            "https://dashscope.aliyuncs.com/apps/anthropic",
            false,
            None,
        )
        .unwrap();
        assert_eq!(
            c,
            vec![
                "https://dashscope.aliyuncs.com/apps/anthropic/v1/models",
                "https://dashscope.aliyuncs.com/v1/models",
                "https://dashscope.aliyuncs.com/models",
                "https://dashscope.aliyuncs.com/compatible-mode/v1/models",
            ]
        );
    }

    #[test]
    fn test_candidates_qianwen_token_plan_compatible_mode() {
        let c = build_models_url_candidates(
            "https://token-plan.maas.qianwenaiapi.com/apps/anthropic/",
            false,
            None,
        )
        .unwrap();
        assert_eq!(
            c.last().map(String::as_str),
            Some("https://token-plan.maas.qianwenaiapi.com/compatible-mode/v1/models")
        );
    }

    #[test]
    fn test_candidates_plain_anthropic_suffix_skips_compatible_mode() {
        let c =
            build_models_url_candidates("https://api.example.com/anthropic", false, None).unwrap();
        assert!(!c.iter().any(|u| u.contains("compatible-mode")));
    }

    #[test]
    fn test_candidates_stepfun_strip_step_plan() {
        let c =
            build_models_url_candidates("https://api.stepfun.com/step_plan", false, None).unwrap();
        assert_eq!(
            c,
            vec![
                "https://api.stepfun.com/step_plan/v1/models",
                "https://api.stepfun.com/v1/models",
                "https://api.stepfun.com/models",
            ]
        );
    }

    #[test]
    fn test_candidates_doubao_strip_api_coding() {
        let c = build_models_url_candidates(
            "https://ark.cn-beijing.volces.com/api/coding",
            false,
            None,
        )
        .unwrap();
        assert_eq!(
            c,
            vec![
                "https://ark.cn-beijing.volces.com/api/coding/v1/models",
                "https://ark.cn-beijing.volces.com/v1/models",
                "https://ark.cn-beijing.volces.com/models",
            ]
        );
    }

    #[test]
    fn test_candidates_rightcode_strip_claude() {
        let c = build_models_url_candidates("https://www.right.codes/claude", false, None).unwrap();
        assert_eq!(
            c,
            vec![
                "https://www.right.codes/claude/v1/models",
                "https://www.right.codes/v1/models",
                "https://www.right.codes/models",
            ]
        );
    }

    #[test]
    fn test_candidates_longer_suffix_wins() {
        // baseURL 以 /api/anthropic 结尾时，应剥离整个 /api/anthropic，
        // 而不是只剥离 /anthropic（那样会得到残缺的 https://.../api 根）。
        let c = build_models_url_candidates("https://api.z.ai/api/anthropic", false, None).unwrap();
        assert_eq!(
            c,
            vec![
                "https://api.z.ai/api/anthropic/v1/models",
                "https://api.z.ai/v1/models",
                "https://api.z.ai/models",
            ]
        );
    }

    #[test]
    fn test_candidates_no_suffix_no_strip() {
        let c = build_models_url_candidates("https://openrouter.ai/api", false, None).unwrap();
        assert_eq!(c, vec!["https://openrouter.ai/api/v1/models"]);
    }

    #[test]
    fn test_candidates_deduplicate() {
        // 虚构 case：baseURL 就是 "scheme://host"，剥不出子路径，应只有一个候选。
        let c = build_models_url_candidates("https://host.example.com", false, None).unwrap();
        assert_eq!(c.len(), 1);
    }

    #[test]
    fn test_parse_response() {
        let json = r#"{"object":"list","data":[{"id":"gpt-4","object":"model","owned_by":"openai"},{"id":"claude-3-sonnet","object":"model","owned_by":"anthropic"}]}"#;
        let resp: ModelsResponse = serde_json::from_str(json).unwrap();
        let data = resp.data.unwrap();
        assert_eq!(data.len(), 2);
        assert_eq!(data[0].id, "gpt-4");
        assert_eq!(data[0].owned_by.as_deref(), Some("openai"));
        assert_eq!(data[1].id, "claude-3-sonnet");
    }

    #[test]
    fn test_parse_response_no_owned_by() {
        let json = r#"{"object":"list","data":[{"id":"my-model","object":"model"}]}"#;
        let resp: ModelsResponse = serde_json::from_str(json).unwrap();
        let data = resp.data.unwrap();
        assert_eq!(data[0].id, "my-model");
        assert!(data[0].owned_by.is_none());
    }

    #[test]
    fn test_parse_response_empty_data() {
        let json = r#"{"object":"list","data":[]}"#;
        let resp: ModelsResponse = serde_json::from_str(json).unwrap();
        assert!(resp.data.unwrap().is_empty());
    }

    #[test]
    fn test_parse_response_with_non_zhipu_models_field() {
        // 回归 #7593：部分供应商的 /models 响应在标准 `data` 之外还附带顶层
        // `models` 字段（条目为 OpenAI 风格的 {id, ...}，无 slug）。整个响应
        // 的反序列化不应因此失败。
        let json = r#"{
            "data": [
                {"id": "model-a", "name": "model-a", "max_tokens": 32000, "context_window": 400000},
                {"id": "model-b", "name": "model-b", "max_tokens": 32000, "context_window": 1000000}
            ],
            "models": [
                {"id": "model-a", "name": "model-a", "max_tokens": 32000, "context_window": 400000},
                {"id": "model-b", "name": "model-b", "max_tokens": 32000, "context_window": 1000000}
            ],
            "success": true
        }"#;
        let resp: ModelsResponse = serde_json::from_str(json).unwrap();
        let data = resp.data.unwrap();
        assert_eq!(data.len(), 2);
        assert_eq!(data[0].id, "model-a");
        assert_eq!(data[1].id, "model-b");
    }

    #[test]
    fn agy_google_model_catalog_keeps_other_protocol_ids() {
        let models = catalog_model_ids(Some(serde_json::json!([
            {"name":"models/gemini-test"}, {"id":"models/custom-id"},
            {"slug":"slug","name":"models/ignored"}, {"name":"Display name"}
        ])));
        assert_eq!(models, ["gemini-test", "models/custom-id", "slug"]);
    }

    #[test]
    fn test_catalog_model_ids_slug_then_id() {
        // `models` 条目解析：slug 优先，回退 id，两者皆缺时跳过。
        let json = r#"{"models": [
            {"slug": "glm-4.7"},
            {"id": "openai-shaped"},
            {"name": "neither-slug-nor-id"},
            {}
        ]}"#;
        let resp: ModelsResponse = serde_json::from_str(json).unwrap();
        let ids = catalog_model_ids(resp.models);
        assert_eq!(
            ids,
            vec!["glm-4.7".to_string(), "openai-shaped".to_string()]
        );
    }

    #[test]
    fn test_advertised_capabilities_are_kept() {
        // OpenRouter 风格：能力字段齐全，应全部收下。
        let json = r#"{"data":[
            {"id":"or-model","context_length":200000,
             "top_provider":{"max_completion_tokens":64000},
             "architecture":{"input_modalities":["text","image"]},
             "supported_parameters":["tools","reasoning"]},
            {"id":"bare-model"}
        ]}"#;
        let resp: ModelsResponse = serde_json::from_str(json).unwrap();
        let models: Vec<FetchedModel> = resp
            .data
            .unwrap()
            .into_iter()
            .map(fetched_from_entry)
            .collect();
        assert_eq!(models[0].context_window, Some(200_000));
        assert_eq!(models[0].max_output_tokens, Some(64_000));
        assert_eq!(
            models[0].input_modalities.as_deref(),
            Some(["image".to_string(), "text".to_string()].as_slice())
        );
        assert_eq!(models[0].reasoning, Some(true));
        // 什么都没说的端点：一个字段都不能凭空造。
        assert_eq!(models[1].context_window, None);
        assert_eq!(models[1].max_output_tokens, None);
        assert_eq!(models[1].input_modalities, None);
        assert_eq!(models[1].reasoning, None);
    }

    #[test]
    fn test_advertised_capabilities_reject_garbage() {
        let json = r#"{"data":[{"id":"m",
            "limit":{"context":0,"output":"32000"},
            "input":["text","file","IMAGE"],
            "capabilities":{"reasoning":false}}]}"#;
        let resp: ModelsResponse = serde_json::from_str(json).unwrap();
        let model = resp
            .data
            .unwrap()
            .into_iter()
            .map(fetched_from_entry)
            .next()
            .unwrap();
        // 0 是「没填」而不是「零窗口」；字符串形式的数字照收。
        assert_eq!(model.context_window, None);
        assert_eq!(model.max_output_tokens, Some(32_000));
        // 未知模态被丢掉、大小写归一、结果排序去重。
        assert_eq!(
            model.input_modalities.as_deref(),
            Some(["image".to_string(), "text".to_string()].as_slice())
        );
        assert_eq!(model.reasoning, Some(false));
    }

    #[test]
    fn test_parse_response_tolerates_any_models_shape() {
        // #7593：data 合法时，任意形状的 models 都不能让整体解析失败
        for models in [
            r#"["a","b"]"#,
            r#"{"count":1}"#,
            "2",
            r#"[{"slug":"glm","id":123}]"#,
            r#"[{"slug":1}]"#,
        ] {
            let json = format!(r#"{{"data":[{{"id":"model-a"}}],"models":{models}}}"#);
            let resp: ModelsResponse = serde_json::from_str(&json).unwrap();
            assert_eq!(resp.data.unwrap()[0].id, "model-a");
        }
        // 无 data 时：非字符串 id 跳过，slug 照常可用
        let resp: ModelsResponse =
            serde_json::from_str(r#"{"models":[{"slug":"glm","id":123}]}"#).unwrap();
        assert_eq!(catalog_model_ids(resp.models), vec!["glm".to_string()]);
    }
}
