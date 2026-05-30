use axum::body::Body;
use axum::extract::Json as JsonExtract;
use axum::http::{Request, StatusCode};
use axum::routing::post;
use axum::{Json, Router as AxumRouter};
use http_body_util::BodyExt;
use serde_json::{json, Value};
use srvcs_rollingaverage::{api::Deps, health, router, telemetry};
use tower::ServiceExt;

const DEAD_URL: &str = "http://127.0.0.1:1";

async fn serve(app: AxumRouter) -> String {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    format!("http://{addr}")
}

/// Mock `srvcs-movingaverage` that ACTUALLY COMPUTES the sliding-window average
/// over `values` with the given `window`, returning
/// `{"values", "window", "result": [...]}`. Mirrors the real service: for each
/// start `i` in `0..=(len - window)`, the windowed average is the mean of
/// `values[i..i+window]`.
async fn spawn_computing_movingaverage() -> String {
    let app = AxumRouter::new().route(
        "/",
        post(|JsonExtract(req): JsonExtract<Value>| async move {
            let values: Vec<f64> = req["values"]
                .as_array()
                .map(|a| a.iter().filter_map(Value::as_f64).collect())
                .unwrap_or_default();
            let window = req["window"].as_i64().unwrap_or(0);
            let len = values.len() as i64;
            let mut result: Vec<f64> = Vec::new();
            if window >= 1 && window <= len {
                let w = window as usize;
                let last = (len - window) as usize;
                let mut i: usize = 0;
                while i <= last {
                    let sum: f64 = values[i..i + w].iter().sum();
                    result.push(sum / window as f64);
                    i += 1;
                }
            }
            Json(json!({ "values": req["values"], "window": window, "result": result }))
        }),
    );
    serve(app).await
}

/// Mock that always answers with a fixed status + body (used to simulate a
/// `422` rejection forwarded from a dependency).
async fn spawn_fixed(status: StatusCode, body: Value) -> String {
    let app = AxumRouter::new().route(
        "/",
        post(move || {
            let body = body.clone();
            async move { (status, Json(body)) }
        }),
    );
    serve(app).await
}

fn app(movingaverage_url: &str) -> axum::Router {
    router(
        telemetry::metrics_handle_for_tests(),
        Deps {
            movingaverage_url: movingaverage_url.to_string(),
        },
    )
}

async fn eval(movingaverage_url: &str, values: Value, window: Value) -> (StatusCode, Value) {
    let res = app(movingaverage_url)
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/")
                .header("content-type", "application/json")
                .body(Body::from(
                    json!({ "values": values, "window": window }).to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    let status = res.status();
    let bytes = res.into_body().collect().await.unwrap().to_bytes();
    (
        status,
        serde_json::from_slice(&bytes).unwrap_or(Value::Null),
    )
}

async fn status_of(uri: &str) -> StatusCode {
    app(DEAD_URL)
        .oneshot(Request::builder().uri(uri).body(Body::empty()).unwrap())
        .await
        .unwrap()
        .status()
}

/// Element-wise approximate comparison of a JSON array against expected `f64`s.
fn approx_list(got: &Value, expected: &[f64]) -> bool {
    let arr = match got.as_array() {
        Some(a) => a,
        None => return false,
    };
    if arr.len() != expected.len() {
        return false;
    }
    arr.iter()
        .zip(expected.iter())
        .all(|(g, e)| g.as_f64().map(|x| (x - e).abs() < 1e-9) == Some(true))
}

// --- Standard endpoints ---

#[tokio::test]
async fn healthz_ok() {
    assert_eq!(status_of("/healthz").await, StatusCode::OK);
}

#[tokio::test]
async fn readyz_reflects_state() {
    health::set_ready(true);
    assert_eq!(status_of("/readyz").await, StatusCode::OK);
}

#[tokio::test]
async fn openapi_ok() {
    assert_eq!(status_of("/openapi.json").await, StatusCode::OK);
}

// --- Correctness cases, exercised against a REAL computing dependency ---

#[tokio::test]
async fn rolling_average_window_two() {
    let ma = spawn_computing_movingaverage().await;
    let (status, body) = eval(&ma, json!([1, 2, 3, 4]), json!(2)).await;
    assert_eq!(status, StatusCode::OK);
    assert!(
        approx_list(&body["result"], &[1.5, 2.5, 3.5]),
        "got {:?}",
        body["result"]
    );
    assert_eq!(body["values"], json!([1, 2, 3, 4]));
    assert_eq!(body["window"], json!(2));
}

#[tokio::test]
async fn rolling_average_window_one_is_identity() {
    let ma = spawn_computing_movingaverage().await;
    let (status, body) = eval(&ma, json!([2, 4, 6]), json!(1)).await;
    assert_eq!(status, StatusCode::OK);
    assert!(
        approx_list(&body["result"], &[2.0, 4.0, 6.0]),
        "got {:?}",
        body["result"]
    );
}

#[tokio::test]
async fn rolling_average_full_window_is_single_mean() {
    let ma = spawn_computing_movingaverage().await;
    // window == len -> one average: (1+2+3+4+5)/5 = 3.0
    let (status, body) = eval(&ma, json!([1, 2, 3, 4, 5]), json!(5)).await;
    assert_eq!(status, StatusCode::OK);
    assert!(
        approx_list(&body["result"], &[3.0]),
        "got {:?}",
        body["result"]
    );
}

#[tokio::test]
async fn rolling_average_window_three_fractional() {
    let ma = spawn_computing_movingaverage().await;
    // windows of 3 over [1,2,4,7]: (1+2+4)/3 = 2.333..., (2+4+7)/3 = 4.333...
    let (status, body) = eval(&ma, json!([1, 2, 4, 7]), json!(3)).await;
    assert_eq!(status, StatusCode::OK);
    assert!(
        approx_list(&body["result"], &[7.0 / 3.0, 13.0 / 3.0]),
        "got {:?}",
        body["result"]
    );
}

#[tokio::test]
async fn rolling_average_with_negatives() {
    let ma = spawn_computing_movingaverage().await;
    // windows of 2 over [-2, 4, -6]: (-2+4)/2 = 1.0, (4-6)/2 = -1.0
    let (status, body) = eval(&ma, json!([-2, 4, -6]), json!(2)).await;
    assert_eq!(status, StatusCode::OK);
    assert!(
        approx_list(&body["result"], &[1.0, -1.0]),
        "got {:?}",
        body["result"]
    );
}

// --- Error / edge cases ---

#[tokio::test]
async fn forwards_422_from_movingaverage() {
    let ma = spawn_fixed(
        StatusCode::UNPROCESSABLE_ENTITY,
        json!({ "error": "window must be <= values.len()" }),
    )
    .await;
    let (status, body) = eval(&ma, json!([1, 2, 3]), json!(9)).await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(body["error"], "window must be <= values.len()");
}

#[tokio::test]
async fn degrades_when_movingaverage_unreachable() {
    let (status, body) = eval(DEAD_URL, json!([1, 2, 3, 4]), json!(2)).await;
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(body["dependency"], "srvcs-movingaverage");
}
