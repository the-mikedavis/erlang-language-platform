/*
 * Copyright (c) Meta Platforms, Inc. and affiliates.
 *
 * This source code is licensed under both the MIT license found in the
 * LICENSE-MIT file in the root directory of this source tree and the Apache
 * License, Version 2.0 found in the LICENSE-APACHE file in the root directory
 * of this source tree.
 */

use elp_ide_db::elp_base_db::fixture;
use elp_ide_db::elp_base_db::fixture::RangeOrOffset;
use elp_ide_db::elp_base_db::fixture::WithFixture;
use elp_ide_db::elp_base_db::FileId;
use elp_ide_db::elp_base_db::FilePosition;
use elp_ide_db::elp_base_db::FileRange;
use elp_ide_db::RootDatabase;
use expect_test::expect;
use expect_test::Expect;
use hir::Semantic;

use crate::MatchFinder;
use crate::SsrRule;

#[track_caller]
fn parse_error_text(query: &str) -> String {
    let (mut db, _file_id) = RootDatabase::with_single_file(&query);
    let pattern = SsrRule::parse_str(&mut db, query);
    format!("{}", pattern.unwrap_err())
}

#[track_caller]
fn parse_good_text(query: &str, expect: Expect) {
    let (mut db, _file_id) = RootDatabase::with_single_file(&query);
    let pattern = SsrRule::parse_str(&mut db, query);
    let actual = pattern.unwrap().tree_print(&db);
    expect.assert_eq(actual.as_str());
}

#[test]
fn parser_empty_query() {
    assert_eq!(parse_error_text(""), "Parse error: Could not lower rule");
}

