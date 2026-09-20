from __future__ import annotations

import asyncio
import json
from collections.abc import Callable, Iterator
from contextlib import contextmanager
from http.server import BaseHTTPRequestHandler, HTTPServer
from pathlib import Path
from queue import Queue
from threading import Thread
from typing import Literal

import pytest
from pydantic import BaseModel

import merry


class SchemaProperty(BaseModel):
    maximum: int | None = None


class ToolSchema(BaseModel):
    properties: dict[str, SchemaProperty]


class ToolFunction(BaseModel):
    name: str
    parameters: ToolSchema


class RequestTool(BaseModel):
    function: ToolFunction


class ModelRequest(BaseModel):
    tools: list[RequestTool]


class ReadArguments(BaseModel):
    path: str
    max_lines: int | None = None


class PatchArguments(BaseModel):
    patch: str


class ReadOutput(BaseModel):
    content: str
    lines: int
    truncated: bool


@contextmanager
def capture_model_request(
    tool_response: bytes | None = None,
) -> Iterator[tuple[str, Queue[bytes]]]:
    final_response = (
        b'data: {"choices":[{"index":0,"delta":{"content":"done"},'
        b'"finish_reason":"stop"}]}\n\n'
        b"data: [DONE]\n\n"
    )
    responses: Queue[bytes] = Queue(maxsize=2)
    if tool_response is not None:
        responses.put_nowait(tool_response)
    responses.put_nowait(final_response)
    requests: Queue[bytes] = Queue(maxsize=responses.qsize())

    class Handler(BaseHTTPRequestHandler):
        timeout = 5

        def do_POST(self) -> None:
            content_length = int(self.headers["Content-Length"])
            requests.put_nowait(self.rfile.read(content_length))
            response = responses.get_nowait()
            self.send_response(200)
            self.send_header("Content-Type", "text/event-stream")
            self.send_header("Content-Length", str(len(response)))
            self.end_headers()
            self.wfile.write(response)

        def log_message(self, format: str, *args: str | int) -> None:
            pass

    with HTTPServer(("127.0.0.1", 0), Handler) as server:
        worker = Thread(
            target=server.serve_forever,
            kwargs={"poll_interval": 0.05},
            name="workspace-limits-provider",
        )
        worker.start()
        try:
            yield f"http://127.0.0.1:{server.server_port}/v1", requests
        finally:
            server.shutdown()
            worker.join(timeout=10)
            if worker.is_alive():
                raise RuntimeError("workspace limit fixture did not stop")


@pytest.mark.parametrize(
    ("limits", "expected_max_lines"),
    [
        (merry.WorkspaceLimits(), 2_000),
        (merry.WorkspaceLimits(max_read_lines=17), 17),
        (merry.WorkspaceLimits(max_read_bytes=2048), 2_000),
    ],
)
def test_workspace_limits_reach_rust_tool_schema(
    tmp_path: Path,
    limits: merry.WorkspaceLimits,
    expected_max_lines: int,
) -> None:
    with capture_model_request() as (base_url, requests):
        agent = (
            merry.AgentBuilder("workspace-limit-contract")
            .provider(
                merry.OpenAICompatible(
                    api_key="test-key",
                    model="test-model",
                    base_url=base_url,
                    protocol="chat_completions",
                )
            )
            .workspace(merry.WorkspaceConfig(root=tmp_path, limits=limits))
            .build()
        )

        async def run() -> None:
            await asyncio.wait_for(agent.run("Reply with done."), timeout=5)

        asyncio.run(run())
        request = ModelRequest.model_validate_json(requests.get(timeout=1))

    read_tool = next(
        tool.function for tool in request.tools if tool.function.name == "read_text"
    )
    assert read_tool.parameters.properties["max_lines"].maximum == expected_max_lines


def run_workspace_tool(
    root: Path,
    limits: merry.WorkspaceLimits,
    name: Literal["read_text", "apply_patch"],
    arguments: ReadArguments | PatchArguments,
) -> merry.ToolCallFinishedPayload:
    chunk = json.dumps(
        {
            "choices": [
                {
                    "index": 0,
                    "delta": {
                        "tool_calls": [
                            {
                                "index": 0,
                                "id": "workspace-tool-call",
                                "type": "function",
                                "function": {
                                    "name": name,
                                    "arguments": arguments.model_dump_json(
                                        exclude_none=True
                                    ),
                                },
                            }
                        ]
                    },
                    "finish_reason": "tool_calls",
                }
            ]
        }
    )
    response = f"data: {chunk}\n\ndata: [DONE]\n\n".encode()
    with capture_model_request(response) as (base_url, requests):
        agent = (
            merry.AgentBuilder("workspace-tool-limits")
            .provider(
                merry.OpenAICompatible(
                    api_key="test-key",
                    model="test-model",
                    base_url=base_url,
                    protocol="chat_completions",
                )
            )
            .workspace(
                merry.WorkspaceConfig(
                    root=root,
                    limits=limits,
                    patch=merry.PatchConfig(write_scope=["note.txt"]),
                )
            )
            .build()
        )

        async def run() -> tuple[merry.Event, ...]:
            result = await asyncio.wait_for(agent.run("Execute the tool."), timeout=5)
            assert result.status is merry.RunStatus.COMPLETED
            return result.events

        events = asyncio.run(run())
        assert requests.qsize() == 2

    finished = [
        event.payload
        for event in events
        if isinstance(event.payload, merry.ToolCallFinishedPayload)
    ]
    assert len(finished) == 1
    return finished[0]


