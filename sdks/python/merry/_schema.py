"""Shared validation and schema synthesis for Pydantic models at SDK boundaries."""

from __future__ import annotations

import inspect
from collections.abc import Mapping, Sequence
from enum import Enum
from types import UnionType
from typing import Annotated, Any, Literal, Union, get_args, get_origin

from pydantic import BaseModel, ConfigDict, create_model
from pydantic.fields import FieldInfo


def validate_model(model: type[BaseModel], label: str) -> None:
    """Validate a strict schema-emitting input or final-output Pydantic model."""

    if not inspect.isclass(model) or not issubclass(model, BaseModel):
        raise TypeError(f"{label} must be a Pydantic BaseModel subclass.")
    missing = _missing_field_descriptions(model, set())
    if missing:
        raise ValueError(
            f"{label} fields need Field(description=...): {', '.join(missing)}"
        )
    non_strict = _models_without_forbid_extra(model, set())
    if non_strict:
        raise ValueError(
            f"{label} models must use ConfigDict(extra='forbid'): "
            f"{', '.join(non_strict)}"
        )
    _validate_model_annotations(model, label, set())


def validate_output_model(model: type[BaseModel], label: str) -> None:
    """Validate an output Pydantic model without requiring input-only schema constraints."""

    if not inspect.isclass(model) or not issubclass(model, BaseModel):
        raise TypeError(f"{label} must be a Pydantic BaseModel subclass.")
    _validate_model_annotations(model, label, set())


def synthesize_input_model(
    model_name: str,
    parameters: Sequence[inspect.Parameter],
    type_hints: Mapping[str, object],
    *,
    label: str,
) -> type[BaseModel]:
    """Build a strict Pydantic input model from annotated Python function parameters."""

    # pydantic.create_model accepts dynamic field descriptors typed as Any at its boundary.
    fields: dict[str, Any] = {}
    for parameter in parameters:
        if parameter.kind not in (
            inspect.Parameter.POSITIONAL_OR_KEYWORD,
            inspect.Parameter.KEYWORD_ONLY,
        ):
            raise TypeError(
                f"Tool parameter {parameter.name!r} in {label} must accept keyword arguments."
            )
        annotation = type_hints.get(parameter.name)
        if annotation is None:
            raise TypeError(
                f"Tool input parameter {parameter.name!r} must be annotated."
            )
        base_type, field_info = _extract_field_info(annotation, parameter.default)
        fields[parameter.name] = (base_type, field_info)

    synthesized: type[BaseModel] = create_model(
        model_name,
        __config__=ConfigDict(extra="forbid"),
        **fields,
    )
    validate_model(synthesized, f"{label} input model")
    return synthesized


def _extract_field_info(annotation: object, default: object) -> tuple[object, object]:
    is_annotated = get_origin(annotation) is Annotated
    if isinstance(default, FieldInfo):
        if is_annotated and any(
            isinstance(meta, FieldInfo) for meta in get_args(annotation)[1:]
        ):
            raise ValueError(
                "Do not supply Field(...) both in Annotated[...] and as a default value."
            )
        unwrapped = get_args(annotation)[0] if is_annotated else annotation
        return unwrapped, default
    if default is inspect.Parameter.empty:
        return annotation, ...
    return annotation, default


def _missing_field_descriptions(
    model: type[BaseModel],
    seen: set[type[BaseModel]],
) -> list[str]:
    if model in seen:
        return []
    seen.add(model)
    missing: list[str] = []
    for name, field in model.model_fields.items():
        description = field.description
        if not isinstance(description, str) or not description.strip():
            missing.append(f"{model.__name__}.{name}")
        for nested_model in _nested_models(field.annotation):
            missing.extend(_missing_field_descriptions(nested_model, seen))
    return missing


def _nested_models(annotation: object) -> list[type[BaseModel]]:
    if isinstance(annotation, type) and issubclass(annotation, BaseModel):
        return [annotation]
    nested: list[type[BaseModel]] = []
    for argument in get_args(annotation):
        nested.extend(_nested_models(argument))
    return nested


def _models_without_forbid_extra(
    model: type[BaseModel], seen: set[type[BaseModel]]
) -> list[str]:
    if model in seen:
        return []
    seen.add(model)
    missing: list[str] = []
    if model.model_config.get("extra") != "forbid":
        missing.append(model.__name__)
    for field in model.model_fields.values():
        for nested_model in _nested_models(field.annotation):
            missing.extend(_models_without_forbid_extra(nested_model, seen))
    return missing


def _validate_model_annotations(
    model: type[BaseModel], label: str, seen: set[type[BaseModel]]
) -> None:
    if model in seen:
        return
    seen.add(model)
    for name, field in model.model_fields.items():
        _validate_annotation(field.annotation, f"{label}.{name}", seen)


def _validate_annotation(
    annotation: object, label: str, seen: set[type[BaseModel]]
) -> None:
    if annotation is Any or annotation is object:
        raise TypeError(f"{label} must not use Any or object")
    if isinstance(annotation, type) and issubclass(annotation, BaseModel):
        _validate_model_annotations(annotation, label, seen)
        return
    if isinstance(annotation, type) and issubclass(annotation, Enum):
        return

    origin = get_origin(annotation)
    arguments = get_args(annotation)
    if origin is Literal:
        return
    if origin is Union or origin is UnionType:
        for argument in arguments:
            _validate_annotation(argument, label, seen)
        return
    if origin is Annotated:
        _validate_annotation(arguments[0], label, seen)
        return
    if origin in {dict, Mapping}:
        if len(arguments) != 2 or arguments[0] is not str:
            raise TypeError(f"{label} must use a typed string-keyed mapping")
        _validate_annotation(arguments[1], label, seen)
        return
    if origin in {list, set, frozenset, Sequence, tuple}:
        if not arguments:
            raise TypeError(f"{label} must use a parameterized collection")
        for argument in arguments:
            if argument is not Ellipsis:
                _validate_annotation(argument, label, seen)
        return
    if origin is None and annotation in {None, type(None), str, bool, int, float}:
        return
    if origin is None and annotation in {
        dict,
        list,
        set,
        frozenset,
        tuple,
        Mapping,
        Sequence,
    }:
        raise TypeError(f"{label} must use a parameterized collection")
    if origin is None and isinstance(annotation, type):
        return
    if origin is None and not arguments:
        raise TypeError(f"{label} uses an unsupported or untyped annotation")
