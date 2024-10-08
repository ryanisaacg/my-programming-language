use std::collections::HashMap;

use crate::{
    declaration_context::{IntrinsicFunction, TypeID},
    id::{AnyID, ConstantID, FunctionID, NodeID, VariableID},
    parser::{AstArena, AstNode},
    provenance::SourceRange,
    typecheck::{
        is_assignable_to, CollectionType, ExpressionType, PrimitiveType, TypeDeclaration,
        TypecheckedFile,
    },
    DeclarationContext,
};

mod auto_deref_dot;
mod auto_numeric_cast;
pub mod constant_inlining;
mod coroutines;
mod create_temp_vars_for_lvalues;
mod discard_unused_values;
mod interface_conversion_pass;
mod lower;
mod rewrite_associated_functions;
mod simplify_sequence_expressions;
mod unions;
mod widen_null;

pub fn desugar_module<'dest>(
    declarations: &'dest DeclarationContext,
    ast: &AstArena,
    module: TypecheckedFile<'_, 'dest>,
    constant_values: &HashMap<ConstantID, HirNode>,
) -> HirModule {
    let mut module = lower::lower_module(declarations, ast, module);

    // Important that this comes before ANY pass that uses the declarations
    coroutines::rewrite_generator_calls(&mut module);

    constant_inlining::inline_constants(&mut module, constant_values);

    // This should come before anyone looks too hard at dot operators and function calls
    unions::convert_calls_to_union_literals(&mut module, declarations);
    // Associated function rewriting needs to come before auto_deref
    module.par_visit_mut(|expr: &mut _| rewrite_associated_functions::rewrite(declarations, expr));
    interface_conversion_pass::rewrite(&mut module, declarations);
    // These passes can be in any order
    coroutines::rewrite_yields(&mut module);
    auto_deref_dot::auto_deref_dot(&mut module);
    auto_numeric_cast::auto_numeric_cast(&mut module, declarations);
    widen_null::widen_null(&mut module, declarations);
    create_temp_vars_for_lvalues::create_temp_vars_for_lvalues(&mut module);

    // These should go after desugaring, to clean up any sequences that are created by earlier passes
    simplify_sequence_expressions::simplify_sequence_assignments(&mut module);
    simplify_sequence_expressions::simplify_sequence_uses(&mut module, declarations);
    simplify_sequence_expressions::simplify_trailing_if(&mut module);

    // This should go last, to clean up any expressions that are returning an unused value
    discard_unused_values::discard_unused_values(&mut module, declarations);

    module
}

#[derive(Debug)]
pub struct HirModule {
    pub top_level_statements: HirNode,
    // TODO: include imports, structs, and extern function declaration
    pub functions: Vec<HirFunction>,
}

impl HirModule {
    pub fn visit_mut(&mut self, mut callback: impl FnMut(&mut HirNode)) {
        self.top_level_statements.visit_mut_recursive(&mut callback);
        for func in self.functions.iter_mut() {
            func.body.visit_mut_recursive(&mut callback);
        }
    }

    pub fn visit(&self, mut callback: impl FnMut(Option<&HirNode>, &HirNode)) {
        self.top_level_statements
            .visit_recursive(None, &mut callback);
        for func in self.functions.iter() {
            func.body.visit_recursive(None, &mut callback);
        }
    }

    pub fn par_visit_mut(&mut self, mut callback: impl Fn(&mut HirNode) + Send + Sync) {
        use rayon::prelude::*;
        self.top_level_statements.visit_mut_recursive(&mut callback);
        self.functions
            .par_iter_mut()
            .for_each(|func| func.body.visit_mut(&callback));
    }

    pub fn par_visit(&self, callback: impl Fn(Option<&HirNode>, &HirNode) + Sync) {
        use rayon::prelude::*;
        self.top_level_statements.visit(&callback);
        self.functions
            .par_iter()
            .for_each(|func| func.body.visit(&callback));
    }
}

#[derive(Debug)]
pub struct HirFunction {
    pub id: FunctionID,
    pub name: Option<String>,
    pub body: HirNode,
    pub generator: Option<GeneratorProperties>,
}

#[derive(Clone, Debug)]
pub struct GeneratorProperties {
    pub generator_var_id: VariableID,
    pub param_var_id: Option<VariableID>,
    pub ty: ExpressionType,
}

