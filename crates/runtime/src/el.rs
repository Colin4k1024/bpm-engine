//! EL (Expression Language) evaluator for gateway conditions.

use std::collections::HashMap;

/// Error from EL expression evaluation.
#[derive(Debug, Clone)]
pub struct ElError(pub String);

/// Evaluate a condition expression against process variables.
///
/// Supports comparisons (`==`, `!=`, `>`, `<`, `>=`, `<=`),
/// boolean logic (`and`, `or`, `not`), and variable references.
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
    // Handle negative numbers: if the string starts with '-' followed by a number,
    // parse the number part and negate it. This handles cases like "-1" or "- 1".
    if s.starts_with('-') && s.len() > 1 {
        let rest = &s[1..];
        // If the rest (after minus) is a valid number literal, parse and negate
        if let Ok(val) = rest.trim().parse::<f64>() {
            return Ok(-val);
        }
    }
    if let Some(q) = unquote(s) {
        // Check for negative inside quotes too
        if q.starts_with('-') && q.len() > 1 {
            if let Ok(val) = q[1..].trim().parse::<f64>() {
                return Ok(-val);
            }
        }
        q.parse::<f64>()
            .map_err(|_| ElError(format!("not a number: {}", q)))
    } else if let Some(v) = variables.get(s) {
        // Check for negative variable values
        let v_trim = v.trim();
        if v_trim.starts_with('-') && v_trim.len() > 1 {
            if let Ok(val) = v_trim[1..].trim().parse::<f64>() {
                return Ok(-val);
            }
        }
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_negative_number_literal_in_comparison() {
        // x >= -5: left value "10" should be >= -5
        let mut vars = HashMap::new();
        vars.insert("x".to_string(), "10".to_string());
        assert!(eval_condition("x >= -5", &vars).unwrap());

        // x >= -5: left value "-10" should NOT be >= -5
        let mut vars = HashMap::new();
        vars.insert("x".to_string(), "-10".to_string());
        assert!(!eval_condition("x >= -5", &vars).unwrap());

        // x > -5: left value "-4" should be > -5
        let mut vars = HashMap::new();
        vars.insert("x".to_string(), "-4".to_string());
        assert!(eval_condition("x > -5", &vars).unwrap());

        // x < -5: left value "-10" should be < -5
        let mut vars = HashMap::new();
        vars.insert("x".to_string(), "-10".to_string());
        assert!(eval_condition("x < -5", &vars).unwrap());
    }

    #[test]
    fn test_negative_number_with_spaces() {
        // x >= - 5 (with space after minus)
        let mut vars = HashMap::new();
        vars.insert("x".to_string(), "10".to_string());
        assert!(eval_condition("x >= - 5", &vars).unwrap());
    }

    #[test]
    fn test_negative_variable_value() {
        // x >= y where y has a negative value
        let mut vars = HashMap::new();
        vars.insert("x".to_string(), "10".to_string());
        vars.insert("y".to_string(), "-5".to_string());
        assert!(eval_condition("x >= y", &vars).unwrap());

        // x >= y where both are negative
        let mut vars = HashMap::new();
        vars.insert("x".to_string(), "-10".to_string());
        vars.insert("y".to_string(), "-5".to_string());
        assert!(!eval_condition("x >= y", &vars).unwrap());
    }

    #[test]
    fn test_negative_number_quoted() {
        // x >= "-5" with quoted negative literal
        let mut vars = HashMap::new();
        vars.insert("x".to_string(), "10".to_string());
        assert!(eval_condition("x >= \"-5\"", &vars).unwrap());
    }

    #[test]
    fn test_parse_f64_negative() {
        let vars = HashMap::new();

        // Direct negative number parsing
        assert_eq!(parse_f64("-5", &vars).unwrap(), -5.0);
        assert_eq!(parse_f64("- 5", &vars).unwrap(), -5.0);
        assert_eq!(parse_f64("-123.45", &vars).unwrap(), -123.45);
        assert_eq!(parse_f64("- 123.45", &vars).unwrap(), -123.45);

        // Positive numbers still work
        assert_eq!(parse_f64("5", &vars).unwrap(), 5.0);
        assert_eq!(parse_f64("123.45", &vars).unwrap(), 123.45);
    }
}
