//! Operator enum for WAL
//!
//! All 76 operators supported by WAL language.

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Operator {
    // Waveform operations
    Load,
    Unload,
    Step,
    EvalFile,
    Require,
    Repl,
    LoadedTraces,
    IsSignal,
    Signals,
    Index,
    MaxIndex,
    Ts,
    TraceName,
    TraceFile,

    // Math operations
    Add,
    Sub,
    Mul,
    Div,
    Exp,
    Floor,
    Ceil,
    Round,
    Abs,
    Mod,

    // Bitwise operations
    Bor,
    Band,
    Bxor,

    // Logical operations
    Not,
    Eq,
    Neq,
    Larger,
    Smaller,
    LargerEqual,
    SmallerEqual,
    And,
    Or,

    // Control flow
    Print,
    Printf,
    Set,
    Define,
    Let,
    If,
    Case,
    When,
    Unless,
    Cond,
    While,
    Do,
    Alias,
    Unalias,
    Exit,
    Fn,
    Defmacro,
    Macroexpand,
    Gensym,
    Type,

    // Special forms
    Quote,
    Quasiquote,
    Unquote,
    Eval,
    Parse,
    RelEval,
    Slice,
    Get,
    Call,
    Import,

    // List operations
    List,
    First,
    Second,
    Last,
    Rest,
    In,
    Map,
    Max,
    Min,
    Fold,
    Length,
    Average,
    Zip,
    Sum,
    Third,

    // Type checks
    IsDefined,
    IsAtom,
    IsSymbol,
    IsString,
    IsInt,
    IsList,
    IsNull,
    ConvertBinary,
    StringToInt,
    BitsToSint,
    StringToSymbol,
    SymbolToString,
    IntToString,

    // Signal operations
    Find,
    FindG,
    Whenever,
    FoldSignal,
    SignalWidth,
    SampleAt,
    TrimTrace,
    Count,
    Timeframe,

    // Scope/group operations
    Allscopes,
    Scoped,
    ResolveScope,
    Setscope,
    UnsetScope,
    Groups,
    InGroup,
    InGroups,
    InScope,
    InScopes,
    ResolveGroup,

    // Array operations
    Array,
    Seta,
    Geta,
    GetaDefault,
    Dela,
    Mapa,

    // Virtual signal operations
    Defsig,
    NewTrace,
    DumpTrace,

    // TileLink analysis
    TileLinkHandshakes,
    TileLinkLatency,
    TileLinkBandwidth,
}

