/*
 * Copyright (c) Meta Platforms, Inc. and affiliates.
 *
 * This source code is licensed under both the MIT license found in the
 * LICENSE-MIT file in the root directory of this source tree and the Apache
 * License, Version 2.0 found in the LICENSE-APACHE file in the root directory
 * of this source tree.
 */

//! This module contains the first phase of stubs validation
//!
//! It ensures that type ids point to types that exist, and expand local ids
//! to remote ids by adding the module they are defined in.
//!
//! It also performs several checks on types, such as ensuring that the same
//! type variable does not appear twice in the parameters of a type.

use elp_base_db::ModuleName;
use elp_base_db::ProjectId;
use elp_syntax::SmolStr;
use elp_types_db::eqwalizer::ext_types::AnyArityFunExtType;
use elp_types_db::eqwalizer::ext_types::ConstrainedFunType;
use elp_types_db::eqwalizer::ext_types::ExtProp;
use elp_types_db::eqwalizer::ext_types::ExtType;
use elp_types_db::eqwalizer::ext_types::FunExtType;
use elp_types_db::eqwalizer::ext_types::ListExtType;
use elp_types_db::eqwalizer::ext_types::MapExtType;
use elp_types_db::eqwalizer::ext_types::OptExtProp;
use elp_types_db::eqwalizer::ext_types::RecordRefinedExtType;
use elp_types_db::eqwalizer::ext_types::RefinedField;
use elp_types_db::eqwalizer::ext_types::RemoteExtType;
use elp_types_db::eqwalizer::ext_types::ReqExtProp;
use elp_types_db::eqwalizer::ext_types::TupleExtType;
use elp_types_db::eqwalizer::ext_types::UnionExtType;
use elp_types_db::eqwalizer::ext_types::VarExtType;
use elp_types_db::eqwalizer::form::ExternalCallback;
use elp_types_db::eqwalizer::form::ExternalForm;
use elp_types_db::eqwalizer::form::ExternalFunSpec;
use elp_types_db::eqwalizer::form::ExternalOpaqueDecl;
use elp_types_db::eqwalizer::form::ExternalRecDecl;
use elp_types_db::eqwalizer::form::ExternalRecField;
use elp_types_db::eqwalizer::form::ExternalTypeDecl;
use elp_types_db::eqwalizer::form::InvalidForm;
use elp_types_db::eqwalizer::form::InvalidFunSpec;
use elp_types_db::eqwalizer::form::InvalidMapType;
use elp_types_db::eqwalizer::form::InvalidRecDecl;
use elp_types_db::eqwalizer::form::InvalidTypeDecl;
use elp_types_db::eqwalizer::form::TypeDecl;
use elp_types_db::eqwalizer::invalid_diagnostics::BadMapKey;
use elp_types_db::eqwalizer::invalid_diagnostics::Invalid;
use elp_types_db::eqwalizer::invalid_diagnostics::NonExportedId;
use elp_types_db::eqwalizer::invalid_diagnostics::RecursiveConstraint;
use elp_types_db::eqwalizer::invalid_diagnostics::RepeatedTyVarInTyDecl;
use elp_types_db::eqwalizer::invalid_diagnostics::TyVarWithMultipleConstraints;
use elp_types_db::eqwalizer::invalid_diagnostics::UnboundTyVarInTyDecl;
use elp_types_db::eqwalizer::invalid_diagnostics::UnknownId;
use elp_types_db::eqwalizer::types::Type;
use fxhash::FxHashMap;
use fxhash::FxHashSet;

use super::convert_types::TypeConverter;
use super::db::EqwalizerASTDatabase;
use super::stub::ModuleStub;
use super::Id;
use super::RemoteId;
use super::TypeConversionError;
use super::AST;
use crate::ast;

struct Expander<'d> {
    module: ModuleName,
    project_id: ProjectId,
    invalids: Vec<InvalidForm>,
    db: &'d dyn EqwalizerASTDatabase,
}

