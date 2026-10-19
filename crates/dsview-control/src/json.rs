//! JSON as Qt's QJsonValue writes it: every number is a double, and a
//! double with an integral value is written without a fraction (so it reads
//! back as an integer).

use serde_json::{Map, Number, Value};

/// Largest integer a double holds exactly (2^53).
const EXACT: f64 = 9_007_199_254_740_992.0;

/// A number as QJsonValue(double) writes it.
pub fn num(v: f64) -> Value {
    if v.is_finite() && v == v.trunc() && v.abs() <= EXACT {
        if v < 0.0 {
            Value::from(v as i64)
        } else {
            Value::from(v as u64)
        }
    } else if v.is_finite() {
        Number::from_f64(v).map_or(Value::Null, Value::Number)
    } else {
        // QJsonValue writes NaN and infinities as null
        Value::Null
    }
}

/// Rewrites every number in `v` the way Qt would have written it.
pub fn qt(v: Value) -> Value {
    match v {
        Value::Number(n) => match n.as_f64() {
            Some(f) if !n.is_u64() && !n.is_i64() => num(f),
            _ => {
                // integers beyond 2^53 lose precision as doubles in Qt
                let f = n.as_f64().unwrap_or(0.0);
                if f.abs() > EXACT {
                    num(f)
                } else {
                    Value::Number(n)
                }
            }
        },
        Value::Array(a) => Value::Array(a.into_iter().map(qt).collect()),
        Value::Object(o) => Value::Object(o.into_iter().map(|(k, x)| (k, qt(x))).collect()),
        other => other,
    }
}

/// An object from key/value pairs.
pub fn obj<const N: usize>(pairs: [(&str, Value); N]) -> Value {
    let mut m = Map::new();
    for (k, v) in pairs {
        m.insert(k.to_owned(), v);
    }
    Value::Object(m)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn integral_doubles_are_integers() {
        assert_eq!(num(1000448.0), serde_json::json!(1000448));
        assert_eq!(num(-3.0), serde_json::json!(-3));
        assert_eq!(num(0.5), serde_json::json!(0.5));
        assert_eq!(num(f64::NAN), Value::Null);
        assert_eq!(
            qt(serde_json::json!({"a": [1.0, 2.5]})),
            serde_json::json!({"a": [1, 2.5]})
        );
    }
}
