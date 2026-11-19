"""Exercise the real CLI against a local fake Responses API; no credentials needed."""

import json
import os
import signal
from pathlib import Path
import subprocess
import tempfile
import threading
import time
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer


BINARY = Path(__file__).resolve().parents[1] / "target/debug/spark"


def message(text):
    return {"type": "message", "role": "assistant", "content": [
        {"type": "output_text", "text": text, "annotations": []}
    ]}


def call(name, args, call_id):
    return {"type": "function_call", "name": name,
            "arguments": json.dumps(args), "call_id": call_id}


def run_case(replies, inputs, check, cancel_command=False, model="test-model"):
    requests = []
    errors = []

    class Handler(BaseHTTPRequestHandler):
        def log_message(self, *_):
            pass

        def do_POST(self):
            try:
                assert self.path == "/v1/responses"
                assert self.headers["Authorization"] == "Bearer fake-test-key"
                request = json.loads(self.rfile.read(int(self.headers["Content-Length"])))
                assert request["model"] == (model or "gpt-5.4-mini")
                requests.append(request)
                reply = replies[len(requests) - 1]
                status, body = (reply if isinstance(reply, tuple) else
                                (200, {"status": "completed", "output": reply}))
            except Exception as error:
                errors.append(error)
                status, body = 500, {"error": {"message": "bad test request"}}
            data = json.dumps(body).encode()
            self.send_response(status)
            self.send_header("Content-Type", "application/json")
            self.send_header("Content-Length", str(len(data)))
            self.end_headers()
            self.wfile.write(data)

    with ThreadingHTTPServer(("127.0.0.1", 0), Handler) as server:
        thread = threading.Thread(target=server.serve_forever, daemon=True)
        thread.start()
        try:
            with tempfile.TemporaryDirectory(prefix="spark-smoke-") as project:
                root = Path(project)
                (root / "README.md").write_text("A tiny demo project.\n")
                env = dict(os.environ, OPENAI_API_KEY="fake-test-key", OPENAI_MODEL="test-model",
                           OPENAI_BASE_URL=f"http://127.0.0.1:{server.server_port}/v1")
                if model is None:
                    env.pop("OPENAI_MODEL", None)
                else:
                    env["OPENAI_MODEL"] = model
                if cancel_command:
                    with subprocess.Popen([str(BINARY)], text=True, stdin=subprocess.PIPE,
                                          stdout=subprocess.PIPE, stderr=subprocess.PIPE,
                                          cwd=root, env=env) as process:
                        process.stdin.write(inputs)
                        process.stdin.flush()
                        deadline = time.monotonic() + 5
                        while not (root / "started").exists():
                            if time.monotonic() > deadline:
                                process.kill()
                                raise AssertionError("command never started")
                            time.sleep(0.01)
                        process.send_signal(signal.SIGINT)
                        stdout, stderr = process.communicate("what happened?\n/exit\n", timeout=5)
                        result = subprocess.CompletedProcess(process.args, process.returncode, stdout, stderr)
                    time.sleep(0.6)
                else:
                    result = subprocess.run([str(BINARY)], input=inputs, text=True,
                                            capture_output=True, cwd=root, env=env, timeout=20)
                assert result.returncode == 0, result.stderr
                assert not errors, errors
                assert len(requests) == len(replies), (len(requests), result.stdout, result.stderr)
                check(requests, result, root)
        finally:
            server.shutdown()
            thread.join()


def check_flow(requests, result, root):
    assert (root / "approved").read_text() == "ok"
    assert not (root / "denied").exists()
    read_result = requests[1]["input"][-1]
    assert read_result["call_id"] == "read-1"
    assert "tiny demo project" in read_result["output"]
    assert any(item.get("type") == "reasoning" for item in requests[1]["input"])
    command_result = requests[2]["input"][-1]
    assert command_result["call_id"] == "run-1"
    assert "exit status: 0" in command_result["output"]
    assert "key-removed" in command_result["output"]
    assert len(requests[3]["input"]) > len(requests[2]["input"])
    assert requests[4]["input"] == [{"role": "user", "content": "try another command"}]
    assert any("User cancelled" in item.get("output", "") for item in requests[5]["input"])
    assert "Follow-up retained." in result.stdout
    assert "Stopped." in result.stdout
    assert all(request["store"] is False for request in requests)


run_case([
    [{"type": "reasoning", "id": "reasoning-test", "summary": [], "encrypted_content": "test"},
     call("read_file", {"path": "README.md"}, "read-1")],
    [call("run_command", {"command": "printf ok > approved; test -z \"$OPENAI_API_KEY\" && printf key-removed",
                          "reason": "Make an approved demo file."}, "run-1")],
    [message("Read and command completed.")],
    [message("Follow-up retained.")],
    [call("run_command", {"command": "touch denied", "reason": "Test denial."}, "deny-1")],
    [message("I will not retry the denied command.")],
], "inspect and run\ny\nwhat happened?\n/clear\ntry another command\n\nnever mind\n/exit\n", check_flow)
print("PASS: reads, approval, real command output, follow-up, reset, denial")


def check_recovery(requests, result, root):
    assert "API 401" in result.stderr
    assert "fake-test-key" not in result.stderr
    assert "Recovered." in result.stdout
    assert "Tool error" in requests[2]["input"][-1]["output"]


run_case([
    (401, {"error": {"message": "Invalid key fake-test-key"}}),
    [call("read_file", {"path": "missing.txt"}, "missing")],
    [message("Recovered.")],
], "first\ntry again\n/exit\n", check_recovery)
print("PASS: API error recovery, credential redaction, tool error feedback")


def check_limit(requests, result, root):
    assert len(requests) == 9
    assert requests[-1]["tool_choice"] == "none"
    assert "Limit summary." in result.stdout


run_case([
    [call("read_file", {"path": "README.md"}, f"read-{index}")]
    for index in range(8)
] + [[message("Limit summary.")]], "inspect\n/exit\n", check_limit, model=None)
print("PASS: default model works without OPENAI_MODEL; tool loop stops after eight rounds")


def check_cancel(requests, result, root):
    assert not (root / "leaked").exists(), "command child survived cancellation"
    assert "cancelled by user" in result.stdout
    outputs = [item["output"] for item in requests[1]["input"]
               if item.get("type") == "function_call_output"]
    assert len(outputs) == 1 and "cancelled by user" in outputs[0]
    assert "began" in outputs[0], "partial command output was lost"
    assert "Cancellation retained." in result.stdout


run_case([
    [call("run_command", {"command": "printf began; touch started; sleep 0.5; touch leaked",
                          "reason": "Exercise cancellation."}, "cancel-1")],
    [message("Cancellation retained.")],
], "run\ny\n", check_cancel, cancel_command=True)
print("PASS: Ctrl-C stops command children and preserves partial output for follow-ups")

env = {key: value for key, value in os.environ.items()
       if key not in {"OPENAI_API_KEY", "OPENAI_MODEL"}}
help_result = subprocess.run([str(BINARY), "--help"], env=env, capture_output=True, text=True)
assert help_result.returncode == 0 and "Usage: spark" in help_result.stdout
missing_key = subprocess.run([str(BINARY)], env=env, capture_output=True, text=True)
assert missing_key.returncode != 0 and "OPENAI_API_KEY" in missing_key.stderr
print("PASS: help without credentials and actionable missing-key error")
