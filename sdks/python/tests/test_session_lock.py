from __future__ import annotations

import asyncio
import subprocess
import sys
from pathlib import Path
from typing import cast

import pytest

import merry
from tests._support import pending_native_agent


def _provider() -> merry.OpenAICompatible:
    return merry.OpenAICompatible(
        api_key="sk-test",
        model="gpt-test",
        base_url="https://api.example.test/v1",
    )


def test_session_lock_enforces_same_process_exclusivity_and_unified_async_with(
    tmp_path: Path,
) -> None:
    async def scenario() -> None:
        store_root = tmp_path / "sessions"
        provider = _provider()

        async with merry.Agent(
            session_id="lock-unified",
            provider=provider,
            session_store=store_root,
        ) as agent:
            await agent.save_session()

            with pytest.raises(merry.MerrySessionLockedError) as build_err:
                merry.Agent(
                    session_id="lock-unified",
                    provider=provider,
                    session_store=store_root,
                )
            assert isinstance(build_err.value, merry.MerryRuntimeError)
            assert build_err.value.code == "session_already_reserved"
            assert build_err.value.domain == "runtime"
            assert build_err.value.retryability == "retryable"
            assert build_err.value.info.context == {"session_id": "lock-unified"}

            with pytest.raises(merry.MerrySessionLockedError) as resume_err:
                async with merry.Agent.resume(
                    "lock-unified",
                    provider=provider,
                    session_store=store_root,
                ):
                    pass
            assert resume_err.value.code == "session_already_reserved"
            assert resume_err.value.info.context == {"session_id": "lock-unified"}

        assert agent.closed

        # Unified `async with Agent.resume(...)` works cleanly once the first handle closes.
        async with merry.Agent.resume(
            "lock-unified",
            provider=provider,
            session_store=store_root,
        ) as resumed:
            assert resumed.session_id == "lock-unified"
            assert not resumed.closed
        assert resumed.closed

        # Backwards-compatible `await Agent.resume(...)` + explicit `await agent.close()` also works.
        manual = await merry.Agent.resume(
            "lock-unified",
            provider=provider,
            session_store=store_root,
        )
        try:
            assert manual.session_id == "lock-unified"
            assert not manual.closed
        finally:
            await manual.close()
        assert manual.closed

    asyncio.run(scenario())


def test_session_lock_prevents_cross_process_concurrent_writes(
    tmp_path: Path,
) -> None:
    store_root = tmp_path / "sessions"
    holder_script = (
        "import asyncio, sys\n"
        "from pathlib import Path\n"
        "import merry\n"
        "async def main() -> None:\n"
        "    provider = merry.OpenAICompatible(\n"
        "        api_key='sk-test', model='gpt-test', base_url='https://api.example.test/v1'\n"
        "    )\n"
        "    async with merry.Agent(\n"
        "        session_id='cross-proc', provider=provider, session_store=Path(sys.argv[1])\n"
        "    ) as agent:\n"
        "        await agent.save_session()\n"
        "        print('LOCKED', flush=True)\n"
        "        sys.stdin.readline()\n"
        "asyncio.run(main())\n"
    )
    proc = subprocess.Popen(
        [sys.executable, "-c", holder_script, str(store_root)],
        stdin=subprocess.PIPE,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        text=True,
    )
    try:
        assert proc.stdout is not None
        assert proc.stdin is not None
        ready = proc.stdout.readline().strip()
        assert ready == "LOCKED", proc.stderr.read() if proc.stderr else ""

        async def check_conflict() -> None:
            provider = _provider()
            with pytest.raises(merry.MerrySessionLockedError) as build_err:
                merry.Agent(
                    session_id="cross-proc",
                    provider=provider,
                    session_store=store_root,
                )
            assert build_err.value.code == "session_already_reserved"

            with pytest.raises(merry.MerrySessionLockedError) as resume_err:
                await merry.Agent.resume(
                    "cross-proc",
                    provider=provider,
                    session_store=store_root,
                )
            assert resume_err.value.code == "session_already_reserved"

        asyncio.run(check_conflict())

        proc.stdin.write("release\n")
        proc.stdin.flush()
        assert proc.wait(timeout=10) == 0

        async def check_resume_after_exit() -> str:
            async with merry.Agent.resume(
                "cross-proc",
                provider=_provider(),
                session_store=store_root,
            ) as resumed:
                return resumed.session_id

        assert asyncio.run(check_resume_after_exit()) == "cross-proc"
    finally:
        if proc.poll() is None:
            proc.kill()
            proc.wait(timeout=5)