#[derive(Debug, PartialEq)]
pub struct HirNode {
    pub id: NodeID,
    pub value: HirNodeValue,
    pub ty: ExpressionType,
    pub provenance: Option<SourceRange>,
}

impl Clone for HirNode {
    fn clone(&self) -> Self {
        HirNode {
            id: NodeID::new(),
            value: self.value.clone(),
            ty: self.ty.clone(),
            provenance: self.provenance.clone(),
        }
    }
}

impl Default for HirNode {
    fn default() -> Self {
        HirNode::dummy()
    }
}

impl HirNode {
    pub fn dummy() -> HirNode {
        HirNode {
            id: NodeID::dummy(),
            value: HirNodeValue::Null,
            ty: ExpressionType::Null,
            provenance: None,
        }
    }

    pub fn new(
        value: HirNodeValue,
        ty: ExpressionType,
        provenance: Option<SourceRange>,
    ) -> HirNode {
        HirNode {
            id: NodeID::new(),
            value,
            ty,
            provenance,
        }
    }

    pub fn autogenerated(value: HirNodeValue, ty: ExpressionType) -> HirNode {
        HirNode {
            id: NodeID::new(),
            value,
            ty,
            provenance: None,
        }
    }

    pub fn new_void(value: HirNodeValue) -> HirNode {
        HirNode::autogenerated(value, ExpressionType::Void)
    }

    pub fn generated_with_id(id: NodeID, value: HirNodeValue, ty: ExpressionType) -> HirNode {
        HirNode {
            id,
            value,
            ty,
            provenance: None,
        }
    }

    pub fn from_ast(ast: &AstNode, value: HirNodeValue, ty: ExpressionType) -> HirNode {
        HirNode {
            id: NodeID::new(),
            value,
            ty,
            provenance: Some(ast.provenance.clone()),
        }
    }

    pub fn from_ast_void(ast: &AstNode, value: HirNodeValue) -> HirNode {
        Self::from_ast(ast, value, ExpressionType::Void)
    }

    pub fn visit_mut(&mut self, mut callback: impl FnMut(&mut HirNode)) {
        self.visit_mut_recursive(&mut callback);
    }

    fn visit_mut_recursive(&mut self, callback: &mut impl FnMut(&mut HirNode)) {
        self.children_mut(|child| {
            child.visit_mut_recursive(callback);
        });
        callback(self);
    }

    pub fn visit(&self, mut callback: impl FnMut(Option<&HirNode>, &HirNode)) {
        self.visit_recursive(None, &mut callback);
    }

    fn visit_recursive(
        &self,
        parent: Option<&HirNode>,
        callback: &mut impl FnMut(Option<&HirNode>, &HirNode),
    ) {
        callback(parent, self);
        self.children(|child| {
            child.visit_recursive(Some(self), callback);
        });
    }

    pub fn children<'a>(&'a self, mut callback: impl FnMut(&'a HirNode)) {
        self.children_impl(None, |_, node| callback(node));
    }

    pub fn children_mut<'a>(&'a mut self, mut callback: impl FnMut(&'a mut HirNode)) {
        self.children_mut_impl(None, |_, node| callback(node));
    }

    // TODO: could use a better name
    pub fn walk_expected_types_for_children(
        &self,
        declarations: &DeclarationContext,
        mut callback: impl FnMut(&ExpressionType, &HirNode),
    ) {
        self.children_impl(Some(declarations), |ty, node| {
            if let Some(ty) = ty {
                callback(ty, node);
            }
        });
    }

    pub fn walk_expected_types_for_children_mut(
        &mut self,
        declarations: &DeclarationContext,
        mut callback: impl FnMut(&ExpressionType, &mut HirNode),
    ) {
        self.children_mut_impl(Some(declarations), |ty, node| {
            if let Some(ty) = ty {
                callback(ty, node);
            }
        });
    }

