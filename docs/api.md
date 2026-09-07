# API reference

Base path: `/api/v1`. HTTP request and response bodies are JSON in camelCase. The
interactive HTTP reference at `/docs` and the document at `/api/v1/openapi.json`
are generated from the code and are authoritative for HTTP schemas. WebSocket
envelopes use snake_case; their nested `payload` and `data` use the HTTP camelCase
schemas. WebSocket messages are specified below.

## POST /api/v1/analysis

Analyses the position after all `moves` have been played.

### Request body

| Field | Type | Default | Notes |
|---|---|---|---|
| `moves` | array | `[]` | Either all bare coordinates (`"D4"`, `"pass"`) or all `[colour, coordinate]` pairs (`["W", "D4"]`). Mixing the two forms is rejected. Bare moves alternate colours starting with `initialPlayer`. |
| `rules` | string or object | Japanese for integer or 6.5 komi, Chinese otherwise | A KataGo rules name (`chinese`, `japanese`, `korean`, `aga`, `tromp-taylor`, ...) or a KataGo rules object. Names are lower-cased. Unknown names are rejected by KataGo and returned as 400 with `field: rules`. |
| `komi` | number | `7.5` | Multiple of 0.5, magnitude at most 150. |
| `boardXSize`, `boardYSize` | integer | `19` | 2 to 25. |
| `initialStones` | array of `[colour, coordinate]` | none | Handicap or setup stones. No passes, no duplicates. |
| `initialPlayer` | `"B"` or `"W"` | `"W"` if `initialStones` is non-empty, else `"B"` | Forwarded to KataGo and used to colour bare moves. |
| `analyzeTurns` | array of integers | final position | Each value from 0 to `moves.length`. Duplicates are removed. For this endpoint only the last result is returned; use `/analysis/game` for several turns. |
| `maxVisits` | integer >= 1 | server `default_max_visits` (10) | Rejected if above the server's `max_visits_limit`. |
| `rootPolicyTemperature` | number > 0 | KataGo default | |
| `rootFpuReductionMax` | number | KataGo default | |
| `analysisPVLen` | integer >= 1 | KataGo default | `analysisPvLen` is accepted as an alias. |
| `includeOwnership` | bool | false | Adds `ownership`. |
| `includeOwnershipStdev` | bool | false | Adds `ownershipStdev`. |
| `includeMovesOwnership` | bool | false | Adds `ownership` to each `moveInfos` entry. |
| `includePolicy` | bool | false | Adds `policy`. |
| `includePVVisits` | bool | false | Adds `pvVisits` and `pvEdgeVisits`. `includePvVisits` is accepted as an alias. |
| `avoidMoves`, `allowMoves` | array of `{player, moves, untilDepth}` | none | `player` is `B`/`W`, `untilDepth` >= 1, coordinates validated for the board. |
| `overrideSettings` | object | none | Passed to KataGo unchanged. `maxVisits` inside it is checked against `max_visits_limit`. |
| `priority` | integer | none | KataGo scheduling priority. |
| `requestId` | string, at most 128 chars | none | Echoed back as `id`. Never sent to KataGo; the server uses its own UUID internally. |

Colours accept `B`, `W`, `b`, `w`, `black`, `white`. Coordinates use GTP letters
(`A` to `Z`, skipping `I`) and rows counted from the bottom, case-insensitive.

`reportDuringSearchEvery` is not supported; partial results are never returned.
Unknown fields are ignored.

### Response

