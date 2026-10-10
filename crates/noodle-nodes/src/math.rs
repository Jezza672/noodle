//! The Math node: arithmetic on its inputs, from a typed expression.

use noodle_engine::{
    Config, ConfigInfo, Context, Instance, Lane, LaneKernel, Layout, NodeError, NodeInfo, NodeType,
    ParamInfo, PerLane, Ports, Setup,
};

/// Computes an expression of its four inputs `a`, `b`, `c` and `d` for every
/// sample, such as `a * 2 + sin(b)` or `clamp(a, -1, 1) * c`.
///
/// - **Operators:** `+ - * / % ^` (power, right to left), unary `-`, and
///   parentheses, with the usual precedence.
/// - **Constants:** `pi`, `tau`, `e`, and numbers like `0.5` or `1e3`.
/// - **Functions:** `sin cos tan tanh abs sqrt exp ln log2 log10 floor ceil
///   round fract sign` of one argument, `min max pow atan2` of two, and
///   `clamp(x, lo, hi)` and `mix(a, b, t)` of three.
///
/// The expression is config, since it decides what the node is, not a value
/// to move; a wrong one fails the node with a message instead of playing.
/// It is parsed into a short program when the node is built, off the audio
/// thread, and each sample just runs that program on a small stack, so
/// nothing allocates while playing. Division by zero and the like give
/// whatever floating point gives (infinity or NaN), as in any other node.
///
/// Every input is a parameter that may be wired, with its own value when not.
/// Inputs broadcast, so a mono `b` can scale every voice of a polyphonic `a`.
pub struct Math;

pub const MATH_ID: &str = "noodle.util.math";

#[derive(Ports)]
struct MathPorts {
    #[param("a", "A", ParamInfo::new(-1_000_000.0, 1_000_000.0, 0.0))]
    a: (),
    #[param("b", "B", ParamInfo::new(-1_000_000.0, 1_000_000.0, 0.0))]
    b: (),
    #[param("c", "C", ParamInfo::new(-1_000_000.0, 1_000_000.0, 0.0))]
    c: (),
    #[param("d", "D", ParamInfo::new(-1_000_000.0, 1_000_000.0, 0.0))]
    d: (),
    #[output("out", "Out")]
    out: (),
}

const OUT: usize = MathPorts::OUT;

/// The expression when none is set: just pass `a` through.
const DEFAULT_EXPRESSION: &str = "a";

static CONFIG: [ConfigInfo; 1] = [ConfigInfo::text("expr", "Expression")];

static INFO: NodeInfo = NodeInfo {
    id: MATH_ID,
    version: 1,
    name: "Math",
    category: "Utilities",
};

impl Math {
    fn program(config: &Config) -> Result<Program, NodeError> {
        let text = CONFIG[0].get_text(config);
        let text = if text.trim().is_empty() {
            DEFAULT_EXPRESSION
        } else {
            &text
        };
        Program::compile(text).map_err(|e| NodeError::config(format!("Math: {e}")))
    }
}

impl NodeType for Math {
    fn info(&self) -> &NodeInfo {
        &INFO
    }

    fn config(&self) -> &[ConfigInfo] {
        &CONFIG
    }

    fn layout(&self, config: &Config) -> Result<Layout, NodeError> {
        Self::program(config)?;
        Ok(MathPorts::layout())
    }

    fn instantiate(&self, setup: &Setup<'_>) -> Result<Instance, NodeError> {
        let program = Self::program(setup.config)?;
        Ok(Instance::realtime(PerLane::new(MathKernel(program), setup)))
    }
}

struct MathKernel(Program);

impl LaneKernel for MathKernel {
    type State = ();

    fn process_lane(&mut self, _: &mut (), _: &Context, mut lane: Lane<'_, '_>) {
        let vars = [
            lane.inputs.get(MathPorts::A),
            lane.inputs.get(MathPorts::B),
            lane.inputs.get(MathPorts::C),
            lane.inputs.get(MathPorts::D),
        ];
        for (i, out) in lane.outputs.get_mut(OUT).iter_mut().enumerate() {
            *out = self
                .0
                .eval([vars[0][i], vars[1][i], vars[2][i], vars[3][i]]);
        }
    }
}