    fn children_impl<'a>(
        &'a self,
        declarations: Option<&DeclarationContext>,
        mut callback: impl FnMut(Option<&ExpressionType>, &'a HirNode),
    ) {
        let callback = &mut callback;
        match &self.value {
            HirNodeValue::DictIndex(lhs, rhs) => {
                callback(None, lhs);
                // TODO: expect dictionary keys
                callback(None, rhs);
            }
            HirNodeValue::ArrayIndex(lhs, idx) => {
                callback(None, lhs);
                callback(
                    Some(&ExpressionType::Primitive(PrimitiveType::PointerSize)),
                    idx,
                );
            }
            HirNodeValue::UnionLiteral(ty, variant, child) => {
                let variant_ty = declarations.and_then(|declarations| {
                    let TypeDeclaration::Union(ty) = &declarations.id_to_decl[ty] else {
                        unreachable!()
                    };
                    ty.variants[variant as &String].as_ref()
                });
                if let Some(child) = child {
                    callback(variant_ty, child);
                }
            }
            HirNodeValue::Arithmetic(_, lhs, rhs) => {
                callback(Some(&self.ty), lhs);
                callback(Some(&self.ty), rhs);
            }
            HirNodeValue::Comparison(_, lhs, rhs) => {
                if let Some(declarations) = declarations {
                    if is_assignable_to(declarations, None, &lhs.ty, &rhs.ty) {
                        callback(Some(&lhs.ty), rhs);
                        callback(None, lhs);
                    } else {
                        callback(Some(&rhs.ty), lhs);
                        callback(None, rhs);
                    }
                } else {
                    callback(None, lhs);
                    callback(None, rhs);
                }
            }
            HirNodeValue::NullCoalesce(lhs, rhs) => {
                callback(Some(&self.ty), rhs);
                callback(
                    Some(&ExpressionType::Nullable(Box::new(self.ty.clone()))),
                    lhs,
                );
            }
            HirNodeValue::BinaryLogical(_, lhs, rhs) => {
                callback(Some(&ExpressionType::Primitive(PrimitiveType::Bool)), lhs);
                callback(Some(&ExpressionType::Primitive(PrimitiveType::Bool)), rhs);
            }
            HirNodeValue::UnaryLogical(_, child) => {
                callback(Some(&ExpressionType::Primitive(PrimitiveType::Bool)), child)
            }
            HirNodeValue::VtableCall(vtable, fn_id, args) => {
                callback(None, vtable);
                let params = declarations.map(|declarations| {
                    let func = &declarations.id_to_func[fn_id];
                    &func.params
                });
                for (i, arg) in args.iter().enumerate() {
                    callback(params.map(|params| &params[i]), arg);
                }
            }
            HirNodeValue::Call(lhs, args) => {
                let params = declarations.map(|declarations| {
                    let ExpressionType::ReferenceToFunction(id) = &lhs.ty else {
                        unreachable!()
                    };
                    let func = &declarations.id_to_func[id];
                    &func.params
                });
                for (i, arg) in args.iter().enumerate() {
                    callback(params.map(|params| &params[i]), arg);
                }
                callback(None, lhs);
            }
            HirNodeValue::IntrinsicCall(runtime_fn, args) => {
                let func =
                    declarations.map(|decls| &decls.id_to_func[&decls.intrinsic_to_id[runtime_fn]]);
                for (i, arg) in args.iter().enumerate() {
                    callback(func.map(|func| &func.params[i]), arg);
                }
            }
            HirNodeValue::Assignment(lhs, rhs) => {
                callback(Some(&lhs.ty), rhs);
                callback(None, lhs);
            }
            HirNodeValue::Yield(child) | HirNodeValue::Return(child) => {
                if let Some(child) = child {
                    // TODO: check return types
                    callback(None, child);
                }
            }
            // TODO: check return types of blocks
            HirNodeValue::If(cond, if_branch, else_branch) => {
                callback(Some(&ExpressionType::Primitive(PrimitiveType::Bool)), cond);
                callback(None, if_branch);
                if let Some(else_branch) = else_branch {
                    callback(None, else_branch);
                }
            }
            // TODO: check return types of blocks
            HirNodeValue::While(cond, body) => {
                callback(Some(&ExpressionType::Primitive(PrimitiveType::Bool)), cond);
                callback(None, body);
            }
            // TODO: check return types of blocks
            HirNodeValue::Sequence(children) => {
                for child in children.iter() {
                    callback(None, child);
                }
            }
            HirNodeValue::StructLiteral(ty_id, fields) => {
                let ty = declarations.map(|declarations| {
                    let TypeDeclaration::Struct(ty) = &declarations.id_to_decl[ty_id] else {
                        unreachable!();
                    };
                    ty
                });
                for (name, field) in fields.iter() {
                    callback(ty.map(|ty| &ty.fields[name]), field);
                }
            }
            HirNodeValue::DictLiteral(children) => {
                // TODO: check types for dicts
                for (key, value) in children.iter() {
                    callback(None, key);
                    callback(None, value);
                }
            }
            HirNodeValue::ArrayLiteral(children) => {
                let ExpressionType::Collection(CollectionType::Array(value_ty)) = &self.ty else {
                    unreachable!()
                };
                for child in children.iter() {
                    callback(Some(value_ty.as_ref()), child);
                }
            }
            HirNodeValue::ArrayLiteralLength(value, len) => {
                let ExpressionType::Collection(CollectionType::Array(value_ty)) = &self.ty else {
                    unreachable!()
                };
                callback(Some(value_ty.as_ref()), value);
                callback(
                    Some(&ExpressionType::Primitive(PrimitiveType::PointerSize)),
                    len,
                );
            }
            HirNodeValue::Parameter(_, _)
            | HirNodeValue::VariableReference(_)
            | HirNodeValue::Declaration(_)
            | HirNodeValue::Int(_)
            | HirNodeValue::PointerSize(_)
            | HirNodeValue::Float(_)
            | HirNodeValue::Bool(_)
            | HirNodeValue::CharLiteral(_)
            | HirNodeValue::StringLiteral(_)
            | HirNodeValue::Null
            | HirNodeValue::GotoLabel(_) => {}
            HirNodeValue::Access(child, _)
            | HirNodeValue::NullableTraverse(child, _)
            | HirNodeValue::InterfaceAddress(child)
            | HirNodeValue::TakeUnique(child)
            | HirNodeValue::TakeShared(child)
            | HirNodeValue::Dereference(child)
            | HirNodeValue::NumericCast { value: child, .. }
            | HirNodeValue::MakeNullable(child)
            | HirNodeValue::StructToInterface { value: child, .. }
            | HirNodeValue::Loop(child) => {
                callback(None, child);
            }
            HirNodeValue::GeneratorSuspend(yielded, _) => {
                // TODO: check types
                callback(None, yielded);
            }
            HirNodeValue::GeneratorResume(child) => {
                callback(None, child);
            }
            HirNodeValue::GeneratorCreate { args, .. } => {
                for arg in args.iter() {
                    callback(None, arg);
                }
            }
            HirNodeValue::StringConcat(left, right) => {
                callback(
                    Some(&ExpressionType::Collection(CollectionType::String)),
                    left,
                );
                callback(
                    Some(&ExpressionType::Collection(CollectionType::String)),
                    right,
                );
            }
            HirNodeValue::Switch { value, cases } => {
                callback(None, value);
                for case in cases.iter() {
                    callback(None, case);
                }
            }
            HirNodeValue::UnionTag(inner) | HirNodeValue::UnionVariant(inner, _) => {
                callback(None, inner);
            }
            HirNodeValue::ReferenceCountLiteral(inner) => {
                callback(None, inner);
            }
            HirNodeValue::CellLiteral(inner) => {
                callback(None, inner);
            }
            HirNodeValue::Discard(inner) => callback(None, inner),
        }
    }

