#!/usr/bin/env python3
"""Local-only, separate-process saved-run regression. Build the CLI first."""
import collections
import json
import os
from pathlib import Path
import shutil
import subprocess
import struct
import sys
import threading
import time
import uuid
import zlib
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from urllib.parse import parse_qs, urlparse

binary = Path(sys.argv[1] if len(sys.argv) > 1 else "target/debug/paperboy").resolve()
work = Path(f".saved-smoke-{uuid.uuid4()}").resolve()
work.mkdir()
calls = collections.Counter()
delay = 0
guard = threading.Lock()
processes = []


class Endpoint(BaseHTTPRequestHandler):
    def log_message(self, *_):
        pass

    def do_GET(self):
        global delay
        url = urlparse(self.path)
        if url.path == "/setup":
            status, body = 200, '{"session":"old","token":"CAPTURE_SECRET"}'
        else:
            _, _, side, case = url.path.split("/")
            session = parse_qs(url.query)["session"][0]
            with guard:
                calls[(side, case)] += 1
            time.sleep(delay)
            bad = side == "candidate" and case == "a" and session != "fresh"
            status, body = (400, "wrong") if bad else (200, "right")
            if self.headers.get("X-Fixture-Auth") != os.environ["PAPERBOY_FIXTURE_TOKEN"]:
                status, body = 401, "fixture authentication failed"
            if self.headers.get("Authorization") != "Bearer CAPTURE_SECRET":
                status, body = 401, "fixture setup capture missing"
        body = body.encode()
        self.send_response(status)
        self.send_header("Content-Length", str(len(body)))
        self.end_headers()
        try:
            self.wfile.write(body)
        except BrokenPipeError:
            pass  # The crash-recovery case intentionally kills an in-flight client.


server = ThreadingHTTPServer(("127.0.0.1", 0), Endpoint)
thread = threading.Thread(target=server.serve_forever)
thread.start()


def command(*args, expected=0):
    result = subprocess.run(
        [str(binary), *map(str, args)], cwd=work, text=True,
        stdout=subprocess.PIPE, stderr=subprocess.PIPE, timeout=30,
    )
    assert result.returncode == expected, (args, result.returncode, result.stdout, result.stderr)
    return result


def inspect(bundle):
    return json.loads(command("--run-state", bundle, "--state-action", "inspect", "--inspect-details").stdout)


def snapshot(bundle):
    return json.loads((work / bundle / "state.json").read_text())


def wait_event(process, name, predicate=lambda _: True):
    while True:
        line = process.stderr.readline()
        assert line, f"process stopped before {name}"
        event = json.loads(line)
        if event.get("event") == name and predicate(event):
            return event