#[derive(Clone, Copy, Debug)]
enum Op {
    Push(f32),
    Var(usize),
    Neg,
    Add,
    Sub,
    Mul,
    Div,
    Rem,
    Pow,
    Min,
    Max,
    Atan2,
    Clamp,
    Mix,
    Unary(fn(f32) -> f32),
}

/// The deepest the evaluation stack can get, so it can live on the stack.
const DEPTH: usize = 32;

/// A compiled expression, in postfix order.
struct Program {
    ops: Vec<Op>,
}

impl Program {
    fn compile(text: &str) -> Result<Self, String> {
        let tokens = lex(text)?;
        let mut parser = Parser {
            tokens,
            at: 0,
            ops: Vec::new(),
            depth: 0,
        };
        parser.expression(0)?;
        if let Some(token) = parser.tokens.get(parser.at) {
            return Err(format!("unexpected {}", token.describe()));
        }
        let program = Self { ops: parser.ops };
        if program.max_depth() > DEPTH {
            return Err("the expression is too deeply nested".into());
        }
        Ok(program)
    }

    /// The most values the program holds on its stack at once.
    fn max_depth(&self) -> usize {
        let (mut depth, mut max) = (0usize, 0usize);
        for op in &self.ops {
            match op {
                Op::Push(_) | Op::Var(_) => depth += 1,
                Op::Neg | Op::Unary(_) => {}
                Op::Clamp | Op::Mix => depth -= 2,
                _ => depth -= 1,
            }
            max = max.max(depth);
        }
        max
    }

    fn eval(&self, vars: [f32; 4]) -> f32 {
        let mut stack = [0.0f32; DEPTH];
        let mut top = 0;
        for op in &self.ops {
            match *op {
                Op::Push(x) => {
                    stack[top] = x;
                    top += 1;
                }
                Op::Var(i) => {
                    stack[top] = vars[i];
                    top += 1;
                }
                Op::Neg => stack[top - 1] = -stack[top - 1],
                Op::Unary(f) => stack[top - 1] = f(stack[top - 1]),
                Op::Clamp => {
                    let (lo, hi) = (stack[top - 2], stack[top - 1]);
                    top -= 2;
                    // Like f32::clamp, without its panic on lo > hi or NaN.
                    stack[top - 1] = stack[top - 1].max(lo).min(hi);
                }
                Op::Mix => {
                    let (b, t) = (stack[top - 2], stack[top - 1]);
                    top -= 2;
                    let a = stack[top - 1];
                    stack[top - 1] = a + (b - a) * t;
                }
                binary => {
                    let (x, y) = (stack[top - 2], stack[top - 1]);
                    top -= 1;
                    stack[top - 1] = match binary {
                        Op::Add => x + y,
                        Op::Sub => x - y,
                        Op::Mul => x * y,
                        Op::Div => x / y,
                        Op::Rem => x % y,
                        Op::Pow => x.powf(y),
                        Op::Min => x.min(y),
                        Op::Max => x.max(y),
                        Op::Atan2 => x.atan2(y),
                        _ => unreachable!("handled above"),
                    };
                }
            }
        }
        stack[0]
    }
}

#[derive(Clone, Debug, PartialEq)]
enum Token {
    Number(f32),
    Name(String),
    Symbol(char),
}

impl Token {
    fn describe(&self) -> String {
        match self {
            Token::Number(n) => format!("number {n}"),
            Token::Name(name) => format!("`{name}`"),
            Token::Symbol(c) => format!("`{c}`"),
        }
    }
}

