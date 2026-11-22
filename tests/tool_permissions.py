"""Actual ACP tool permission boundaries; disposable scripted provider/MCP only.

Use AGENT_RUNTIME_TEST_BINARY to test an immutable baseline or candidate. The
legacy approval policy intentionally reproduces trusting rawInput identifiers;
it is a test consumer, never the recommended permission policy.
"""
import copy
import json
import os
import sqlite3
import subprocess
import tempfile
import threading
import unittest
import uuid
from contextlib import closing
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from pathlib import Path

from protocol import BINARY, Client
from responses_ws import ResponsesWebSocketMixin


META = "com.microsoft.amplifier.tool"
POLICY_META = "com.microsoft.amplifier.approval-policy"
POLICY_PRODUCER = "microsoft.amplifier-constellation-runtime"


class PermissionClient(Client):
    def finish(self, request_id, approve=False, legacy=False):
        while True:
            frame = self.q.get(timeout=15)
            if frame.get("method") == "session/request_permission":
                self.callbacks.append(frame)
                raw = frame["params"]["toolCall"].get("rawInput")
                allowed = approve
                if legacy:
                    allowed = (frame["params"].get("sessionId") == self.session
                               and isinstance(raw, dict)
                               and raw.get("server") == "minecraft"
                               and raw.get("tool") == "observe")
                outcome = ({"outcome": "selected", "optionId": "allow_once"}
                           if allowed else {"outcome": "cancelled"})
                self.send({"jsonrpc": "2.0", "id": frame["id"],
                           "result": {"outcome": outcome}})
            elif frame.get("method") == "elicitation/create":
                self.callbacks.append(frame)
                self.send({"jsonrpc": "2.0", "id": frame["id"],
                           "result": {"action": "decline", "content": {}}})
            elif "method" in frame:
                self.updates.append(frame)
            elif frame.get("id") == request_id:
                return frame
            else:
                raise AssertionError(frame)


class Handler(ResponsesWebSocketMixin, BaseHTTPRequestHandler):
    def log_message(self, *_):
        pass

    def reply(self, body, result):
        self.send_response(200)
        self.send_header("Content-Type", "application/json")
        if body["method"] == "initialize" or not getattr(self.server, "conflicting_session_headers", False):
            self.send_header("Mcp-Session-Id", "permission-fixture")
        self.end_headers()
        self.wfile.write(json.dumps({"jsonrpc": "2.0", "id": body["id"],
                                    "result": result}).encode())

    def do_POST(self):
        body = json.loads(self.rfile.read(int(self.headers["Content-Length"])))
        if self.path == "/mcp":
            method = body.get("method")
            if hasattr(self.server, "mcp_traffic"):
                self.server.mcp_traffic.append((method, self.headers.get("Mcp-Session-Id")))
            if method in ("notifications/initialized", "notifications/cancelled"):
                self.send_response(202)
                if method == "notifications/cancelled" and getattr(self.server, "conflicting_session_headers", False):
                    self.send_header("Mcp-Session-Id", "from-cancellation")
                self.end_headers()
                return
            if method == "initialize":
                result = {"protocolVersion": getattr(self.server, "mcp_protocol", "2025-06-18"), "capabilities": {"tools": {}},
                          "serverInfo": {"name": "permission-fixture", "version": "1"}}
            elif method == "tools/list":
                result = {"tools": [{"name": "observe", "description": "Fixture observation",
                                     "inputSchema": getattr(self.server, "mcp_schema", {"type": "object"}),
                                     "annotations": {"readOnlyHint": True}}]}
                if hasattr(self.server, "recorded_values"):
                    result["tools"].append({"name": "record", "description": "Append a fixture value",
                                            "inputSchema": {"type": "object", "required": ["value"],
                                                            "properties": {"value": {"type": "string"}}}})
            elif method == "tools/call":
                self.server.effects.append(copy.deepcopy(body["params"]))
                if body["params"]["name"] == "record":
                    self.server.recorded_values.append(body["params"]["arguments"]["value"])
                if body["params"] == getattr(self.server, "lose_response_for", None):
                    self.server.interrupted_request_id = body["id"]
                    if getattr(self.server, "conflicting_session_headers", False):
                        self.send_response(200)
                        self.send_header("Content-Type", "application/json")
                        self.send_header("Mcp-Session-Id", "from-incomplete-result")
                        self.end_headers()
                        self.wfile.write(b'{"jsonrpc":')
                    self.close_connection = True
                    return  # The tool ran, but no valid terminal result reaches the client.
                observation = (json.dumps(self.server.recorded_values) if hasattr(self.server, "recorded_values")
                               else "Fixture observation returned")
                result = {"content": [{"type": "text", "text": observation}]}
            else:
                result = {}
            self.reply(body, result)
            return
        if self.headers.get("Authorization") != "Bearer fixture":
            self.send_response(401)
            self.end_headers()
            return
        self.server.requests.append(copy.deepcopy(body))
        messages = body["input"]
        last = max(i for i, item in enumerate(messages) if item.get("role") == "user")
        label = messages[last]["content"]
        if not isinstance(label, str):
            label = "\n".join(item.get("text", "") for item in label)
        label = label.strip()
        program = self.server.programs[label]
        completed = sum(item.get("type") == "function_call_output" for item in messages[last + 1:])
        output = []
        if completed < len(program):
            step = program[completed]
            name = step["name"]
            if name == "@mcp":
                name = next(tool["name"] for tool in body["tools"]
                            if tool.get("name", "").startswith("mcp_"))
            if step.get("unlisted") or any(tool.get("name") == name for tool in body["tools"]):
                call = {"type": "function_call", "call_id": f"{label}-call-{completed}",
                        "name": name, "arguments": json.dumps(step["arguments"])}
                call.update(step.get("provider_fields", {}))
                output = [call]
                self.server.emitted.append(copy.deepcopy(call))
        if not output:
            output = [{"type": "message", "role": "assistant",
                       "content": [{"type": "output_text", "text": "Fixture finished"}]}]
        event = {"type": "response.completed", "response": {"status": "completed", "output": output,
                 "usage": {"input_tokens": 10, "output_tokens": 10}}}
        self.send_response(200)
        self.send_header("Content-Type", "text/event-stream")
        self.end_headers()
        self.wfile.write(("data: " + json.dumps(event) + "\n\n").encode())
        self.wfile.flush()


