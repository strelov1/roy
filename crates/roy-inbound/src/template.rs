//! Minimal template renderer. Replaces `{{payload.a.b.c}}` substrings with
//! the corresponding nested value from a `serde_json::Value`. Non-existent
//! paths render as empty string (with a `tracing::warn`). Non-string scalars
//! are rendered via `Display`-equivalent JSON serialization (so a number
//! becomes "42", a boolean "true", an object/array its JSON form).

use serde_json::Value;

pub fn render(template: &str, payload: &Value) -> String {
    let mut out = String::with_capacity(template.len());
    let mut rest = template;
    while let Some(start) = rest.find("{{") {
        out.push_str(&rest[..start]);
        let after_open = &rest[start + 2..];
        let Some(end) = after_open.find("}}") else {
            out.push_str("{{");
            rest = after_open;
            continue;
        };
        let expr = after_open[..end].trim();
        let value_str = resolve_path(expr, payload);
        out.push_str(&value_str);
        rest = &after_open[end + 2..];
    }
    out.push_str(rest);
    out
}

fn resolve_path(expr: &str, payload: &Value) -> String {
    let Some(rest) = expr.strip_prefix("payload") else {
        tracing::warn!(expr, "template path missing `payload.` prefix");
        return String::new();
    };
    let mut node = payload;
    for segment in rest.split('.').filter(|s| !s.is_empty()) {
        match node {
            Value::Object(map) => match map.get(segment) {
                Some(child) => node = child,
                None => {
                    tracing::warn!(expr, segment, "template path missing in payload");
                    return String::new();
                }
            },
            _ => {
                tracing::warn!(
                    expr,
                    segment,
                    "template path tried to descend into non-object"
                );
                return String::new();
            }
        }
    }
    match node {
        Value::String(s) => s.clone(),
        Value::Null => String::new(),
        other => other.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn flat_string_substitutes() {
        let out = render("hello {{payload.name}}", &json!({"name": "world"}));
        assert_eq!(out, "hello world");
    }

    #[test]
    fn nested_path_substitutes() {
        let out = render(
            "order {{payload.body.id}} from {{payload.body.user.email}}",
            &json!({"body": {"id": 42, "user": {"email": "a@b"}}}),
        );
        assert_eq!(out, "order 42 from a@b");
    }

    #[test]
    fn missing_path_renders_empty() {
        let out = render("hi {{payload.absent}}", &json!({}));
        assert_eq!(out, "hi ");
    }

    #[test]
    fn no_placeholders_passthrough() {
        let out = render("static text", &json!({"x": 1}));
        assert_eq!(out, "static text");
    }

    #[test]
    fn boolean_and_object_serialize() {
        let out = render(
            "active={{payload.active}} meta={{payload.meta}}",
            &json!({"active": true, "meta": {"a": 1}}),
        );
        assert_eq!(out, "active=true meta={\"a\":1}");
    }
}
