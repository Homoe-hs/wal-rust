module.exports = grammar({
  name: 'wal',

  extras: $ => [$._comment, $.whitespace],

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

    timed_atom: $ => seq($.atom, "@", $.atom),

    quoted: $ => seq("'", $.sexpr),
    quasiquoted: $ => seq("`", $.sexpr),
    unquote: $ => seq(",", $.sexpr),
    unquote_splice: $ => seq(",@", $.sexpr),

    whitespace: () => /[\t \r\n]+/,
    _comment: () => /;;.*/,

    atom: $ => choice(
      $.string,
      $.bool,
      $.operator,
      $.symbol,
      $.float,
      $.int,
    ),

    int: $ => choice($.dec_int, $.bin_int, $.hex_int),
    float: () => /[+-]?[0-9]+\.[0-9]+/,
    dec_int: () => /[+-]?[0-9]+/,
    bin_int: () => /0b[0-1]+/,
    hex_int: () => /0x[0-9a-fA-F]+/,

    bool: () => choice("true", "false"),

    operator: () => choice(
      "+", "-", "*", "/", "&&", "||", "=", "!=", ">", "<", ">=", "<=", "!", "**"
    ),

    symbol: $ => choice(
      $.base_symbol,
      $.scoped_symbol,
      $.grouped_symbol,
    ),

    scoped_symbol: $ => seq("~", $.base_symbol),
    grouped_symbol: $ => seq("#", $.base_symbol),
    base_symbol: () => /[a-zA-Z_\.][=$\*\/>:\.\-_\?=%§^!\\~+<>|,\w]*/,

    string: () => /"[^"]*"/,

    list: $ => choice(
      seq("(", optional($.sexpr_list), ")"),
      seq("[", optional($.sexpr_list), "]"),
      seq("{", optional($.sexpr_list), "}"),
    ),

    sexpr_list: $ => repeat1($.sexpr),
  },
});