impl Operator {
    pub fn from_str(s: &str) -> Option<Self> {
        match s {
            // Waveform
            "load" => Some(Operator::Load),
            "unload" => Some(Operator::Unload),
            "step" => Some(Operator::Step),
            "eval-file" => Some(Operator::EvalFile),
            "require" => Some(Operator::Require),
            "repl" => Some(Operator::Repl),
            "loaded-traces" => Some(Operator::LoadedTraces),
            "signal?" => Some(Operator::IsSignal),
            "signals" => Some(Operator::Signals),
            "index" => Some(Operator::Index),
            "max-index" => Some(Operator::MaxIndex),
            "ts" => Some(Operator::Ts),
            "trace-name" => Some(Operator::TraceName),
            "trace-file" => Some(Operator::TraceFile),

            // Math
            "+" => Some(Operator::Add),
            "-" => Some(Operator::Sub),
            "*" => Some(Operator::Mul),
            "/" => Some(Operator::Div),
            "**" => Some(Operator::Exp),
            "floor" => Some(Operator::Floor),
            "ceil" => Some(Operator::Ceil),
            "round" => Some(Operator::Round),
            "abs" => Some(Operator::Abs),
            "mod" => Some(Operator::Mod),

            // Bitwise
            "bor" => Some(Operator::Bor),
            "band" => Some(Operator::Band),
            "bxor" => Some(Operator::Bxor),

            // Logical
            "not" => Some(Operator::Not),
            "!" => Some(Operator::Not),
            "=" => Some(Operator::Eq),
            "!=" => Some(Operator::Neq),
            ">" => Some(Operator::Larger),
            "<" => Some(Operator::Smaller),
            ">=" => Some(Operator::LargerEqual),
            "<=" => Some(Operator::SmallerEqual),
            "&&" => Some(Operator::And),
            "||" => Some(Operator::Or),

            // Control flow
            "print" => Some(Operator::Print),
            "printf" => Some(Operator::Printf),
            "set" => Some(Operator::Set),
            "define" => Some(Operator::Define),
            "let" => Some(Operator::Let),
            "if" => Some(Operator::If),
            "case" => Some(Operator::Case),
            "when" => Some(Operator::When),
            "unless" => Some(Operator::Unless),
            "cond" => Some(Operator::Cond),
            "while" => Some(Operator::While),
            "do" => Some(Operator::Do),
            "alias" => Some(Operator::Alias),
            "unalias" => Some(Operator::Unalias),
            "exit" => Some(Operator::Exit),
            "fn" => Some(Operator::Fn),
            "lambda" => Some(Operator::Fn),
            "defmacro" => Some(Operator::Defmacro),
            "macroexpand" => Some(Operator::Macroexpand),
            "gensym" => Some(Operator::Gensym),
            "type" => Some(Operator::Type),

            // Special forms
            "quote" => Some(Operator::Quote),
            "quasiquote" => Some(Operator::Quasiquote),
            "unquote" => Some(Operator::Unquote),
            "eval" => Some(Operator::Eval),
            "parse" => Some(Operator::Parse),
            "rel_eval" | "@" => Some(Operator::RelEval),
            "slice" => Some(Operator::Slice),
            "get" => Some(Operator::Get),
            "call" => Some(Operator::Call),
            "import" => Some(Operator::Import),

            // List
            "list" => Some(Operator::List),
            "first" => Some(Operator::First),
            "second" => Some(Operator::Second),
            "last" => Some(Operator::Last),
            "rest" => Some(Operator::Rest),
            "in" => Some(Operator::In),
            "map" => Some(Operator::Map),
            "max" => Some(Operator::Max),
            "min" => Some(Operator::Min),
            "fold" => Some(Operator::Fold),
            "length" => Some(Operator::Length),
            "average" => Some(Operator::Average),
            "zip" => Some(Operator::Zip),
            "sum" => Some(Operator::Sum),
            "third" => Some(Operator::Third),

            // Type checks
            "defined?" => Some(Operator::IsDefined),
            "atom?" => Some(Operator::IsAtom),
            "symbol?" => Some(Operator::IsSymbol),
            "string?" => Some(Operator::IsString),
            "int?" => Some(Operator::IsInt),
            "list?" => Some(Operator::IsList),
            "null?" => Some(Operator::IsNull),
            "empty?" => Some(Operator::IsNull),
            "convert/bin" => Some(Operator::ConvertBinary),
            "string->int" => Some(Operator::StringToInt),
            "bits->sint" => Some(Operator::BitsToSint),
            "symbol->string" => Some(Operator::SymbolToString),
            "string->symbol" => Some(Operator::StringToSymbol),
            "int->string" => Some(Operator::IntToString),

            // Signal operations
            "find" => Some(Operator::Find),
            "find/g" => Some(Operator::FindG),
            "whenever" => Some(Operator::Whenever),
            "fold/signal" => Some(Operator::FoldSignal),
            "signal-width" => Some(Operator::SignalWidth),
            "sample-at" => Some(Operator::SampleAt),
            "trim-trace" => Some(Operator::TrimTrace),
            "count" => Some(Operator::Count),
            "timeframe" => Some(Operator::Timeframe),

            // Scope/group
            "all-scopes" => Some(Operator::Allscopes),
            "scoped" => Some(Operator::Scoped),
            "resolve-scope" => Some(Operator::ResolveScope),
            "set-scope" => Some(Operator::Setscope),
            "unset-scope" => Some(Operator::UnsetScope),
            "groups" => Some(Operator::Groups),
            "in-group" => Some(Operator::InGroup),
            "in-groups" => Some(Operator::InGroups),
            "in-scope" => Some(Operator::InScope),
            "in-scopes" => Some(Operator::InScopes),
            "resolve-group" => Some(Operator::ResolveGroup),

            // Array
            "array" => Some(Operator::Array),
            "seta" => Some(Operator::Seta),
            "geta" => Some(Operator::Geta),
            "geta/default" => Some(Operator::GetaDefault),
            "dela" => Some(Operator::Dela),
            "mapa" => Some(Operator::Mapa),

// Virtual
            "defsig" => Some(Operator::Defsig),
            "new-trace" => Some(Operator::NewTrace),
            "dump-trace" => Some(Operator::DumpTrace),

            // TileLink analysis
            "tl-handshakes" => Some(Operator::TileLinkHandshakes),
            "tl-latency" => Some(Operator::TileLinkLatency),
            "tl-bandwidth" => Some(Operator::TileLinkBandwidth),

            _ => None,
        }
    }

    pub fn is_special_form(&self) -> bool {
        matches!(
            self,
            Operator::Define
                | Operator::Set
                | Operator::Let
                | Operator::If
                | Operator::While
                | Operator::Fn
                | Operator::Defmacro
                | Operator::Quote
                | Operator::Quasiquote
                | Operator::Unquote
                | Operator::Do
                | Operator::Case
        )
    }