```json
{
  "id": "requestId or generated UUID",
  "turnNumber": 3,
  "isDuringSearch": false,
  "moveInfos": [
    {
      "moveCoord": "D16", "visits": 21, "winrate": 0.52, "scoreMean": 1.9, "scoreStdev": 12.1,
      "scoreLead": 1.9, "scoreSelfplay": 2.3, "utility": 0.03, "utilityLcb": -0.1, "lcb": 0.49,
      "prior": 0.18, "order": 0, "edgeVisits": 21, "edgeWeight": 21.4, "weight": 21.4,
      "playSelectionValue": 21.4, "pv": ["D16", "C14"], "pvVisits": [21, 9], "pvEdgeVisits": [21, 9],
      "ownership": [0.02, -0.11], "humanPrior": 0.2
    }
  ],
  "rootInfo": {
    "winrate": 0.51, "scoreLead": 1.5, "scoreSelfplay": 1.7, "scoreStdev": 12.0, "utility": 0.02,
    "visits": 50, "currentPlayer": "W", "weight": 50.2, "rawWinrate": 0.5, "rawLead": 1.2,
    "rawScoreSelfplay": 1.3, "rawScoreSelfplayStdev": 12.9, "rawStScoreError": 0.5,
    "rawStWrError": 0.03, "rawNoResultProb": 0.0008, "rawVarTimeLeft": 50.1,
    "symHash": "…", "thisHash": "…",
    "humanWinrate": 0.5, "humanScoreMean": 1.0, "humanScoreStdev": 11.0
  },
  "ownership": [0.02, -0.11],
  "ownershipStdev": [0.03],
  "policy": [0.00001],
  "humanPolicy": [0.0001]
}
```

- `winrate` and `scoreLead` are from the perspective of the side to move (`reportAnalysisWinratesAs = SIDETOMOVE` in the shipped configs).
- `ownership` and `ownershipStdev` have `boardXSize * boardYSize` entries, row by row from the top. Positive means the side to move.
- `policy` and `humanPolicy` have `boardXSize * boardYSize + 1` entries; the last one is pass.
- Optional fields are omitted when KataGo did not report them. `noResults: true` never appears in a 200 response; a terminated search is a 503 `analysis-terminated`.
- `human*` fields need a Human SL network (`human-*` or `combo-*` images, or `katago.human_model_path`) and `"overrideSettings": {"humanSLProfile": "rank_5k"}` or similar.

## POST /api/v1/analysis/game

Same request body. `analyzeTurns` defaults to every turn from 0 to `moves.length`.
KataGo analyses all turns in one query, so this is much cheaper than one request per move.

```json
{
  "id": "requestId or generated UUID",
  "boardXSize": 19,
  "boardYSize": 19,
  "turns": [ { "turnNumber": 0, "...": "an /analysis response" }, { "turnNumber": 1, "...": "..." } ]
}
```

`turns` is ordered by `turnNumber`. Each entry carries the same `id`. The engine
timeout (`katago.move_timeout_secs`) applies per turn as an inactivity limit; the
whole request is bounded by `server.request_timeout_secs`.

## GET /api/v1/analysis/ws

Upgrade to a persistent WebSocket at `ws://localhost:2718/api/v1/analysis/ws`
(use `wss://` behind TLS). Send JSON text messages with snake_case envelope fields,
preserving the existing WS wire contract. Nested `payload` and `data` fields
remain camelCase:

```json
{
  "type": "analyze",
  "request_id": "review-1",
  "payload": {
    "moves": ["D4", "Q16"],
    "analyzeTurns": [0, 2],
    "maxVisits": 50,
    "includeOwnership": true
  }
}
```

```json
{"type": "cancel", "request_id": "review-1"}
```

```json
{"type": "ping"}
```

`payload` uses the `/analysis` request fields, with these WS-specific rules:

- The envelope `request_id` must be nonblank and at most 128 UTF-8 bytes. It is echoed exactly, including surrounding whitespace; `payload.requestId` is ignored. Each query gets a fresh independent internal UUID, not an ID derived from the client ID or connection ID.
- Omit `analyzeTurns` to analyse only the final position. An explicit array must be non-empty, contain no duplicates, and contain only integers from 0 to `moves.length`. These stricter rules do not change REST: REST removes duplicates and treats `[]` like an omitted value; out-of-range turns are rejected by both transports.
- Up to 32 requests may be active on one connection, with distinct IDs. Results from different requests can interleave. The same ID can be used independently on other connections.
- Only final results for each requested turn are streamed, progressively as they finish, possibly out of turn order. These are not intermediate search updates; `reportDuringSearchEvery` is not supported.

