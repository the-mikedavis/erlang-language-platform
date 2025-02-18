/*
 * Copyright (c) Meta Platforms, Inc. and affiliates.
 *
 * This source code is licensed under both the MIT license found in the
 * LICENSE-MIT file in the root directory of this source tree and the Apache
 * License, Version 2.0 found in the LICENSE-APACHE file in the root directory
 * of this source tree.
 */

use elp_base_db::AtomName;
use elp_syntax::SmolStr;
use elp_types_db::eqwalizer::expr::Body;
use elp_types_db::eqwalizer::expr::Clause;
use elp_types_db::eqwalizer::expr::Expr;
use elp_types_db::eqwalizer::expr::Lambda;
use elp_types_db::eqwalizer::expr::RemoteCall;
use elp_types_db::eqwalizer::guard::Guard;
use elp_types_db::eqwalizer::guard::Test;
use elp_types_db::eqwalizer::guard::TestAtom;
use elp_types_db::eqwalizer::guard::TestBinOp;
use elp_types_db::eqwalizer::guard::TestCall;
use elp_types_db::eqwalizer::guard::TestNumber;
use elp_types_db::eqwalizer::guard::TestTuple;
use elp_types_db::eqwalizer::guard::TestUnOp;
use elp_types_db::eqwalizer::guard::TestVar;
use elp_types_db::eqwalizer::pat::Pat;
use elp_types_db::eqwalizer::pat::PatVar;
use elp_types_db::eqwalizer::transformer;
use elp_types_db::eqwalizer::transformer::Transformer;
use elp_types_db::eqwalizer::Id;
use elp_types_db::eqwalizer::Pos;
use elp_types_db::eqwalizer::RemoteId;
use elp_types_db::eqwalizer::AST;
use fxhash::FxHashSet;
use lazy_static::lazy_static;

use crate::ast;

lazy_static! {
    static ref PREDICATES: FxHashSet<ast::Id> = {
        vec![
            ast::Id {
                name: "is_atom".into(),
                arity: 1,
            },
            ast::Id {
                name: "is_binary".into(),
                arity: 1,
            },
            ast::Id {
                name: "is_bitstring".into(),
                arity: 1,
            },
            ast::Id {
                name: "is_boolean".into(),
                arity: 1,
            },
            ast::Id {
                name: "is_float".into(),
                arity: 1,
            },
            ast::Id {
                name: "is_function".into(),
                arity: 1,
            },
            ast::Id {
                name: "is_integer".into(),
                arity: 1,
            },
            ast::Id {
                name: "is_list".into(),
                arity: 1,
            },
            ast::Id {
                name: "is_number".into(),
                arity: 1,
            },
            ast::Id {
                name: "is_pid".into(),
                arity: 1,
            },
            ast::Id {
                name: "is_port".into(),
                arity: 1,
            },
            ast::Id {
                name: "is_reference".into(),
                arity: 1,
            },
            ast::Id {
                name: "is_map".into(),
                arity: 1,
            },
            ast::Id {
                name: "is_tuple".into(),
                arity: 1,
            },
            ast::Id {
                name: "is_record".into(),
                arity: 2,
            },
            ast::Id {
                name: "is_function".into(),
                arity: 2,
            },
            ast::Id {
                name: "is_record".into(),
                arity: 3,
            },
        ]
        .into_iter()
        .collect()
    };
}

lazy_static! {
    static ref BINOP: FxHashSet<SmolStr> = {
        vec![
            "/", "*", "-", "+", "div", "rem", "band", "bor", "bxor", "bsl", "bsr", "or", "xor",
            "and", ">=", ">", "=<", "<", "/=", "=/=", "==", "=:=", "andalso", "orelse",
        ]
        .into_iter()
        .map(|s| s.into())
        .collect()
    };
}

lazy_static! {
    static ref UNOP: FxHashSet<SmolStr> = vec!["bnot", "+", "-", "not"]
        .into_iter()
        .map(|s| s.into())
        .collect();
}

fn as_test(expr: Expr) -> Option<Test> {
    match expr {
        Expr::Var(var) => Some(Test::TestVar(TestVar {
            v: var.n,
            location: var.location,
        })),
        Expr::AtomLit(atom) => Some(Test::TestAtom(TestAtom {
            s: atom.s,
            location: atom.location,
        })),
        Expr::IntLit(lit) => Some(Test::TestNumber(TestNumber {
            location: lit.location,
            lit: lit.value,
        })),
        Expr::RemoteCall(rcall) if PREDICATES.contains(&rcall.id.clone().into()) => {
            Some(Test::TestCall(TestCall {
                location: rcall.location,
                id: rcall.id.into(),
                args: as_tests(rcall.args)?,
            }))
        }
        Expr::Tuple(tuple) => Some(Test::TestTuple(TestTuple {
            location: tuple.location,
            elems: as_tests(tuple.elems)?,
        })),
        Expr::UnOp(unop) if UNOP.contains(&unop.op) => Some(Test::TestUnOp(TestUnOp {
            location: unop.location,
            op: unop.op,
            arg: Box::new(as_test(*unop.arg)?),
        })),
        Expr::BinOp(binop) if BINOP.contains(&binop.op) => Some(Test::TestBinOp(TestBinOp {
            location: binop.location,
            op: binop.op,
            arg_1: Box::new(as_test(*binop.arg_1)?),
            arg_2: Box::new(as_test(*binop.arg_2)?),
        })),
        _ => None,
    }
}

