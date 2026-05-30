use axum::{
    extract::State,
    http::StatusCode,
    response::{IntoResponse, Response},
    Json,
};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use utoipa::{OpenApi, ToSchema};

use crate::client::{self, DepError};

pub const SERVICE: &str = "srvcs-rollingaverage";
pub const CONCERN: &str = "statistics: rolling (sliding-window) average";
pub const DEPENDS_ON: &[&str] = &["srvcs-movingaverage"];

/// Dependency endpoints, injected as router state so tests can point them at
/// mock services.
#[derive(Clone)]
pub struct Deps {
    pub movingaverage_url: String,
}

#[derive(Serialize, ToSchema)]
pub struct Info {
    pub service: &'static str,
    pub concern: &'static str,
    pub depends_on: Vec<&'static str>,
}

/// `GET /` — service identity (srvcs service standard).
#[utoipa::path(get, path = "/", responses((status = 200, body = Info)))]
pub async fn index() -> Json<Info> {
    Json(Info {
        service: SERVICE,
        concern: CONCERN,
        depends_on: DEPENDS_ON.to_vec(),
    })
}

#[derive(Deserialize, ToSchema)]
pub struct EvalRequest {
    /// The list of numbers to slide a window over.
    #[schema(value_type = Object)]
    pub values: Vec<Value>,
    /// The window size. Must be `>= 1` and `<= values.len()`.
    pub window: i64,
}

#[derive(Serialize, ToSchema)]
pub struct RollingAverageResponse {
    #[schema(value_type = Object)]
    pub values: Vec<Value>,
    pub window: i64,
    /// The list of windowed averages, as `f64`s.
    #[schema(value_type = Object)]
    pub result: Vec<f64>,
}

fn ok(values: Vec<Value>, window: i64, result: Vec<f64>) -> Response {
    (
        StatusCode::OK,
        Json(json!({ "values": values, "window": window, "result": result })),
    )
        .into_response()
}

fn degraded(dependency: &str) -> Response {
    (
        StatusCode::SERVICE_UNAVAILABLE,
        Json(json!({ "error": "dependency unavailable", "dependency": dependency })),
    )
        .into_response()
}

/// Forward a dependency's response verbatim (used to propagate `422` for
/// invalid input from a dependency).
fn forward(status: u16, body: Value) -> Response {
    let code = StatusCode::from_u16(status).unwrap_or(StatusCode::BAD_GATEWAY);
    (code, Json(body)).into_response()
}

/// A reachable dependency answered `200` but its body lacked a usable `result`.
/// That is a contract violation we cannot recover from, so surface a `500`
/// rather than guessing.
fn malformed(dependency: &str) -> Response {
    (
        StatusCode::INTERNAL_SERVER_ERROR,
        Json(
            json!({ "error": "dependency returned a malformed result", "dependency": dependency }),
        ),
    )
        .into_response()
}

/// Ask `srvcs-movingaverage` to compute the windowed averages, returning its
/// `result` JSON array verbatim:
///
/// - unreachable / non-`200`/`422` -> `503` degraded
/// - `422` -> forwarded `422` (the dependency rejected the input)
/// - `200` without an array `result` -> `500` malformed
async fn ask_movingaverage(url: &str, body: &Value) -> Result<Vec<Value>, Response> {
    match client::call(url, body).await {
        Err(DepError::Unreachable) => Err(degraded("srvcs-movingaverage")),
        Ok((200, body)) => match body.get("result").and_then(Value::as_array) {
            Some(arr) => Ok(arr.clone()),
            None => Err(malformed("srvcs-movingaverage")),
        },
        Ok((422, body)) => Err(forward(422, body)),
        Ok(_) => Err(degraded("srvcs-movingaverage")),
    }
}

/// `POST /` — the rolling (sliding-window) average of a list of numbers.
///
/// This service is a pure orchestrator: it delegates the entire computation to
/// `srvcs-movingaverage`, whose `result` (a JSON array of `f64`s) it returns
/// verbatim. So `rollingaverage({values: [1,2,3,4], window: 2})` is
/// `[1.5, 2.5, 3.5]`.
///
/// It does no validation of its own — `window` range checks and element
/// validation are propagated from `srvcs-movingaverage`'s `422`.
#[utoipa::path(
    post,
    path = "/",
    request_body = EvalRequest,
    responses(
        (status = 200, body = RollingAverageResponse),
        (status = 422, description = "window out of range, or a dependency rejected an input (forwarded)"),
        (status = 500, description = "a dependency returned a malformed result"),
        (status = 503, description = "a dependency is unavailable")
    )
)]
pub async fn evaluate(State(deps): State<Deps>, Json(req): Json<EvalRequest>) -> Response {
    let body = json!({ "values": req.values, "window": req.window });
    let arr = match ask_movingaverage(&deps.movingaverage_url, &body).await {
        Ok(arr) => arr,
        Err(resp) => return resp,
    };

    let mut result: Vec<f64> = Vec::with_capacity(arr.len());
    for v in &arr {
        match v.as_f64() {
            Some(x) => result.push(x),
            None => return malformed("srvcs-movingaverage"),
        }
    }

    ok(req.values, req.window, result)
}

#[derive(OpenApi)]
#[openapi(
    paths(index, evaluate),
    components(schemas(Info, EvalRequest, RollingAverageResponse))
)]
pub struct ApiDoc;

/// Serve OpenAPI document
pub async fn openapi_json() -> Json<utoipa::openapi::OpenApi> {
    Json(ApiDoc::openapi())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn openapi_documents_routes() {
        let doc = ApiDoc::openapi();
        let root = doc.paths.paths.get("/").expect("path / present");
        assert!(root.get.is_some());
        assert!(root.post.is_some());
    }

    #[tokio::test]
    async fn index_reports_all_dependencies() {
        let Json(info) = index().await;
        assert_eq!(info.service, "srvcs-rollingaverage");
        assert_eq!(info.concern, "statistics: rolling (sliding-window) average");
        assert_eq!(info.depends_on, vec!["srvcs-movingaverage"]);
    }
}