    fn children_mut_impl<'a>(
        &'a mut self,
        declarations: Option<&DeclarationContext>,
        mut callback: impl FnMut(Option<&ExpressionType>, &'a mut HirNode),
    ) {
        let callback = &mut callback;
        match &mut self.value {
            HirNodeValue::DictIndex(lhs, rhs) => {
                callback(None, lhs);
                // TODO: expect dictionary keys
                callback(None, rhs);
            }
            HirNodeValue::ArrayIndex(lhs, idx) => {
                callback(None, lhs);
                callback(
                    Some(&ExpressionType::Primitive(PrimitiveType::PointerSize)),
                    idx,
                );
            }
            HirNodeValue::UnionLiteral(ty, variant, child) => {
                let variant_ty = declarations.and_then(|declarations| {
                    let TypeDeclaration::Union(ty) = &declarations.id_to_decl[ty as &TypeID] else {
                        unreachable!()
                    };
                    ty.variants[variant as &String].as_ref()
                });
                if let Some(child) = child {
                    callback(variant_ty, child);
                }
            }
            HirNodeValue::Arithmetic(_, lhs, rhs) => {
                callback(Some(&self.ty), lhs);
                callback(Some(&self.ty), rhs);
            }
            HirNodeValue::Comparison(_, lhs, rhs) => {
                if let Some(declarations) = declarations {
                    if is_assignable_to(declarations, None, &lhs.ty, &rhs.ty) {
                        callback(Some(&lhs.ty), rhs);
                        callback(None, lhs);
                    } else {
                        callback(Some(&rhs.ty), lhs);
                        callback(None, rhs);
                    }
                } else {
                    callback(None, lhs);
                    callback(None, rhs);
                }
            }
            HirNodeValue::NullCoalesce(lhs, rhs) => {
                callback(Some(&self.ty), rhs);
                callback(
                    Some(&ExpressionType::Nullable(Box::new(self.ty.clone()))),
                    lhs,
                );
            }
            HirNodeValue::BinaryLogical(_, lhs, rhs) => {
                callback(Some(&ExpressionType::Primitive(PrimitiveType::Bool)), lhs);
                callback(Some(&ExpressionType::Primitive(PrimitiveType::Bool)), rhs);
            }
            HirNodeValue::UnaryLogical(_, child) => {
                callback(Some(&ExpressionType::Primitive(PrimitiveType::Bool)), child)
            }
            HirNodeValue::VtableCall(vtable, fn_id, args) => {
                callback(None, vtable);
                let params = declarations.map(|declarations| {
                    let func = &declarations.id_to_func[fn_id as &FunctionID];
                    &func.params
                });
                for (i, arg) in args.iter_mut().enumerate() {
                    callback(params.map(|params| &params[i]), arg);
                }
            }
            HirNodeValue::Call(lhs, args) => {
                let params = declarations.map(|declarations| match &lhs.ty {
                    ExpressionType::ReferenceToFunction(id) => {
                        let func = &declarations.id_to_func[id];
                        &func.params
                    }
                    ExpressionType::FunctionReference { parameters, .. } => parameters,
                    ty => unreachable!("illegal type: {:?}", ty),
                });
                for (i, arg) in args.iter_mut().enumerate() {
                    callback(params.map(|params| &params[i]), arg);
                }
                callback(None, lhs);
            }
            HirNodeValue::IntrinsicCall(runtime_fn, args) => {
                let func = declarations.map(|decls| {
                    &decls.id_to_func[&decls.intrinsic_to_id[runtime_fn as &IntrinsicFunction]]
                });
                for (i, arg) in args.iter_mut().enumerate() {
                    callback(func.map(|func| &func.params[i]), arg);
                }
            }
            HirNodeValue::Assignment(lhs, rhs) => {
                callback(Some(&lhs.ty), rhs);
                callback(None, lhs);
            }
            HirNodeValue::Yield(child) | HirNodeValue::Return(child) => {
                if let Some(child) = child {
                    // TODO: check return types
                    callback(None, child);
                }
            }
            // TODO: check return types of blocks
            HirNodeValue::If(cond, if_branch, else_branch) => {
                callback(Some(&ExpressionType::Primitive(PrimitiveType::Bool)), cond);
                callback(None, if_branch);
                if let Some(else_branch) = else_branch {
                    callback(None, else_branch);
                }
            }
            // TODO: check return types of blocks
            HirNodeValue::While(cond, body) => {
                callback(Some(&ExpressionType::Primitive(PrimitiveType::Bool)), cond);
                callback(None, body);
            }
            // TODO: check return types of blocks
            HirNodeValue::Sequence(children) => {
                for child in children.iter_mut() {
                    callback(None, child);
                }
            }
            HirNodeValue::StructLiteral(ty_id, fields) => {
                let ty = declarations.map(|declarations| {
                    let TypeDeclaration::Struct(ty) = &declarations.id_to_decl[ty_id as &TypeID]
                    else {
                        unreachable!();
                    };
                    ty
                });
                for (name, field) in fields.iter_mut() {
                    callback(ty.map(|ty| &ty.fields[name]), field);
                }
            }
            HirNodeValue::DictLiteral(children) => {
                // TODO: check types for dicts
                for (key, value) in children.iter_mut() {
                    callback(None, key);
                    callback(None, value);
                }
            }
            HirNodeValue::ArrayLiteral(children) => {
                let ExpressionType::Collection(CollectionType::Array(value_ty)) = &self.ty else {
                    unreachable!()
                };
                for child in children.iter_mut() {
                    callback(Some(value_ty.as_ref()), child);
                }
            }
            HirNodeValue::ArrayLiteralLength(value, len) => {
                let ExpressionType::Collection(CollectionType::Array(value_ty)) = &self.ty else {
                    unreachable!()
                };
                callback(Some(value_ty.as_ref()), value);
                callback(
                    Some(&ExpressionType::Primitive(PrimitiveType::PointerSize)),
                    len,
                );
            }
            HirNodeValue::Parameter(_, _)
            | HirNodeValue::VariableReference(_)
            | HirNodeValue::Declaration(_)
            | HirNodeValue::Int(_)
            | HirNodeValue::PointerSize(_)
            | HirNodeValue::Float(_)
            | HirNodeValue::Bool(_)
            | HirNodeValue::CharLiteral(_)
            | HirNodeValue::StringLiteral(_)
            | HirNodeValue::Null
            | HirNodeValue::GotoLabel(_) => {}
            HirNodeValue::Access(child, _)
            | HirNodeValue::NullableTraverse(child, _)
            | HirNodeValue::InterfaceAddress(child)
            | HirNodeValue::TakeUnique(child)
            | HirNodeValue::TakeShared(child)
            | HirNodeValue::Dereference(child)
            | HirNodeValue::NumericCast { value: child, .. }
            | HirNodeValue::MakeNullable(child)
            | HirNodeValue::StructToInterface { value: child, .. }
            | HirNodeValue::Loop(child) => {
                callback(None, child);
            }
            HirNodeValue::GeneratorSuspend(yielded, _) => {
                // TODO: check types
                callback(None, yielded);
            }
            HirNodeValue::GeneratorResume(child) => {
                callback(None, child);
            }
            HirNodeValue::GeneratorCreate { args, .. } => {
                for arg in args.iter_mut() {
                    callback(None, arg);
                }
            }
            HirNodeValue::StringConcat(left, right) => {
                callback(
                    Some(&ExpressionType::Collection(CollectionType::String)),
                    left,
                );
                callback(
                    Some(&ExpressionType::Collection(CollectionType::String)),
                    right,
                );
            }
            HirNodeValue::Switch { value, cases } => {
                callback(None, value);
                for case in cases.iter_mut() {
                    callback(None, case);
                }
            }
            HirNodeValue::UnionTag(inner) | HirNodeValue::UnionVariant(inner, _) => {
                callback(None, inner);
            }
            HirNodeValue::ReferenceCountLiteral(inner) => {
                callback(None, inner);
            }
            HirNodeValue::CellLiteral(inner) => {
                callback(None, inner);
            }
            HirNodeValue::Discard(inner) => callback(None, inner),
        }
    }