    pub fn as_str(&self) -> &'static str {
        match self {
            Operator::Load => "load",
            Operator::Unload => "unload",
            Operator::Step => "step",
            Operator::EvalFile => "eval-file",
            Operator::Require => "require",
            Operator::Repl => "repl",
            Operator::LoadedTraces => "loaded-traces",
            Operator::IsSignal => "signal?",
            Operator::Signals => "signals",
            Operator::Index => "index",
            Operator::MaxIndex => "max-index",
            Operator::Ts => "ts",
            Operator::TraceName => "trace-name",
            Operator::TraceFile => "trace-file",
            Operator::Add => "+",
            Operator::Sub => "-",
            Operator::Mul => "*",
            Operator::Div => "/",
            Operator::Exp => "**",
            Operator::Floor => "floor",
            Operator::Ceil => "ceil",
            Operator::Round => "round",
            Operator::Abs => "abs",
            Operator::Mod => "mod",
            Operator::Bor => "bor",
            Operator::Band => "band",
            Operator::Bxor => "bxor",
            Operator::Not => "not",
            Operator::Eq => "=",
            Operator::Neq => "!=",
            Operator::Larger => ">",
            Operator::Smaller => "<",
            Operator::LargerEqual => ">=",
            Operator::SmallerEqual => "<=",
            Operator::And => "&&",
            Operator::Or => "||",
            Operator::Print => "print",
            Operator::Printf => "printf",
            Operator::Set => "set",
            Operator::Define => "define",
            Operator::Let => "let",
            Operator::If => "if",
            Operator::Case => "case",
            Operator::When => "when",
            Operator::Unless => "unless",
            Operator::Cond => "cond",
            Operator::While => "while",
            Operator::Do => "do",
            Operator::Alias => "alias",
            Operator::Unalias => "unalias",
            Operator::Exit => "exit",
            Operator::Fn => "fn",
            Operator::Defmacro => "defmacro",
            Operator::Macroexpand => "macroexpand",
            Operator::Gensym => "gensym",
            Operator::Type => "type",
            Operator::Quote => "quote",
            Operator::Quasiquote => "quasiquote",
            Operator::Unquote => "unquote",
            Operator::Eval => "eval",
            Operator::Parse => "parse",
            Operator::RelEval => "rel_eval",
            Operator::Slice => "slice",
            Operator::Get => "get",
            Operator::Call => "call",
            Operator::Import => "import",
            Operator::List => "list",
            Operator::First => "first",
            Operator::Second => "second",
            Operator::Last => "last",
            Operator::Rest => "rest",
            Operator::In => "in",
            Operator::Map => "map",
            Operator::Max => "max",
            Operator::Min => "min",
            Operator::Fold => "fold",
            Operator::Length => "length",
            Operator::Average => "average",
            Operator::Zip => "zip",
            Operator::Sum => "sum",
            Operator::Third => "third",
            Operator::IsDefined => "defined?",
            Operator::IsAtom => "atom?",
            Operator::IsSymbol => "symbol?",
            Operator::IsString => "string?",
            Operator::IsInt => "int?",
            Operator::IsList => "list?",
            Operator::IsNull => "null?",
            Operator::ConvertBinary => "convert/bin",
            Operator::StringToInt => "string->int",
            Operator::BitsToSint => "bits->sint",
            Operator::StringToSymbol => "string->symbol",
            Operator::SymbolToString => "symbol->string",
            Operator::IntToString => "int->string",
            Operator::Find => "find",
            Operator::FindG => "find/g",
            Operator::Whenever => "whenever",
            Operator::FoldSignal => "fold/signal",
            Operator::SignalWidth => "signal-width",
            Operator::SampleAt => "sample-at",
            Operator::TrimTrace => "trim-trace",
            Operator::Count => "count",
            Operator::Timeframe => "timeframe",
            Operator::Allscopes => "all-scopes",
            Operator::Scoped => "scoped",
            Operator::ResolveScope => "resolve-scope",
            Operator::Setscope => "set-scope",
            Operator::UnsetScope => "unset-scope",
            Operator::Groups => "groups",
            Operator::InGroup => "in-group",
            Operator::InGroups => "in-groups",
            Operator::InScope => "in-scope",
            Operator::InScopes => "in-scopes",
            Operator::ResolveGroup => "resolve-group",
            Operator::Array => "array",
            Operator::Seta => "seta",
            Operator::Geta => "geta",
            Operator::GetaDefault => "geta/default",
            Operator::Dela => "dela",
            Operator::Mapa => "mapa",
            Operator::Defsig => "defsig",
            Operator::NewTrace => "new-trace",
            Operator::DumpTrace => "dump-trace",
            Operator::TileLinkHandshakes => "tl-handshakes",
            Operator::TileLinkLatency => "tl-latency",
            Operator::TileLinkBandwidth => "tl-bandwidth",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_operator_from_str() {
        assert_eq!(Operator::from_str("+"), Some(Operator::Add));
        assert_eq!(Operator::from_str("define"), Some(Operator::Define));
        assert_eq!(Operator::from_str("invalid"), None);
    }

    #[test]
    fn test_operator_as_str() {
        assert_eq!(Operator::Add.as_str(), "+");
        assert_eq!(Operator::Define.as_str(), "define");
    }
}