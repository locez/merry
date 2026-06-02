def test_import_exposes_version():
    import merry

    assert isinstance(merry.__version__, str)
    assert merry.__version__


def test_import_does_not_expose_global_tool_decorator():
    import merry

    assert not hasattr(merry, "tool")