    pub fn is_valid_lvalue(self: &HirNode) -> bool {
        match &self.value {
            HirNodeValue::VariableReference(_) => true,
            HirNodeValue::Access(lhs, _) => lhs.is_valid_lvalue(),
            HirNodeValue::Dereference(lhs) => lhs.is_valid_lvalue(),
            HirNodeValue::ArrayIndex(arr, _) => arr.is_valid_lvalue(),
            HirNodeValue::DictIndex(dict, _) => dict.is_valid_lvalue(),
            HirNodeValue::UnionVariant(union, _) => union.is_valid_lvalue(),
            _ => false,
        }
    }
}

// TODO: should struct fields also be referred to via opaque IDs?

#[derive(Clone, Debug, PartialEq, Default)]
pub enum HirNodeValue {
    /// Give the Nth parameter the given ID
    Parameter(usize, VariableID),
    VariableReference(AnyID),
    Declaration(VariableID),

    Call(Box<HirNode>, Vec<HirNode>),
    // TODO: break this up into Union Access and Struct Access?
    Access(Box<HirNode>, String),
    NullableTraverse(Box<HirNode>, Vec<String>),
    Assignment(Box<HirNode>, Box<HirNode>),
    ArrayIndex(Box<HirNode>, Box<HirNode>),
    DictIndex(Box<HirNode>, Box<HirNode>),
    StringConcat(Box<HirNode>, Box<HirNode>),
    Arithmetic(ArithmeticOp, Box<HirNode>, Box<HirNode>),
    Comparison(ComparisonOp, Box<HirNode>, Box<HirNode>),
    BinaryLogical(BinaryLogicalOp, Box<HirNode>, Box<HirNode>),
    NullCoalesce(Box<HirNode>, Box<HirNode>),
    UnaryLogical(UnaryLogicalOp, Box<HirNode>),

