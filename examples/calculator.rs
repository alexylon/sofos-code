/// A simple CLI calculator demonstrating idiomatic Rust patterns:
/// enums, pattern matching, error handling, and iterators.
use std::fmt;

#[derive(Debug)]
enum CalcError {
    DivisionByZero,
    UnknownOperator(String),
    ParseError(String),
}

impl fmt::Display for CalcError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            CalcError::DivisionByZero => write!(f, "division by zero"),
            CalcError::UnknownOperator(op) => write!(f, "unknown operator: '{op}'"),
            CalcError::ParseError(s) => write!(f, "could not parse number: '{s}'"),
        }
    }
}

#[derive(Debug, Clone, Copy)]
enum Op {
    Add,
    Sub,
    Mul,
    Div,
}

impl Op {
    fn from_str(s: &str) -> Result<Self, CalcError> {
        match s {
            "+" => Ok(Op::Add),
            "-" => Ok(Op::Sub),
            "*" => Ok(Op::Mul),
            "/" => Ok(Op::Div),
            other => Err(CalcError::UnknownOperator(other.to_string())),
        }
    }

    fn apply(self, a: f64, b: f64) -> Result<f64, CalcError> {
        match self {
            Op::Add => Ok(a + b),
            Op::Sub => Ok(a - b),
            Op::Mul => Ok(a * b),
            Op::Div if b == 0.0 => Err(CalcError::DivisionByZero),
            Op::Div => Ok(a / b),
        }
    }
}

fn parse_f64(s: &str) -> Result<f64, CalcError> {
    s.parse::<f64>()
        .map_err(|_| CalcError::ParseError(s.to_string()))
}

/// Evaluates a space-separated expression like "3 + 4 * 2" left-to-right.
fn evaluate(expr: &str) -> Result<f64, CalcError> {
    let tokens: Vec<&str> = expr.split_whitespace().collect();

    if tokens.is_empty() {
        return Ok(0.0);
    }

    let mut result = parse_f64(tokens[0])?;

    for chunk in tokens[1..].chunks(2) {
        if let [op_str, num_str] = chunk {
            let op = Op::from_str(op_str)?;
            let num = parse_f64(num_str)?;
            result = op.apply(result, num)?;
        }
    }

    Ok(result)
}

fn main() {
    let expressions = [
        "10 + 5",
        "100 - 37",
        "6 * 7",
        "22 / 7",
        "10 / 0",
        "2 + 3 * 4",
        "bad input",
    ];

    for expr in &expressions {
        match evaluate(expr) {
            Ok(result) => println!("{expr} = {result:.4}"),
            Err(e) => println!("{expr} => Error: {e}"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_basic_ops() {
        assert_eq!(evaluate("2 + 3").unwrap(), 5.0);
        assert_eq!(evaluate("10 - 4").unwrap(), 6.0);
        assert_eq!(evaluate("3 * 4").unwrap(), 12.0);
        assert_eq!(evaluate("15 / 3").unwrap(), 5.0);
    }

    #[test]
    fn test_division_by_zero() {
        assert!(matches!(evaluate("5 / 0"), Err(CalcError::DivisionByZero)));
    }

    #[test]
    fn test_unknown_operator() {
        assert!(matches!(
            evaluate("5 % 2"),
            Err(CalcError::UnknownOperator(_))
        ));
    }

    #[test]
    fn test_parse_error() {
        assert!(matches!(evaluate("abc + 1"), Err(CalcError::ParseError(_))));
    }

    #[test]
    fn test_chained_ops() {
        assert_eq!(evaluate("1 + 2 + 3 + 4").unwrap(), 10.0);
    }
}
