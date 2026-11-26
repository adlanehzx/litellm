//! The gateway's router: `model_list` built through router-core's deployment build, and
//! `router_settings` read by the core. The config load resolved every `os.environ/` reference.

use std::collections::HashMap;

use crate::host::NativeBootHost;
use litellm_config::{Config, Environment, Spelled};
use litellm_core_utils::get_llm_provider_logic::LlmProviders;
use litellm_inference::provider::resolve_llm_provider;
use litellm_inference_messages::{MessagesSettings, MessagesShaping, VertexShaping};
use litellm_llms::anthropic::model_capabilities::messages_route_capabilities;
use litellm_llms::vertex_ai::common_utils::supported_regions;
use litellm_model_catalog::Catalog;
use litellm_python_compat::flags::lax_bool;
use litellm_router_core::{
    BuildPath, Deployment, Router, RouterConfig, build::Install, registry::DeploymentFilter,
};
use serde_json::{Map, Value};

use crate::Error;

fn written(value: &impl serde::Serialize) -> Map<String, Value> {
    match serde_json::to_value(value) {
        Ok(Value::Object(fields)) => fields,
        _ => Map::new(),
    }
}

fn entries(config: &Config) -> Vec<Map<String, Value>> {
    config
        .model_list
        .iter()
        .map(|model| {
            let params = written(&model.written_params);
            written(&*model.additional_fields)
                .into_iter()
                .chain([
                    (
                        "model_name".to_owned(),
                        Value::from(model.model_name.as_str()),
                    ),
                    ("litellm_params".to_owned(), Value::Object(params)),
                    (
                        "model_info".to_owned(),
                        Value::Object(written(&*model.model_info)),
                    ),
                ])
                .collect()
        })
        .collect()
}

fn is_on(flag: Option<&Spelled<bool>>) -> bool {
    match flag {
        Some(Spelled::Value(on)) => *on,
        Some(Spelled::Text(text)) => lax_bool(&text.as_str().into()) == Some(true),
        None => false,
    }
}

fn additional_drop_params(deployment: &Deployment) -> Vec<String> {
    deployment
        .params
        .extra
        .get("additional_drop_params")
        .and_then(Value::as_array)
        .map(|names| {
            names
                .iter()
                .filter_map(Value::as_str)
                .map(str::to_owned)
                .collect()
        })
        .unwrap_or_default()
}

/// Python's `ANTHROPIC_MESSAGES_DETACHED_STREAM_DRAIN_TIMEOUT_SECONDS` and
/// `ANTHROPIC_MESSAGES_MAX_DETACHED_STREAM_DRAINS` defaults.
const DETACHED_DRAIN_TIMEOUT_SECONDS: f64 = 600.0;
const MAX_DETACHED_DRAINS: u64 = 100;

/// How long, and in how many calls at once, a stream whose client left is still read to its end,
/// as Python's async Messages relay reads a Bedrock invoke stream. The route drains only a
/// provider whose stream it decodes itself.
#[derive(Clone, Copy)]
struct DetachedDrain {
    timeout_seconds: f64,
    limit: usize,
}

impl DetachedDrain {
    fn read(environment: &Environment) -> Self {
        Self {
            timeout_seconds: environment
                .anthropic_messages_detached_stream_drain_timeout_seconds
                .unwrap_or(DETACHED_DRAIN_TIMEOUT_SECONDS),
            limit: usize::try_from(
                environment
                    .anthropic_messages_max_detached_stream_drains
                    .unwrap_or(MAX_DETACHED_DRAINS),
            )
            .unwrap_or(usize::MAX),
        }
    }
}

/// The model map's `supported_regions` of a Claude on Vertex AI deployment, which the route
/// moves a configured location outside of to the first one, as Python's
/// `VertexBase.get_vertex_region` does.
fn vertex(deployment: &Deployment, catalog: &Catalog) -> VertexShaping {
    let params = &deployment.params;
    let supported = resolve_llm_provider(
        &params.model,
        params.custom_llm_provider.as_deref(),
        "messages",
    )
    .ok()
    .filter(|resolved| resolved.provider == LlmProviders::VertexAi)
    .map(|resolved| supported_regions(catalog, resolved.model));
    VertexShaping {
        supported_regions: supported.unwrap_or_default(),
    }
}

/// How the messages route shapes a call to `deployment`: its `drop_params`, the capabilities
/// the catalog and its `model_info` declare, and what the route reads of a cloud provider's call.
fn shaping(
    deployment: &Deployment,
    catalog: &Catalog,
    drops_params_globally: bool,
    drain: DetachedDrain,
) -> MessagesShaping {
    let params = &deployment.params;
    let drop_params = drops_params_globally || is_on(params.drop_params.as_ref());
    let base_model = params.extra.get("base_model").and_then(Value::as_str);
    MessagesShaping {
        settings: MessagesSettings {
            drop_params,
            drops_unsupported_params: drop_params,
            additional_drop_params: additional_drop_params(deployment),
            base_model: base_model.map(str::to_owned),
            pricing: crate::pricing::identity(deployment),
            detached_drain_timeout: Some(drain.timeout_seconds),
            detached_drain_limit: Some(drain.limit),
            ..MessagesSettings::default()
        },
        vertex: vertex(deployment, catalog),
        capabilities: messages_route_capabilities(
            catalog,
            &params.model,
            params.custom_llm_provider.as_deref(),
            base_model,
            &|capability| {
                deployment
                    .model_info
                    .raw
                    .get(capability)
                    .and_then(Value::as_bool)
            },
        ),
    }
}