class ToolPermissions(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory()
        self.home = Path(self.tmp.name) / "runtime"
        self.work = Path(self.tmp.name) / "work"
        self.clients = []
        self.server = ThreadingHTTPServer(("127.0.0.1", 0), Handler)
        self.server.programs = {}
        self.server.requests = []
        self.server.emitted = []
        self.server.effects = []
        threading.Thread(target=self.server.serve_forever, daemon=True).start()
        self.url = f"http://127.0.0.1:{self.server.server_port}"
        subprocess.run([str(BINARY), "--home", str(self.home), "init", "--workspace", str(self.work),
                        "--model", "fixture"], check=True, capture_output=True)
        path = self.home / "runtime.json"
        config = json.loads(path.read_text())
        config["provider"].update(endpoint=self.url + "/responses", api_key_env="RUNTIME_FIXTURE_KEY",
                                  reasoning_effort=None)
        config["context"]["provider_compaction"] = False
        path.write_text(json.dumps(config))

    def tearDown(self):
        for client in self.clients:
            client.close()
        self.server.shutdown()
        self.server.server_close()
        self.tmp.cleanup()

    def client(self, mcp_server=None, forms=False):
        client = PermissionClient(self.home)
        self.clients.append(client)
        caps = {"elicitation": {"form": {}}} if forms else {}
        initialized = client.call("initialize", {"protocolVersion": 1, "clientCapabilities": caps})
        self.assertIn("result", initialized)
        client.initialized = initialized["result"]
        servers = ([] if mcp_server is None else [{"name": mcp_server, "type": "http",
                    "url": self.url + "/mcp", "headers": []}])
        new = client.call("session/new", {"cwd": str(self.work), "mcpServers": servers})
        client.session = new["result"]["sessionId"]
        return client

    def prompt(self, client, steps, **policy):
        label = f"case-{len(self.server.programs)}"
        self.server.programs[label] = steps
        result = client.call("session/prompt", {"sessionId": client.session,
                             "prompt": [{"type": "text", "text": label}]}, **policy)
        return label, result

    @staticmethod
    def step(name, arguments, unlisted=False, **provider_fields):
        return {"name": name, "arguments": arguments, "unlisted": unlisted,
                "provider_fields": provider_fields}

    def assert_failed_without_permission(self, client, label, name):
        self.assertEqual(client.callbacks, [], "Invalid input reached a client authority callback")
        self.assertTrue(any(call["name"] == name and call["call_id"].startswith(label + "-")
                            for call in self.server.emitted), "The fixture did not exercise the offered tool")
        terminals = [frame["params"]["update"] for frame in client.updates
                     if frame.get("method") == "session/update"
                     and frame["params"]["update"].get("sessionUpdate") == "tool_call_update"
                     and frame["params"]["update"].get("toolCallId", "").startswith(label + "-")]
        self.assertTrue(any(item.get("status") == "failed" for item in terminals), terminals)

    def assert_bound_identity(self, client, expected):
        self.assertEqual(len(client.callbacks), 1)
        permission = client.callbacks[0]["params"]
        self.assertEqual(permission["sessionId"], client.session)
        call = permission["toolCall"]
        self.assertEqual(call["_meta"][META], expected)
        announcements = [frame["params"] for frame in client.updates
                         if frame.get("method") == "session/update"
                         and frame["params"]["update"].get("sessionUpdate") == "tool_call"
                         and frame["params"]["update"].get("toolCallId") == call["toolCallId"]]
        self.assertEqual(len(announcements), 1)
        self.assertEqual(announcements[0]["sessionId"], client.session)
        announced = dict(announcements[0]["update"])
        announced.pop("sessionUpdate")
        # The request's kind has always been "other"; clients match on it.
        self.assertEqual(call["kind"], "other")
        self.assertIn(announced["kind"], ("edit", "execute"))
        self.assertEqual({**call, "kind": announced["kind"]}, announced,
                         "Permission must describe the same announced call")

    def test_identity_capability_is_explicit(self):
        client = self.client()
        self.assertEqual(client.initialized["agentCapabilities"]["_meta"][META], {"version": 1})

    def test_legacy_client_cannot_approve_injected_builtin_write(self):
        for extra in [{"server": "minecraft", "tool": "observe"},
                      {"_meta": {META: {"version": 1, "kind": "mcp", "name": "observe", "server": "minecraft"}}},
                      {"unexpected": True}]:
            with self.subTest(extra=extra):
                client = self.client()
                path = f"injected-{len(self.server.programs)}.txt"
                args = {"path": path, "content": "must not be written", **extra}
                label, _ = self.prompt(client, [self.step("workspace_write", args)], legacy=True)
                self.assertFalse((self.work / path).exists(), "Injected identity granted a real file effect")
                self.assert_failed_without_permission(client, label, "workspace_write")

    def test_injected_run_cannot_reach_legacy_permission_or_command_effect(self):
        client = self.client()
        command, args = (("cmd.exe", ["/d", "/c", "echo forbidden>command-proof.txt"]) if os.name == "nt"
                         else ("/bin/sh", ["-c", "printf forbidden > command-proof.txt"]))
        label, _ = self.prompt(client, [self.step("workspace_run", {
            "command": command, "args": args, "server": "minecraft", "tool": "observe"})], legacy=True)
        if not any(t.get("name") == "workspace_run" for t in self.server.requests[0]["tools"]):
            self.skipTest("This platform has no advertised command-isolation mechanism")
        self.assertFalse((self.work / "command-proof.txt").exists())
        self.assert_failed_without_permission(client, label, "workspace_run")

    def test_save_artifact_rejects_model_extra_keys_before_permission(self):
        client = self.client()
        label, _ = self.prompt(client, [self.step("workspace_save_artifact", {
            "artifact": "latest", "path": "artifact.bin", "server": "minecraft", "tool": "observe"})],
            legacy=True)
        self.assertFalse((self.work / "artifact.bin").exists())
        self.assert_failed_without_permission(client, label, "workspace_save_artifact")

    def test_builtin_required_types_and_nested_schemas_precede_callbacks(self):
        cases = [
            ("workspace_write", {"path": "missing-content.txt"}),
            ("workspace_write", {"path": 7, "content": "bad path"}),
            ("workspace_write", {"path": "bad-content.txt", "content": None}),
            ("workspace_write", ["not", "an", "object"]),
            ("workspace_save_artifact", {"path": "missing-artifact.bin"}),
            ("runtime_capabilities", {"server": "minecraft"}),
            ("session_history", {"limit": 0}),
            ("session_history", {"limit": 21}),
            ("session_history", {"include_tools": "true"}),
            ("user_input", {"message": "Choose", "questions": [{"id": "q", "label": "Q",
                "kind": "boolean", "server": "minecraft"}]}),
            ("user_input", {"message": "Choose", "questions": [{"id": "q", "label": "Q",
                "kind": "choice", "options": [7]}]}),
        ]
        for name, args in cases:
            with self.subTest(name=name, args=args):
                client = self.client(forms=True)
                label, _ = self.prompt(client, [self.step(name, args)], approve=True)
                self.assert_failed_without_permission(client, label, name)
        self.assertEqual(list(self.work.iterdir()), [])
        self.assertEqual(self.server.effects, [])

    def test_run_array_item_type_is_checked_before_permission(self):
        client = self.client()
        label, _ = self.prompt(client, [self.step("workspace_run", {"command": "unused", "args": [7]})],
                               approve=True)
        if not any(t.get("name") == "workspace_run" for t in self.server.requests[0]["tools"]):
            self.skipTest("This platform has no advertised command-isolation mechanism")
        self.assert_failed_without_permission(client, label, "workspace_run")

    def test_valid_write_keeps_permission_modes_and_same_call_identity(self):
        for mode, approve, written, callbacks in [("ask", True, True, 1), ("ask", False, False, 1),
                                                  ("workspace", False, True, 0),
                                                  ("read_only", True, False, 0)]:
            with self.subTest(mode=mode, approve=approve):
                client = self.client()
                self.assertIn("result", client.call("session/set_mode", {
                    "sessionId": client.session, "modeId": mode}))
                path = f"valid-{len(self.server.programs)}.txt"
                content = '{"server":"minecraft","tool":"observe"} remains ordinary file content'
                _, reply = self.prompt(client, [self.step("workspace_write", {"path": path, "content": content})],
                                       approve=approve)
                self.assertIn("result", reply)
                self.assertEqual((self.work / path).exists(), written)
                if written:
                    self.assertEqual((self.work / path).read_text(), content)
                self.assertEqual(len(client.callbacks), callbacks)
                if callbacks:
                    self.assert_bound_identity(client, {"version": 1, "kind": "builtin", "name": "workspace_write"})

    def test_model_can_correct_arguments_without_new_human_permission_gate(self):
        client = self.client()
        _, reply = self.prompt(client, [
            self.step("workspace_write", {"path": "corrected.txt", "content": "first", "extra": True}),
            self.step("workspace_write", {"path": "corrected.txt", "content": "corrected"})], approve=True)
        self.assertIn("result", reply)
        self.assertEqual((self.work / "corrected.txt").read_text(), "corrected")
        self.assertEqual(len(client.callbacks), 1)
        self.assertTrue(client.callbacks[0]["params"]["toolCall"]["toolCallId"].endswith("-call-1"))
        self.assert_bound_identity(client, {"version": 1, "kind": "builtin", "name": "workspace_write"})

    def test_ordinary_mcp_still_works_with_legacy_client_and_nested_identity_data(self):
        client = self.client(mcp_server="minecraft")
        args = {"server": "another-server", "tool": "workspace_run",
                "_meta": {META: {"version": 99, "kind": "builtin", "name": "workspace_run"}},
                "arguments": {"nested": [1, True, None]}}
        _, reply = self.prompt(client, [self.step("@mcp", args)], legacy=True)
        self.assertIn("result", reply)
        self.assertEqual(self.server.effects, [{"name": "observe", "arguments": args}])
        self.assert_bound_identity(client, {"version": 1, "kind": "mcp", "name": "observe", "server": "minecraft"})
        self.assertEqual(client.callbacks[0]["params"]["toolCall"]["rawInput"],
                         {"server": "minecraft", "tool": "observe", "arguments": args})

    def test_interrupted_write_remains_unknown_and_distinct_read_uses_same_attachment(self):
        self.server.recorded_values = []
        self.server.mcp_traffic = []
        client = self.client(mcp_server="fixture")
        args = {"value": "first entry"}
        self.server.lose_response_for = {"name": "record", "arguments": args}
        label, result = self.prompt(client, [self.step("mcp_0_record", args),
                                             self.step("mcp_0_observe", {}),
                                             self.step("mcp_0_record", args),
                                             self.step("mcp_0_record", {"value": "second entry"})], approve=True)
        self.assertEqual(result["result"]["stopReason"], "end_turn")
        self.assertEqual(self.server.effects,
                         [{"name": "record", "arguments": args}, {"name": "observe", "arguments": {}}])
        self.assertEqual(self.server.recorded_values, ["first entry"])
        traffic = self.server.mcp_traffic
        self.assertEqual([sid for method, sid in traffic if method == "initialize"], [None])
        self.assertEqual([sid for method, sid in traffic if method == "tools/list"], ["permission-fixture"])
        self.assertEqual([sid for method, sid in traffic if method == "tools/call"], ["permission-fixture"] * 2)

        write_id, read_id, repeat_id, changed_id = [f"{label}-call-{i}" for i in range(4)]
        permissions = [p["params"] for p in client.callbacks]
        self.assertTrue(all(p["sessionId"] == client.session for p in permissions))
        self.assertEqual([p["toolCall"]["toolCallId"] for p in permissions],
                         [write_id, read_id, repeat_id, changed_id])

        def terminal_receipts(connection):
            updates = [f["params"]["update"] for f in connection.updates
                       if f.get("method") == "session/update" and f["params"]["sessionId"] == client.session]
            return [(u["toolCallId"], u["status"], json.loads(u["content"][0]["content"]["text"]))
                    for u in updates if u.get("sessionUpdate") == "tool_call_update"
                    and u.get("status") in ("failed", "completed")]

        receipts = terminal_receipts(client)
        self.assertEqual([(call_id, status) for call_id, status, _ in receipts],
                         [(write_id, "failed"), (read_id, "completed"), (repeat_id, "failed"),
                          (changed_id, "failed")])
        self.assertIn("unknown", receipts[0][2]["error"].lower())
        self.assertEqual(json.loads(receipts[1][2]["content"][0]["text"]), ["first entry"])
        self.assertTrue(receipts[2][2].get("error"))
        self.assertTrue(receipts[3][2].get("error"))
        with closing(sqlite3.connect(self.home / "history.sqlite3")) as db:
            reports = [json.loads(row[0]) for row in db.execute(
                "SELECT data FROM events WHERE session=? AND kind='tool.cancellation'", (client.session,))]
        self.assertEqual([(r["request_id"], r["transport"], r["outcome_uncertain"]) for r in reports],
                         [(self.server.interrupted_request_id, "http", True)])

        client.close()
        self.clients.remove(client)
        resumed = PermissionClient(self.home)
        self.clients.append(resumed)
        self.assertIn("result", resumed.call("initialize", {"protocolVersion": 1, "clientCapabilities": {}}))
        loaded = resumed.call("session/load", {"cwd": str(self.work), "sessionId": client.session,
                              "mcpServers": [{"type": "http", "name": "fixture", "url": self.url + "/mcp",
                                              "headers": []}]})
        self.assertIn("result", loaded)
        self.assertEqual(terminal_receipts(resumed), receipts)
        self.assertEqual(resumed.callbacks, [])
        self.assertEqual(len(self.server.effects), 2, "Loading history replayed an MCP call")

    def test_interrupted_read_cannot_be_replayed_but_another_read_can_proceed(self):
        self.server.recorded_values = []
        client = self.client(mcp_server="fixture")
        args = {"page": 1, "limit": 3}
        self.server.lose_response_for = {"name": "observe", "arguments": args}
        new_args = {"page": 2, "limit": 3}
        _, result = self.prompt(client, [self.step("mcp_0_observe", args),
                                         self.step("mcp_0_observe", {"limit": 3, "page": 1}),
                                         self.step("mcp_0_observe", new_args)], approve=True)
        self.assertEqual(result["result"]["stopReason"], "end_turn")
        self.assertEqual(self.server.effects, [self.server.lose_response_for,
                                               {"name": "observe", "arguments": new_args}])
        self.assertEqual(len(client.callbacks), 3)

        write_args = {"value": "new turn entry"}
        label, result = self.prompt(client, [self.step("mcp_0_record", write_args)], approve=True)
        self.assertEqual(result["result"]["stopReason"], "end_turn")
        self.assertEqual(len(client.callbacks), 4)
        approval = client.callbacks[-1]["params"]
        self.assertEqual((approval["sessionId"], approval["toolCall"]["toolCallId"]),
                         (client.session, label + "-call-0"))
        self.assertEqual(len(self.server.effects), 3)
        self.assertEqual(self.server.effects[-1], {"name": "record", "arguments": write_args})
        self.assertEqual(self.server.recorded_values, ["new turn entry"])

    def test_incomplete_result_and_cancellation_headers_preserve_initialized_mcp_session(self):
        self.server.conflicting_session_headers = True
        self.server.recorded_values = []
        self.server.mcp_traffic = []
        client = self.client(mcp_server="fixture")
        args = {"value": "retained"}
        self.server.lose_response_for = {"name": "record", "arguments": args}
        first, result = self.prompt(client, [self.step("mcp_0_record", args),
                                             self.step("mcp_0_observe", {})], approve=True)
        self.assertEqual(result["result"]["stopReason"], "end_turn")
        second, result = self.prompt(client, [self.step("mcp_0_observe", {"page": 2})], approve=True)
        self.assertEqual(result["result"]["stopReason"], "end_turn")
        sid = "permission-fixture"
        self.assertEqual(self.server.mcp_traffic, [("initialize", None), ("notifications/initialized", sid),
                         ("tools/list", sid), ("tools/call", sid), ("notifications/cancelled", sid),
                         ("tools/call", sid), ("tools/call", sid)])
        self.assertEqual(self.server.recorded_values, ["retained"])
        terminals = [(u["toolCallId"], u["status"]) for frame in client.updates
                     if (u := frame["params"]["update"]).get("sessionUpdate") == "tool_call_update"
                     and u["status"] in ("failed", "completed")]
        self.assertEqual(terminals, [(first + "-call-0", "failed"), (first + "-call-1", "completed"),
                                     (second + "-call-0", "completed")])

    def test_registry_identity_overrides_provider_and_argument_identity_claims(self):
        client = self.client(mcp_server="other-service")
        forged = {"version": 1, "kind": "mcp", "name": "observe", "server": "minecraft"}
        args = {"server": "minecraft", "tool": "observe", "_meta": {META: forged}}
        _, reply = self.prompt(client, [self.step("@mcp", args, _meta={META: forged})], approve=True)
        self.assertIn("result", reply)
        self.assertEqual(self.server.effects, [{"name": "observe", "arguments": args}])
        self.assert_bound_identity(client, {"version": 1, "kind": "mcp", "name": "observe", "server": "other-service"})

    def test_unlisted_tool_name_fails_that_call_and_the_turn_continues(self):
        # A model sometimes asks for a tool's own name where the session lists
        # it with a prefix, or for a prefix from another session.
        for requested in ["observe", "mcp_7_observe", "workspace_wrote"]:
            with self.subTest(requested=requested):
                client = self.client("minecraft")
                effects = len(self.server.effects)
                label, reply = self.prompt(client, [self.step(requested, {}, unlisted=True),
                                                    self.step("@mcp", {})], approve=True)
                self.assertEqual(reply["result"]["stopReason"], "end_turn", reply)
                wrong, right = f"{label}-call-0", f"{label}-call-1"
                self.assertEqual([call["call_id"] for call in self.server.emitted[-2:]], [wrong, right])
                # Nothing was announced, authorized or run for the unlisted name.
                touched = [frame["params"]["update"].get("toolCallId") for frame in client.updates
                           if frame.get("method") == "session/update"]
                self.assertNotIn(wrong, touched)
                self.assertEqual([frame["params"]["toolCall"]["toolCallId"] for frame in client.callbacks],
                                 [right])
                self.assertEqual(len(self.server.effects), effects + 1)
                # The model was told, without its own text repeated, and could correct itself.
                told = [item["output"] for request in self.server.requests for item in request["input"]
                        if item.get("type") == "function_call_output" and item.get("call_id") == wrong]
                self.assertTrue(told and "nothing was run" in told[0], told)
                self.assertNotIn(requested, told[0].replace("mcp_0_observe", ""))
                self.assertEqual("mcp_0_observe" in told[0], requested != "workspace_wrote", told[0])

    def test_cancelled_permission_does_not_accept_a_late_allow_reply(self):
        client = self.client()
        label = "cancel-case"
        self.server.programs[label] = [self.step("workspace_write", {"path": "cancelled.txt", "content": "late"})]
        request = client.request("session/prompt", {"sessionId": client.session,
                                  "prompt": [{"type": "text", "text": label}]})
        while True:
            frame = client.q.get(timeout=15)
            if frame.get("method") == "session/request_permission":
                break
            self.assertIn("method", frame)
            client.updates.append(frame)
        client.callbacks.append(frame)
        self.assert_bound_identity(client, {"version": 1, "kind": "builtin", "name": "workspace_write"})
        client.send({"jsonrpc": "2.0", "method": "session/cancel", "params": {"sessionId": client.session}})
        result = client.finish(request)
        self.assertEqual(result["result"]["stopReason"], "cancelled")
        client.send({"jsonrpc": "2.0", "id": frame["id"],
                     "result": {"outcome": {"outcome": "selected", "optionId": "allow_once"}}})
        self.prompt(client, [])
        self.assertFalse((self.work / "cancelled.txt").exists())
        self.assertEqual(len(client.callbacks), 1)


class ReusableApprovalScopes(unittest.TestCase):
    """Host-owned rules: the Runtime describes scope, never stores a decision."""
    setUp = ToolPermissions.setUp
    tearDown = ToolPermissions.tearDown
    prompt = ToolPermissions.prompt
    step = staticmethod(ToolPermissions.step)
    assert_bound_identity = ToolPermissions.assert_bound_identity
    test_cancelled_permission_does_not_accept_a_late_allow_reply = ToolPermissions.test_cancelled_permission_does_not_accept_a_late_allow_reply

    def client(self, *, negotiation=True, servers=None, cwd=None, additional=None):
        client = PermissionClient(self.home)
        self.clients.append(client)
        params = {"protocolVersion": 1, "clientCapabilities": {}}
        if negotiation is not False:
            params["_meta"] = {POLICY_META: ({"version": 1, "owner": "instance", "instanceId": str(uuid.uuid4())}
                                           if negotiation is True else negotiation)}
        reply = client.call("initialize", params)
        self.assertIn("result", reply, reply)
        client.initialized = reply["result"]
        params = {"cwd": str(cwd or self.work), "mcpServers": servers or []}
        if additional is not None:
            params["additionalDirectories"] = [str(p) for p in additional]
        new = client.call("session/new", params)
        self.assertIn("result", new, new)
        client.session = new["result"]["sessionId"]
        return client

    def descriptor(self, client):
        call = client.callbacks[-1]["params"]["toolCall"]
        descriptor = call["_meta"][POLICY_META]
        self.assertEqual(set(descriptor), {"version", "producer", "key", "description", "allowOptionId", "authorityLifetime"})
        self.assertEqual(descriptor["version"], 1)
        self.assertEqual(descriptor["producer"], POLICY_PRODUCER)
        self.assertEqual(descriptor["allowOptionId"], "allow_once")
        self.assertEqual(descriptor["authorityLifetime"], "request")
        self.assertRegex(descriptor["key"], "^[0-9a-f]{64}$")
        self.assertTrue(1 <= len(descriptor["description"]) <= 256)
        announcements = [frame["params"]["update"] for frame in client.updates
                         if frame.get("method") == "session/update"
                         and frame["params"]["update"].get("sessionUpdate") == "tool_call"
                         and frame["params"]["update"].get("toolCallId") == call["toolCallId"]]
        self.assertEqual(len(announcements), 1)
        self.assertEqual(announcements[0]["_meta"][POLICY_META], descriptor)
        return descriptor

    def test_exact_scope_survives_new_chat_process_and_instance_ids_without_caching_authority(self):
        args = {"path": "proof.txt", "content": "exact private body"}
        first = self.client()
        self.assertEqual(first.initialized["_meta"][POLICY_META], {
            "version": 1, "owner": "instance", "producer": POLICY_PRODUCER, "reusableRequests": True})
        self.prompt(first, [self.step("workspace_write", args)], approve=True)
        one = self.descriptor(first)
        self.assertTrue(one["description"].startswith("Write: proof.txt."))
        self.assertEqual((self.work / "proof.txt").read_text(), args["content"])
        (self.work / "proof.txt").unlink()
        second = self.client()
        self.prompt(second, [self.step("workspace_write", {"content": args["content"], "path": str(self.work / "proof.txt")})])
        self.assertEqual(self.descriptor(second), one)
        self.assertNotEqual(first.session, second.session)
        self.assertNotEqual(first.callbacks[0]["params"]["toolCall"]["toolCallId"], second.callbacks[0]["params"]["toolCall"]["toolCallId"])
        self.assertFalse((self.work / "proof.txt").exists(), "A previous allow must not become Runtime authority")
        for private in [args["content"], str(self.work), first.session]:
            self.assertNotIn(private, json.dumps(one))

    def test_changed_content_path_working_folder_grants_and_limits_do_not_match(self):
        args = {"path": "proof.txt", "content": "body"}
        first = self.client()
        self.prompt(first, [self.step("workspace_write", args)])
        key = self.descriptor(first)["key"]
        for changed in [{**args, "content": "different"}, {**args, "path": "other.txt"}]:
            self.prompt(first, [self.step("workspace_write", changed)])
            self.assertNotEqual(self.descriptor(first)["key"], key)
        child = self.work / "child"
        child.mkdir()
        for client in [self.client(cwd=child), self.client(additional=[child])]:
            self.prompt(client, [self.step("workspace_write", args)])
            self.assertNotEqual(self.descriptor(client)["key"], key)
        path = self.home / "runtime.json"
        config = json.loads(path.read_text())
        config["limits"]["max_tool_calls"] += 1
        path.write_text(json.dumps(config))
        changed = self.client()
        self.prompt(changed, [self.step("workspace_write", args)])
        self.assertNotEqual(self.descriptor(changed)["key"], key)

    def test_legacy_and_unsupported_negotiation_have_original_wire_and_authority(self):
        for negotiation in [False, {}, {"version": 2, "owner": "instance", "instanceId": str(uuid.uuid4())},
                            {"version": 1, "owner": "engine", "instanceId": str(uuid.uuid4())},
                            {"version": 1, "owner": "instance", "instanceId": "uninstalled"}]:
            with self.subTest(negotiation=negotiation):
                client = self.client(negotiation=negotiation)
                self.assertNotIn(POLICY_META, client.initialized.get("_meta", {}))
                self.prompt(client, [self.step("workspace_write", {"path": "once.txt", "content": "once"})], approve=True)
                call = client.callbacks[0]["params"]["toolCall"]
                self.assertNotIn(POLICY_META, call["_meta"])
                self.assertEqual((self.work / "once.txt").read_text(), "once")

    def test_mcp_key_binds_actual_endpoint_and_credentials_but_not_server_label(self):
        def server(label="same", endpoint=None, secret="first"):
            return {"name": label, "type": "http", "url": endpoint or self.url + "/mcp",
                    "headers": [{"name": "Authorization", "value": "Bearer " + secret}]}
        keys = []
        for spec in [server(), server(label="renamed"), server(secret="second")]:
            client = self.client(servers=[spec])
            self.prompt(client, [self.step("@mcp", {"private": "data"})], approve=True)
            descriptor = self.descriptor(client)
            self.assertTrue(descriptor["description"].startswith("MCP tool: observe."))
            keys.append(descriptor["key"])
            for private in ["Bearer", "first", "second", self.url, '"private"']:
                self.assertNotIn(private, json.dumps(descriptor))
        self.assertEqual(keys[0], keys[1])
        self.assertNotEqual(keys[0], keys[2])
        self.server.mcp_schema = {"type": "object", "properties": {"private": {"type": "string"}}}
        client = self.client(servers=[server()])
        self.prompt(client, [self.step("@mcp", {"private": "data"})], approve=True)
        self.assertNotEqual(self.descriptor(client)["key"], keys[0])
        self.server.mcp_schema = {"type": "object"}
        self.server.mcp_protocol = "2025-03-26"
        client = self.client(servers=[server()])
        self.prompt(client, [self.step("@mcp", {"private": "data"})], approve=True)
        self.assertNotEqual(self.descriptor(client)["key"], keys[0])
        other = ThreadingHTTPServer(("127.0.0.1", 0), Handler)
        other.effects = []
        threading.Thread(target=other.serve_forever, daemon=True).start()
        try:
            client = self.client(servers=[server(endpoint=f"http://127.0.0.1:{other.server_port}/mcp")])
            self.prompt(client, [self.step("@mcp", {"private": "data"})], approve=True)
            self.assertNotEqual(self.descriptor(client)["key"], keys[0])
            self.assertEqual(len(other.effects), 1)
        finally:
            other.shutdown()
            other.server_close()
        self.assertEqual(len(self.server.effects), 5)

    def test_host_only_always_option_is_not_new_engine_authority(self):
        client = self.client()
        label = "unoffered-option"
        self.server.programs[label] = [self.step("workspace_write", {"path": "never.txt", "content": "never"})]
        request = client.request("session/prompt", {"sessionId": client.session,
                                  "prompt": [{"type": "text", "text": label}]})
        while True:
            frame = client.q.get(timeout=15)
            if frame.get("method") == "session/request_permission":
                break
            self.assertIn("method", frame)
            client.updates.append(frame)
        client.callbacks.append(frame)
        self.descriptor(client)
        self.assertEqual([option["optionId"] for option in frame["params"]["options"]], ["allow_once", "reject_once"])
        client.send({"jsonrpc": "2.0", "id": frame["id"], "result": {
            "outcome": {"outcome": "selected", "optionId": "allow_always"}}})
        self.assertIn("result", client.finish(request))
        self.assertFalse((self.work / "never.txt").exists())

    def test_negotiation_does_not_bypass_builtin_schema_or_read_only(self):
        client = self.client()
        self.prompt(client, [self.step("workspace_write", {"path": "no.txt", "content": "no",
                    "_meta": {POLICY_META: {"key": "0" * 64}}})], approve=True)
        self.assertFalse(client.callbacks)
        self.assertFalse((self.work / "no.txt").exists())
        self.assertIn("result", client.call("session/set_mode", {"sessionId": client.session, "modeId": "read_only"}))
        self.prompt(client, [self.step("workspace_write", {"path": "no.txt", "content": "no"})], approve=True)
        self.assertFalse(client.callbacks)
        self.assertFalse((self.work / "no.txt").exists())

    def test_negotiation_and_auto_allow_replies_do_not_expand_tool_budget(self):
        path = self.home / "runtime.json"
        config = json.loads(path.read_text())
        config["limits"]["max_tool_calls"] = 1
        path.write_text(json.dumps(config))
        client = self.client()
        _, result = self.prompt(client, [
            self.step("workspace_write", {"path": "first.txt", "content": "first"}),
            self.step("workspace_write", {"path": "second.txt", "content": "second"})], approve=True)
        self.assertEqual(result["result"]["_meta"]["io.github.bkrabach.agent-runtime"]["stopCause"], "tool_call_budget")
        self.assertEqual(len(client.callbacks), 1)
        self.descriptor(client)
        self.assertEqual((self.work / "first.txt").read_text(), "first")
        self.assertFalse((self.work / "second.txt").exists())


if __name__ == "__main__":
    unittest.main()