try:
    (work / "inputs").mkdir()
    def chunk(name, data):
        return struct.pack(">I", len(data)) + name + data + struct.pack(">I", zlib.crc32(name + data))
    png = (b"\x89PNG\r\n\x1a\n" + chunk(b"IHDR", struct.pack(">IIBBBBB", 1, 1, 8, 6, 0, 0, 0))
           + chunk(b"IDAT", zlib.compress(b"\0\xff\0\0\xff")) + chunk(b"IEND", b""))
    for name in ["a.png", "b.png"]:
        (work / "inputs" / name).write_bytes(png)
    (work / "inputs/cases.csv").write_text("case_id,session_id,expected\na,old,right\nb,old,right\n")
    (work / "api.hurl").write_text(
        '# setup\nGET {{base_url}}/setup\nHTTP 200\n[Captures]\n'
        'session_id: jsonpath "$.session"\ntoken: jsonpath "$.token"\n\n'
        '# check\nGET {{base_url}}/check/{{side}}/{{case_id}}?session={{session_id}}\n'
        'X-Fixture-Auth: {{private_token}}\nAuthorization: Bearer {{token}}\n'
        'HTTP 200\n# [Reports]\n# answer: body\n'
    )
    (work / "flow.trail").write_text(
        '# name: Fixture\n# root: inputs\n# collection: api.hurl\n'
        '# columns: case_id, Picture, check.answer TRUTH "{{expected}}", Result, Correct, Trend, check.Time STATISTICS(MEAN)\n'
        'REQUEST setup\n'
        'FOR ENV IN ENVS BASELINE("baseline"), COMPARISON("candidate","other")\n'
        ' FOR case_id IN TUPLES FROM "cases.csv"\n'
        '  REPORT "{{case_id}}.png" AS Picture IMAGE(HEIGHT 16)\n'
        '  REPORT REQUEST check HIDE(HttpStatus, Response)\n'
        ' END\nEND\n'
    )
    for side in ("baseline", "candidate", "other"):
        (work / f"{side}.vars").write_text(
            f"base_url=http://127.0.0.1:{server.server_port}\nside={side}\n"
            "private_token={{ env:PAPERBOY_FIXTURE_TOKEN }}\n"
        )
    os.environ["PAPERBOY_FIXTURE_TOKEN"] = "ENV_SECRET"
    common = ["-r", "flow.trail", "-e", "baseline.vars", "-e", "candidate.vars", "-e", "other.vars"]
    initial = command(*common, "--run-state", "run", "--case-key", "case_id",
            "--execution-binding", "session_id", "-o", "initial.json", expected=1)
    assert (work / "run/state.json").exists(), (initial.stdout, initial.stderr)
    state = inspect("run")
    assert len(state["executions"]) == 6
    failed = [r for r in state["executions"] if r["status"] == "error"]
    assert len(failed) == 1 and failed[0]["target"] == "candidate"
    assert failed[0]["details"]["check.HttpStatus"] == "400"
    assert failed[0]["details"]["check.Response"] == "wrong"
    assert "check.HttpStatus" not in failed[0]["cells"]
    old = snapshot("run")
    assert all(r["images"]["Picture"]["bytes"] == list(png) for r in old["rows"])
    before = calls.copy()
    os.environ["PAPERBOY_FIXTURE_TOKEN"] = "ROTATED_SECRET"
    repair = {
        "schema": 1, "run_id": state["run_id"], "revision": state["revision"],
        "overrides": [{"execution_id": failed[0]["execution_id"], "target": "candidate",
                       "values": {"session_id": "fresh"}}],
    }
    (work / "bindings.json").write_text(json.dumps(repair))
    rerun = [*common, "--run-state", "run", "--state-action", "rerun",
             "--expected-revision", str(state["revision"]), "--select", failed[0]["execution_id"],
             "--bindings", "bindings.json", "-o", "patched.json", "-o", "patched.html"]
    command(*rerun)
    assert calls - before == {("candidate", "a"): 1}, (before, calls)
    patched = snapshot("run")
    assert not patched["errors"]
    for a, b in zip(old["rows"], patched["rows"]):
        if a["execution_id"] != failed[0]["execution_id"]:
            assert a == b, "unselected raw side changed"
    assert [r["key"] for r in old["rows"]] == [r["key"] for r in patched["rows"]]
    before = calls.copy()
    command(*rerun, expected=1)  # stale revision rejected before HTTP
    assert calls == before
    command(*common, "--run-state", "run", "--state-action", "rerun",
            "--expected-revision", str(patched["revision"]), "--select", "missing",
            "-o", "bad.json", expected=1)
    assert calls == before and snapshot("run") == patched
    (work / "inputs/a.png").rename(work / "inputs/hidden-a.png")
    command("--run-state", "run", "--state-action", "render", "-o", "rendered.json")
    (work / "inputs/hidden-a.png").rename(work / "inputs/a.png")
    assert (work / "rendered.json").read_bytes() == (work / "patched.json").read_bytes()
    assert calls == before
    for file in (work / "run").iterdir():
        if file.is_file():
            text = file.read_text()
            assert all(secret not in text for secret in ("ENV_SECRET", "CAPTURE_SECRET", "ROTATED_SECRET"))
    # Relocate the complete sources and corpus, re-resolving current credentials.
    relocated = work / "relocated"
    relocated.mkdir()
    shutil.copytree(work / "inputs", relocated / "inputs")
    for name in ["flow.trail", "api.hurl", "baseline.vars", "candidate.vars", "other.vars"]:
        shutil.copy(work / name, relocated / name)
    baseline = next(r for r in patched["rows"] if r["role"] == "Baseline" and r["key"] == ["a"])
    command("-r", relocated / "flow.trail", "-e", relocated / "baseline.vars",
            "-e", relocated / "candidate.vars", "-e", relocated / "other.vars",
            "--run-state", "run", "--state-action", "rerun",
            "--expected-revision", str(patched["revision"]), "--select", baseline["execution_id"],
            "-o", "relocated.json")
    assert calls - before == {("baseline", "a"): 1}
    current = snapshot("run")
    before = calls.copy()
    b = next(r for r in current["rows"] if r["target"] == "candidate" and r["key"] == ["b"])
    command(*common, "--run-state", "run", "--state-action", "rerun",
            "--expected-revision", str(current["revision"]), "--select", b["execution_id"],
            "--whole-comparison", "-o", "wide.json")
    assert calls - before == {("baseline", "b"): 1, ("candidate", "b"): 1, ("other", "b"): 1}
    current = snapshot("run")
    before = calls.copy()
    guarded = [*common, "--run-state", "run", "--state-action", "rerun",
               "--expected-revision", str(current["revision"]), "--select", b["execution_id"]]
    repair.update(revision=current["revision"])
    repair["overrides"][0].update(execution_id=b["execution_id"], target="baseline")
    (work / "invalid-bindings.json").write_text(json.dumps(repair))
    command(*guarded, "--bindings", "invalid-bindings.json", "-o", "bad.json", expected=1)
    command(*guarded, "-o", "run/state.json", expected=1)
    manifest = work / "inputs/cases.csv"
    original = manifest.read_text()
    manifest.write_text(original.replace("right", "changed"))
    command(*guarded, "-o", "bad.json", expected=1)
    manifest.write_text(original)
    env = work / "candidate.vars"
    original = env.read_text()
    env.write_text(original.replace(str(server.server_port), "1"))
    command(*guarded, "-o", "bad.json", expected=1)
    env.write_text(original)
    assert calls == before and snapshot("run") == current
    # A completed error is not pending. Stop after the candidate failure, then
    # resume in a new process; neither successful baselines nor that failure replay.
    delay = 0.12
    process = subprocess.Popen(
        [str(binary), *common, "--run-state", "stopped", "--case-key", "case_id",
         "--execution-binding", "session_id", "--progress-json", "--stop-on-stdin",
         "-o", "partial.json"], cwd=work, text=True,
        stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=subprocess.PIPE,
    )
    processes.append(process)
    wait_event(process, "row_completed", lambda e: e["status"] == "error")
    process.stdin.write("stop\n")
    process.stdin.flush()
    _, stderr = process.communicate(timeout=30)
    assert process.returncode == 4, stderr
    stopped = inspect("stopped")
    completed = {r["execution_id"] for r in stopped["executions"] if r["status"] in ("success", "error")}
    assert any(r["status"] == "error" for r in stopped["executions"])
    assert len(completed) < 6
    before = calls.copy()
    process = subprocess.Popen(
        [str(binary), *common, "--run-state", "stopped", "--state-action", "resume",
         "--expected-revision", str(stopped["revision"]), "--progress-json",
         "--stop-on-stdin", "-o", "partial-again.json"], cwd=work, text=True,
        stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=subprocess.PIPE,
    )
    processes.append(process)
    wait_event(process, "row_started")
    process.stdin.write("stop\n")
    process.stdin.flush()
    _, stderr = process.communicate(timeout=30)
    assert process.returncode == 4, stderr
    stopped = inspect("stopped")
    assert any(r["status"] in ("pending", "in_flight") for r in stopped["executions"])
    assert calls[("baseline", "a")] == before[("baseline", "a")]
    assert calls[("candidate", "a")] == before[("candidate", "a")]
    before = calls.copy()
    command(*common, "--run-state", "stopped", "--state-action", "resume",
            "--expected-revision", str(stopped["revision"]), "-o", "resumed.json", expected=1)
    assert calls[("baseline", "a")] == before[("baseline", "a")]
    assert calls[("candidate", "a")] == before[("candidate", "a")]
    assert all(r["status"] in ("success", "error") for r in inspect("stopped")["executions"])
    # Abrupt termination leaves a readable checkpoint and releases the OS lock.
    process = subprocess.Popen(
        [str(binary), *common, "--run-state", "killed", "--case-key", "case_id",
         "--progress-json", "-o", "killed.json"], cwd=work, text=True,
        stdout=subprocess.PIPE, stderr=subprocess.PIPE,
    )
    processes.append(process)
    wait_event(process, "row_completed")
    process.kill()
    process.communicate(timeout=30)
    killed = inspect("killed")
    assert any(r["status"] == "success" for r in killed["executions"])
    before = calls.copy()
    command(*common, "--run-state", "killed", "--state-action", "resume",
            "--expected-revision", str(killed["revision"]), "-o", "recovered.json", expected=1)
    assert calls[("baseline", "a")] == before[("baseline", "a")]
    print("saved-run local smoke: save/inspect/exact sides/comparison-wide/validation/render/relocate/stop/resume/kill passed")
finally:
    for process in processes:
        if process.poll() is None:
            process.kill()
        process.communicate(timeout=5)
    server.shutdown()
    server.server_close()
    thread.join()
    shutil.rmtree(work)