Every server message has a `type` plus the following fields:

| Type | Fields | Meaning |
|---|---|---|
| `accepted` | `request_id`, `connection_id` | Query written to KataGo's stdin; not a guarantee that KataGo has started it or will accept the position. |
| `analysis` | `request_id`, `turn_number`, `data` | One final turn result. `data` has the `/analysis` response shape, with `data.id` equal to the envelope ID and `isDuringSearch: false`. |
| `completed` | `request_id`, `status`, `expected`, `received` | Terminal status: `completed` (success), `cancelled`, `timeout`, or `error`. `expected` counts requested turns; `received` counts final results queued for WS output, not visits or client acknowledgements. |
| `error` | `request_id`, `code`, `message` | Envelope/control rejection or query failure; see below. |
| `pong` | none | Reply to the JSON `ping` message. |

For the example above, successful completion is:

```json
{"type":"completed","request_id":"review-1","status":"completed","expected":2,"received":2}
```

Collect results by the envelope's `turn_number` (equal to `data.turnNumber`), not
arrival order. On status `completed`, both counts and the number of distinct
results must match the requested turn count (one if turns were omitted).
Cancellation stops the engine query best-effort and produces one terminal
`completed` event with status `cancelled`; if completion wins the race, its
terminal status wins.
Wait for that terminal message before reusing the ID. Late output or cleanup
from an earlier query cannot contaminate a new query using the same client ID.

### WS errors and lifetime

WS errors are JSON envelopes, not RFC 9457 HTTP problem details. Only validation
and control rejections lack a corresponding `completed`: `bad_request` before
`accepted`, `duplicate_request_id`, `too_many_requests`, and `not_found`.
`duplicate_request_id` rejects the new command, not the original execution:
keep the original ID and state even if its `accepted` has not arrived yet.
Likewise, `not_found` rejects a cancel command, not an analysis execution.

Execution errors such as `process_died`, `timeout`, `shutting_down`,
`katago_error` and `parse_error` are followed by one terminal `completed`, even
without `accepted` if sending to the engine failed or timed out. `bad_request`
after `accepted` is an engine rejection and also has a `completed`. Keep the ID
pending until that terminal message (or fail it locally on disconnect); absence
of `accepted` alone does not make an error terminal.

| Code | When |
|---|---|
| `bad_request` | Invalid JSON/message syntax, invalid ID or payload, or KataGo rejecting a request field. |
| `duplicate_request_id` | The ID is already active on this connection. |
| `too_many_requests` | The connection already has 32 active requests. |
| `not_found` | `cancel` names an ID not active on this connection. |
| `timeout` | The query's total `katago.move_timeout_secs` deadline expired; completion status is `timeout`. |
| `katago_error` | KataGo returned an engine error not tied to an invalid request field. |
| `parse_error` | The engine response could not be parsed. |
| `process_died` | The engine process died or is unavailable. |
| `shutting_down` | The server or engine is stopping; completion status is `error`. |
| `analysis_terminated` | KataGo returned `noResults: true` instead of a final result. |

The WS deadline covers the entire query, not a fresh interval per turn. The
outer HTTP `server.request_timeout_secs` does not bound an upgraded socket's
lifetime. Each incoming frame and message is limited to `server.max_body_bytes`.
WS event/output queues are bounded and socket writes have a timeout;
slow/non-reading clients are disconnected. These limits bound the WS-side
buffers, not raw engine buffering end to end.