@pytest.mark.parametrize(
    ("limits", "expected_error"),
    [
        (merry.WorkspaceLimits(), None),
        (merry.WorkspaceLimits(max_read_bytes=3), "workspace_file_too_large"),
        (merry.WorkspaceLimits(max_read_bytes=4), None),
        (merry.WorkspaceLimits(max_write_bytes=1, max_patch_bytes=1), None),
    ],
)
def test_read_byte_limits_are_enforced_without_changing_other_limits(
    tmp_path: Path, limits: merry.WorkspaceLimits, expected_error: str | None
) -> None:
    path = tmp_path / "note.txt"
    path.write_text("界\n", encoding="utf-8")
    finished = run_workspace_tool(
        tmp_path, limits, "read_text", ReadArguments(path="note.txt")
    )

    assert_tool_status(finished, expected_error)
    if expected_error is None:
        assert finished.output is not None
        output = ReadOutput.model_validate_json(finished.output.value)
        assert output.content == "界\n"
        assert output.lines == 1
        assert not output.truncated
    assert path.read_text(encoding="utf-8") == "界\n"


@pytest.mark.parametrize("requested_lines", [None, 2, 3])
def test_read_line_override_controls_default_window_and_explicit_limit(
    tmp_path: Path, requested_lines: int | None
) -> None:
    (tmp_path / "note.txt").write_text("one\ntwo\nthree\n", encoding="utf-8")
    finished = run_workspace_tool(
        tmp_path,
        merry.WorkspaceLimits(max_read_lines=2),
        "read_text",
        ReadArguments(path="note.txt", max_lines=requested_lines),
    )

    expected_error = "tool_input_schema_invalid" if requested_lines == 3 else None
    assert_tool_status(finished, expected_error)
    if expected_error is None:
        assert finished.output is not None
        output = ReadOutput.model_validate_json(finished.output.value)
        assert output.content == "one\ntwo\n"
        assert output.lines == 2
        assert output.truncated


PATCH = (
    "*** Begin Patch\n*** Update File: note.txt\n@@\n-old\n+changed\n*** End Patch\n"
)


@pytest.mark.parametrize(
    ("limits", "expected_error"),
    [
        (merry.WorkspaceLimits(), None),
        (merry.WorkspaceLimits(max_write_bytes=7), "workspace_file_too_large"),
        (merry.WorkspaceLimits(max_write_bytes=8), None),
        (
            merry.WorkspaceLimits(max_patch_bytes=len(PATCH.encode()) - 1),
            "tool_input_schema_invalid",
        ),
        (merry.WorkspaceLimits(max_patch_bytes=len(PATCH.encode())), None),
        (merry.WorkspaceLimits(max_read_bytes=4, max_read_lines=1), None),
    ],
)
def test_patch_limits_are_enforced_before_writes(
    tmp_path: Path, limits: merry.WorkspaceLimits, expected_error: str | None
) -> None:
    path = tmp_path / "note.txt"
    path.write_text("old\n", encoding="utf-8")
    finished = run_workspace_tool(
        tmp_path, limits, "apply_patch", PatchArguments(patch=PATCH)
    )

    assert_tool_status(finished, expected_error)
    expected_content = "changed\n" if expected_error is None else "old\n"
    assert path.read_text(encoding="utf-8") == expected_content


@pytest.mark.parametrize(
    "make_limits",
    [
        lambda value: merry.WorkspaceLimits(max_read_bytes=value),
        lambda value: merry.WorkspaceLimits(max_read_lines=value),
        lambda value: merry.WorkspaceLimits(max_write_bytes=value),
        lambda value: merry.WorkspaceLimits(max_patch_bytes=value),
    ],
)
@pytest.mark.parametrize("invalid_value", [0, -1])
def test_invalid_limits_are_rejected_instead_of_inheriting_defaults(
    make_limits: Callable[[int], merry.WorkspaceLimits], invalid_value: int
) -> None:
    with pytest.raises(ValueError, match="must be greater than zero"):
        make_limits(invalid_value)


def assert_tool_status(
    finished: merry.ToolCallFinishedPayload, expected_error: str | None
) -> None:
    if expected_error is None:
        assert finished.result.status is merry.RuntimeToolResultStatus.SUCCEEDED
        assert finished.result.diagnostic is None
    else:
        assert finished.result.status is merry.RuntimeToolResultStatus.FAILED
        assert finished.result.diagnostic is not None
        assert finished.result.diagnostic.code == expected_error
