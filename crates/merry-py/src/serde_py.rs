use pyo3::{
    prelude::*,
    types::{PyDict, PyList},
};
use serde_json::Value;

pub(crate) fn json_to_py(py: Python<'_>, value: &Value) -> PyResult<Py<PyAny>> {
    Ok(match value {
        Value::Null => py.None(),
        Value::Bool(value) => value.into_pyobject(py)?.to_owned().unbind().into_any(),
        Value::Number(value) => number_to_py(py, value)?,
        Value::String(value) => value.into_pyobject(py)?.unbind().into_any(),
        Value::Array(values) => {
            let list = PyList::empty(py);
            for item in values {
                list.append(json_to_py(py, item)?)?;
            }
            list.unbind().into_any()
        }
        Value::Object(values) => {
            let dict = PyDict::new(py);
            for (key, item) in values {
                dict.set_item(key, json_to_py(py, item)?)?;
            }
            dict.unbind().into_any()
        }
    })
}

fn number_to_py(py: Python<'_>, value: &serde_json::Number) -> PyResult<Py<PyAny>> {
    if let Some(integer) = value.as_i64() {
        return Ok(integer.into_pyobject(py)?.unbind().into_any());
    }

    if let Some(unsigned) = value.as_u64() {
        return Ok(unsigned.into_pyobject(py)?.unbind().into_any());
    }

    Ok(value
        .as_f64()
        .expect("serde_json finite number must convert to f64")
        .into_pyobject(py)?
        .unbind()
        .into_any())
}