fn lex(text: &str) -> Result<Vec<Token>, String> {
    let chars: Vec<char> = text.chars().collect();
    let mut tokens = Vec::new();
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        if c.is_whitespace() {
            i += 1;
        } else if c.is_ascii_digit() || c == '.' {
            let start = i;
            while i < chars.len() && (chars[i].is_ascii_digit() || chars[i] == '.') {
                i += 1;
            }
            // An exponent: e3, E-2. Not the constant `e`, which has no digits
            // straight after it.
            if i < chars.len() && matches!(chars[i], 'e' | 'E') {
                let mut j = i + 1;
                if j < chars.len() && matches!(chars[j], '+' | '-') {
                    j += 1;
                }
                if j < chars.len() && chars[j].is_ascii_digit() {
                    while j < chars.len() && chars[j].is_ascii_digit() {
                        j += 1;
                    }
                    i = j;
                }
            }
            let literal: String = chars[start..i].iter().collect();
            let number = literal
                .parse::<f32>()
                .map_err(|_| format!("`{literal}` isn't a number"))?;
            tokens.push(Token::Number(number));
        } else if c.is_ascii_alphabetic() || c == '_' {
            let start = i;
            while i < chars.len() && (chars[i].is_ascii_alphanumeric() || chars[i] == '_') {
                i += 1;
            }
            let name: String = chars[start..i].iter().collect();
            tokens.push(Token::Name(name.to_ascii_lowercase()));
        } else if "+-*/%^(),".contains(c) {
            tokens.push(Token::Symbol(c));
            i += 1;
        } else {
            return Err(format!("unexpected `{c}`"));
        }
    }
    Ok(tokens)
}

struct Parser {
    tokens: Vec<Token>,
    at: usize,
    ops: Vec<Op>,
    /// How deep the recursion is now.
    depth: usize,
}

/// (binding power, right-associative) of a binary operator.
fn binary(c: char) -> Option<(u8, bool, Op)> {
    Some(match c {
        '+' => (1, false, Op::Add),
        '-' => (1, false, Op::Sub),
        '*' => (2, false, Op::Mul),
        '/' => (2, false, Op::Div),
        '%' => (2, false, Op::Rem),
        '^' => (4, true, Op::Pow),
        _ => return None,
    })
}

/// How tightly unary minus binds: looser than `^`, so `-a^2` is `-(a^2)`.
const NEGATE: u8 = 3;

/// The deepest the parser recurses, so a hostile string of `(` can't
/// overflow the stack.
const NESTING: usize = 64;

impl Parser {
    fn peek(&self) -> Option<&Token> {
        self.tokens.get(self.at)
    }

    fn eat(&mut self, symbol: char) -> bool {
        if self.peek() == Some(&Token::Symbol(symbol)) {
            self.at += 1;
            true
        } else {
            false
        }
    }

    fn expect(&mut self, symbol: char) -> Result<(), String> {
        if self.eat(symbol) {
            Ok(())
        } else {
            Err(match self.peek() {
                Some(token) => format!("expected `{symbol}`, found {}", token.describe()),
                None => format!("expected `{symbol}` at the end"),
            })
        }
    }

    fn expression(&mut self, min_power: u8) -> Result<(), String> {
        self.enter()?;
        self.prefix()?;
        while let Some(&Token::Symbol(c)) = self.peek() {
            let Some((power, right, op)) = binary(c) else {
                break;
            };
            if power < min_power {
                break;
            }
            self.at += 1;
            self.expression(if right { power } else { power + 1 })?;
            self.ops.push(op);
        }
        self.leave();
        Ok(())
    }

    fn enter(&mut self) -> Result<(), String> {
        if self.ops.len() > 4096 {
            return Err("the expression is too long".into());
        }
        if self.depth >= NESTING {
            return Err("the expression is too deeply nested".into());
        }
        self.depth += 1;
        Ok(())
    }

    fn leave(&mut self) {
        self.depth -= 1;
    }