    Return(Option<Box<HirNode>>),
    /// Desugared out of existence, but hard to do before lowering to HIR
    Yield(Option<Box<HirNode>>),

    Int(i64),
    Float(f64),
    Bool(bool),
    PointerSize(usize),
    #[default]
    Null,
    CharLiteral(char),
    StringLiteral(String),
    NumericCast {
        value: Box<HirNode>,
        from: PrimitiveType,
        to: PrimitiveType,
    },

    TakeUnique(Box<HirNode>),
    TakeShared(Box<HirNode>),
    Dereference(Box<HirNode>),

    /// Like a Block in that it's a collection of nodes, but the IR
    /// doesn't care about scoping or expressions
    Sequence(Vec<HirNode>),

    // Expressions
    If(Box<HirNode>, Box<HirNode>, Option<Box<HirNode>>),
    While(Box<HirNode>, Box<HirNode>),
    Loop(Box<HirNode>),
    StructLiteral(TypeID, HashMap<String, HirNode>),
    UnionLiteral(TypeID, String, Option<Box<HirNode>>),
    ArrayLiteral(Vec<HirNode>),
    ArrayLiteralLength(Box<HirNode>, Box<HirNode>),
    DictLiteral(Vec<(HirNode, HirNode)>),
    ReferenceCountLiteral(Box<HirNode>),
    CellLiteral(Box<HirNode>),