/// The router `config` describes, and how the messages route shapes a call to each of its
/// deployments, by id.
pub(crate) async fn router(
    config: &Config,
    catalog: &Catalog,
    environment: &Environment,
) -> Result<(Router, HashMap<String, MessagesShaping>), Error> {
    let (settings, dynamic) = RouterConfig::from_settings(&config.router_settings)?;
    let router = Router::new(settings, dynamic)?;
    router
        .build(
            &NativeBootHost,
            BuildPath::Create,
            entries(config),
            true,
            Install::Replace,
        )
        .await?;
    let drops_params_globally = config.litellm_settings.drop_params == Some(true);
    let drain = DetachedDrain::read(environment);
    let shaping = router
        .registry()
        .deployments(&DeploymentFilter::default())
        .iter()
        .map(|deployment| {
            (
                deployment.id.clone(),
                shaping(deployment, catalog, drops_params_globally, drain),
            )
        })
        .collect();
    Ok((router, shaping))
}

/// The router `config` describes with no host behind its build, for a test that asks it only what
/// its settings and model groups say.
#[cfg(test)]
pub(crate) fn unhosted(config: &Config) -> Router {
    let (settings, dynamic) = RouterConfig::from_settings(&config.router_settings).unwrap();
    let model_list = entries(config)
        .into_iter()
        .map(|entry| serde_json::from_value(Value::Object(entry)).unwrap())
        .collect();
    let settings = RouterConfig {
        model_list,
        ..settings
    };
    Router::new(settings, dynamic).unwrap()
}

#[cfg(test)]
mod tests {
    use litellm_config::Config;
    use litellm_model_catalog::Catalog;
    use rstest::rstest;