When a handshake supplies `Origin`, it must match `server.cors_allowed_origins`
(or `*` must be configured); otherwise the upgrade is rejected with HTTP 403
[`origin-not-allowed`](problems.md#origin-not-allowed). CLI clients without
`Origin` are allowed; origin checks are not authentication.

Disconnecting or engine shutdown cancels pending queries. After disconnect,
fail all pending work locally: reconnecting has no resume or replay. Engine
failure sends terminal errors for affected queries; a still-open socket remains
usable for new requests after the supervised engine restarts.

See [clients.md](clients.md#websocket-javascript) for a client example and
[deployment.md](deployment.md#reverse-proxy) for proxy timeouts and upgrades.

## GET /api/v1/health

Returns 200 once KataGo has loaded its network and answered its first query, 503 otherwise.

```json
{
  "status": "healthy | starting | unhealthy",
  "timestamp": "2026-09-07T10:00:00Z",
  "uptime": 3600,
  "katago": { "alive": true, "ready": true, "restarts": 0, "version": "1.18.2" }
}
```

## GET /api/v1/health/live

`{"status": "live"}` with 200 while the process can still serve or recover. 503
(`dead`) only when KataGo is down and the restart budget (`max_restart_attempts`)
is spent. Use this for Kubernetes liveness.

## GET /api/v1/health/ready

`{"status": "ready"}` with 200 once KataGo is ready, else 503 (`not-ready`). Use
this for readiness and startup probes.

## GET /api/v1/version

```json
{
  "server": { "name": "katago-server", "version": "1.8.0", "gitSha": "abc1234" },
  "katago": { "version": "1.18.2", "gitHash": "…" },
  "model": { "name": "kata1-b28c512nbt-s12043015936-d5616446734.bin.gz", "humanModel": "b18c384nbt-humanv0.bin.gz" }
}
```

`katago` is omitted until KataGo has answered its first query. This endpoint never blocks on KataGo.

## POST /api/v1/cache/clear

Clears the neural network cache and waits for KataGo's acknowledgement.
Returns `{"status": "cleared", "timestamp": "..."}`.

## GET /metrics

Prometheus text exposition. See [deployment.md](deployment.md#observability) for the metric list.

## GET /api/v1/openapi.json and GET /docs

The OpenAPI 3.1 document and an interactive reference rendered from it.

## GET /

`{"name": "katago-server", "version": "...", "docs": "/docs", "openapi": "/api/v1/openapi.json", "health": "/api/v1/health"}`.

## Headers

Every HTTP response carries `x-request-id`. Clients may send their own; otherwise a UUID is generated.
CORS is enabled for the origins in `server.cors_allowed_origins` (default: any).

## Errors

Every HTTP error is `application/problem+json` (RFC 9457):

```json
{
  "type": "https://github.com/goban-app/katago-server/blob/main/docs/problems.md#invalid-request",
  "title": "Invalid Request",
  "status": 400,
  "detail": "moves[1] (\"Z99\") is not on a 19x19 board (columns A-T, skipping I, rows 1-19, or \"pass\")",
  "instance": "/api/v1/analysis",
  "requestId": "game-7",
  "field": "moves"
}
```

`instance`, `requestId` and `field` are present when known.

| Slug | Status | When |
|---|---|---|
| `invalid-request` | 400 | Validation failed, or KataGo rejected a field (`field` says which). |
| `malformed-json` | 400 | Body is not valid JSON. |
| `unsupported-media-type` | 415 | Missing `Content-Type: application/json`. |
| `payload-too-large` | 413 | Body exceeds `server.max_body_bytes`. |
| `origin-not-allowed` | 403 | The WS upgrade supplied an `Origin` not allowed by `server.cors_allowed_origins`. |
| `not-found` | 404 | Unknown path. |
| `method-not-allowed` | 405 | Wrong method for the path. |
| `analysis-timeout` | 504 | KataGo produced no result within `katago.move_timeout_secs`. The query is terminated in KataGo. |
| `request-timeout` | 504 | The whole request exceeded `server.request_timeout_secs`. |
| `engine-unavailable` | 503 | KataGo is not running; a restart is in progress or the budget is spent. |
| `analysis-terminated` | 503 | The search was terminated before producing a result (shutdown). |
| `shutting-down` | 503 | The server is stopping. |
| `overloaded` | 503 | More than `server.max_concurrent_requests` in flight. |
| `engine-error` | 502 | KataGo returned an error not tied to the request, or unparseable output. |
| `internal-error` | 500 | Unexpected server failure. |

Details and client guidance per slug: [problems.md](problems.md).
