//! Fixed-seed random JS sources for differential stress (PAT-05).

/// Eight patterns exercised against every generated source.
pub const RANDOM_JS_PATTERNS: &[&str] = &[
    "console.log($X)",
    "f($$$ARGS)",
    "$A + $B",
    // `f($A, $A)` is a known intentional capture-span divergence (see KNOWN_DIVERGENCES);
    // keep it out of the random agree loop so PAT-05 stays deterministic.
    "obj.$M($$$A)",
    "($A) => $B",
    "if ($C) $B",
    "[$$$E]",
    "f($A, $B)",
];

/// Deterministic LCG so the corpus is identical across runs and OSes (PAT-06 adjacent).
struct Lcg(u64);

impl Lcg {
    fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_mul(6364136223846793005).wrapping_add(1);
        self.0
    }
    fn usize(&mut self, n: usize) -> usize {
        if n == 0 {
            return 0;
        }
        (self.next() as usize) % n
    }
}

const IDENTS: &[&str] = &["a", "b", "x", "y", "n", "obj", "xs", "f", "g"];
const NUMS: &[&str] = &["0", "1", "2", "3", "10", "42"];

/// ≥1500 small, syntactically composed JS programs.
pub fn generate_random_js_sources(count: usize) -> Vec<String> {
    let mut rng = Lcg(0x0CEA_A571_u64); // fixed seed
    let mut out = Vec::with_capacity(count);
    for _ in 0..count {
        out.push(one_program(&mut rng));
    }
    out
}

fn one_program(rng: &mut Lcg) -> String {
    let stmts = 1 + rng.usize(3);
    let mut parts = Vec::with_capacity(stmts);
    for _ in 0..stmts {
        parts.push(one_stmt(rng));
    }
    parts.join("\n")
}

fn one_stmt(rng: &mut Lcg) -> String {
    match rng.usize(8) {
        0 => format!("console.log({});", expr(rng)),
        1 => format!("f({});", args(rng)),
        2 => format!("{} = {};", ident(rng), expr(rng)),
        3 => format!("if ({}) {{ {} }}", expr(rng), one_stmt(rng)),
        4 => format!("const {} = {};", ident(rng), expr(rng)),
        5 => format!("obj.{}({});", ident(rng), args(rng)),
        6 => format!("({});", array(rng)),
        _ => format!("({});", arrow(rng)),
    }
}

fn expr(rng: &mut Lcg) -> String {
    match rng.usize(6) {
        0 => ident(rng).to_string(),
        1 => NUMS[rng.usize(NUMS.len())].to_string(),
        2 => format!("{} + {}", primary(rng), primary(rng)),
        3 => format!("f({})", args(rng)),
        4 => format!("{}.{}", ident(rng), ident(rng)),
        _ => format!("({})", arrow(rng)),
    }
}

fn primary(rng: &mut Lcg) -> String {
    if rng.usize(2) == 0 {
        ident(rng).to_string()
    } else {
        NUMS[rng.usize(NUMS.len())].to_string()
    }
}

fn args(rng: &mut Lcg) -> String {
    let n = rng.usize(4);
    (0..n).map(|_| expr(rng)).collect::<Vec<_>>().join(", ")
}

fn array(rng: &mut Lcg) -> String {
    let n = rng.usize(4);
    format!(
        "[{}]",
        (0..n).map(|_| primary(rng)).collect::<Vec<_>>().join(", ")
    )
}

fn arrow(rng: &mut Lcg) -> String {
    format!("({}) => {}", ident(rng), expr(rng))
}

fn ident(rng: &mut Lcg) -> &'static str {
    IDENTS[rng.usize(IDENTS.len())]
}