    fn catalog() -> Catalog {
        Catalog::parse(br#"{"provider-model": {"litellm_provider": "provider"}}"#)
            .unwrap()
            .value
    }

    async fn built(
        yaml: &str,
    ) -> (
        litellm_router_core::Router,
        super::HashMap<String, super::MessagesShaping>,
    ) {
        let config = Config::from_yaml(yaml).unwrap();
        super::router(&config, &catalog(), &litellm_config::Environment::default())
            .await
            .unwrap()
    }

    #[rstest]
    #[case::unset("", "", false)]
    #[case::deployment_flag("", "\n      drop_params: true", true)]
    #[case::deployment_word("", "\n      drop_params: \" True \"", true)]
    #[case::deployment_off("", "\n      drop_params: false", false)]
    #[case::deployment_unknown_word("", "\n      drop_params: maybe", false)]
    #[case::deployment_yes("", "\n      drop_params: yes", true)]
    #[case::deployment_no("", "\n      drop_params: \"no\"", false)]
    #[case::global_flag("litellm_settings:\n  drop_params: true\n", "", true)]
    #[case::global_word("litellm_settings:\n  drop_params: \"true\"\n", "", true)]
    #[case::global_off("litellm_settings:\n  drop_params: false\n", "", false)]
    #[case::global_over_deployment_off(
        "litellm_settings:\n  drop_params: true\n",
        "\n      drop_params: false",
        true
    )]
    #[tokio::test]
    async fn configured_drop_params_reach_the_deployment(
        #[case] settings: &str,
        #[case] deployment: &str,
        #[case] expected: bool,
    ) {
        let (_, shaping) = built(&format!(
            "{settings}model_list:\n  - model_name: public-model\n    litellm_params:\n      model: provider/model{deployment}"
        ))
        .await;
        let settings = &shaping.values().next().unwrap().settings;

        assert_eq!(
            (settings.drop_params, settings.drops_unsupported_params),
            (expected, expected)
        );
    }

    #[rstest]
    #[case::declared("\n    model_info:\n      supports_reasoning: true", true)]
    #[case::declared_off("\n    model_info:\n      supports_reasoning: false", false)]
    #[case::undeclared("", false)]
    #[tokio::test]
    async fn model_info_flags_reach_the_deployment(
        #[case] model_info: &str,
        #[case] expected: bool,
    ) {
        let (_, shaping) = built(&format!(
            "model_list:\n  - model_name: public-model\n    litellm_params:\n      model: anthropic/claude-unlisted-9{model_info}"
        ))
        .await;

        assert_eq!(
            shaping
                .values()
                .next()
                .unwrap()
                .capabilities
                .supports_reasoning,
            expected
        );
    }

    #[rstest]
    #[case::a_catalog_priced_deployment("", "", Some(None))]
    #[case::a_base_model_in_its_params(
        "\n      base_model: claude-opus-5-5",
        "",
        Some(Some("claude-opus-5-5"))
    )]
    #[case::a_base_model_in_its_model_info(
        "",
        "\n      base_model: claude-opus-5-5",
        Some(Some("claude-opus-5-5"))
    )]
    #[case::custom_pricing_in_its_params("\n      input_cost_per_token: 0.000001", "", None)]
    #[case::custom_pricing_in_its_model_info("", "\n      output_cost_per_token: 0.000002", None)]
    #[case::off_peak_pricing_in_its_model_info("", "\n      off_peak_pricing: {}", None)]
    #[case::a_custom_pricing_key_left_unset("\n      input_cost_per_token: null", "", Some(None))]
    #[case::a_pricing_provider("", "\n      pricing_provider: anthropic", None)]
    #[case::an_invoked_bedrock_model_id("\n      model_id: arn:aws:bedrock:model", "", None)]
    #[tokio::test]
    async fn a_deployment_is_priced_unless_python_prices_it_another_way(
        #[case] params: &str,
        #[case] model_info: &str,
        #[case] expected: Option<Option<&str>>,
    ) {
        let (_, shaping) = built(&format!(
            "model_list:\n  - model_name: public-model\n    litellm_params:\n      model: anthropic/claude-opus-5-5{params}\n    model_info:\n      id: deployment-1{model_info}"
        ))
        .await;
        let pricing = shaping.values().next().unwrap().settings.pricing.clone();

        assert_eq!(
            pricing.as_ref().map(|identity| (
                identity.deployment_id.as_deref(),
                identity.base_model.as_deref()
            )),
            expected.map(|base_model| (Some("deployment-1"), base_model))
        );
    }

    #[rstest]
    #[case::vertex_ai("vertex_ai/claude-opus-5-5")]
    #[case::bedrock_mantle("bedrock_mantle/claude-opus-5-5")]
    #[case::mantle_through_bedrock("bedrock/mantle/claude-opus-5-5")]
    #[tokio::test]
    async fn a_deployment_whose_price_reads_a_region_is_left_unpriced(#[case] model: &str) {
        let (_, shaping) = built(&format!(
            "model_list:\n  - model_name: public-model\n    litellm_params:\n      model: {model}"
        ))
        .await;

        assert_eq!(shaping.values().next().unwrap().settings.pricing, None);
    }

    #[rstest]
    #[case::vertex_claude("vertex_ai/claude-opus-5-5", &["us-east5", "europe-west1"])]
    #[case::provider_named_apart("claude-opus-5-5\n      custom_llm_provider: vertex_ai", &["us-east5", "europe-west1"])]
    #[case::another_provider("anthropic/claude-opus-5-5", &[])]
    #[tokio::test]
    async fn a_vertex_deployment_carries_the_regions_its_model_is_served_in(
        #[case] model: &str,
        #[case] regions: &[&str],
    ) {
        let catalog = Catalog::parse(
            br#"{"vertex_ai/claude-opus-5-5": {"litellm_provider": "vertex_ai-anthropic_models", "supported_regions": ["us-east5", "europe-west1"]}}"#,
        )
        .unwrap()
        .value;
        let config = Config::from_yaml(&format!(
            "model_list:\n  - model_name: public-model\n    litellm_params:\n      model: {model}"
        ))
        .unwrap();

        let (_, shaping) =
            super::router(&config, &catalog, &litellm_config::Environment::default())
                .await
                .unwrap();

        assert_eq!(
            shaping.values().next().unwrap().vertex.supported_regions,
            regions
        );
    }

    #[rstest]
    #[case::python_defaults(None, None, 600.0, 100)]
    #[case::set_in_the_environment(Some(30.0), Some(4), 30.0, 4)]
    #[tokio::test]
    async fn a_stream_whose_client_left_is_drained_as_python_drains_it(
        #[case] timeout: Option<f64>,
        #[case] limit: Option<u64>,
        #[case] expected_timeout: f64,
        #[case] expected_limit: usize,
    ) {
        let config = Config::from_yaml(
            "model_list:\n  - model_name: public-model\n    litellm_params:\n      model: bedrock/us.anthropic.claude-opus-5-5-v1:0",
        )
        .unwrap();
        let environment = litellm_config::Environment {
            anthropic_messages_detached_stream_drain_timeout_seconds: timeout,
            anthropic_messages_max_detached_stream_drains: limit,
            ..litellm_config::Environment::default()
        };

        let (_, shaping) = super::router(&config, &catalog(), &environment)
            .await
            .unwrap();

        assert_eq!(
            shaping.values().next().unwrap().settings.detached_drain(),
            Some(litellm_host::protocol::DetachedDrain {
                timeout: std::time::Duration::from_secs_f64(expected_timeout),
                limit: expected_limit,
            })
        );
    }
}
