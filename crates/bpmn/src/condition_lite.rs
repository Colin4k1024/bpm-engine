//! A deliberately small condition evaluator shared by the E2-2 static
//! validator ([`crate::validation`]) and dry-run simulator
//! ([`crate::simulation`]).
//!
//! The full expression language lives in `bpm_engine_runtime::el`
//! (`eval_condition`). The BPMN crate cannot depend on the runtime crate
//! (the runtime depends on the BPMN compiler), so this module re-implements
//! exactly the subset the dry-run needs, mirroring the runtime's semantics
//! for the cases it covers:
//!
//! * `lhs == "literal"` / `lhs != "literal"` — a single spaced comparison
//!   with a single-token left operand and a quoted or single-token right
//!   operand (the canonical form the DSL `variableEq` condition lowers to,
//!   `${variable == "value"}`). A quoted literal is one operand even when
//!   it contains spaces (`x == "salt and pepper"`), like the runtime.
//! * bare-variable truthiness for no-space operands — the runtime returns
//!   the non-emptiness of `variables[expr]` whenever the expression contains
//!   no space, *before* it ever looks for operators.
//!
//! Anything else (`and`/`or` composition, ordering comparisons `>`, `<`,
//! `>=`, `<=`, multi-token operands, …) parses as [`LiteExpr::Unsupported`]:
//! the dry-run reports the branch as unresolved rather than guessing. This
//! is an intentional, documented deviation: dry-run is advisory, so "cannot
//! decide" is a more useful answer than a wrong route. At runtime the full
//! EL evaluator handles every one of these forms (and an expression the full
//! EL cannot parse is treated as `false` by the exclusive gateway evaluator).
//!
//! Like the runtime's evaluator, scanning is **quote-aware**: ` and `, ` or `
//! and operator text inside a single/double-quoted literal (`x == "salt and
//! pepper"`, `x != "a == b"`) never split the expression — only TOP-LEVEL
//! separators and operators do.

use std::collections::HashMap;

/// The right-hand side of a supported comparison.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum LiteOperand {
    /// A quoted string literal (`"value"` or `'value'`).
    Quoted(String),
    /// A bare single token: the runtime looks it up as a variable and falls
    /// back to treating it as a literal when no such variable exists.
    Token(String),
}

/// The comparison operator of a supported `==`/`!=` comparison.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum LiteOperator {
    /// `lhs == rhs`
    Eq,
    /// `lhs != rhs`
    Neq,
}

/// The parsed `==`/`!=` comparison of a supported expression, exposed so
/// callers can compare branch conditions structurally (the static
/// validator's exhaustive-pair analysis, see
/// [`crate::validation`]). [`LiteExpr::Truthy`] and
/// [`LiteExpr::Unsupported`] have no comparison; `Unsupported` stays opaque.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct LiteComparison {
    /// Left operand: always a variable name.
    pub(crate) lhs: String,
    /// The comparison operator.
    pub(crate) operator: LiteOperator,
    /// Right operand: quoted literal or bare token.
    pub(crate) rhs: LiteOperand,
}

/// The parse result of one condition expression under the lightweight subset.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum LiteExpr {
    /// `lhs == rhs` (spaced `==`, single-token operands).
    Eq {
        /// Left operand: always a variable name.
        lhs: String,
        /// Right operand: quoted literal or bare token.
        rhs: LiteOperand,
    },
    /// `lhs != rhs` (spaced `!=`, single-token operands).
    Neq {
        /// Left operand: always a variable name.
        lhs: String,
        /// Right operand: quoted literal or bare token.
        rhs: LiteOperand,
    },
    /// A no-space expression: truthiness of `variables[expr]`.
    Truthy {
        /// The variable (the whole expression text).
        var: String,
    },
    /// Anything the subset does not cover; evaluation is not determinable.
    Unsupported,
}

impl LiteExpr {
    /// The comparison this expression performs, when it is one of the
    /// supported `==`/`!=` forms (else `None`).
    pub(crate) fn comparison(&self) -> Option<LiteComparison> {
        match self {
            LiteExpr::Eq { lhs, rhs } => Some(LiteComparison {
                lhs: lhs.clone(),
                operator: LiteOperator::Eq,
                rhs: rhs.clone(),
            }),
            LiteExpr::Neq { lhs, rhs } => Some(LiteComparison {
                lhs: lhs.clone(),
                operator: LiteOperator::Neq,
                rhs: rhs.clone(),
            }),
            LiteExpr::Truthy { .. } | LiteExpr::Unsupported => None,
        }
    }
}

