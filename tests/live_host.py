"""Exercise a running Everything plugin using only isolated synthetic fixtures.

Run with Python 3.10+: python tests/live_host.py
The installed plugin must be 1.4.0; override --expected-version for a baseline.
No Everything settings are modified. Fixtures and JSON results stay in target/.
"""

import argparse
import concurrent.futures
import http.client
import json
from pathlib import Path
import socket
import time
import uuid


def check(condition, message):
    if not condition:
        raise AssertionError(message)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--port", type=int, default=8285)
    parser.add_argument("--expected-version", default="1.4.0")
    parser.add_argument("--output", type=Path)
    args = parser.parse_args()
    marker = "mcp-live-" + uuid.uuid4().hex[:12]
    root = (Path(__file__).resolve().parents[1] / "target" / marker).resolve()
    fixtures = root / "fixtures"
    report = {"expected_version": args.expected_version, "fixture_root": str(fixtures),
              "started_at": time.strftime("%Y-%m-%dT%H:%M:%S%z"), "checks": []}

    def rpc(method, params=None, headers=None, envelope=None):
        body = envelope if envelope is not None else {
            "jsonrpc": "2.0", "id": 1, "method": method, "params": params or {}}
        connection = http.client.HTTPConnection("127.0.0.1", args.port, timeout=15)
        try:
            connection.request("POST", "/mcp", json.dumps(body).encode("utf-8"),
                               {"Content-Type": "application/json", **(headers or {})})
            response = connection.getresponse()
            data = response.read()
            return response.status, dict(response.getheaders()), json.loads(data) if data else None
        finally:
            connection.close()

    def tool(name, arguments):
        status, _, result = rpc("tools/call", {"name": name, "arguments": arguments})
        check(status == 200 and "result" in result, str(result))
        output = result["result"]
        text = output["content"][0]["text"]
        try:
            data = json.loads(text)
        except json.JSONDecodeError:
            data = text
        return output, data

    def success(name, arguments):
        output, data = tool(name, arguments)
        check(not output.get("isError"), str(data))
        return data

    def case(name, action):
        started = time.monotonic()
        try:
            detail = action()
            row = {"name": name, "status": "pass", "detail": detail}
        except Exception as error:
            row = {"name": name, "status": "fail", "detail": str(error)}
        row["elapsed_ms"] = round((time.monotonic() - started) * 1000)
        report["checks"].append(row)
        print(f"{row['status'].upper():4} {name} ({row['elapsed_ms']} ms)", flush=True)

    def handshake():
        _, _, result = rpc("initialize", {"protocolVersion": "2024-11-05",
            "capabilities": {}, "clientInfo": {"name": "isolated-live-test", "version": "1"}})
        report["actual_version"] = result["result"]["serverInfo"]["version"]
        check(report["actual_version"] == args.expected_version,
              f"installed {report['actual_version']}; expected {args.expected_version}")
        return report["actual_version"]

    case("installed version / legacy handshake", handshake)
    if report["checks"][-1]["status"] == "pass":
        (fixtures / "nested").mkdir(parents=True)
        (fixtures / "excluded").mkdir()
        (fixtures / "alpha.txt").write_text("first\nneedle 中文🙂\nlast\nneedle twice\n", encoding="utf-8")
        (fixtures / "nested" / "beta.txt").write_text("needle nested\n", encoding="utf-8")
        (fixtures / "excluded" / "ignored.txt").write_text("needle excluded\n", encoding="utf-8")
        (fixtures / "utf16.txt").write_text("一行\nneedle 宽字符🙂\n", encoding="utf-16")
        (fixtures / "binary.dat").write_bytes(b"\0\x01\x02binary" * 500)
        (fixtures / "fake.exe").write_text("needle extension\n", encoding="utf-8")
        (fixtures / ".env").write_text("SYNTHETIC_TEST_VALUE=not-a-secret\n", encoding="utf-8")
        (fixtures / "id_rsa").write_text("SYNTHETIC_TEST_VALUE\n", encoding="utf-8")
        with (fixtures / "oversized.txt").open("wb") as stream:
            stream.write(b"x" * (8 * 1024 * 1024 + 1))
        folder = {"folder": str(fixtures)}

        def indexed():
            deadline = time.monotonic() + 30
            while time.monotonic() < deadline:
                data = success("count", {**folder, "pattern": "ext:txt"})
                if data["count"] == 5:
                    return "5 synthetic text files indexed"
                time.sleep(0.25)
            raise AssertionError(f"index did not converge: {data}")

        case("wait for host index", indexed)

        def search():
            data = success("search_in_folder", {**folder, "pattern": "ext:txt", "max_results": 2})
            check(data["total"] == 5 and data["count"] == 2 and data["truncated"], str(data))
            rest = success("search_in_folder", {**folder, "pattern": "ext:txt", "offset": 2})
            names = [r["name"] for r in data["results"] + rest["results"]]
            check(len(set(names)) == 5 and names == sorted(names), str(names))
            descending = success("search_in_folder", {**folder, "pattern": "ext:txt", "descending": True})
            check([r["name"] for r in descending["results"]] == list(reversed(names)), "descending order")
            count = success("count", {**folder, "pattern": "ext:txt", "exclude": ["\\excluded\\"]})
            check(count["count"] == 4, str(count))
            return "recursive scope, exclusion, ascending/descending sort and pagination"

        case("search / count / pagination", search)

        def children():
            data = success("list_folder", folder)
            names = {r["name"] for r in data["items"]}
            check(len(names) == 9 and "nested" in names and "beta.txt" not in names, str(names))
        case("list immediate children", children)

        def text_read():
            output, data = tool("read_file", {"path": str(fixtures / "alpha.txt"), "start_line": 2, "max_lines": 2})
            check(not output.get("isError"), str(data))
            check(data["total_lines"] == 4 and data["next_start_line"] == 4, str(data))
            check("needle 中文🙂\nlast" in output["content"][1]["text"], str(output))
            output, data = tool("read_file", {"path": str(fixtures / "utf16.txt")})
            check(not output.get("isError") and "宽字符🙂" in output["content"][1]["text"], str(data))
        case("read Unicode / UTF-16 / line window", text_read)

        def rejections():
            for filename, code in [(".env", "PATH_DENIED"), ("id_rsa", "PATH_DENIED"),
                                   ("binary.dat", "BINARY_CONTENT"), ("fake.exe", "BINARY_CONTENT"),
                                   ("oversized.txt", "TOO_LARGE"), ("nested", "PATH_IS_DIRECTORY")]:
                output, data = tool("read_file", {"path": str(fixtures / filename)})
                check(output.get("isError") and data["code"] == code, f"{filename}: {data}")
        case("read sensitive / binary / oversized / directory rejection", rejections)

        def grep_modes():
            for mode, key, expected in [("content", "matches", 4), ("filesWithMatches", "files", 3),
                                        ("count", "counts", 3)]:
                data = success("grep", {**folder, "pattern": "needle", "output_mode": mode,
                    "filter": "ext:txt", "exclude": ["\\excluded\\"]})
                check(len(data[key]) == expected, str(data))
                if mode == "count":
                    check(sum(r["count"] for r in data[key]) == 4, str(data))
            return "all three output modes and exclusions"
        case("grep modes and exclusions", grep_modes)

        def skipped_files():
            data = success("grep", {**folder, "pattern": "needle"})
            check(data["files_skipped"] >= 5, str(data))
            check(data["bytes_scanned"] >= 4096, "binary sniff must count toward actual bytes read")
            return "sensitive, binary and oversized candidates skipped; sniff reads charged"
        case("grep skipped files and read accounting", skipped_files)

        def global_search():
            data = success("search_everywhere", {"pattern": f'path:"{marker}" ext:txt'})
            check(data.get("total") == 5, str(data))
        case("global search scoped by unique marker", global_search)

        def concurrent_queries():
            def run(i):
                pattern = "alpha.txt" if i % 2 == 0 else "beta.txt"
                data = success("search_in_folder", {**folder, "pattern": pattern})
                check(data["count"] == 1 and data["results"][0]["name"] == pattern, str(data))
            with concurrent.futures.ThreadPoolExecutor(max_workers=8) as executor:
                list(executor.map(run, range(24)))
            for _ in range(4):
                tool("search_in_folder", {**folder, "pattern": "ext:txt", "timeout_ms": 1})
            check(success("count", {**folder, "pattern": "ext:txt"})["count"] == 5, "recovery failed")
            return "24 concurrent requests plus short deadlines and recovery"
        case("concurrent host callbacks / timeout recovery", concurrent_queries)

        def modern():
            status, _, result = rpc("tools/list", {"_meta": {
                "io.modelcontextprotocol/protocolVersion": "2026-07-28"}},
                {"MCP-Protocol-Version": "2026-07-28"})
            check(status == 200 and result["result"]["resultType"] == "complete", str(result))
            check(len(result["result"]["tools"]) == 8, "tool count")
        case("modern protocol and tool discovery", modern)

        def envelopes():
            for body in [{}, {"jsonrpc": "1.0", "id": 1, "method": "ping"},
                         {"jsonrpc": "2.0", "id": {}, "method": "ping"}]:
                _, _, result = rpc("", envelope=body)
                check(result["error"]["code"] == -32600, str(result))
                check("id" in result, "error response omitted id")
            status, _, result = rpc("", envelope={"jsonrpc": "2.0", "id": None, "method": "ping"})
            check(status == 200 and result["id"] is None and "result" in result, str(result))
        case("invalid envelopes / explicit null id", envelopes)

        def cors():
            origin = "http://localhost:3000"
            _, headers, _ = rpc("ping", headers={"Origin": origin})
            headers = {k.lower(): v for k, v in headers.items()}
            check(headers.get("access-control-allow-origin") == origin, str(headers))
            check("Origin" in headers.get("vary", ""), str(headers))
            status, _, _ = rpc("ping", headers={"Origin": "https://invalid.example"})
            check(status == 403, str(status))
        case("CORS and rejected remote Origin", cors)

        def framing():
            for extra in ["Content-Length: 2\r\nContent-Length: 3\r\n",
                          "Transfer-Encoding: chunked\r\nContent-Length: 2\r\n"]:
                with socket.create_connection(("127.0.0.1", args.port), timeout=5) as stream:
                    stream.sendall(("POST / HTTP/1.1\r\nHost: localhost\r\n" + extra + "\r\n{}").encode())
                    stream.shutdown(socket.SHUT_WR)
                    status = stream.recv(4096).split(b"\r\n")[0]
                    check(b"400" in status, repr(status))
        case("conflicting HTTP framing", framing)

        def journal():
            output, data = tool("index_changes", {"path": str(fixtures), "max_results": 50})
            if output.get("isError"):
                check("journal_log" in str(data), str(data))
                return "journal disabled: actionable prerequisite error verified; events not tested"
            return f"journal enabled: {data['count']} fixture events observed"
        case("journal prerequisite / fixture-only query", journal)

    root.mkdir(parents=True, exist_ok=True)
    destination = args.output or root / "report.json"
    destination.parent.mkdir(parents=True, exist_ok=True)
    report["passed"] = sum(row["status"] == "pass" for row in report["checks"])
    report["failed"] = sum(row["status"] == "fail" for row in report["checks"])
    destination.write_text(json.dumps(report, ensure_ascii=False, indent=2), encoding="utf-8")
    print(f"Report: {destination.resolve()}")
    return int(report["failed"] > 0)


if __name__ == "__main__":
    raise SystemExit(main())
