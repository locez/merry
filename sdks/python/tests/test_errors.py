import pytest

import merry
from merry._errors import _decode_native_error


def test_merry_error_exposes_stable_info():
    info = merry.MerryErrorInfo(
        code="config.invalid",
        domain="config",
        message="Config is invalid.",
        hint="Fix the TOML file.",
        retryability="user_action_required",
        context={"config_path": "merry.toml"},
    )
    error = merry.MerryError(info)

    assert str(error) == "Config is invalid."
    assert error.info == info
    assert error.code == "config.invalid"
    assert error.domain == "config"
    assert error.retryability == "user_action_required"


def test_native_invalid_session_error_maps_to_merry_error():
    with pytest.raises(merry.MerryRuntimeError) as raised:
        merry.Runtime(session_id=" ")

    assert raised.value.code == "runtime.invalid_session_id"
    assert raised.value.domain == "runtime"
    assert raised.value.retryability == "user_action_required"


def test_native_invalid_session_error_does_not_leak_rejected_value():
    with pytest.raises(merry.MerryRuntimeError) as raised:
        merry.Runtime(session_id=" secret-token ")

    assert raised.value.info.message == "Invalid Merry runtime session id."
    assert "secret-token" not in raised.value.info.message
    assert "secret-token" not in str(raised.value)
    assert "secret-token" not in (raised.value.info.hint or "")
    assert "secret-token" not in repr(raised.value.info.context)


@pytest.mark.parametrize("session_id", ["bad/session", "bad space"])
def test_native_invalid_filesystem_session_id_maps_without_leaking_rejected_value(session_id):
    with pytest.raises(merry.MerryRuntimeError) as raised:
        merry.Runtime(session_id=session_id)

    assert raised.value.code == "runtime.invalid_session_id"
    assert raised.value.info.message == "Invalid Merry runtime session id."
    assert session_id not in str(raised.value)
    assert session_id not in (raised.value.info.hint or "")
    assert session_id not in repr(raised.value.info.context)


@pytest.mark.parametrize("session_id", [".", ".."])
def test_native_dot_session_ids_are_rejected(session_id):
    with pytest.raises(merry.MerryRuntimeError) as raised:
        merry.Runtime(session_id=session_id)

    assert raised.value.code == "runtime.invalid_session_id"
    assert raised.value.info.message == "Invalid Merry runtime session id."


@pytest.mark.parametrize("domain", ["artifact", "sandbox"])
def test_native_sdk_domains_map_to_public_runtime_error(domain):
    native_error = merry.NativeMerryError(
        (
            "{"
            f'"code":"{domain}.failed",'
            f'"domain":"{domain}",'
            f'"message":"{domain.title()} failed.",'
            '"hint":null,'
            '"retryability":"unknown",'
            '"context":{}'
            "}"
        )
    )

    error = _decode_native_error(native_error)

    assert isinstance(error, merry.MerryRuntimeError)
    assert error.domain == domain
