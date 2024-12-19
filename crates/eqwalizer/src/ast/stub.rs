/*
 * Copyright (c) Meta Platforms, Inc. and affiliates.
 *
 * This source code is licensed under both the MIT license found in the
 * LICENSE-MIT file in the root directory of this source tree and the Apache
 * License, Version 2.0 found in the LICENSE-APACHE file in the root directory
 * of this source tree.
 */

use elp_base_db::AtomName;
use elp_base_db::ModuleName;
use elp_syntax::SmolStr;
use elp_types_db::eqwalizer::form::Callback;
use elp_types_db::eqwalizer::form::FunSpec;
use elp_types_db::eqwalizer::form::InvalidForm;
use elp_types_db::eqwalizer::form::OpaqueTypeDecl;
use elp_types_db::eqwalizer::form::OverloadedFunSpec;
use elp_types_db::eqwalizer::form::RecDecl;
use elp_types_db::eqwalizer::form::TypeDecl;
use fxhash::FxHashMap;
use fxhash::FxHashSet;
use serde::Serialize;

use super::Id;

#[derive(Serialize, Debug, Clone, PartialEq, Eq)]
pub struct ModuleStub {
    pub module: ModuleName,
    pub exports: FxHashSet<Id>,
    pub imports: FxHashMap<Id, SmolStr>,
    pub export_types: FxHashSet<Id>,
    pub private_opaques: FxHashMap<Id, TypeDecl>,
    pub public_opaques: FxHashMap<Id, OpaqueTypeDecl>,
    pub types: FxHashMap<Id, TypeDecl>,
    pub specs: FxHashMap<Id, FunSpec>,
    pub overloaded_specs: FxHashMap<Id, OverloadedFunSpec>,
    pub records: FxHashMap<AtomName, RecDecl>,
    pub callbacks: Vec<Callback>,
    pub optional_callbacks: FxHashSet<Id>,
    pub invalid_forms: Vec<InvalidForm>,
}

impl ModuleStub {
    pub fn new(module: ModuleName) -> Self {
        Self {
            module,
            exports: Default::default(),
            imports: Default::default(),
            export_types: Default::default(),
            private_opaques: Default::default(),
            public_opaques: Default::default(),
            types: Default::default(),
            specs: Default::default(),
            overloaded_specs: Default::default(),
            records: Default::default(),
            callbacks: Default::default(),
            optional_callbacks: Default::default(),
            invalid_forms: Default::default(),
        }
    }

    pub fn to_bytes(&self) -> Vec<u8> {
        serde_json::to_vec(self).unwrap()
    }
}
