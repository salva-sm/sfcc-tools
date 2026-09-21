/**
 * @file ISML (Salesforce B2C Commerce) grammar for tree-sitter
 * @license MIT
 *
 * Derived from tree-sitter/tree-sitter-html (MIT). On top of HTML it adds:
 *  - the ISML tag set, with the right void/container semantics;
 *  - `${ ... }` expressions in text, in attribute position, as an attribute
 *    value, and interleaved inside quoted attribute values;
 *  - ISML tags inside a tag's attribute list and inside quoted attribute
 *    values, both of which are everyday SFRA idioms:
 *      <form ... <isprint value="${form.attributes}"/>>
 *      <div class="a <isif condition="${x}">active</isif>">
 *  - `<isscript>` and `<iscomment>` raw text.
 */

/// <reference types="tree-sitter-cli/dsl" />
// @ts-check

module.exports = grammar({
  name: 'isml',

  extras: $ => [
    $.comment,
    /\s+/,
  ],

  externals: $ => [
    $._start_tag_name,
    $._script_start_tag_name,
    $._style_start_tag_name,
    $._isscript_start_tag_name,
    $._iscomment_start_tag_name,
    $._end_tag_name,
    $.erroneous_end_tag_name,
    '/>',
    $._implicit_end_tag,
    $.raw_text,
    $.comment,
    $.expression_content,
  ],

  rules: {
    document: $ => repeat($._node),

    doctype: $ => seq(
      '<!',
      alias($._doctype, 'doctype'),
      /[^>]+/,
      '>',
    ),

    _doctype: _ => /[Dd][Oo][Cc][Tt][Yy][Pp][Ee]/,

    _node: $ => choice(
      $.doctype,
      $.entity,
      $.isml_expression,
      $.text,
      $.element,
      $.script_element,
      $.style_element,
      $.isscript_element,
      $.iscomment_element,
      $.erroneous_end_tag,
    ),

    // `${ ... }` — an ISML expression. The body is scanned externally so that
    // nested braces, strings and comparison operators do not end it early.
    isml_expression: $ => seq(
      '${',
      optional($.expression_content),
      '}',
    ),

    element: $ => choice(
      seq(
        $.start_tag,
        repeat($._node),
        choice($.end_tag, $._implicit_end_tag),
      ),
      $.self_closing_tag,
    ),

    script_element: $ => seq(
      alias($.script_start_tag, $.start_tag),
      optional($.raw_text),
      $.end_tag,
    ),

    style_element: $ => seq(
      alias($.style_start_tag, $.start_tag),
      optional($.raw_text),
      $.end_tag,
    ),

    isscript_element: $ => seq(
      alias($.isscript_start_tag, $.start_tag),
      optional($.raw_text),
      $.end_tag,
    ),

    iscomment_element: $ => seq(
      alias($.iscomment_start_tag, $.start_tag),
      optional($.raw_text),
      $.end_tag,
    ),

    start_tag: $ => seq(
      '<',
      alias($._start_tag_name, $.tag_name),
      repeat($._tag_content),
      '>',
    ),

    script_start_tag: $ => seq(
      '<',
      alias($._script_start_tag_name, $.tag_name),
      repeat($._tag_content),
      '>',
    ),

    style_start_tag: $ => seq(
      '<',
      alias($._style_start_tag_name, $.tag_name),
      repeat($._tag_content),
      '>',
    ),

    isscript_start_tag: $ => seq(
      '<',
      alias($._isscript_start_tag_name, $.tag_name),
      repeat($._tag_content),
      '>',
    ),

    iscomment_start_tag: $ => seq(
      '<',
      alias($._iscomment_start_tag_name, $.tag_name),
      repeat($._tag_content),
      '>',
    ),

    self_closing_tag: $ => seq(
      '<',
      alias($._start_tag_name, $.tag_name),
      repeat($._tag_content),
      '/>',
    ),

    end_tag: $ => seq(
      '</',
      alias($._end_tag_name, $.tag_name),
      '>',
    ),

    erroneous_end_tag: $ => seq(
      '</',
      $.erroneous_end_tag_name,
      '>',
    ),

    // Where HTML only allows attributes, ISML also allows a bare expression
    // (`${obj.disabled ? 'disabled' : ''}`) and a whole ISML tag
    // (`<isprint value="${form.attributes}"/>`, `<isif ...>attr="v"</isif>`).
    _tag_content: $ => choice(
      $.attribute,
      $.isml_expression,
      $.element,
      $.iscomment_element,
      $.isscript_element,
    ),

    attribute: $ => seq(
      $.attribute_name,
      optional(seq(
        '=',
        choice(
          $.attribute_value,
          $.quoted_attribute_value,
          $.isml_expression,
        ),
      )),
    ),

    attribute_name: _ => /([^<>"'/=\s$]|\$[^{])+/,

    attribute_value: _ => /([^<>"'=\s$]|\$[^{])+/,

    // An entity can be named, numeric (decimal), or numeric (hexadecimal). The
    // longest entity name is 29 characters long, and the HTML spec says that
    // no more will ever be added.
    entity: _ => /&(#([xX][0-9a-fA-F]{1,6}|[0-9]{1,5})|[A-Za-z]{1,30});?/,

    quoted_attribute_value: $ => choice(
      seq('\'', repeat(choice(
        $.isml_expression,
        $.element,
        alias($._single_quoted_text, $.attribute_value),
        alias('$', $.attribute_value),
      )), '\''),
      seq('"', repeat(choice(
        $.isml_expression,
        $.element,
        alias($._double_quoted_text, $.attribute_value),
        alias('$', $.attribute_value),
      )), '"'),
    ),

    // Any run of characters that neither closes the value, opens an expression
    // nor opens a tag. A `$` not followed by `{` is ordinary text, matched
    // either by the second alternative or, at the very end, by the lone `$`
    // above — a trailing `\$?` here would swallow the `$` of a following `${`.
    _single_quoted_text: _ => token(prec(-1, /([^'$<]|\$[^{'<])+/)),
    _double_quoted_text: _ => token(prec(-1, /([^"$<]|\$[^{"<])+/)),

    text: _ => choice(
      /[^<>&\s$]([^<>&$]*[^<>&\s$])?/,
      '$',
      // A bare `&` that is not the start of an entity is plain text here,
      // unlike in tree-sitter-html where it is a parse error.
      '&',
    ),
  },
});
