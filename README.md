# srvcs-rollingaverage

The rolling-average service of the srvcs.cloud distributed standard library.

Its single concern: **the rolling (sliding-window) average of a list of
numbers**, returned as a JSON array of `f64`s. It does no arithmetic of its own.
It is a pure orchestrator that delegates the entire computation to one
sibling statistics service:

```text
result = movingaverage(values, window).result   # one call to srvcs-movingaverage
```

So `rollingaverage({values: [1, 2, 3, 4], window: 2}) == [1.5, 2.5, 3.5]` — the
windowed averages `(1+2)/2`, `(2+3)/2`, `(3+4)/2`. The `result` from
`srvcs-movingaverage` is forwarded verbatim.

## API

| Method | Path | Purpose |
| --- | --- | --- |
| `GET` | `/` | Service identity, concern, and dependency list |
| `POST` | `/` | Compute the rolling average of `values` over `window` |
| `GET` | `/healthz` `/readyz` `/metrics` `/openapi.json` | srvcs service standard surface |

```sh
curl -s -X POST localhost:8080/ -H 'content-type: application/json' \
  -d '{"values": [1, 2, 3, 4], "window": 2}'
# {"values":[1,2,3,4],"window":2,"result":[1.5,2.5,3.5]}
```

Responses:

- `200 {"values": [...], "window": w, "result": [...]}` — evaluated; `result` is a JSON array of `f64`s.
- `422` — window out of range, or an element is not a valid number (forwarded from `srvcs-movingaverage`).
- `500` — a dependency returned a malformed result.
- `503` — a dependency is unavailable.

## Dependencies

- [`srvcs-movingaverage`](https://github.com/srvcs/movingaverage)

This service is an orchestrator: it never calls `srvcs-isnumber` directly.
Input validation propagates from its dependency — an out-of-range `window` or a
non-numeric element is caught by `srvcs-movingaverage`, whose `422` is forwarded
verbatim.

## Configuration

| Variable | Default | Purpose |
| --- | --- | --- |
| `SRVCS_BIND_ADDR` | `0.0.0.0:8080` | Bind address |
| `SRVCS_MOVINGAVERAGE_URL` | `http://127.0.0.1:8090` | Base URL of `srvcs-movingaverage` |
| `SRVCS_ENV` | `development` | Environment label for logs |
| `RUST_LOG` | `info,tower_http=info` | Tracing filter |

## Local checks

```sh
cargo fmt --check
cargo clippy --all-targets -- -D warnings
cargo test
```

Orchestration tests stand up a mock `srvcs-movingaverage` in-process that
**actually computes** the sliding-window averages, so the composition is
genuinely exercised against asserted cases — e.g.
`rollingaverage([1,2,3,4], 2) == [1.5, 2.5, 3.5]` — with a `1e-9` tolerance. See
[`srvcs/platform`](https://github.com/srvcs/platform) for the shared standard.

> Note: the `cargoHash` in `flake.nix` is inherited from the template and must be
> refreshed with a `nix build` before the Nix gates pass.
