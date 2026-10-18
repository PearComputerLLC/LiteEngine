//! Monitor value formatting & list extraction.
//!
//! Kept separate from the render crate so the runtime can unit-test the
//! formatting rules (Scratch-style number display) without a renderer.

use crate::scene::MonitorKind;
use mlua::Value;

/// Format a Lua value the way Scratch shows variables:
/// integers without a decimal point, floats rounded to 4 trimmed decimals,
/// booleans as `true`/`false`, strings verbatim, nil as empty.
pub fn format_value(v: &Value) -> String {
    match v {
        Value::Number(n) => format_number(*n),
        Value::Integer(i) => format!("{i}"),
        Value::Boolean(b) => format!("{b}"),
        Value::String(s) => s.to_string_lossy().to_string(),
        Value::Nil => String::new(),
        _ => "(...)".to_string(),
    }
}

/// Scratch never shows `0.30000000000000004`. Round to 4 decimals, strip
/// trailing zeros, and print integral values without a point.
pub fn format_number(n: f64) -> String {
    if !n.is_finite() {
        return if n.is_nan() {
            "nan".into()
        } else {
            if n > 0.0 { "inf" } else { "-inf" }.into()
        };
    }
    if n.fract() == 0.0 && n.abs() < 1e15 {
        return format!("{}", n as i64);
    }
    let rounded = (n * 10_000.0).round() / 10_000.0;
    let mut s = format!("{rounded}");
    if s.contains('.') {
        while s.ends_with('0') {
            s.pop();
        }
        if s.ends_with('.') {
            s.pop();
        }
    }
    s
}

/// Read a Lua global as a list of display items plus the total count.
/// Tables only; anything else yields an empty list. The probe is capped so
/// a huge table cannot stall a frame (`max_items`).
pub fn read_list(lua: &mlua::Lua, name: &str, max_items: usize) -> (Vec<String>, usize) {
    let Ok(Value::Table(t)) = lua.globals().raw_get::<Value>(name) else {
        return (Vec::new(), 0);
    };
    let mut items = Vec::new();
    let mut i = 1i64;
    while items.len() < max_items {
        match t.raw_get::<Value>(i) {
            Ok(v) => {
                items.push(match &v {
                    Value::Nil => break, // end of the array part
                    other => format_value(other),
                });
                i += 1;
            }
            Err(_) => break,
        }
    }
    // Count the rest without formatting (cheap type probe only).
    let mut total = items.len();
    if i > 1 {
        let mut probe = i;
        while total < 100_000 {
            match t.raw_get::<Value>(probe) {
                Ok(Value::Nil) | Err(_) => break,
                Ok(_) => {
                    total += 1;
                    probe += 1;
                }
            }
        }
    }
    (items, total)
}

/// Classify a global for `monitors.show` auto-detection: a table becomes a
/// list monitor, anything else a variable monitor.
pub fn auto_kind(lua: &mlua::Lua, name: &str) -> MonitorKind {
    match lua.globals().raw_get::<Value>(name) {
        Ok(Value::Table(_)) => MonitorKind::List,
        _ => MonitorKind::Variable,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn numbers_display_scratch_style() {
        assert_eq!(format_number(3.0), "3");
        assert_eq!(format_number(-42.0), "-42");
        assert_eq!(format_number(1.5), "1.5");
        assert_eq!(format_number(0.1 + 0.2), "0.3");
        assert_eq!(format_number(1.23456), "1.2346");
        assert_eq!(format_number(2.00001), "2");
        assert_eq!(format_number(100.0), "100");
    }

    #[test]
    fn values_format() {
        let lua = mlua::Lua::new();
        assert_eq!(format_value(&Value::Boolean(true)), "true");
        assert_eq!(format_value(&Value::Nil), "");
        let s = lua.create_string("hi").unwrap();
        assert_eq!(format_value(&Value::String(s)), "hi");
    }

    #[test]
    fn list_read_window() {
        let lua = mlua::Lua::new();
        lua.load("inventory = {'sword', 'shield', 42, true}").exec().unwrap();
        let (items, total) = read_list(&lua, "inventory", 64);
        assert_eq!(items, vec!["sword", "shield", "42", "true"]);
        assert_eq!(total, 4);
        assert_eq!(read_list(&lua, "missing", 8), (vec![], 0));
        assert_eq!(auto_kind(&lua, "inventory"), MonitorKind::List);
        assert_eq!(auto_kind(&lua, "missing"), MonitorKind::Variable);
    }
}