impl Expander<'_> {
    fn expand_fun_spec(
        &mut self,
        fun_spec: ExternalFunSpec,
    ) -> Result<ExternalFunSpec, InvalidFunSpec> {
        match self.expand_cfts(fun_spec.types) {
            Ok(types) => Ok(ExternalFunSpec { types, ..fun_spec }),
            Err(te) => Err(InvalidFunSpec {
                location: fun_spec.location,
                id: fun_spec.id,
                te,
            }),
        }
    }

    fn expand_cfts(
        &mut self,
        cfts: Vec<ConstrainedFunType>,
    ) -> Result<Vec<ConstrainedFunType>, Invalid> {
        cfts.into_iter().map(|cft| self.expand_cft(cft)).collect()
    }

    fn expand_cft(&mut self, cft: ConstrainedFunType) -> Result<ConstrainedFunType, Invalid> {
        let ft = {
            if cft.constraints.is_empty() {
                cft.ty
            } else {
                self.check_multiply_constrained_type_var(&cft)?;
                let subst: FxHashMap<SmolStr, ExtType> = cft
                    .constraints
                    .into_iter()
                    .map(|c| (c.t_var, c.ty))
                    .collect();
                let arg_tys = cft
                    .ty
                    .arg_tys
                    .into_iter()
                    .map(|ty| self.expand_constraints(ty, &subst, &FxHashSet::default()))
                    .collect::<Result<Vec<_>, _>>()?;
                let res_ty =
                    self.expand_constraints(*cft.ty.res_ty, &subst, &FxHashSet::default())?;
                FunExtType {
                    arg_tys,
                    res_ty: Box::new(res_ty),
                    location: cft.ty.location,
                }
            }
        };
        let arg_tys = self.expand_types(ft.arg_tys)?;
        let res_ty = Box::new(self.expand_type(*ft.res_ty)?);
        let exp_ft = FunExtType {
            arg_tys,
            res_ty,
            location: ft.location,
        };
        Ok(ConstrainedFunType {
            location: cft.location,
            ty: exp_ft,
            constraints: vec![],
        })
    }

    fn expand_callback(
        &mut self,
        cb: ExternalCallback,
    ) -> Result<ExternalCallback, InvalidFunSpec> {
        match self.expand_cfts(cb.types) {
            Ok(types) => Ok(ExternalCallback { types, ..cb }),
            Err(te) => Err(InvalidFunSpec {
                location: cb.location,
                id: cb.id,
                te,
            }),
        }
    }

    fn expand_type_decl(
        &mut self,
        decl: ExternalTypeDecl,
    ) -> Result<ExternalTypeDecl, InvalidTypeDecl> {
        let result = self
            .validate_type_vars(&decl.location, &decl.body, &decl.params)
            .and_then(|()| self.expand_type(decl.body));
        match result {
            Ok(body) => Ok(ExternalTypeDecl { body, ..decl }),
            Err(te) => Err(InvalidTypeDecl {
                id: decl.id,
                te,
                location: decl.location,
            }),
        }
    }

    fn expand_opaque_decl(
        &mut self,
        decl: ExternalOpaqueDecl,
    ) -> Result<ExternalOpaqueDecl, InvalidTypeDecl> {
        let result = self
            .validate_type_vars(&decl.location, &decl.body, &decl.params)
            .and_then(|()| self.expand_type(decl.body));
        match result {
            Ok(body) => Ok(ExternalOpaqueDecl { body, ..decl }),
            Err(te) => Err(InvalidTypeDecl {
                id: decl.id,
                te,
                location: decl.location,
            }),
        }
    }

    fn validate_type_vars(
        &mut self,
        pos: &ast::Pos,
        body: &ExtType,
        params: &Vec<SmolStr>,
    ) -> Result<(), Invalid> {
        self.check_repeated_type_param(pos, params)?;
        body.traverse(&mut |ty| match ty {
            ExtType::VarExtType(ty_var) => self.check_unbound_type_var(pos, params, ty_var),
            _ => Ok(()),
        })?;
        Ok(())
    }

    fn check_repeated_type_param(
        &mut self,
        pos: &ast::Pos,
        params: &Vec<SmolStr>,
    ) -> Result<(), Invalid> {
        let mut names = FxHashSet::default();
        for name in params {
            if names.contains(name) {
                return Err(Invalid::RepeatedTyVarInTyDecl(RepeatedTyVarInTyDecl {
                    location: pos.clone(),
                    name: name.clone(),
                }));
            }
            names.insert(name);
        }
        Ok(())
    }

    fn check_multiply_constrained_type_var(
        &mut self,
        cft: &ConstrainedFunType,
    ) -> Result<(), Invalid> {
        let mut names = FxHashSet::default();
        let vars: Vec<&SmolStr> = cft.constraints.iter().map(|c| &c.t_var).collect();
        for name in vars {
            if names.contains(name) {
                return Err(Invalid::TyVarWithMultipleConstraints(
                    TyVarWithMultipleConstraints {
                        location: cft.location.clone(),
                        n: name.clone(),
                    },
                ));
            }
            names.insert(name);
        }
        Ok(())
    }

    fn check_unbound_type_var(
        &mut self,
        pos: &ast::Pos,
        params: &[SmolStr],
        ty_var: &VarExtType,
    ) -> Result<(), Invalid> {
        if !params.contains(&ty_var.name) {
            Err(Invalid::UnboundTyVarInTyDecl(UnboundTyVarInTyDecl {
                location: pos.clone(),
                name: ty_var.name.clone(),
            }))
        } else {
            Ok(())
        }
    }

    fn expand_rec_decl(
        &mut self,
        decl: ExternalRecDecl,
    ) -> Result<ExternalRecDecl, InvalidRecDecl> {
        let fields = decl
            .fields
            .into_iter()
            .map(|field| self.expand_rec_field(field))
            .collect::<Result<Vec<_>, _>>();
        match fields {
            Ok(fields) => Ok(ExternalRecDecl { fields, ..decl }),
            Err(te) => Err(InvalidRecDecl {
                location: decl.location,
                name: decl.name,
                te,
            }),
        }
    }

    fn expand_types(&mut self, ts: Vec<ExtType>) -> Result<Vec<ExtType>, Invalid> {
        ts.into_iter().map(|t| self.expand_type(t)).collect()
    }

    fn expand_type(&mut self, t: ExtType) -> Result<ExtType, Invalid> {
        match t {
            ExtType::LocalExtType(ty) => {
                let id = RemoteId {
                    module: self.module.clone(),
                    name: ty.id.name,
                    arity: ty.id.arity,
                };
                let expanded_params = self.expand_types(ty.args)?;
                Ok(ExtType::RemoteExtType(RemoteExtType {
                    id,
                    args: expanded_params,
                    location: ty.location,
                }))
            }
            ExtType::RemoteExtType(ty) => {
                let local_id = Id {
                    name: ty.id.name.clone(),
                    arity: ty.id.arity,
                };
                let is_defined = self
                    .db
                    .type_ids(self.project_id, ty.id.module.clone())
                    .map(|ids| ids.contains(&local_id))
                    .unwrap_or(false);
                let is_exported = self
                    .db
                    .exported_type_ids(self.project_id, ty.id.module.clone())
                    .map(|ids| ids.contains(&local_id))
                    .unwrap_or(false);
                if !is_defined {
                    Err(Invalid::UnknownId(UnknownId {
                        location: ty.location,
                        id: ty.id,
                    }))
                } else if ty.id.module != self.module && !is_exported {
                    Err(Invalid::NonExportedId(NonExportedId {
                        location: ty.location,
                        id: ty.id,
                    }))
                } else {
                    let expanded_params = self.expand_types(ty.args)?;
                    Ok(ExtType::RemoteExtType(RemoteExtType {
                        id: ty.id,
                        args: expanded_params,
                        location: ty.location,
                    }))
                }
            }
            ExtType::FunExtType(ty) => Ok(ExtType::FunExtType(FunExtType {
                location: ty.location,
                arg_tys: self.expand_types(ty.arg_tys)?,
                res_ty: Box::new(self.expand_type(*ty.res_ty)?),
            })),
            ExtType::AnyArityFunExtType(ty) => {
                Ok(ExtType::AnyArityFunExtType(AnyArityFunExtType {
                    location: ty.location,
                    res_ty: Box::new(self.expand_type(*ty.res_ty)?),
                }))
            }
            ExtType::TupleExtType(ty) => Ok(ExtType::TupleExtType(TupleExtType {
                location: ty.location,
                arg_tys: self.expand_types(ty.arg_tys)?,
            })),
            ExtType::ListExtType(ty) => Ok(ExtType::ListExtType(ListExtType {
                location: ty.location,
                t: Box::new(self.expand_type(*ty.t)?),
            })),
            ExtType::AnyListExtType(ty) => Ok(ExtType::ListExtType(ListExtType {
                location: ty.location.clone(),
                t: Box::new(ExtType::dynamic_ext_type(ty.location)),
            })),
            ExtType::UnionExtType(ty) => Ok(ExtType::UnionExtType(UnionExtType {
                location: ty.location,
                tys: self.expand_types(ty.tys)?,
            })),
            ExtType::MapExtType(ty) => {
                if let Some(invalid_prop) = ty.props.iter().find(|prop| !prop.is_ok()) {
                    let location = invalid_prop.location().clone();
                    let invalid_form = InvalidForm::InvalidMapType(InvalidMapType {
                        location: location.clone(),
                        te: Invalid::BadMapKey(BadMapKey {
                            location,
                            required: invalid_prop.required(),
                        }),
                    });
                    self.invalids.push(invalid_form);
                    Ok(ExtType::MapExtType(MapExtType {
                        location: ty.location.clone(),
                        props: vec![ExtProp::OptExtProp(OptExtProp {
                            location: ty.location.clone(),
                            key: ExtType::dynamic_ext_type(ty.location.clone()),
                            tp: ExtType::dynamic_ext_type(ty.location),
                        })],
                    }))
                } else {
                    let props = ty
                        .props
                        .into_iter()
                        .map(|prop| self.expand_prop(prop))
                        .collect::<Result<Vec<_>, _>>()?;
                    Ok(ExtType::MapExtType(MapExtType {
                        location: ty.location,
                        props,
                    }))
                }
            }
            ExtType::AnyMapExtType(ty) => Ok(ExtType::MapExtType(MapExtType {
                location: ty.location.clone(),
                props: vec![ExtProp::OptExtProp(OptExtProp {
                    location: ty.location.clone(),
                    key: ExtType::dynamic_ext_type(ty.location.clone()),
                    tp: ExtType::dynamic_ext_type(ty.location),
                })],
            })),
            ExtType::RecordRefinedExtType(ty) => {
                Ok(ExtType::RecordRefinedExtType(RecordRefinedExtType {
                    location: ty.location,
                    name: ty.name,
                    refined_fields: ty
                        .refined_fields
                        .into_iter()
                        .map(|field| self.expand_refined_record_field(field))
                        .collect::<Result<Vec<_>, _>>()?,
                }))
            }
            ExtType::VarExtType(_)
            | ExtType::BuiltinExtType(_)
            | ExtType::IntLitExtType(_)
            | ExtType::AtomLitExtType(_)
            | ExtType::RecordExtType(_)
            | ExtType::UnOpType(_)
            | ExtType::BinOpType(_) => Ok(t),
        }
    }

    fn expand_prop(&mut self, prop: ExtProp) -> Result<ExtProp, Invalid> {
        match prop {
            ExtProp::ReqExtProp(prop) => Ok(ExtProp::ReqExtProp(ReqExtProp {
                location: prop.location,
                key: self.expand_type(prop.key)?,
                tp: self.expand_type(prop.tp)?,
            })),
            ExtProp::OptExtProp(prop) => Ok(ExtProp::OptExtProp(OptExtProp {
                location: prop.location,
                key: self.expand_type(prop.key)?,
                tp: self.expand_type(prop.tp)?,
            })),
            prop => Err(Invalid::BadMapKey(BadMapKey {
                location: prop.location().clone(),
                required: prop.required(),
            })),
        }
    }

    fn expand_refined_record_field(
        &mut self,
        field: RefinedField,
    ) -> Result<RefinedField, Invalid> {
        Ok(RefinedField {
            label: field.label,
            ty: self.expand_type(field.ty)?,
        })
    }

    fn expand_all_constraints(
        &mut self,
        ts: Vec<ExtType>,
        sub: &FxHashMap<SmolStr, ExtType>,
        stack: &FxHashSet<SmolStr>,
    ) -> Result<Vec<ExtType>, Invalid> {
        ts.into_iter()
            .map(|t| self.expand_constraints(t, sub, stack))
            .collect()
    }

    fn expand_constraints(
        &mut self,
        t: ExtType,
        sub: &FxHashMap<SmolStr, ExtType>,
        stack: &FxHashSet<SmolStr>,
    ) -> Result<ExtType, Invalid> {
        match t {
            ExtType::LocalExtType(ty) => {
                let id = RemoteId {
                    module: self.module.clone(),
                    name: ty.id.name,
                    arity: ty.id.arity,
                };
                let expanded_params = self.expand_all_constraints(ty.args, sub, stack)?;
                Ok(ExtType::RemoteExtType(RemoteExtType {
                    id,
                    args: expanded_params,
                    location: ty.location,
                }))
            }
            ExtType::RemoteExtType(ty) => Ok(ExtType::RemoteExtType(RemoteExtType {
                location: ty.location,
                id: ty.id,
                args: self.expand_all_constraints(ty.args, sub, stack)?,
            })),
            ExtType::FunExtType(ty) => Ok(ExtType::FunExtType(FunExtType {
                location: ty.location,
                arg_tys: self.expand_all_constraints(ty.arg_tys, sub, stack)?,
                res_ty: Box::new(self.expand_constraints(*ty.res_ty, sub, stack)?),
            })),
            ExtType::AnyArityFunExtType(ty) => {
                Ok(ExtType::AnyArityFunExtType(AnyArityFunExtType {
                    location: ty.location,
                    res_ty: Box::new(self.expand_constraints(*ty.res_ty, sub, stack)?),
                }))
            }
            ExtType::TupleExtType(ty) => Ok(ExtType::TupleExtType(TupleExtType {
                location: ty.location,
                arg_tys: self.expand_all_constraints(ty.arg_tys, sub, stack)?,
            })),
            ExtType::ListExtType(ty) => Ok(ExtType::ListExtType(ListExtType {
                location: ty.location,
                t: Box::new(self.expand_constraints(*ty.t, sub, stack)?),
            })),
            ExtType::UnionExtType(ty) => Ok(ExtType::UnionExtType(UnionExtType {
                location: ty.location,
                tys: self.expand_all_constraints(ty.tys, sub, stack)?,
            })),
            ExtType::MapExtType(ty) => {
                if let Some(invalid_prop) = ty.props.iter().find(|prop| !prop.is_ok()) {
                    let location = invalid_prop.location().clone();
                    let invalid_form = InvalidForm::InvalidMapType(InvalidMapType {
                        location: location.clone(),
                        te: Invalid::BadMapKey(BadMapKey {
                            location,
                            required: invalid_prop.required(),
                        }),
                    });
                    self.invalids.push(invalid_form);
                    Ok(ExtType::MapExtType(MapExtType {
                        location: ty.location.clone(),
                        props: vec![ExtProp::OptExtProp(OptExtProp {
                            location: ty.location.clone(),
                            key: ExtType::dynamic_ext_type(ty.location.clone()),
                            tp: ExtType::dynamic_ext_type(ty.location),
                        })],
                    }))
                } else {
                    let props = ty
                        .props
                        .into_iter()
                        .map(|prop| self.expand_prop_constraint(prop, sub, stack))
                        .collect::<Result<Vec<_>, _>>()?;
                    Ok(ExtType::MapExtType(MapExtType {
                        location: ty.location,
                        props,
                    }))
                }
            }
            ExtType::RecordRefinedExtType(ty) => {
                Ok(ExtType::RecordRefinedExtType(RecordRefinedExtType {
                    location: ty.location,
                    name: ty.name,
                    refined_fields: ty
                        .refined_fields
                        .into_iter()
                        .map(|field| self.expand_refined_record_field_constraint(field, sub, stack))
                        .collect::<Result<Vec<_>, _>>()?,
                }))
            }
            ExtType::VarExtType(ty) => {
                if stack.contains(&ty.name) {
                    Err(Invalid::RecursiveConstraint(RecursiveConstraint {
                        location: ty.location,
                        n: ty.name,
                    }))
                } else if let Some(tp) = sub.get(&ty.name) {
                    let mut stack2 = stack.clone();
                    stack2.insert(ty.name);
                    self.expand_constraints(tp.clone(), sub, &stack2)
                } else {
                    Ok(ExtType::VarExtType(ty))
                }
            }
            ExtType::BuiltinExtType(_)
            | ExtType::IntLitExtType(_)
            | ExtType::AtomLitExtType(_)
            | ExtType::RecordExtType(_)
            | ExtType::AnyMapExtType(_)
            | ExtType::UnOpType(_)
            | ExtType::BinOpType(_)
            | ExtType::AnyListExtType(_) => Ok(t),
        }
    }

    fn expand_prop_constraint(
        &mut self,
        prop: ExtProp,
        sub: &FxHashMap<SmolStr, ExtType>,
        stack: &FxHashSet<SmolStr>,
    ) -> Result<ExtProp, Invalid> {
        match prop {
            ExtProp::ReqExtProp(prop) => Ok(ExtProp::ReqExtProp(ReqExtProp {
                location: prop.location,
                key: self.expand_constraints(prop.key, sub, stack)?,
                tp: self.expand_constraints(prop.tp, sub, stack)?,
            })),
            ExtProp::OptExtProp(prop) => Ok(ExtProp::OptExtProp(OptExtProp {
                location: prop.location,
                key: self.expand_constraints(prop.key, sub, stack)?,
                tp: self.expand_constraints(prop.tp, sub, stack)?,
            })),
            prop => Err(Invalid::BadMapKey(BadMapKey {
                location: prop.location().clone(),
                required: prop.required(),
            })),
        }
    }

    fn expand_refined_record_field_constraint(
        &mut self,
        field: RefinedField,
        sub: &FxHashMap<SmolStr, ExtType>,
        stack: &FxHashSet<SmolStr>,
    ) -> Result<RefinedField, Invalid> {
        Ok(RefinedField {
            label: field.label,
            ty: self.expand_constraints(field.ty, sub, stack)?,
        })
    }

    fn expand_rec_field(&mut self, field: ExternalRecField) -> Result<ExternalRecField, Invalid> {
        let tp = {
            if let Some(tp) = field.tp {
                Some(self.expand_type(tp)?)
            } else {
                None
            }
        };
        Ok(ExternalRecField { tp, ..field })
    }
}

