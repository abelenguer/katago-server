//! OpenAPI document generated from the handler and type annotations.

use utoipa::OpenApi;

use super::{handlers, problem, types, ws};

/// The OpenAPI 3.1 description of this server.
#[derive(OpenApi)]
#[openapi(
    info(
        title = "KataGo Server",
        description = "REST API in front of the KataGo Go engine: position and whole-game analysis, \
                       ownership, policy and Human SL profiles. All errors are RFC 9457 problem details.",
        license(name = "MIT", url = "https://github.com/goban-app/katago-server/blob/main/LICENSE"),
        contact(name = "goban-app", url = "https://github.com/goban-app/katago-server"),
    ),
    paths(
        handlers::index,
        handlers::analysis,
        handlers::analysis_game,
        ws::upgrade,
        handlers::health,
        handlers::health_live,
        handlers::health_ready,
        handlers::version,
        handlers::cache_clear,
        handlers::metrics,
    ),
    components(schemas(
        types::AnalysisRequest,
        types::MoveInput,
        types::Rules,
        types::MoveFilter,
        types::AnalysisResponse,
        types::GameAnalysisResponse,
        types::MoveInfo,
        types::RootInfo,
        types::HealthResponse,
        types::EngineHealth,
        types::VersionResponse,
        types::ServerVersion,
        types::KatagoVersionInfo,
        types::ModelInfo,
        types::CacheClearResponse,
        types::IndexResponse,
        ws::ClientMessage,
        ws::ServerMessage,
        ws::CompletionStatus,
        problem::ProblemDetails,
    )),
    tags(
        (name = "analysis", description = "Position and game analysis"),
        (name = "operations", description = "Health, version, cache and metrics"),
    )
)]
#[derive(Debug)]
pub struct ApiDoc;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn document_lists_every_route() {
        let doc = ApiDoc::openapi();
        let paths: Vec<&String> = doc.paths.paths.keys().collect();
        for expected in [
            "/",
            "/api/v1/analysis",
            "/api/v1/analysis/game",
            "/api/v1/analysis/ws",
            "/api/v1/health",
            "/api/v1/health/live",
            "/api/v1/health/ready",
            "/api/v1/version",
            "/api/v1/cache/clear",
            "/metrics",
        ] {
            assert!(
                paths.iter().any(|p| *p == expected),
                "missing {expected} in {paths:?}"
            );
        }
        assert_eq!(doc.info.version, crate::VERSION);
        let json = doc.to_json().unwrap();
        assert!(json.contains("ProblemDetails"));
    }

    #[test]
    fn ws_completion_statuses_match_the_wire_contract() {
        let expected = serde_json::json!(["completed", "cancelled", "timeout", "error"]);
        let statuses = [
            ws::CompletionStatus::Ok,
            ws::CompletionStatus::Cancelled,
            ws::CompletionStatus::Timeout,
            ws::CompletionStatus::Error,
        ];
        assert_eq!(serde_json::to_value(statuses).unwrap(), expected);
        let doc = serde_json::to_value(ApiDoc::openapi()).unwrap();
        assert_eq!(
            doc["components"]["schemas"]["CompletionStatus"]["enum"],
            expected
        );
    }

    #[test]
    fn ws_envelopes_use_snake_case_and_shared_analysis_schemas_keep_camel_case() {
        let doc = serde_json::to_value(ApiDoc::openapi()).unwrap();
        let schemas = &doc["components"]["schemas"];
        for (schema, wire_name, camel_name) in [
            ("ClientMessage", "request_id", "requestId"),
            ("ServerMessage", "request_id", "requestId"),
            ("ServerMessage", "connection_id", "connectionId"),
            ("ServerMessage", "turn_number", "turnNumber"),
        ] {
            let variants = schemas[schema]["oneOf"].as_array().unwrap();
            assert!(
                variants
                    .iter()
                    .any(|variant| variant["properties"].get(wire_name).is_some()),
                "{schema} is missing {wire_name}: {variants:?}"
            );
            assert!(
                variants
                    .iter()
                    .all(|variant| variant["properties"].get(camel_name).is_none()),
                "{schema} still contains {camel_name}: {variants:?}"
            );
        }
        for (schema, field, snake_name) in [
            ("AnalysisRequest", "requestId", "request_id"),
            ("AnalysisRequest", "analyzeTurns", "analyze_turns"),
            ("AnalysisResponse", "turnNumber", "turn_number"),
        ] {
            let properties = &schemas[schema]["properties"];
            assert!(properties.get(field).is_some());
            assert!(properties.get(snake_name).is_none());
        }
    }
}