/// Parse `raw` (already stripped of any `${...}` wrapper by the compiler)
/// into the lightweight subset.
pub(crate) fn parse(raw: &str) -> LiteExpr {
    let expr = raw.trim();
    if expr.is_empty() {
        return LiteExpr::Unsupported;
    }
    // Composition is out of scope for the subset — but only TOP-LEVEL
    // separators count (quote-aware, like the runtime): `x == "salt and
    // pepper"` has no top-level ` and ` and stays a single comparison.
    if split_outside_quotes(expr, " or ").is_some() || split_outside_quotes(expr, " and ").is_some()
    {
        return LiteExpr::Unsupported;
    }
    // No space at all: the runtime treats the whole text as a variable
    // reference (truthiness), regardless of any operator characters.
    if !expr.contains(' ') {
        return LiteExpr::Truthy {
            var: expr.to_string(),
        };
    }
    // Spaced single comparison, mirroring the runtime operator order. The
    // split is quote-aware, so operator text inside a quoted literal
    // (`x != "a == b"`) does not mis-split the expression.
    for (op, is_eq) in [(" == ", true), (" != ", false)] {
        if let Some(parts) = split_outside_quotes(expr, op) {
            if parts.len() != 2 {
                return LiteExpr::Unsupported;
            }
            let (lhs, rhs) = (parts[0].as_str(), parts[1].as_str());
            if lhs.is_empty() || rhs.is_empty() {
                return LiteExpr::Unsupported;
            }
            // The left operand is always a variable name: single token only.
            if lhs.contains(' ') {
                return LiteExpr::Unsupported;
            }
            let rhs = match unquote(rhs) {
                // A quoted literal is ONE operand even when it contains
                // spaces (`x == "salt and pepper"` — the runtime evaluates
                // the whole quoted text as a single literal).
                Some(q) => LiteOperand::Quoted(q),
                // A bare right operand must be a single token; anything
                // more complex is not covered (the runtime would slice it
                // differently).
                None => {
                    if rhs.contains(' ') {
                        return LiteExpr::Unsupported;
                    }
                    LiteOperand::Token(rhs.to_string())
                }
            };
            return if is_eq {
                LiteExpr::Eq {
                    lhs: lhs.to_string(),
                    rhs,
                }
            } else {
                LiteExpr::Neq {
                    lhs: lhs.to_string(),
                    rhs,
                }
            };
        }
    }
    LiteExpr::Unsupported
}

/// Split `expr` on `sep`, ignoring separators that appear inside single- or
/// double-quoted string literals. Returns `None` when `sep` does not occur
/// at the top level.
///
/// Copied verbatim from `split_outside_quotes` in
/// `crates/runtime/src/el.rs`: the BPMN crate cannot depend on the runtime
/// crate (the runtime depends on the BPMN compiler), so the quote-aware
/// scan is mirrored here to keep both evaluators in lockstep. Iteration is
/// over `char_indices` so multi-byte UTF-8 text is preserved verbatim and
/// every slice is taken at a char boundary.
fn split_outside_quotes(expr: &str, sep: &str) -> Option<Vec<String>> {
    let mut parts: Vec<String> = Vec::new();
    let mut current = String::new();
    let mut in_single = false;
    let mut in_double = false;
    let sep_len = sep.len();
    let mut skip_until = 0usize;
    for (idx, c) in expr.char_indices() {
        if idx < skip_until {
            continue;
        }
        if c == '"' && !in_single {
            in_double = !in_double;
            current.push(c);
            continue;
        }
        if c == '\'' && !in_double {
            in_single = !in_single;
            current.push(c);
            continue;
        }
        if !in_single && !in_double && expr.get(idx..idx + sep_len) == Some(sep) {
            parts.push(current.trim().to_string());
            current.clear();
            skip_until = idx + sep_len;
            continue;
        }
        current.push(c);
    }
    if parts.is_empty() {
        return None;
    }
    parts.push(current.trim().to_string());
    Some(parts)
}

/// Evaluate a parsed expression against `variables`.
///
/// Returns `None` exactly when the expression is
/// [`LiteExpr::Unsupported`] — the caller reports the branch as unresolved.
pub(crate) fn eval(expr: &LiteExpr, variables: &HashMap<String, String>) -> Option<bool> {
    match expr {
        LiteExpr::Truthy { var } => {
            Some(variables.get(var).map(|v| !v.is_empty()).unwrap_or(false))
        }
        LiteExpr::Eq { lhs, rhs } => {
            Some(operand_value(lhs, variables) == rhs_value(rhs, variables))
        }
        LiteExpr::Neq { lhs, rhs } => {
            Some(operand_value(lhs, variables) != rhs_value(rhs, variables))
        }
        LiteExpr::Unsupported => None,
    }
}

