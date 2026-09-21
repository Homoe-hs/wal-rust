// `#name` 分组符号必须是**单个 token**(见 grouped_symbol 注释), 而 token() 只接受
// 正则/字面量, 不接受规则引用 —— 所以把 base_symbol 的字符类提出来共用, 避免两处漂移。
const BASE_SYMBOL_RE = /[a-zA-Z_\.][=$\*\/>:\.\-_\?=%§^!\\~+<>|,\w]*/;

module.exports = grammar({
  name: 'wal',

  extras: $ => [$._comment, $.whitespace, $._line_comment],

  word: $ => $.base_symbol,

  rules: {
    program: $ => repeat1($.sexpr),

    sexpr: $ => choice(
      $.atom,
      $.list,
      $.quoted,
      $.quasiquoted,
      $.unquote,
      $.unquote_splice,
      $.timed_atom,
    ),

    timed_atom: $ => seq($.atom, "@", choice($.atom, $.list)),

    quoted: $ => seq("'", $.sexpr),
    quasiquoted: $ => seq("`", $.sexpr),
    unquote: $ => seq(",", $.sexpr),
    unquote_splice: $ => seq(",@", $.sexpr),

    whitespace: () => /[\t \r\n]+/,
    _comment: () => /;;.*/,
    _line_comment: () => /;.*/,

    atom: $ => choice(
      $.string,
      $.bool,
      $.operator,
      $.symbol,
      $.float,
      $.int,
    ),

    int: $ => choice($.dec_int, $.bin_int, $.hex_int),
    // 浮点: 支持科学计数法(1e3 / 1.5e3 / 1.5E-3)。
    // 词法器取"最长匹配", 所以 `1e3` 会整体匹配成 float, 不会被拆成 int(1)+symbol(e3)。
    float: () => choice(
      /[+-]?[0-9]+\.[0-9]+([eE][+-]?[0-9]+)?/,
      /[+-]?[0-9]+[eE][+-]?[0-9]+/,
    ),
    dec_int: () => /[+-]?[0-9]+/,
    bin_int: () => /0b[0-1]+/,
    hex_int: () => /0x[0-9a-fA-F]+/,

    // `#t`/`#f` 必须是**独立 token**: 否则 `#timeout`(合法的分组符号 `#name`)会被
    // 拆成 `#t` + `imeout`(实测)。prec 只用于与 `#t` 等长时的平局(`grouped_symbol`
    // 也是单 token, 更长的匹配按最长匹配规则优先)。
    bool: () => choice("true", "false", "#t", "#f", "nil"),

    operator: () => choice(
      "+", "-", "*", "/", "&&", "||", "=", "!=", ">", "<", ">=", "<=", "!", "**"
    ),

    symbol: $ => choice(
      $.base_symbol,
      $.scoped_symbol,
      $.grouped_symbol,
    ),

    scoped_symbol: $ => seq("~", $.base_symbol),
    // 单个 token: 让 `#timeout` 整体参与最长匹配(两个 token 的 `seq("#", ...)`
    // 只会先匹配 `#`, 于是输给关键字 `#t`)。
    grouped_symbol: $ => token(seq("#", BASE_SYMBOL_RE)),
    base_symbol: () => BASE_SYMBOL_RE,

    string: () => /"([^"\\]|\\.)*"/,

    list: $ => choice(
      seq("(", optional($.sexpr_list), ")"),
      seq("[", optional($.sexpr_list), "]"),
      seq("{", optional($.sexpr_list), "}"),
    ),

    sexpr_list: $ => repeat1($.sexpr),
  },
});
