#!/usr/bin/env python3
"""A stand-in for `katago analysis` that speaks the same JSON line protocol.

Used by the integration tests so the server can be exercised end to end without
a neural network. Behaviour is steered through `overrideSettings` keys that real
KataGo would never see:

  fakeDelayMs   milliseconds before each turn; scalar or list in emission order
                (turns past the end of a delay list have no delay)
  fakeCrash     exit immediately with status 7 (drives restart tests)
  fakeWarn      emit a warning line before the result
  fakeError     emit an error without a field (an "engine" error)
  fakeTurnOrder replace the requested turn emission order with this list
  fakeDuplicate true repeats each final immediately; a positive number delays it
                by that many milliseconds (including after the last final)
  fakeDuringSearch emit a search-progress response before each final
  fakeErrorAfterTurns emit fakeError after this many finals instead of immediately
  fakeNoResults emit noResults for the requested turns without any analysis
  fakeUnexpectedTurn replace the first final's turnNumber with this number
  fakeMalformed make the first final's moveInfos invalid for response parsing
  fakeIgnoreTerminate deliberately finish despite cancellation, testing stale IDs
  fakeHuman     include deterministic optional Human SL response fields

Environment:
  FAKE_KATAGO_LOG            append every received query line to this file
  FAKE_KATAGO_STARTUP_DELAY  seconds to sleep before reading stdin
  FAKE_KATAGO_CRASH_ON_START exit with status 3 before reading anything
"""
import json
import os
import sys
import threading
import time

KNOWN_RULES = {
    "chinese", "japanese", "korean", "aga", "tromp-taylor", "bga", "new-zealand",
    "stone-scoring", "chinese-ogs", "chinese-kgs", "aga-button", "ancient-area",
    "ancient-territory",
}
OUTPUT_LOCK = threading.Lock()


def emit(obj):
    with OUTPUT_LOCK:
        sys.stdout.write(json.dumps(obj) + "\n")
        sys.stdout.flush()


def log_query(line):
    path = os.environ.get("FAKE_KATAGO_LOG")
    if path:
        with open(path, "a", encoding="utf-8") as f:
            f.write(line + "\n")


