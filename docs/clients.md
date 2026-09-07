# Client snippets

Generate a typed HTTP client from `GET /api/v1/openapi.json` with any OpenAPI 3.1
generator when you need more than the snippets below.

## Python

```python
import requests

BASE = "http://localhost:2718"

def analyse(moves, **kwargs):
    r = requests.post(f"{BASE}/api/v1/analysis", json={"moves": moves, **kwargs}, timeout=120)
    if r.status_code >= 400:
        problem = r.json()  # RFC 9457
        raise RuntimeError(f"{problem['title']}: {problem['detail']} (field={problem.get('field')})")
    return r.json()

result = analyse(["D4", "Q16", "R4"], komi=7.5, rules="chinese", maxVisits=50, includeOwnership=True)
best = result["moveInfos"][0]
print(best["moveCoord"], f"{best['winrate']:.1%}", best["scoreLead"])

review = requests.post(f"{BASE}/api/v1/analysis/game",
                       json={"moves": ["D4", "Q16", "R4"], "maxVisits": 20}, timeout=600).json()
for turn in review["turns"]:
    root = turn["rootInfo"]
    print(turn["turnNumber"], root["currentPlayer"], f"{root['winrate']:.1%}")
```

## JavaScript / TypeScript

```ts
const BASE = "http://localhost:2718";

async function analyse(body: Record<string, unknown>) {
  const res = await fetch(`${BASE}/api/v1/analysis`, {
    method: "POST",
    headers: { "Content-Type": "application/json" },
    body: JSON.stringify(body),
  });
  if (!res.ok) {
    const problem = await res.json(); // { type, title, status, detail, field?, requestId? }
    throw new Error(`${problem.title}: ${problem.detail}`);
  }
  return res.json();
}

const result = await analyse({ moves: ["D4", "Q16"], komi: 7.5, maxVisits: 50, includeOwnership: true });
console.log(result.moveInfos[0].moveCoord, result.rootInfo.winrate);
```

## WebSocket (JavaScript)

This browser example keeps the socket open after the query. Use `wss://` when
the page uses HTTPS. See the [WS protocol](api.md#get-apiv1analysisws) for limits
and error codes; OpenAPI client generation covers HTTP, not these messages.

```javascript
const socket = new WebSocket("ws://localhost:2718/api/v1/analysis/ws");
const requestId = crypto.randomUUID();
const payload = {
  moves: ["D4", "Q16"],
  boardXSize: 19,
  boardYSize: 19,
  analyzeTurns: [0, 2],
  maxVisits: 50,
  includeOwnership: true,
  includePolicy: true,
};
const requestedTurns = new Set(payload.analyzeTurns ?? [payload.moves.length]);
const results = new Map();
let accepted = false;
let done = false;

socket.addEventListener("open", () => {
  socket.send(JSON.stringify({ type: "analyze", request_id: requestId, payload }));
});
socket.addEventListener("message", ({ data }) => {
  const message = JSON.parse(data);
  if (message.type === "pong") return;
  if (message.request_id !== requestId || done) return;

  switch (message.type) {
    case "accepted":
      accepted = true;
      console.log("Written to KataGo on connection", message.connection_id);
      break;
    case "analysis": {
      const result = message.data;
      const points = payload.boardXSize * payload.boardYSize;
      console.assert(requestedTurns.has(message.turn_number), "Unexpected turn");
      console.assert(!results.has(message.turn_number), "Duplicate turn");
      console.assert(result.id === requestId && result.turnNumber === message.turn_number);
      console.assert(result.isDuringSearch === false, "Expected a final result");
      if (result.ownership) console.assert(result.ownership.length === points);
      if (result.policy) console.assert(result.policy.length === points + 1); // Last entry is pass.
      results.set(message.turn_number, result);
      console.log(message.turn_number, result.rootInfo.currentPlayer,
                  result.rootInfo.winrate, result.moveInfos[0]?.moveCoord);
      break;
    }
    case "error":
      console.error(message.code, message.message);
      // A rejected duplicate or cancel must not release the original analysis.
      if (message.code === "duplicate_request_id" || message.code === "not_found") break;
      // Only submission rejections end here; execution errors still get completed.
      if (!accepted && (message.code === "bad_request" || message.code === "too_many_requests")) {
        done = true;
      }
      break;
    case "completed":
      done = true;
      if (message.status === "completed") {
        console.assert(message.expected === requestedTurns.size);
        console.assert(message.received === message.expected && results.size === message.received);
        console.log([...results.values()].sort((a, b) => a.turnNumber - b.turnNumber));
      } else {
        console.error("Query ended", message.status, message.received, message.expected);
      }
      break;
  }
});
socket.addEventListener("close", () => {
  if (!done) console.error("Disconnected before completion; query will not resume");
  done = true;
});
socket.addEventListener("error", () => console.error("WebSocket transport error"));

// While open, send a JSON ping; after acceptance, optionally cancel this query:
// socket.send(JSON.stringify({ type: "ping" }));
// socket.send(JSON.stringify({ type: "cancel", request_id: requestId }));
```

For multiple concurrent IDs, allocate separate `accepted`, `done` and result
state when sending each request (up to 32 active); never overwrite a pending
entry. Release a submitted ID without `completed` only for pre-acceptance
`bad_request` or `too_many_requests`. Execution errors, including `process_died`,
`timeout` and `shutting_down`, require waiting for `completed` even if no
`accepted` arrived. A `duplicate_request_id` must not delete the original entry,
and `not_found` only rejects a cancel. Sending `cancel` does not release the ID.

Compare `completed.received` (results queued by the server, not client ACKs) with
the results actually collected locally. Fail pending queries on close and
explicitly submit new work after reconnect; there is no replay. Engine restart
does not require reconnecting a socket that remains open after terminal errors.

## Rust

```rust
// reqwest = { version = "0.12", features = ["json"] }, serde_json = "1"
let client = reqwest::Client::new();
let res = client
    .post("http://localhost:2718/api/v1/analysis/game")
    .json(&serde_json::json!({ "moves": ["D4", "Q16", "R4"], "maxVisits": 20 }))
    .send()
    .await?;
if !res.status().is_success() {
    let problem: serde_json::Value = res.json().await?;
    anyhow::bail!("{}: {}", problem["title"], problem["detail"]);
}
let review: serde_json::Value = res.json().await?;
for turn in review["turns"].as_array().unwrap() {
    println!("{} {}", turn["turnNumber"], turn["rootInfo"]["winrate"]);
}
```

## Handicap games

Place the handicap stones with `initialStones`; White then moves first by default.

```json
{ "initialStones": [["B", "D4"], ["B", "Q16"], ["B", "D16"]], "moves": ["Q4", "R6"], "komi": 0.5, "rules": "chinese" }
```

## Human-style analysis

Requires a `human-*` or `combo-*` image (or `katago.human_model_path`):

```json
{ "moves": ["D4", "Q16"], "maxVisits": 20, "includePolicy": true, "overrideSettings": { "humanSLProfile": "rank_5k" } }
```

The response gains `humanPolicy`, `moveInfos[].humanPrior` and `rootInfo.humanWinrate`.