pub struct StubExpander<'d> {
    expander: Expander<'d>,
    type_converter: TypeConverter,
    pub stub: ModuleStub,
    module_file: SmolStr,
    current_file: SmolStr,
}

impl StubExpander<'_> {
    pub fn new<'d>(
        db: &'d dyn EqwalizerASTDatabase,
        project_id: ProjectId,
        module: ModuleName,
        ast: &AST,
    ) -> StubExpander<'d> {
        let expander = Expander {
            module: module.clone(),
            invalids: vec![],
            db,
            project_id,
        };
        let type_converter = TypeConverter::new(module.clone());
        let stub = ModuleStub::new(module);
        let module_file = ast
            .iter()
            .find_map(|form| match form {
                ExternalForm::File(f) => Some(f.file.clone()),
                _ => None,
            })
            .unwrap();
        let current_file = module_file.clone();
        StubExpander {
            expander,
            type_converter,
            stub,
            module_file,
            current_file,
        }
    }

    fn add_type_decl(&mut self, t: ExternalTypeDecl) -> Result<(), TypeConversionError> {
        match self.expander.expand_type_decl(t) {
            Ok(decl) => {
                let decl = self.type_converter.convert_type_decl(decl)?;
                self.stub.types.insert(decl.id.clone(), decl);
            }
            Err(invalid) => {
                if self.current_file == self.module_file {
                    self.stub
                        .invalid_forms
                        .push(InvalidForm::InvalidTypeDecl(invalid));
                }
            }
        }
        Ok(())
    }

    fn add_opaque_decl(&mut self, t: ExternalOpaqueDecl) -> Result<(), TypeConversionError> {
        match self.expander.expand_opaque_decl(t) {
            Ok(decl) => {
                let public_decl = self.type_converter.convert_opaque_decl_public(decl.clone());
                self.stub
                    .public_opaques
                    .insert(public_decl.id.clone(), public_decl);
                let opaque_decl = self.type_converter.convert_opaque_private(decl)?;
                self.stub
                    .private_opaques
                    .insert(opaque_decl.id.clone(), opaque_decl);
            }
            Err(invalid) => {
                if self.current_file == self.module_file {
                    self.stub
                        .invalid_forms
                        .push(InvalidForm::InvalidTypeDecl(invalid));
                }
            }
        }
        Ok(())
    }

    fn add_record_decl(&mut self, t: ExternalRecDecl) -> Result<(), TypeConversionError> {
        match self.expander.expand_rec_decl(t) {
            Ok(decl) => match self.type_converter.convert_rec_decl(decl)? {
                Ok(decl) => {
                    self.stub.records.insert(decl.name.clone(), decl);
                }
                Err(invalid) => {
                    if self.current_file == self.module_file {
                        self.stub
                            .invalid_forms
                            .push(InvalidForm::InvalidConvertTypeInRecDecl(invalid));
                    }
                }
            },
            Err(invalid) => {
                if self.current_file == self.module_file {
                    self.stub
                        .invalid_forms
                        .push(InvalidForm::InvalidRecDecl(invalid));
                }
            }
        }
        Ok(())
    }

    fn add_spec(&mut self, t: ExternalFunSpec) -> Result<(), TypeConversionError> {
        match self.expander.expand_fun_spec(t) {
            Ok(decl) => {
                if decl.types.len() == 1 {
                    let spec = self.type_converter.convert_spec(decl)?;
                    self.stub.specs.insert(spec.id.clone(), spec);
                } else {
                    let spec = self.type_converter.convert_overloaded_spec(decl)?;
                    self.stub.overloaded_specs.insert(spec.id.clone(), spec);
                }
            }
            Err(invalid) => {
                if self.current_file == self.module_file {
                    self.stub
                        .invalid_forms
                        .push(InvalidForm::InvalidFunSpec(invalid));
                }
            }
        }
        Ok(())
    }

    fn add_callback(&mut self, cb: ExternalCallback) -> Result<(), TypeConversionError> {
        match self.expander.expand_callback(cb) {
            Ok(cb) => {
                let cb = self.type_converter.convert_callback(cb)?;
                self.stub.callbacks.push(cb);
            }
            Err(invalid) => {
                if self.current_file == self.module_file {
                    self.stub
                        .invalid_forms
                        .push(InvalidForm::InvalidFunSpec(invalid));
                }
            }
        }
        Ok(())
    }

    fn add_extra_types(&mut self) {
        if self.stub.module == "erlang" {
            let pos: ast::Pos = {
                if self
                    .expander
                    .db
                    .from_beam(self.expander.project_id, ModuleName::new("erlang"))
                {
                    elp_types_db::eqwalizer::LineAndColumn::fake().into()
                } else {
                    elp_types_db::eqwalizer::TextRange::fake().into()
                }
            };
            Type::builtin_type_aliases("erlang")
                .into_iter()
                .for_each(|name| {
                    let body = Type::builtin_type_alias_body(&name).unwrap();
                    let id = ast::Id { name, arity: 0 };
                    let decl = TypeDecl {
                        location: pos.clone(),
                        id: id.clone(),
                        params: vec![],
                        body,
                        file: None,
                    };
                    self.stub.types.insert(id, decl);
                })
        }
    }

    pub fn expand(&mut self, ast: AST) -> Result<(), TypeConversionError> {
        ast.into_iter().try_for_each(|form| match form {
            ExternalForm::File(f) => {
                self.current_file = f.file;
                Ok(())
            }
            ExternalForm::Export(e) => {
                self.stub.exports.extend(e.funs);
                Ok(())
            }
            ExternalForm::Import(i) => {
                i.funs.into_iter().for_each(|id| {
                    self.stub.imports.insert(id, i.module.clone());
                });
                Ok(())
            }
            ExternalForm::ExportType(e) => {
                self.stub.export_types.extend(e.types);
                Ok(())
            }
            ExternalForm::ExternalTypeDecl(d) => self.add_type_decl(d),
            ExternalForm::ExternalOpaqueDecl(d) => self.add_opaque_decl(d),
            ExternalForm::ExternalFunSpec(s) => self.add_spec(s),
            ExternalForm::ExternalRecDecl(r) => self.add_record_decl(r),
            ExternalForm::Behaviour(_) => Ok(()),
            ExternalForm::ExternalCallback(cb) => self.add_callback(cb),
            ExternalForm::ExternalOptionalCallbacks(ocb) => {
                self.stub.optional_callbacks.extend(ocb.ids);
                Ok(())
            }
            ExternalForm::Module(_)
            | ExternalForm::CompileExportAll(_)
            | ExternalForm::FunDecl(_)
            | ExternalForm::ElpMetadata(_)
            | ExternalForm::EqwalizerUnlimitedRefinement(_)
            | ExternalForm::EqwalizerNowarnFunction(_)
            | ExternalForm::TypingAttribute(_) => Ok(()),
        })?;
        self.add_extra_types();
        self.stub.invalid_forms.append(&mut self.expander.invalids);
        Ok(())
    }
}
