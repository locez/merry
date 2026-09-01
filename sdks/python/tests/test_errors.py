from __future__ import annotations

import pytest

import merry
from merry._errors import _decode_native_error


def test_error_info_preserves_cross_language_diagnostic_contract() -> None:
    info = merry.MerryErrorInfo(
        code="config.invalid",
        domain="config",
        message="Config is invalid.",
        hint="Fix the TOML file.",
        retryability="user_action_required",
        context={"field_path": "provider.model"},
    )
    error = merry.MerryConfigError(info)

    assert str(error) == "Config is invalid."
    assert error.info == info
    assert error.code == "config.invalid"
    assert error.domain == "config"
    assert error.retryability == "user_action_required"


def test_invalid_session_id_is_mapped_at_the_binding_boundary() -> None:
    with pytest.raises(merry.MerryConfigError) as raised:
        merry.AgentBuilder(" ")

    assert raised.value.code == "config.invalid"
    assert raised.value.domain == "config"


def test_native_error_mapping_preserves_hint_retryability_and_context() -> None:
    native_error = merry.NativeMerryError(
        '{"code":"provider.timeout","domain":"provider",'
        '"message":"Provider request timed out.",'
        '"hint":"Retry after checking the endpoint.",'
        '"retryability":"retryable",'
        '"context":{"provider_name":"test"}}'
    )

    error = _decode_native_error(native_error)

    assert isinstance(error, merry.MerryProviderError)
    assert error.info.hint == "Retry after checking the endpoint."
    assert error.info.retryability == "retryable"
    assert error.info.context == {"provider_name": "test"}


def test_malformed_native_error_becomes_internal_error() -> None:
    error = _decode_native_error(merry.NativeMerryError("not-json"))

    assert isinstance(error, merry.MerryInternalError)
    assert error.code == "protocol.native_error_invalid"


def test_tool_domain_error_keeps_model_visible_json_content() -> None:
    info = merry.MerryErrorInfo(
        code="order.not_found",
        domain="tool",
        message="The order was not found.",
        retryability="not_retryable",
    )
    error = merry.ToolDomainError(info, {"found": False})

    assert error.content == {"found": False}
    assert error.info == info


def test_error_info_rejects_unbounded_or_unknown_context() -> None:
    error = _decode_native_error(
        merry.NativeMerryError(
            '{"code":"invalid.domain","domain":"unknown",'
            '"message":"Invalid domain.","hint":null,'
            '"retryability":"not_retryable","context":{}}'
        )
    )
    assert isinstance(error, merry.MerryInternalError)
    assert error.code == "protocol.native_error_invalid"

    with pytest.raises(ValueError, match="unsupported key"):
        merry.MerryErrorInfo(
            code="invalid.context",
            domain="runtime",
            message="Invalid context.",
            context={"raw_exception": "must not become a public field"},
        )


def test_json_content_rejects_invalid_or_non_finite_json() -> None:
    with pytest.raises(ValueError, match="valid JSON"):
        merry.JsonContent("not-json")

    with pytest.raises(TypeError, match="non-finite"):
        merry.JsonContent("NaN")