def analysis(q, cancelled):
    qid = q.get("id")
    overrides = q.get("overrideSettings") or {}
    moves = q.get("moves", [])
    turns = q.get("analyzeTurns", [len(moves)])
    order = overrides.get("fakeTurnOrder", turns)
    completed = set()

    def no_results():
        for turn in turns:
            if turn not in completed:
                emit({"id": qid, "turnNumber": turn, "isDuringSearch": False,
                      "noResults": True})

    def wait(milliseconds):
        if overrides.get("fakeIgnoreTerminate"):
            time.sleep(milliseconds / 1000.0)
            return False
        if cancelled.wait(milliseconds / 1000.0):
            no_results()
            return True
        return False

    if "fakeCrash" in overrides:
        sys.stderr.write("fake katago: crashing on purpose\n")
        sys.stderr.flush()
        os._exit(7)
    if "fakeWarn" in overrides:
        emit({"field": "fakeWarn", "id": qid, "warning": "Unexpected or unused field"})
    error_after = overrides.get("fakeErrorAfterTurns", 0)
    if "fakeError" in overrides and error_after == 0:
        emit({"error": overrides["fakeError"], "id": qid})
        return
    seen = set()
    for i, (_, coord) in enumerate(moves):
        if coord.lower() != "pass":
            if coord in seen:
                emit({"error": f"Illegal move {i}: {coord}", "field": "moves", "id": qid})
                return
            seen.add(coord)
    rules = q.get("rules")
    if isinstance(rules, str) and rules not in KNOWN_RULES:
        emit({"error": f"Could not parse rules: {rules}", "field": "rules", "id": qid})
        return

    x, y = q["boardXSize"], q["boardYSize"]
    visits = q.get("maxVisits", 10)
    first = (q.get("initialPlayer") or ("W" if q.get("initialStones") else "B")).upper()
    for index, turn in enumerate(order):
        delay = overrides.get("fakeDelayMs", 0)
        if isinstance(delay, list):
            delay = delay[index] if index < len(delay) else 0
        if wait(delay):
            return
        if overrides.get("fakeNoResults"):
            no_results()
            return
        if turn < len(moves):
            player = moves[turn][0].upper()
        else:
            player = first if turn % 2 == 0 else ("W" if first == "B" else "B")
        best = "Q16" if x >= 16 and y >= 16 else "C3"
        move_info = {
            "move": best, "visits": visits, "winrate": 0.5, "scoreMean": 0.1,
            "scoreStdev": 10.0, "scoreLead": 0.1, "scoreSelfplay": 0.2, "utility": 0.0,
            "utilityLcb": -0.1, "lcb": 0.45, "prior": 0.3, "order": 0, "pv": [best],
            "edgeVisits": visits, "edgeWeight": 1.0, "weight": 1.0, "playSelectionValue": 1.0,
        }
        if q.get("includePVVisits"):
            move_info["pvVisits"] = [visits]
            move_info["pvEdgeVisits"] = [visits]
        if q.get("includeMovesOwnership"):
            move_info["ownership"] = [0.0] * (x * y)
        resp = {
            "id": qid, "isDuringSearch": False, "turnNumber": turn,
            "moveInfos": [move_info],
            "rootInfo": {
                "winrate": 0.5, "scoreLead": 0.1, "scoreSelfplay": 0.2, "scoreStdev": 10.0,
                "utility": 0.0, "visits": visits, "currentPlayer": player, "weight": 1.0,
                "rawWinrate": 0.5, "rawLead": 0.1, "symHash": "00", "thisHash": "00",
            },
        }
        if q.get("includeOwnership"):
            resp["ownership"] = [0.0] * (x * y)
        if q.get("includeOwnershipStdev"):
            resp["ownershipStdev"] = [0.1] * (x * y)
        if q.get("includePolicy"):
            resp["policy"] = [1.0 / (x * y + 1)] * (x * y + 1)
        if overrides.get("fakeHuman"):
            move_info["humanPrior"] = 0.25
            resp["rootInfo"].update({
                "humanWinrate": 0.55, "humanScoreMean": 1.5, "humanScoreStdev": 9.0,
                "humanStWrError": 0.02, "humanStScoreError": 0.3,
            })
            resp["humanPolicy"] = [1.0 / (x * y + 1)] * (x * y + 1)
        if overrides.get("fakeDuringSearch"):
            emit({**resp, "isDuringSearch": True})
        if index == 0 and "fakeUnexpectedTurn" in overrides:
            resp["turnNumber"] = overrides["fakeUnexpectedTurn"]
        if index == 0 and overrides.get("fakeMalformed"):
            resp["moveInfos"] = "not an array"
        emit(resp)
        completed.add(turn)
        if "fakeError" in overrides and len(completed) == error_after:
            emit({"error": overrides["fakeError"], "id": qid})
            return
        duplicate = overrides.get("fakeDuplicate", False)
        if duplicate:
            if wait(0 if duplicate is True else duplicate):
                return
            emit(resp)


def main():
    time.sleep(float(os.environ.get("FAKE_KATAGO_STARTUP_DELAY", "0")))
    sys.stderr.write("fake katago: loading pretend model\n")
    sys.stderr.flush()
    if os.environ.get("FAKE_KATAGO_CRASH_ON_START"):
        sys.stderr.write("fake katago: crashing on start\n")
        sys.exit(3)
    workers = {}
    for raw in sys.stdin:
        line = raw.strip()
        if not line:
            continue
        log_query(line)
        try:
            q = json.loads(line)
        except ValueError:
            emit({"error": "could not parse input line as json request: " + line})
            continue
        qid = q.get("id")
        action = q.get("action")
        if action == "query_version":
            emit({"action": action, "git_hash": "fakehash123", "id": qid, "version": "9.9.9-fake"})
        elif action == "clear_cache":
            emit({"action": action, "id": qid})
        elif action == "terminate_all":
            for _, cancelled in workers.values():
                cancelled.set()
            emit({"action": action, "id": qid})
        elif action == "terminate":
            worker = workers.get(q.get("terminateId"))
            if worker:
                worker[1].set()
            emit({"action": action, "id": qid, "terminateId": q.get("terminateId")})
        elif action:
            emit({"error": f"unknown action {action}", "id": qid})
        else:
            workers = {key: worker for key, worker in workers.items() if worker[0].is_alive()}
            cancelled = threading.Event()
            thread = threading.Thread(target=analysis, args=(q, cancelled))
            workers[qid] = (thread, cancelled)
            thread.start()
    # Drain non-daemon workers after stdin closes; terminate_all remains readable
    # and acknowledged even while analysis workers are waiting on their delays.
    for thread, _ in workers.values():
        thread.join()


if __name__ == "__main__":
    main()