/// Parse and evaluate in one step.
pub(crate) fn eval_str(raw: &str, variables: &HashMap<String, String>) -> Option<bool> {
    eval(&parse(raw), variables)
}

/// Variable names referenced by an expression, for the informational input
/// contract ([`crate::validation`] `condition_variable_scope`).
///
/// Simple tokenizer: everything outside single/double quotes, split on
/// whitespace and operator characters, with numeric literals and the
/// boolean/composition keywords filtered out. Unquote-aware, so
/// `x == "lit"` yields only `x`, while `x == y` yields both `x` and `y`
/// (the runtime resolves a bare right operand as a variable first).
pub(crate) fn referenced_variables(raw: &str) -> Vec<String> {
    let mut vars: Vec<String> = Vec::new();
    let mut current = String::new();
    let mut in_single = false;
    let mut in_double = false;
    let flush = |current: &mut String, vars: &mut Vec<String>| {
        let token = current.trim().to_string();
        current.clear();
        if is_variable_token(&token) && !vars.contains(&token) {
            vars.push(token);
        }
    };
    for c in raw.chars() {
        match c {
            '"' if !in_single => {
                in_double = !in_double;
                // Discard any partial token: quoted text is a literal.
                current.clear();
            }
            '\'' if !in_double => {
                in_single = !in_single;
                current.clear();
            }
            // Quoted text is a literal, never a variable reference.
            _ if in_single || in_double => {}
            _ if c.is_whitespace() || is_operator_char(c) => flush(&mut current, &mut vars),
            _ => current.push(c),
        }
    }
    flush(&mut current, &mut vars);
    vars
}

fn is_operator_char(c: char) -> bool {
    matches!(c, '=' | '!' | '<' | '>' | '&' | '|' | '(' | ')')
}

fn is_variable_token(token: &str) -> bool {
    if token.is_empty() {
        return false;
    }
    // Numeric literals (`100`, `-5`, `1.5`) are not variable references.
    if token.parse::<f64>().is_ok() {
        return false;
    }
    // EL keywords are not variable references.
    !matches!(token, "and" | "or" | "not" | "true" | "false")
}

/// Unquote a string literal, mirroring the runtime's `unquote` (double or
/// single quotes, inner trim).
fn unquote(s: &str) -> Option<String> {
    let s = s.trim();
    if (s.starts_with('"') && s.ends_with('"') && s.len() >= 2)
        || (s.starts_with('\'') && s.ends_with('\'') && s.len() >= 2)
    {
        Some(s[1..s.len() - 1].trim().to_string())
    } else {
        None
    }
}

fn operand_value(name: &str, variables: &HashMap<String, String>) -> String {
    variables.get(name).cloned().unwrap_or_default()
}

