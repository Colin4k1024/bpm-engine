//! EL (Expression Language) evaluator for gateway conditions.

use std::collections::HashMap;

#[derive(Debug, Clone)]
pub struct ElError(pub String);

pub fn eval_condition(expr: &str, variables: &HashMap<String, String>) -> Result<bool, ElError> {
    let expr = expr.trim();
    if expr.is_empty() {
        return Err(ElError("empty expression".into()));
    }
    if expr.contains(" or ") {
        let parts: Vec<&str> = expr.split(" or ").map(str::trim).collect();
        for part in parts {
            if eval_condition(part, variables)? {
                return Ok(true);
            }
        }
        return Ok(false);
    }
    if expr.contains(" and ") {
        let parts: Vec<&str> = expr.split(" and ").map(str::trim).collect();
        for part in parts {
            if !eval_condition(part, variables)? {
                return Ok(false);
            }
        }
        return Ok(true);
    }
    if !expr.contains(' ') {
        let val = variables.get(expr).map(|s| s.as_str()).unwrap_or("");
        return Ok(!val.is_empty());
    }
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

fn eval_eq(
    left_val: &str,
    right: &str,
    variables: &HashMap<String, String>,
) -> Result<bool, ElError> {
    let right_val = if let Some(q) = unquote(right) {
        q.to_string()
    } else {
        variables
            .get(right.trim())
            .cloned()
            .unwrap_or_else(|| right.trim().to_string())
    };
    Ok(left_val == right_val.as_str())
}

fn eval_neq(
    left_val: &str,
    right: &str,
    variables: &HashMap<String, String>,
) -> Result<bool, ElError> {
    let right_val = if let Some(q) = unquote(right) {
        q.to_string()
    } else {
        variables
            .get(right.trim())
            .cloned()
            .unwrap_or_else(|| right.trim().to_string())
    };
    Ok(left_val != right_val.as_str())
}

fn parse_f64(s: &str, variables: &HashMap<String, String>) -> Result<f64, ElError> {
    let s = s.trim();
    if let Some(q) = unquote(s) {
        q.parse::<f64>()
            .map_err(|_| ElError(format!("not a number: {}", q)))
    } else if let Some(v) = variables.get(s) {
        v.trim()
            .parse::<f64>()
            .map_err(|_| ElError(format!("variable {} is not a number: {:?}", s, v)))
    } else {
        s.parse::<f64>()
            .map_err(|_| ElError(format!("not a number: {}", s)))
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
    let a = parse_f64(left_val, variables).or_else(|_| {
        left_val
            .trim()
            .parse::<f64>()
            .map_err(|_| ElError(format!("left operand not numeric: {:?}", left_val)))
    })?;
    let b = parse_f64(right, variables)?;
    Ok(op(a, b))
}