    fn prefix(&mut self) -> Result<(), String> {
        let token = self
            .tokens
            .get(self.at)
            .cloned()
            .ok_or_else(|| "the expression ends too soon".to_string())?;
        self.at += 1;
        match token {
            Token::Number(n) => self.ops.push(Op::Push(n)),
            Token::Symbol('-') => {
                self.expression(NEGATE)?;
                self.ops.push(Op::Neg);
            }
            Token::Symbol('+') => self.expression(NEGATE)?,
            Token::Symbol('(') => {
                self.expression(0)?;
                self.expect(')')?;
            }
            Token::Name(name) => {
                if self.peek() == Some(&Token::Symbol('(')) {
                    self.at += 1;
                    self.call(&name)?;
                } else {
                    self.ops.push(match name.as_str() {
                        "a" => Op::Var(0),
                        "b" => Op::Var(1),
                        "c" => Op::Var(2),
                        "d" => Op::Var(3),
                        "pi" => Op::Push(std::f32::consts::PI),
                        "tau" => Op::Push(std::f32::consts::TAU),
                        "e" => Op::Push(std::f32::consts::E),
                        _ => return Err(format!("unknown name `{name}` (inputs are a, b, c, d)")),
                    });
                }
            }
            other => return Err(format!("unexpected {}", other.describe())),
        }
        Ok(())
    }