fn rhs_value(rhs: &LiteOperand, variables: &HashMap<String, String>) -> String {
    match rhs {
        LiteOperand::Quoted(q) => q.clone(),
        // The runtime resolves a bare right operand as a variable first,
        // falling back to the literal text.
        LiteOperand::Token(t) => variables.get(t).cloned().unwrap_or_else(|| t.clone()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn vars(pairs: &[(&str, &str)]) -> HashMap<String, String> {
        pairs
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect()
    }

    #[test]
    fn parse_supported_forms() {
        assert_eq!(
            parse("x == \"a\""),
            LiteExpr::Eq {
                lhs: "x".into(),
                rhs: LiteOperand::Quoted("a".into())
            }
        );
        assert_eq!(
            parse("status != 'done'"),
            LiteExpr::Neq {
                lhs: "status".into(),
                rhs: LiteOperand::Quoted("done".into())
            }
        );
        assert_eq!(
            parse("amount == 100"),
            LiteExpr::Eq {
                lhs: "amount".into(),
                rhs: LiteOperand::Token("100".into())
            }
        );
        assert_eq!(parse("flag"), LiteExpr::Truthy { var: "flag".into() });
        // No-space text with operators is still truthiness, like the runtime.
        assert_eq!(parse("x==5"), LiteExpr::Truthy { var: "x==5".into() });
    }

    #[test]
    fn parse_unsupported_forms() {
        assert_eq!(parse(""), LiteExpr::Unsupported);
        assert_eq!(parse("x > 5"), LiteExpr::Unsupported);
        assert_eq!(parse("x >= 5 and y < 2"), LiteExpr::Unsupported);
        assert_eq!(parse("a or b"), LiteExpr::Unsupported);
        assert_eq!(parse("foo bar"), LiteExpr::Unsupported);
    }

    #[test]
    fn quoted_separator_text_stays_a_single_comparison() {
        // ` and `/` or ` inside a quoted literal must NOT make the
        // expression a composition (parity with the runtime's quote-aware
        // split, which explicitly supports and tests these forms).
        assert_eq!(
            parse("x == \"salt and pepper\""),
            LiteExpr::Eq {
                lhs: "x".into(),
                rhs: LiteOperand::Quoted("salt and pepper".into())
            }
        );
        assert_eq!(
            parse("x != \"rock or roll\""),
            LiteExpr::Neq {
                lhs: "x".into(),
                rhs: LiteOperand::Quoted("rock or roll".into())
            }
        );
        // A real TOP-LEVEL separator is still a composition (Unsupported).
        assert_eq!(parse("x == \"a\" or y == \"b\""), LiteExpr::Unsupported);
        assert_eq!(parse("x == \"a\" and y == \"b\""), LiteExpr::Unsupported);
    }

    #[test]
    fn quoted_operator_text_does_not_mis_split() {
        // The ` == ` inside the quoted literal must not be found before the
        // top-level ` != `.
        assert_eq!(
            parse("x != \"a == b\""),
            LiteExpr::Neq {
                lhs: "x".into(),
                rhs: LiteOperand::Quoted("a == b".into())
            }
        );
        // Multiple comparisons at top level are still out of the subset.
        assert_eq!(parse("x == y == z"), LiteExpr::Unsupported);
    }

    #[test]
    fn quoted_literals_evaluate_as_exact_matches() {
        let v = vars(&[
            ("flavor", "salt and pepper"),
            ("song", "rock or roll"),
            ("x", "a == b"),
        ]);
        assert_eq!(eval_str("flavor == \"salt and pepper\"", &v), Some(true));
        assert_eq!(eval_str("flavor == \"plain\"", &v), Some(false));
        assert_eq!(eval_str("song != \"rock or roll\"", &v), Some(false));
        assert_eq!(eval_str("song != \"other\"", &v), Some(true));
        assert_eq!(eval_str("x != \"a == b\"", &v), Some(false));
        assert_eq!(eval_str("x == \"a == b\"", &v), Some(true));
    }

    #[test]
    fn comparison_exposes_lhs_operator_rhs() {
        let eq = parse("x == \"a\"").comparison().expect("eq comparison");
        assert_eq!(eq.lhs, "x");
        assert_eq!(eq.operator, LiteOperator::Eq);
        assert_eq!(eq.rhs, LiteOperand::Quoted("a".into()));
        let neq = parse("x != y").comparison().expect("neq comparison");
        assert_eq!(neq.lhs, "x");
        assert_eq!(neq.operator, LiteOperator::Neq);
        assert_eq!(neq.rhs, LiteOperand::Token("y".into()));
        // Truthy and Unsupported have no comparison (Unsupported stays
        // opaque).
        assert!(parse("flag").comparison().is_none());
        assert!(parse("a or b").comparison().is_none());
        assert!(parse("x > 5").comparison().is_none());
    }

    #[test]
    fn eval_matches_runtime_semantics_for_subset() {
        let v = vars(&[("x", "a"), ("flag", "yes"), ("empty", ""), ("y", "b")]);
        assert_eq!(eval_str("x == \"a\"", &v), Some(true));
        assert_eq!(eval_str("x == \"b\"", &v), Some(false));
        assert_eq!(eval_str("x != \"b\"", &v), Some(true));
        // Bare token rhs resolves as variable first.
        assert_eq!(eval_str("x == y", &v), Some(false));
        assert_eq!(eval_str("x == z", &v), Some(false)); // literal fallback "z"
                                                         // Truthiness: missing/empty false, non-empty true.
        assert_eq!(eval_str("flag", &v), Some(true));
        assert_eq!(eval_str("empty", &v), Some(false));
        assert_eq!(eval_str("missing", &v), Some(false));
        // Unsupported -> None (not determinable).
        assert_eq!(eval_str("amount > 100", &v), None);
    }

    #[test]
    fn referenced_variables_extracts_operands_outside_quotes() {
        assert_eq!(referenced_variables("x == \"lit\""), vec!["x".to_string()]);
        assert_eq!(
            referenced_variables("x == y"),
            vec!["x".to_string(), "y".to_string()]
        );
        assert_eq!(
            referenced_variables("amount > 100"),
            vec!["amount".to_string()]
        );
        assert_eq!(
            referenced_variables("a and b or c"),
            vec!["a".to_string(), "b".to_string(), "c".to_string()]
        );
        assert_eq!(referenced_variables("flag"), vec!["flag".to_string()]);
        assert!(referenced_variables("\"quoted only\"").is_empty());
    }
}