#[test]
fn parser_basic_query() {
    parse_good_text(
        "ssr: V ==>> V + 1.",
        expect![[r#"

            SsrBody {
                lhs
                    Expr::Var(V)
                rhs
                    Expr::BinaryOp {
                        lhs
                            Expr::Var(V)
                        rhs
                            Literal(Integer(1))
                        op
                            ArithOp(Add),
                    }
                when
            }
        "#]],
    );
}

#[test]
fn parser_basic_query_with_placeholder() {
    parse_good_text(
        "ssr: _@V ==>> _@V + 1.",
        expect![[r#"

            SsrBody {
                lhs
                    SsrPlaceholder {var: _@V, conditions: TBD}
                rhs
                    Expr::BinaryOp {
                        lhs
                            SsrPlaceholder {var: _@V, conditions: TBD}
                        rhs
                            Literal(Integer(1))
                        op
                            ArithOp(Add),
                    }
                when
            }
        "#]],
    );
}

#[test]
fn parser_basic_query_with_cond() {
    parse_good_text(
        "ssr: V ==>> V + 1
              when is_atom(V)
         .
        ",
        expect![[r#"

            SsrBody {
                lhs
                    Expr::Var(V)
                rhs
                    Expr::BinaryOp {
                        lhs
                            Expr::Var(V)
                        rhs
                            Literal(Integer(1))
                        op
                            ArithOp(Add),
                    }
                when
                    guard
                        Expr::Call {
                            target
                                CallTarget::Remote {
                                    Literal(Atom('erlang'))
                                    Literal(Atom('is_atom'))
                                }
                            args
                                Expr::Var(V),
                        },
            }
        "#]],
    );
}

// ---------------------------------------------------------------------

/// `code` may optionally contain a cursor marker `~`. If it doesn't,
/// then the position will be the start of the file. If there's a
/// second cursor marker, then we'll return a single range.
pub(crate) fn single_file(code: &str) -> (RootDatabase, FilePosition, Vec<FileRange>) {
    let (db, file_id, range_or_offset) = if code.contains(fixture::CURSOR_MARKER) {
        RootDatabase::with_range_or_offset(code)
    } else {
        let (db, file_id) = RootDatabase::with_single_file(code);
        (db, file_id, RangeOrOffset::Offset(0.into()))
    };
    let selections;
    let position;
    match range_or_offset {
        RangeOrOffset::Range(range) => {
            position = FilePosition {
                file_id,
                offset: range.start(),
            };
            selections = vec![FileRange { file_id, range }];
        }
        RangeOrOffset::Offset(offset) => {
            position = FilePosition { file_id, offset };
            selections = vec![];
        }
    }
    (db, position, selections)
}

fn print_match_debug_info(match_finder: &MatchFinder<'_>, file_id: FileId, snippet: &str) {
    let debug_info = match_finder.debug_where_text_equal(file_id, snippet);
    println!(
        "Match debug info: {} nodes had text exactly equal to '{}'",
        debug_info.len(),
        snippet
    );
    for (index, d) in debug_info.iter().enumerate() {
        println!("Node #{index}\n{d:#?}\n");
    }
}

#[track_caller]
fn assert_matches(pattern: &str, code: &str, expected: &[&str]) {
    let (db, position, selections) = single_file(code);
    if expected.len() > 0 {
        if expected[0] == "" {
            panic!("empty expected string");
        }
    }
    let sema = Semantic::new(&db);
    let pattern = SsrRule::parse_str(sema.db, pattern).unwrap();
    let mut match_finder = MatchFinder::in_context(&sema, position.file_id, selections).unwrap();
    match_finder.add_search_pattern(pattern).unwrap();
    let matched_strings: Vec<String> = match_finder
        .matches()
        .flattened()
        .matches
        .iter()
        .map(|m| m.matched_text(&db))
        .collect();
    if matched_strings != expected && !expected.is_empty() {
        print_match_debug_info(&match_finder, position.file_id, expected[0]);
    }
    assert_eq!(matched_strings, expected);
}

// ---------------------------------------------------------------------

#[test]
fn ssr_let_stmt_in_fn_match_1() {
    assert_matches("ssr: _@A = 10.", "foo() -> X = 10, X.", &["X = 10"]);
}

#[test]
fn ssr_let_stmt_in_fn_match_2() {
    assert_matches("ssr: _@A = _@B.", "foo() -> X = 10, X.", &["X = 10"]);
}

#[test]
fn ssr_block_expr_match_1() {
    assert_matches(
        "ssr: begin _@A = _@B end.",
        "fon() -> begin X = 10 end.",
        &["begin X = 10 end"],
    );
}

#[test]
fn ssr_block_expr_match_2() {
    assert_matches(
        "ssr: begin _@A = _@B, _@C end.",
        "foo() -> begin X = 10, X end.",
        &["begin X = 10, X end"],
    );
}

#[test]
fn ssr_block_expr_match_multiple_statements() {
    assert_matches(
        "ssr: begin _@A = _@B, _@C, _@D end.",
        "foo() -> begin X = 10, Y = 20, Z = X + Y end.",
        &["begin X = 10, Y = 20, Z = X + Y end"],
    );
}

#[test]
fn ssr_expr_match_tuple() {
    assert_matches(
        "ssr: {foo, _@A, _@B, _@C, _@D}.",
        "fn() -> X = {foo, a, b, c, d}, X.",
        &["{foo, a, b, c, d}"],
    );
}

#[test]
fn ssr_expr_match_tuple_nested() {
    assert_matches(
        "ssr: {foo, {foo, 1}}.",
        "fn() -> X = {foo, {foo, 1}}.",
        &["{foo, {foo, 1}}"],
    );
    assert_matches(
        "ssr: {foo, _@A}.",
        "fn() -> X = {foo, {foo, 1}}.",
        &["{foo, {foo, 1}}", "{foo, 1}"],
    );
}

#[test]
fn ssr_record_expr_match() {
    assert_matches(
        "ssr: #foo{k1 = _@A, k2 = _@B, k3 = _@C}.",
        "fn() -> X = #foo{k1 = a, k2 = <<\"blah\">>, k3 = {c, d}}, X.",
        &["#foo{k1 = a, k2 = <<\"blah\">>, k3 = {c, d}}"],
    );
}

#[test]
fn ssr_record_expr_match_5() {
    assert_matches(
        "ssr: #foo{k1 = _@A, k2 = _@B, k3 = _@C}.",
        "fn() -> X = #foo{k2 = a, k3 = <<\"blah\">>, k1 = {c, d}}, X.",
        &["#foo{k2 = a, k3 = <<\"blah\">>, k1 = {c, d}}"],
    );
}

#[test]
fn ssr_record_expr_match_6() {
    // Note: HIR record only stores atom field names, so will silently
    // discard the placeholder. This will be fixed later in the stack.
    assert_matches(
        "ssr: #foo{_@K = _@A, k2 = _@B, k3 = _@C}.",
        "fn() -> X = #foo{k1 = a, k2 = <<\"blah\">>, k3 = {c, d}}, X.",
        &[],
    );
}

#[test]
fn ssr_record_expr_match_record() {
    assert_matches(
        "ssr: #foo{k1 = _@A, k2 = _@B, k3 = _@C}.",
        "fn() -> X = #boo{k1 = a, k2 = <<\"blah\">>, k3 = {c, d}}, X.",
        &[],
    );
    assert_matches(
        "ssr: #foo{k1 = _@A, k2 = _@B, k3 = _@C}.",
        "fn() -> X = #foo{ka1 = a, ka2 = <<\"blah\">>, ka3 = {c, d}}, X.",
        &[],
    );
    assert_matches(
        "ssr: #foo{k1 = _@A, k2 = _@B, k3 = _@C}.",
        "fn() -> X = #foo{k1 = a, k2 = <<\"blah\">>, k3 = {c, d}}, X.",
        &["#foo{k1 = a, k2 = <<\"blah\">>, k3 = {c, d}}"],
    );
}

#[test]
#[ignore]
fn ssr_record_expr_match_record_subset() {
    // Note: this test currently fails.
    // We need to extend the syntax to be able to say there are
    // possibly don't care extra fields.
    assert_matches(
        "ssr: #foo{k1 = _@A, k2 = _@B}.",
        "fn() -> X = #foo{k1 = a, k2 = <<\"blah\">>, k3 = {c, d}}, X.",
        &["#foo{k1 = a, k2 = <<\"blah\">>, k3 = {c, d}}"],
    );
}

#[test]
fn ssr_record_expr_match_unordered() {
    assert_matches(
        "ssr: #foo{k1 = _@A, k2 = _@B, k3 = _@C}.",
        "fn() -> X = #foo{k2 = a, k3 = <<\"blah\">>, k1 = {c, d}}, X.",
        &["#foo{k2 = a, k3 = <<\"blah\">>, k1 = {c, d}}"],
    );
}

#[test]
fn ssr_record_expr_match_rhs() {
    assert_matches(
        "ssr: #foo{k1 = 3, k2 = {_@A, _@B}, k3 = _@C}.",
        "fn() -> X = #foo{k1 = a, k3 = <<\"blah\">>, k2 = {c, d}}, X.",
        &[],
    );
    assert_matches(
        "ssr: #foo{k1 = 3, k2 = {_@A, _@B}, k3 = _@C}.",
        "fn() -> X = #foo{k1 = 3, k3 = <<\"blah\">>, k2 = {c, d}}, X.",
        &["#foo{k1 = 3, k3 = <<\"blah\">>, k2 = {c, d}}"],
    );
}

#[test]
fn ssr_expr_match_list() {
    assert_matches(
        "ssr: [ _@A, _@B | _@C].",
        "fn(Y) -> X = [1, 2 | [Y]].",
        &["[1, 2 | [Y]]"],
    );
}

#[test]
fn ssr_expr_match_list_match_pipe() {
    assert_matches("ssr: [ _@A, _@B | _@C].", "fn(Y) -> X = [1, 2, [Y]].", &[]);
}

#[test]
fn ssr_expr_match_binary() {
    assert_matches(
        "ssr: << _@A, _@B>>.",
        "fn(Y) -> X=1, <<X,Y>>.",
        &["<<X,Y>>"],
    );
}

#[test]
fn ssr_expr_match_unary_op() {
    assert_matches("ssr: not _@A.", "fn(Y) -> not Y.", &["not Y"]);
    assert_matches("ssr: bnot _@A.", "fn(Y) -> not Y.", &[]);
    assert_matches("ssr: bnot _@A.", "fn(Y) -> bnot Y.", &["bnot Y"]);
    // Note: it is an AST, not textual match
    assert_matches("ssr: + _@A.", "fn(Y) -> +Y.", &["+Y"]);
    assert_matches("ssr: -_@A.", "fn(Y) -> -Y.", &["-Y"]);
}

#[test]
fn ssr_expr_binary_op() {
    assert_matches("ssr: _@A + _@B.", "fn(X) -> Y = {X + 1}, Y.", &["X + 1"]);
    assert_matches("ssr: _@A - _@B.", "fn(X) -> Y = {X + 1}, Y.", &[]);
    assert_matches("ssr: _@A and _@B.", "fn(X,Y) -> X and Y .", &["X and Y"]);
    assert_matches(
        "ssr: _@A andalso _@B.",
        "fn(X,Y) -> X andalso Y .",
        &["X andalso Y"],
    );
    assert_matches("ssr: _@A or _@B.", "fn(X,Y) -> X or Y .", &["X or Y"]);
    assert_matches(
        "ssr: _@A orelse _@B.",
        "fn(X,Y) -> X orelse Y .",
        &["X orelse Y"],
    );
    assert_matches("ssr: _@A ! _@B.", "fn(X,Y) -> X ! Y .", &["X ! Y"]);
    // Comparison operators
    assert_matches("ssr: _@A == _@B.", "fn(X,Y) -> X == Y .", &["X == Y"]);
    assert_matches("ssr: _@A /= _@B.", "fn(X,Y) -> X /= Y .", &["X /= Y"]);
    assert_matches("ssr: _@A =< _@B.", "fn(X,Y) -> X =< Y .", &["X =< Y"]);
    assert_matches("ssr: _@A  < _@B.", "fn(X,Y) -> X  < Y .", &["X  < Y"]);
    assert_matches("ssr: _@A >= _@B.", "fn(X,Y) -> X >= Y .", &["X >= Y"]);
    assert_matches("ssr: _@A >  _@B.", "fn(X,Y) -> X >  Y .", &["X >  Y"]);
    assert_matches("ssr: _@A =:= _@B.", "fn(X,Y) -> X =:= Y .", &["X =:= Y"]);
    assert_matches("ssr: _@A =/= _@B.", "fn(X,Y) -> X =/= Y .", &["X =/= Y"]);
    // List operators
    assert_matches("ssr: _@A ++ _@B.", "fn(X,Y) -> X ++ Y .", &["X ++ Y"]);
    assert_matches("ssr: _@A -- _@B.", "fn(X,Y) -> X -- Y .", &["X -- Y"]);
    // Add operators
    assert_matches("ssr: _@A + _@B.", "fn(X,Y) -> X + Y .", &["X + Y"]);
    assert_matches("ssr: _@A - _@B.", "fn(X,Y) -> X - Y .", &["X - Y"]);
    assert_matches("ssr: _@A bor _@B.", "fn(X,Y) -> X bor Y .", &["X bor Y"]);
    assert_matches("ssr: _@A bxor _@B.", "fn(X,Y) -> X bxor Y .", &["X bxor Y"]);
    assert_matches("ssr: _@A bsl _@B.", "fn(X,Y) -> X bsl Y .", &["X bsl Y"]);
    assert_matches("ssr: _@A bsr _@B.", "fn(X,Y) -> X bsr Y .", &["X bsr Y"]);
    assert_matches("ssr: _@A or _@B.", "fn(X,Y) -> X or Y .", &["X or Y"]);
    // Mult operators
    assert_matches("ssr: _@A / _@B.", "fn(X,Y) -> X / Y .", &["X / Y"]);
    assert_matches("ssr: _@A * _@B.", "fn(X,Y) -> X * Y .", &["X * Y"]);
    assert_matches("ssr: _@A div _@B.", "fn(X,Y) -> X div Y .", &["X div Y"]);
    assert_matches("ssr: _@A rem _@B.", "fn(X,Y) -> X rem Y .", &["X rem Y"]);
    assert_matches("ssr: _@A band _@B.", "fn(X,Y) -> X band Y .", &["X band Y"]);
    assert_matches("ssr: _@A and _@B.", "fn(X,Y) -> X and Y .", &["X and Y"]);
}

#[test]
fn ssr_expr_match_record_update() {
    assert_matches(
        "ssr: _@A#a_record{a_field = _@B}.",
        "bar(List) -> XX = 1, List#record{field = XX}.",
        &[],
    );
    assert_matches(
        "ssr: _@A#a_record{field = _@B}.",
        "bar(List) -> XX = 1, List#a_record{field = XX}.",
        &["List#a_record{field = XX}"],
    );
}

#[test]
fn ssr_expr_match_record_index() {
    assert_matches(
        "ssr: #a_record.a_field.",
        "bar(List) -> XX = #record.field, XX.",
        &[],
    );
    assert_matches(
        "ssr: #a_record.a_field.",
        "bar(List) -> XX = #a_record.a_field, XX.",
        &["#a_record.a_field"],
    );
}

#[test]
fn ssr_expr_match_record_field() {
    assert_matches(
        "ssr: _@A#a_record.a_field.",
        "bar(List) -> XX = List#record.field, XX.",
        &[],
    );
    assert_matches(
        "ssr: _@A#record.field.",
        "bar(List) -> XX = List#record.field, XX.",
        &["List#record.field"],
    );
}

#[test]
fn ssr_expr_match_map() {
    // Note that the map operation is always Assoc (`=>`), as per the
    // HIR lowering
    assert_matches(
        "ssr: #{ field => _@A }.",
        "bar() -> XX = 1, #{foo => XX}.",
        &[],
    );
    assert_matches(
        "ssr: #{ field => _@A }.",
        "bar() -> XX = 1, #{field => XX}.",
        &["#{field => XX}"],
    );
    assert_matches(
        "ssr: #{ field => _@A, another => _@B }.",
        "bar() -> XX = 1, #{another => 3, field => XX}.",
        &["#{another => 3, field => XX}"],
    );
}

#[test]
fn ssr_expr_match_map_update() {
    assert_matches(
        "ssr: _@A#{ foo => _@B }.",
        "bar(List) -> XX = 1, List#{foo := XX}.",
        &[],
    );
    assert_matches(
        "ssr: _@A#{ foo => _@B }.",
        "bar(List) -> XX = 1, List#{foo => XX}.",
        &["List#{foo => XX}"],
    );
    assert_matches(
        "ssr: _@A#{ foo => _@B, zz => _@A }.",
        "bar(List) -> XX = 1, List#{zz => 1, foo => XX}.",
        &["List#{zz => 1, foo => XX}"],
    );
    assert_matches(
        "ssr: _@A#{ foo => _@B, zz => {_@A} }.",
        "bar(List) -> XX = 1, List#{zz => 1, foo => XX}.",
        &[],
    );
}

#[test]
fn ssr_expr_match_catch() {
    assert_matches(
        "ssr: catch _@A.",
        "bar() -> XX = 1, catch XX.",
        &["catch XX"],
    );
}

#[test]
fn ssr_expr_match_macro_call() {
    // TODO: fails because we do not have a visible macro call in the
    // template, only Missing
    // And it comes down to having some sort of meaningful fold option
    // that gives the surface call, and the expansion. Maybe try both?
    // assert_matches(
    //     "ssr: ?ANY_MACRO(_@AA).",
    //     "-define(BAR(X), {X}).
    //      bar() -> ?BAR(4).",
    //     &["broken"],
    // );
}

#[test]
fn ssr_expr_list_comprehension() {
    assert_matches(
        "ssr: [XX || XX <- _@List, _@Cond].",
        "bar() -> XX = 1, [XX || XX <- List, XX >= 5].",
        &["[XX || XX <- List, XX >= 5]"],
    );
}

#[test]
fn ssr_expr_list_comprehension_binary_generator_pattern() {
    assert_matches(
        "ssr: <<XX || XX <= _@List, _@Cond>>.",
        "bar() -> XX = 1, [XX || XX <- List, XX >= 5].",
        &[],
    );
}

#[test]
fn ssr_expr_list_comprehension_binary() {
    assert_matches(
        "ssr: <<XX || XX <= _@List>>.",
        "bar(List) -> XX = 1, <<XX || XX <= List>>.",
        &["<<XX || XX <= List>>"],
    );
}

#[test]
fn ssr_expr_map_comprehension() {
    assert_matches(
        "ssr: #{_@K => _@V || _@K := _@V <- _@Map}.",
        "bar(Map) -> #{ K => V || K := V <- Map}.",
        &["#{ K => V || K := V <- Map}"],
    );
}

#[test]
fn ssr_expr_case() {
    assert_matches(
        "ssr: case _@XX of _@A -> _@B end .",
        "bar(F) -> XX = 1, case F of undefined -> XX end.",
        &["case F of undefined -> XX end"],
    );
}

#[test]
fn ssr_expr_receive() {
    assert_matches(
        "ssr: receive _@XX -> 3 end.",
        "bar(F) -> XX = 1, receive F -> 3 end.",
        &["receive F -> 3 end"],
    );
    assert_matches(
        "ssr: receive _@XX -> 3 after _@MS -> ok end.",
        "bar(F) -> XX = 1, receive F -> 3 end.",
        &[],
    );
    assert_matches(
        "ssr: receive _@XX -> 3 after _@MS -> ok end.",
        "bar(F) -> XX = 1, receive F -> 3 after 1000 -> ok end.",
        &["receive F -> 3 after 1000 -> ok end"],
    );
}