fn as_tests(exprs: Vec<Expr>) -> Option<Vec<Test>> {
    let mut exprs_test = vec![];
    for expr in exprs {
        if let Some(test) = as_test(expr) {
            exprs_test.push(test);
        } else {
            return None;
        }
    }
    Some(exprs_test)
}

struct Preprocessor {
    var: u32,
}

impl Preprocessor {
    fn fresh_var(&mut self) -> SmolStr {
        let var = self.var;
        self.var += 1;
        format!("$pp{}", var).into()
    }

    fn eta_expand_unary_predicate(&mut self, location: &Pos, name: AtomName) -> Lambda {
        let var_name = self.fresh_var();
        let test_call = Test::TestCall(TestCall {
            location: location.clone(),
            id: Id { name, arity: 1 },
            args: vec![Test::test_var(location.clone(), var_name.clone())],
        });
        let clause_pos = Clause {
            location: location.clone(),
            pats: vec![Pat::pat_var(location.clone(), var_name.clone())],
            guards: vec![Guard {
                tests: vec![test_call],
            }],
            body: Body {
                exprs: vec![Expr::atom_true(location.clone())],
            },
        };
        let clause_neg = Clause {
            location: location.clone(),
            pats: vec![Pat::pat_var(location.clone(), var_name.clone())],
            guards: vec![],
            body: Body {
                exprs: vec![Expr::atom_false(location.clone())],
            },
        };
        Lambda {
            location: location.clone(),
            name: None,
            clauses: vec![clause_pos, clause_neg],
        }
    }

    fn preprocess_lists_partition_arg_fun(&mut self, location: &Pos, expr: Expr) -> Expr {
        match expr {
            Expr::RemoteFun(rfun)
                if PREDICATES.contains(&rfun.id.clone().into()) && rfun.id.arity == 1 =>
            {
                Expr::Lambda(self.eta_expand_unary_predicate(location, rfun.id.name.clone()))
            }
            Expr::Lambda(lambda) if lambda.clauses.len() == 1 => {
                let clause = &lambda.clauses[0];
                if let [body] = &clause.body.exprs[..] {
                    if let [pat] = &clause.pats[..] {
                        if let Some(test) = as_test(body.clone()) {
                            return Expr::Lambda(Lambda {
                                location: lambda.location.clone(),
                                name: lambda.name.clone(),
                                clauses: vec![
                                    Clause {
                                        location: clause.location.clone(),
                                        pats: vec![pat.clone()],
                                        guards: vec![Guard { tests: vec![test] }],
                                        body: Body {
                                            exprs: vec![Expr::atom_true(clause.location.clone())],
                                        },
                                    },
                                    Clause {
                                        location: clause.location.clone(),
                                        pats: vec![Pat::PatVar(PatVar {
                                            location: clause.location.clone(),
                                            n: self.fresh_var(),
                                        })],
                                        guards: vec![],
                                        body: Body {
                                            exprs: vec![Expr::atom_false(clause.location.clone())],
                                        },
                                    },
                                ],
                            });
                        }
                    }
                }
                return Expr::Lambda(lambda);
            }
            expr => expr,
        }
    }
}

impl Transformer<()> for Preprocessor {
    fn transform_expr(&mut self, expr: Expr) -> Result<Expr, ()> {
        match expr {
            Expr::RemoteCall(RemoteCall {
                location,
                id:
                    RemoteId {
                        module,
                        name,
                        arity: 2,
                    },
                args,
            }) if module == "lists" && name == "partition" => {
                let [arg_fun, arg_list] = args.try_into().unwrap();
                let arg_trans = self.preprocess_lists_partition_arg_fun(&location, arg_fun);
                Ok(Expr::RemoteCall(RemoteCall {
                    location,
                    id: RemoteId {
                        module: "lists".into(),
                        name: "partition".into(),
                        arity: 2,
                    },
                    args: vec![arg_trans, arg_list],
                }))
            }
            e => transformer::walk_expr(self, e),
        }
    }
}

pub(crate) fn preprocess(ast: AST) -> AST {
    let mut preprocessor = Preprocessor { var: 0 };
    preprocessor.transform_ast(ast).unwrap()
}