    fn call(&mut self, name: &str) -> Result<(), String> {
        let unary: Option<fn(f32) -> f32> = match name {
            "sin" => Some(f32::sin),
            "cos" => Some(f32::cos),
            "tan" => Some(f32::tan),
            "tanh" => Some(f32::tanh),
            "abs" => Some(f32::abs),
            "sqrt" => Some(f32::sqrt),
            "exp" => Some(f32::exp),
            "ln" => Some(f32::ln),
            "log2" => Some(f32::log2),
            "log10" => Some(f32::log10),
            "floor" => Some(f32::floor),
            "ceil" => Some(f32::ceil),
            "round" => Some(f32::round),
            "fract" => Some(f32::fract),
            "sign" => Some(|x| if x == 0.0 { 0.0 } else { x.signum() }),
            _ => None,
        };
        let (arity, op) = match (unary, name) {
            (Some(f), _) => (1, Op::Unary(f)),
            (None, "min") => (2, Op::Min),
            (None, "max") => (2, Op::Max),
            (None, "pow") => (2, Op::Pow),
            (None, "atan2") => (2, Op::Atan2),
            (None, "clamp") => (3, Op::Clamp),
            (None, "mix") => (3, Op::Mix),
            _ => return Err(format!("unknown function `{name}`")),
        };
        for n in 0..arity {
            if n > 0 {
                self.expect(',')?;
            }
            self.expression(0)?;
        }
        if self.peek() == Some(&Token::Symbol(',')) {
            return Err(format!("`{name}` takes {arity} argument(s)"));
        }
        self.expect(')')?;
        self.ops.push(op);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use noodle_core::Value;
    use noodle_engine::Shape;
    use noodle_engine::testing::Harness;

    fn eval(text: &str, vars: [f32; 4]) -> f32 {
        Program::compile(text).unwrap().eval(vars)
    }

    fn at(text: &str) -> f32 {
        eval(text, [3.0, 4.0, 5.0, 6.0])
    }

    #[test]
    fn precedence_and_associativity() {
        assert_eq!(at("1 + 2 * 3"), 7.0);
        assert_eq!(at("(1 + 2) * 3"), 9.0);
        assert_eq!(at("2 ^ 3 ^ 2"), 512.0, "power is right to left");
        assert_eq!(at("-2 ^ 2"), -4.0, "minus binds looser than power");
        assert_eq!(at("2 ^ -1"), 0.5);
        assert_eq!(at("10 - 4 - 3"), 3.0, "subtraction is left to right");
        assert_eq!(at("12 / 4 / 3"), 1.0);
        assert_eq!(at("7 % 4"), 3.0);
        assert_eq!(at("- - 3"), 3.0);
        assert_eq!(at("2 * -3"), -6.0);
    }

    #[test]
    fn inputs_constants_and_numbers() {
        assert_eq!(at("a * b + c - d"), 11.0);
        assert!((at("pi") - std::f32::consts::PI).abs() < 1e-6);
        assert!((at("tau / 2") - std::f32::consts::PI).abs() < 1e-6);
        assert!((at("e") - std::f32::consts::E).abs() < 1e-6);
        assert_eq!(at("1e3"), 1000.0);
        assert_eq!(at("2.5e-1 + .5"), 0.75);
        assert_eq!(at("A + B"), 7.0, "names are not case sensitive");
    }

    #[test]
    fn functions() {
        assert_eq!(at("max(a, b)"), 4.0);
        assert_eq!(at("min(a, b)"), 3.0);
        assert_eq!(at("clamp(a * 10, 0, b)"), 4.0);
        assert_eq!(at("clamp(a, 5, 1)"), 1.0, "a backwards range doesn't panic");
        assert_eq!(at("mix(a, b, 0.5)"), 3.5);
        assert_eq!(at("abs(0 - a)"), 3.0);
        assert_eq!(at("sqrt(16) + floor(2.7) + ceil(2.1)"), 9.0);
        assert_eq!(at("sign(-5) + sign(0) + sign(2)"), 0.0);
        assert_eq!(at("pow(2, 10)"), 1024.0);
        assert!(at("sin(pi / 2)") > 0.999);
        assert!((at("exp(ln(a))") - 3.0).abs() < 1e-5);
        assert!((at("log2(8)") - 3.0).abs() < 1e-6);
        assert!(at("fract(2.25)") - 0.25 < 1e-6);
    }

    #[test]
    fn mistakes_are_reported() {
        for (text, wanted) in [
            ("a +", "ends too soon"),
            ("(a", "expected `)`"),
            ("a b", "unexpected"),
            ("a )", "unexpected"),
            ("foo", "unknown name"),
            ("foo(1)", "unknown function"),
            ("sin(1, 2)", "takes 1"),
            ("max(1)", "expected `,`"),
            ("a $ b", "unexpected `$`"),
            ("1.2.3", "isn't a number"),
        ] {
            let error = Program::compile(text)
                .err()
                .unwrap_or_else(|| panic!("{text}"));
            assert!(error.contains(wanted), "{text:?}: {error}");
        }
        // A hostile pile of brackets is an error, not a crash.
        let deep = "(".repeat(5000) + "a" + &")".repeat(5000);
        assert!(Program::compile(&deep).is_err());
        let long = vec!["a"; 5000].join("+");
        assert!(Program::compile(&long).is_err(), "too long is an error");
    }

    fn node(expression: &str) -> Config {
        let mut config = Config::new();
        config.set("expr", Value::Text(expression.into()));
        config
    }

    #[test]
    fn the_node_computes_its_expression_for_every_sample_and_voice() {
        let connected = [
            (MathPorts::A, Shape::new(2, 1)),
            (MathPorts::B, Shape::MONO),
        ];
        let mut h = Harness::new(&Math, &node("a * b + 1"), &connected, 48_000.0, 4).unwrap();
        let mut a = h.input(MathPorts::A, 4);
        a.lane_mut(0, 0).copy_from_slice(&[1.0, 2.0, 3.0, 4.0]);
        a.lane_mut(1, 0).fill(10.0);
        h.input(MathPorts::B, 4).fill(2.0);
        h.run(4).unwrap();
        let out = h.output(OUT);
        assert_eq!(out.shape(), Shape::new(2, 1));
        assert_eq!(out.lane(0, 0), &[3.0, 5.0, 7.0, 9.0]);
        assert_eq!(out.lane(1, 0), &[21.0; 4]);
    }

    #[test]
    fn unconnected_inputs_hold_their_values_and_an_empty_expression_passes_a() {
        let mut h = Harness::new(&Math, &node("a + b * 100"), &[], 48_000.0, 2).unwrap();
        h.set(MathPorts::A, 0.5);
        h.set(MathPorts::B, 2.0);
        h.run(2).unwrap();
        assert_eq!(h.output(OUT).lane(0, 0), &[200.5; 2]);

        let mut h = Harness::new(&Math, &Config::new(), &[], 48_000.0, 2).unwrap();
        h.set(MathPorts::A, 0.25);
        h.run(2).unwrap();
        assert_eq!(h.output(OUT).lane(0, 0), &[0.25; 2]);
    }

    #[test]
    fn a_bad_expression_fails_the_node_with_a_message() {
        let error = Math.layout(&node("a +")).err().unwrap();
        assert!(error.to_string().contains("Math"), "{error}");
    }
}