    // Instructions only generated by IR passes
    /// Look up the given virtual function ID in the LHS vtable
    VtableCall(Box<HirNode>, FunctionID, Vec<HirNode>),
    IntrinsicCall(IntrinsicFunction, Vec<HirNode>),
    InterfaceAddress(Box<HirNode>),
    StructToInterface {
        value: Box<HirNode>,
        vtable: HashMap<FunctionID, FunctionID>,
    },
    MakeNullable(Box<HirNode>),

    Discard(Box<HirNode>),

    // Generator instructions, all created during HIR
    GeneratorSuspend(Box<HirNode>, usize),
    GotoLabel(usize),
    GeneratorResume(Box<HirNode>),
    GeneratorCreate {
        generator_function: FunctionID,
        args: Vec<HirNode>,
    },

    Switch {
        value: Box<HirNode>,
        cases: Vec<HirNode>,
    },
    UnionTag(Box<HirNode>),
    UnionVariant(Box<HirNode>, String),
}

impl HirNodeValue {
    pub fn lvalue_mut(&mut self) -> Option<&mut HirNode> {
        match self {
            HirNodeValue::Call(lvalue, _)
            | HirNodeValue::Access(lvalue, _)
            | HirNodeValue::NullableTraverse(lvalue, _)
            | HirNodeValue::TakeUnique(lvalue)
            | HirNodeValue::TakeShared(lvalue)
            | HirNodeValue::UnionVariant(lvalue, _)
            | HirNodeValue::StructToInterface { value: lvalue, .. } => Some(lvalue),
            _ => None,
        }
    }
}

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum ArithmeticOp {
    Add,
    Subtract,
    Multiply,
    Divide,
}

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum ComparisonOp {
    LessThan,
    GreaterThan,
    LessEqualThan,
    GreaterEqualThan,
    EqualTo,
    NotEquals,
}

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum BinaryLogicalOp {
    BooleanAnd,
    BooleanOr,
}

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum UnaryLogicalOp {
    BooleanNot,
}
