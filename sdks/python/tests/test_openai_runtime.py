import pytest
from uuid import UUID

import merry


def test_openai_compatible_constructor_validates_without_network():
    runtime = merry.Runtime.with_openai_compatible(
        api_key="sk-test",
        model="gpt-test",
        base_url="https://api.example.test/v1",
    )

    assert isinstance(runtime, merry.Runtime)


def test_openai_compatible_constructor_uses_random_session_id_by_default():
    first = merry.Runtime.with_openai_compatible(
        api_key="sk-test",
        model="gpt-test",
        base_url="https://api.example.test/v1",
    )
    second = merry.Runtime.with_openai_compatible(
        api_key="sk-test",
        model="gpt-test",
        base_url="https://api.example.test/v1",
    )

    assert first.session_id != second.session_id
    assert UUID(first.session_id).version == 4
    assert UUID(second.session_id).version == 4


def test_openai_compatible_constructor_accepts_explicit_session_id():
    runtime = merry.Runtime.with_openai_compatible(
        api_key="sk-test",
        model="gpt-test",
        base_url="https://api.example.test/v1",
        session_id="tenant-openai.debug_1",
    )

    assert runtime.session_id == "tenant-openai.debug_1"


def test_openai_compatible_constructor_maps_invalid_base_url():
    with pytest.raises(merry.MerryConfigError) as raised:
        merry.Runtime.with_openai_compatible(
            api_key="sk-test",
            model="gpt-test",
            base_url="api.example.test/v1",
        )

    assert raised.value.code == "config.openai_invalid"
    assert raised.value.domain == "config"


def test_from_env_uses_openai_compatible_config(monkeypatch):
    monkeypatch.setenv("MERRY_OPENAI_API_KEY", "sk-test")
    monkeypatch.setenv("MERRY_OPENAI_MODEL", "gpt-test")
    monkeypatch.setenv("MERRY_OPENAI_BASE_URL", "https://api.example.test/v1")

    runtime = merry.Runtime.from_env()

    assert isinstance(runtime, merry.Runtime)


def test_from_env_accepts_explicit_session_id(monkeypatch):
    monkeypatch.setenv("MERRY_OPENAI_API_KEY", "sk-test")
    monkeypatch.setenv("MERRY_OPENAI_MODEL", "gpt-test")
    monkeypatch.setenv("MERRY_OPENAI_BASE_URL", "https://api.example.test/v1")

    runtime = merry.Runtime.from_env(session_id="env-session.debug_1")

    assert runtime.session_id == "env-session.debug_1"


def test_from_env_requires_api_key(monkeypatch):
    monkeypatch.delenv("MERRY_OPENAI_API_KEY", raising=False)
    monkeypatch.delenv("OPENAI_API_KEY", raising=False)
    monkeypatch.setenv("MERRY_OPENAI_MODEL", "gpt-test")

    with pytest.raises(merry.MerryConfigError) as raised:
        merry.Runtime.from_env()

    assert raised.value.code == "config.openai_api_key_missing"
    assert raised.value.domain == "config"


def test_from_env_requires_model(monkeypatch):
    monkeypatch.setenv("MERRY_OPENAI_API_KEY", "sk-test")
    monkeypatch.delenv("MERRY_OPENAI_MODEL", raising=False)
    monkeypatch.delenv("OPENAI_MODEL", raising=False)

    with pytest.raises(merry.MerryConfigError) as raised:
        merry.Runtime.from_env()

    assert raised.value.code == "config.openai_model_missing"
    assert raised.value.domain == "config"
