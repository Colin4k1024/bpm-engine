//! EL (Expression Language) evaluator for gateway conditions.
//! Lightweight parser: no external deps. Supports key == "literal", key != "", key > 100, etc.

use std::collections::HashMap;

#[derive(Debug, Clone)]
pub struct ElError(pub String);

/// Evaluates a condition expression against process variables. Returns true iff the condition holds.
/// Supported forms:
/// - `a and b`, `a or b` — logical and/or (plan v2.0 D.1); " or " splits first, then " and "
/// - `key == "literal"` or `key == 'literal'` — string equality
/// - `key != "literal"` — string inequality
/// - `key > n`, `key >= n`, `key < n`, `key <= n` — numeric comparison (variable value parsed as f64)
/// - `key` — truthy: variable exists and is non-empty
pub fn eval_condition(expr: &str, variables: &HashMap<String, String>) -> Result<bool, ElError> {
    let expr = expr.trim();
    if expr.is_empty() {
        return Err(ElError("empty expression".into()));
    }

    // Top-level: " or " (disjunction)
    if expr.contains(" or ") {
        let parts: Vec<&str> = expr.split(" or ").map(str::trim).collect();
        for part in parts {
            if eval_condition(part, variables)? {
                return Ok(true);
            }
        }
        return Ok(false);
    }

    // Second-level: " and " (conjunction)
    if expr.contains(" and ") {
        let parts: Vec<&str> = expr.split(" and ").map(str::trim).collect();
        for part in parts {
            if !eval_condition(part, variables)? {
                return Ok(false);
            }
        }
        return Ok(true);
    }

    // Single identifier: truthy if present and non-empty
    if !expr.contains(' ') {
        let val = variables.get(expr).map(|s| s.as_str()).unwrap_or("");
        return Ok(!val.is_empty());
    }

    // Binary operators (with spaces): op must be surrounded by spaces to avoid matching inside identifiers
    let ops = [" == ", " != ", " >= ", " <= ", " > ", " < "];
    for op in ops {
        if let Some(pos) = expr.find(op) {
            let left = expr[..pos].trim();
            let right = expr[pos + op.len()..].trim();
            if left.is_empty() {
                return Err(ElError("missing left operand".into()));
            }
            let left_val = variables.get(left).map(|s| s.as_str()).unwrap_or("");
            let right_trim = op.trim();
            return match right_trim {
                "==" => eval_eq(left_val, right, variables),
                "!=" => eval_neq(left_val, right, variables),
                ">" => eval_cmp(left_val, right, variables, |a, b| a > b),
                ">=" => eval_cmp(left_val, right, variables, |a, b| a >= b),
                "<" => eval_cmp(left_val, right, variables, |a, b| a < b),
                "<=" => eval_cmp(left_val, right, variables, |a, b| a <= b),
                _ => Err(ElError(format!("unknown operator: {}", right_trim))),
            };
        }
    }

    Err(ElError(format!("unrecognized expression: {}", expr)))
}

fn unquote(s: &str) -> Option<&str> {
    let s = s.trim();
    if (s.starts_with('"') && s.ends_with('"')) || (s.starts_with('\'') && s.ends_with('\'')) {
        Some(s[1..s.len() - 1].trim())
    } else {
        None
    }
}

fn eval_eq(left_val: &str, right: &str, variables: &HashMap<String, String>) -> Result<bool, ElError> {
    let right_val = if let Some(q) = unquote(right) {
        q.to_string()
    } else {
        variables.get(right.trim()).cloned().unwrap_or_else(|| right.trim().to_string())
    };
    Ok(left_val == right_val.as_str())
}

fn eval_neq(left_val: &str, right: &str, variables: &HashMap<String, String>) -> Result<bool, ElError> {
    let right_val = if let Some(q) = unquote(right) {
        q.to_string()
    } else {
        variables.get(right.trim()).cloned().unwrap_or_else(|| right.trim().to_string())
    };
    Ok(left_val != right_val.as_str())
}

fn parse_f64(s: &str, variables: &HashMap<String, String>) -> Result<f64, ElError> {
    let s = s.trim();
    if let Some(q) = unquote(s) {
        q.parse::<f64>().map_err(|_| ElError(format!("not a number: {}", q)))
    } else if let Some(v) = variables.get(s) {
        v.trim().parse::<f64>().map_err(|_| ElError(format!("variable {} is not a number: {:?}", s, v)))
    } else {
        s.parse::<f64>().map_err(|_| ElError(format!("not a number: {}", s)))
    }
}

fn eval_cmp<F>(
    left_val: &str,
    right: &str,
    variables: &HashMap<String, String>,
    op: F,
) -> Result<bool, ElError>
where
    F: FnOnce(f64, f64) -> bool,
{
    let a = parse_f64(left_val, variables).or_else(|_| left_val.trim().parse::<f64>().map_err(|_| ElError(format!("left operand not numeric: {:?}", left_val))))?;
    let b = parse_f64(right, variables)?;
    Ok(op(a, b))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn vars(pairs: &[(&str, &str)]) -> HashMap<String, String> {
        pairs.iter().map(|(k, v)| ((*k).into(), (*v).into())).collect()
    }

    #[test]
    fn el_single_key_truthy() {
        let v = vars(&[("valid", "true")]);
        assert!(eval_condition("valid", &v).unwrap());
        assert!(!eval_condition("missing", &v).unwrap());
        assert!(!eval_condition("empty", &vars(&[("empty", "")])).unwrap());
    }

    #[test]
    fn el_eq_string() {
        let v = vars(&[("valid", "true")]);
        assert!(eval_condition(r#"valid == "true""#, &v).unwrap());
        assert!(!eval_condition(r#"valid == "false""#, &v).unwrap());
        assert!(eval_condition("valid == true", &v).unwrap()); // unquoted right = literal "true"
    }

    #[test]
    fn el_neq_string() {
        let v = vars(&[("status", "rejected")]);
        assert!(eval_condition(r#"status != "approved""#, &v).unwrap());
        assert!(!eval_condition(r#"status != "rejected""#, &v).unwrap());
    }

    #[test]
    fn el_numeric_cmp() {
        let v = vars(&[("amount", "100")]);
        assert!(eval_condition("amount > 50", &v).unwrap());
        assert!(eval_condition("amount >= 100", &v).unwrap());
        assert!(!eval_condition("amount > 100", &v).unwrap());
        assert!(eval_condition("amount < 200", &v).unwrap());
        assert!(eval_condition("amount <= 100", &v).unwrap());
    }

    #[test]
    fn el_empty_err() {
        assert!(eval_condition("", &HashMap::new()).is_err());
    }

    #[test]
    fn el_and_or() {
        let v = vars(&[("a", "1"), ("b", "2"), ("c", "")]);
        assert!(eval_condition("a and b", &v).unwrap());
        assert!(!eval_condition("a and c", &v).unwrap());
        assert!(eval_condition("a or c", &v).unwrap());
        assert!(eval_condition("c or a", &v).unwrap());
        assert!(!eval_condition("c or missing", &v).unwrap());
        assert!(eval_condition(r#"a == "1" and b == "2""#, &v).unwrap());
    }
}