def test_agent_close_is_idempotent_cleans_empty_dir_and_blocks_reuse(
    tmp_path: Path,
) -> None:
    async def scenario() -> None:
        store_root = tmp_path / "sessions"
        provider = _provider()

        agent = merry.Agent(
            session_id="untouched-close",
            provider=provider,
            session_store=store_root,
        )
        session_dir = store_root / "untouched-close"
        assert session_dir.exists()

        await agent.close()
        assert agent.closed
        assert not session_dir.exists()
        # Repeated close is idempotent.
        await agent.close()

        with pytest.raises(merry.MerryRuntimeError) as save_err:
            await agent.save_session()
        assert save_err.value.code == "session_closed"
        assert save_err.value.info.context == {"session_id": "untouched-close"}

        with pytest.raises(merry.MerryRuntimeError) as stream_err:
            agent.stream("after close")
        assert stream_err.value.code == "session_closed"

        with pytest.raises(merry.MerryRuntimeError) as enter_err:
            async with agent:
                pass
        assert enter_err.value.code == "session_closed"

    asyncio.run(scenario())


def test_session_lock_opt_out_validation_and_pending_agent_errors(
    tmp_path: Path,
) -> None:
    async def scenario() -> None:
        store_root = tmp_path / "sessions"
        provider = _provider()

        with pytest.raises(TypeError, match="session_lock must be a boolean"):
            merry.AgentBuilder("bad-lock").session_lock(cast(bool, 1))

        async with merry.Agent(
            session_id="opt-out",
            provider=provider,
            session_store=store_root,
        ) as locked:
            await locked.save_session()
            async with merry.Agent(
                session_id="opt-out",
                provider=provider,
                session_store=store_root,
                session_lock=False,
            ) as unlocked:
                assert unlocked.session_id == "opt-out"
            async with merry.Agent.resume(
                "opt-out",
                provider=provider,
                session_store=store_root,
                session_lock=False,
            ) as unlocked_resumed:
                assert unlocked_resumed.session_id == "opt-out"

        # Failed resume of a missing session does not leave behind an empty lock directory.
        with pytest.raises(merry.MerryRuntimeError):
            async with merry.Agent.resume(
                "does-not-exist",
                provider=provider,
                session_store=store_root,
            ):
                pass
        assert not (store_root / "does-not-exist").exists()

        # PendingAgent cannot be awaited twice.
        pending = merry.Agent.resume(
            "opt-out",
            provider=provider,
            session_store=store_root,
        )
        resumed = await pending
        await resumed.close()
        with pytest.raises(merry.MerryRuntimeError) as reused_err:
            await pending
        assert reused_err.value.code == "pending_agent_consumed"

    asyncio.run(scenario())


def test_agent_close_rejects_active_run_until_run_closes() -> None:
    async def scenario() -> None:
        agent = merry.Agent._from_native(pending_native_agent("close-active-run"))
        run = agent.stream("still running")
        with pytest.raises(merry.MerryRuntimeError) as close_err:
            await agent.close()
        assert close_err.value.code == "step_already_active"
        assert not agent.closed

        await asyncio.wait_for(run.close(), timeout=5)
        await agent.close()
        assert agent.closed

    asyncio.run(scenario())
